//! Stage 3 Level-Shifter (`LS_DIR_CELL_MISMATCH`, `LS_VRANGE_CELL_MISMATCH`,
//! `LS_ORDER_VIOLATION`) and Retention (`RET_CELL_UNMAPPED`, `RET_PIN_UNCONNECTED`)
//! checks over Liberty-mapped gate netlists.

use std::collections::HashSet;

use informal_liberty::{LevelShifterType, LibertyCell, PinDir};
use informal_svlang::struct_graph::CellId;
use informal_upf::{LsRequirement, PowerDomainId, PstSat, StrategyMatch, SupplySetId};

use super::cells_aon::{
    resolve_driving_sources, resolve_sink_loads, trace_ctrl_cone_for_non_aon, SignalEndpoint,
};
use super::pg_ctx::{pin_parent_nets, push_violation, GatePgCtx};
use crate::lp::LpViolation;

pub fn check_ls_cell(
    ctx: &mut GatePgCtx<'_>,
    sat: &mut PstSat,
    cid: CellId,
    lib_c: &LibertyCell,
    tol_uv: i64,
    out: &mut Vec<LpViolation>,
) {
    let cell_upf = ctx.graph.cell(cid).upf_path.clone();
    let rtl_loc = ctx.graph.format_loc(ctx.graph.cell(cid).loc);

    let data_name = lib_c
        .ls_data_pin()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "A".to_string());
    let out_name = lib_c
        .pins
        .iter()
        .find(|p| p.direction == PinDir::Output)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "Y".to_string());

    let in_pid = ctx
        .graph
        .cell(cid)
        .pins
        .iter()
        .copied()
        .find(|&p| ctx.graph.pin(p).name == data_name);
    let out_pid = ctx
        .graph
        .cell(cid)
        .pins
        .iter()
        .copied()
        .find(|&p| ctx.graph.pin(p).name == out_name);

    let mut src_ss: Option<SupplySetId> = None;
    let mut src_pd: Option<PowerDomainId> = None;
    let mut src_is_iso = false;
    if let Some(pid) = in_pid {
        for pnet in pin_parent_nets(ctx.graph, pid) {
            for drv in resolve_driving_sources(ctx.graph, ctx.liberty, pnet, 6) {
                match drv {
                    SignalEndpoint::LeafPin { cell: sc, pin: spid } => {
                        let smod = ctx.graph.cell(sc).module.clone();
                        if ctx
                            .liberty
                            .cell(&smod)
                            .is_some_and(|l| l.is_isolation_cell)
                        {
                            src_is_iso = true;
                        }
                        let spn = ctx.graph.pin(spid).name.clone();
                        src_ss = ctx.effective_pin_ss(sc, &spn);
                        src_pd = ctx.cell_domains[sc.0 as usize];
                        if src_ss.is_some() {
                            break;
                        }
                    }
                    SignalEndpoint::TopPort { cell: sc, .. } => {
                        src_pd = ctx.cell_domains[sc.0 as usize];
                        src_ss = ctx.domain_primary_ss(src_pd);
                    }
                }
            }
        }
    }

    let mut dst_ss: Option<SupplySetId> = None;
    let mut dst_pd: Option<PowerDomainId> = None;
    let mut feeds_iso = false;
    if let Some(pid) = out_pid {
        for pnet in pin_parent_nets(ctx.graph, pid) {
            for ld in resolve_sink_loads(ctx.graph, ctx.liberty, pnet, 6) {
                match ld {
                    SignalEndpoint::LeafPin { cell: dc, pin: dpid } => {
                        let dmod = ctx.graph.cell(dc).module.clone();
                        if ctx
                            .liberty
                            .cell(&dmod)
                            .is_some_and(|l| l.is_isolation_cell)
                        {
                            feeds_iso = true;
                        }
                        let dpn = ctx.graph.pin(dpid).name.clone();
                        dst_ss = ctx.effective_pin_ss(dc, &dpn);
                        dst_pd = ctx.cell_domains[dc.0 as usize];
                        if dst_ss.is_some() {
                            break;
                        }
                    }
                    SignalEndpoint::TopPort { cell: dc, .. } => {
                        dst_pd = ctx.cell_domains[dc.0 as usize];
                        dst_ss = ctx.domain_primary_ss(dst_pd);
                    }
                }
            }
        }
    }

    if !lib_c.is_els() {
        let unisolated_in = !src_is_iso
            && src_ss
                .zip(dst_ss)
                .is_some_and(|(s, d)| sat.can_be_off_while_on(s, d).is_some());
        if feeds_iso || unisolated_in {
            push_violation(
                out,
                "LS_ORDER_VIOLATION",
                "netlist",
                ctx.domain_name(src_pd),
                ctx.domain_name(dst_pd),
                cell_upf.clone(),
                format!(
                    "level shifter `{cell_upf}` ({}) precedes isolation or receives an unisolated switchable input without ELS support",
                    lib_c.name
                ),
                rtl_loc.clone(),
            );
        }
    }

    let (Some(s_ss), Some(d_ss)) = (src_ss, dst_ss) else {
        return;
    };

    // Check LS direction directly via symbolic PST `voltage_relation`.
    let rel = sat.voltage_relation(s_ss, d_ss, tol_uv);
    let dir_bad = matches!(
        (lib_c.level_shifter_type, &rel),
        (
            Some(LevelShifterType::Hl),
            LsRequirement::LowToHigh { .. } | LsRequirement::Both { .. }
        ) | (
            Some(LevelShifterType::Lh),
            LsRequirement::HighToLow { .. } | LsRequirement::Both { .. }
        )
    );
    if dir_bad {
        push_violation(
            out,
            "LS_DIR_CELL_MISMATCH",
            "netlist",
            ctx.domain_name(src_pd),
            ctx.domain_name(dst_pd),
            cell_upf.clone(),
            format!(
                "level shifter `{cell_upf}` ({}, type `{:?}`) direction conflicts with PST voltage relation `{rel:?}`",
                lib_c.name, lib_c.level_shifter_type
            ),
            rtl_loc.clone(),
        );
    }

    // Check Liberty `input_voltage_range` / `output_voltage_range` against UPF nominal voltages.
    let s_volts = supply_set_nominal_volts(ctx, s_ss);
    let d_volts = supply_set_nominal_volts(ctx, d_ss);
    let tol_v = (tol_uv as f64) / 1_000_000.0;
    if let (Some(vs), Some(vd)) = (s_volts, d_volts) {
        let in_bad = lib_c
            .input_voltage_range
            .is_some_and(|(lo, hi)| vs < lo - tol_v || vs > hi + tol_v);
        let out_bad = lib_c
            .output_voltage_range
            .is_some_and(|(lo, hi)| vd < lo - tol_v || vd > hi + tol_v);
        if in_bad || out_bad {
            push_violation(
                out,
                "LS_VRANGE_CELL_MISMATCH",
                "netlist",
                ctx.domain_name(src_pd),
                ctx.domain_name(dst_pd),
                cell_upf,
                format!(
                    "level shifter `{}` voltage range (in={:?}, out={:?}) does not cover crossing {vs:.2}V -> {vd:.2}V",
                    lib_c.name, lib_c.input_voltage_range, lib_c.output_voltage_range
                ),
                rtl_loc,
            );
        }
    }
}

