//! Monotonic-ID Btor2 emitter (`BtorEmitter`) used by power-aware formal
//! instrumentation, isolation/retention modeling, and VCD trace enrichment.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use informal_ir::{Entry, Line, Model, Op, Sort};

/// Appends sorts, value nodes, states, inits, nexts, constraints, bads, and
/// trace outputs cleanly on top of a parsed base Btor2 [`Model`].
pub struct BtorEmitter<'a> {
    pub base: &'a Model,
    next_id: u32,
    bv_sorts: HashMap<u32, u32>,
    node_widths: HashMap<u32, u32>,
    /// Strictly increasing ID definitions (`sort`, value ops, `input`, `state`).
    extra_defs: String,
    extra_inits: Vec<(u32, u32, i64)>,
    extra_nexts: Vec<(u32, u32, i64)>,
    extra_constraints: Vec<i64>,
    extra_bads: Vec<(i64, String)>,
    extra_outputs: Vec<(i64, String)>,
    used_syms: HashSet<String>,
    zero_cache: HashMap<u32, i64>,
    one_cache: HashMap<u32, i64>,
    ones_cache: HashMap<u32, i64>,
}

impl<'a> BtorEmitter<'a> {
    pub fn new(base: &'a Model) -> Self {
        let mut next_id = 1u32;
        let mut bv_sorts = HashMap::new();
        let mut node_widths = HashMap::new();
        let mut used_syms = HashSet::new();

        for entry in base.entries() {
            match entry {
                Entry::Sort { id, sort, .. } => {
                    next_id = next_id.max(id.saturating_add(1));
                    if let Sort::BitVec(w) = *sort {
                        bv_sorts.entry(w).or_insert(*id);
                    }
                }
                Entry::Line(l) => {
                    next_id = next_id.max(l.id.saturating_add(1));
                    if let Some(w) = l.sort.and_then(|sid| base.width(sid)) {
                        node_widths.insert(l.id, w);
                    }
                    if let Some(ref sym) = l.symbol {
                        used_syms.insert(sym.clone());
                    }
                }
            }
        }

        Self {
            base,
            next_id,
            bv_sorts,
            node_widths,
            extra_defs: String::new(),
            extra_inits: Vec::new(),
            extra_nexts: Vec::new(),
            extra_constraints: Vec::new(),
            extra_bads: Vec::new(),
            extra_outputs: Vec::new(),
            used_syms,
            zero_cache: HashMap::new(),
            one_cache: HashMap::new(),
            ones_cache: HashMap::new(),
        }
    }

