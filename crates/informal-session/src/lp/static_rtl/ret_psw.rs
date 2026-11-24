//! Stage 2 Retention (`RET_*`), Power Switch (`PSW_CTRL_UNPOWERED`), and
//! Top-Level Port Attribute (`PORT_ATTR_MISSING`) rules.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use informal_svlang::struct_graph::{
    format_bit_range, BitRange, DriverOrigin, ExprOpSummary, NetSegId, NetSegKind, SeqElement,
    StructuralGraph,
};
use informal_svlang::PortDir;
use informal_upf::{
    endpoint_base_matches, parse_bit_slice_expr, sv_slice_to_phys_range, AppliesTo,
    IsolationStrategy, PowerDomainId, RetentionStrategy, SignalEndpoint, StrategyMatch,
};

use super::{
    iso_rank, pin_full_path, resolve_upf_signal_range_in_scope, strat_ref, sv_escaped_upf_variant,
    Stage2Ctx, ViolationSink,
};

pub(crate) fn check_retention(ctx: &mut Stage2Ctx<'_>, sink: &mut ViolationSink) {
    // Gather all sequential elements per domain.
    let mut dom_flops: Vec<Vec<&SeqElement>> = vec![Vec::new(); ctx.resolved.intent.domains.len()];
    for cell in &ctx.graph.cells {
        if let Some(pd) = ctx.domain_of_cell(cell.id) {
            for fl in &cell.flops {
                dom_flops[pd.0 as usize].push(fl);
            }
            for la in &cell.latches {
                dom_flops[pd.0 as usize].push(la);
            }
        }
    }

    let mut matched_ret_strats: HashSet<usize> = HashSet::new();

    for pd in &ctx.resolved.intent.domains {
        let Some(prim_ss) = ctx.resolved.primary_ss(pd.id) else {
            continue;
        };
        let flops = &dom_flops[pd.id.0 as usize];
        for &fl in flops {
            let m = ctx.resolved.match_retention(pd.id, &fl.upf_path);
            if let StrategyMatch::Matched(ret) = m {
                matched_ret_strats.insert(ret.id.0 as usize);
                check_retained_flop(ctx, pd.id, &pd.name, prim_ss, fl, ret, sink);
            }
        }
    }

    // Also check active retention strategies with explicit `-elements` that matched 0 flops.
    for ret in &ctx.resolved.intent.retention_strategies {
        if ret.no_retention {
            continue;
        }
        let Some(pd_id) = ctx.resolved.resolve_domain(&ret.domain, &ret.scope) else {
            continue;
        };
        let Some(prim_ss) = ctx.resolved.primary_ss(pd_id) else {
            continue;
        };
        let pd_name = ctx.domain_name(pd_id);
        // Always check backup supply & save/restore control even if no flops matched yet.
        check_ret_strategy_supplies(ctx, &pd_name, prim_ss, ret, None, sink);

        if !ret.elements.is_empty() && !matched_ret_strats.contains(&(ret.id.0 as usize)) {
            sink.push(
                "RET_NO_ELEMENTS",
                None,
                Some(pd_name.clone()),
                None,
                ret.name.clone(),
                1,
                1,
                format!(
                    "retention strategy `{}` on domain `{pd_name}` matched 0 sequential elements",
                    ret.name
                ),
                ret.loc.clone(),
                None,
                None,
            );
        }
    }
}

