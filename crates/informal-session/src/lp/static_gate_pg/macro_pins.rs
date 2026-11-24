//! Stage 3 multi-rail macro pin supply-crossing checks (`MACRO_PIN_SUPPLY_CROSSING`).

use std::collections::HashSet;

use informal_liberty::PinDir;
use informal_svlang::struct_graph::CellId;
use informal_upf::{PstSat, SupplySetId};

use super::cells_aon::{resolve_driving_sources, resolve_sink_loads, SignalEndpoint};
use super::pg_ctx::{needs_level_shifter, pin_parent_nets, push_violation, GatePgCtx};
use crate::lp::LpViolation;

/// Checks every instantiated Liberty macro cell (`is_macro_cell`) for
/// unisolated or unshifted crossings between each pin's `related_power_pin`
/// domain and its external drivers/loads.
pub fn check_macro_pin_crossings(
    ctx: &mut GatePgCtx<'_>,
    sat: &mut PstSat,
    tol_uv: i64,
    out: &mut Vec<LpViolation>,
) {
    let mut emitted: HashSet<(CellId, String)> = HashSet::new();
    let cell_count = ctx.graph.cells.len();

    for raw_id in 0..cell_count {
        let cid = CellId(raw_id as u32);
        let module = ctx.graph.cell(cid).module.clone();
        let Some(lib_c) = ctx.liberty.cell(&module) else {
            continue;
        };
        if !lib_c.is_macro_cell {
            continue;
        }
        let cell_upf = ctx.graph.cell(cid).upf_path.clone();
        let rtl_loc = ctx.graph.format_loc(ctx.graph.cell(cid).loc);
        let cell_pd = ctx.cell_domains[raw_id];
        let pin_ids = ctx.graph.cell(cid).pins.clone();

        for pid in pin_ids {
            let pin_name = ctx.graph.pin(pid).name.clone();
            let Some(sp) = lib_c.pin(&pin_name) else {
                continue;
            };
            let Some(pin_ss) = ctx.effective_pin_ss(cid, &pin_name) else {
                continue;
            };
            let pnets = pin_parent_nets(ctx.graph, pid);
            if pnets.is_empty() {
                continue;
            }

            match sp.direction {
                PinDir::Input => {
                    for pnet in pnets {
                        let drivers = resolve_driving_sources(ctx.graph, ctx.liberty, pnet, 8);
                        for drv in drivers {
                            if is_endpoint_iso_or_ls(ctx, &drv) {
                                continue;
                            }
                            let Some(drv_ss) = endpoint_ss(ctx, &drv) else {
                                continue;
                            };
                            if drv_ss == pin_ss {
                                continue;
                            }
                            let needs_iso = sat.can_be_off_while_on(drv_ss, pin_ss).is_some();
                            let needs_ls = needs_level_shifter(sat, drv_ss, pin_ss, tol_uv);
                            if (needs_iso || needs_ls) && emitted.insert((cid, pin_name.clone())) {
                                let src_dom = endpoint_domain(ctx, &drv);
                                let dst_dom = ctx.domain_name(cell_pd);
                                push_violation(
                                    out,
                                    "MACRO_PIN_SUPPLY_CROSSING",
                                    "netlist",
                                    src_dom,
                                    dst_dom,
                                    format!("{cell_upf}/{pin_name}"),
                                    format!(
                                        "macro `{cell_upf}` ({module}) input pin `{pin_name}` (related_power_pin `{}`) receives {} crossing without isolation/level-shifter",
                                        sp.related_power_pin.as_deref().unwrap_or("?"),
                                        if needs_iso { "switchable-to-active" } else { "cross-voltage" }
                                    ),
                                    rtl_loc.clone(),
                                );
                            }
                        }
                    }
                }
                PinDir::Output | PinDir::Inout => {
                    for pnet in pnets {
                        let loads = resolve_sink_loads(ctx.graph, ctx.liberty, pnet, 8);
                        for ld in loads {
                            if is_endpoint_iso_or_ls(ctx, &ld) {
                                continue;
                            }
                            let Some(ld_ss) = endpoint_ss(ctx, &ld) else {
                                continue;
                            };
                            if ld_ss == pin_ss {
                                continue;
                            }
                            let needs_iso = sat.can_be_off_while_on(pin_ss, ld_ss).is_some();
                            let needs_ls = needs_level_shifter(sat, pin_ss, ld_ss, tol_uv);
                            if (needs_iso || needs_ls) && emitted.insert((cid, pin_name.clone())) {
                                let src_dom = ctx.domain_name(cell_pd);
                                let dst_dom = endpoint_domain(ctx, &ld);
                                push_violation(
                                    out,
                                    "MACRO_PIN_SUPPLY_CROSSING",
                                    "netlist",
                                    src_dom,
                                    dst_dom,
                                    format!("{cell_upf}/{pin_name}"),
                                    format!(
                                        "macro `{cell_upf}` ({module}) output pin `{pin_name}` (related_power_pin `{}`) drives {} sink without isolation/level-shifter",
                                        sp.related_power_pin.as_deref().unwrap_or("?"),
                                        if needs_iso { "active" } else { "different-voltage" }
                                    ),
                                    rtl_loc.clone(),
                                );
                            }
                        }
                    }
                }
                PinDir::Internal => {}
            }
        }
    }
}

fn is_endpoint_iso_or_ls(ctx: &GatePgCtx<'_>, ep: &SignalEndpoint) -> bool {
    match ep {
        SignalEndpoint::LeafPin { cell, .. } => ctx
            .liberty
            .cell(&ctx.graph.cell(*cell).module)
            .is_some_and(|lc| lc.is_isolation_cell || lc.is_level_shifter),
        SignalEndpoint::TopPort { .. } => false,
    }
}

fn endpoint_ss(ctx: &mut GatePgCtx<'_>, ep: &SignalEndpoint) -> Option<SupplySetId> {
    match ep {
        SignalEndpoint::LeafPin { cell, pin } => {
            let pname = ctx.graph.pin(*pin).name.clone();
            ctx.effective_pin_ss(*cell, &pname)
        }
        SignalEndpoint::TopPort { cell, .. } => {
            ctx.domain_primary_ss(ctx.cell_domains[cell.0 as usize])
        }
    }
}

fn endpoint_domain(ctx: &GatePgCtx<'_>, ep: &SignalEndpoint) -> Option<String> {
    match ep {
        SignalEndpoint::LeafPin { cell, .. } | SignalEndpoint::TopPort { cell, .. } => {
            ctx.domain_name(ctx.cell_domains[cell.0 as usize])
        }
    }
}
