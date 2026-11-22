//! Coalesces bit-level and slice `CrossingEdge`s into bus-level `CrossingGroup`s.

use std::collections::{BTreeMap, BTreeSet};

use super::ft_alias::{collect_pre_comb_nets, project_range};
use super::{
    format_bit_range, BitRange, CellId, CrossingEdge, EdgeId, FeedthroughKind, FeedthroughRoute,
    GroupId, InlineClampInfo, NetSegId, PinId, StructuralGraph,
};

#[derive(Debug, Clone)]
pub struct CrossingGroup {
    pub id: GroupId,
    pub src_cell: CellId,
    pub dst_cell: CellId,
    pub src_base_net: NetSegId,
    pub dst_base_net: NetSegId,
    pub src_ranges: Vec<BitRange>,
    pub dst_ranges: Vec<BitRange>,
    pub lanes: Vec<(BitRange, BitRange)>,
    pub src_display: String,
    pub dst_display: String,
    pub src_upf_display: String,
    pub dst_upf_display: String,
    pub width_bits: u32,
    pub raw_crossings: usize,
    pub feedthrough: FeedthroughKind,
    pub inverted: bool,
    pub boundary_pin: Option<PinId>,
    pub mid_cell: Option<CellId>,
    pub mid_nets: Vec<(NetSegId, BitRange)>,
    pub mid_net_scopes: Vec<((NetSegId, BitRange), BitRange, BitRange)>,
    pub feedthrough_pins: Vec<PinId>,
    pub feedthrough_nets: Vec<NetSegId>,
    pub feedthrough_downstream_cells: Vec<CellId>,
    pub feedthrough_routes: Vec<FeedthroughRoute>,
    pub inline_clamps: Vec<InlineClampInfo>,
    pub inline_clamp_scopes: Vec<(InlineClampInfo, BitRange, BitRange)>,
    pub edge_ids: Vec<EdgeId>,
    pub loc: svlang::Loc,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    src_cell: CellId,
    dst_cell: CellId,
    src_net: NetSegId,
    dst_net: NetSegId,
    inverted: bool,
    mid_cell: Option<CellId>,
}

fn merge_group_ft_kind(a: FeedthroughKind, b: FeedthroughKind) -> FeedthroughKind {
    use FeedthroughKind::*;
    match (a, b) {
        (Combinational, _) | (_, Combinational) => Combinational,
        (Buffered, _) | (_, Buffered) => Buffered,
        (PureWire, _) | (_, PureWire) => PureWire,
        (Sequential, _) | (_, Sequential) => Sequential,
        (None, None) => None,
    }
}

