//! Parent pure-wire pass-through alias traversal, range projection,
//! and intra-cell feedthrough sub-interval splitting helpers.

use std::collections::{BTreeSet, HashSet, VecDeque};

use super::{
    BitRange, CellId, DriverOrigin, ExprOpSummary, InlineClampInfo, LoadEdge, NetSegId, NetSegKind,
    PinId, StructuralGraph,
};

pub(super) struct ParentAliasReach {
    pub parent_loads: Vec<(NetSegId, BitRange, BitRange, bool)>,
    pub sibling_loads: Vec<ParentSiblingHit>,
    pub mid_nets: Vec<(NetSegId, BitRange)>,
}

pub(super) struct ParentSiblingHit {
    pub src_sub: BitRange,
    pub overlap_p: BitRange,
    pub dpin_id: PinId,
    pub dnet: NetSegId,
    pub drange: BitRange,
    pub conn_clamp: Option<InlineClampInfo>,
    pub boundary_inv: bool,
    pub mid_nets: Vec<(NetSegId, BitRange)>,
}

pub(super) fn project_range(sub: BitRange, from_range: BitRange, to_range: BitRange) -> BitRange {
    if from_range.width() == to_range.width() && from_range.width() > 0 {
        let lo_off = sub.lo.saturating_sub(from_range.lo);
        let hi_off = sub.hi.saturating_sub(from_range.lo);
        let lo = to_range.lo.saturating_add(lo_off).min(to_range.hi);
        let hi = to_range.lo.saturating_add(hi_off).min(to_range.hi);
        BitRange { lo, hi: hi.max(lo) }
    } else {
        to_range
    }
}

pub(super) fn is_same_domain_in_subtree(
    graph: &StructuralGraph,
    domain_of: &dyn Fn(CellId) -> Option<u32>,
    root_cell: CellId,
    d_root: Option<u32>,
    candidate: CellId,
) -> bool {
    if candidate == root_cell {
        return true;
    }
    if domain_of(candidate) != d_root {
        return false;
    }
    let mut cur = graph.cell(candidate).parent;
    while let Some(p) = cur {
        if p == root_cell {
            return true;
        }
        cur = graph.cell(p).parent;
    }
    false
}

pub(super) fn is_pure_wire_alias_load(
    graph: &StructuralGraph,
    src_net: NetSegId,
    load: &LoadEdge,
) -> Option<(NetSegId, BitRange)> {
    if load.dst_pin.is_some() || !matches!(load.op, ExprOpSummary::Identity) {
        return None;
    }
    let dn = load.dst_net?;
    let dseg = graph.net(dn);
    if dseg.is_seq_state || dseg.cell != graph.net(src_net).cell {
        return None;
    }
    if is_buf_gate_edge(graph, src_net, dn, load.dst_range) {
        return None;
    }
    Some((dn, load.dst_range))
}

