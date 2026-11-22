//! Intra-cell feedthrough BFS classification and per-route exit-path tracking.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use super::feedthrough::FeedthroughKind;
use super::ft_alias::{
    is_buf_gate_edge, is_pure_wire_alias_load, is_same_domain_in_subtree, project_range,
};
use super::{BitRange, CellId, ExprOpSummary, NetSegId, NetSegKind, PinId, StructuralGraph};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClassifyRoute {
    pub kind: FeedthroughKind,
    pub downstream_cell: CellId,
    pub reaching_pins: Vec<(PinId, BitRange)>,
    pub reaching_nets: Vec<(NetSegId, BitRange)>,
}

pub(super) fn classify_feedthrough_in_cell(
    graph: &StructuralGraph,
    domain_of: &dyn Fn(CellId) -> Option<u32>,
    cell_id: CellId,
    start_net: NetSegId,
    start_range: BitRange,
    initial_inv: bool,
) -> (
    FeedthroughKind,
    Vec<PinId>,
    Vec<NetSegId>,
    Vec<CellId>,
    Vec<ClassifyRoute>,
    bool,
) {
    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    struct StateKey {
        net: NetSegId,
        lo: u32,
        hi: u32,
        inv: bool,
        kind: FeedthroughKind,
    }

    struct ExitTargetHit {
        dc: CellId,
        kind: FeedthroughKind,
        pin: Option<(PinId, BitRange)>,
        exit_net: (NetSegId, BitRange),
    }

    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    struct RouteKey {
        dc: CellId,
        pin: Option<(PinId, BitRange)>,
        exit_net: (NetSegId, BitRange),
    }

    let d_cell = domain_of(cell_id);
    let mut visited: HashSet<StateKey> = HashSet::new();
    let mut queue: VecDeque<(NetSegId, BitRange, FeedthroughKind, bool, usize)> = VecDeque::new();
    let mut exit_pins: BTreeSet<PinId> = BTreeSet::new();
    let mut exit_nets: Vec<(NetSegId, BitRange)> = Vec::new();
    let mut rev_edges: HashMap<(NetSegId, BitRange), Vec<(NetSegId, BitRange)>> = HashMap::new();
    let mut node_out_pins: HashMap<(NetSegId, BitRange), Vec<(PinId, BitRange)>> = HashMap::new();
    let mut downstream: BTreeSet<CellId> = BTreeSet::new();
    let mut exit_hits: Vec<ExitTargetHit> = Vec::new();
    let mut best_kind: Option<FeedthroughKind> = None;
    let mut any_inverted = false;
    let mut saw_seq = false;

    queue.push_back((
        start_net,
        start_range,
        FeedthroughKind::PureWire,
        initial_inv,
        0,
    ));
    visited.insert(StateKey {
        net: start_net,
        lo: start_range.lo,
        hi: start_range.hi,
        inv: initial_inv,
        kind: FeedthroughKind::PureWire,
    });

    while let Some((cur_net, cur_range, cur_kind, cur_inv, depth)) = queue.pop_front() {
        let seg = graph.net(cur_net);
        if !is_same_domain_in_subtree(graph, domain_of, cell_id, d_cell, seg.cell) {
            continue;
        }
        if seg.is_seq_state {
            saw_seq = true;
            continue;
        }

        if seg.cell == cell_id {
            for &pid in &graph.cell(cell_id).pins {
                let p = graph.pin(pid);
                if p.internal_net == Some(cur_net)
                    && matches!(
                        p.dir,
                        Some(svlang::PortDir::Output | svlang::PortDir::Inout)
                    )
                {
                    exit_pins.insert(pid);
                    if !exit_nets.contains(&(cur_net, cur_range)) {
                        exit_nets.push((cur_net, cur_range));
                    }
                    best_kind = Some(merge_ft_kind(best_kind, cur_kind));
                    any_inverted |= cur_inv;

                    for conn in &p.external_conns {
                        let Some(pin_ov) = cur_range.intersect(conn.pin_range) else {
                            continue;
                        };
                        let Some(pnet) = conn.parent_net else {
                            continue;
                        };
                        let Some(parent_id) = graph.cell(cell_id).parent else {
                            continue;
                        };
                        let target_pr = project_range(pin_ov, conn.pin_range, conn.parent_range);
                        let (conn_dcs, parent_consumes) = collect_parent_downstream(
                            graph, domain_of, cell_id, d_cell, parent_id, pnet, target_pr,
                        );
                        for dc in conn_dcs {
                            downstream.insert(dc);
                            exit_hits.push(ExitTargetHit {
                                dc,
                                kind: cur_kind,
                                pin: Some((pid, pin_ov)),
                                exit_net: (cur_net, cur_range),
                            });
                        }
                        if parent_consumes && domain_of(parent_id) != d_cell {
                            downstream.insert(parent_id);
                            exit_hits.push(ExitTargetHit {
                                dc: parent_id,
                                kind: cur_kind,
                                pin: Some((pid, pin_ov)),
                                exit_net: (cur_net, cur_range),
                            });
                        }
                    }
                }
            }
        } else {
            for &pid in &graph.cell(seg.cell).pins {
                let p = graph.pin(pid);
                if p.internal_net == Some(cur_net)
                    && matches!(
                        p.dir,
                        Some(svlang::PortDir::Output | svlang::PortDir::Inout)
                    )
                {
                    let pin_range = if p.width == seg.width {
                        cur_range
                    } else {
                        BitRange::full(p.width)
                    };
                    let list = node_out_pins.entry((cur_net, cur_range)).or_default();
                    if !list.contains(&(pid, pin_range)) {
                        list.push((pid, pin_range));
                    }
                }
            }
        }

        if depth >= 64 {
            continue;
        }

        for load in &seg.loads {
            let Some(overlap) = load.src_range.intersect(cur_range) else {
                continue;
            };
            if let Some(dp) = load.dst_pin {
                let dc = graph.pin(dp).cell;
                if dc != cell_id
                    && !is_same_domain_in_subtree(graph, domain_of, cell_id, d_cell, dc)
                    && domain_of(dc) != d_cell
                {
                    let pin_range =
                        if matches!(load.op, ExprOpSummary::Identity | ExprOpSummary::Invert)
                            && load.src_range.width() == load.dst_range.width()
                        {
                            project_range(overlap, load.src_range, load.dst_range)
                        } else {
                            load.dst_range
                        };
                    let Some((hop_kind, flip)) = classify_load_hop(graph, cur_net, load, pin_range)
                    else {
                        if matches!(load.op, ExprOpSummary::Seq) {
                            saw_seq = true;
                        }
                        continue;
                    };
                    let exit_kind = combine_ft_hop(cur_kind, hop_kind);
                    let exit_inv = cur_inv ^ flip;
                    best_kind = Some(merge_ft_kind(best_kind, exit_kind));
                    any_inverted |= exit_inv;
                    if !exit_nets.contains(&(cur_net, cur_range)) {
                        exit_nets.push((cur_net, cur_range));
                    }
                    downstream.insert(dc);
                    exit_hits.push(ExitTargetHit {
                        dc,
                        kind: exit_kind,
                        pin: Some((dp, pin_range)),
                        exit_net: (cur_net, cur_range),
                    });
                    continue;
                }
            }
            let Some(dn) = load.dst_net else {
                continue;
            };
            if !is_same_domain_in_subtree(graph, domain_of, cell_id, d_cell, graph.net(dn).cell) {
                continue;
            }
            if matches!(load.op, ExprOpSummary::Seq) || graph.net(dn).is_seq_state {
                saw_seq = true;
                continue;
            }
            let next_range = if matches!(load.op, ExprOpSummary::Identity | ExprOpSummary::Invert)
                && load.src_range.width() == load.dst_range.width()
            {
                project_range(overlap, load.src_range, load.dst_range)
            } else {
                load.dst_range
            };
            let Some((hop_kind, flip)) = classify_load_hop(graph, cur_net, load, next_range) else {
                continue;
            };
            let next_kind = combine_ft_hop(cur_kind, hop_kind);
            let next_inv = cur_inv ^ flip;
            let preds = rev_edges.entry((dn, next_range)).or_default();
            if !preds.contains(&(cur_net, cur_range)) {
                preds.push((cur_net, cur_range));
            }
            let key = StateKey {
                net: dn,
                lo: next_range.lo,
                hi: next_range.hi,
                inv: next_inv,
                kind: next_kind,
            };
            if visited.insert(key) {
                queue.push_back((dn, next_range, next_kind, next_inv, depth + 1));
            }
        }
    }

    let collect_back_nets = |seeds: &[(NetSegId, BitRange)]| -> Vec<(NetSegId, BitRange)> {
        let mut seen_nodes: BTreeSet<(NetSegId, BitRange)> = BTreeSet::new();
        let mut back_q: VecDeque<(NetSegId, BitRange)> = VecDeque::new();
        for &seed in seeds {
            if seen_nodes.insert(seed) {
                back_q.push_back(seed);
            }
        }
        while let Some(node) = back_q.pop_front() {
            if let Some(preds) = rev_edges.get(&node) {
                for &pn in preds {
                    if seen_nodes.insert(pn) {
                        back_q.push_back(pn);
                    }
                }
            }
        }
        seen_nodes.into_iter().collect()
    };

    let all_ft_net_ranges = collect_back_nets(&exit_nets);
    let mut ft_nets: BTreeSet<NetSegId> = BTreeSet::new();
    for node in &all_ft_net_ranges {
        ft_nets.insert(node.0);
        if let Some(pins) = node_out_pins.get(node) {
            for &(pid, _) in pins {
                exit_pins.insert(pid);
            }
        }
    }

    let mut by_route: BTreeMap<RouteKey, Option<FeedthroughKind>> = BTreeMap::new();
    for hit in exit_hits {
        let key = RouteKey {
            dc: hit.dc,
            pin: hit.pin,
            exit_net: hit.exit_net,
        };
        let entry = by_route.entry(key).or_default();
        *entry = Some(merge_ft_kind(*entry, hit.kind));
    }
    let routes: Vec<ClassifyRoute> = by_route
        .into_iter()
        .map(|(key, kind)| {
            let back_nets = collect_back_nets(&[key.exit_net]);
            let mut pins = Vec::new();
            if let Some(p) = key.pin {
                pins.push(p);
            }
            for node in &back_nets {
                if let Some(child_pins) = node_out_pins.get(node) {
                    for &cp in child_pins {
                        if !pins.contains(&cp) {
                            pins.push(cp);
                        }
                    }
                }
            }
            ClassifyRoute {
                kind: kind.unwrap_or(FeedthroughKind::PureWire),
                downstream_cell: key.dc,
                reaching_pins: pins,
                reaching_nets: back_nets,
            }
        })
        .collect();

    let final_kind = if let Some(k) = best_kind {
        k
    } else if saw_seq {
        FeedthroughKind::Sequential
    } else {
        FeedthroughKind::None
    };
    let final_inv = if best_kind.is_some() {
        any_inverted
    } else {
        initial_inv
    };
    (
        final_kind,
        exit_pins.into_iter().collect(),
        ft_nets.into_iter().collect(),
        downstream.into_iter().collect(),
        routes,
        final_inv,
    )
}

