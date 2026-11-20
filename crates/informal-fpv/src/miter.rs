//! Two-safety self-composition miter builder over `informal_ir::Model`.
//!
//! Powers both `check_spv` (formal non-interference / security path verification)
//! and `check_xprop` (exact 2-copy X-propagation verification without ternary
//! X-pessimism or X-optimism). All auxiliary latches use constant `0`/`1`
//! initializers so IC3/PDR, `Ic3Mt`, `KindIc3`, k-induction, and BMC all run
//! natively at full speed.

use informal_ir::{Entry, Line, Model, Op, Sort};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

#[derive(Debug, Clone)]
pub struct ResolvedSig {
    /// Signed node reference (`+id` or `-id`).
    pub node_ref: i64,
    /// Bit width of the signal (`>= 1`).
    pub width: u32,
    /// Underlying positive line ID when directly naming an `Input` or `State`.
    pub direct_line: Option<(u32, Op)>,
    /// Canonical signal name.
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct MiterSpec {
    /// Primary/cut/X `Op::Input` line IDs allowed to differ between Copy 0 and Copy 1.
    pub diverging_inputs: HashSet<u32>,
    /// Internal/register node IDs replaced in Copy 1 by a fresh unconstrained `input`
    /// of identical width (used when `check_spv -from` names an internal wire or register).
    pub diverging_nodes: HashMap<u32, String>,
    /// When `false` (`check_spv`), uninitialized registers without `init` are constrained
    /// equal at step 0 (`r0(0) == r1(0)`) so power-on unknowns do not masquerade as secret
    /// taint; when `true` (`check_xprop`), uninitialized registers start independently in
    /// Copy 0 and Copy 1 (`r0(0)` vs `r1(0)`), except for `tied_uninit_states`.
    pub uninit_states_diverge: bool,
    /// Specific uninitialized `Op::State` IDs forced equal at step 0 even when
    /// `uninit_states_diverge` is `true` (used by `check_xprop` waivers on source flops).
    pub tied_uninit_states: HashSet<u32>,
    /// Model steps from 0 to skip before asserting `obs_0 == obs_1` (e.g. reset window).
    pub skip_steps: u32,
    /// Optional 1-bit enabling guard (signed node ref in Copy 0, required true in both copies).
    pub guard_ref: Option<i64>,
    /// Target observation `(node_ref, width, target_symbol)` checked for `obs_0 != obs_1`.
    pub target: (i64, u32, String),
}

/// Parses `full_btor` (including `; informal-trace-outputs` `output` lines) into `Model`.
pub fn parse_full_model(full_btor: &str) -> Result<Model, String> {
    informal_ir::parse(full_btor).map_err(|e| format!("miter parse: {e}"))
}

/// Resolves a user signal name (`u_sub.sig` or `top.u_sub.sig`) in `m`.
pub fn resolve_signal(m: &Model, top: Option<&str>, raw_name: &str) -> Result<ResolvedSig, String> {
    let trimmed = raw_name.trim();
    let mut candidates = vec![trimmed.to_string()];
    if let Some(t) = top.filter(|t| !t.is_empty()) {
        if let Some(rest) = trimmed.strip_prefix(t).and_then(|r| r.strip_prefix('.')) {
            candidates.push(rest.to_string());
        } else {
            candidates.push(format!("{t}.{trimmed}"));
        }
    }

    let width_of_line = |line: &Line| -> Option<u32> { line.sort.and_then(|sid| m.width(sid)) };

    for cand in &candidates {
        for l in m.lines() {
            if matches!(l.op, Op::Input | Op::State) && l.symbol.as_deref() == Some(cand.as_str()) {
                if let Some(w) = width_of_line(l) {
                    return Ok(ResolvedSig {
                        node_ref: l.id as i64,
                        width: w,
                        direct_line: Some((l.id, l.op)),
                        name: cand.clone(),
                    });
                }
            }
        }
        for l in m.lines() {
            if l.op == Op::Output && l.symbol.as_deref() == Some(cand.as_str()) {
                if let Some(&arg) = l.args.first() {
                    let base = m
                        .line(arg.unsigned_abs() as u32)
                        .ok_or_else(|| format!("dangling output node on `{cand}`"))?;
                    if let Some(w) = width_of_line(base) {
                        let direct = (matches!(base.op, Op::Input | Op::State) && arg > 0)
                            .then_some((base.id, base.op));
                        return Ok(ResolvedSig {
                            node_ref: arg,
                            width: w,
                            direct_line: direct,
                            name: cand.clone(),
                        });
                    }
                }
            }
        }
    }
    Err(format!("signal `{trimmed}` not found in lowered design"))
}

/// Lists default X-prop observation sinks in `m` according to `categories`
/// (`outputs`, `control`, `memories`, `assertions`, `all`).
pub fn collect_xprop_sinks(
    m: &Model,
    top: Option<&str>,
    top_output_ports: &[String],
    categories: &[String],
) -> Vec<(i64, u32, String, &'static str)> {
    let all = categories.is_empty() || categories.iter().any(|c| c == "all");
    let want_out = all || categories.iter().any(|c| c == "outputs" || c == "output");
    let want_ctrl = all || categories.iter().any(|c| c == "control" || c == "mux");
    let want_mem = all || categories.iter().any(|c| c == "memories" || c == "memory");
    let want_assert = categories
        .iter()
        .any(|c| c == "assertions" || c == "assert");

    let top_prefix = top.map(|t| format!("{t}.")).unwrap_or_default();
    let mut sinks: Vec<(i64, u32, String, &'static str)> = Vec::new();
    let mut seen_refs: HashSet<(i64, &'static str)> = HashSet::new();
    let mut seen_out_names: HashSet<String> = HashSet::new();

    if want_out {
        if !top_output_ports.is_empty() {
            for port in top_output_ports {
                if let Ok(r) = resolve_signal(m, top, port) {
                    if seen_out_names.insert(r.name.clone()) {
                        sinks.push((r.node_ref, r.width, r.name, "output"));
                    }
                }
            }
        } else {
            // Fallback when scope hierarchy is unavailable: top-level `output` lines in trace section.
            for l in m.lines().filter(|l| l.op == Op::Output) {
                let Some(sym) = l.symbol.as_deref() else {
                    continue;
                };
                let is_top_sig = match sym.strip_prefix(&top_prefix) {
                    Some(rest) => !rest.contains('.'),
                    None => !sym.contains('.'),
                };
                if !is_top_sig {
                    continue;
                };
                let Some(&arg) = l.args.first() else { continue };
                let Some(base) = m.line(arg.unsigned_abs() as u32) else {
                    continue;
                };
                let Some(w) = base.sort.and_then(|s| m.width(s)) else {
                    continue;
                };
                if seen_out_names.insert(sym.to_string()) {
                    sinks.push((arg, w, sym.to_string(), "output"));
                }
            }
        }
    }

    if want_ctrl {
        // Every non-trivial 1-bit `Ite` condition on bit-vectors is a mux/branch/clock-enable control!
        let mut ctrl_idx = 0usize;
        for l in m.lines().filter(|l| l.op == Op::Ite) {
            if l.sort.and_then(|s| m.width(s)).is_none() {
                continue;
            }
            let Some(&cond) = l.args.first() else {
                continue;
            };
            let Some(base) = m.line(cond.unsigned_abs() as u32) else {
                continue;
            };
            if matches!(
                base.op,
                Op::Const | Op::Constd | Op::Consth | Op::Zero | Op::One | Op::Ones
            ) {
                continue;
            }
            if base
                .symbol
                .as_deref()
                .is_some_and(|s| s.starts_with("informal_"))
            {
                continue;
            }
            if seen_refs.insert((cond.abs(), "control")) {
                let name = base
                    .symbol
                    .clone()
                    .unwrap_or_else(|| format!("{top_prefix}_xprop_ctrl_{ctrl_idx}"));
                ctrl_idx += 1;
                sinks.push((cond.abs(), 1, name, "control"));
            }
        }
    }

    if want_mem {
        let mut mem_idx = 0usize;
        for l in m.lines().filter(|l| l.op == Op::Write) {
            if l.args.len() >= 2 {
                let addr = l.args[1];
                if let Some(base) = m.line(addr.unsigned_abs() as u32) {
                    if let Some(w) = base.sort.and_then(|s| m.width(s)) {
                        if seen_refs.insert((addr, "memory")) {
                            sinks.push((
                                addr,
                                w,
                                format!("{top_prefix}_xprop_mem_addr_{mem_idx}"),
                                "memory",
                            ));
                            mem_idx += 1;
                        }
                    }
                }
            }
        }
    }

    if want_assert {
        for l in m.lines().filter(|l| l.op == Op::Bad) {
            let Some(sym) = l.symbol.as_deref() else {
                continue;
            };
            if sym.contains(':') {
                continue;
            }
            if let Some(&arg) = l.args.first() {
                if seen_refs.insert((arg, "assert")) {
                    sinks.push((arg, 1, sym.to_string(), "assert"));
                }
            }
        }
    }

    sinks
}

/// Builds a standalone Btor2 2-safety miter model from `m` and `spec`.
pub fn build_miter_btor2(m: &Model, spec: &MiterSpec) -> Result<String, String> {
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

    // 1. Emit sorts.
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

    let mut map0: HashMap<u32, u32> = HashMap::new();
    let mut map1: HashMap<u32, u32> = HashMap::new();
    let mut pending_next: Vec<(u32, u32, i64)> = Vec::new();
    let mut constraints: Vec<i64> = Vec::new();
    let mut step0_ties: Vec<(u32, u32, u32)> = Vec::new(); // (sort_new, s0, s1)

    let tr = |arg: i64, map: &HashMap<u32, u32>| -> Result<i64, String> {
        let base = arg.unsigned_abs() as u32;
        let mapped = *map
            .get(&base)
            .ok_or_else(|| format!("miter: unmapped node {base}"))? as i64;
        Ok(if arg < 0 { -mapped } else { mapped })
    };

    // 2. Emit Copy 0 and Copy 1 nodes in topological order (deferring `Next` & `Constraint`).
    for l in m.lines() {
        match l.op {
            Op::Bad | Op::Justice | Op::Fair | Op::Output => continue,
            Op::Const | Op::Constd | Op::Consth | Op::Zero | Op::One | Op::Ones => {
                let srt = sort_map[&l.sort.unwrap()];
                let nid = alloc();
                let _ = write!(out, "{nid} {} {srt}", l.op.name());
                for num in &l.nums {
                    let _ = write!(out, " {num}");
                }
                out.push('\n');
                map0.insert(l.id, nid);
                map1.insert(l.id, nid);
            }
            Op::Input => {
                let srt = sort_map[&l.sort.unwrap()];
                let sym = l.symbol.as_deref().unwrap_or("in");
                let id0 = alloc();
                let _ = writeln!(out, "{id0} input {srt} {sym}__m0");
                map0.insert(l.id, id0);
                if spec.diverging_inputs.contains(&l.id) || spec.diverging_nodes.contains_key(&l.id)
                {
                    let id1 = alloc();
                    let _ = writeln!(out, "{id1} input {srt} {sym}__m1");
                    map1.insert(l.id, id1);
                } else {
                    map1.insert(l.id, id0);
                }
            }
            Op::State => {
                let orig_srt = l.sort.unwrap();
                let srt = sort_map[&orig_srt];
                let sym = l.symbol.as_deref().unwrap_or("st");
                let id0 = alloc();
                let _ = writeln!(out, "{id0} state {srt} {sym}__m0");
                map0.insert(l.id, id0);

                if let Some(alt_name) = spec.diverging_nodes.get(&l.id) {
                    let id1 = alloc();
                    let _ = writeln!(out, "{id1} input {srt} {alt_name}__m1_cut");
                    map1.insert(l.id, id1);
                } else {
                    let id1 = alloc();
                    let _ = writeln!(out, "{id1} state {srt} {sym}__m1");
                    map1.insert(l.id, id1);

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
            Op::Init => {
                let orig_srt = l.sort.unwrap();
                let srt = sort_map[&orig_srt];
                let st_orig = l.args[0].unsigned_abs() as u32;
                let s0 = map0[&st_orig];
                let v0 = tr(l.args[1], &map0)?;
                let i0 = alloc();
                let _ = writeln!(out, "{i0} init {srt} {s0} {v0}");
                if !spec.diverging_nodes.contains_key(&st_orig) {
                    let s1 = map1[&st_orig];
                    let v1 = tr(l.args[1], &map1)?;
                    let i1 = alloc();
                    let _ = writeln!(out, "{i1} init {srt} {s1} {v1}");
                }
            }
            Op::Next => {
                let orig_srt = l.sort.unwrap();
                let srt = sort_map[&orig_srt];
                let st_orig = l.args[0].unsigned_abs() as u32;
                let s0 = map0[&st_orig];
                let v0 = tr(l.args[1], &map0)?;
                pending_next.push((srt, s0, v0));
                if !spec.diverging_nodes.contains_key(&st_orig) {
                    let s1 = map1[&st_orig];
                    let v1 = tr(l.args[1], &map1)?;
                    pending_next.push((srt, s1, v1));
                }
            }
            Op::Constraint => {
                let c0 = tr(l.args[0], &map0)?;
                let c1 = tr(l.args[0], &map1)?;
                constraints.push(c0);
                if c1 != c0 {
                    constraints.push(c1);
                }
            }
            _ => {
                let srt = sort_map[&l.sort.unwrap()];
                let args0: Result<Vec<i64>, String> =
                    l.args.iter().map(|&a| tr(a, &map0)).collect();
                let args0 = args0?;
                let id0 = alloc();
                let _ = write!(out, "{id0} {} {srt}", l.op.name());
                for a in &args0 {
                    let _ = write!(out, " {a}");
                }
                for num in &l.nums {
                    let _ = write!(out, " {num}");
                }
                out.push('\n');
                map0.insert(l.id, id0);

                if let Some(alt_name) = spec.diverging_nodes.get(&l.id) {
                    let id1 = alloc();
                    let _ = writeln!(out, "{id1} input {srt} {alt_name}__m1_cut");
                    map1.insert(l.id, id1);
                } else {
                    let args1: Result<Vec<i64>, String> =
                        l.args.iter().map(|&a| tr(a, &map1)).collect();
                    let args1 = args1?;
                    if args1 == args0 {
                        map1.insert(l.id, id0);
                    } else {
                        let id1 = alloc();
                        let _ = write!(out, "{id1} {} {srt}", l.op.name());
                        for a in &args1 {
                            let _ = write!(out, " {a}");
                        }
                        for num in &l.nums {
                            let _ = write!(out, " {num}");
                        }
                        out.push('\n');
                        map1.insert(l.id, id1);
                    }
                }
            }
        }
    }

    // 3. Step-0 equality ties for uninitialized non-secret states (`first_step -> s0 == s1`).
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

    // 4. Post-reset step gate (`skip_steps`).
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

    // 5. Optional guard expression (must hold in both Copy 0 and Copy 1).
    if let Some(g_orig) = spec.guard_ref {
        let g0 = tr(g_orig, &map0)?;
        let g1 = tr(g_orig, &map1)?;
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

    // 6. Target `neq` miter (`obs_0 != obs_1`).
    let (obs_orig, _obs_w, ref target_sym) = spec.target;
    let obs0 = tr(obs_orig, &map0)?;
    let obs1 = tr(obs_orig, &map1)?;
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

    // 7. Emit `next`, `constraint`, and `bad`.
    for (srt, st, nxt) in pending_next {
        let nid = alloc();
        let _ = writeln!(out, "{nid} next {srt} {st} {nxt}");
    }
    for c in constraints {
        let nid = alloc();
        let _ = writeln!(out, "{nid} constraint {c}");
    }
    let bad_id = alloc();
    let clean_sym: String = target_sym
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    let _ = writeln!(out, "{bad_id} bad {bad_cond} {clean_sym}");
    Ok(out)
}
