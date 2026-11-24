//! Stage 3 Isolation, AON Control Cone, and Cross-Domain Cell Checks:
//! `ISO_CELL_MISSING`, `ISO_CELL_MISMATCH`, `ISO_LOGIC_BEFORE_CELL`,
//! `LS_CELL_MISSING`, `AON_NON_AON_BUFFER`.

use std::collections::{HashSet, VecDeque};

use informal_liberty::{IsoClampKind, LibertyCell, LibertyDb, PinDir};
use informal_svlang::struct_graph::{CellId, DriverOrigin, NetSegId, PinId, StructuralGraph};
use informal_upf::{ClampValue, PowerDomainId, PstSat, SupplySetId};

use super::ls_ret::{check_ls_cell, check_ret_cell_pins, check_retention_mapping};
use super::pg_ctx::{needs_level_shifter, pin_parent_nets, push_violation, GatePgCtx};
use crate::lp::LpViolation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignalEndpoint {
    LeafPin { cell: CellId, pin: PinId },
    TopPort { cell: CellId, pin: PinId },
}

/// Traces backward from `start` through pure wires, `assign`s, and hierarchical
/// module ports, stopping immediately at the first Liberty leaf output pin or
/// top-level input port (`max_hops` bounds traversal to O(1) on 100k chains).
pub fn resolve_driving_sources(
    graph: &StructuralGraph,
    liberty: &LibertyDb,
    start: NetSegId,
    max_hops: usize,
) -> Vec<SignalEndpoint> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back((start, 0usize));
    visited.insert(start);

    while let Some((nid, d)) = queue.pop_front() {
        if d > max_hops {
            continue;
        }
        let net = graph.net(nid);
        for drv in &net.drivers {
            match &drv.origin {
                DriverOrigin::PrimaryInput(pid) => {
                    let ep = SignalEndpoint::TopPort {
                        cell: graph.pin(*pid).cell,
                        pin: *pid,
                    };
                    if !out.contains(&ep) {
                        out.push(ep);
                    }
                }
                DriverOrigin::ChildPin(pid) => {
                    let pin = graph.pin(*pid);
                    let child = graph.cell(pin.cell);
                    if liberty.contains_cell(&child.module) {
                        let ep = SignalEndpoint::LeafPin {
                            cell: pin.cell,
                            pin: *pid,
                        };
                        if !out.contains(&ep) {
                            out.push(ep);
                        }
                    } else if let Some(src) = drv.src_net {
                        if visited.insert(src) {
                            queue.push_back((src, d + 1));
                        }
                    }
                }
                DriverOrigin::Assign | DriverOrigin::ParentPort(_) | DriverOrigin::NetAlias => {
                    if let Some(src) = drv.src_net {
                        if visited.insert(src) {
                            queue.push_back((src, d + 1));
                        }
                    }
                    for &(dep, _) in &drv.extra_deps {
                        if visited.insert(dep) {
                            queue.push_back((dep, d + 1));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Traces forward from `start` through pure wires, `assign`s, and hierarchical
/// module ports, stopping immediately at the first Liberty leaf input pin or
/// top-level output port.
pub fn resolve_sink_loads(
    graph: &StructuralGraph,
    liberty: &LibertyDb,
    start: NetSegId,
    max_hops: usize,
) -> Vec<SignalEndpoint> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back((start, 0usize));
    visited.insert(start);

    while let Some((nid, d)) = queue.pop_front() {
        if d > max_hops {
            continue;
        }
        let net = graph.net(nid);
        let owner = graph.cell(net.cell);
        if owner.is_top {
            if let Some(&pid) = owner.pins.iter().find(|&&pid| {
                let p = graph.pin(pid);
                p.internal_net == Some(nid)
                    && matches!(p.dir, Some(informal_svlang::PortDir::Output))
            }) {
                let ep = SignalEndpoint::TopPort {
                    cell: net.cell,
                    pin: pid,
                };
                if !out.contains(&ep) {
                    out.push(ep);
                }
            }
        }
        for ld in &net.loads {
            if let Some(pid) = ld.dst_pin {
                let pin = graph.pin(pid);
                let dst_cell = graph.cell(pin.cell);
                if liberty.contains_cell(&dst_cell.module) {
                    let ep = SignalEndpoint::LeafPin {
                        cell: pin.cell,
                        pin: pid,
                    };
                    if !out.contains(&ep) {
                        out.push(ep);
                    }
                    continue;
                }
            }
            if let Some(dn) = ld.dst_net {
                if visited.insert(dn) {
                    queue.push_back((dn, d + 1));
                }
            }
        }
    }
    out
}

/// Runs Stage 3 ISO / LS / AON / Retention checks across the gate netlist.
pub fn run_stage3_cell_checks(
    ctx: &mut GatePgCtx<'_>,
    sat: &mut PstSat,
    tol_uv: i64,
    out: &mut Vec<LpViolation>,
) {
    let mut stray_before_iso: HashSet<CellId> = HashSet::new();
    let mut emitted_aon: HashSet<CellId> = HashSet::new();
    let mut emitted_iso_miss: HashSet<(CellId, PinId)> = HashSet::new();
    let mut emitted_ls_miss: HashSet<(CellId, PinId)> = HashSet::new();

    // Pass 1: Special cells (ISO, LS, Retention, Power Switch) & backward control cones.
    let cell_count = ctx.graph.cells.len();
    for raw_id in 0..cell_count {
        let cid = CellId(raw_id as u32);
        let module = ctx.graph.cell(cid).module.clone();
        let Some(lib_c) = ctx.liberty.cell(&module) else {
            continue;
        };

        if lib_c.is_isolation_cell {
            check_iso_cell(
                ctx,
                sat,
                cid,
                lib_c,
                &mut stray_before_iso,
                &mut emitted_aon,
                out,
            );
        }
        if lib_c.is_level_shifter {
            check_ls_cell(ctx, sat, cid, lib_c, tol_uv, out);
        }
        if lib_c.is_retention() {
            check_ret_cell_pins(ctx, cid, lib_c, &mut emitted_aon, out);
        }
        if lib_c.is_power_switch() {
            check_psw_ctrl_cone(ctx, cid, lib_c, &mut emitted_aon, out);
        }
    }

    // Pass 2: Forward fanout of UPF control signals for `AON_NON_AON_BUFFER`.
    check_upf_ctrl_forward_aon(ctx, &mut emitted_aon, out);

    // Pass 3: Retention strategy element mapping (`RET_CELL_UNMAPPED`).
    check_retention_mapping(ctx, out);

    // Pass 4: Regular cell input crossings (`ISO_CELL_MISSING`, `LS_CELL_MISSING`).
    for raw_id in 0..cell_count {
        let cid = CellId(raw_id as u32);
        if stray_before_iso.contains(&cid) || emitted_aon.contains(&cid) {
            continue;
        }
        let module = ctx.graph.cell(cid).module.clone();
        let Some(lib_c) = ctx.liberty.cell(&module) else {
            continue;
        };
        if lib_c.is_isolation_cell
            || lib_c.is_level_shifter
            || lib_c.is_macro_cell
            || lib_c.is_power_switch()
            || lib_c.is_always_on
        {
            continue;
        }
        let dst_pd = ctx.cell_domains[raw_id];
        let pin_ids = ctx.graph.cell(cid).pins.clone();
        for pid in pin_ids {
            let pname = ctx.graph.pin(pid).name.clone();
            let Some(sp) = lib_c.pin(&pname) else {
                continue;
            };
            if sp.direction != PinDir::Input || sp.is_clock {
                continue;
            }
            let Some(dst_ss) = ctx.effective_pin_ss(cid, &pname) else {
                continue;
            };
            for pnet in pin_parent_nets(ctx.graph, pid) {
                let drivers = resolve_driving_sources(ctx.graph, ctx.liberty, pnet, 6);
                for drv in drivers {
                    let (src_ss, src_pd, src_is_ls, src_is_macro) = match drv {
                        SignalEndpoint::LeafPin {
                            cell: sc,
                            pin: spid,
                        } => {
                            let smod = ctx.graph.cell(sc).module.clone();
                            let slib = ctx.liberty.cell(&smod);
                            let is_ls = slib.is_some_and(|l| l.is_level_shifter);
                            let is_macro = slib.is_some_and(|l| l.is_macro_cell);
                            if slib.is_some_and(|l| l.is_power_switch()) {
                                continue;
                            }
                            let spn = ctx.graph.pin(spid).name.clone();
                            (
                                ctx.effective_pin_ss(sc, &spn),
                                ctx.cell_domains[sc.0 as usize],
                                is_ls,
                                is_macro,
                            )
                        }
                        SignalEndpoint::TopPort { cell: sc, .. } => (
                            ctx.domain_primary_ss(ctx.cell_domains[sc.0 as usize]),
                            ctx.cell_domains[sc.0 as usize],
                            false,
                            false,
                        ),
                    };
                    if src_is_macro {
                        continue;
                    }
                    let Some(src_ss) = src_ss else {
                        continue;
                    };
                    if src_ss == dst_ss {
                        continue;
                    }
                    if sat.can_be_off_while_on(src_ss, dst_ss).is_some()
                        && emitted_iso_miss.insert((cid, pid))
                    {
                        let cell_upf = ctx.graph.cell(cid).upf_path.clone();
                        let rtl_loc = ctx.graph.format_loc(ctx.graph.cell(cid).loc);
                        push_violation(
                            out,
                            "ISO_CELL_MISSING",
                            "netlist",
                            ctx.domain_name(src_pd),
                            ctx.domain_name(dst_pd),
                            format!("{cell_upf}/{pname}"),
                            format!(
                                "switchable signal reaches `{cell_upf}/{pname}` ({module}) without an isolation cell"
                            ),
                            rtl_loc,
                        );
                    }
                    if !src_is_ls
                        && needs_level_shifter(sat, src_ss, dst_ss, tol_uv)
                        && emitted_ls_miss.insert((cid, pid))
                    {
                        let cell_upf = ctx.graph.cell(cid).upf_path.clone();
                        let rtl_loc = ctx.graph.format_loc(ctx.graph.cell(cid).loc);
                        push_violation(
                            out,
                            "LS_CELL_MISSING",
                            "netlist",
                            ctx.domain_name(src_pd),
                            ctx.domain_name(dst_pd),
                            format!("{cell_upf}/{pname}"),
                            format!(
                                "cross-voltage signal reaches `{cell_upf}/{pname}` ({module}) without a level-shifter cell"
                            ),
                            rtl_loc,
                        );
                    }
                }
            }
        }
    }
}

fn check_iso_cell(
    ctx: &mut GatePgCtx<'_>,
    sat: &mut PstSat,
    cid: CellId,
    lib_c: &LibertyCell,
    stray_before_iso: &mut HashSet<CellId>,
    emitted_aon: &mut HashSet<CellId>,
    out: &mut Vec<LpViolation>,
) {
    let cell_upf = ctx.graph.cell(cid).upf_path.clone();
    let rtl_loc = ctx.graph.format_loc(ctx.graph.cell(cid).loc);
    let iso_pd = ctx.cell_domains[cid.0 as usize];

    if let Some(en_def) = lib_c.iso_enable_pin() {
        let en_name = en_def.name.clone();
        if let Some(&en_pid) = ctx
            .graph
            .cell(cid)
            .pins
            .iter()
            .find(|&&p| ctx.graph.pin(p).name == en_name)
        {
            trace_ctrl_cone_for_non_aon(ctx, en_pid, "isolation enable", emitted_aon, out);
        }
    }

    let data_name = lib_c
        .iso_data_pin()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "A".to_string());
    let Some(&data_pid) = ctx
        .graph
        .cell(cid)
        .pins
        .iter()
        .find(|&&p| ctx.graph.pin(p).name == data_name)
    else {
        return;
    };

    let mut upstream_src_pd: Option<PowerDomainId> = None;
    for pnet in pin_parent_nets(ctx.graph, data_pid) {
        for drv in resolve_driving_sources(ctx.graph, ctx.liberty, pnet, 6) {
            if let SignalEndpoint::LeafPin {
                cell: sc,
                pin: spid,
            } = drv
            {
                let sc_pd = ctx.cell_domains[sc.0 as usize];
                let smod = ctx.graph.cell(sc).module.clone();
                let slib = ctx.liberty.cell(&smod);
                let spn = ctx.graph.pin(spid).name.clone();
                let sc_ss = ctx.effective_pin_ss(sc, &spn);

                if let Some(sl) = slib {
                    let is_regular_comb = !sl.is_isolation_cell
                        && !sl.is_always_on
                        && !sl.is_sequential()
                        && !sl.is_macro_cell
                        && !sl.is_power_switch()
                        && !sl.is_level_shifter;
                    if is_regular_comb {
                        if let Some(sc_ss_id) = sc_ss {
                            if let Some((orig_pd, _)) =
                                find_upstream_switchable_domain(ctx, sat, sc, sc_ss_id, 4)
                            {
                                upstream_src_pd = Some(orig_pd);
                                if stray_before_iso.insert(sc) {
                                    let sc_upf = ctx.graph.cell(sc).upf_path.clone();
                                    let sc_loc = ctx.graph.format_loc(ctx.graph.cell(sc).loc);
                                    push_violation(
                                        out,
                                        "ISO_LOGIC_BEFORE_CELL",
                                        "netlist",
                                        ctx.domain_name(Some(orig_pd)),
                                        ctx.domain_name(sc_pd),
                                        sc_upf.clone(),
                                        format!(
                                            "non-AON gate `{sc_upf}` ({smod}) lies between switchable domain and isolation cell `{cell_upf}`"
                                        ),
                                        sc_loc,
                                    );
                                }
                                continue;
                            }
                        }
                    }
                }
                if upstream_src_pd.is_none() {
                    upstream_src_pd = sc_pd;
                }
            }
        }
    }

    if let Some(src_pd) = upstream_src_pd {
        let cell_clamp = lib_c.iso_clamp_kind();
        let src_dom_name = &ctx.resolved.intent.domains[src_pd.0 as usize].name;
        if let Some(iso_strat) = ctx
            .resolved
            .intent
            .isolation_strategies
            .iter()
            .find(|s| s.domain == *src_dom_name && !s.no_isolation)
        {
            let clamp_mismatch = matches!(
                (iso_strat.clamp_value, cell_clamp),
                (
                    Some(ClampValue::Zero),
                    Some(IsoClampKind::One | IsoClampKind::Latch)
                ) | (
                    Some(ClampValue::One),
                    Some(IsoClampKind::Zero | IsoClampKind::Latch)
                ) | (
                    Some(ClampValue::Latch),
                    Some(IsoClampKind::Zero | IsoClampKind::One)
                )
            );
            if clamp_mismatch {
                push_violation(
                    out,
                    "ISO_CELL_MISMATCH",
                    "netlist",
                    ctx.domain_name(Some(src_pd)),
                    ctx.domain_name(iso_pd),
                    cell_upf.clone(),
                    format!(
                        "isolation cell `{cell_upf}` ({}) clamp `{cell_clamp:?}` disagrees with UPF strategy `{}` (`{:?}`)",
                        lib_c.name, iso_strat.name, iso_strat.clamp_value
                    ),
                    rtl_loc,
                );
            }
        }
    }
}

fn find_upstream_switchable_domain(
    ctx: &mut GatePgCtx<'_>,
    sat: &mut PstSat,
    start_cell: CellId,
    sink_ss: SupplySetId,
    max_depth: usize,
) -> Option<(PowerDomainId, SupplySetId)> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back((start_cell, 0usize));
    visited.insert(start_cell);

    while let Some((cid, d)) = queue.pop_front() {
        if d >= max_depth {
            continue;
        }
        let cmod = ctx.graph.cell(cid).module.clone();
        let Some(clib) = ctx.liberty.cell(&cmod) else {
            continue;
        };
        let pin_ids = ctx.graph.cell(cid).pins.clone();
        for pid in pin_ids {
            let pname = ctx.graph.pin(pid).name.clone();
            if clib
                .pin(&pname)
                .is_none_or(|sp| sp.direction != PinDir::Input)
            {
                continue;
            }
            for pnet in pin_parent_nets(ctx.graph, pid) {
                for drv in resolve_driving_sources(ctx.graph, ctx.liberty, pnet, 6) {
                    if let SignalEndpoint::LeafPin {
                        cell: sc,
                        pin: spid,
                    } = drv
                    {
                        let smod = ctx.graph.cell(sc).module.clone();
                        let slib = ctx.liberty.cell(&smod);
                        if slib.is_some_and(|l| l.is_isolation_cell) {
                            continue;
                        }
                        let spn = ctx.graph.pin(spid).name.clone();
                        if let Some(sc_ss) = ctx.effective_pin_ss(sc, &spn) {
                            if sc_ss != sink_ss && sat.can_be_off_while_on(sc_ss, sink_ss).is_some()
                            {
                                if let Some(spd) = ctx.cell_domains[sc.0 as usize] {
                                    return Some((spd, sc_ss));
                                }
                            }
                            if visited.insert(sc) {
                                queue.push_back((sc, d + 1));
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn check_psw_ctrl_cone(
    ctx: &mut GatePgCtx<'_>,
    cid: CellId,
    lib_c: &LibertyCell,
    emitted_aon: &mut HashSet<CellId>,
    out: &mut Vec<LpViolation>,
) {
    let sw_pins: Vec<String> = lib_c
        .pins
        .iter()
        .filter(|p| p.is_switch_pin)
        .map(|p| p.name.clone())
        .collect();
    for sp in sw_pins {
        if let Some(&pid) = ctx
            .graph
            .cell(cid)
            .pins
            .iter()
            .find(|&&p| ctx.graph.pin(p).name == sp)
        {
            trace_ctrl_cone_for_non_aon(ctx, pid, "power-switch control", emitted_aon, out);
        }
    }
}

pub(super) fn trace_ctrl_cone_for_non_aon(
    ctx: &mut GatePgCtx<'_>,
    start_pid: PinId,
    role: &str,
    emitted_aon: &mut HashSet<CellId>,
    out: &mut Vec<LpViolation>,
) {
    let mut visited_cells = HashSet::new();
    let mut queue = VecDeque::new();
    for pnet in pin_parent_nets(ctx.graph, start_pid) {
        queue.push_back((pnet, 0usize));
    }

    while let Some((nid, d)) = queue.pop_front() {
        if d > 8 {
            continue;
        }
        for drv in resolve_driving_sources(ctx.graph, ctx.liberty, nid, 6) {
            if let SignalEndpoint::LeafPin { cell: sc, .. } = drv {
                if !visited_cells.insert(sc) {
                    continue;
                }
                let smod = ctx.graph.cell(sc).module.clone();
                let Some(slib) = ctx.liberty.cell(&smod) else {
                    continue;
                };
                if slib.is_always_on
                    || slib.is_isolation_cell
                    || slib.is_power_switch()
                    || slib.is_retention()
                    || slib.is_macro_cell
                {
                    continue;
                }
                if !slib.is_sequential() && ctx.cell_power_can_be_off(sc) && emitted_aon.insert(sc)
                {
                    let sc_upf = ctx.graph.cell(sc).upf_path.clone();
                    let sc_loc = ctx.graph.format_loc(ctx.graph.cell(sc).loc);
                    let dom = ctx.domain_name(ctx.cell_domains[sc.0 as usize]);
                    push_violation(
                        out,
                        "AON_NON_AON_BUFFER",
                        "netlist",
                        dom,
                        None,
                        sc_upf.clone(),
                        format!(
                            "non-AON cell `{sc_upf}` ({smod}) on {role} path is powered by a switchable supply"
                        ),
                        sc_loc,
                    );
                }
                let sc_pins = ctx.graph.cell(sc).pins.clone();
                for pid in sc_pins {
                    let pn = ctx.graph.pin(pid).name.clone();
                    if slib
                        .pin(&pn)
                        .is_some_and(|sp| sp.direction == PinDir::Input)
                    {
                        for up_net in pin_parent_nets(ctx.graph, pid) {
                            queue.push_back((up_net, d + 1));
                        }
                    }
                }
            }
        }
    }
}

fn check_upf_ctrl_forward_aon(
    ctx: &mut GatePgCtx<'_>,
    emitted_aon: &mut HashSet<CellId>,
    out: &mut Vec<LpViolation>,
) {
    let mut ctrl_sigs: Vec<String> = Vec::new();
    for iso in &ctx.resolved.intent.isolation_strategies {
        if let Some(ref s) = iso.isolation_signal {
            ctrl_sigs.push(s.clone());
        }
    }
    for ret in &ctx.resolved.intent.retention_strategies {
        if let Some(ref s) = ret.save_signal {
            ctrl_sigs.push(s.clone());
        }
        if let Some(ref s) = ret.restore_signal {
            ctrl_sigs.push(s.clone());
        }
    }
    for sw in &ctx.resolved.intent.power_switches {
        for (_, net) in &sw.control_ports {
            ctrl_sigs.push(net.clone());
        }
    }

    for sig in ctrl_sigs {
        let Some(nid) = ctx.graph.lookup_net_by_upf(&sig) else {
            continue;
        };
        for ld in resolve_sink_loads(ctx.graph, ctx.liberty, nid, 4) {
            if let SignalEndpoint::LeafPin { cell: dc, .. } = ld {
                let dmod = ctx.graph.cell(dc).module.clone();
                let Some(dlib) = ctx.liberty.cell(&dmod) else {
                    continue;
                };
                if dlib.is_always_on
                    || dlib.is_isolation_cell
                    || dlib.is_level_shifter
                    || dlib.is_retention()
                    || dlib.is_power_switch()
                    || dlib.is_macro_cell
                    || dlib.is_sequential()
                {
                    continue;
                }
                if ctx.cell_power_can_be_off(dc) && emitted_aon.insert(dc) {
                    let dc_upf = ctx.graph.cell(dc).upf_path.clone();
                    let dc_loc = ctx.graph.format_loc(ctx.graph.cell(dc).loc);
                    let dom = ctx.domain_name(ctx.cell_domains[dc.0 as usize]);
                    push_violation(
                        out,
                        "AON_NON_AON_BUFFER",
                        "netlist",
                        dom,
                        None,
                        dc_upf.clone(),
                        format!(
                            "non-AON cell `{dc_upf}` ({dmod}) buffers UPF control signal `{sig}` while powered by a switchable supply"
                        ),
                        dc_loc,
                    );
                }
            }
        }
    }
}