fn collect_parent_downstream(
    graph: &StructuralGraph,
    domain_of: &dyn Fn(CellId) -> Option<u32>,
    cell_id: CellId,
    d_cell: Option<u32>,
    parent_id: CellId,
    start_pnet: NetSegId,
    start_prange: BitRange,
) -> (Vec<CellId>, bool) {
    let mut visited: HashSet<(NetSegId, BitRange)> = HashSet::new();
    let mut queue: VecDeque<(NetSegId, BitRange, usize)> = VecDeque::new();
    let mut dcs: Vec<CellId> = Vec::new();
    let mut parent_consumes = false;

    visited.insert((start_pnet, start_prange));
    queue.push_back((start_pnet, start_prange, 0));

    while let Some((cur_pnet, cur_pr, depth)) = queue.pop_front() {
        let pseg = graph.net(cur_pnet);
        if matches!(
            pseg.kind,
            NetSegKind::Port(svlang::PortDir::Output | svlang::PortDir::Inout)
        ) {
            parent_consumes = true;
        }
        for l in &pseg.loads {
            let Some(lov) = l.src_range.intersect(cur_pr) else {
                continue;
            };
            if let Some(dp) = l.dst_pin {
                let dc = graph.pin(dp).cell;
                if dc == parent_id {
                    parent_consumes = true;
                } else if dc != cell_id && domain_of(dc) != d_cell && !dcs.contains(&dc) {
                    dcs.push(dc);
                }
            } else if let Some((dn, _)) = is_pure_wire_alias_load(graph, cur_pnet, l) {
                if depth < 32 {
                    let next_pr = project_range(lov, l.src_range, l.dst_range);
                    if visited.insert((dn, next_pr)) {
                        queue.push_back((dn, next_pr, depth + 1));
                    }
                } else {
                    parent_consumes = true;
                }
            } else {
                parent_consumes = true;
            }
        }
    }

    (dcs, parent_consumes)
}

