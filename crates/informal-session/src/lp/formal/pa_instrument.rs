//! Power-aware Btor2 instrumentation: `BoolExpr` lowering, IEEE 1801 `SIMSTATE`
//! (`CORRUPT`, `CORRUPT_ON_ACTIVITY`, `CORRUPT_STATE_ON_ACTIVITY`) domain
//! predicates, and `lp.pd.<domain>.{on,iso,ret,corrupt}` VCD trace outputs.

use std::collections::{HashMap, HashSet};

use informal_ir::{Model, Op};
use informal_svlang::struct_graph::StructuralGraph;
use informal_upf::{PowerDomainId, ResolvedPowerIntent};

pub use super::btor_emit::{sanitize_sym, BtorEmitter};
use super::power_expr::{extract_domain_controls, BoolExpr, DomainPowerControls};
use crate::lp::static_rtl::assign_cell_domains;
use crate::miter::parse_full_model;

/// Resolves a UPF or SV hierarchical signal name to `(node_ref, width)` in `m`.
pub fn resolve_btor_sig(m: &Model, top: Option<&str>, raw: &str) -> Option<(i64, u32)> {
    let norm = raw.trim().trim_start_matches('/').replace('/', ".");
    if norm.is_empty() {
        return None;
    }
    let mut cands = vec![norm.clone()];
    if let Some(t) = top.filter(|t| !t.is_empty()) {
        if let Some(rest) = norm.strip_prefix(t).and_then(|r| r.strip_prefix('.')) {
            cands.push(rest.to_string());
        } else {
            cands.push(format!("{t}.{norm}"));
        }
    }
    for cand in &cands {
        for l in m.lines() {
            if matches!(l.op, Op::Input | Op::State) && l.symbol.as_deref() == Some(cand.as_str()) {
                if let Some(w) = l.sort.and_then(|s| m.width(s)) {
                    return Some((l.id as i64, w));
                }
            }
        }
        for l in m.lines() {
            if l.op == Op::Output && l.symbol.as_deref() == Some(cand.as_str()) {
                if let Some(&arg) = l.args.first() {
                    if let Some(base) = m.line(arg.unsigned_abs() as u32) {
                        if let Some(w) = base.sort.and_then(|s| m.width(s)) {
                            return Some((arg, w));
                        }
                    }
                }
            }
        }
    }
    None
}

/// Compiles a UPF [`BoolExpr`] to a 1-bit Btor2 node in `em`.
pub fn compile_bool_expr(
    em: &mut BtorEmitter<'_>,
    expr: &BoolExpr,
    scope: &str,
    top: Option<&str>,
) -> Result<i64, String> {
    let raw = compile_expr_raw(em, expr, scope, top)?;
    Ok(em.to_bool(raw))
}

fn compile_expr_raw(
    em: &mut BtorEmitter<'_>,
    expr: &BoolExpr,
    scope: &str,
    top: Option<&str>,
) -> Result<i64, String> {
    match expr {
        BoolExpr::ConstBool(true) => Ok(em.const_one(1)),
        BoolExpr::ConstBool(false) => Ok(em.const_zero(1)),
        BoolExpr::Lit { val, width } => Ok(em.const_u64(*width, *val)),
        BoolExpr::Sig(s) => {
            let norm_scope = scope.trim_matches('/').replace('/', ".");
            if let Some((nref, _)) = resolve_btor_sig(em.base, top, s) {
                return Ok(nref);
            }
            if !norm_scope.is_empty() && !s.contains('.') && !s.contains('/') {
                let scoped = format!("{norm_scope}.{s}");
                if let Some((nref, _)) = resolve_btor_sig(em.base, top, &scoped) {
                    return Ok(nref);
                }
            }
            Err(format!(
                "LP formal: signal `{s}` not found in lowered Btor2 model"
            ))
        }
        BoolExpr::Not(inner) => {
            let v = compile_bool_expr(em, inner, scope, top)?;
            Ok(em.not1(v))
        }
        BoolExpr::And(a, b) => {
            let va = compile_bool_expr(em, a, scope, top)?;
            let vb = compile_bool_expr(em, b, scope, top)?;
            Ok(em.and1(va, vb))
        }
        BoolExpr::Or(a, b) => {
            let va = compile_bool_expr(em, a, scope, top)?;
            let vb = compile_bool_expr(em, b, scope, top)?;
            Ok(em.or1(va, vb))
        }
        BoolExpr::Xor(a, b) => {
            let va = compile_bool_expr(em, a, scope, top)?;
            let vb = compile_bool_expr(em, b, scope, top)?;
            Ok(em.binary("xor", 1, va, vb))
        }
        BoolExpr::Eq(a, b) | BoolExpr::Neq(a, b) => {
            let op = if matches!(expr, BoolExpr::Eq(_, _)) {
                "eq"
            } else {
                "neq"
            };
            let (va, vb) = compile_comparable_pair(em, a, b, scope, top)?;
            Ok(em.binary(op, 1, va, vb))
        }
    }
}

