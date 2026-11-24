//! Stage 4 Physical Power/Ground (PG) netlist checks:
//! `PG_PIN_UNCONNECTED`, `PG_SUPPLY_MISMATCH`, `PG_BACKUP_MISMATCH`,
//! `PG_SHORT`, `PG_BIAS_MISMATCH`, `PG_SWITCH_CHAIN_BROKEN`.

use std::collections::HashMap;

use informal_liberty::cell::eval_lib_bool;
use informal_liberty::{LibertyCell, PgPinDef, PgType};
use informal_svlang::struct_graph::{CellId, ExprOpSummary, PinId, StructuralGraph};
use informal_upf::{PowerDomainId, PstSat, SupplyNetId};

use super::pg_ctx::{push_violation, GatePgCtx, PgResolved, PinPgState};
use crate::lp::LpViolation;

/// Runs all Stage 4 physical PG checks across all Liberty-instantiated cells.
pub fn run_stage4_checks(
    ctx: &mut GatePgCtx<'_>,
    sat: &mut PstSat,
    out: &mut Vec<LpViolation>,
) {
    let cell_count = ctx.graph.cells.len();
    for raw_id in 0..cell_count {
        let cid = CellId(raw_id as u32);
        let module = ctx.graph.cell(cid).module.clone();
        let Some(lib_c) = ctx.liberty.cell(&module) else {
            continue;
        };
        let pd = ctx.cell_domains[raw_id];
        let cell_upf = ctx.graph.cell(cid).upf_path.clone();
        let rtl_loc = ctx.graph.format_loc(ctx.graph.cell(cid).loc);
        let dom_name = ctx.domain_name(pd);
        let exp_pwr = ctx.domain_primary_power(pd);
        let exp_gnd = ctx.domain_primary_ground(pd);

        let mut pwr_sns: Vec<(String, SupplyNetId)> = Vec::new();
        let mut gnd_sns: Vec<(String, SupplyNetId)> = Vec::new();
        let mut psw_in_sn: Option<SupplyNetId> = None;
        let mut psw_out_sn: Option<SupplyNetId> = None;
        let mut psw_out_pg: Option<PgPinDef> = None;

        let pg_pins = lib_c.pg_pins.clone();
        for pg in &pg_pins {
            let state = ctx.pin_pg_state(cid, &pg.name);
            let pin_sig = format!("{cell_upf}/{}", pg.name);
            match state {
                PinPgState::Unconnected => {
                    push_violation(
                        out,
                        "PG_PIN_UNCONNECTED",
                        "pg",
                        dom_name.clone(),
                        None,
                        pin_sig,
                        format!(
                            "Liberty pg_pin `{}` on cell `{cell_upf}` ({module}) is unconnected",
                            pg.name
                        ),
                        rtl_loc.clone(),
                    );
                }
                PinPgState::Connected(PgResolved::Short(sns)) => {
                    let names: Vec<&str> = sns.iter().map(|&s| ctx.sn_name(s)).collect();
                    push_violation(
                        out,
                        "PG_SHORT",
                        "pg",
                        dom_name.clone(),
                        None,
                        pin_sig,
                        format!(
                            "pg_pin `{}` on `{cell_upf}` ({module}) shorts incompatible supply nets: {}",
                            pg.name,
                            names.join(", ")
                        ),
                        rtl_loc.clone(),
                    );
                }
                PinPgState::Connected(PgResolved::Single(sn)) => {
                    if pg.pg_type.is_power() {
                        pwr_sns.push((pg.name.clone(), sn));
                    } else if pg.pg_type.is_ground() {
                        gnd_sns.push((pg.name.clone(), sn));
                    }
                    check_single_pg_pin(
                        ctx, lib_c, pg, sn, pd, exp_pwr, exp_gnd, &cell_upf, &pin_sig, &dom_name,
                        &rtl_loc, out,
                    );
                    if lib_c.is_power_switch() {
                        if pg.pg_type == PgType::PrimaryPower {
                            psw_in_sn = Some(sn);
                        } else if pg.pg_type == PgType::InternalPower
                            || pg.switch_function.is_some()
                        {
                            psw_out_sn = Some(sn);
                            psw_out_pg = Some(pg.clone());
                        }
                    }
                }
                PinPgState::Connected(PgResolved::Const { .. } | PgResolved::Unresolved) => {}
            }
        }

        for (pn, psn) in &pwr_sns {
            for (gn, gsn) in &gnd_sns {
                if psn == gsn {
                    push_violation(
                        out,
                        "PG_SHORT",
                        "pg",
                        dom_name.clone(),
                        None,
                        format!("{cell_upf}/{pn}"),
                        format!(
                            "power pin `{pn}` and ground pin `{gn}` on `{cell_upf}` ({module}) are shorted to `{}`",
                            ctx.sn_name(*psn)
                        ),
                        rtl_loc.clone(),
                    );
                }
            }
        }

        if lib_c.is_power_switch() {
            check_psw_chain(
                ctx,
                sat,
                cid,
                lib_c,
                psw_in_sn,
                psw_out_sn,
                psw_out_pg.as_ref(),
                &cell_upf,
                &dom_name,
                &rtl_loc,
                out,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn check_single_pg_pin(
    ctx: &GatePgCtx<'_>,
    lib_c: &LibertyCell,
    pg: &PgPinDef,
    sn: SupplyNetId,
    pd: Option<PowerDomainId>,
    exp_pwr: Option<SupplyNetId>,
    exp_gnd: Option<SupplyNetId>,
    cell_upf: &str,
    pin_sig: &str,
    dom_name: &Option<String>,
    rtl_loc: &Option<String>,
    out: &mut Vec<LpViolation>,
) {
    let is_special_multi_rail = lib_c.is_isolation_cell
        || lib_c.is_level_shifter
        || lib_c.is_always_on
        || lib_c.is_power_switch()
        || lib_c.is_macro_cell;

    match pg.pg_type {
        PgType::PrimaryPower if !is_special_multi_rail => {
            if let Some(ep) = exp_pwr {
                if sn != ep {
                    push_violation(
                        out,
                        "PG_SUPPLY_MISMATCH",
                        "pg",
                        dom_name.clone(),
                        None,
                        pin_sig.to_string(),
                        format!(
                            "primary power pin `{}` on `{cell_upf}` ({}) connects to `{}` instead of domain primary supply `{}`",
                            pg.name,
                            lib_c.name,
                            ctx.sn_name(sn),
                            ctx.sn_name(ep)
                        ),
                        rtl_loc.clone(),
                    );
                }
            }
        }
        PgType::PrimaryGround => {
            if let Some(eg) = exp_gnd {
                if sn != eg {
                    push_violation(
                        out,
                        "PG_SUPPLY_MISMATCH",
                        "pg",
                        dom_name.clone(),
                        None,
                        pin_sig.to_string(),
                        format!(
                            "primary ground pin `{}` on `{cell_upf}` ({}) connects to `{}` instead of domain ground `{}`",
                            pg.name,
                            lib_c.name,
                            ctx.sn_name(sn),
                            ctx.sn_name(eg)
                        ),
                        rtl_loc.clone(),
                    );
                }
            }
        }
        PgType::BackupPower if !lib_c.is_level_shifter => {
            if ctx.is_supply_switchable(sn) {
                push_violation(
                    out,
                    "PG_BACKUP_MISMATCH",
                    "pg",
                    dom_name.clone(),
                    None,
                    pin_sig.to_string(),
                    format!(
                        "backup/always-on power pin `{}` on `{cell_upf}` ({}) is connected to switchable supply `{}`",
                        pg.name,
                        lib_c.name,
                        ctx.sn_name(sn)
                    ),
                    rtl_loc.clone(),
                );
            }
        }
        PgType::Nwell | PgType::DeepNwell => {
            if exp_gnd == Some(sn) {
                push_violation(
                    out,
                    "PG_BIAS_MISMATCH",
                    "pg",
                    dom_name.clone(),
                    None,
                    pin_sig.to_string(),
                    format!(
                        "nwell bias pin `{}` on `{cell_upf}` is tied to ground `{}`",
                        pg.name,
                        ctx.sn_name(sn)
                    ),
                    rtl_loc.clone(),
                );
            } else if let Some(exp_nw) = ctx.domain_bias_net(pd, "nwell") {
                if sn != exp_nw {
                    push_violation(
                        out,
                        "PG_BIAS_MISMATCH",
                        "pg",
                        dom_name.clone(),
                        None,
                        pin_sig.to_string(),
                        format!(
                            "nwell bias pin `{}` on `{cell_upf}` connects to `{}` instead of UPF nwell `{}`",
                            pg.name,
                            ctx.sn_name(sn),
                            ctx.sn_name(exp_nw)
                        ),
                        rtl_loc.clone(),
                    );
                }
            }
        }
        PgType::Pwell | PgType::DeepPwell => {
            if exp_pwr == Some(sn) {
                push_violation(
                    out,
                    "PG_BIAS_MISMATCH",
                    "pg",
                    dom_name.clone(),
                    None,
                    pin_sig.to_string(),
                    format!(
                        "pwell bias pin `{}` on `{cell_upf}` is tied to power `{}`",
                        pg.name,
                        ctx.sn_name(sn)
                    ),
                    rtl_loc.clone(),
                );
            } else if let Some(exp_pw) = ctx.domain_bias_net(pd, "pwell") {
                if sn != exp_pw {
                    push_violation(
                        out,
                        "PG_BIAS_MISMATCH",
                        "pg",
                        dom_name.clone(),
                        None,
                        pin_sig.to_string(),
                        format!(
                            "pwell bias pin `{}` on `{cell_upf}` connects to `{}` instead of UPF pwell `{}`",
                            pg.name,
                            ctx.sn_name(sn),
                            ctx.sn_name(exp_pw)
                        ),
                        rtl_loc.clone(),
                    );
                }
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn check_psw_chain(
    ctx: &GatePgCtx<'_>,
    sat: &mut PstSat,
    cid: CellId,
    lib_c: &LibertyCell,
    psw_in_sn: Option<SupplyNetId>,
    psw_out_sn: Option<SupplyNetId>,
    psw_out_pg: Option<&PgPinDef>,
    cell_upf: &str,
    dom_name: &Option<String>,
    rtl_loc: &Option<String>,
    out: &mut Vec<LpViolation>,
) {
    if let (Some(in_sn), Some(out_sn)) = (psw_in_sn, psw_out_sn) {
        if in_sn == out_sn {
            push_violation(
                out,
                "PG_SHORT",
                "pg",
                dom_name.clone(),
                None,
                cell_upf.to_string(),
                format!(
                    "power switch `{cell_upf}` shorts input and output supply rails onto `{}`",
                    ctx.sn_name(in_sn)
                ),
                rtl_loc.clone(),
            );
        }
    } else {
        push_violation(
            out,
            "PG_SWITCH_CHAIN_BROKEN",
            "pg",
            dom_name.clone(),
            None,
            cell_upf.to_string(),
            format!(
                "power switch `{cell_upf}` ({}) is missing a valid input or output supply rail connection",
                lib_c.name
            ),
            rtl_loc.clone(),
        );
        return;
    }

    let switch_pins: Vec<&str> = lib_c
        .pins
        .iter()
        .filter(|p| p.is_switch_pin)
        .map(|p| p.name.as_str())
        .collect();
    let mut const_env: HashMap<&str, bool> = HashMap::new();
    let mut any_const = false;
    for &sp_name in &switch_pins {
        let pin_opt = ctx
            .graph
            .cell(cid)
            .pins
            .iter()
            .copied()
            .find(|&pid| ctx.graph.pin(pid).name == sp_name);
        let Some(pid) = pin_opt else {
            push_violation(
                out,
                "PG_SWITCH_CHAIN_BROKEN",
                "pg",
                dom_name.clone(),
                None,
                format!("{cell_upf}/{sp_name}"),
                format!(
                    "power switch control pin `{sp_name}` on `{cell_upf}` is unconnected"
                ),
                rtl_loc.clone(),
            );
            continue;
        };
        if let Some(const_bit) = pin_const_value(ctx.graph, pid) {
            any_const = true;
            const_env.insert(sp_name, const_bit);
        }
    }

    if any_const {
        let sw_fn = psw_out_pg.and_then(|p| p.switch_function.as_deref());
        let stuck_on = sw_fn.and_then(|f| eval_lib_bool(f, &const_env));
        let out_ss = psw_out_sn.and_then(|sn| ctx.ss_by_power_net.get(&sn).copied());
        let has_off_state = out_ss.is_some_and(|ss| sat.can_be_off_while_off(ss, ss).is_some());
        let stuck_violates = match stuck_on {
            Some(false) => true,
            Some(true) => has_off_state,
            None => true,
        };
        if stuck_violates {
            push_violation(
                out,
                "PG_SWITCH_CHAIN_BROKEN",
                "pg",
                dom_name.clone(),
                None,
                cell_upf.to_string(),
                format!(
                    "power switch `{cell_upf}` ({}) has constant control pin binding ({const_env:?}) locking switch_function `{}` permanently {}",
                    lib_c.name,
                    sw_fn.unwrap_or("?"),
                    if stuck_on == Some(false) { "OFF" } else { "ON" }
                ),
                rtl_loc.clone(),
            );
        }
    }
}

fn pin_const_value(graph: &StructuralGraph, pid: PinId) -> Option<bool> {
    let pin = graph.pin(pid);
    if pin.unconnected || pin.external_conns.is_empty() {
        return None;
    }
    let bnd = &pin.external_conns[0];
    if let ExprOpSummary::Const {
        is_zero,
        is_all_ones,
    } = bnd.expr_op
    {
        if is_zero {
            return Some(false);
        }
        if is_all_ones {
            return Some(true);
        }
    }
    if let Some(pnet) = bnd.parent_net {
        let net = graph.net(pnet);
        if net.drivers.len() == 1 {
            if let ExprOpSummary::Const {
                is_zero,
                is_all_ones,
            } = net.drivers[0].op
            {
                if is_zero {
                    return Some(false);
                }
                if is_all_ones {
                    return Some(true);
                }
            }
        }
    }
    None
}