fn check_retained_flop(
    ctx: &mut Stage2Ctx<'_>,
    pd_id: PowerDomainId,
    pd_name: &str,
    prim_ss: informal_upf::SupplySetId,
    fl: &SeqElement,
    ret: &RetentionStrategy,
    sink: &mut ViolationSink,
) {
    let rtl_loc = ctx.graph.format_loc(fl.loc);
    check_ret_strategy_supplies(ctx, pd_name, prim_ss, ret, rtl_loc.as_deref(), sink);

    let Some(ret_ss) = ctx.resolved.effective_ret_supply(ret) else {
        return;
    };

    // Check `RET_ASYNC_RST_UNISOLATED`: if retained flop's async reset originates from or
    // traverses a domain that can turn OFF while `ret_ss` is ON, and any path to `rst_net`
    // bypasses isolation.
    if let Some(rst_net) = fl.async_reset {
        let rst_seg = ctx.graph.net(rst_net);
        let w = rst_seg.width;
        let rst_range = fl.async_reset_range.unwrap_or_else(|| BitRange::full(w));
        let cone = ctx.graph.trace_ctrl_cone(rst_net, rst_range, 64);
        let cone_set: HashSet<NetSegId> = cone.cone_nets.iter().copied().collect();
        let adj = build_cone_forward_adj(ctx.graph, &cone.cone_nets, &cone_set, &cone.net_ranges);
        let rst_disp = format_bit_range(&rst_seg.upf_path, w, rst_seg.packed_bounds, rst_range);
        let mut seen_sw_doms: HashSet<PowerDomainId> = HashSet::new();

        for &nid in &cone.cone_nets {
            let dseg = ctx.graph.net(nid);
            let Some(rst_pd) = ctx.domain_of_cell(dseg.cell) else {
                continue;
            };
            if seen_sw_doms.contains(&rst_pd) {
                continue;
            }
            if rst_pd == pd_id {
                let active_ranges = cone.net_ranges.get(&nid);
                let has_internal_driver = dseg.drivers.iter().any(|drv| {
                    active_ranges.is_none_or(|rs| rs.iter().any(|r| drv.dst_range.overlaps(*r)))
                        && !matches!(
                            drv.origin,
                            DriverOrigin::PrimaryInput(_)
                                | DriverOrigin::ParentPort(_)
                                | DriverOrigin::ChildPin(_)
                                | DriverOrigin::Supply
                        )
                });
                if !has_internal_driver {
                    continue;
                }
            }
            let Some(rst_ss) = ctx.resolved.primary_ss(rst_pd) else {
                continue;
            };
            if ctx.resolved.same_func_supply_class(rst_ss, ret_ss) {
                continue;
            }
            let Some(witness) = ctx.sat.can_be_off_while_on(rst_ss, ret_ss) else {
                continue;
            };
            if has_unisolated_path_to_sink(ctx, &adj, nid, rst_net, rst_pd, pd_id, &cone.net_ranges)
            {
                seen_sw_doms.insert(rst_pd);
                let rst_dom = ctx.domain_name(rst_pd);
                sink.push(
                    "RET_ASYNC_RST_UNISOLATED",
                    None,
                    Some(rst_dom.clone()),
                    Some(pd_name.to_string()),
                    rst_disp.clone(),
                    rst_range.width().max(1),
                    1,
                    format!(
                        "retained flop `{}` async reset `{}` from switchable domain `{rst_dom}` is unisolated while retention backup is active",
                        fl.upf_path, rst_disp
                    ),
                    ret.loc.clone(),
                    rtl_loc.clone(),
                    Some(witness),
                );
            }
        }
    }
}

fn build_cone_forward_adj(
    graph: &StructuralGraph,
    cone_nets: &[NetSegId],
    cone_set: &HashSet<NetSegId>,
    net_ranges: &HashMap<NetSegId, Vec<BitRange>>,
) -> HashMap<NetSegId, Vec<NetSegId>> {
    let mut adj: HashMap<NetSegId, Vec<NetSegId>> = HashMap::new();
    let mut push_edge = |u: NetSegId, v: NetSegId| {
        if u != v {
            let list = adj.entry(u).or_default();
            if !list.contains(&v) {
                list.push(v);
            }
        }
    };
    for &dst_nid in cone_nets {
        let dseg = graph.net(dst_nid);
        if dseg.is_seq_state
            || matches!(dseg.kind, NetSegKind::Supply0 | NetSegKind::Supply1)
            || graph.cell(dseg.cell).is_blackbox
        {
            continue;
        }
        let dst_ranges = net_ranges.get(&dst_nid);
        for drv in &dseg.drivers {
            if dst_ranges.is_some_and(|rs| !rs.iter().any(|r| drv.dst_range.overlaps(*r))) {
                continue;
            }
            if matches!(
                drv.origin,
                DriverOrigin::PrimaryInput(_)
                    | DriverOrigin::Supply
                    | DriverOrigin::AlwaysFf
                    | DriverOrigin::AlwaysLatch
                    | DriverOrigin::Blackbox
            ) {
                continue;
            }
            if let Some(sn) = drv.src_net {
                if cone_set.contains(&sn) {
                    push_edge(sn, dst_nid);
                }
            }
            for &(dn, _) in &drv.extra_deps {
                if cone_set.contains(&dn) {
                    push_edge(dn, dst_nid);
                }
            }
            if let ExprOpSummary::Mux {
                sel: Some((sel_n, _)),
                ..
            } = drv.op
            {
                if cone_set.contains(&sel_n) {
                    push_edge(sel_n, dst_nid);
                }
            }
        }
    }
    for &src_nid in cone_nets {
        let src_ranges = net_ranges.get(&src_nid);
        for load in &graph.net(src_nid).loads {
            if src_ranges.is_some_and(|rs| !rs.iter().any(|r| load.src_range.overlaps(*r))) {
                continue;
            }
            if let Some(dn) = load.dst_net {
                let dseg = graph.net(dn);
                let dst_ranges = net_ranges.get(&dn);
                if cone_set.contains(&dn)
                    && dst_ranges.is_none_or(|rs| rs.iter().any(|r| load.dst_range.overlaps(*r)))
                    && !dseg.is_seq_state
                    && !matches!(dseg.kind, NetSegKind::Supply0 | NetSegKind::Supply1)
                    && !graph.cell(dseg.cell).is_blackbox
                {
                    push_edge(src_nid, dn);
                }
            }
        }
    }
    adj
}