fn compile_comparable_pair(
    em: &mut BtorEmitter<'_>,
    a: &BoolExpr,
    b: &BoolExpr,
    scope: &str,
    top: Option<&str>,
) -> Result<(i64, i64), String> {
    match (a, b) {
        (BoolExpr::Lit { val, .. }, other) => {
            let vb = compile_expr_raw(em, other, scope, top)?;
            let w = em.width_of(vb).unwrap_or(1);
            let va = em.const_u64(w, *val);
            Ok((va, vb))
        }
        (other, BoolExpr::Lit { val, .. }) => {
            let va = compile_expr_raw(em, other, scope, top)?;
            let w = em.width_of(va).unwrap_or(1);
            let vb = em.const_u64(w, *val);
            Ok((va, vb))
        }
        (BoolExpr::ConstBool(flag), other) => {
            let vb = compile_bool_expr(em, other, scope, top)?;
            let va = if *flag {
                em.const_one(1)
            } else {
                em.const_zero(1)
            };
            Ok((va, vb))
        }
        (other, BoolExpr::ConstBool(flag)) => {
            let va = compile_bool_expr(em, other, scope, top)?;
            let vb = if *flag {
                em.const_one(1)
            } else {
                em.const_zero(1)
            };
            Ok((va, vb))
        }
        _ => {
            let va = compile_expr_raw(em, a, scope, top)?;
            let vb = compile_expr_raw(em, b, scope, top)?;
            let wa = em.width_of(va).unwrap_or(1);
            let wb = em.width_of(vb).unwrap_or(1);
            if wa == wb {
                Ok((va, vb))
            } else {
                Ok((em.to_bool(va), em.to_bool(vb)))
            }
        }
    }
}

/// Synthesized Btor2 power-state signals for one UPF power domain.
#[derive(Debug, Clone)]
pub struct DomainBtorSignals {
    pub pd_id: PowerDomainId,
    pub name: String,
    pub on: i64,
    pub iso: i64,
    pub ret: i64,
    /// Unconditional power-off corruption OR `CORRUPT_ON_ACTIVITY` trigger.
    pub corrupt: i64,
    /// State corruption (`corrupt` OR `CORRUPT_STATE_ON_ACTIVITY` trigger).
    pub state_corrupt: i64,
    pub ret_save: Option<i64>,
    pub ret_restore: Option<i64>,
    pub ret_backup_on: Option<i64>,
    pub iso_supply_on: Option<i64>,
    pub is_corruptible: bool,
}