pub(super) fn net_has_local_non_childpin_driver(
    graph: &StructuralGraph,
    net: NetSegId,
    range: BitRange,
) -> bool {
    let mut visited: HashSet<(NetSegId, BitRange)> = HashSet::new();
    let mut queue: VecDeque<(NetSegId, BitRange, usize)> = VecDeque::new();
    visited.insert((net, range));
    queue.push_back((net, range, 0));

    while let Some((cur_net, cur_range, depth)) = queue.pop_front() {
        let seg = graph.net(cur_net);
        if seg.drivers.is_empty() && cur_net != net {
            return true;
        }
        for drv in &seg.drivers {
            let Some(ov) = drv.dst_range.intersect(cur_range) else {
                continue;
            };
            match drv.origin {
                DriverOrigin::ChildPin(_) => continue,
                DriverOrigin::Assign | DriverOrigin::NetAlias
                    if matches!(drv.op, ExprOpSummary::Identity)
                        && drv.src_net.is_some()
                        && drv.src_range.is_some()
                        && !is_buf_gate_edge(
                            graph,
                            drv.src_net.unwrap(),
                            cur_net,
                            drv.dst_range,
                        ) =>
                {
                    if depth >= 32 {
                        return true;
                    }
                    let sn = drv.src_net.unwrap();
                    let sr = drv.src_range.unwrap();
                    if graph.net(sn).cell != seg.cell {
                        return true;
                    }
                    let next_r = project_range(ov, drv.dst_range, sr);
                    if visited.insert((sn, next_r)) {
                        queue.push_back((sn, next_r, depth + 1));
                    }
                }
                _ => return true,
            }
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_parent_alias_reach(
    graph: &StructuralGraph,
    src_cell: CellId,
    parent_cell: CellId,
    pnet: NetSegId,
    prange: BitRange,
    src_range: BitRange,
    op_is_inv: &dyn Fn(&ExprOpSummary) -> bool,
) -> ParentAliasReach {
    let mut parent_loads: Vec<(NetSegId, BitRange, BitRange, bool)> = Vec::new();
    let mut sibling_loads: Vec<ParentSiblingHit> = Vec::new();
    let mut all_mid_nets: Vec<(NetSegId, BitRange)> = Vec::new();
    let mut visited: HashSet<(NetSegId, BitRange, bool)> = HashSet::new();
    struct AliasQueueItem {
        pnet: NetSegId,
        pr: BitRange,
        src_r: BitRange,
        inv: bool,
        mids: Vec<(NetSegId, BitRange)>,
        depth: usize,
    }
    let mut queue: VecDeque<AliasQueueItem> = VecDeque::new();

    visited.insert((pnet, prange, false));
    queue.push_back(AliasQueueItem {
        pnet,
        pr: prange,
        src_r: src_range,
        inv: false,
        mids: vec![(pnet, prange)],
        depth: 0,
    });
    all_mid_nets.push((pnet, prange));

    while let Some(AliasQueueItem {
        pnet: cur_pnet,
        pr: cur_pr,
        src_r: cur_src_r,
        inv: cur_inv,
        mids: cur_mids,
        depth,
    }) = queue.pop_front()
    {
        let pseg = graph.net(cur_pnet);
        if matches!(
            pseg.kind,
            NetSegKind::Port(svlang::PortDir::Output | svlang::PortDir::Inout)
        ) {
            let entry = (cur_pnet, cur_src_r, cur_pr, cur_inv);
            if !parent_loads.contains(&entry) {
                parent_loads.push(entry);
            }
        }
        for load in &pseg.loads {
            let Some(overlap_p) = load.src_range.intersect(cur_pr) else {
                continue;
            };
            let sub_src = project_range(overlap_p, cur_pr, cur_src_r);
            let boundary_inv = cur_inv ^ op_is_inv(&load.op);
            if let Some(dst_pin_id) = load.dst_pin {
                let dpin = graph.pin(dst_pin_id);
                if dpin.cell == src_cell {
                    continue;
                }
                if dpin.cell == parent_cell {
                    let entry = (cur_pnet, sub_src, overlap_p, boundary_inv);
                    if !parent_loads.contains(&entry) {
                        parent_loads.push(entry);
                    }
                } else if let Some(dnet) = dpin.internal_net {
                    let clamp = dpin
                        .external_conns
                        .iter()
                        .find(|c| {
                            c.parent_net == Some(cur_pnet) && c.pin_range.overlaps(load.dst_range)
                        })
                        .and_then(|c| c.inline_clamp);
                    let drange = project_range(overlap_p, load.src_range, load.dst_range);
                    let mids: Vec<(NetSegId, BitRange)> = cur_mids
                        .iter()
                        .map(|&(mn, mr)| {
                            let narrowed = if mr.width() == cur_pr.width() {
                                project_range(overlap_p, cur_pr, mr)
                            } else {
                                mr
                            };
                            (mn, narrowed)
                        })
                        .collect();
                    sibling_loads.push(ParentSiblingHit {
                        src_sub: sub_src,
                        overlap_p,
                        dpin_id: dst_pin_id,
                        dnet,
                        drange,
                        conn_clamp: clamp,
                        boundary_inv,
                        mid_nets: mids,
                    });
                }
            } else if let Some((dn, _)) = is_pure_wire_alias_load(graph, cur_pnet, load) {
                if depth < 32 {
                    let next_pr = project_range(overlap_p, load.src_range, load.dst_range);
                    if visited.insert((dn, next_pr, cur_inv)) {
                        let mut next_mids: Vec<(NetSegId, BitRange)> = cur_mids
                            .iter()
                            .map(|&(mn, mr)| {
                                let narrowed = if mr.width() == cur_pr.width() {
                                    project_range(overlap_p, cur_pr, mr)
                                } else {
                                    mr
                                };
                                (mn, narrowed)
                            })
                            .collect();
                        next_mids.push((dn, next_pr));
                        if !all_mid_nets.contains(&(dn, next_pr)) {
                            all_mid_nets.push((dn, next_pr));
                        }
                        queue.push_back(AliasQueueItem {
                            pnet: dn,
                            pr: next_pr,
                            src_r: sub_src,
                            inv: cur_inv,
                            mids: next_mids,
                            depth: depth + 1,
                        });
                    }
                }
            } else {
                let entry = (cur_pnet, sub_src, overlap_p, boundary_inv);
                if !parent_loads.contains(&entry) {
                    parent_loads.push(entry);
                }
            }
        }
    }

    ParentAliasReach {
        parent_loads,
        sibling_loads,
        mid_nets: all_mid_nets,
    }
}

pub(super) fn collect_dst_sub_intervals(
    graph: &StructuralGraph,
    domain_of: &dyn Fn(CellId) -> Option<u32>,
    cell_id: CellId,
    start_net: NetSegId,
    start_range: BitRange,
) -> Vec<BitRange> {
    if start_range.width() <= 1 {
        return vec![start_range];
    }
    let d_cell = domain_of(cell_id);
    let mut cuts: BTreeSet<u32> = BTreeSet::new();
    cuts.insert(start_range.lo);
    cuts.insert(start_range.hi + 1);
    let mut visited: HashSet<(NetSegId, BitRange)> = HashSet::new();
    let mut queue: VecDeque<(NetSegId, BitRange, i64, usize)> = VecDeque::new();
    queue.push_back((start_net, start_range, 0, 0));
    visited.insert((start_net, start_range));

    let add_cut_from_range = |cuts: &mut BTreeSet<u32>, ov: BitRange, base_delta: i64| {
        let mapped_lo = (ov.lo as i64 - base_delta).max(start_range.lo as i64) as u32;
        let mapped_hi = (ov.hi as i64 - base_delta).min(start_range.hi as i64) as u32;
        if mapped_lo <= mapped_hi && (mapped_lo > start_range.lo || mapped_hi < start_range.hi) {
            cuts.insert(mapped_lo);
            cuts.insert(mapped_hi + 1);
        }
    };

    while let Some((net, cur_range, base_delta, depth)) = queue.pop_front() {
        let seg = graph.net(net);
        if !is_same_domain_in_subtree(graph, domain_of, cell_id, d_cell, seg.cell) {
            continue;
        }
        if seg.cell == cell_id {
            for &pid in &graph.cell(cell_id).pins {
                let p = graph.pin(pid);
                if p.internal_net == Some(net)
                    && matches!(
                        p.dir,
                        Some(svlang::PortDir::Output | svlang::PortDir::Inout)
                    )
                {
                    for conn in &p.external_conns {
                        let Some(pin_ov) = cur_range.intersect(conn.pin_range) else {
                            continue;
                        };
                        add_cut_from_range(&mut cuts, pin_ov, base_delta);
                        if let (Some(pnet), Some(parent_id)) =
                            (conn.parent_net, graph.cell(cell_id).parent)
                        {
                            if conn.pin_range.width() == conn.parent_range.width() {
                                let p_ov = project_range(pin_ov, conn.pin_range, conn.parent_range);
                                let p_delta = base_delta + (p_ov.lo as i64 - pin_ov.lo as i64);
                                collect_parent_sub_cuts(
                                    graph,
                                    parent_id,
                                    pnet,
                                    p_ov,
                                    p_delta,
                                    start_range,
                                    &mut cuts,
                                );
                            }
                        }
                    }
                }
            }
        }
        for load in &seg.loads {
            let Some(ov) = load.src_range.intersect(cur_range) else {
                continue;
            };
            add_cut_from_range(&mut cuts, ov, base_delta);
            if depth < 12
                && matches!(load.op, ExprOpSummary::Identity | ExprOpSummary::Invert)
                && load.src_range.width() == load.dst_range.width()
            {
                if let Some(dn) = load.dst_net {
                    if is_same_domain_in_subtree(
                        graph,
                        domain_of,
                        cell_id,
                        d_cell,
                        graph.net(dn).cell,
                    ) {
                        let next_r = project_range(ov, load.src_range, load.dst_range);
                        let delta = base_delta + (next_r.lo as i64 - ov.lo as i64);
                        if visited.insert((dn, next_r)) {
                            queue.push_back((dn, next_r, delta, depth + 1));
                        }
                    }
                }
            }
        }
    }

    let pts: Vec<u32> = cuts.into_iter().collect();
    if pts.len() <= 2 {
        return vec![start_range];
    }
    pts.windows(2)
        .filter(|w| w[0] < w[1])
        .map(|w| BitRange {
            lo: w[0],
            hi: w[1] - 1,
        })
        .collect()
}

fn collect_parent_sub_cuts(
    graph: &StructuralGraph,
    parent_id: CellId,
    start_pnet: NetSegId,
    start_prange: BitRange,
    start_delta: i64,
    root_range: BitRange,
    cuts: &mut BTreeSet<u32>,
) {
    let mut visited: HashSet<(NetSegId, BitRange)> = HashSet::new();
    let mut queue: VecDeque<(NetSegId, BitRange, i64, usize)> = VecDeque::new();
    visited.insert((start_pnet, start_prange));
    queue.push_back((start_pnet, start_prange, start_delta, 0));

    while let Some((pnet, pr, delta, depth)) = queue.pop_front() {
        let pseg = graph.net(pnet);
        if pseg.cell != parent_id {
            continue;
        }
        for load in &pseg.loads {
            let Some(ov) = load.src_range.intersect(pr) else {
                continue;
            };
            let mapped_lo = (ov.lo as i64 - delta).max(root_range.lo as i64) as u32;
            let mapped_hi = (ov.hi as i64 - delta).min(root_range.hi as i64) as u32;
            if mapped_lo <= mapped_hi && (mapped_lo > root_range.lo || mapped_hi < root_range.hi) {
                cuts.insert(mapped_lo);
                cuts.insert(mapped_hi + 1);
            }
            if depth < 8
                && is_pure_wire_alias_load(graph, pnet, load).is_some()
                && load.src_range.width() == load.dst_range.width()
            {
                if let Some(dn) = load.dst_net {
                    let next_r = project_range(ov, load.src_range, load.dst_range);
                    let next_delta = delta + (next_r.lo as i64 - ov.lo as i64);
                    if visited.insert((dn, next_r)) {
                        queue.push_back((dn, next_r, next_delta, depth + 1));
                    }
                }
            }
        }
    }
}

pub(super) fn is_buf_gate_edge(
    graph: &StructuralGraph,
    src_net: NetSegId,
    dst_net: NetSegId,
    dst_range: BitRange,
) -> bool {
    graph.net(dst_net).drivers.iter().any(|drv| {
        drv.dst_range.overlaps(dst_range)
            && matches!(drv.origin, DriverOrigin::Gate(ref g) if g == "buf" || g == "bufif0" || g == "bufif1")
            && (drv.src_net == Some(src_net)
                || drv.extra_deps.iter().any(|&(en, _)| en == src_net))
    })
}

/// Walks forward along pure-wire (`ExprOpSummary::Identity`, non-gate) alias loads starting at
/// `(start_net, start_range)` before any combinational/buffered hop, returning
/// `(net_id, net_range, corresponding_start_range)` triples for nets on `reaching_nets`.
pub(super) fn collect_pre_comb_nets(
    graph: &StructuralGraph,
    start_net: NetSegId,
    start_range: BitRange,
    reaching_nets: &[(NetSegId, BitRange)],
) -> Vec<(NetSegId, BitRange, BitRange)> {
    let mut out = vec![(start_net, start_range, start_range)];
    let mut visited: HashSet<(NetSegId, BitRange)> = HashSet::new();
    let mut queue: VecDeque<(NetSegId, BitRange, BitRange, usize)> = VecDeque::new();
    visited.insert((start_net, start_range));
    queue.push_back((start_net, start_range, start_range, 0));

    while let Some((cur_n, cur_r, cur_dst_r, depth)) = queue.pop_front() {
        if depth >= 32 {
            continue;
        }
        for load in &graph.net(cur_n).loads {
            if load.dst_pin.is_some() || !matches!(load.op, ExprOpSummary::Identity) {
                continue;
            }
            let Some(dn) = load.dst_net else {
                continue;
            };
            if dn == cur_n || graph.net(dn).is_seq_state {
                continue;
            }
            if !reaching_nets.iter().any(|&(rn, _)| rn == dn) {
                continue;
            }
            if is_buf_gate_edge(graph, cur_n, dn, load.dst_range) {
                continue;
            }
            let Some(ov) = load.src_range.intersect(cur_r) else {
                continue;
            };
            if load.src_range.width() == 0 || load.src_range.width() != load.dst_range.width() {
                continue;
            }
            let next_r = project_range(ov, load.src_range, load.dst_range);
            let pure_drivers = graph.net(dn).drivers.iter().all(|d| {
                !d.dst_range.overlaps(next_r)
                    || (matches!(d.op, ExprOpSummary::Identity)
                        && !matches!(d.origin, DriverOrigin::Gate(_)))
            });
            if !pure_drivers {
                continue;
            }
            let next_dst_r = project_range(ov, cur_r, cur_dst_r);
            if visited.insert((dn, next_r)) {
                out.push((dn, next_r, next_dst_r));
                queue.push_back((dn, next_r, next_dst_r, depth + 1));
            }
        }
    }
    out
}

pub(super) type UpstreamOrigin = (CellId, NetSegId, BitRange, Option<PinId>);

/// If `child_cell`'s output `internal_net[pin_range]` is driven purely by passive
/// `Identity` wires from input pin(s) of `child_cell` (no local gates, buffers,
/// sequential elements, or constants), resolves and returns the true upstream
/// driver(s) `(src_cell, src_net, src_range, boundary_pin)` feeding those input pins.
pub(super) fn resolve_pure_wire_ft_upstream(
    graph: &StructuralGraph,
    child_cell: CellId,
    internal_net: NetSegId,
    pin_range: BitRange,
) -> Option<Vec<UpstreamOrigin>> {
    resolve_pure_wire_ft_inner(graph, child_cell, internal_net, pin_range, 0)
}

fn find_cell_input_pin(graph: &StructuralGraph, cell: CellId, net: NetSegId) -> Option<PinId> {
    graph.cell(cell).pins.iter().copied().find(|&pid| {
        let p = graph.pin(pid);
        p.internal_net == Some(net) && p.dir == Some(svlang::PortDir::Input)
    })
}

fn trace_pure_wire_inputs_in_cell(
    graph: &StructuralGraph,
    cell: CellId,
    start_net: NetSegId,
    start_range: BitRange,
) -> Option<Vec<(PinId, BitRange)>> {
    let mut visited: HashSet<(NetSegId, BitRange)> = HashSet::new();
    let mut queue: VecDeque<(NetSegId, BitRange, usize)> = VecDeque::new();
    let mut in_pins: Vec<(PinId, BitRange)> = Vec::new();
    visited.insert((start_net, start_range));
    queue.push_back((start_net, start_range, 0));

    while let Some((cur_n, cur_r, depth)) = queue.pop_front() {
        if depth >= 32 {
            return None;
        }
        let seg = graph.net(cur_n);
        if seg.cell != cell || seg.is_seq_state {
            return None;
        }
        let ov_drvs: Vec<_> = seg
            .drivers
            .iter()
            .filter(|d| d.dst_range.overlaps(cur_r))
            .collect();
        if let Some(in_pid) = find_cell_input_pin(graph, cell, cur_n) {
            let has_non_parent_drv = ov_drvs
                .iter()
                .any(|d| !matches!(d.origin, DriverOrigin::ParentPort(_)));
            if !has_non_parent_drv {
                if !in_pins.contains(&(in_pid, cur_r)) {
                    in_pins.push((in_pid, cur_r));
                }
                continue;
            }
        }
        if ov_drvs.is_empty() {
            return None;
        }
        for drv in ov_drvs {
            if !matches!(drv.op, ExprOpSummary::Identity)
                || matches!(drv.origin, DriverOrigin::Gate(_) | DriverOrigin::Blackbox)
                || !drv.extra_deps.is_empty()
                || drv.inline_clamp.is_some()
            {
                return None;
            }
            let (Some(sn), Some(sr)) = (drv.src_net, drv.src_range) else {
                return None;
            };
            if graph.net(sn).cell != cell || sn == cur_n {
                return None;
            }
            let ov = cur_r.intersect(drv.dst_range)?;
            let next_r = if drv.dst_range.width() == sr.width() && sr.width() > 0 {
                project_range(ov, drv.dst_range, sr)
            } else {
                sr
            };
            if visited.insert((sn, next_r)) {
                queue.push_back((sn, next_r, depth + 1));
            }
        }
    }
    (!in_pins.is_empty()).then_some(in_pins)
}

fn resolve_pure_wire_ft_inner(
    graph: &StructuralGraph,
    child_cell: CellId,
    internal_net: NetSegId,
    pin_range: BitRange,
    depth: usize,
) -> Option<Vec<UpstreamOrigin>> {
    if depth >= 16 {
        return None;
    }
    let in_pins = trace_pure_wire_inputs_in_cell(graph, child_cell, internal_net, pin_range)?;
    let parent_cell = graph.cell(child_cell).parent?;
    let mut origins: Vec<UpstreamOrigin> = Vec::new();
    let mut visited_p: HashSet<(NetSegId, BitRange)> = HashSet::new();
    let mut pq: VecDeque<(NetSegId, BitRange, usize)> = VecDeque::new();

    for (in_pid, in_r) in in_pins {
        let p = graph.pin(in_pid);
        for conn in &p.external_conns {
            let Some(ov) = in_r.intersect(conn.pin_range) else {
                continue;
            };
            if let Some(pnet) = conn.parent_net {
                let pr = project_range(ov, conn.pin_range, conn.parent_range);
                if visited_p.insert((pnet, pr)) {
                    pq.push_back((pnet, pr, 0));
                }
            }
        }
    }

    while let Some((pn, pr, pdepth)) = pq.pop_front() {
        if pdepth >= 32 {
            continue;
        }
        let pseg = graph.net(pn);
        let ov_drvs: Vec<_> = pseg
            .drivers
            .iter()
            .filter(|d| d.dst_range.overlaps(pr))
            .collect();
        let mut saw_child_pin = false;
        let mut saw_local = false;
        for drv in &ov_drvs {
            match drv.origin {
                DriverOrigin::ChildPin(up_pid) => {
                    saw_child_pin = true;
                    let up_pin = graph.pin(up_pid);
                    let up_cell = up_pin.cell;
                    let Some(up_inet) = up_pin.internal_net else {
                        continue;
                    };
                    let ov = pr.intersect(drv.dst_range).unwrap_or(pr);
                    let up_r = drv
                        .src_range
                        .map(|sr| {
                            if drv.dst_range.width() == sr.width() && sr.width() > 0 {
                                project_range(ov, drv.dst_range, sr)
                            } else {
                                sr
                            }
                        })
                        .unwrap_or_else(|| BitRange::full(up_pin.width));
                    if let Some(nested) =
                        resolve_pure_wire_ft_inner(graph, up_cell, up_inet, up_r, depth + 1)
                    {
                        for item in nested {
                            if !origins.contains(&item) {
                                origins.push(item);
                            }
                        }
                    } else {
                        let item = (up_cell, up_inet, up_r, Some(up_pid));
                        if !origins.contains(&item) {
                            origins.push(item);
                        }
                    }
                }
                DriverOrigin::Assign | DriverOrigin::NetAlias
                    if matches!(drv.op, ExprOpSummary::Identity)
                        && drv.extra_deps.is_empty()
                        && drv.inline_clamp.is_none() =>
                {
                    if let (Some(sn), Some(sr)) = (drv.src_net, drv.src_range) {
                        if graph.net(sn).cell == parent_cell && sn != pn {
                            let ov = pr.intersect(drv.dst_range).unwrap_or(pr);
                            let next_r = if drv.dst_range.width() == sr.width() && sr.width() > 0 {
                                project_range(ov, drv.dst_range, sr)
                            } else {
                                sr
                            };
                            if visited_p.insert((sn, next_r)) {
                                pq.push_back((sn, next_r, pdepth + 1));
                            }
                            continue;
                        }
                    }
                    saw_local = true;
                }
                _ => {
                    saw_local = true;
                }
            }
        }
        if saw_local || (!saw_child_pin && ov_drvs.is_empty()) {
            let item = (parent_cell, pn, pr, None);
            if !origins.contains(&item) {
                origins.push(item);
            }
        }
    }

    (!origins.is_empty()).then_some(origins)
}
