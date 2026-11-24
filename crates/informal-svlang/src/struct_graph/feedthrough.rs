//! Domain-crossing extraction and feedthrough classification
//! (pure wire vs buffered vs combinational vs sequential).

use std::collections::HashSet;

use super::ft_alias::{
    collect_dst_sub_intervals, collect_parent_alias_reach, net_has_local_non_childpin_driver,
    project_range, resolve_pure_wire_ft_upstream,
};
use super::ft_classify::{classify_feedthrough_in_cell, ClassifyRoute};
use super::{
    BitRange, CellId, EdgeId, ExprOpSummary, InlineClampInfo, NetSegId, PinId, StructuralGraph,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FeedthroughKind {
    None,
    PureWire,
    Buffered,
    Combinational,
    Sequential,
}

impl FeedthroughKind {
    pub fn is_comb_feedthrough(self) -> bool {
        matches!(self, Self::PureWire | Self::Buffered | Self::Combinational)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedthroughRoute {
    pub src_range: BitRange,
    pub dst_range: BitRange,
    pub kind: FeedthroughKind,
    pub downstream_cell: CellId,
    pub reaching_pins: Vec<(PinId, BitRange)>,
    pub reaching_nets: Vec<(NetSegId, BitRange)>,
}

#[derive(Debug, Clone)]
pub struct CrossingEdge {
    pub id: EdgeId,
    pub src_net: NetSegId,
    pub src_range: BitRange,
    pub dst_net: NetSegId,
    pub dst_range: BitRange,
    pub src_cell: CellId,
    pub dst_cell: CellId,
    pub boundary_pin: Option<PinId>,
    /// Intermediate cell if signal passed purely through a parent wrapper between siblings.
    pub mid_cell: Option<CellId>,
    pub mid_nets: Vec<(NetSegId, BitRange)>,
    pub feedthrough_pins: Vec<PinId>,
    pub feedthrough_nets: Vec<NetSegId>,
    pub feedthrough_downstream_cells: Vec<CellId>,
    pub feedthrough_routes: Vec<FeedthroughRoute>,
    pub feedthrough: FeedthroughKind,
    pub inverted: bool,
    pub inline_clamps: Vec<InlineClampInfo>,
    pub loc: svlang::Loc,
}

/// Extracts all domain-crossing edges across the hierarchical [`StructuralGraph`]
/// using `domain_of(cell_id) -> Option<u32>`.
pub fn extract_crossings(
    graph: &StructuralGraph,
    domain_of: &dyn Fn(CellId) -> Option<u32>,
) -> Vec<CrossingEdge> {
    let mut edges = Vec::new();
    let mut seen_keys: HashSet<(NetSegId, BitRange, NetSegId, BitRange, CellId, CellId)> =
        HashSet::new();

    for pin in &graph.pins {
        let child_cell = pin.cell;
        let Some(parent_cell) = graph.cell(child_cell).parent else {
            continue;
        };
        let d_child = domain_of(child_cell);
        let d_parent = domain_of(parent_cell);
        let Some(internal_net) = pin.internal_net else {
            continue;
        };

        match pin.dir {
            Some(svlang::PortDir::Output) => {
                // Child output drives parent bindings -> trace to local parent loads and/or sibling child pins.
                for conn in &pin.external_conns {
                    let Some(pnet) = conn.parent_net else {
                        continue;
                    };
                    let mut inline_clamps = Vec::new();
                    if let Some(ic) = conn.inline_clamp {
                        inline_clamps.push(ic);
                    }
                    let sources = resolve_pure_wire_ft_upstream(
                        graph,
                        child_cell,
                        internal_net,
                        conn.pin_range,
                    )
                    .unwrap_or_else(|| {
                        vec![(child_cell, internal_net, conn.pin_range, Some(pin.id))]
                    });
                    for (src_c, src_n, src_r, src_bpin) in sources {
                        collect_downstream_from_parent_net(
                            graph,
                            domain_of,
                            src_c,
                            domain_of(src_c),
                            src_n,
                            src_r,
                            src_bpin,
                            parent_cell,
                            d_parent,
                            pnet,
                            conn.parent_range,
                            conn.loc,
                            &inline_clamps,
                            &mut seen_keys,
                            &mut edges,
                        );
                    }
                }
            }
            Some(svlang::PortDir::Input) => {
                // Parent drives child input pin: only emit here if parent net has non-ChildPin drivers
                // (sibling ChildPin -> ChildPin crossings are handled in Output branch above so we avoid duplicates
                // and preserve exact `u_src -> u_dst` source net attribution).
                if d_parent == d_child {
                    continue;
                }
                for conn in &pin.external_conns {
                    if let Some(pnet) = conn.parent_net {
                        let pseg = graph.net(pnet);
                        let has_local_driver =
                            net_has_local_non_childpin_driver(graph, pnet, conn.parent_range);
                        // If `pseg` has drivers from `expr_deps` (e.g., `.in_sig(!iso_en & raw)`), collect clamps too.
                        let mut inline_clamps =
                            collect_clamps_on_net(graph, pnet, conn.parent_range);
                        if let Some(ic) = conn.inline_clamp {
                            inline_clamps.push(ic);
                        }
                        let init_inv = op_is_inverting(&conn.expr_op);
                        if has_local_driver
                            || (pseg.drivers.is_empty()
                                && !has_blackbox_driver(graph, pnet, conn.parent_range))
                        {
                            push_crossing(
                                graph,
                                domain_of,
                                &mut seen_keys,
                                &mut edges,
                                pnet,
                                conn.parent_range,
                                internal_net,
                                conn.pin_range,
                                parent_cell,
                                child_cell,
                                Some(pin.id),
                                None,
                                Vec::new(),
                                inline_clamps,
                                conn.loc,
                                init_inv,
                            );
                        } else if !conn.expr_deps.is_empty() && conn.inline_clamp.is_some() {
                            // Connection itself has inline clamp expression on a ChildPin-driven net.
                            push_crossing(
                                graph,
                                domain_of,
                                &mut seen_keys,
                                &mut edges,
                                pnet,
                                conn.parent_range,
                                internal_net,
                                conn.pin_range,
                                parent_cell,
                                child_cell,
                                Some(pin.id),
                                None,
                                Vec::new(),
                                inline_clamps,
                                conn.loc,
                                init_inv,
                            );
                        }
                    } else if !conn.expr_deps.is_empty() {
                        for (idx, &(dep_net, dep_range)) in conn.expr_deps.iter().enumerate() {
                            let dseg = graph.net(dep_net);
                            let has_local_driver =
                                net_has_local_non_childpin_driver(graph, dep_net, dep_range);
                            if !(has_local_driver
                                || (dseg.drivers.is_empty()
                                    && !has_blackbox_driver(graph, dep_net, dep_range)))
                            {
                                continue;
                            }
                            let mut inline_clamps =
                                collect_clamps_on_net(graph, dep_net, dep_range);
                            if let Some(ic) = conn.inline_clamp {
                                inline_clamps.push(ic);
                            }
                            let op = conn.dep_ops.get(idx).unwrap_or(&conn.expr_op);
                            let init_inv = op_is_inverting(op);
                            let dst_sub = conn.pin_range;
                            push_crossing(
                                graph,
                                domain_of,
                                &mut seen_keys,
                                &mut edges,
                                dep_net,
                                dep_range,
                                internal_net,
                                dst_sub,
                                parent_cell,
                                child_cell,
                                Some(pin.id),
                                None,
                                Vec::new(),
                                inline_clamps,
                                conn.loc,
                                init_inv,
                            );
                        }
                    }
                }
            }
            Some(svlang::PortDir::Inout) => {
                for conn in &pin.external_conns {
                    let Some(pnet) = conn.parent_net else {
                        continue;
                    };
                    if d_parent != d_child {
                        let pseg = graph.net(pnet);
                        let has_local_driver =
                            net_has_local_non_childpin_driver(graph, pnet, conn.parent_range);
                        if has_local_driver
                            || (pseg.drivers.is_empty()
                                && !has_blackbox_driver(graph, pnet, conn.parent_range))
                        {
                            let mut inline_clamps =
                                collect_clamps_on_net(graph, pnet, conn.parent_range);
                            if let Some(ic) = conn.inline_clamp {
                                inline_clamps.push(ic);
                            }
                            push_crossing(
                                graph,
                                domain_of,
                                &mut seen_keys,
                                &mut edges,
                                pnet,
                                conn.parent_range,
                                internal_net,
                                conn.pin_range,
                                parent_cell,
                                child_cell,
                                Some(pin.id),
                                None,
                                Vec::new(),
                                inline_clamps,
                                conn.loc,
                                false,
                            );
                        }
                    }
                    let mut inline_clamps = Vec::new();
                    if let Some(ic) = conn.inline_clamp {
                        inline_clamps.push(ic);
                    }
                    collect_downstream_from_parent_net(
                        graph,
                        domain_of,
                        child_cell,
                        d_child,
                        internal_net,
                        conn.pin_range,
                        Some(pin.id),
                        parent_cell,
                        d_parent,
                        pnet,
                        conn.parent_range,
                        conn.loc,
                        &inline_clamps,
                        &mut seen_keys,
                        &mut edges,
                    );
                }
            }
            None => {
                // Blackbox stub pin (undirected): infer direction from whether parent net already has a driver.
                for conn in &pin.external_conns {
                    if let Some(pnet) = conn.parent_net {
                        let pseg = graph.net(pnet);
                        let has_any_driver = pseg
                            .drivers
                            .iter()
                            .any(|drv| drv.dst_range.overlaps(conn.parent_range));
                        let has_local_driver =
                            net_has_local_non_childpin_driver(graph, pnet, conn.parent_range);
                        if has_any_driver {
                            if d_parent != d_child && has_local_driver {
                                let mut inline_clamps =
                                    collect_clamps_on_net(graph, pnet, conn.parent_range);
                                if let Some(ic) = conn.inline_clamp {
                                    inline_clamps.push(ic);
                                }
                                push_crossing(
                                    graph,
                                    domain_of,
                                    &mut seen_keys,
                                    &mut edges,
                                    pnet,
                                    conn.parent_range,
                                    internal_net,
                                    conn.pin_range,
                                    parent_cell,
                                    child_cell,
                                    Some(pin.id),
                                    None,
                                    Vec::new(),
                                    inline_clamps,
                                    conn.loc,
                                    false,
                                );
                            }
                        } else {
                            let mut inline_clamps = Vec::new();
                            if let Some(ic) = conn.inline_clamp {
                                inline_clamps.push(ic);
                            }
                            collect_downstream_from_parent_net(
                                graph,
                                domain_of,
                                child_cell,
                                d_child,
                                internal_net,
                                conn.pin_range,
                                Some(pin.id),
                                parent_cell,
                                d_parent,
                                pnet,
                                conn.parent_range,
                                conn.loc,
                                &inline_clamps,
                                &mut seen_keys,
                                &mut edges,
                            );
                        }
                    } else if d_parent != d_child && !conn.expr_deps.is_empty() {
                        for (idx, &(dep_net, dep_range)) in conn.expr_deps.iter().enumerate() {
                            let dseg = graph.net(dep_net);
                            let has_local_driver =
                                net_has_local_non_childpin_driver(graph, dep_net, dep_range);
                            if !(has_local_driver
                                || (dseg.drivers.is_empty()
                                    && !has_blackbox_driver(graph, dep_net, dep_range)))
                            {
                                continue;
                            }
                            let mut inline_clamps =
                                collect_clamps_on_net(graph, dep_net, dep_range);
                            if let Some(ic) = conn.inline_clamp {
                                inline_clamps.push(ic);
                            }
                            let op = conn.dep_ops.get(idx).unwrap_or(&conn.expr_op);
                            push_crossing(
                                graph,
                                domain_of,
                                &mut seen_keys,
                                &mut edges,
                                dep_net,
                                dep_range,
                                internal_net,
                                conn.pin_range,
                                parent_cell,
                                child_cell,
                                Some(pin.id),
                                None,
                                Vec::new(),
                                inline_clamps,
                                conn.loc,
                                op_is_inverting(op),
                            );
                        }
                    }
                }
            }
            Some(svlang::PortDir::Ref) => {}
        }
    }

    edges
}

fn op_is_inverting(op: &ExprOpSummary) -> bool {
    matches!(
        op,
        ExprOpSummary::Invert | ExprOpSummary::UnateGate { is_inverting: true }
    )
}

fn has_blackbox_driver(graph: &StructuralGraph, pnet: NetSegId, range: BitRange) -> bool {
    let pseg = graph.net(pnet);
    let has_drv = pseg.drivers.iter().any(|d| d.dst_range.overlaps(range));
    if has_drv {
        return false;
    }
    pseg.loads.iter().any(|l| {
        l.src_range.overlaps(range) && l.dst_pin.is_some_and(|pid| graph.pin(pid).dir.is_none())
    })
}

#[allow(clippy::too_many_arguments)]
fn collect_downstream_from_parent_net(
    graph: &StructuralGraph,
    domain_of: &dyn Fn(CellId) -> Option<u32>,
    src_cell: CellId,
    d_src: Option<u32>,
    src_net: NetSegId,
    src_range: BitRange,
    boundary_pin: Option<PinId>,
    parent_cell: CellId,
    d_parent: Option<u32>,
    pnet: NetSegId,
    prange: BitRange,
    loc: svlang::Loc,
    inline_clamps: &[InlineClampInfo],
    seen_keys: &mut HashSet<(NetSegId, BitRange, NetSegId, BitRange, CellId, CellId)>,
    edges: &mut Vec<CrossingEdge>,
) {
    let reach = collect_parent_alias_reach(
        graph,
        src_cell,
        parent_cell,
        pnet,
        prange,
        src_range,
        &op_is_inverting,
    );

    // Collect any 1-hop inline clamp gates in `parent_cell` driven by `mid_nets`.
    let mut parent_clamps: Vec<InlineClampInfo> = Vec::new();
    for &(mn, mr) in &reach.mid_nets {
        for load in &graph.net(mn).loads {
            if !load.src_range.overlaps(mr) || load.dst_pin.is_some() {
                continue;
            }
            if let Some(dn) = load.dst_net {
                let dseg = graph.net(dn);
                if dseg.cell == parent_cell {
                    for drv in &dseg.drivers {
                        if let Some(ic) = drv.inline_clamp {
                            if ic.data_net == mn
                                && ic.data_range.overlaps(mr)
                                && !parent_clamps.contains(&ic)
                            {
                                parent_clamps.push(ic);
                            }
                        }
                    }
                }
            }
        }
    }

    // 1. If `parent_cell` itself consumes `pnet` locally and has a different domain, emit `src_cell -> parent_cell`.
    if d_src != d_parent {
        for (dst_pnet, src_sub, overlap_p, boundary_inv) in reach.parent_loads {
            let mut clamps = inline_clamps.to_vec();
            for &ic in &parent_clamps {
                if (ic.data_net != dst_pnet || ic.data_range.overlaps(overlap_p))
                    && !clamps.contains(&ic)
                {
                    clamps.push(ic);
                }
            }
            push_crossing(
                graph,
                domain_of,
                seen_keys,
                edges,
                src_net,
                src_sub,
                dst_pnet,
                overlap_p,
                src_cell,
                parent_cell,
                boundary_pin,
                None,
                Vec::new(),
                clamps,
                loc,
                boundary_inv,
            );
        }
    }

    // 2. For direct/aliased sibling pins fed by `pnet`, emit `src_cell -> dst_child`.
    for s in reach.sibling_loads {
        let dst_cell = graph.pin(s.dpin_id).cell;
        let d_dst = domain_of(dst_cell);
        if d_dst == d_src && d_parent == d_src {
            continue;
        }
        let mid = (d_parent != d_src && d_parent != d_dst).then_some(parent_cell);
        if d_dst == d_src && mid.is_none() {
            continue;
        }
        let mut clamps = inline_clamps.to_vec();
        for &ic in &parent_clamps {
            if ic.data_range.overlaps(s.overlap_p) && !clamps.contains(&ic) {
                clamps.push(ic);
            }
        }
        if let Some(c) = s.conn_clamp {
            if !clamps.contains(&c) {
                clamps.push(c);
            }
        }
        let mid_nets = if mid.is_some() {
            s.mid_nets
        } else {
            Vec::new()
        };
        push_crossing(
            graph,
            domain_of,
            seen_keys,
            edges,
            src_net,
            s.src_sub,
            s.dnet,
            s.drange,
            src_cell,
            dst_cell,
            boundary_pin,
            mid,
            mid_nets,
            clamps,
            loc,
            s.boundary_inv,
        );
    }
}

fn collect_clamps_on_net(
    graph: &StructuralGraph,
    net: NetSegId,
    range: BitRange,
) -> Vec<InlineClampInfo> {
    let mut out = Vec::new();
    for drv in &graph.net(net).drivers {
        if drv.dst_range.overlaps(range) {
            if let Some(ic) = drv.inline_clamp {
                out.push(ic);
            }
        }
    }
    out
}

fn build_feedthrough_routes(
    src_range: BitRange,
    dst_range: BitRange,
    routes: Vec<ClassifyRoute>,
) -> Vec<FeedthroughRoute> {
    routes
        .into_iter()
        .map(|r| FeedthroughRoute {
            src_range,
            dst_range,
            kind: r.kind,
            downstream_cell: r.downstream_cell,
            reaching_pins: r.reaching_pins,
            reaching_nets: r.reaching_nets,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn push_crossing(
    graph: &StructuralGraph,
    domain_of: &dyn Fn(CellId) -> Option<u32>,
    seen_keys: &mut HashSet<(NetSegId, BitRange, NetSegId, BitRange, CellId, CellId)>,
    edges: &mut Vec<CrossingEdge>,
    src_net: NetSegId,
    src_range: BitRange,
    dst_net: NetSegId,
    dst_range: BitRange,
    src_cell: CellId,
    dst_cell: CellId,
    boundary_pin: Option<PinId>,
    mid_cell: Option<CellId>,
    mid_nets: Vec<(NetSegId, BitRange)>,
    inline_clamps: Vec<InlineClampInfo>,
    loc: svlang::Loc,
    initial_inv: bool,
) {
    let sub_intervals = collect_dst_sub_intervals(graph, domain_of, dst_cell, dst_net, dst_range);
    if sub_intervals.len() > 1 {
        #[derive(Clone)]
        struct SubRes {
            range: BitRange,
            ft_kind: FeedthroughKind,
            ft_pins: Vec<PinId>,
            ft_nets: Vec<NetSegId>,
            ft_downstream: Vec<CellId>,
            ft_routes: Vec<ClassifyRoute>,
            inverted: bool,
        }
        let mut results: Vec<SubRes> = Vec::with_capacity(sub_intervals.len());
        for sr in &sub_intervals {
            let (fk, fp, fnets, fd, froutes, inv) =
                classify_feedthrough_in_cell(graph, domain_of, dst_cell, dst_net, *sr, initial_inv);
            if let Some(last) = results.last_mut() {
                if last.ft_kind == fk
                    && last.ft_pins == fp
                    && last.ft_nets == fnets
                    && last.ft_downstream == fd
                    && last.ft_routes == froutes
                    && last.inverted == inv
                    && last.range.hi + 1 == sr.lo
                {
                    last.range.hi = sr.hi;
                    continue;
                }
            }
            results.push(SubRes {
                range: *sr,
                ft_kind: fk,
                ft_pins: fp,
                ft_nets: fnets,
                ft_downstream: fd,
                ft_routes: froutes,
                inverted: inv,
            });
        }
        let inv_diff = results.iter().any(|r| r.inverted != initial_inv)
            && results.iter().any(|r| r.inverted == initial_inv);
        let active: Vec<&SubRes> = results
            .iter()
            .filter(|r| r.ft_kind != FeedthroughKind::None)
            .collect();
        let distinct_active_diff = active.windows(2).any(|w| {
            w[0].ft_kind != w[1].ft_kind
                || w[0].ft_pins != w[1].ft_pins
                || w[0].ft_nets != w[1].ft_nets
                || w[0].ft_downstream != w[1].ft_downstream
                || w[0].ft_routes != w[1].ft_routes
                || w[0].inverted != w[1].inverted
        });
        let split = results.len() > 1 && (inv_diff || distinct_active_diff);
        if split {
            for r in results {
                let src_sub = project_range(r.range, dst_range, src_range);
                let emit_fk = if mid_cell.is_some() && r.ft_kind == FeedthroughKind::None {
                    FeedthroughKind::PureWire
                } else {
                    r.ft_kind
                };
                let routes = build_feedthrough_routes(src_sub, r.range, r.ft_routes);
                let sub_mids: Vec<(NetSegId, BitRange)> = mid_nets
                    .iter()
                    .map(|&(mn, mr)| {
                        let narrowed = if mr.width() == dst_range.width() {
                            project_range(r.range, dst_range, mr)
                        } else {
                            mr
                        };
                        (mn, narrowed)
                    })
                    .collect();
                emit_crossing_edge(
                    seen_keys,
                    edges,
                    src_net,
                    src_sub,
                    dst_net,
                    r.range,
                    src_cell,
                    dst_cell,
                    boundary_pin,
                    mid_cell,
                    sub_mids,
                    r.ft_pins,
                    r.ft_nets,
                    r.ft_downstream,
                    routes,
                    emit_fk,
                    r.inverted,
                    inline_clamps.clone(),
                    loc,
                );
            }
            return;
        }

        let mut agg_kind = FeedthroughKind::None;
        let mut agg_pins = Vec::new();
        let mut agg_nets = Vec::new();
        let mut agg_downstream = Vec::new();
        let mut agg_routes = Vec::new();
        let mut agg_inv = initial_inv;
        let mut saw_active = false;
        for r in results {
            if r.ft_kind != FeedthroughKind::None {
                if !saw_active {
                    agg_kind = r.ft_kind;
                    agg_inv = r.inverted;
                    saw_active = true;
                }
                for p in r.ft_pins {
                    if !agg_pins.contains(&p) {
                        agg_pins.push(p);
                    }
                }
                for n in r.ft_nets {
                    if !agg_nets.contains(&n) {
                        agg_nets.push(n);
                    }
                }
                for d in r.ft_downstream {
                    if !agg_downstream.contains(&d) {
                        agg_downstream.push(d);
                    }
                }
                let src_sub = project_range(r.range, dst_range, src_range);
                agg_routes.extend(build_feedthrough_routes(src_sub, r.range, r.ft_routes));
            }
        }
        let kind = if mid_cell.is_some() && agg_kind == FeedthroughKind::None {
            FeedthroughKind::PureWire
        } else {
            agg_kind
        };
        emit_crossing_edge(
            seen_keys,
            edges,
            src_net,
            src_range,
            dst_net,
            dst_range,
            src_cell,
            dst_cell,
            boundary_pin,
            mid_cell,
            mid_nets,
            agg_pins,
            agg_nets,
            agg_downstream,
            agg_routes,
            kind,
            agg_inv,
            inline_clamps,
            loc,
        );
        return;
    }

    let (ft_kind, ft_pins, ft_nets, ft_downstream, ft_routes, inverted) =
        classify_feedthrough_in_cell(graph, domain_of, dst_cell, dst_net, dst_range, initial_inv);
    let kind = if mid_cell.is_some() && ft_kind == FeedthroughKind::None {
        FeedthroughKind::PureWire
    } else {
        ft_kind
    };
    let routes = build_feedthrough_routes(src_range, dst_range, ft_routes);
    emit_crossing_edge(
        seen_keys,
        edges,
        src_net,
        src_range,
        dst_net,
        dst_range,
        src_cell,
        dst_cell,
        boundary_pin,
        mid_cell,
        mid_nets,
        ft_pins,
        ft_nets,
        ft_downstream,
        routes,
        kind,
        inverted,
        inline_clamps,
        loc,
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_crossing_edge(
    seen_keys: &mut HashSet<(NetSegId, BitRange, NetSegId, BitRange, CellId, CellId)>,
    edges: &mut Vec<CrossingEdge>,
    src_net: NetSegId,
    src_range: BitRange,
    dst_net: NetSegId,
    dst_range: BitRange,
    src_cell: CellId,
    dst_cell: CellId,
    boundary_pin: Option<PinId>,
    mid_cell: Option<CellId>,
    mid_nets: Vec<(NetSegId, BitRange)>,
    ft_pins: Vec<PinId>,
    ft_nets: Vec<NetSegId>,
    ft_downstream: Vec<CellId>,
    ft_routes: Vec<FeedthroughRoute>,
    feedthrough: FeedthroughKind,
    inverted: bool,
    inline_clamps: Vec<InlineClampInfo>,
    loc: svlang::Loc,
) {
    let key = (src_net, src_range, dst_net, dst_range, src_cell, dst_cell);
    if !seen_keys.insert(key) {
        return;
    }
    let id = EdgeId(edges.len() as u32);
    edges.push(CrossingEdge {
        id,
        src_net,
        src_range,
        dst_net,
        dst_range,
        src_cell,
        dst_cell,
        boundary_pin,
        mid_cell,
        mid_nets,
        feedthrough_pins: ft_pins,
        feedthrough_nets: ft_nets,
        feedthrough_downstream_cells: ft_downstream,
        feedthrough_routes: ft_routes,
        feedthrough,
        inverted,
        inline_clamps,
        loc,
    });
}
