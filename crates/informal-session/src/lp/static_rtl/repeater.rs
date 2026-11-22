//! Stage 2 `set_repeater` route coverage and bit-slice splitting helpers.

use std::collections::BTreeSet;

use informal_svlang::struct_graph::{
    BitRange, CrossingGroup, FeedthroughKind, FeedthroughRoute, NetSegId, PinId,
};
use informal_svlang::PortDir;
use informal_upf::{
    endpoint_base_matches, parse_bit_slice_expr, sv_slice_to_phys_range, AppliesTo, PowerDomainId,
    RepeaterStrategy, SignalEndpoint, SupplySetId,
};

use super::{pin_full_path, sv_escaped_upf_variant, Stage2Ctx, ViolationSink};

pub(super) fn split_route_by_repeater_cuts(
    ctx: &Stage2Ctx<'_>,
    group: &CrossingGroup,
    route: &FeedthroughRoute,
) -> Vec<FeedthroughRoute> {
    if route.dst_range.width() <= 1 {
        return vec![route.clone()];
    }
    let dst_net = ctx.graph.net(group.dst_base_net);
    let dst_esc = sv_escaped_upf_variant(&dst_net.upf_path);
    let mut pts: BTreeSet<u32> = BTreeSet::new();
    pts.insert(route.dst_range.lo);
    pts.insert(route.dst_range.hi + 1);

    let add_cut = |pts: &mut BTreeSet<u32>, cut: BitRange, base_r: BitRange| {
        if base_r.width() == route.dst_range.width() {
            if let Some(ov) = base_r.intersect(cut) {
                let d_lo = route.dst_range.lo + (ov.lo - base_r.lo);
                let d_hi = route.dst_range.lo + (ov.hi - base_r.lo);
                pts.insert(d_lo);
                pts.insert(d_hi + 1);
            }
        }
    };

    let allow_downstream_slice = route.kind != FeedthroughKind::Combinational;
    let pre_comb = if allow_downstream_slice {
        Vec::new()
    } else {
        ctx.graph
            .pre_comb_nets_for_route(group.dst_base_net, route.dst_range, &route.reaching_nets)
    };
    let add_pre_comb_cut =
        |pts: &mut BTreeSet<u32>, cut: BitRange, net_r: BitRange, dst_sub: BitRange| {
            if net_r.width() > 0 && net_r.width() == dst_sub.width() {
                if let Some(ov) = net_r.intersect(cut) {
                    pts.insert(dst_sub.lo + (ov.lo - net_r.lo));
                    pts.insert(dst_sub.lo + (ov.hi - net_r.lo) + 1);
                }
            }
        };

    for rep in &ctx.resolved.intent.repeater_strategies {
        for elem in rep.elements.iter().chain(rep.exclude_elements.iter()) {
            let Some((base, msb, lsb)) = parse_bit_slice_expr(elem) else {
                continue;
            };
            if endpoint_base_matches(
                &base,
                &dst_net.upf_path,
                &dst_net.full_path,
                dst_esc.as_deref(),
            ) {
                if let Some((lo, hi)) =
                    sv_slice_to_phys_range(msb, lsb, dst_net.width, dst_net.packed_bounds)
                {
                    add_cut(&mut pts, BitRange { lo, hi }, route.dst_range);
                }
            }
            for &(nid, pnr, pdst) in &pre_comb {
                if nid == group.dst_base_net {
                    continue;
                }
                let net = ctx.graph.net(nid);
                let nesc = sv_escaped_upf_variant(&net.upf_path);
                if endpoint_base_matches(&base, &net.upf_path, &net.full_path, nesc.as_deref()) {
                    if let Some((lo, hi)) =
                        sv_slice_to_phys_range(msb, lsb, net.width, net.packed_bounds)
                    {
                        add_pre_comb_cut(&mut pts, BitRange { lo, hi }, pnr, pdst);
                    }
                }
            }
            if !allow_downstream_slice {
                continue;
            }
            for &(pid, pr) in &route.reaching_pins {
                let pin = ctx.graph.pin(pid);
                let pesc = sv_escaped_upf_variant(&pin.upf_path);
                let pfull = pin_full_path(ctx.graph, pid);
                if endpoint_base_matches(&base, &pin.upf_path, &pfull, pesc.as_deref()) {
                    if let Some((lo, hi)) =
                        sv_slice_to_phys_range(msb, lsb, pin.width, pin.packed_bounds)
                    {
                        add_cut(&mut pts, BitRange { lo, hi }, pr);
                    }
                }
            }
            for &(nid, nr) in &route.reaching_nets {
                let net = ctx.graph.net(nid);
                let nesc = sv_escaped_upf_variant(&net.upf_path);
                if endpoint_base_matches(&base, &net.upf_path, &net.full_path, nesc.as_deref()) {
                    if let Some((lo, hi)) =
                        sv_slice_to_phys_range(msb, lsb, net.width, net.packed_bounds)
                    {
                        add_cut(&mut pts, BitRange { lo, hi }, nr);
                    }
                }
            }
        }
    }

    let pts_vec: Vec<u32> = pts.into_iter().collect();
    if pts_vec.len() <= 2 {
        return vec![route.clone()];
    }
    let same_w = route.src_range.width() == route.dst_range.width();
    pts_vec
        .windows(2)
        .filter(|w| w[0] < w[1])
        .map(|w| {
            let sub_dst = BitRange {
                lo: w[0],
                hi: w[1] - 1,
            };
            let sub_src = if same_w {
                BitRange {
                    lo: route.src_range.lo + (sub_dst.lo - route.dst_range.lo),
                    hi: route.src_range.lo + (sub_dst.hi - route.dst_range.lo),
                }
            } else {
                route.src_range
            };
            let narrow = |r: BitRange| {
                if allow_downstream_slice && r.width() == route.dst_range.width() {
                    BitRange {
                        lo: r.lo + (sub_dst.lo - route.dst_range.lo),
                        hi: r.lo + (sub_dst.hi - route.dst_range.lo),
                    }
                } else {
                    r
                }
            };
            let reaching_nets = if allow_downstream_slice {
                route
                    .reaching_nets
                    .iter()
                    .map(|&(n, nr)| (n, narrow(nr)))
                    .collect()
            } else {
                let mut out_nets = Vec::with_capacity(route.reaching_nets.len());
                for &(n, nr) in &route.reaching_nets {
                    let pre_matches: Vec<(BitRange, BitRange)> = pre_comb
                        .iter()
                        .filter(|&&(pn, pnr, _)| pn == n && pnr.overlaps(nr))
                        .map(|&(_, pnr, pdst)| (pnr, pdst))
                        .collect();
                    if pre_matches.is_empty() {
                        out_nets.push((n, nr));
                    } else {
                        for (pnr, pdst) in pre_matches {
                            if let Some(ov) = pdst.intersect(sub_dst) {
                                if pdst.width() > 0 && pdst.width() == pnr.width() {
                                    let proj = BitRange {
                                        lo: pnr.lo + (ov.lo - pdst.lo),
                                        hi: pnr.lo + (ov.hi - pdst.lo),
                                    };
                                    if !out_nets.contains(&(n, proj)) {
                                        out_nets.push((n, proj));
                                    }
                                } else if !out_nets.contains(&(n, pnr)) {
                                    out_nets.push((n, pnr));
                                }
                            }
                        }
                    }
                }
                out_nets
            };
            FeedthroughRoute {
                src_range: sub_src,
                dst_range: sub_dst,
                kind: route.kind,
                downstream_cell: route.downstream_cell,
                reaching_pins: route
                    .reaching_pins
                    .iter()
                    .map(|&(p, pr)| (p, narrow(pr)))
                    .collect(),
                reaching_nets,
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn check_case_b_fallback_dcs(
    ctx: &mut Stage2Ctx<'_>,
    group: &CrossingGroup,
    src_pd: PowerDomainId,
    dst_pd: PowerDomainId,
    src_ss: SupplySetId,
    dst_ss: SupplySetId,
    sig_disp: &str,
    rtl_loc: &Option<String>,
    sink: &mut ViolationSink,
) {
    let mut witness = ctx.sat.can_be_off_while_on(dst_ss, src_ss);
    let mut down_name = ctx.domain_name(dst_pd);
    if witness.is_none() {
        for &dc in &group.feedthrough_downstream_cells {
            if let (Some(dpd), Some(dss)) = (ctx.domain_of_cell(dc), ctx.ss_of_cell(dc)) {
                if let Some(w) = ctx.sat.can_be_off_while_on(dst_ss, dss) {
                    witness = Some(w);
                    down_name = ctx.domain_name(dpd);
                    break;
                }
            }
        }
    }
    let Some(w) = witness else {
        return;
    };

    let dst_net = ctx.graph.net(group.dst_base_net);
    let dst_esc = sv_escaped_upf_variant(&dst_net.upf_path);
    let dst_rngs: Vec<(u32, u32)> = group.dst_ranges.iter().map(|r| (r.lo, r.hi)).collect();
    let dst_ep = SignalEndpoint {
        upf_path: &dst_net.upf_path,
        full_path: &dst_net.full_path,
        esc_upf_path: dst_esc.as_deref(),
        width: dst_net.width,
        packed_bounds: dst_net.packed_bounds,
        ranges: &dst_rngs,
    };
    let has_aon_repeater = group.feedthrough_downstream_cells.iter().all(|&dc| {
        ctx.resolved.intent.repeater_strategies.iter().any(|rep| {
            let pins: Vec<(PinId, BitRange)> = group
                .feedthrough_pins
                .iter()
                .map(|&p| (p, BitRange::full(ctx.graph.pin(p).width)))
                .collect();
            let nets: Vec<(NetSegId, BitRange)> = group
                .feedthrough_nets
                .iter()
                .map(|&n| (n, BitRange::full(ctx.graph.net(n).width)))
                .collect();
            let route = FeedthroughRoute {
                src_range: group
                    .src_ranges
                    .first()
                    .copied()
                    .unwrap_or(BitRange::full(1)),
                dst_range: group
                    .dst_ranges
                    .first()
                    .copied()
                    .unwrap_or(BitRange::full(1)),
                kind: group.feedthrough,
                downstream_cell: dc,
                reaching_pins: pins,
                reaching_nets: nets,
            };
            repeater_covers_route_ep(ctx, rep, &dst_ep, &route, src_pd, dst_pd, src_ss)
        })
    });
    if has_aon_repeater {
        return;
    }

    let (rule_id, kind_lbl) = match group.feedthrough {
        FeedthroughKind::PureWire => ("ISO_FEEDTHROUGH", "pure-wire"),
        FeedthroughKind::Buffered => ("ISO_FEEDTHROUGH_BUFFER", "buffered"),
        FeedthroughKind::Combinational => ("ISO_FEEDTHROUGH_BUFFER", "combinational"),
        FeedthroughKind::None | FeedthroughKind::Sequential => return,
    };
    let mid_name = ctx.domain_name(dst_pd);
    sink.push(
        rule_id,
        None,
        Some(mid_name.clone()),
        Some(down_name),
        sig_disp.to_string(),
        group.width_bits,
        group.raw_crossings,
        format!(
            "{kind_lbl} feedthrough `{sig_disp}` passes through switchable domain `{mid_name}` without always-on repeater"
        ),
        None,
        rtl_loc.clone(),
        Some(w),
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn repeater_covers_route(
    ctx: &mut Stage2Ctx<'_>,
    rep: &RepeaterStrategy,
    group: &CrossingGroup,
    route: &FeedthroughRoute,
    src_pd: PowerDomainId,
    dst_pd: PowerDomainId,
    src_ss: SupplySetId,
) -> bool {
    let dst_net = ctx.graph.net(group.dst_base_net);
    let dst_esc = sv_escaped_upf_variant(&dst_net.upf_path);
    let dst_rng = [(route.dst_range.lo, route.dst_range.hi)];
    let dst_ep = SignalEndpoint {
        upf_path: &dst_net.upf_path,
        full_path: &dst_net.full_path,
        esc_upf_path: dst_esc.as_deref(),
        width: dst_net.width,
        packed_bounds: dst_net.packed_bounds,
        ranges: &dst_rng,
    };
    repeater_covers_route_ep(ctx, rep, &dst_ep, route, src_pd, dst_pd, src_ss)
}

#[allow(clippy::too_many_arguments)]
fn repeater_covers_route_ep(
    ctx: &mut Stage2Ctx<'_>,
    rep: &RepeaterStrategy,
    dst_ep: &SignalEndpoint<'_>,
    route: &FeedthroughRoute,
    src_pd: PowerDomainId,
    dst_pd: PowerDomainId,
    src_ss: SupplySetId,
) -> bool {
    if ctx.resolved.resolve_domain(&rep.domain, &rep.scope) != Some(dst_pd) {
        return false;
    }
    if let Some(ref sf) = rep.source_filter {
        if !ctx
            .resolved
            .filter_matches_domain(sf, &rep.scope, Some(src_pd))
        {
            return false;
        }
    }
    if let Some(ref df) = rep.sink_filter {
        let dc_pd = ctx.domain_of_cell(route.downstream_cell);
        if !ctx.resolved.filter_matches_domain(df, &rep.scope, dc_pd) {
            return false;
        }
    }
    let excluded = !rep.exclude_elements.is_empty()
        && (informal_upf::endpoint_matches_patterns(
            dst_ep,
            &rep.scope,
            &rep.exclude_elements,
            true,
        ) || route.reaching_pins.iter().any(|&(pid, pr)| {
            matches_pin_patterns_ranges(
                ctx,
                pid,
                &[(pr.lo, pr.hi)],
                &rep.scope,
                &rep.exclude_elements,
                true,
            )
        }) || route.reaching_nets.iter().any(|&(nid, nr)| {
            endpoint_matches_net_range(ctx, nid, nr, &rep.scope, &rep.exclude_elements, true)
        }));
    if excluded {
        return false;
    }
    let applies = rep.applies_to.unwrap_or(AppliesTo::Both);
    let match_in = matches!(applies, AppliesTo::Inputs | AppliesTo::Both);
    let match_out = matches!(applies, AppliesTo::Outputs | AppliesTo::Both);
    let elem_match = rep.elements.is_empty()
        || (match_in
            && informal_upf::endpoint_matches_patterns(dst_ep, &rep.scope, &rep.elements, false))
        || (match_out
            && route.reaching_pins.iter().any(|&(pid, pr)| {
                let pin = ctx.graph.pin(pid);
                let is_dc_input =
                    pin.cell == route.downstream_cell && !matches!(pin.dir, Some(PortDir::Output));
                (!is_dc_input || matches!(applies, AppliesTo::Both))
                    && matches_pin_patterns_ranges(
                        ctx,
                        pid,
                        &[(pr.lo, pr.hi)],
                        &rep.scope,
                        &rep.elements,
                        false,
                    )
            }))
        || (matches!(applies, AppliesTo::Both)
            && route.reaching_nets.iter().any(|&(nid, nr)| {
                endpoint_matches_net_range(ctx, nid, nr, &rep.scope, &rep.elements, false)
            }));
    if !elem_match {
        return false;
    }
    let Some(rep_ss) = rep
        .repeater_supply
        .as_deref()
        .and_then(|s| ctx.resolved.resolve_supply_set(s, &rep.scope))
    else {
        return false;
    };
    ctx.sat.is_supply_always_on_when(rep_ss, src_ss).is_ok()
        && ctx
            .ss_of_cell(route.downstream_cell)
            .is_none_or(|dss| ctx.sat.is_supply_always_on_when(rep_ss, dss).is_ok())
}

pub(super) fn endpoint_matches_net_range(
    ctx: &Stage2Ctx<'_>,
    nid: NetSegId,
    range: BitRange,
    scope: &str,
    patterns: &[String],
    is_exclude: bool,
) -> bool {
    let net = ctx.graph.net(nid);
    let esc = sv_escaped_upf_variant(&net.upf_path);
    let rng = [(range.lo, range.hi)];
    let ep = SignalEndpoint {
        upf_path: &net.upf_path,
        full_path: &net.full_path,
        esc_upf_path: esc.as_deref(),
        width: net.width,
        packed_bounds: net.packed_bounds,
        ranges: &rng,
    };
    informal_upf::endpoint_matches_patterns(&ep, scope, patterns, is_exclude)
}

pub(super) fn matches_pin_patterns_ranges(
    ctx: &Stage2Ctx<'_>,
    pid: PinId,
    ranges: &[(u32, u32)],
    scope: &str,
    patterns: &[String],
    is_exclude: bool,
) -> bool {
    let pin = ctx.graph.pin(pid);
    let esc = sv_escaped_upf_variant(&pin.upf_path);
    let full = pin_full_path(ctx.graph, pid);
    let ep = SignalEndpoint {
        upf_path: &pin.upf_path,
        full_path: &full,
        esc_upf_path: esc.as_deref(),
        width: pin.width,
        packed_bounds: pin.packed_bounds,
        ranges,
    };
    if informal_upf::endpoint_matches_patterns(&ep, scope, patterns, is_exclude) {
        return true;
    }
    if let Some(nid) = pin.internal_net {
        let net = ctx.graph.net(nid);
        let nr = if net.width == pin.width {
            ranges
                .first()
                .map(|&(lo, hi)| BitRange { lo, hi })
                .unwrap_or_else(|| BitRange::full(net.width))
        } else {
            BitRange::full(net.width)
        };
        if endpoint_matches_net_range(ctx, nid, nr, scope, patterns, is_exclude) {
            return true;
        }
    }
    false
}