/// Synthesizes `lp.pd.<domain>.{on,iso,ret,corrupt}` nodes and trace outputs
/// for every domain in `resolved`. When `reset_steps > 0`, gates `corrupt`,
/// `state_corrupt`, `ret_save`, and `ret_restore` with a `post_reset` shift
/// register so pre-reset uninitialized PMU registers never inject corruption
/// before hardware reset completes.
pub fn synthesize_domain_signals(
    em: &mut BtorEmitter<'_>,
    graph: &StructuralGraph,
    resolved: &ResolvedPowerIntent,
    top: Option<&str>,
    include_activity_monitors: bool,
    reset_steps: u32,
) -> Vec<DomainBtorSignals> {
    let controls = extract_domain_controls(resolved);
    let cell_domain = assign_cell_domains(graph, resolved);
    let raw_edges = if include_activity_monitors {
        graph.extract_crossings(&|c| cell_domain[c.0 as usize].map(|d| d.0))
    } else {
        Vec::new()
    };

    let post_reset = if reset_steps > 0 {
        let z1 = em.const_zero(1);
        let o1 = em.const_one(1);
        let mut cur = o1;
        for step in 0..reset_steps {
            let st = em.alloc_state(1, &format!("informal_lp_post_rst_{step}"));
            em.emit_init(1, st, z1);
            em.emit_next(1, st, cur);
            cur = st as i64;
        }
        Some(cur)
    } else {
        None
    };

    let mut out = Vec::with_capacity(controls.len());
    for ctrl in &controls {
        let on = ctrl
            .on_expr
            .as_ref()
            .and_then(|e| compile_bool_expr(em, e, &ctrl.scope, top).ok())
            .unwrap_or_else(|| em.const_one(1));

        let iso = ctrl
            .iso_active_expr
            .as_ref()
            .and_then(|e| compile_bool_expr(em, e, &ctrl.scope, top).ok())
            .unwrap_or_else(|| em.const_zero(1));

        let raw_ret_save = ctrl
            .ret_save_expr
            .as_ref()
            .and_then(|e| compile_bool_expr(em, e, &ctrl.scope, top).ok());
        let raw_ret_restore = ctrl
            .ret_restore_expr
            .as_ref()
            .and_then(|e| compile_bool_expr(em, e, &ctrl.scope, top).ok());
        let ret_save = raw_ret_save.map(|s| match post_reset {
            Some(pr) => em.and1(pr, s),
            None => s,
        });
        let ret_restore = raw_ret_restore.map(|r| match post_reset {
            Some(pr) => em.and1(pr, r),
            None => r,
        });
        let ret_backup_on = ctrl
            .ret_backup_on_expr
            .as_ref()
            .and_then(|e| compile_bool_expr(em, e, &ctrl.scope, top).ok());
        let iso_supply_on = ctrl
            .iso_supply_on_expr
            .as_ref()
            .and_then(|e| compile_bool_expr(em, e, &ctrl.scope, top).ok());

        let has_ret = raw_ret_save.is_some() || raw_ret_restore.is_some();
        let ret = if has_ret {
            let mut terms = Vec::new();
            if let Some(s) = raw_ret_save {
                terms.push(s);
            }
            if let Some(r) = raw_ret_restore {
                terms.push(r);
            }
            if ctrl.on_expr.is_some() {
                let off = em.not1(on);
                terms.push(off);
            }
            em.any1(&terms)
        } else {
            em.const_zero(1)
        };

        let uncond_corrupt = ctrl
            .corrupt_expr
            .as_ref()
            .and_then(|e| compile_bool_expr(em, e, &ctrl.scope, top).ok())
            .unwrap_or_else(|| em.const_zero(1));

        let mut corrupt = uncond_corrupt;
        let mut state_corrupt = uncond_corrupt;

        if include_activity_monitors && (ctrl.coa_expr.is_some() || ctrl.csoa_expr.is_some()) {
            let inputs = collect_domain_input_nodes(
                em.base,
                graph,
                &cell_domain,
                &raw_edges,
                ctrl.pd_id,
                top,
            );
            let any_chg = build_input_activity_detector(em, &ctrl.name, &inputs);
            if let Some(ref coa_e) = ctrl.coa_expr {
                if let Ok(in_coa) = compile_bool_expr(em, coa_e, &ctrl.scope, top) {
                    let coa_trig =
                        build_stay_in_state_trigger(em, &ctrl.name, "coa", in_coa, any_chg);
                    corrupt = em.or1(corrupt, coa_trig);
                    state_corrupt = em.or1(state_corrupt, coa_trig);
                }
            }
            if let Some(ref csoa_e) = ctrl.csoa_expr {
                if let Ok(in_csoa) = compile_bool_expr(em, csoa_e, &ctrl.scope, top) {
                    let csoa_trig =
                        build_stay_in_state_trigger(em, &ctrl.name, "csoa", in_csoa, any_chg);
                    state_corrupt = em.or1(state_corrupt, csoa_trig);
                }
            }
        }

        if let Some(pr) = post_reset {
            corrupt = em.and1(pr, corrupt);
            state_corrupt = em.and1(pr, state_corrupt);
        }

        // Emit VCD swimlane outputs: `lp.pd.<domain>.{on,iso,ret,corrupt}`.
        let dom = sanitize_sym(&ctrl.name);
        em.add_output(on, format!("lp.pd.{dom}.on"));
        em.add_output(iso, format!("lp.pd.{dom}.iso"));
        em.add_output(ret, format!("lp.pd.{dom}.ret"));
        em.add_output(corrupt, format!("lp.pd.{dom}.corrupt"));

        out.push(DomainBtorSignals {
            pd_id: ctrl.pd_id,
            name: ctrl.name.clone(),
            on,
            iso,
            ret,
            corrupt,
            state_corrupt,
            ret_save,
            ret_restore,
            ret_backup_on,
            iso_supply_on,
            is_corruptible: ctrl.is_corruptible(),
        });
    }
    out
}

