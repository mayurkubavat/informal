//! Power-aware Btor2 isolation, retention, and domain-crossing boundary
//! transformation (`PaModel`).
//!
//! Supports three modes:
//! - [`PaMode::TwoCopy`]: emits UPF isolation clamping (`0`/`1`/`latch`),
//!   retention shadow flops, and [`CondDivergence`] plans for exact 2-copy
//!   self-composition corruption proofs (`-prove_corruption`, `-prove_waivers`).
//! - [`PaMode::SingleCopySec`]: emits UPF isolation clamping, retention shadow
//!   flops, AND inline fresh corruption inputs on power-off for power-aware
//!   Sequential Equivalence Checking (`-sec`).
//! - [`PaMode::IsoClampOnly`]: emits deterministic UPF isolation clamping only
//!   for combinational RTL+UPF vs Netlist+UPF LP-LEC (`-lec`).

use std::collections::{HashMap, HashSet, VecDeque};

use informal_ir::{Model, Op, Sort};
use informal_svlang::struct_graph::{
    split_crossing_group_by_cuts, CrossingGroup, ExprOpSummary, NetSegId, StructuralGraph,
};
use informal_upf::{
    endpoint_base_matches, glob, AppliesTo, ClampValue, IsolationStrategy, PowerDomainId,
    ResolvedPowerIntent, RetentionStrategy, SignalEndpoint, StrategyMatch,
};

use super::pa_instrument::{
    compile_bool_expr, domain_sig_by_id, resolve_btor_sig, sanitize_sym, synthesize_domain_signals,
    BtorEmitter, DomainBtorSignals,
};
use crate::lp::static_rtl::{assign_cell_domains, collect_strategy_slice_cuts};
use crate::miter::{parse_full_model, CondDivergence};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaMode {
    TwoCopy,
    SingleCopySec,
    IsoClampOnly,
}

#[derive(Debug, Clone)]
pub struct RetainedRegInfo {
    pub reg_path: String,
    pub short_name: String,
    pub domain_name: String,
    pub pd_id: PowerDomainId,
    pub state_id: u32,
    pub shadow_id: u32,
    pub width: u32,
    pub save_cond: Option<i64>,
    pub restore_cond: Option<i64>,
    pub domain_on: i64,
    pub domain_iso: i64,
}