pub fn check_ret_cell_pins(
    ctx: &mut GatePgCtx<'_>,
    cid: CellId,
    lib_c: &LibertyCell,
    emitted_aon: &mut HashSet<CellId>,
    out: &mut Vec<LpViolation>,
) {
    let cell_upf = ctx.graph.cell(cid).upf_path.clone();
    let rtl_loc = ctx.graph.format_loc(ctx.graph.cell(cid).loc);
    let dom = ctx.domain_name(ctx.cell_domains[cid.0 as usize]);

    let ret_pins: Vec<String> = lib_c
        .pins
        .iter()
        .filter(|p| p.retention_pin.is_some())
        .map(|p| p.name.clone())
        .collect();
    for rp in ret_pins {
        let pid_opt = ctx
            .graph
            .cell(cid)
            .pins
            .iter()
            .copied()
            .find(|&p| ctx.graph.pin(p).name == rp);
        let unconnected = match pid_opt {
            None => true,
            Some(pid) => {
                let p = ctx.graph.pin(pid);
                p.unconnected || p.external_conns.is_empty()
            }
        };
        if unconnected {
            push_violation(
                out,
                "RET_PIN_UNCONNECTED",
                "netlist",
                dom.clone(),
                None,
                format!("{cell_upf}/{rp}"),
                format!(
                    "retention pin `{rp}` on `{cell_upf}` ({}) is unconnected",
                    lib_c.name
                ),
                rtl_loc.clone(),
            );
        } else if let Some(pid) = pid_opt {
            trace_ctrl_cone_for_non_aon(ctx, pid, "retention control", emitted_aon, out);
        }
    }
}