fn edge_is_isolated(
    ctx: &Stage2Ctx<'_>,
    u: NetSegId,
    v: NetSegId,
    rst_pd: PowerDomainId,
    ret_pd: PowerDomainId,
    net_ranges: &HashMap<NetSegId, Vec<BitRange>>,
) -> bool {
    let u_pd = ctx.domain_of_cell(ctx.graph.net(u).cell);
    let v_pd = ctx.domain_of_cell(ctx.graph.net(v).cell);
    if u_pd == v_pd {
        return false;
    }

    struct EpMeta {
        upf: String,
        full: String,
        esc: Option<String>,
        width: u32,
        bounds: Option<(i128, i128)>,
    }
    let collect_meta = |nid: NetSegId| -> (Vec<(u32, u32)>, Vec<EpMeta>) {
        let seg = ctx.graph.net(nid);
        let rng: Vec<(u32, u32)> = match net_ranges.get(&nid) {
            Some(rs) if !rs.is_empty() => rs.iter().map(|r| (r.lo, r.hi)).collect(),
            _ => vec![(0, seg.width.saturating_sub(1))],
        };
        let mut items = vec![EpMeta {
            upf: seg.upf_path.clone(),
            full: seg.full_path.clone(),
            esc: sv_escaped_upf_variant(&seg.upf_path),
            width: seg.width,
            bounds: seg.packed_bounds,
        }];
        for &pid in &ctx.graph.cell(seg.cell).pins {
            let pin = ctx.graph.pin(pid);
            if pin.internal_net == Some(nid) {
                items.push(EpMeta {
                    upf: pin.upf_path.clone(),
                    full: pin_full_path(ctx.graph, pid),
                    esc: sv_escaped_upf_variant(&pin.upf_path),
                    width: pin.width,
                    bounds: pin.packed_bounds,
                });
            }
        }
        (rng, items)
    };
    let (u_rngs, u_items) = collect_meta(u);
    let (v_rngs, v_items) = collect_meta(v);
    fn to_eps<'a>(items: &'a [EpMeta], rng: &'a [(u32, u32)]) -> Vec<SignalEndpoint<'a>> {
        items
            .iter()
            .map(|e| SignalEndpoint {
                upf_path: &e.upf,
                full_path: &e.full,
                esc_upf_path: e.esc.as_deref(),
                width: e.width,
                packed_bounds: e.bounds,
                ranges: rng,
            })
            .collect()
    }
    let check_ranges = |ur: &[(u32, u32)], vr: &[(u32, u32)]| -> bool {
        let out_eps = to_eps(&u_items, ur);
        let in_eps = to_eps(&v_items, vr);
        let mut best =
            ctx.resolved
                .match_isolation_crossing(u_pd, v_pd, &out_eps, &in_eps, AppliesTo::Both);
        if v_pd != Some(ret_pd) {
            update_best_iso(
                &mut best,
                ctx.resolved.match_isolation_crossing(
                    u_pd,
                    Some(ret_pd),
                    &out_eps,
                    &in_eps,
                    AppliesTo::Outputs,
                ),
            );
        }
        if u_pd != Some(rst_pd) {
            update_best_iso(
                &mut best,
                ctx.resolved.match_isolation_crossing(
                    Some(rst_pd),
                    v_pd,
                    &out_eps,
                    &in_eps,
                    AppliesTo::Inputs,
                ),
            );
        }
        matches!(best, StrategyMatch::Matched(_))
    };

    let split_by_iso_cuts = |items: &[EpMeta], rngs: &[(u32, u32)]| -> Vec<(u32, u32)> {
        let mut cuts: Vec<(u32, u32)> = Vec::new();
        for iso in &ctx.resolved.intent.isolation_strategies {
            for elem in iso.elements.iter().chain(iso.exclude_elements.iter()) {
                let Some((base, msb, lsb)) = parse_bit_slice_expr(elem) else {
                    continue;
                };
                for ep in items {
                    if endpoint_base_matches(&base, &ep.upf, &ep.full, ep.esc.as_deref()) {
                        if let Some((lo, hi)) =
                            sv_slice_to_phys_range(msb, lsb, ep.width, ep.bounds)
                        {
                            cuts.push((lo, hi));
                        }
                    }
                }
            }
        }
        if cuts.is_empty() {
            return rngs.to_vec();
        }
        let mut out = Vec::new();
        for &(lo, hi) in rngs {
            let mut pts = BTreeSet::new();
            pts.insert(lo);
            pts.insert(hi + 1);
            for &(c_lo, c_hi) in &cuts {
                if c_lo <= hi && lo <= c_hi {
                    pts.insert(lo.max(c_lo));
                    pts.insert(hi.min(c_hi) + 1);
                }
            }
            let pts_vec: Vec<u32> = pts.into_iter().collect();
            for w in pts_vec.windows(2) {
                if w[0] < w[1] {
                    out.push((w[0], w[1] - 1));
                }
            }
        }
        out
    };

    let u_sub = split_by_iso_cuts(&u_items, &u_rngs);
    let v_sub = split_by_iso_cuts(&v_items, &v_rngs);
    !u_sub.is_empty()
        && !v_sub.is_empty()
        && u_sub
            .iter()
            .all(|&ur| v_sub.iter().all(|&vr| check_ranges(&[ur], &[vr])))
}

