use super::{is_valued_op, sanitize_btor_sym, CondDivergence, MiterSpec};
use informal_ir::{Line, Model, Op, Sort};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

pub(super) struct MiterEmitter<'a> {
    pub(super) m: &'a Model,
    pub(super) out: String,
    pub(super) next_id: u32,
    pub(super) sort_map: HashMap<u32, u32>,
    pub(super) raw_map0: HashMap<u32, u32>,
    pub(super) raw_map1: HashMap<u32, u32>,
    pub(super) pa_map0: HashMap<u32, u32>,
    pub(super) pa_map1: HashMap<u32, u32>,
    pub(super) div_node_cut_m1: HashMap<u32, u32>,
    pub(super) cond_div_map: HashMap<u32, Vec<(&'a CondDivergence, bool)>>,
    pub(super) state_corrupt_wrappers: HashMap<u32, Vec<(i64, u32, i64, u32)>>,
    pub(super) used_syms: HashSet<String>,
    pub(super) visiting_pa: HashSet<u32>,
}

impl<'a> MiterEmitter<'a> {
    fn alloc(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn alloc_corrupt_syms(&mut self, label: &str) -> (String, String) {
        let base = sanitize_btor_sym(label);
        let mut idx = 1usize;
        loop {
            let stem = if idx == 1 {
                base.clone()
            } else {
                format!("{base}_{idx}")
            };
            let s0 = format!("{stem}__m0_corrupt");
            let s1 = format!("{stem}__m1_corrupt");
            if !self.used_syms.contains(&s0) && !self.used_syms.contains(&s1) {
                self.used_syms.insert(s0.clone());
                self.used_syms.insert(s1.clone());
                return (s0, s1);
            }
            idx += 1;
        }
    }

    fn emit_line_with_args(&mut self, l: &Line, srt: u32, args: &[i64]) -> u32 {
        let id = self.alloc();
        let _ = write!(self.out, "{id} {} {srt}", l.op.name());
        for a in args {
            let _ = write!(self.out, " {a}");
        }
        for num in &l.nums {
            let _ = write!(self.out, " {num}");
        }
        self.out.push('\n');
        id
    }

    fn emit_raw(&mut self, id: u32) -> Result<(u32, u32), String> {
        if let (Some(&r0), Some(&r1)) = (self.raw_map0.get(&id), self.raw_map1.get(&id)) {
            return Ok((r0, r1));
        }
        let l = self
            .m
            .line(id)
            .ok_or_else(|| format!("miter: unmapped node {id}"))?;
        if !is_valued_op(l.op) {
            return Err(format!("miter: cannot reference non-value node {id}"));
        }
        let mut args0 = Vec::with_capacity(l.args.len());
        let mut args1 = Vec::with_capacity(l.args.len());
        for &a in &l.args {
            let (a0, a1) = self.emit_raw(a.unsigned_abs() as u32)?;
            args0.push(if a < 0 { -(a0 as i64) } else { a0 as i64 });
            args1.push(if a < 0 { -(a1 as i64) } else { a1 as i64 });
        }
        let srt = self.sort_map[&l.sort.unwrap()];
        let id0 = self.emit_line_with_args(l, srt, &args0);
        let id1 = if let Some(&cut1) = self.div_node_cut_m1.get(&id) {
            cut1
        } else if args1 == args0 {
            id0
        } else {
            self.emit_line_with_args(l, srt, &args1)
        };
        self.raw_map0.insert(id, id0);
        self.raw_map1.insert(id, id1);
        Ok((id0, id1))
    }

    fn eval_pa(&mut self, id: u32) -> Result<(u32, u32, HashSet<u32>), String> {
        if let (Some(&p0), Some(&p1)) = (self.pa_map0.get(&id), self.pa_map1.get(&id)) {
            return Ok((p0, p1, HashSet::new()));
        }
        if self.visiting_pa.contains(&id) {
            let (r0, r1) = self.emit_raw(id)?;
            let mut hits = HashSet::new();
            hits.insert(id);
            return Ok((r0, r1, hits));
        }
        let l = self
            .m
            .line(id)
            .ok_or_else(|| format!("miter: unmapped node {id}"))?;
        if !is_valued_op(l.op) {
            return Err(format!("miter: cannot reference non-value node {id}"));
        }

        let cds = self.cond_div_map.get(&id).cloned();
        if let Some(cds) = cds {
            self.visiting_pa.insert(id);
            let (mut func0, mut func1, mut hits) = match l.op {
                Op::Const
                | Op::Constd
                | Op::Consth
                | Op::Zero
                | Op::One
                | Op::Ones
                | Op::Input
                | Op::State => (self.raw_map0[&id], self.raw_map1[&id], HashSet::new()),
                _ => {
                    let mut args0 = Vec::with_capacity(l.args.len());
                    let mut args1 = Vec::with_capacity(l.args.len());
                    let mut h = HashSet::new();
                    for &a in &l.args {
                        let (a0, a1, ah) = self.eval_pa(a.unsigned_abs() as u32)?;
                        h.extend(ah);
                        args0.push(if a < 0 { -(a0 as i64) } else { a0 as i64 });
                        args1.push(if a < 0 { -(a1 as i64) } else { a1 as i64 });
                    }
                    let srt = self.sort_map[&l.sort.unwrap()];
                    let id0 = self.emit_line_with_args(l, srt, &args0);
                    let id1 = if let Some(&cut1) = self.div_node_cut_m1.get(&id) {
                        cut1
                    } else if args1 == args0 {
                        id0
                    } else {
                        self.emit_line_with_args(l, srt, &args1)
                    };
                    (id0, id1, h)
                }
            };

            let orig_srt = l.sort.unwrap();
            let srt = self.sort_map[&orig_srt];
            let mut local_state_wrappers = Vec::new();
            for (cd, is_state_corrupt) in cds {
                let cond_id = cd.corrupt_cond.unsigned_abs() as u32;
                let (c0_base, c1_base, chits) = self.eval_pa(cond_id)?;
                hits.extend(chits);
                let c0 = if cd.corrupt_cond < 0 {
                    -(c0_base as i64)
                } else {
                    c0_base as i64
                };
                let c1 = if cd.corrupt_cond < 0 {
                    -(c1_base as i64)
                } else {
                    c1_base as i64
                };
                let (sym0, sym1) = self.alloc_corrupt_syms(&cd.label);
                let xi0 = self.alloc();
                let _ = writeln!(self.out, "{xi0} input {srt} {sym0}");
                let xi1 = self.alloc();
                let _ = writeln!(self.out, "{xi1} input {srt} {sym1}");
                let pa0 = self.alloc();
                let _ = writeln!(self.out, "{pa0} ite {srt} {c0} {xi0} {func0}");
                let pa1 = self.alloc();
                let _ = writeln!(self.out, "{pa1} ite {srt} {c1} {xi1} {func1}");
                func0 = pa0;
                func1 = pa1;
                if is_state_corrupt {
                    local_state_wrappers.push((c0, xi0, c1, xi1));
                }
            }

            self.visiting_pa.remove(&id);
            hits.remove(&id);
            if hits.is_empty() {
                self.pa_map0.insert(id, func0);
                self.pa_map1.insert(id, func1);
                if !local_state_wrappers.is_empty() {
                    self.state_corrupt_wrappers
                        .entry(id)
                        .or_default()
                        .extend(local_state_wrappers);
                }
            }
            Ok((func0, func1, hits))
        } else {
            match l.op {
                Op::Const
                | Op::Constd
                | Op::Consth
                | Op::Zero
                | Op::One
                | Op::Ones
                | Op::Input
                | Op::State => {
                    let (r0, r1) = (self.raw_map0[&id], self.raw_map1[&id]);
                    self.pa_map0.insert(id, r0);
                    self.pa_map1.insert(id, r1);
                    Ok((r0, r1, HashSet::new()))
                }
                _ => {
                    let mut args0 = Vec::with_capacity(l.args.len());
                    let mut args1 = Vec::with_capacity(l.args.len());
                    let mut hits = HashSet::new();
                    for &a in &l.args {
                        let (a0, a1, ah) = self.eval_pa(a.unsigned_abs() as u32)?;
                        hits.extend(ah);
                        args0.push(if a < 0 { -(a0 as i64) } else { a0 as i64 });
                        args1.push(if a < 0 { -(a1 as i64) } else { a1 as i64 });
                    }
                    let srt = self.sort_map[&l.sort.unwrap()];
                    let id0 = self.emit_line_with_args(l, srt, &args0);
                    let id1 = if let Some(&cut1) = self.div_node_cut_m1.get(&id) {
                        cut1
                    } else if args1 == args0 {
                        id0
                    } else {
                        self.emit_line_with_args(l, srt, &args1)
                    };
                    if hits.is_empty() {
                        self.pa_map0.insert(id, id0);
                        self.pa_map1.insert(id, id1);
                    }
                    Ok((id0, id1, hits))
                }
            }
        }
    }

    pub(super) fn emit_pa_top(&mut self, id: u32) -> Result<(u32, u32), String> {
        let (p0, p1, hits) = self.eval_pa(id)?;
        debug_assert!(hits.is_empty());
        Ok((p0, p1))
    }
}

pub(super) fn validate_cond_divergences(m: &Model, spec: &MiterSpec) -> Result<(), String> {
    let validate_cd = |cd: &CondDivergence, require_state: bool| -> Result<(), String> {
        if cd.corrupt_cond == 0 {
            return Err("miter: CondDivergence corrupt_cond must be non-zero".into());
        }
        let cond_id = u32::try_from(cd.corrupt_cond.unsigned_abs()).map_err(|_| {
            format!(
                "miter: invalid CondDivergence corrupt_cond {}",
                cd.corrupt_cond
            )
        })?;
        let cond_line = m.line(cond_id).ok_or_else(|| {
            format!(
                "miter: CondDivergence corrupt_cond {} does not exist",
                cd.corrupt_cond
            )
        })?;
        if !is_valued_op(cond_line.op) {
            return Err(format!(
                "miter: CondDivergence corrupt_cond {} is not a value node",
                cd.corrupt_cond
            ));
        }
        let cond_sort = cond_line.sort.and_then(|sid| m.sort(sid));
        if cond_sort != Some(Sort::BitVec(1)) {
            return Err(format!(
                "miter: CondDivergence corrupt_cond {} must have 1-bit bitvec sort (got {:?})",
                cd.corrupt_cond, cond_sort
            ));
        }
        let node_line = m.line(cd.node_id).ok_or_else(|| {
            format!(
                "miter: CondDivergence target node {} does not exist",
                cd.node_id
            )
        })?;
        if !is_valued_op(node_line.op) || node_line.sort.is_none() {
            return Err(format!(
                "miter: CondDivergence target node {} has no value sort",
                cd.node_id
            ));
        }
        if require_state && node_line.op != Op::State {
            return Err(format!(
                "miter: conditional_state_corruptions target {} must be Op::State (got {})",
                cd.node_id,
                node_line.op.name()
            ));
        }
        Ok(())
    };
    for cd in &spec.conditional_diverging_nodes {
        validate_cd(cd, false)?;
    }
    for cd in &spec.conditional_state_corruptions {
        validate_cd(cd, true)?;
    }
    Ok(())
}