fn collect_domain_input_nodes(
    m: &Model,
    graph: &StructuralGraph,
    cell_domain: &[Option<PowerDomainId>],
    raw_edges: &[informal_svlang::struct_graph::CrossingEdge],
    pd_id: PowerDomainId,
    top: Option<&str>,
) -> Vec<(i64, u32)> {
    let mut nodes = Vec::new();
    let mut seen = HashSet::new();
    let is_clk_or_rst = |name: &str| {
        let low = name
            .rsplit(['.', '/'])
            .next()
            .unwrap_or(name)
            .to_ascii_lowercase();
        low == "clk"
            || low == "clock"
            || low.ends_with("_clk")
            || low.starts_with("clk_")
            || low == "rst"
            || low == "rst_n"
            || low == "reset"
            || low == "reset_n"
            || low.ends_with("_rst_n")
            || low.ends_with("_rst")
    };

    for edge in raw_edges {
        let dst_dom = cell_domain[edge.dst_cell.0 as usize];
        let src_dom = cell_domain[edge.src_cell.0 as usize];
        if dst_dom != Some(pd_id) || src_dom == Some(pd_id) {
            continue;
        }
        let dst_net = graph.net(edge.dst_net);
        let src_net = graph.net(edge.src_net);
        if is_clk_or_rst(&dst_net.full_path) || is_clk_or_rst(&src_net.full_path) {
            continue;
        }
        let resolved = resolve_btor_sig(m, top, &dst_net.full_path)
            .or_else(|| resolve_btor_sig(m, top, &src_net.full_path));
        if let Some((nref, w)) = resolved {
            if seen.insert(nref) {
                nodes.push((nref, w));
            }
        }
    }
    nodes
}

fn build_input_activity_detector(
    em: &mut BtorEmitter<'_>,
    dom_name: &str,
    inputs: &[(i64, u32)],
) -> i64 {
    let mut chgs = Vec::new();
    let dom = sanitize_sym(dom_name);
    for (idx, &(nref, w)) in inputs.iter().enumerate() {
        let prev = em.alloc_state(w, &format!("informal_lp_prev_{dom}_{idx}"));
        let z = em.const_zero(w);
        em.emit_init(w, prev, z);
        em.emit_next(w, prev, nref);
        let diff = em.binary("neq", 1, nref, prev as i64);
        chgs.push(diff);
    }
    em.any1(&chgs)
}