fn update_best_iso<'a>(
    best: &mut StrategyMatch<&'a IsolationStrategy>,
    m: StrategyMatch<&'a IsolationStrategy>,
) {
    match (strat_ref(best), strat_ref(&m)) {
        (None, Some(_)) => *best = m,
        (Some(b), Some(s)) if iso_rank(s) > iso_rank(b) => *best = m,
        _ => {}
    }
}

fn has_unisolated_path_to_sink(
    ctx: &Stage2Ctx<'_>,
    adj: &HashMap<NetSegId, Vec<NetSegId>>,
    src_nid: NetSegId,
    rst_net: NetSegId,
    rst_pd: PowerDomainId,
    ret_pd: PowerDomainId,
    net_ranges: &HashMap<NetSegId, Vec<BitRange>>,
) -> bool {
    if src_nid == rst_net {
        return true;
    }
    let mut visited: HashSet<NetSegId> = HashSet::new();
    let mut queue = VecDeque::new();
    visited.insert(src_nid);
    queue.push_back(src_nid);
    while let Some(cur) = queue.pop_front() {
        let Some(nexts) = adj.get(&cur) else {
            continue;
        };
        for &nxt in nexts {
            if edge_is_isolated(ctx, cur, nxt, rst_pd, ret_pd, net_ranges) {
                continue;
            }
            if nxt == rst_net {
                return true;
            }
            if visited.insert(nxt) {
                queue.push_back(nxt);
            }
        }
    }
    false
}