/// Coalesces contiguous/overlapping bit-slice crossings into bus-level [`CrossingGroup`]s.
pub fn group_crossings(graph: &StructuralGraph, edges: &[CrossingEdge]) -> Vec<CrossingGroup> {
    let mut buckets: BTreeMap<GroupKey, Vec<&CrossingEdge>> = BTreeMap::new();
    for edge in edges {
        let key = GroupKey {
            src_cell: edge.src_cell,
            dst_cell: edge.dst_cell,
            src_net: edge.src_net,
            dst_net: edge.dst_net,
            inverted: edge.inverted,
            mid_cell: edge.mid_cell,
        };
        buckets.entry(key).or_default().push(edge);
    }

    let mut groups = Vec::with_capacity(buckets.len());
    for (key, group_edges) in buckets {
        let src_seg = graph.net(key.src_net);
        let dst_seg = graph.net(key.dst_net);

        let lanes = merge_lanes(
            group_edges
                .iter()
                .map(|e| (e.src_range, e.dst_range))
                .collect(),
        );
        let src_ranges = merge_ranges(lanes.iter().map(|&(s, _)| s).collect());
        let dst_ranges = merge_ranges(lanes.iter().map(|&(_, d)| d).collect());
        let width_bits: u32 = src_ranges.iter().map(|r| r.width()).sum::<u32>().max(1);

        let src_display = format_ranges(
            &src_seg.full_path,
            src_seg.width,
            src_seg.packed_bounds,
            &src_ranges,
        );
        let dst_display = format_ranges(
            &dst_seg.full_path,
            dst_seg.width,
            dst_seg.packed_bounds,
            &dst_ranges,
        );
        let src_upf_display = format_ranges(
            &src_seg.upf_path,
            src_seg.width,
            src_seg.packed_bounds,
            &src_ranges,
        );
        let dst_upf_display = format_ranges(
            &dst_seg.upf_path,
            dst_seg.width,
            dst_seg.packed_bounds,
            &dst_ranges,
        );

        let mut mid_nets = Vec::new();
        let mut mid_net_scopes = Vec::new();
        let mut ft_pins = Vec::new();
        let mut ft_nets = Vec::new();
        let mut ft_downstream = Vec::new();
        let mut ft_routes = Vec::new();
        let mut clamps = Vec::new();
        let mut clamp_scopes = Vec::new();
        let mut edge_ids = Vec::with_capacity(group_edges.len());
        let first = group_edges[0];
        let mut group_ft = first.feedthrough;
        for e in &group_edges {
            group_ft = merge_group_ft_kind(group_ft, e.feedthrough);
            edge_ids.push(e.id);
            for &mn in &e.mid_nets {
                if !mid_nets.contains(&mn) {
                    mid_nets.push(mn);
                }
                let scope = (mn, e.src_range, e.dst_range);
                if !mid_net_scopes.contains(&scope) {
                    mid_net_scopes.push(scope);
                }
            }
            for &p in &e.feedthrough_pins {
                if !ft_pins.contains(&p) {
                    ft_pins.push(p);
                }
            }
            for &n in &e.feedthrough_nets {
                if !ft_nets.contains(&n) {
                    ft_nets.push(n);
                }
            }
            for &c in &e.feedthrough_downstream_cells {
                if !ft_downstream.contains(&c) {
                    ft_downstream.push(c);
                }
            }
            for r in &e.feedthrough_routes {
                if !ft_routes.contains(r) {
                    ft_routes.push(r.clone());
                }
            }
            for &ic in &e.inline_clamps {
                if !clamps.contains(&ic) {
                    clamps.push(ic);
                }
                let scope = (ic, e.src_range, e.dst_range);
                if !clamp_scopes.contains(&scope) {
                    clamp_scopes.push(scope);
                }
            }
        }

        groups.push(CrossingGroup {
            id: GroupId(groups.len() as u32),
            src_cell: key.src_cell,
            dst_cell: key.dst_cell,
            src_base_net: key.src_net,
            dst_base_net: key.dst_net,
            src_ranges,
            dst_ranges,
            lanes,
            src_display,
            dst_display,
            src_upf_display,
            dst_upf_display,
            width_bits,
            raw_crossings: group_edges.len(),
            feedthrough: group_ft,
            inverted: key.inverted,
            boundary_pin: first.boundary_pin,
            mid_cell: key.mid_cell,
            mid_nets,
            mid_net_scopes,
            feedthrough_pins: ft_pins,
            feedthrough_nets: ft_nets,
            feedthrough_downstream_cells: ft_downstream,
            feedthrough_routes: ft_routes,
            inline_clamps: clamps,
            inline_clamp_scopes: clamp_scopes,
            edge_ids,
            loc: first.loc,
        });
    }

    groups.sort_by(|a, b| {
        (a.src_cell.0, a.dst_cell.0, &a.src_display, &a.dst_display).cmp(&(
            b.src_cell.0,
            b.dst_cell.0,
            &b.src_display,
            &b.dst_display,
        ))
    });
    for (idx, g) in groups.iter_mut().enumerate() {
        g.id = GroupId(idx as u32);
    }
    groups
}

fn merge_lanes(mut pairs: Vec<(BitRange, BitRange)>) -> Vec<(BitRange, BitRange)> {
    if pairs.is_empty() {
        return pairs;
    }
    pairs.sort_by_key(|(s, d)| (s.lo, s.hi, d.lo, d.hi));
    let mut merged = vec![pairs[0]];
    for (sr, dr) in pairs.into_iter().skip(1) {
        let (ls, ld) = merged.last_mut().expect("non-empty");
        if *ls == sr && *ld == dr {
            continue;
        }
        let aligned_1to1 = ls.width() > 0
            && ls.width() == ld.width()
            && sr.width() > 0
            && sr.width() == dr.width()
            && sr.lo <= ls.hi.saturating_add(1)
            && dr.lo >= ld.lo
            && (sr.lo as i64 - ls.lo as i64) == (dr.lo as i64 - ld.lo as i64);
        if aligned_1to1 {
            ls.hi = ls.hi.max(sr.hi);
            ld.hi = ld.hi.max(dr.hi);
        } else {
            merged.push((sr, dr));
        }
    }
    merged
}