fn classify_load_hop(
    graph: &StructuralGraph,
    cur_net: NetSegId,
    load: &super::LoadEdge,
    next_range: BitRange,
) -> Option<(FeedthroughKind, bool)> {
    match &load.op {
        ExprOpSummary::Identity => {
            if let Some(dn) = load.dst_net {
                if is_buf_gate_edge(graph, cur_net, dn, next_range) {
                    return Some((FeedthroughKind::Buffered, false));
                }
            }
            Some((FeedthroughKind::PureWire, false))
        }
        ExprOpSummary::Invert => Some((FeedthroughKind::Combinational, true)),
        ExprOpSummary::UnateGate { is_inverting } => {
            if *is_inverting {
                Some((FeedthroughKind::Combinational, true))
            } else {
                Some((FeedthroughKind::Buffered, false))
            }
        }
        ExprOpSummary::Mux { .. } | ExprOpSummary::Comb => {
            Some((FeedthroughKind::Combinational, false))
        }
        ExprOpSummary::Const { .. } | ExprOpSummary::Seq => None,
    }
}

fn combine_ft_hop(a: FeedthroughKind, b: FeedthroughKind) -> FeedthroughKind {
    match (a, b) {
        (FeedthroughKind::Combinational, _) | (_, FeedthroughKind::Combinational) => {
            FeedthroughKind::Combinational
        }
        (FeedthroughKind::Buffered, _) | (_, FeedthroughKind::Buffered) => {
            FeedthroughKind::Buffered
        }
        _ => FeedthroughKind::PureWire,
    }
}

fn merge_ft_kind(prev: Option<FeedthroughKind>, new_k: FeedthroughKind) -> FeedthroughKind {
    match prev {
        None => new_k,
        Some(p) => match (p, new_k) {
            (FeedthroughKind::Combinational, _) | (_, FeedthroughKind::Combinational) => {
                FeedthroughKind::Combinational
            }
            (FeedthroughKind::Buffered, _) | (_, FeedthroughKind::Buffered) => {
                FeedthroughKind::Buffered
            }
            _ => p,
        },
    }
}