fn check_ret_strategy_supplies(
    ctx: &mut Stage2Ctx<'_>,
    pd_name: &str,
    prim_ss: informal_upf::SupplySetId,
    ret: &RetentionStrategy,
    rtl_loc: Option<&str>,
    sink: &mut ViolationSink,
) {
    let ret_ss_opt = ctx.resolved.effective_ret_supply(ret);
    match ret_ss_opt {
        None => {
            sink.push(
                "RET_BACKUP_OFF",
                None,
                Some(pd_name.to_string()),
                None,
                ret.name.clone(),
                1,
                1,
                format!(
                    "retention strategy `{}` has no resolved backup supply set",
                    ret.name
                ),
                ret.loc.clone(),
                rtl_loc.map(str::to_string),
                None,
            );
        }
        Some(ret_ss) => {
            if let Some(w) = ctx.sat.can_be_non_normal_while_off(prim_ss, ret_ss) {
                sink.push(
                    "RET_BACKUP_OFF",
                    None,
                    Some(pd_name.to_string()),
                    None,
                    ret.name.clone(),
                    1,
                    1,
                    format!(
                        "retention backup supply `{}` for `{}` can turn OFF while `{pd_name}` is non-NORMAL",
                        ctx.resolved.intent.supply_sets[ret_ss.0 as usize].name,
                        ret.name
                    ),
                    ret.loc.clone(),
                    rtl_loc.map(str::to_string),
                    Some(w),
                );
            }

            // `RET_CTRL_UNPOWERED` on `save_signal` / `restore_signal` / `*_condition`.
            let save_sc = ret.save_scope.as_deref().unwrap_or(&ret.scope);
            let save_cond_sc = ret.save_cond_scope.as_deref().unwrap_or(&ret.scope);
            let restore_sc = ret.restore_scope.as_deref().unwrap_or(&ret.scope);
            let restore_cond_sc = ret.restore_cond_scope.as_deref().unwrap_or(&ret.scope);
            let retention_cond_sc = ret.retention_cond_scope.as_deref().unwrap_or(&ret.scope);
            for (kind, sig_opt, sig_scope) in [
                ("save", ret.save_signal.as_deref(), save_sc),
                ("restore", ret.restore_signal.as_deref(), restore_sc),
                (
                    "save_condition",
                    ret.save_condition.as_deref(),
                    save_cond_sc,
                ),
                (
                    "restore_condition",
                    ret.restore_condition.as_deref(),
                    restore_cond_sc,
                ),
                (
                    "retention_condition",
                    ret.retention_condition.as_deref(),
                    retention_cond_sc,
                ),
            ] {
                let Some(sig) = sig_opt else { continue };
                let mut sigs = Vec::new();
                if let Some(nr) = resolve_upf_signal_range_in_scope(ctx, sig_scope, sig) {
                    sigs.push(nr);
                } else if let Ok(sig_refs) = informal_upf::collect_logic_expr_signal_refs(sig) {
                    for sref in sig_refs {
                        if let Some(nr) = resolve_upf_signal_range_in_scope(ctx, sig_scope, &sref) {
                            if !sigs.contains(&nr) {
                                sigs.push(nr);
                            }
                        }
                    }
                }
                'sig_loop: for (nid, range) in sigs {
                    let cone = ctx.graph.trace_ctrl_cone(nid, range, 64);
                    for &cid in &cone.cone_cells {
                        if let Some(ctrl_ss) = ctx.ss_of_cell(cid) {
                            let bad = ctx
                                .sat
                                .is_supply_always_on_when(ctrl_ss, ret_ss)
                                .err()
                                .or_else(|| ctx.sat.can_be_off_while_on(ctrl_ss, prim_ss));
                            if let Some(wit) = bad {
                                let cdom = ctx
                                    .domain_of_cell(cid)
                                    .map(|d| ctx.domain_name(d))
                                    .unwrap_or_else(|| "unknown".to_string());
                                sink.push(
                                    "RET_CTRL_UNPOWERED",
                                    None,
                                    Some(cdom.clone()),
                                    Some(pd_name.to_string()),
                                    sig.to_string(),
                                    1,
                                    1,
                                    format!(
                                        "retention `{kind}` control `{sig}` traverses/originates in `{cdom}` which can be OFF while `{}` retains",
                                        ret.name
                                    ),
                                    ret.loc.clone(),
                                    rtl_loc.map(str::to_string),
                                    Some(wit),
                                );
                                break 'sig_loop;
                            }
                        }
                    }
                }
            }
        }
    }
}