fn merge_ranges(mut ranges: Vec<BitRange>) -> Vec<BitRange> {
    if ranges.is_empty() {
        return ranges;
    }
    ranges.sort_by_key(|r| (r.lo, r.hi));
    let mut merged = vec![ranges[0]];
    for r in ranges.into_iter().skip(1) {
        let last = merged.last_mut().expect("non-empty");
        if last.hi.saturating_add(1) >= r.lo {
            last.hi = last.hi.max(r.hi);
        } else {
            merged.push(r);
        }
    }
    merged
}

fn format_ranges(
    base: &str,
    width: u32,
    packed_bounds: Option<(i128, i128)>,
    ranges: &[BitRange],
) -> String {
    if ranges.len() == 1 {
        let r = ranges[0];
        if width > 1 && r.lo == 0 && r.hi + 1 >= width {
            let (left, right) = packed_bounds.unwrap_or(((width - 1) as i128, 0));
            return format!("{base}[{left}:{right}]");
        }
        return format_bit_range(base, width, packed_bounds, r);
    }
    let slices: Vec<String> = ranges
        .iter()
        .rev()
        .map(|&r| {
            let s = format_bit_range("", width, packed_bounds, r);
            s.trim_start_matches('[').trim_end_matches(']').to_string()
        })
        .collect();
    format!("{base}[{}]", slices.join(","))
}