    pub fn alloc(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn unique_sym(&mut self, base_sym: &str) -> String {
        let clean: String = base_sym
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let stem = if clean.is_empty() {
            "lp_sig".to_string()
        } else {
            clean
        };
        if self.used_syms.insert(stem.clone()) {
            return stem;
        }
        let mut idx = 1usize;
        loop {
            let cand = format!("{stem}_{idx}");
            if self.used_syms.insert(cand.clone()) {
                return cand;
            }
            idx += 1;
        }
    }

    pub fn ensure_bv(&mut self, w: u32) -> u32 {
        let w = w.max(1);
        if let Some(&sid) = self.bv_sorts.get(&w) {
            return sid;
        }
        let sid = self.alloc();
        let _ = writeln!(self.extra_defs, "{sid} sort bitvec {w}");
        self.bv_sorts.insert(w, sid);
        sid
    }

    pub fn width_of(&self, nref: i64) -> Option<u32> {
        let id = nref.unsigned_abs() as u32;
        self.node_widths.get(&id).copied()
    }

    pub fn sort_of(&mut self, nref: i64) -> Option<u32> {
        let w = self.width_of(nref)?;
        Some(self.ensure_bv(w))
    }

    pub fn const_zero(&mut self, w: u32) -> i64 {
        let w = w.max(1);
        if let Some(&id) = self.zero_cache.get(&w) {
            return id;
        }
        let sid = self.ensure_bv(w);
        let id = self.alloc();
        let _ = writeln!(self.extra_defs, "{id} zero {sid}");
        self.node_widths.insert(id, w);
        self.zero_cache.insert(w, id as i64);
        id as i64
    }

    pub fn const_one(&mut self, w: u32) -> i64 {
        let w = w.max(1);
        if let Some(&id) = self.one_cache.get(&w) {
            return id;
        }
        let sid = self.ensure_bv(w);
        let id = self.alloc();
        let _ = writeln!(self.extra_defs, "{id} one {sid}");
        self.node_widths.insert(id, w);
        self.one_cache.insert(w, id as i64);
        id as i64
    }

    pub fn const_ones(&mut self, w: u32) -> i64 {
        let w = w.max(1);
        if let Some(&id) = self.ones_cache.get(&w) {
            return id;
        }
        let sid = self.ensure_bv(w);
        let id = self.alloc();
        let _ = writeln!(self.extra_defs, "{id} ones {sid}");
        self.node_widths.insert(id, w);
        self.ones_cache.insert(w, id as i64);
        id as i64
    }

    pub fn const_u64(&mut self, w: u32, val: u64) -> i64 {
        let w = w.max(1);
        if val == 0 {
            return self.const_zero(w);
        }
        if val == 1 {
            return self.const_one(w);
        }
        let sid = self.ensure_bv(w);
        let id = self.alloc();
        let _ = writeln!(self.extra_defs, "{id} constd {sid} {val}");
        self.node_widths.insert(id, w);
        id as i64
    }

    pub fn unary(&mut self, op: &str, out_w: u32, a: i64) -> i64 {
        let sid = self.ensure_bv(out_w);
        let id = self.alloc();
        let _ = writeln!(self.extra_defs, "{id} {op} {sid} {a}");
        self.node_widths.insert(id, out_w);
        id as i64
    }

    pub fn binary(&mut self, op: &str, out_w: u32, a: i64, b: i64) -> i64 {
        let sid = self.ensure_bv(out_w);
        let id = self.alloc();
        let _ = writeln!(self.extra_defs, "{id} {op} {sid} {a} {b}");
        self.node_widths.insert(id, out_w);
        id as i64
    }

    pub fn ite(&mut self, out_w: u32, cond: i64, t: i64, f: i64) -> i64 {
        let sid = self.ensure_bv(out_w);
        let id = self.alloc();
        let _ = writeln!(self.extra_defs, "{id} ite {sid} {cond} {t} {f}");
        self.node_widths.insert(id, out_w);
        id as i64
    }

    pub fn emit_raw_op(
        &mut self,
        op: Op,
        sort_id: u32,
        out_w: Option<u32>,
        args: &[i64],
        nums: &[String],
    ) -> u32 {
        let id = self.alloc();
        let _ = write!(self.extra_defs, "{id} {} {sort_id}", op.name());
        for a in args {
            let _ = write!(self.extra_defs, " {a}");
        }
        for n in nums {
            let _ = write!(self.extra_defs, " {n}");
        }
        self.extra_defs.push('\n');
        if let Some(w) = out_w {
            self.node_widths.insert(id, w);
        }
        id
    }

    pub fn alloc_input(&mut self, w: u32, sym_hint: &str) -> u32 {
        let sid = self.ensure_bv(w);
        let id = self.alloc();
        let sym = self.unique_sym(sym_hint);
        let _ = writeln!(self.extra_defs, "{id} input {sid} {sym}");
        self.node_widths.insert(id, w);
        id
    }

    pub fn alloc_state(&mut self, w: u32, sym_hint: &str) -> u32 {
        let sid = self.ensure_bv(w);
        let id = self.alloc();
        let sym = self.unique_sym(sym_hint);
        let _ = writeln!(self.extra_defs, "{id} state {sid} {sym}");
        self.node_widths.insert(id, w);
        id
    }

    pub fn emit_init(&mut self, w: u32, st_id: u32, init_val: i64) {
        let sid = self.ensure_bv(w);
        self.extra_inits.push((sid, st_id, init_val));
    }

    pub fn emit_next(&mut self, w: u32, st_id: u32, next_val: i64) {
        let sid = self.ensure_bv(w);
        self.extra_nexts.push((sid, st_id, next_val));
    }

    pub fn emit_constraint(&mut self, cond: i64) {
        self.extra_constraints.push(cond);
    }

    pub fn emit_bad(&mut self, cond: i64, sym: &str) {
        self.extra_bads.push((cond, sanitize_sym(sym)));
    }

    pub fn add_output(&mut self, nref: i64, sym: String) {
        self.extra_outputs.push((nref, sym));
    }

    pub fn to_bool(&mut self, nref: i64) -> i64 {
        match self.width_of(nref) {
            Some(1) | None => nref,
            Some(_) => self.unary("redor", 1, nref),
        }
    }

    pub fn not1(&mut self, a: i64) -> i64 {
        let b = self.to_bool(a);
        self.unary("not", 1, b)
    }

    pub fn and1(&mut self, a: i64, b: i64) -> i64 {
        let a1 = self.to_bool(a);
        let b1 = self.to_bool(b);
        if a1 == b1 {
            return a1;
        }
        self.binary("and", 1, a1, b1)
    }

    pub fn or1(&mut self, a: i64, b: i64) -> i64 {
        let a1 = self.to_bool(a);
        let b1 = self.to_bool(b);
        if a1 == b1 {
            return a1;
        }
        self.binary("or", 1, a1, b1)
    }

    pub fn any1(&mut self, items: &[i64]) -> i64 {
        let mut it = items.iter().copied();
        let Some(first) = it.next() else {
            return self.const_zero(1);
        };
        let mut acc = self.to_bool(first);
        for x in it {
            acc = self.or1(acc, x);
        }
        acc
    }

    /// Serializes the base model plus all appended instrumentation into a single
    /// valid Btor2 file (strictly increasing IDs, including `; informal-trace-outputs`).
    pub fn finish(
        &mut self,
        next_overrides: &HashMap<u32, i64>,
        output_overrides: &HashMap<String, i64>,
        keep_base_bads: bool,
    ) -> String {
        let mut out = String::new();

        // 1. Original entries (Sorts + value/init lines) in exact original order
        //    so IDs are strictly increasing and < initial `next_id`.
        for entry in self.base.entries() {
            match entry {
                Entry::Sort { id, sort, .. } => match *sort {
                    Sort::BitVec(w) => {
                        let _ = writeln!(out, "{id} sort bitvec {w}");
                    }
                    Sort::Array { index, element } => {
                        let _ = writeln!(out, "{id} sort array {index} {element}");
                    }
                },
                Entry::Line(l) => {
                    if matches!(
                        l.op,
                        Op::Next | Op::Bad | Op::Constraint | Op::Fair | Op::Justice | Op::Output
                    ) {
                        continue;
                    }
                    write_original_line(&mut out, l);
                }
            }
        }

        // 2. Extra definitions (`sort`, value ops, `input`, `state`) in strict alloc order.
        out.push_str(&self.extra_defs);

        // 3. Extra `init` lines (fresh strictly increasing IDs).
        for &(sid, st_id, val) in &self.extra_inits.clone() {
            let id = self.alloc();
            let _ = writeln!(out, "{id} init {sid} {st_id} {val}");
        }

        // 4. All `next` lines (original + overrides + extra) with fresh strictly increasing IDs.
        let mut overridden_emitted = HashSet::new();
        for l in self.base.lines() {
            if l.op != Op::Next {
                continue;
            }
            let st_id = l.args[0].unsigned_abs() as u32;
            let srt = l.sort.unwrap();
            let val = if let Some(&new_next) = next_overrides.get(&st_id) {
                overridden_emitted.insert(st_id);
                new_next
            } else {
                l.args[1]
            };
            let id = self.alloc();
            let _ = writeln!(out, "{id} next {srt} {st_id} {val}");
        }
        for (&st_id, &new_next) in next_overrides {
            if !overridden_emitted.contains(&st_id) {
                if let Some(srt) = self.base.line(st_id).and_then(|l| l.sort) {
                    let id = self.alloc();
                    let _ = writeln!(out, "{id} next {srt} {st_id} {new_next}");
                }
            }
        }
        for &(sid, st_id, val) in &self.extra_nexts.clone() {
            let id = self.alloc();
            let _ = writeln!(out, "{id} next {sid} {st_id} {val}");
        }

        // 5. Constraints, Fair, Justice, and Bads with fresh strictly increasing IDs.
        for l in self.base.lines() {
            match l.op {
                Op::Constraint | Op::Fair | Op::Justice => {
                    let id = self.alloc();
                    write_reindexed_line(&mut out, id, l);
                }
                Op::Bad if keep_base_bads => {
                    let id = self.alloc();
                    write_reindexed_line(&mut out, id, l);
                }
                _ => {}
            }
        }
        for &c in &self.extra_constraints.clone() {
            let id = self.alloc();
            let _ = writeln!(out, "{id} constraint {c}");
        }
        for (cond, sym) in &self.extra_bads.clone() {
            let id = self.alloc();
            let _ = writeln!(out, "{id} bad {cond} {sym}");
        }

        // 6. Trace outputs (`; informal-trace-outputs`).
        let mut trace_rows: Vec<(i64, String)> = Vec::new();
        let mut seen = HashSet::new();
        for l in self.base.lines() {
            if l.op == Op::Output {
                if let (Some(&arg), Some(sym)) = (l.args.first(), l.symbol.as_deref()) {
                    let eff = output_overrides.get(sym).copied().unwrap_or(arg);
                    if seen.insert(sym.to_string()) {
                        trace_rows.push((eff, sym.to_string()));
                    }
                }
            }
        }
        for (nref, sym) in &self.extra_outputs {
            if seen.insert(sym.clone()) {
                trace_rows.push((*nref, sym.clone()));
            }
        }
        if !trace_rows.is_empty() {
            let _ = writeln!(out, "{}", informal_svlang::TRACE_MARKER);
            for (nref, sym) in trace_rows {
                let oid = self.alloc();
                let _ = writeln!(out, "{oid} output {nref} {sym}");
            }
        }

        out
    }
}

fn write_original_line(out: &mut String, l: &Line) {
    write_reindexed_line(out, l.id, l);
}

fn write_reindexed_line(out: &mut String, id: u32, l: &Line) {
    let _ = write!(out, "{id} {}", l.op.name());
    if let Some(s) = l.sort {
        let _ = write!(out, " {s}");
    }
    for a in &l.args {
        let _ = write!(out, " {a}");
    }
    for n in &l.nums {
        let _ = write!(out, " {n}");
    }
    if let Some(ref sym) = l.symbol {
        let _ = write!(out, " {sym}");
    }
    out.push('\n');
}

pub fn sanitize_sym(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "lp_sym".to_string()
    } else {
        s
    }
}