pub fn check_retention_mapping(ctx: &GatePgCtx<'_>, out: &mut Vec<LpViolation>) {
    if ctx.resolved.intent.retention_strategies.is_empty() {
        return;
    }
    let mut flagged: HashSet<CellId> = HashSet::new();

    for (raw_id, cell) in ctx.graph.cells.iter().enumerate() {
        let Some(pd_id) = ctx.cell_domains[raw_id] else {
            continue;
        };
        let Some(lib_c) = ctx.liberty.cell(&cell.module) else {
            continue;
        };
        if !lib_c.is_sequential() || lib_c.is_macro_cell || lib_c.is_retention() {
            continue;
        }
        if let StrategyMatch::Matched(ret) = ctx.resolved.match_retention(pd_id, &cell.upf_path) {
            if !ret.no_retention && flagged.insert(CellId(raw_id as u32)) {
                push_violation(
                    out,
                    "RET_CELL_UNMAPPED",
                    "netlist",
                    ctx.domain_name(Some(pd_id)),
                    None,
                    cell.upf_path.clone(),
                    format!(
                        "sequential cell `{}` ({}) targeted by retention strategy `{}` is not mapped to a Liberty retention cell",
                        cell.upf_path, cell.module, ret.name
                    ),
                    ctx.graph.format_loc(cell.loc),
                );
            }
        }
    }
}

fn supply_set_nominal_volts(ctx: &GatePgCtx<'_>, ss_id: SupplySetId) -> Option<f64> {
    let ss_name = &ctx.resolved.intent.supply_sets[ss_id.0 as usize].name;
    let mut best_uv: Option<i64> = None;
    for ps in &ctx.resolved.intent.power_states {
        if ps.illegal || ps.target != *ss_name {
            continue;
        }
        if let Some(ref expr) = ps.supply_expr {
            for uv in extract_volts_uv_from_expr(expr) {
                best_uv = Some(best_uv.map_or(uv, |prev| prev.max(uv)));
            }
        }
    }
    if let Some(uv) = best_uv {
        return Some((uv as f64) / 1_000_000.0);
    }
    if let Some(pnet) = ctx
        .resolved
        .ss_power_net
        .get(ss_id.0 as usize)
        .copied()
        .flatten()
    {
        let sn = &ctx.resolved.intent.supply_nets[pnet.0 as usize];
        if let Some((lo, hi)) = sn.nominal_voltage.and_then(|v| v.bounds_uv()) {
            if hi > 0 {
                return Some(((lo + hi) as f64) / 2_000_000.0);
            }
        }
        for st in &sn.port_states {
            if let Some((lo, hi)) = st.voltage.bounds_uv() {
                if hi > 0 {
                    return Some(((lo + hi) as f64) / 2_000_000.0);
                }
            }
        }
        if let Some(&v) = ctx.liberty.voltage_map.get(&sn.name) {
            return Some(v);
        }
    }
    None
}

fn extract_volts_uv_from_expr(expr: &str) -> Vec<i64> {
    let mut out = Vec::new();
    let tokens: Vec<&str> = expr
        .split(|c: char| c.is_whitespace() || matches!(c, '{' | '}' | '(' | ')' | ','))
        .filter(|t| !t.is_empty())
        .collect();
    for w in tokens.windows(2) {
        if w[0].eq_ignore_ascii_case("FULL_ON") {
            if let Ok(v) = w[1].parse::<f64>() {
                if v > 0.001 {
                    out.push((v * 1_000_000.0).round() as i64);
                }
            }
        }
    }
    out
}