pub(crate) fn check_power_switches(ctx: &mut Stage2Ctx<'_>, sink: &mut ViolationSink) {
    for sw in &ctx.resolved.intent.power_switches {
        let Some(sw_pd) = ctx.resolved.resolve_domain(&sw.domain, &sw.scope) else {
            continue;
        };
        let sw_name = ctx.domain_name(sw_pd);
        let sw_prim_ss = ctx.resolved.primary_ss(sw_pd);
        // Input supply net's supply set or top always-on fallback.
        let ref_ss = sw
            .supply_set
            .as_deref()
            .and_then(|s| ctx.resolved.resolve_supply_set(s, &sw.scope))
            .or(ctx.top_ss)
            .or(sw_prim_ss);

        for (port_n, ctrl_sig) in &sw.control_ports {
            let port_scope = sw
                .control_port_scopes
                .get(port_n)
                .map(String::as_str)
                .unwrap_or(&sw.scope);
            let mut sigs = Vec::new();
            if let Some(nr) = resolve_upf_signal_range_in_scope(ctx, port_scope, ctrl_sig) {
                sigs.push(nr);
            } else if let Ok(sig_refs) = informal_upf::collect_logic_expr_signal_refs(ctrl_sig) {
                for sig in sig_refs {
                    if let Some(nr) = resolve_upf_signal_range_in_scope(ctx, port_scope, &sig) {
                        if !sigs.contains(&nr) {
                            sigs.push(nr);
                        }
                    }
                }
            }
            for (nid, range) in sigs {
                let cone = ctx.graph.trace_ctrl_cone(nid, range, 64);
                for &cid in &cone.cone_cells {
                    let Some(ctrl_pd) = ctx.domain_of_cell(cid) else {
                        continue;
                    };
                    if ctrl_pd == sw_pd {
                        sink.push(
                            "PSW_CTRL_UNPOWERED",
                            None,
                            Some(ctx.domain_name(ctrl_pd)),
                            Some(sw_name.clone()),
                            ctrl_sig.clone(),
                            1,
                            1,
                            format!(
                                "power switch `{}` control port `{port_n}` (`{ctrl_sig}`) is driven by its own switched domain `{sw_name}`",
                                sw.name
                            ),
                            sw.loc.clone(),
                            ctx.graph.format_loc(ctx.graph.net(nid).loc),
                            None,
                        );
                        break;
                    }
                    if let (Some(ctrl_ss), Some(rss)) = (ctx.resolved.primary_ss(ctrl_pd), ref_ss) {
                        if let Err(wit) = ctx.sat.is_supply_always_on_when(ctrl_ss, rss) {
                            sink.push(
                                "PSW_CTRL_UNPOWERED",
                                None,
                                Some(ctx.domain_name(ctrl_pd)),
                                Some(sw_name.clone()),
                                ctrl_sig.clone(),
                                1,
                                1,
                                format!(
                                    "power switch `{}` control `{ctrl_sig}` comes from `{}` which can be OFF while switch input is ON",
                                    sw.name,
                                    ctx.domain_name(ctrl_pd)
                                ),
                                sw.loc.clone(),
                                ctx.graph.format_loc(ctx.graph.net(nid).loc),
                                Some(wit),
                            );
                            break;
                        }
                    }
                }
            }
        }
    }
}

