//! Stage 2 Feedthrough isolation (`ISO_FEEDTHROUGH` / `ISO_FEEDTHROUGH_BUFFER`)
//! and always-on `set_repeater` coverage verification (Case A parent pass-throughs
//! and Case B per-route / per-bit feedthrough paths).

use std::collections::BTreeSet;

use informal_svlang::struct_graph::{
    format_bit_range, split_crossing_group_by_cuts, BitRange, CellId, CrossingGroup,
    FeedthroughKind, NetSegId,
};
use informal_upf::{
    endpoint_base_matches, parse_bit_slice_expr, sv_slice_to_phys_range, AppliesTo, PowerDomainId,
    PstWitness, RepeaterStrategy, SignalEndpoint, SupplySetId,
};

use super::iso::display_signal;
use super::repeater::{
    check_case_b_fallback_dcs, endpoint_matches_net_range, matches_pin_patterns_ranges,
    repeater_covers_route, split_route_by_repeater_cuts,
};
use super::{sv_escaped_upf_variant, Stage2Ctx, ViolationSink};

#[allow(clippy::too_many_arguments)]
pub(crate) fn check_feedthrough(
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
    // Case A: pure pass-through across an intermediate parent cell whose domain can turn OFF.
    if let Some(mid_cid) = group.mid_cell {
        check_case_a_mid_cell(
            ctx, group, mid_cid, src_pd, dst_pd, src_ss, dst_ss, sig_disp, rtl_loc, sink,
        );
    }

    if src_pd == dst_pd {
        return;
    }

    // Case B: signal enters `dst_cell` (`mid`) and feeds straight out to downstream domain(s).
    if !group.feedthrough.is_comb_feedthrough() {
        return;
    }

    if !group.feedthrough_routes.is_empty() {
        check_case_b_routes(
            ctx, group, src_pd, dst_pd, src_ss, dst_ss, sig_disp, rtl_loc, sink,
        );
        return;
    }

    if group.feedthrough_downstream_cells.is_empty() {
        return;
    }
    check_case_b_fallback_dcs(
        ctx, group, src_pd, dst_pd, src_ss, dst_ss, sig_disp, rtl_loc, sink,
    );
}

#[allow(clippy::too_many_arguments)]
fn check_case_a_mid_cell(
    ctx: &mut Stage2Ctx<'_>,
    group: &CrossingGroup,
    mid_cid: CellId,
    src_pd: PowerDomainId,
    dst_pd: PowerDomainId,
    src_ss: SupplySetId,
    dst_ss: SupplySetId,
    sig_disp: &str,
    rtl_loc: &Option<String>,
    sink: &mut ViolationSink,
) {
    let (Some(mid_pd), Some(mid_ss)) = (ctx.domain_of_cell(mid_cid), ctx.ss_of_cell(mid_cid))
    else {
        return;
    };
    let Some(w) = ctx
        .sat
        .can_be_off_while_on(mid_ss, dst_ss)
        .or_else(|| ctx.sat.can_be_off_while_on(mid_ss, src_ss))
    else {
        return;
    };

    let rep_cuts = collect_case_a_repeater_cuts(ctx, group);
    let subs = split_crossing_group_by_cuts(ctx.graph, group, &rep_cuts);
    let uncovered: Vec<&CrossingGroup> = subs
        .iter()
        .filter(|sub| {
            !ctx.resolved.intent.repeater_strategies.iter().any(|rep| {
                repeater_covers_case_a(ctx, rep, group, sub, src_pd, mid_pd, dst_pd, src_ss, dst_ss)
            })
        })
        .collect();
    if uncovered.is_empty() {
        return;
    }

    let mid_name = ctx.domain_name(mid_pd);
    let dst_name = ctx.domain_name(dst_pd);
    if uncovered.len() == subs.len() {
        sink.push(
            "ISO_FEEDTHROUGH",
            None,
            Some(mid_name.clone()),
            Some(dst_name),
            sig_disp.to_string(),
            group.width_bits,
            group.raw_crossings,
            format!(
                "pure-wire feedthrough `{sig_disp}` traverses switchable intermediate domain `{mid_name}`"
            ),
            None,
            rtl_loc.clone(),
            Some(w),
        );
    } else {
        for sub in uncovered {
            let sub_sig = display_signal(ctx, sub);
            sink.push(
                "ISO_FEEDTHROUGH",
                None,
                Some(mid_name.clone()),
                Some(dst_name.clone()),
                sub_sig.clone(),
                sub.width_bits,
                sub.raw_crossings,
                format!(
                    "pure-wire feedthrough `{sub_sig}` traverses switchable intermediate domain `{mid_name}`"
                ),
                None,
                rtl_loc.clone(),
                Some(w.clone()),
            );
        }
    }
}

