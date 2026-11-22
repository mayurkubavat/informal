use super::cond_corrupt::{validate_cond_divergences, MiterEmitter};
use super::{is_internal_helper, is_valued_op, sanitize_btor_sym, CondDivergence, MiterSpec};
use informal_ir::{Entry, Model, Op, Sort};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

/// Builds a standalone Btor2 2-safety miter model from `m` and `spec`.
pub fn build_miter_btor2(m: &Model, spec: &MiterSpec) -> Result<String, String> {
    validate_cond_divergences(m, spec)?;
    let mut initialized_states: HashSet<u32> = HashSet::new();
    for l in m.lines().filter(|l| l.op == Op::Init) {
        if let Some(&st) = l.args.first() {
            initialized_states.insert(st.unsigned_abs() as u32);
        }
    }

    let mut out = String::new();
    let mut next_id: u32 = 1;
    let mut alloc = || -> u32 {
        let id = next_id;
        next_id += 1;
        id
    };

    let mut sort_map: HashMap<u32, u32> = HashMap::new();
    let mut bv_sort_by_width: HashMap<u32, u32> = HashMap::new();
    let ensure_bv_sort = |w: u32,
                          out: &mut String,
                          bv_map: &mut HashMap<u32, u32>,
                          alloc: &mut dyn FnMut() -> u32|
     -> u32 {
        if let Some(&sid) = bv_map.get(&w) {
            return sid;
        }
        let sid = alloc();
        let _ = writeln!(out, "{sid} sort bitvec {w}");
        bv_map.insert(w, sid);
        sid
    };

    // Phase 0: Emit sorts.
    for entry in m.entries() {
        if let Entry::Sort { id, sort, .. } = entry {
            let new_sid = match *sort {
                Sort::BitVec(w) => ensure_bv_sort(w, &mut out, &mut bv_sort_by_width, &mut alloc),
                Sort::Array { index, element } => {
                    let ix = *sort_map
                        .get(&index)
                        .ok_or_else(|| format!("missing index sort {index}"))?;
                    let el = *sort_map
                        .get(&element)
                        .ok_or_else(|| format!("missing element sort {element}"))?;
                    let sid = alloc();
                    let _ = writeln!(out, "{sid} sort array {ix} {el}");
                    sid
                }
            };
            sort_map.insert(*id, new_sid);
        }
    }
    let sort1 = ensure_bv_sort(1, &mut out, &mut bv_sort_by_width, &mut alloc);

    let mut raw_map0: HashMap<u32, u32> = HashMap::new();
    let mut raw_map1: HashMap<u32, u32> = HashMap::new();
    let mut div_node_cut_m1: HashMap<u32, u32> = HashMap::new();
    let mut used_syms: HashSet<String> = HashSet::new();
    let mut step0_ties: Vec<(u32, u32, u32)> = Vec::new(); // (sort_new, s0, s1)

    // Phase 1: Emit Constants, Inputs, States, and `diverging_nodes` Copy-1 cut inputs.
    for l in m.lines() {
        match l.op {
            Op::Const | Op::Constd | Op::Consth | Op::Zero | Op::One | Op::Ones => {
                let srt = sort_map[&l.sort.unwrap()];
                let nid = alloc();
                let _ = write!(out, "{nid} {} {srt}", l.op.name());
                for num in &l.nums {
                    let _ = write!(out, " {num}");
                }
                out.push('\n');
                raw_map0.insert(l.id, nid);
                raw_map1.insert(l.id, nid);
            }
            Op::Input => {
                let srt = sort_map[&l.sort.unwrap()];
                let sym = l.symbol.as_deref().unwrap_or("in");
                let s0_name = format!("{sym}__m0");
                let id0 = alloc();
                let _ = writeln!(out, "{id0} input {srt} {s0_name}");
                used_syms.insert(s0_name);
                raw_map0.insert(l.id, id0);
                if spec.diverging_inputs.contains(&l.id) || spec.diverging_nodes.contains_key(&l.id)
                {
                    let s1_name = format!("{sym}__m1");
                    let id1 = alloc();
                    let _ = writeln!(out, "{id1} input {srt} {s1_name}");
                    used_syms.insert(s1_name);
                    raw_map1.insert(l.id, id1);
                } else {
                    raw_map1.insert(l.id, id0);
                }
            }
            Op::State => {
                let orig_srt = l.sort.unwrap();
                let srt = sort_map[&orig_srt];
                let sym = l.symbol.as_deref().unwrap_or("st");
                let s0_name = format!("{sym}__m0");
                let id0 = alloc();
                let _ = writeln!(out, "{id0} state {srt} {s0_name}");
                used_syms.insert(s0_name);
                raw_map0.insert(l.id, id0);

                if let Some(alt_name) = spec.diverging_nodes.get(&l.id) {
                    let cut_name = format!("{alt_name}__m1_cut");
                    let id1 = alloc();
                    let _ = writeln!(out, "{id1} input {srt} {cut_name}");
                    used_syms.insert(cut_name);
                    raw_map1.insert(l.id, id1);
                } else {
                    let s1_name = format!("{sym}__m1");
                    let id1 = alloc();
                    let _ = writeln!(out, "{id1} state {srt} {s1_name}");
                    used_syms.insert(s1_name);
                    raw_map1.insert(l.id, id1);

                    let is_bv = matches!(m.sort(orig_srt), Some(Sort::BitVec(_)));
                    let has_init = initialized_states.contains(&l.id);
                    if is_bv
                        && !has_init
                        && (!spec.uninit_states_diverge || spec.tied_uninit_states.contains(&l.id))
                    {
                        step0_ties.push((srt, id0, id1));
                    }
                }
            }
            op if is_valued_op(op) => {
                if let Some(alt_name) = spec.diverging_nodes.get(&l.id) {
                    let srt = sort_map[&l.sort.unwrap()];
                    let cut_name = format!("{alt_name}__m1_cut");
                    let id1 = alloc();
                    let _ = writeln!(out, "{id1} input {srt} {cut_name}");
                    used_syms.insert(cut_name);
                    div_node_cut_m1.insert(l.id, id1);
                }
            }
            _ => {}
        }
    }

    let mut cond_div_map: HashMap<u32, Vec<(&CondDivergence, bool)>> = HashMap::new();
    for cd in &spec.conditional_diverging_nodes {
        cond_div_map
            .entry(cd.node_id)
            .or_default()
            .push((cd, false));
    }
    for cd in &spec.conditional_state_corruptions {
        cond_div_map.entry(cd.node_id).or_default().push((cd, true));
    }

    let mut emitter = MiterEmitter {
        m,
        out,
        next_id,
        sort_map,
        raw_map0,
        raw_map1,
        pa_map0: HashMap::new(),
        pa_map1: HashMap::new(),
        div_node_cut_m1,
        cond_div_map,
        state_corrupt_wrappers: HashMap::new(),
        used_syms,
        visiting_pa: HashSet::new(),
    };

    // Phase 2-4: Demand-driven topological PA emission across all value nodes.
    for l in m.lines() {
        if is_valued_op(l.op) {
            emitter.emit_pa_top(l.id)?;
        }
    }

    let MiterEmitter {
        mut out,
        mut next_id,
        sort_map,
        raw_map0,
        raw_map1,
        pa_map0,
        pa_map1,
        state_corrupt_wrappers,
        ..
    } = emitter;

    let mut alloc = || -> u32 {
        let id = next_id;
        next_id += 1;
        id
    };

    let tr = |arg: i64, map: &HashMap<u32, u32>| -> Result<i64, String> {
        let base = arg.unsigned_abs() as u32;
        let mapped = *map
            .get(&base)
            .ok_or_else(|| format!("miter: unmapped node {base}"))? as i64;
        Ok(if arg < 0 { -mapped } else { mapped })
    };

    let mut pending_next: Vec<(u32, u32, i64)> = Vec::new();
    let mut constraints: Vec<i64> = Vec::new();

    // Phase 5: Emit `Init` (deferred until all value nodes are defined) and collect `Constraint` / `Next`.
    for l in m.lines() {
        match l.op {
            Op::Init => {
                let orig_srt = l.sort.unwrap();
                let srt = sort_map[&orig_srt];
                let st_orig = l.args[0].unsigned_abs() as u32;
                let s0 = raw_map0[&st_orig];
                let v0 = tr(l.args[1], &pa_map0)?;
                let i0 = alloc();
                let _ = writeln!(out, "{i0} init {srt} {s0} {v0}");
                if !spec.diverging_nodes.contains_key(&st_orig) {
                    let s1 = raw_map1[&st_orig];
                    let v1 = tr(l.args[1], &pa_map1)?;
                    let i1 = alloc();
                    let _ = writeln!(out, "{i1} init {srt} {s1} {v1}");
                }
            }
            Op::Next => {
                let orig_srt = l.sort.unwrap();
                let srt = sort_map[&orig_srt];
                let st_orig = l.args[0].unsigned_abs() as u32;
                let s0 = raw_map0[&st_orig];
                let mut v0 = tr(l.args[1], &pa_map0)?;
                if let Some(wrappers) = state_corrupt_wrappers.get(&st_orig) {
                    for &(c0, xi0, _, _) in wrappers {
                        let wid = alloc();
                        let _ = writeln!(out, "{wid} ite {srt} {c0} {xi0} {v0}");
                        v0 = wid as i64;
                    }
                }
                pending_next.push((srt, s0, v0));
                if !spec.diverging_nodes.contains_key(&st_orig) {
                    let s1 = raw_map1[&st_orig];
                    let mut v1 = tr(l.args[1], &pa_map1)?;
                    if let Some(wrappers) = state_corrupt_wrappers.get(&st_orig) {
                        for &(_, _, c1, xi1) in wrappers {
                            let wid = alloc();
                            let _ = writeln!(out, "{wid} ite {srt} {c1} {xi1} {v1}");
                            v1 = wid as i64;
                        }
                    }
                    pending_next.push((srt, s1, v1));
                }
            }
            Op::Constraint => {
                let c0 = tr(l.args[0], &pa_map0)?;
                let c1 = tr(l.args[0], &pa_map1)?;
                constraints.push(c0);
                if c1 != c0 {
                    constraints.push(c1);
                }
            }
            _ => {}
        }
    }

    // Phase 6: Step-0 ties, `skip_steps`, `guard_ref`, and target `neq` miter.
    if !step0_ties.is_empty() {
        let z1 = alloc();
        let _ = writeln!(out, "{z1} zero {sort1}");
        let o1 = alloc();
        let _ = writeln!(out, "{o1} one {sort1}");
        let fs = alloc();
        let _ = writeln!(out, "{fs} state {sort1} miter_first_step");
        let fs_init = alloc();
        let _ = writeln!(out, "{fs_init} init {sort1} {fs} {o1}");
        pending_next.push((sort1, fs, z1 as i64));
        for (_, s0, s1) in step0_ties {
            let eq = alloc();
            let _ = writeln!(out, "{eq} eq {sort1} {s0} {s1}");
            let imp = alloc();
            let _ = writeln!(out, "{imp} implies {sort1} {fs} {eq}");
            constraints.push(imp as i64);
        }
    }

    let mut gate_ref: Option<i64> = None;
    if spec.skip_steps > 0 {
        let z1 = alloc();
        let _ = writeln!(out, "{z1} zero {sort1}");
        let o1 = alloc();
        let _ = writeln!(out, "{o1} one {sort1}");
        let mut cur = o1 as i64;
        for step in 0..spec.skip_steps {
            let st = alloc();
            let _ = writeln!(out, "{st} state {sort1} miter_post_rst_{step}");
            let ini = alloc();
            let _ = writeln!(out, "{ini} init {sort1} {st} {z1}");
            pending_next.push((sort1, st, cur));
            cur = st as i64;
        }
        gate_ref = Some(cur);
    }

    if let Some(g_orig) = spec.guard_ref {
        let g0 = tr(g_orig, &pa_map0)?;
        let g1 = tr(g_orig, &pa_map1)?;
        let g_both = if g0 == g1 {
            g0
        } else {
            let a = alloc();
            let _ = writeln!(out, "{a} and {sort1} {g0} {g1}");
            a as i64
        };
        gate_ref = Some(match gate_ref {
            None => g_both,
            Some(prev) => {
                let a = alloc();
                let _ = writeln!(out, "{a} and {sort1} {prev} {g_both}");
                a as i64
            }
        });
    }

    let (obs_orig, _obs_w, ref target_sym) = spec.target;
    let obs0 = tr(obs_orig, &pa_map0)?;
    let obs1 = tr(obs_orig, &pa_map1)?;
    let diff = if obs0 == obs1 {
        let z1 = alloc();
        let _ = writeln!(out, "{z1} zero {sort1}");
        z1 as i64
    } else {
        let neq = alloc();
        let _ = writeln!(out, "{neq} neq {sort1} {obs0} {obs1}");
        neq as i64
    };
    let bad_cond = match gate_ref {
        None => diff,
        Some(g) => {
            let a = alloc();
            let _ = writeln!(out, "{a} and {sort1} {g} {diff}");
            a as i64
        }
    };

    // Phase 7: Emit `next`, `constraint`, and `bad`.
    for (srt, st, nxt) in pending_next {
        let nid = alloc();
        let _ = writeln!(out, "{nid} next {srt} {st} {nxt}");
    }
    for c in constraints {
        let nid = alloc();
        let _ = writeln!(out, "{nid} constraint {c}");
    }
    let bad_id = alloc();
    let clean_sym = sanitize_btor_sym(target_sym);
    let _ = writeln!(out, "{bad_id} bad {bad_cond} {clean_sym}");

    // Phase 8: Optional `; informal-trace-outputs` with `m0.<sig>` / `m1.<sig>`.
    if spec.emit_trace_outputs {
        let _ = writeln!(out, "{}", informal_svlang::TRACE_MARKER);
        let mut seen_out: HashSet<String> = HashSet::new();
        let mut emit_pair = |sig: &str, r0: i64, r1: i64| {
            let clean = sanitize_btor_sym(sig);
            let n0 = format!("m0.{clean}");
            if seen_out.insert(n0.clone()) {
                let oid0 = alloc();
                let _ = writeln!(out, "{oid0} output {r0} {n0}");
            }
            let n1 = format!("m1.{clean}");
            if seen_out.insert(n1.clone()) {
                let oid1 = alloc();
                let _ = writeln!(out, "{oid1} output {r1} {n1}");
            }
        };

        for l in m.lines() {
            if l.op == Op::Output {
                if let (Some(&arg), Some(sym)) = (l.args.first(), l.symbol.as_deref()) {
                    if !is_internal_helper(sym) {
                        let r0 = tr(arg, &pa_map0)?;
                        let r1 = tr(arg, &pa_map1)?;
                        emit_pair(sym, r0, r1);
                    }
                }
            }
        }
        for l in m.lines() {
            if matches!(l.op, Op::Input | Op::State) {
                if let Some(sym) = l.symbol.as_deref() {
                    if !is_internal_helper(sym) {
                        let r0 = tr(l.id as i64, &pa_map0)?;
                        let r1 = tr(l.id as i64, &pa_map1)?;
                        emit_pair(sym, r0, r1);
                    }
                }
            }
        }
        emit_pair(target_sym, obs0, obs1);
    }

    Ok(out)
}