/// Extracts the sub-ranges inside `ranges` corresponding to cumulative 0-based bit offsets
/// `[off_lo ..= off_hi]`.
pub fn map_range_list_offset(ranges: &[BitRange], off_lo: u32, off_hi: u32) -> Vec<BitRange> {
    if off_lo > off_hi {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cum = 0u32;
    for &r in ranges {
        let w = r.width();
        let seg_lo = cum;
        let seg_hi = cum + w.saturating_sub(1);
        if w > 0 && off_lo <= seg_hi && seg_lo <= off_hi {
            let sub_lo = off_lo.max(seg_lo) - seg_lo;
            let sub_hi = off_hi.min(seg_hi) - seg_lo;
            out.push(BitRange {
                lo: r.lo + sub_lo,
                hi: r.lo + sub_hi,
            });
        }
        cum += w;
    }
    out
}

/// Splits a [`CrossingGroup`] along explicit strategy slice boundaries (`cuts` in `src_base_net`
/// physical coordinates), preserving paired `(src_range, dst_range)` bit lanes across permuted
/// concatenations and discontiguous slices.
pub fn split_crossing_group_by_cuts(
    graph: &StructuralGraph,
    group: &CrossingGroup,
    cuts: &[BitRange],
) -> Vec<CrossingGroup> {
    if cuts.is_empty() || (group.lanes.is_empty() && group.src_ranges.is_empty()) {
        return vec![group.clone()];
    }
    if !group.lanes.is_empty() {
        return split_by_lanes(graph, group, cuts);
    }
    if group.src_ranges.len() > 1 {
        let uniform = cuts.iter().all(|&c| {
            let hits_any = group.src_ranges.iter().any(|sr| sr.overlaps(c));
            !hits_any
                || group
                    .src_ranges
                    .iter()
                    .all(|sr| c.lo <= sr.lo && c.hi >= sr.hi)
        });
        if uniform {
            return vec![group.clone()];
        }
    }

    let total_src_w: u32 = group.src_ranges.iter().map(|r| r.width()).sum();
    let total_dst_w: u32 = group.dst_ranges.iter().map(|r| r.width()).sum();
    let prop_dst = total_src_w > 0 && total_dst_w == total_src_w;

    let src_seg = graph.net(group.src_base_net);
    let dst_seg = graph.net(group.dst_base_net);
    let mut out = Vec::new();
    let mut cum_off = 0u32;

    for &sr in &group.src_ranges {
        let mut pts: BTreeSet<u32> = BTreeSet::new();
        pts.insert(sr.lo);
        pts.insert(sr.hi + 1);
        for &c in cuts {
            if let Some(ov) = sr.intersect(c) {
                pts.insert(ov.lo);
                pts.insert(ov.hi + 1);
            }
        }
        let pts_vec: Vec<u32> = pts.into_iter().collect();
        for w in pts_vec.windows(2) {
            if w[0] >= w[1] {
                continue;
            }
            let sub_src = BitRange {
                lo: w[0],
                hi: w[1] - 1,
            };
            let sub_dst_ranges = if prop_dst {
                let off_lo = cum_off + (sub_src.lo - sr.lo);
                let off_hi = cum_off + (sub_src.hi - sr.lo);
                let mapped = map_range_list_offset(&group.dst_ranges, off_lo, off_hi);
                if mapped.is_empty() {
                    group.dst_ranges.clone()
                } else {
                    mapped
                }
            } else {
                group.dst_ranges.clone()
            };
            let sub_src_ranges = vec![sub_src];
            let mut sub = group.clone();
            sub.src_display = format_ranges(
                &src_seg.full_path,
                src_seg.width,
                src_seg.packed_bounds,
                &sub_src_ranges,
            );
            sub.dst_display = format_ranges(
                &dst_seg.full_path,
                dst_seg.width,
                dst_seg.packed_bounds,
                &sub_dst_ranges,
            );
            sub.src_upf_display = format_ranges(
                &src_seg.upf_path,
                src_seg.width,
                src_seg.packed_bounds,
                &sub_src_ranges,
            );
            sub.dst_upf_display = format_ranges(
                &dst_seg.upf_path,
                dst_seg.width,
                dst_seg.packed_bounds,
                &sub_dst_ranges,
            );
            sub.src_ranges = sub_src_ranges;
            sub.dst_ranges = sub_dst_ranges;
            sub.width_bits = sub_src.width().max(1);
            retain_sub_inline_clamps(group, &mut sub);
            retain_sub_ft_routes(graph, group, &mut sub);
            retain_sub_mid_nets(group, &mut sub);
            out.push(sub);
        }
        cum_off += sr.width();
    }

    if (out.len() <= 1 && group.src_ranges.len() == 1) || out.is_empty() {
        vec![group.clone()]
    } else {
        out
    }
}

fn retain_sub_ft_routes(graph: &StructuralGraph, parent: &CrossingGroup, sub: &mut CrossingGroup) {
    if parent.feedthrough_routes.is_empty() {
        return;
    }
    let mut kept = Vec::new();
    for r in &parent.feedthrough_routes {
        for &sr in &sub.src_ranges {
            let Some(s_ov) = r.src_range.intersect(sr) else {
                continue;
            };
            for &dr in &sub.dst_ranges {
                let Some(d_ov) = r.dst_range.intersect(dr) else {
                    continue;
                };
                let mut nr = r.clone();
                nr.src_range = s_ov;
                nr.dst_range = d_ov;
                if r.dst_range.width() == d_ov.width() {
                    kept.push(nr);
                } else if r.kind == FeedthroughKind::Combinational {
                    // Preserve downstream reaching_pins/nets on bit-mixing Combinational
                    // routes, while narrowing `dst_base_net` and pre-combinational pure-wire
                    // alias nets so input-side `-exclude_elements` slices do not over-exclude.
                    let pre_comb = collect_pre_comb_nets(
                        graph,
                        parent.dst_base_net,
                        r.dst_range,
                        &r.reaching_nets,
                    );
                    let mut narrowed_nets = Vec::with_capacity(nr.reaching_nets.len());
                    for (nid, nrr) in nr.reaching_nets {
                        let pre_matches: Vec<(BitRange, BitRange)> = pre_comb
                            .iter()
                            .filter(|&&(pn, pnr, _)| pn == nid && pnr.overlaps(nrr))
                            .map(|&(_, pnr, pdst)| (pnr, pdst))
                            .collect();
                        if pre_matches.is_empty() {
                            narrowed_nets.push((nid, nrr));
                        } else {
                            for (pnr, pdst) in pre_matches {
                                if let Some(ov) = pdst.intersect(d_ov) {
                                    let proj = project_range(ov, pdst, pnr);
                                    if !narrowed_nets.contains(&(nid, proj)) {
                                        narrowed_nets.push((nid, proj));
                                    }
                                }
                            }
                        }
                    }
                    nr.reaching_nets = narrowed_nets;
                    kept.push(nr);
                } else {
                    // Project reaching_pins/nets whose width matches `r.dst_range` using offset.
                    for (_, pr) in &mut nr.reaching_pins {
                        if r.dst_range.width() > 0
                            && pr.width() == r.dst_range.width()
                            && d_ov.lo >= r.dst_range.lo
                            && d_ov.hi <= r.dst_range.hi
                        {
                            *pr = BitRange {
                                lo: pr.lo + (d_ov.lo - r.dst_range.lo),
                                hi: pr.lo + (d_ov.hi - r.dst_range.lo),
                            };
                        }
                    }
                    for (_, nrr) in &mut nr.reaching_nets {
                        if r.dst_range.width() > 0
                            && nrr.width() == r.dst_range.width()
                            && d_ov.lo >= r.dst_range.lo
                            && d_ov.hi <= r.dst_range.hi
                        {
                            *nrr = BitRange {
                                lo: nrr.lo + (d_ov.lo - r.dst_range.lo),
                                hi: nrr.lo + (d_ov.hi - r.dst_range.lo),
                            };
                        }
                    }
                    kept.push(nr);
                }
            }
        }
    }
    sub.feedthrough_routes = kept;
    let mut pins = Vec::new();
    let mut nets = Vec::new();
    let mut dcs = Vec::new();
    for r in &sub.feedthrough_routes {
        if !dcs.contains(&r.downstream_cell) {
            dcs.push(r.downstream_cell);
        }
        for &(p, _) in &r.reaching_pins {
            if !pins.contains(&p) {
                pins.push(p);
            }
        }
        for &(n, _) in &r.reaching_nets {
            if !nets.contains(&n) {
                nets.push(n);
            }
        }
    }
    sub.feedthrough_downstream_cells = dcs;
    sub.feedthrough_pins = pins;
    sub.feedthrough_nets = nets;
}

fn retain_sub_inline_clamps(parent: &CrossingGroup, sub: &mut CrossingGroup) {
    sub.inline_clamps.retain(|ic| {
        let net_ok = (ic.data_net != parent.src_base_net
            || sub.src_ranges.iter().any(|r| ic.data_range.overlaps(*r)))
            && (ic.data_net != parent.dst_base_net
                || sub.dst_ranges.iter().any(|r| ic.data_range.overlaps(*r)));
        let scope_ok = (ic.data_net == parent.src_base_net || ic.data_net == parent.dst_base_net)
            || parent.inline_clamp_scopes.is_empty()
            || parent.inline_clamp_scopes.iter().any(|&(c_ic, es, ed)| {
                c_ic == *ic
                    && sub.src_ranges.iter().any(|r| r.overlaps(es))
                    && sub.dst_ranges.iter().any(|r| r.overlaps(ed))
            });
        net_ok && scope_ok
    });
    sub.inline_clamp_scopes
        .retain(|&(ic, _, _)| sub.inline_clamps.contains(&ic));
}

fn retain_sub_mid_nets(parent: &CrossingGroup, sub: &mut CrossingGroup) {
    if parent.mid_net_scopes.is_empty() {
        return;
    }
    let lane_pairs: Vec<(BitRange, BitRange)> = if !sub.lanes.is_empty() {
        sub.lanes.clone()
    } else {
        sub.src_ranges
            .iter()
            .flat_map(|&sr| sub.dst_ranges.iter().map(move |&dr| (sr, dr)))
            .collect()
    };
    let mut kept_nets = Vec::new();
    let mut kept_scopes = Vec::new();
    for &((mn, mr), es, ed) in &parent.mid_net_scopes {
        for &(sr, dr) in &lane_pairs {
            let (Some(s_ov), Some(d_ov)) = (sr.intersect(es), dr.intersect(ed)) else {
                continue;
            };
            let sub_mr = if mr.width() == es.width() && s_ov.width() < es.width() {
                BitRange {
                    lo: mr.lo + (s_ov.lo - es.lo),
                    hi: mr.lo + (s_ov.hi - es.lo),
                }
            } else if mr.width() == ed.width() && d_ov.width() < ed.width() {
                BitRange {
                    lo: mr.lo + (d_ov.lo - ed.lo),
                    hi: mr.lo + (d_ov.hi - ed.lo),
                }
            } else {
                mr
            };
            let net_entry = (mn, sub_mr);
            if !kept_nets.contains(&net_entry) {
                kept_nets.push(net_entry);
            }
            let scope_entry = (net_entry, s_ov, d_ov);
            if !kept_scopes.contains(&scope_entry) {
                kept_scopes.push(scope_entry);
            }
        }
    }
    sub.mid_nets = kept_nets;
    sub.mid_net_scopes = kept_scopes;
}

fn split_by_lanes(
    graph: &StructuralGraph,
    group: &CrossingGroup,
    cuts: &[BitRange],
) -> Vec<CrossingGroup> {
    if group.lanes.len() > 1 {
        let has_overlapping_src_lanes = (0..group.lanes.len()).any(|i| {
            ((i + 1)..group.lanes.len()).any(|j| group.lanes[i].0.overlaps(group.lanes[j].0))
        });
        let uniform = !has_overlapping_src_lanes
            && cuts.iter().all(|&c| {
                let hits_any = group.lanes.iter().any(|&(sr, _)| sr.overlaps(c));
                !hits_any
                    || group
                        .lanes
                        .iter()
                        .all(|&(sr, _)| c.lo <= sr.lo && c.hi >= sr.hi)
            });
        if uniform {
            return vec![group.clone()];
        }
    }

    let src_seg = graph.net(group.src_base_net);
    let dst_seg = graph.net(group.dst_base_net);
    let mut out = Vec::new();

    for &(sr, dr) in &group.lanes {
        let mut pts: BTreeSet<u32> = BTreeSet::new();
        pts.insert(sr.lo);
        pts.insert(sr.hi + 1);
        for &c in cuts {
            if let Some(ov) = sr.intersect(c) {
                pts.insert(ov.lo);
                pts.insert(ov.hi + 1);
            }
        }
        let pts_vec: Vec<u32> = pts.into_iter().collect();
        for w in pts_vec.windows(2) {
            if w[0] >= w[1] {
                continue;
            }
            let sub_src = BitRange {
                lo: w[0],
                hi: w[1] - 1,
            };
            let sub_dst = if sr.width() > 0 && sr.width() == dr.width() {
                let off_lo = sub_src.lo - sr.lo;
                let off_hi = sub_src.hi - sr.lo;
                BitRange {
                    lo: dr.lo + off_lo,
                    hi: dr.lo + off_hi,
                }
            } else {
                dr
            };
            let sub_src_ranges = vec![sub_src];
            let sub_dst_ranges = vec![sub_dst];
            let mut sub = group.clone();
            sub.lanes = vec![(sub_src, sub_dst)];
            sub.src_display = format_ranges(
                &src_seg.full_path,
                src_seg.width,
                src_seg.packed_bounds,
                &sub_src_ranges,
            );
            sub.dst_display = format_ranges(
                &dst_seg.full_path,
                dst_seg.width,
                dst_seg.packed_bounds,
                &sub_dst_ranges,
            );
            sub.src_upf_display = format_ranges(
                &src_seg.upf_path,
                src_seg.width,
                src_seg.packed_bounds,
                &sub_src_ranges,
            );
            sub.dst_upf_display = format_ranges(
                &dst_seg.upf_path,
                dst_seg.width,
                dst_seg.packed_bounds,
                &sub_dst_ranges,
            );
            sub.src_ranges = sub_src_ranges;
            sub.dst_ranges = sub_dst_ranges;
            sub.width_bits = sub_src.width().max(1);
            retain_sub_inline_clamps(group, &mut sub);
            retain_sub_ft_routes(graph, group, &mut sub);
            retain_sub_mid_nets(group, &mut sub);
            out.push(sub);
        }
    }

    if (out.len() <= 1 && group.lanes.len() == 1) || out.is_empty() {
        vec![group.clone()]
    } else {
        out
    }
}