fn collect_case_a_repeater_cuts(ctx: &Stage2Ctx<'_>, group: &CrossingGroup) -> Vec<BitRange> {
    if group.src_ranges.is_empty() {
        return Vec::new();
    }
    let src_net = ctx.graph.net(group.src_base_net);
    let dst_net = ctx.graph.net(group.dst_base_net);
    let src_esc = sv_escaped_upf_variant(&src_net.upf_path);
    let dst_esc = sv_escaped_upf_variant(&dst_net.upf_path);
    let mut cuts = Vec::new();
    let total_src_w: u32 = group.src_ranges.iter().map(|r| r.width()).sum();
    let total_dst_w: u32 = group.dst_ranges.iter().map(|r| r.width()).sum();

    for rep in &ctx.resolved.intent.repeater_strategies {
        for elem in rep.elements.iter().chain(rep.exclude_elements.iter()) {
            let Some((base, msb, lsb)) = parse_bit_slice_expr(elem) else {
                for &((mn, mr), es, _ed) in &group.mid_net_scopes {
                    if endpoint_matches_net_range(
                        ctx,
                        mn,
                        mr,
                        &rep.scope,
                        std::slice::from_ref(elem),
                        true,
                    ) {
                        cuts.push(es);
                    }
                }
                continue;
            };
            if endpoint_base_matches(
                &base,
                &src_net.upf_path,
                &src_net.full_path,
                src_esc.as_deref(),
            ) {
                if let Some((lo, hi)) =
                    sv_slice_to_phys_range(msb, lsb, src_net.width, src_net.packed_bounds)
                {
                    cuts.push(BitRange { lo, hi });
                }
            }
            if endpoint_base_matches(
                &base,
                &dst_net.upf_path,
                &dst_net.full_path,
                dst_esc.as_deref(),
            ) {
                if let Some((lo, hi)) =
                    sv_slice_to_phys_range(msb, lsb, dst_net.width, dst_net.packed_bounds)
                {
                    project_dst_cut_to_src(
                        group,
                        BitRange { lo, hi },
                        total_src_w,
                        total_dst_w,
                        &mut cuts,
                    );
                }
            }
            if !group.mid_net_scopes.is_empty() {
                for &((mn, mr), es, _ed) in &group.mid_net_scopes {
                    let mnet = ctx.graph.net(mn);
                    let mesc = sv_escaped_upf_variant(&mnet.upf_path);
                    if endpoint_base_matches(
                        &base,
                        &mnet.upf_path,
                        &mnet.full_path,
                        mesc.as_deref(),
                    ) {
                        if let Some((lo, hi)) =
                            sv_slice_to_phys_range(msb, lsb, mnet.width, mnet.packed_bounds)
                        {
                            if let Some(ov) = mr.intersect(BitRange { lo, hi }) {
                                if mr.width() == es.width() {
                                    cuts.push(BitRange {
                                        lo: es.lo + (ov.lo - mr.lo),
                                        hi: es.lo + (ov.hi - mr.lo),
                                    });
                                } else {
                                    cuts.push(es);
                                }
                            }
                        }
                    }
                }
            } else {
                for &(mn, mr) in &group.mid_nets {
                    let mnet = ctx.graph.net(mn);
                    let mesc = sv_escaped_upf_variant(&mnet.upf_path);
                    if endpoint_base_matches(
                        &base,
                        &mnet.upf_path,
                        &mnet.full_path,
                        mesc.as_deref(),
                    ) {
                        if let Some((lo, hi)) =
                            sv_slice_to_phys_range(msb, lsb, mnet.width, mnet.packed_bounds)
                        {
                            if let Some(ov) = mr.intersect(BitRange { lo, hi }) {
                                if mr.width() == total_src_w && group.src_ranges.len() == 1 {
                                    let sr = group.src_ranges[0];
                                    cuts.push(BitRange {
                                        lo: sr.lo + (ov.lo - mr.lo),
                                        hi: sr.lo + (ov.hi - mr.lo),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    cuts
}

fn project_dst_cut_to_src(
    group: &CrossingGroup,
    dst_phys: BitRange,
    total_src_w: u32,
    total_dst_w: u32,
    cuts: &mut Vec<BitRange>,
) {
    if !group.lanes.is_empty() {
        for &(sr, dr) in &group.lanes {
            if let Some(ov) = dst_phys.intersect(dr) {
                if sr.width() > 0 && sr.width() == dr.width() {
                    cuts.push(BitRange {
                        lo: sr.lo + (ov.lo - dr.lo),
                        hi: sr.lo + (ov.hi - dr.lo),
                    });
                } else {
                    cuts.push(sr);
                }
            }
        }
    } else if total_src_w > 0 && total_dst_w == total_src_w {
        let mut dst_cum = 0u32;
        for &dr in &group.dst_ranges {
            if let Some(ov) = dst_phys.intersect(dr) {
                let off_lo = dst_cum + (ov.lo - dr.lo);
                let off_hi = dst_cum + (ov.hi - dr.lo);
                cuts.extend(informal_svlang::struct_graph::map_range_list_offset(
                    &group.src_ranges,
                    off_lo,
                    off_hi,
                ));
            }
            dst_cum += dr.width();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn repeater_covers_case_a(
    ctx: &mut Stage2Ctx<'_>,
    rep: &RepeaterStrategy,
    parent_group: &CrossingGroup,
    sub: &CrossingGroup,
    src_pd: PowerDomainId,
    mid_pd: PowerDomainId,
    dst_pd: PowerDomainId,
    src_ss: SupplySetId,
    dst_ss: SupplySetId,
) -> bool {
    if ctx.resolved.resolve_domain(&rep.domain, &rep.scope) != Some(mid_pd) {
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
        if !ctx
            .resolved
            .filter_matches_domain(df, &rep.scope, Some(dst_pd))
        {
            return false;
        }
    }

    let src_net = ctx.graph.net(sub.src_base_net);
    let dst_net = ctx.graph.net(sub.dst_base_net);
    let src_esc = sv_escaped_upf_variant(&src_net.upf_path);
    let dst_esc = sv_escaped_upf_variant(&dst_net.upf_path);
    let src_rngs: Vec<(u32, u32)> = sub.src_ranges.iter().map(|r| (r.lo, r.hi)).collect();
    let dst_rngs: Vec<(u32, u32)> = sub.dst_ranges.iter().map(|r| (r.lo, r.hi)).collect();
    let src_ep = SignalEndpoint {
        upf_path: &src_net.upf_path,
        full_path: &src_net.full_path,
        esc_upf_path: src_esc.as_deref(),
        width: src_net.width,
        packed_bounds: src_net.packed_bounds,
        ranges: &src_rngs,
    };
    let dst_ep = SignalEndpoint {
        upf_path: &dst_net.upf_path,
        full_path: &dst_net.full_path,
        esc_upf_path: dst_esc.as_deref(),
        width: dst_net.width,
        packed_bounds: dst_net.packed_bounds,
        ranges: &dst_rngs,
    };

    let mid_nets = if sub.mid_net_scopes.is_empty() {
        project_mid_nets(parent_group, sub)
    } else {
        sub.mid_nets.clone()
    };
    let bpin_in = sub
        .boundary_pin
        .filter(|&pid| ctx.graph.pin(pid).cell == sub.src_cell);

    let excluded = !rep.exclude_elements.is_empty()
        && (informal_upf::endpoint_matches_patterns(
            &src_ep,
            &rep.scope,
            &rep.exclude_elements,
            true,
        ) || informal_upf::endpoint_matches_patterns(
            &dst_ep,
            &rep.scope,
            &rep.exclude_elements,
            true,
        ) || bpin_in.is_some_and(|pid| {
            matches_pin_patterns_ranges(
                ctx,
                pid,
                &src_rngs,
                &rep.scope,
                &rep.exclude_elements,
                true,
            )
        }) || mid_nets.iter().any(|&(nid, mr)| {
            endpoint_matches_net_range(ctx, nid, mr, &rep.scope, &rep.exclude_elements, true)
        }));
    if excluded {
        return false;
    }

    let applies = rep.applies_to.unwrap_or(AppliesTo::Both);
    let match_in = matches!(applies, AppliesTo::Inputs | AppliesTo::Both);
    let elem_match = rep.elements.is_empty()
        || (match_in
            && (informal_upf::endpoint_matches_patterns(
                &src_ep,
                &rep.scope,
                &rep.elements,
                false,
            ) || bpin_in.is_some_and(|pid| {
                matches_pin_patterns_ranges(ctx, pid, &src_rngs, &rep.scope, &rep.elements, false)
            })))
        || (matches!(applies, AppliesTo::Both)
            && (informal_upf::endpoint_matches_patterns(
                &dst_ep,
                &rep.scope,
                &rep.elements,
                false,
            ) || mid_nets_cover_sub(ctx, &mid_nets, sub, &rep.scope, &rep.elements)));
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
        && ctx.sat.is_supply_always_on_when(rep_ss, dst_ss).is_ok()
}

fn mid_nets_cover_sub(
    ctx: &Stage2Ctx<'_>,
    mid_nets: &[(NetSegId, BitRange)],
    sub: &CrossingGroup,
    scope: &str,
    patterns: &[String],
) -> bool {
    if !sub.mid_net_scopes.is_empty() {
        let mut cov_src = Vec::new();
        let mut cov_dst = Vec::new();
        for &((nid, mr), es, ed) in &sub.mid_net_scopes {
            if endpoint_matches_net_range(ctx, nid, mr, scope, patterns, false) {
                cov_src.push(es);
                cov_dst.push(ed);
            }
        }
        return ranges_cover_all(&sub.src_ranges, &cov_src)
            && ranges_cover_all(&sub.dst_ranges, &cov_dst);
    }
    mid_nets.iter().any(|&(nid, mr)| {
        mr.width() >= sub.width_bits
            && endpoint_matches_net_range(ctx, nid, mr, scope, patterns, false)
    })
}

fn ranges_cover_all(targets: &[BitRange], covered: &[BitRange]) -> bool {
    !targets.is_empty()
        && targets.iter().all(|&t| {
            let mut segs: Vec<BitRange> = covered.iter().filter_map(|c| c.intersect(t)).collect();
            if segs.is_empty() {
                return false;
            }
            segs.sort_by_key(|r| (r.lo, r.hi));
            if segs[0].lo > t.lo {
                return false;
            }
            let mut end = segs[0].hi;
            for s in segs.into_iter().skip(1) {
                if s.lo > end.saturating_add(1) {
                    return false;
                }
                end = end.max(s.hi);
            }
            end >= t.hi
        })
}

fn project_mid_nets(parent: &CrossingGroup, sub: &CrossingGroup) -> Vec<(NetSegId, BitRange)> {
    if parent.mid_nets.is_empty() {
        return Vec::new();
    }
    if parent.width_bits == sub.width_bits
        || parent.src_ranges.len() != 1
        || sub.src_ranges.len() != 1
    {
        return parent.mid_nets.clone();
    }
    let ps = parent.src_ranges[0];
    let ss = sub.src_ranges[0];
    parent
        .mid_nets
        .iter()
        .map(|&(nid, mr)| {
            if mr.width() == ps.width() && ss.lo >= ps.lo && ss.hi <= ps.hi {
                let lo = mr.lo + (ss.lo - ps.lo);
                let hi = mr.lo + (ss.hi - ps.lo);
                (
                    nid,
                    BitRange {
                        lo,
                        hi: hi.min(mr.hi),
                    },
                )
            } else {
                (nid, mr)
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn check_case_b_routes(
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
    struct UncoveredHit {
        src_range: BitRange,
        kind: FeedthroughKind,
        down_name: String,
        witness: PstWitness,
    }

    let mut uncovered: Vec<UncoveredHit> = Vec::new();
    let mut total_checked_bits = 0u32;
    let mut all_routes_uncovered = true;

    for route in &group.feedthrough_routes {
        // An always-on repeater buffer is only required when the traversed domain's
        // supply rail can actually power OFF (not when a FULL_ON rail enters an
        // activity-gated CORRUPT_ON_ACTIVITY / CORRUPT_STATE_ON_ACTIVITY state).
        if !supply_set_can_power_off(ctx, dst_ss) {
            continue;
        }
        let mut route_w = ctx.sat.can_be_off_while_on(dst_ss, src_ss);
        let mut down_name = ctx.domain_name(dst_pd);
        if route_w.is_none() {
            if let (Some(dpd), Some(dss)) = (
                ctx.domain_of_cell(route.downstream_cell),
                ctx.ss_of_cell(route.downstream_cell),
            ) {
                if let Some(w) = ctx.sat.can_be_off_while_on(dst_ss, dss) {
                    route_w = Some(w);
                    down_name = ctx.domain_name(dpd);
                }
            }
        }
        let Some(w) = route_w else {
            continue;
        };

        let sub_routes = split_route_by_repeater_cuts(ctx, group, route);
        for sub_r in sub_routes {
            total_checked_bits += sub_r.src_range.width();
            let covered =
                ctx.resolved.intent.repeater_strategies.iter().any(|rep| {
                    repeater_covers_route(ctx, rep, group, &sub_r, src_pd, dst_pd, src_ss)
                });
            if covered {
                all_routes_uncovered = false;
            } else {
                uncovered.push(UncoveredHit {
                    src_range: sub_r.src_range,
                    kind: sub_r.kind,
                    down_name: down_name.clone(),
                    witness: w.clone(),
                });
            }
        }
    }

    if uncovered.is_empty() || total_checked_bits == 0 {
        return;
    }

    let mid_name = ctx.domain_name(dst_pd);
    let first_kind = uncovered[0].kind;
    let same_kind = uncovered.iter().all(|u| u.kind == first_kind);
    let total_uncov_bits = distinct_bit_width(uncovered.iter().map(|u| u.src_range));
    if all_routes_uncovered && same_kind && total_uncov_bits >= group.width_bits {
        let first = &uncovered[0];
        let (rule_id, kind_lbl) = match first.kind {
            FeedthroughKind::PureWire => ("ISO_FEEDTHROUGH", "pure-wire"),
            FeedthroughKind::Buffered => ("ISO_FEEDTHROUGH_BUFFER", "buffered"),
            FeedthroughKind::Combinational => ("ISO_FEEDTHROUGH_BUFFER", "combinational"),
            FeedthroughKind::None | FeedthroughKind::Sequential => return,
        };
        sink.push(
            rule_id,
            None,
            Some(mid_name.clone()),
            Some(first.down_name.clone()),
            sig_disp.to_string(),
            group.width_bits,
            group.raw_crossings,
            format!(
                "{kind_lbl} feedthrough `{sig_disp}` passes through switchable domain `{mid_name}` without always-on repeater"
            ),
            None,
            rtl_loc.clone(),
            Some(first.witness.clone()),
        );
        return;
    }

    let src_seg = ctx.graph.net(group.src_base_net);
    let src_base = if src_seg.upf_path.is_empty() {
        &src_seg.full_path
    } else {
        &src_seg.upf_path
    };
    let mut emitted: BTreeSet<(&'static str, BitRange, String)> = BTreeSet::new();
    for hit in uncovered {
        let (rule_id, kind_lbl) = match hit.kind {
            FeedthroughKind::PureWire => ("ISO_FEEDTHROUGH", "pure-wire"),
            FeedthroughKind::Buffered => ("ISO_FEEDTHROUGH_BUFFER", "buffered"),
            FeedthroughKind::Combinational => ("ISO_FEEDTHROUGH_BUFFER", "combinational"),
            FeedthroughKind::None | FeedthroughKind::Sequential => continue,
        };
        if !emitted.insert((rule_id, hit.src_range, hit.down_name.clone())) {
            continue;
        }
        let hit_sig = format_bit_range(
            src_base,
            src_seg.width,
            src_seg.packed_bounds,
            hit.src_range,
        );
        sink.push(
            rule_id,
            None,
            Some(mid_name.clone()),
            Some(hit.down_name),
            hit_sig.clone(),
            hit.src_range.width().max(1),
            1,
            format!(
                "{kind_lbl} feedthrough `{hit_sig}` passes through switchable domain `{mid_name}` without always-on repeater"
            ),
            None,
            rtl_loc.clone(),
            Some(hit.witness),
        );
    }
}

fn distinct_bit_width(ranges: impl Iterator<Item = BitRange>) -> u32 {
    let mut sorted: Vec<BitRange> = ranges.filter(|r| r.width() > 0).collect();
    if sorted.is_empty() {
        return 0;
    }
    sorted.sort_by_key(|r| (r.lo, r.hi));
    let mut total = 0u32;
    let mut cur = sorted[0];
    for r in sorted.into_iter().skip(1) {
        if r.lo <= cur.hi.saturating_add(1) {
            cur.hi = cur.hi.max(r.hi);
        } else {
            total += cur.width();
            cur = r;
        }
    }
    total + cur.width()
}

fn supply_set_can_power_off(ctx: &Stage2Ctx<'_>, ss: SupplySetId) -> bool {
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