pub(crate) fn check_port_attributes(ctx: &mut Stage2Ctx<'_>, sink: &mut ViolationSink) {
    let Some(&top_cid) = ctx.graph.tops.first() else {
        return;
    };
    let Some(top_pd) = ctx.domain_of_cell(top_cid) else {
        return;
    };
    let Some(top_ss) = ctx.resolved.primary_ss(top_pd) else {
        return;
    };
    if !domain_can_turn_off(ctx, top_ss) {
        return;
    }
    let top_name = ctx.domain_name(top_pd);
    let top_cell_name = ctx.graph.cell(top_cid).name.clone();
    let pin_ids = ctx.graph.cell(top_cid).pins.clone();
    for pid in pin_ids {
        let pin = ctx.graph.pin(pid);
        let Some(dir) = pin.dir else { continue };
        let pin_esc = sv_escaped_upf_variant(&pin.name);
        let pin_upf_esc = (pin.upf_path != pin.name)
            .then(|| sv_escaped_upf_variant(&pin.upf_path))
            .flatten();
        let top_prefixed =
            (!top_cell_name.is_empty()).then(|| format!("{top_cell_name}/{}", pin.name));
        let top_esc_prefixed = (!top_cell_name.is_empty())
            .then(|| pin_esc.as_deref().map(|e| format!("{top_cell_name}/{e}")))
            .flatten();
        let mut has_driver = false;
        let mut has_receiver = false;
        for pa in &ctx.resolved.intent.port_attributes {
            if !pa.domains.is_empty()
                && !pa
                    .domains
                    .iter()
                    .any(|d| ctx.resolved.resolve_domain(d, &pa.scope) == Some(top_pd))
            {
                continue;
            }
            if let Some(applies) = pa.applies_to {
                let dir_ok = !matches!(
                    (dir, applies),
                    (PortDir::Input, AppliesTo::Outputs) | (PortDir::Output, AppliesTo::Inputs)
                );
                if !dir_ok {
                    continue;
                }
            }
            let matches_port_pats = |pats: &[String]| {
                ctx.resolved
                    .element_matches_in_scope(pats, &pa.scope, &pin.name)
                    || pin_esc
                        .as_deref()
                        .is_some_and(|e| ctx.resolved.element_matches_in_scope(pats, &pa.scope, e))
                    || (pin.upf_path != pin.name
                        && ctx
                            .resolved
                            .element_matches_in_scope(pats, &pa.scope, &pin.upf_path))
                    || pin_upf_esc
                        .as_deref()
                        .is_some_and(|e| ctx.resolved.element_matches_in_scope(pats, &pa.scope, e))
                    || top_prefixed.as_deref().is_some_and(|tp| {
                        ctx.resolved.element_matches_in_scope(pats, &pa.scope, tp)
                    })
                    || top_esc_prefixed.as_deref().is_some_and(|tp| {
                        ctx.resolved.element_matches_in_scope(pats, &pa.scope, tp)
                    })
            };
            if !pa.exclude_elements.is_empty() && matches_port_pats(&pa.exclude_elements) {
                continue;
            }
            let name_match = (!pa.ports.is_empty() && matches_port_pats(&pa.ports))
                || (!pa.elements.is_empty() && matches_port_pats(&pa.elements))
                || (pa.ports.is_empty() && pa.elements.is_empty() && !pa.domains.is_empty());
            if !name_match {
                continue;
            }
            let applies = pa.applies_to.unwrap_or(AppliesTo::Both);
            if matches!(applies, AppliesTo::Inputs | AppliesTo::Both)
                && (pa.driver_supply.is_some() || pa.related_power_port.is_some())
            {
                has_driver = true;
            }
            if matches!(applies, AppliesTo::Outputs | AppliesTo::Both)
                && (pa.receiver_supply.is_some() || pa.related_power_port.is_some())
            {
                has_receiver = true;
            }
        }
        let has_attr = match dir {
            PortDir::Input => has_driver,
            PortDir::Output => has_receiver,
            PortDir::Inout => has_driver && has_receiver,
            PortDir::Ref => true,
        };
        if !has_attr {
            sink.push(
                "PORT_ATTR_MISSING",
                None,
                Some(top_name.clone()),
                None,
                pin.name.clone(),
                pin.width,
                1,
                format!(
                    "top-level `{}` port `{}` in switchable domain `{top_name}` lacks `set_port_attributes` supply",
                    dir.as_str(),
                    pin.name
                ),
                None,
                ctx.graph.format_loc(pin.loc),
                None,
            );
        }
    }
}

#[allow(dead_code)]
fn domain_can_turn_off(ctx: &Stage2Ctx<'_>, ss: informal_upf::SupplySetId) -> bool {
    ctx.resolved
        .ss_states
        .get(ss.0 as usize)
        .is_some_and(|states| {
            states.iter().any(|st| {
                !st.illegal
                    && (st.rail_states.iter().any(|r| r.is_off)
                        || st.simstate.is_some_and(|s| {
                            matches!(
                                s,
                                informal_upf::SimState::Corrupt
                                    | informal_upf::SimState::Off
                                    | informal_upf::SimState::NotNormal
                            )
                        }))
            })
        })
}

#[allow(dead_code)]
fn _unused(_n: NetSegId) {}