#[derive(Debug, Clone)]
pub struct CrossingBoundaryInfo {
    pub signal: String,
    pub src_net: NetSegId,
    pub dst_net: NetSegId,
    pub src_domain: PowerDomainId,
    pub dst_domain: PowerDomainId,
    pub src_domain_name: String,
    pub dst_domain_name: String,
    pub has_iso: bool,
    pub boundary_node: i64,
    pub width: u32,
    pub corrupt_cond: i64,
    pub divergence_node: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct PaModel {
    pub btor2: String,
    pub domains: Vec<DomainBtorSignals>,
    pub retained_regs: Vec<RetainedRegInfo>,
    pub non_retained_states: HashMap<u32, (String, String)>, // state_id -> (reg_path, domain_name)
    pub crossings: Vec<CrossingBoundaryInfo>,
    pub cond_divergences: Vec<CondDivergence>,
    pub cond_state_corruptions: Vec<CondDivergence>,
}

/// Instruments `base_btor` with UPF domain power states, isolation clamping,
/// retention shadow flops, and corruption divergence hooks.
pub fn build_pa_model(
    base_btor: &str,
    graph: &StructuralGraph,
    resolved: &ResolvedPowerIntent,
    top: Option<&str>,
    mode: PaMode,
    reset_steps: u32,
) -> Result<PaModel, String> {
    let m = parse_full_model(base_btor)?;
    let cell_domain = assign_cell_domains(graph, resolved);
    let mut em = BtorEmitter::new(&m);

    let include_activity = mode != PaMode::IsoClampOnly;
    let domains =
        synthesize_domain_signals(&mut em, graph, resolved, top, include_activity, reset_steps);

    let mut next_overrides: HashMap<u32, i64> = HashMap::new();
    let mut output_overrides: HashMap<String, i64> = HashMap::new();
    let mut cond_divergences: Vec<CondDivergence> = Vec::new();
    let mut cond_state_corruptions: Vec<CondDivergence> = Vec::new();
    let mut retained_regs: Vec<RetainedRegInfo> = Vec::new();
    let mut non_retained_states: HashMap<u32, (String, String)> = HashMap::new();

    // Pre-index original state Init / Next lines.
    let mut orig_init: HashMap<u32, i64> = HashMap::new();
    let mut orig_next: HashMap<u32, i64> = HashMap::new();
    for l in m.lines() {
        match l.op {
            Op::Init if l.args.len() >= 2 => {
                orig_init.insert(l.args[0].unsigned_abs() as u32, l.args[1]);
            }
            Op::Next if l.args.len() >= 2 => {
                orig_next.insert(l.args[0].unsigned_abs() as u32, l.args[1]);
            }
            _ => {}
        }
    }

    // Resolve computed/gated-clock backing states (`informal_reg_K` / `informal_data_K`)
    // back to their real RTL register symbols via `; informal-trace-outputs` `output` lines.
    let mut state_effective_sym: HashMap<u32, String> = HashMap::new();
    for l in m.with_op(Op::Output) {
        let (Some(&arg), Some(sym)) = (l.args.first(), l.symbol.as_deref()) else {
            continue;
        };
        if is_helper_state(sym) {
            continue;
        }
        // Only map outputs that correspond to a real sequential flop/latch in `graph`
        // (or a direct `ite` wrapping `informal_reg_*`).
        if let Some(root) = m.line(arg.unsigned_abs() as u32) {
            if root.op == Op::Ite {
                for &a in root.args.iter().skip(1) {
                    let aid = a.unsigned_abs() as u32;
                    if let Some(st) = m.line(aid) {
                        if st.op == Op::State {
                            if let Some(st_sym) = st.symbol.as_deref() {
                                if st_sym.starts_with("informal_reg_")
                                    || st_sym.starts_with("informal_data_")
                                {
                                    state_effective_sym
                                        .entry(aid)
                                        .or_insert_with(|| sym.to_string());
                                }
                            }
                        }
                    }
                }
            } else if root.op == Op::State {
                if let Some(st_sym) = root.symbol.as_deref() {
                    if st_sym.starts_with("informal_reg_") || st_sym.starts_with("informal_data_") {
                        state_effective_sym
                            .entry(root.id)
                            .or_insert_with(|| sym.to_string());
                    }
                }
            }
        }
    }

    // Map each base Op::State to its PowerDomainId.
    let mut state_domain: HashMap<u32, PowerDomainId> = HashMap::new();
    for l in m.with_op(Op::State) {
        let eff_sym = state_effective_sym
            .get(&l.id)
            .map(String::as_str)
            .or(l.symbol.as_deref());
        let Some(sym) = eff_sym else {
            continue;
        };
        if is_helper_state(sym) {
            continue;
        }
        if let Some(pd_id) = domain_of_signal_path(graph, &cell_domain, top, sym) {
            state_domain.insert(l.id, pd_id);
        }
    }

    // 1. Instrument sequential states (retention shadow flops & state corruption).
    if mode != PaMode::IsoClampOnly {
        for l in m.with_op(Op::State) {
            let Some(&pd_id) = state_domain.get(&l.id) else {
                continue;
            };
            let Some(dom) = domain_sig_by_id(&domains, pd_id) else {
                continue;
            };
            if !dom.is_corruptible {
                continue;
            }
            let Some(orig_srt) = l.sort else { continue };
            let Some(Sort::BitVec(w)) = m.sort(orig_srt) else {
                continue;
            };
            let sym = state_effective_sym
                .get(&l.id)
                .map(String::as_str)
                .or(l.symbol.as_deref())
                .unwrap_or("st");
            let rel = strip_top_prefix(sym, top);
            let rel_upf = rel.replace('.', "/");
            let clean = sanitize_sym(rel);
            let onext = orig_next.get(&l.id).copied().unwrap_or(l.id as i64);

            if let Some(_ret_strat) = match_retention_for_state(resolved, pd_id, sym, &rel_upf) {
                let sh_id = em.alloc_state(w, &format!("lp.ret.{clean}"));
                let sh_init = orig_init
                    .get(&l.id)
                    .copied()
                    .unwrap_or_else(|| em.const_zero(w));
                em.emit_init(w, sh_id, sh_init);

                let save_cond = dom.ret_save.map(|sv| {
                    let s1 = em.and1(sv, dom.on);
                    if let Some(bk) = dom.ret_backup_on {
                        em.and1(s1, bk)
                    } else {
                        s1
                    }
                });
                let mut sh_next = match save_cond {
                    Some(sc) => em.ite(w, sc, l.id as i64, sh_id as i64),
                    None => sh_id as i64,
                };

                if let Some(bk) = dom.ret_backup_on {
                    let bk_off = em.not1(bk);
                    match mode {
                        PaMode::TwoCopy => {
                            cond_state_corruptions.push(CondDivergence {
                                node_id: sh_id,
                                corrupt_cond: bk_off,
                                label: format!("ret_bk_{clean}"),
                            });
                        }
                        PaMode::SingleCopySec => {
                            let xi = em.alloc_input(w, &format!("informal_lp_retbk_{clean}"));
                            sh_next = em.ite(w, bk_off, xi as i64, sh_next);
                        }
                        PaMode::IsoClampOnly => {}
                    }
                }
                em.emit_next(w, sh_id, sh_next);

                let restore_cond = dom.ret_restore.map(|rs| em.and1(rs, dom.on));
                let mut prim_next = match restore_cond {
                    Some(rc) => em.ite(w, rc, sh_id as i64, onext),
                    None => onext,
                };
                match mode {
                    PaMode::TwoCopy => {
                        cond_state_corruptions.push(CondDivergence {
                            node_id: l.id,
                            corrupt_cond: dom.state_corrupt,
                            label: format!("ret_prim_{clean}"),
                        });
                    }
                    PaMode::SingleCopySec => {
                        let xi = em.alloc_input(w, &format!("informal_lp_retp_{clean}"));
                        prim_next = em.ite(w, dom.state_corrupt, xi as i64, prim_next);
                    }
                    PaMode::IsoClampOnly => {}
                }
                next_overrides.insert(l.id, prim_next);

                retained_regs.push(RetainedRegInfo {
                    reg_path: rel.to_string(),
                    short_name: sanitize_sym(rel),
                    domain_name: dom.name.clone(),
                    pd_id,
                    state_id: l.id,
                    shadow_id: sh_id,
                    width: w,
                    save_cond,
                    restore_cond,
                    domain_on: dom.on,
                    domain_iso: dom.iso,
                });
            } else {
                non_retained_states.insert(l.id, (rel.to_string(), dom.name.clone()));
                match mode {
                    PaMode::TwoCopy => {
                        cond_state_corruptions.push(CondDivergence {
                            node_id: l.id,
                            corrupt_cond: dom.state_corrupt,
                            label: format!("nonret_{clean}"),
                        });
                    }
                    PaMode::SingleCopySec => {
                        let xi = em.alloc_input(w, &format!("informal_lp_nonret_{clean}"));
                        let cnext = em.ite(w, dom.state_corrupt, xi as i64, onext);
                        next_overrides.insert(l.id, cnext);
                    }
                    PaMode::IsoClampOnly => {}
                }
            }
        }
    }

    // 2. Instrument domain crossings (isolation clamps + boundary corruption).
    let raw_edges = graph.extract_crossings(&|c| cell_domain[c.0 as usize].map(|d| d.0));
    let groups = graph.group_crossings(&raw_edges);

    // Map `(dst_domain, src_raw_node_id) -> boundary_node_ref` for non-constant sources.
    let mut dst_node_subst: HashMap<(PowerDomainId, u32), i64> = HashMap::new();
    let mut crossings: Vec<CrossingBoundaryInfo> = Vec::new();
    let mut seen_cross: HashSet<(PowerDomainId, PowerDomainId, NetSegId, NetSegId)> = HashSet::new();

    for group in &groups {
        let iso_cuts = collect_strategy_slice_cuts(graph, resolved, group, true);
        for sub in split_crossing_group_by_cuts(graph, group, &iso_cuts) {
            let Some(src_pd) = cell_domain[sub.src_cell.0 as usize] else {
                continue;
            };
            let Some(dst_pd) = cell_domain[sub.dst_cell.0 as usize] else {
                continue;
            };
            if src_pd == dst_pd {
                continue;
            }
            let Some(src_dom) = domain_sig_by_id(&domains, src_pd).cloned() else {
                continue;
            };
            let Some(dst_dom) = domain_sig_by_id(&domains, dst_pd).cloned() else {
                continue;
            };
            if mode != PaMode::IsoClampOnly && !src_dom.is_corruptible {
                continue;
            }
            if !seen_cross.insert((src_pd, dst_pd, sub.src_base_net, sub.dst_base_net)) {
                continue;
            }

            let src_net = graph.net(sub.src_base_net);
            let dst_net = graph.net(sub.dst_base_net);
            let Some((raw_src_ref, w)) = resolve_btor_sig(&m, top, &src_net.full_path)
                .or_else(|| resolve_btor_sig(&m, top, &dst_net.full_path))
            else {
                continue;
            };

            let iso_match = match_iso_for_crossing(graph, resolved, src_pd, dst_pd, &sub);
            let iso_strat = match iso_match {
                StrategyMatch::Matched(s) => Some(s),
                _ => None,
            };
            let sig_label = sanitize_sym(strip_top_prefix(&src_net.full_path, top));

            // Create a dedicated non-constant boundary carrier when injecting corruption so
            // shared constants (`const 1 1`) or `src_pd` internal readers are never corrupted.
            let (mut boundary_ref, div_node) = if mode == PaMode::IsoClampOnly {
                (raw_src_ref, None)
            } else {
                let carrier = make_carrier_node(&mut em, w, raw_src_ref);
                match mode {
                    PaMode::TwoCopy => {
                        cond_divergences.push(CondDivergence {
                            node_id: carrier,
                            corrupt_cond: src_dom.corrupt,
                            label: format!("cross_{sig_label}"),
                        });
                        (carrier as i64, Some(carrier))
                    }
                    PaMode::SingleCopySec => {
                        let xi = em.alloc_input(w, &format!("informal_lp_cross_{sig_label}"));
                        let cor = em.ite(w, src_dom.corrupt, xi as i64, raw_src_ref);
                        (cor, None)
                    }
                    PaMode::IsoClampOnly => (raw_src_ref, None),
                }
            };

            if let Some(iso) = iso_strat {
                if let Some(iso_en) = compile_iso_enable(&mut em, iso, top) {
                    let clamp_val = match iso.clamp_value.unwrap_or(ClampValue::Zero) {
                        ClampValue::Zero => Some(em.const_zero(w)),
                        ClampValue::One => Some(em.const_ones(w)),
                        ClampValue::Latch => {
                            let lat = em.alloc_state(w, &format!("lp.iso.latch_{sig_label}"));
                            let z = em.const_zero(w);
                            em.emit_init(w, lat, z);
                            // Latch captures the un-corrupted/boundary value while transparent.
                            let lat_next = em.ite(w, iso_en, lat as i64, boundary_ref);
                            em.emit_next(w, lat, lat_next);
                            Some(lat as i64)
                        }
                        ClampValue::Any | ClampValue::Z => None,
                    };
                    if let Some(cv) = clamp_val {
                        boundary_ref = em.ite(w, iso_en, cv, boundary_ref);
                    } else if mode == PaMode::TwoCopy {
                        let any_node = make_carrier_node(&mut em, w, boundary_ref);
                        cond_divergences.push(CondDivergence {
                            node_id: any_node,
                            corrupt_cond: iso_en,
                            label: format!("iso_any_{sig_label}"),
                        });
                        boundary_ref = any_node as i64;
                    }
                    if mode != PaMode::IsoClampOnly {
                        if let Some(iso_ss) = resolved.effective_iso_supply(iso) {
                            if let Some(iso_ok_expr) =
                                super::power_expr::supply_set_on_expr(resolved, iso_ss)
                            {
                                if let Ok(iso_ok) =
                                    compile_bool_expr(&mut em, &iso_ok_expr, &iso.scope, top)
                                {
                                    let iso_off = em.not1(iso_ok);
                                    if mode == PaMode::TwoCopy {
                                        let sup_node = make_carrier_node(&mut em, w, boundary_ref);
                                        cond_divergences.push(CondDivergence {
                                            node_id: sup_node,
                                            corrupt_cond: iso_off,
                                            label: format!("iso_sup_{sig_label}"),
                                        });
                                        boundary_ref = sup_node as i64;
                                    } else {
                                        let xi = em.alloc_input(
                                            w,
                                            &format!("informal_lp_isosup_{sig_label}"),
                                        );
                                        boundary_ref = em.ite(w, iso_off, xi as i64, boundary_ref);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let raw_src_id = raw_src_ref.unsigned_abs() as u32;
            let raw_is_const = m.line(raw_src_id).is_some_and(|l| {
                matches!(
                    l.op,
                    Op::Const | Op::Constd | Op::Consth | Op::Zero | Op::One | Op::Ones
                )
            });
            if !raw_is_const && raw_src_ref > 0 {
                dst_node_subst.insert((dst_pd, raw_src_id), boundary_ref);
            } else {
                // Constant-driven crossing: attach boundary directly to `dst_net` and any
                // same-domain direct identity aliases (e.g., `core_valid = 1'b1; assign valid_o = core_valid;`).
                for alias_path in collect_dst_identity_aliases(graph, &cell_domain, sub.dst_base_net, dst_pd) {
                    output_overrides.insert(alias_path.clone(), boundary_ref);
                    if let Some(rel) = top
                        .and_then(|t| alias_path.strip_prefix(t))
                        .and_then(|r| r.strip_prefix('.'))
                    {
                        output_overrides.insert(rel.to_string(), boundary_ref);
                    }
                }
            }

            crossings.push(CrossingBoundaryInfo {
                signal: strip_top_prefix(&src_net.full_path, top).to_string(),
                src_net: sub.src_base_net,
                dst_net: sub.dst_base_net,
                src_domain: src_pd,
                dst_domain: dst_pd,
                src_domain_name: src_dom.name.clone(),
                dst_domain_name: dst_dom.name.clone(),
                has_iso: iso_strat.is_some(),
                boundary_node: boundary_ref,
                width: w,
                corrupt_cond: src_dom.corrupt,
                divergence_node: div_node,
            });
        }
    }

    // 3. Rewire `dst_pd` state next-expressions and `dst_pd` outputs using memoized domain substitution.
    if !dst_node_subst.is_empty() {
        let mut subst_memo: HashMap<(PowerDomainId, u32), u32> = HashMap::new();

        for (&st_id, &dst_pd) in &state_domain {
            if !dst_node_subst.keys().any(|&(dpd, _)| dpd == dst_pd) {
                continue;
            }
            let cur_next = next_overrides
                .get(&st_id)
                .copied()
                .or_else(|| orig_next.get(&st_id).copied());
            if let Some(nref) = cur_next {
                let rewritten = rewrite_ref_for_domain(
                    &m,
                    &mut em,
                    dst_pd,
                    nref,
                    &dst_node_subst,
                    &mut subst_memo,
                );
                if rewritten != nref {
                    next_overrides.insert(st_id, rewritten);
                }
            }
        }

        for l in m.with_op(Op::Output) {
            let (Some(&arg), Some(sym)) = (l.args.first(), l.symbol.as_deref()) else {
                continue;
            };
            if output_overrides.contains_key(sym) {
                continue;
            }
            let Some(dst_pd) = domain_of_signal_path(graph, &cell_domain, top, sym) else {
                continue;
            };
            if !dst_node_subst.keys().any(|&(dpd, _)| dpd == dst_pd) {
                continue;
            }
            let rewritten = rewrite_ref_for_domain(
                &m,
                &mut em,
                dst_pd,
                arg,
                &dst_node_subst,
                &mut subst_memo,
            );
            if rewritten != arg {
                output_overrides.insert(sym.to_string(), rewritten);
            }
        }
    }

    let btor2 = em.finish(&next_overrides, &output_overrides, false);
    Ok(PaModel {
        btor2,
        domains,
        retained_regs,
        non_retained_states,
        crossings,
        cond_divergences,
        cond_state_corruptions,
    })
}

fn make_carrier_node(em: &mut BtorEmitter<'_>, w: u32, raw_ref: i64) -> u32 {
    let one = em.const_one(1);
    em.ite(w, one, raw_ref, raw_ref).unsigned_abs() as u32
}

fn rewrite_ref_for_domain(
    m: &Model,
    em: &mut BtorEmitter<'_>,
    dst_pd: PowerDomainId,
    nref: i64,
    subst: &HashMap<(PowerDomainId, u32), i64>,
    memo: &mut HashMap<(PowerDomainId, u32), u32>,
) -> i64 {
    let base_id = nref.unsigned_abs() as u32;
    let mapped = rewrite_node_for_domain(m, em, dst_pd, base_id, subst, memo);
    if nref < 0 {
        -(mapped as i64)
    } else {
        mapped as i64
    }
}

fn rewrite_node_for_domain(
    m: &Model,
    em: &mut BtorEmitter<'_>,
    dst_pd: PowerDomainId,
    node_id: u32,
    subst: &HashMap<(PowerDomainId, u32), i64>,
    memo: &mut HashMap<(PowerDomainId, u32), u32>,
) -> u32 {
    if let Some(&sub_ref) = subst.get(&(dst_pd, node_id)) {
        if sub_ref > 0 {
            return sub_ref as u32;
        }
        let w = em.width_of(sub_ref).unwrap_or(1);
        return em.unary("not", w, -sub_ref).unsigned_abs() as u32;
    }
    if let Some(&cached) = memo.get(&(dst_pd, node_id)) {
        return cached;
    }
    let Some(l) = m.line(node_id) else {
        return node_id;
    };
    if matches!(
        l.op,
        Op::Const
            | Op::Constd
            | Op::Consth
            | Op::Zero
            | Op::One
            | Op::Ones
            | Op::Input
            | Op::State
    ) || l.args.is_empty()
    {
        memo.insert((dst_pd, node_id), node_id);
        return node_id;
    }
    let mut new_args = Vec::with_capacity(l.args.len());
    let mut changed = false;
    for &a in &l.args {
        let na = rewrite_ref_for_domain(m, em, dst_pd, a, subst, memo);
        if na != a {
            changed = true;
        }
        new_args.push(na);
    }
    let out_id = if !changed {
        node_id
    } else {
        let srt = l.sort.unwrap();
        let out_w = m.width(srt);
        em.emit_raw_op(l.op, srt, out_w, &new_args, &l.nums)
    };
    memo.insert((dst_pd, node_id), out_id);
    out_id
}

fn collect_dst_identity_aliases(
    graph: &StructuralGraph,
    cell_domain: &[Option<PowerDomainId>],
    start: NetSegId,
    dst_pd: PowerDomainId,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut vis = HashSet::new();
    let mut q = VecDeque::new();
    vis.insert(start);
    q.push_back(start);
    while let Some(nid) = q.pop_front() {
        let seg = graph.net(nid);
        out.push(seg.full_path.clone());
        for ld in &seg.loads {
            if ld.op != ExprOpSummary::Identity {
                continue;
            }
            if let Some(next_net) = ld.dst_net {
                let next_cell = graph.net(next_net).cell;
                if cell_domain[next_cell.0 as usize] == Some(dst_pd) && vis.insert(next_net) {
                    q.push_back(next_net);
                }
            }
        }
    }
    out
}

fn compile_iso_enable(
    em: &mut BtorEmitter<'_>,
    iso: &IsolationStrategy,
    top: Option<&str>,
) -> Option<i64> {
    let sig = iso.isolation_signal.as_deref()?;
    let scope = iso.ctrl_scope.as_deref().unwrap_or(&iso.scope);
    let expr = super::power_expr::BoolExpr::sig_with_sense(sig, iso.isolation_sense);
    compile_bool_expr(em, &expr, scope, top).ok()
}

fn match_iso_for_crossing<'a>(
    graph: &StructuralGraph,
    resolved: &'a ResolvedPowerIntent,
    src_pd: PowerDomainId,
    dst_pd: PowerDomainId,
    group: &CrossingGroup,
) -> StrategyMatch<&'a IsolationStrategy> {
    let src_rngs: Vec<(u32, u32)> = group.src_ranges.iter().map(|r| (r.lo, r.hi)).collect();
    let dst_rngs: Vec<(u32, u32)> = group.dst_ranges.iter().map(|r| (r.lo, r.hi)).collect();
    let src_net = graph.net(group.src_base_net);
    let dst_net = graph.net(group.dst_base_net);
    let mut src_eps = vec![SignalEndpoint {
        upf_path: &src_net.upf_path,
        full_path: &src_net.full_path,
        esc_upf_path: None,
        width: src_net.width,
        packed_bounds: src_net.packed_bounds,
        ranges: &src_rngs,
    }];
    let mut dst_eps = vec![SignalEndpoint {
        upf_path: &dst_net.upf_path,
        full_path: &dst_net.full_path,
        esc_upf_path: None,
        width: dst_net.width,
        packed_bounds: dst_net.packed_bounds,
        ranges: &dst_rngs,
    }];
    let bpin_full;
    if let Some(bp) = group.boundary_pin {
        let bpin = graph.pin(bp);
        let cpath = &graph.cell(bpin.cell).path;
        bpin_full = if cpath.is_empty() {
            bpin.name.clone()
        } else {
            format!("{cpath}.{}", bpin.name)
        };
        let is_dst = bpin.cell == group.dst_cell && bpin.cell != group.src_cell;
        let ep = SignalEndpoint {
            upf_path: &bpin.upf_path,
            full_path: &bpin_full,
            esc_upf_path: None,
            width: bpin.width,
            packed_bounds: bpin.packed_bounds,
            ranges: if is_dst { &dst_rngs } else { &src_rngs },
        };
        if is_dst {
            dst_eps.push(ep);
        } else {
            src_eps.push(ep);
        }
    }
    resolved.match_isolation_crossing(
        Some(src_pd),
        Some(dst_pd),
        &src_eps,
        &dst_eps,
        AppliesTo::Both,
    )
}

fn match_retention_for_state<'a>(
    resolved: &'a ResolvedPowerIntent,
    pd_id: PowerDomainId,
    full_path: &str,
    upf_path: &str,
) -> Option<&'a RetentionStrategy> {
    let mut matched: Option<&'a RetentionStrategy> = None;
    for ret in &resolved.intent.retention_strategies {
        if resolved.resolve_domain(&ret.domain, &ret.scope) != Some(pd_id) {
            continue;
        }
        let excluded = ret
            .exclude_elements
            .iter()
            .any(|p| ret_elem_matches(p, upf_path, full_path));
        if excluded {
            continue;
        }
        let included = ret.elements.is_empty()
            || ret
                .elements
                .iter()
                .any(|p| ret_elem_matches(p, upf_path, full_path));
        if included {
            if ret.no_retention {
                return None;
            }
            matched = Some(ret);
        }
    }
    matched
}

fn ret_elem_matches(pat: &str, upf_path: &str, full_path: &str) -> bool {
    let p = pat.trim().trim_start_matches('/');
    if endpoint_base_matches(p, upf_path, full_path, None) || glob(p, upf_path) {
        return true;
    }
    let p_slash = p.replace('.', "/");
    if p_slash == upf_path || upf_path.starts_with(&format!("{p_slash}/")) {
        return true;
    }
    let leaf = upf_path.rsplit('/').next().unwrap_or(upf_path);
    p_slash == leaf
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

fn is_helper_state(sym: &str) -> bool {
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