fn build_stay_in_state_trigger(
    em: &mut BtorEmitter<'_>,
    dom_name: &str,
    tag: &str,
    in_state: i64,
    any_chg: i64,
) -> i64 {
    let dom = sanitize_sym(dom_name);
    let prev_st = em.alloc_state(1, &format!("informal_lp_{tag}_prev_{dom}"));
    let z1 = em.const_zero(1);
    em.emit_init(1, prev_st, z1);
    em.emit_next(1, prev_st, in_state);
    let stayed = em.and1(in_state, prev_st as i64);
    em.and1(stayed, any_chg)
}

/// Pure observer instrumentation: injects `lp.pd.<dom>.{on,iso,ret,corrupt}`
/// trace outputs into an existing lowered Btor2 model without modifying any
/// `bad`, `constraint`, or `next` line.
pub fn inject_lp_domain_trace_signals(
    base_btor: &str,
    graph: &StructuralGraph,
    resolved: &ResolvedPowerIntent,
    top: Option<&str>,
) -> Result<String, String> {
    let m = parse_full_model(base_btor)?;
    let mut em = BtorEmitter::new(&m);
    let _ = synthesize_domain_signals(&mut em, graph, resolved, top, false, 0);
    Ok(em.finish(&HashMap::new(), &HashMap::new(), true))
}

/// Finds domain controls by `PowerDomainId`.
pub fn domain_sig_by_id(
    sigs: &[DomainBtorSignals],
    pd_id: PowerDomainId,
) -> Option<&DomainBtorSignals> {
    sigs.iter().find(|d| d.pd_id == pd_id)
}

/// Helper re-export for extracting domain power controls.
pub fn all_domain_controls(resolved: &ResolvedPowerIntent) -> Vec<DomainPowerControls> {
    extract_domain_controls(resolved)
}

pub(crate) fn domain_of_signal_path(
    graph: &StructuralGraph,
    cell_domain: &[Option<PowerDomainId>],
    top: Option<&str>,
    sym: &str,
) -> Option<PowerDomainId> {
    let rel = strip_top_prefix(sym, top);
    let mut best: Option<(usize, PowerDomainId)> = None;
    for cell in &graph.cells {
        let Some(pd) = cell_domain[cell.id.0 as usize] else {
            continue;
        };
        if cell.is_top {
            if best.is_none() {
                best = Some((0, pd));
            }
            continue;
        }
        let cdot = cell.upf_path.replace('/', ".");
        let matches = sym == cell.path
            || sym.starts_with(&format!("{}.", cell.path))
            || rel == cdot
            || rel.starts_with(&format!("{cdot}."));
        if matches {
            let depth = cdot.len() + 1;
            if best.is_none_or(|(d, _)| depth > d) {
                best = Some((depth, pd));
            }
        }
    }
    best.map(|(_, pd)| pd)
}

pub(crate) fn strip_top_prefix<'a>(sym: &'a str, top: Option<&str>) -> &'a str {
    match top.filter(|t| !t.is_empty()) {
        Some(t) => sym
            .strip_prefix(t)
            .and_then(|r| r.strip_prefix('.'))
            .unwrap_or(sym),
        None => sym,
    }
}

pub(crate) fn is_helper_state(sym: &str) -> bool {
    let leaf = sym.rsplit('.').next().unwrap_or(sym);
    sym.starts_with("informal.")
        || sym.starts_with("lp.")
        || leaf.starts_with("informal_")
        || leaf.starts_with("_sva_")
        || leaf.starts_with("_past_")
        || leaf.starts_with("_was_")
        || leaf.starts_with("_clk_")
        || leaf.starts_with("miter_")
}
