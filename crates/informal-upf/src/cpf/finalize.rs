//! CPF finalization: lowers nominal conditions, power modes, and domain shutoff rules
//! into canonical `PowerStateDef`, `IllegalStateCombination`, and `PstTable` IR.

use std::collections::{BTreeMap, BTreeSet};

use crate::expr_bv::qualify_logic_expr_to_root;
use crate::ir::{
    IllegalStateCombination, PowerIntent, PowerStateDef, SimState, SupplyRailState, UpfLoc,
    VoltageSpec, UV_PER_VOLT,
};
use crate::resolve::resolve_power_state_target_ss;

use super::finalize_pst::{
    cpf_global_illegal_target, remove_cpf_illegal_entries, remove_cpf_pst_row, upsert_cpf_pst_row,
};
use super::{
    cpf_ss_name, ensure_cpf_domain_supply_set, negate_bool_expr, resolve_nc_in_map, CpfCondState,
    CpfState, NominalCondition,
};

fn format_voltage_supply_expr(v: VoltageSpec) -> String {
    match v {
        VoltageSpec::Off => "power == OFF && ground == {FULL_ON 0.0}".to_string(),
        VoltageSpec::Nominal(uv) => {
            let volts = format_uv_decimal(uv);
            format!("power == {{FULL_ON {volts}}} && ground == {{FULL_ON 0.0}}")
        }
        VoltageSpec::Range(lo, hi) => {
            format!(
                "power == {{PARTIAL_ON {} {}}} && ground == {{FULL_ON 0.0}}",
                format_uv_decimal(lo),
                format_uv_decimal(hi)
            )
        }
        VoltageSpec::Undetermined => "power == UNDETERMINED && ground == {FULL_ON 0.0}".to_string(),
    }
}

fn format_uv_decimal(uv: i64) -> String {
    let neg = uv < 0;
    let mag = uv.unsigned_abs();
    let whole = mag / (UV_PER_VOLT as u64);
    let frac = mag % (UV_PER_VOLT as u64);
    if frac == 0 {
        format!("{}{}.0", if neg { "-" } else { "" }, whole)
    } else {
        let frac_str = format!("{frac:06}");
        let trimmed = frac_str.trim_end_matches('0');
        format!("{}{}.{}", if neg { "-" } else { "" }, whole, trimmed)
    }
}

pub(crate) fn cpf_domain_nominal_voltage(
    intent: &PowerIntent,
    pwr_net_name: Option<&str>,
) -> VoltageSpec {
    pwr_net_name
        .and_then(|pn| {
            let key = pn.trim_start_matches('/');
            intent.supply_nets.iter().find(|n| n.name == key)
        })
        .and_then(|n| n.nominal_voltage)
        .unwrap_or(VoltageSpec::Undetermined)
}

pub(crate) fn cpf_domain_fallback_state_names(
    intent: &PowerIntent,
    domain_enable: &BTreeMap<String, (String, String)>,
    domain_shutoff: &BTreeMap<String, (String, String)>,
    domain: &crate::ir::PowerDomain,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let dom_key = domain.name.trim_start_matches('/');
    if domain_enable.contains_key(dom_key) || domain_shutoff.contains_key(dom_key) {
        names.insert("ON".to_string());
        names.insert("OFF".to_string());
    } else if cpf_domain_nominal_voltage(intent, domain.legacy_primary_power_net.as_deref())
        .is_on_known()
    {
        names.insert("ON".to_string());
    }
    names
}

/// Lowers CPF nominal conditions, power modes, and domain shutoff rules into
/// canonical `PowerStateDef`s and a `PstTable` inside `PowerIntent`.
pub(crate) fn finalize_cpf(intent: &mut PowerIntent, cs: &mut CpfState) {
    finalize_cpf_ex(intent, cs, true, None);
}

fn target_matches_ss(intent: &PowerIntent, s: &PowerStateDef, norm_ss: &str) -> bool {
    resolve_power_state_target_ss(intent, &s.target, &s.scope).trim_start_matches('/') == norm_ss
}

fn retain_power_states_by<F>(intent: &mut PowerIntent, mut keep_pred: F)
where
    F: FnMut(&PowerStateDef, &str) -> bool,
{
    let keep: Vec<bool> = intent
        .power_states
        .iter()
        .map(|s| {
            let ss = resolve_power_state_target_ss(intent, &s.target, &s.scope);
            keep_pred(s, ss.trim_start_matches('/'))
        })
        .collect();
    if keep.iter().any(|&k| !k) {
        let mut idx = 0;
        intent.power_states.retain(|_| {
            let k = keep[idx];
            idx += 1;
            k
        });
    }
}

fn collect_deferred_fallback_names(
    intent: &PowerIntent,
    domain_enable: &BTreeMap<String, (String, String)>,
    domain_shutoff: &BTreeMap<String, (String, String)>,
    norm_ss: &str,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for d in &intent.domains {
        if cpf_ss_name(&d.name).trim_start_matches('/') == norm_ss {
            names.extend(cpf_domain_fallback_state_names(
                intent,
                domain_enable,
                domain_shutoff,
                d,
            ));
        }
    }
    names
}

pub(crate) fn finalize_cpf_ex(
    intent: &mut PowerIntent,
    cs: &mut CpfState,
    allow_fallback: bool,
    defer_other_scopes: Option<&str>,
) {
    if cs.finalized
        && cs.updated_nominal_conditions.is_empty()
        && cs.updated_domain_conditions.is_empty()
        && (!allow_fallback || cs.fallback_applied)
    {
        return;
    }
    if allow_fallback {
        cs.fallback_applied = true;
    }

    let mut seen_states: BTreeSet<(String, String)> = intent
        .power_states
        .iter()
        .filter(|s| !s.update)
        .map(|s| {
            let norm_target = resolve_power_state_target_ss(intent, &s.target, &s.scope)
                .trim_start_matches('/')
                .to_string();
            (norm_target, s.state_name.clone())
        })
        .collect();

    let wrap_paren = |s: &str| {
        if s.contains('\\') && !s.ends_with(char::is_whitespace) {
            format!("({s} )")
        } else {
            format!("({s})")
        }
    };
    let nc_is_updated = |updated_ncs: &BTreeSet<(String, String)>,
                         ncs: &BTreeMap<(String, String), NominalCondition>,
                         nc_name: &str,
                         pm_scope: &str| {
        let key = nc_name.trim().trim_start_matches('/');
        resolve_nc_in_map(ncs, pm_scope, nc_name)
            .is_some_and(|(def_sc, _)| updated_ncs.contains(&(def_sc.to_string(), key.to_string())))
    };
    let active_mode_refs: BTreeSet<(String, String)> = cs
        .power_modes
        .iter()
        .filter(|m| !m.illegal)
        .flat_map(|m| {
            m.domain_conditions.iter().map(|(pd, nc)| {
                (
                    cpf_ss_name(pd).trim_start_matches('/').to_string(),
                    nc.clone(),
                )
            })
        })
        .collect();
    let mut deferred_any = false;
    for pm in &mut cs.power_modes {
        if !pm.emitted && defer_other_scopes.is_some_and(|sc| pm.scope != sc) {
            deferred_any = true;
            continue;
        }
        let pm_needs_refresh = pm.domain_conditions.iter().any(|(pd, nc)| {
            nc_is_updated(
                &cs.updated_nominal_conditions,
                &cs.nominal_conditions,
                nc,
                &pm.scope,
            ) || cs
                .updated_domain_conditions
                .contains(pd.trim_start_matches('/'))
        });
        if pm.emitted && !pm_needs_refresh {
            continue;
        }
        pm.emitted = true;
        if pm.illegal {
            remove_cpf_pst_row(intent, pm);
            for (pd, nc_name) in &pm.domain_conditions {
                let norm_ss = cpf_ss_name(pd).trim_start_matches('/').to_string();
                let key = (norm_ss.clone(), nc_name.clone());
                if cs.emitted_mode_states.contains(&key)
                    && !active_mode_refs.contains(&key)
                    && !cs.emitted_fallback_states.contains(&key)
                {
                    if let Some(pos) = intent.power_states.iter().position(|s| {
                        !s.update
                            && !s.illegal
                            && s.state_name == *nc_name
                            && target_matches_ss(intent, s, &norm_ss)
                    }) {
                        intent.power_states.remove(pos);
                    }
                    cs.emitted_mode_states.remove(&key);
                    if !intent.power_states.iter().any(|s| {
                        !s.update
                            && s.state_name == *nc_name
                            && target_matches_ss(intent, s, &norm_ss)
                    }) {
                        seen_states.remove(&key);
                    }
                    if !allow_fallback {
                        cs.fallback_applied = false;
                    }
                }
            }
            let mut logic_parts = Vec::new();
            if let Some(cond) = &pm.condition {
                let q = qualify_logic_expr_to_root(cond, &pm.condition_scope);
                logic_parts.push(wrap_paren(&q));
            }
            for (pd, nc_name) in &pm.domain_conditions {
                let dom_key = pd.trim_start_matches('/');
                if let Some((_, nc)) = resolve_nc_in_map(&cs.nominal_conditions, &pm.scope, nc_name)
                {
                    if let Some((en_cond, en_scope)) = cs.domain_enable.get(dom_key) {
                        let raw = if matches!(nc.state, CpfCondState::Off) {
                            negate_bool_expr(en_cond)
                        } else {
                            en_cond.clone()
                        };
                        let q = qualify_logic_expr_to_root(&raw, en_scope);
                        logic_parts.push(wrap_paren(&q));
                    } else if let Some((off_cond, off_scope)) = cs.domain_shutoff.get(dom_key) {
                        let raw = if matches!(nc.state, CpfCondState::Off) {
                            off_cond.clone()
                        } else {
                            negate_bool_expr(off_cond)
                        };
                        let q = qualify_logic_expr_to_root(&raw, off_scope);
                        logic_parts.push(wrap_paren(&q));
                    }
                }
            }
            if !pm.domain_conditions.is_empty() {
                let mut domain_states = Vec::with_capacity(pm.domain_conditions.len());
                for (pd, nc_name) in &pm.domain_conditions {
                    let ss_name = ensure_cpf_domain_supply_set(intent, pd, "", pm.loc.clone());
                    domain_states.push((ss_name, nc_name.clone()));
                }
                remove_cpf_illegal_entries(intent, pm);
                intent.illegal_combinations.push(IllegalStateCombination {
                    mode_name: pm.name.clone(),
                    scope: pm.condition_scope.clone(),
                    source_scope: pm.scope.clone(),
                    design_top: pm.design_top.clone(),
                    domain_states,
                    logic_expr: pm.condition.clone(),
                    fallback_logic_expr: if logic_parts.is_empty() {
                        None
                    } else {
                        Some(logic_parts.join(" && "))
                    },
                    loc: pm.loc.clone(),
                });
            } else {
                remove_cpf_illegal_entries(intent, pm);
                if !logic_parts.is_empty() {
                    let global_target =
                        cpf_global_illegal_target(&pm.scope, pm.design_top.as_deref());
                    intent.power_states.push(PowerStateDef {
                        target: global_target,
                        scope: pm.condition_scope.clone(),
                        state_name: pm.name.clone(),
                        simstate: None,
                        supply_expr: None,
                        logic_expr: Some(logic_parts.join(" && ")),
                        rail_states: Vec::new(),
                        illegal: true,
                        illegal_override: None,
                        complete: false,
                        update: false,
                        loc: pm.loc.clone(),
                    });
                }
            }
            continue;
        }
        remove_cpf_illegal_entries(intent, pm);

        let mut ss_cond_map: BTreeMap<String, String> = BTreeMap::new();
        for (pd, nc_name) in &pm.domain_conditions {
            let ss_name = ensure_cpf_domain_supply_set(intent, pd, "", pm.loc.clone());
            let norm_ss = ss_name.trim_start_matches('/');
            let has_emitted_fb = cs
                .emitted_fallback_states
                .contains(&(norm_ss.to_string(), "ON".to_string()))
                || cs
                    .emitted_fallback_states
                    .contains(&(norm_ss.to_string(), "OFF".to_string()));
            if has_emitted_fb {
                let deferred_names = collect_deferred_fallback_names(
                    intent,
                    &cs.domain_enable,
                    &cs.domain_shutoff,
                    norm_ss,
                );
                retain_power_states_by(intent, |s, s_norm_ss| {
                    !(s_norm_ss == norm_ss
                        && (((!s.illegal || s.update)
                            && cs
                                .emitted_fallback_states
                                .contains(&(norm_ss.to_string(), s.state_name.clone())))
                            || (s.update && deferred_names.contains(&s.state_name))))
                });
                for name in ["ON", "OFF"] {
                    if cs
                        .emitted_fallback_states
                        .remove(&(norm_ss.to_string(), name.to_string()))
                    {
                        seen_states.remove(&(norm_ss.to_string(), name.to_string()));
                    }
                }
            } else if cs.cpf_ss_names.contains(norm_ss)
                && !intent
                    .power_states
                    .iter()
                    .any(|s| !s.illegal && !s.update && target_matches_ss(intent, s, norm_ss))
            {
                let deferred_names = collect_deferred_fallback_names(
                    intent,
                    &cs.domain_enable,
                    &cs.domain_shutoff,
                    norm_ss,
                );
                if !deferred_names.is_empty() {
                    retain_power_states_by(intent, |s, s_norm_ss| {
                        !(s.update
                            && deferred_names.contains(&s.state_name)
                            && s_norm_ss == norm_ss)
                    });
                    for name in &deferred_names {
                        seen_states.remove(&(norm_ss.to_string(), name.clone()));
                    }
                }
            }
            ss_cond_map.insert(ss_name.clone(), nc_name.clone());

            let dom_key = pd.trim_start_matches('/');
            let inserted = seen_states.insert((norm_ss.to_string(), nc_name.clone()));
            let should_refresh = !inserted
                && (nc_is_updated(
                    &cs.updated_nominal_conditions,
                    &cs.nominal_conditions,
                    nc_name,
                    &pm.scope,
                ) || cs.updated_domain_conditions.contains(dom_key));
            if inserted || should_refresh {
                let nc_opt = resolve_nc_in_map(&cs.nominal_conditions, &pm.scope, nc_name)
                    .map(|(_, nc)| nc.clone());
                let (voltage, simstate, pmos_bias, nmos_bias, loc) = match nc_opt {
                    Some(nc) => {
                        let ss = if matches!(nc.state, CpfCondState::Off) || nc.voltage.is_off() {
                            SimState::Corrupt
                        } else {
                            SimState::Normal
                        };
                        (
                            nc.voltage,
                            ss,
                            nc.pmos_bias,
                            nc.nmos_bias,
                            nc.loc.or_else(|| pm.loc.clone()),
                        )
                    }
                    None => (
                        VoltageSpec::Undetermined,
                        SimState::Normal,
                        None,
                        None,
                        pm.loc.clone(),
                    ),
                };

                let (logic_expr, expr_scope) =
                    if let Some((en_cond, sc)) = cs.domain_enable.get(dom_key) {
                        let expr = if simstate.is_normal() {
                            en_cond.clone()
                        } else {
                            negate_bool_expr(en_cond)
                        };
                        (Some(expr), sc.clone())
                    } else if let Some((off_cond, sc)) = cs.domain_shutoff.get(dom_key) {
                        let expr = if simstate.is_normal() {
                            negate_bool_expr(off_cond)
                        } else {
                            off_cond.clone()
                        };
                        (Some(expr), sc.clone())
                    } else {
                        let pd_scope = intent
                            .domains
                            .iter()
                            .find(|d| d.name == dom_key)
                            .map(|d| d.scope.clone())
                            .unwrap_or_else(|| pm.scope.clone());
                        (None, pd_scope)
                    };

                let supply_expr = format_voltage_supply_expr(voltage);
                let mut rail_states = vec![
                    SupplyRailState {
                        function: "power".to_string(),
                        is_off: voltage.is_off(),
                        voltage,
                    },
                    SupplyRailState {
                        function: "ground".to_string(),
                        is_off: false,
                        voltage: VoltageSpec::Nominal(0),
                    },
                ];
                if let Some(nw) = pmos_bias {
                    rail_states.push(SupplyRailState {
                        function: "nwell".to_string(),
                        is_off: nw.is_off(),
                        voltage: nw,
                    });
                }
                if let Some(pw) = nmos_bias {
                    rail_states.push(SupplyRailState {
                        function: "pwell".to_string(),
                        is_off: pw.is_off(),
                        voltage: pw,
                    });
                }
                let def = PowerStateDef {
                    target: ss_name.clone(),
                    scope: expr_scope,
                    state_name: nc_name.clone(),
                    simstate: Some(simstate),
                    supply_expr: Some(supply_expr),
                    logic_expr,
                    rail_states,
                    illegal: false,
                    illegal_override: None,
                    complete: false,
                    update: false,
                    loc,
                };
                retain_power_states_by(intent, |s, s_norm_ss| {
                    !(s.update && s.state_name == *nc_name && s_norm_ss == norm_ss)
                });
                if inserted {
                    intent.power_states.push(def);
                    cs.emitted_mode_states
                        .insert((norm_ss.to_string(), nc_name.clone()));
                } else if let Some(pos) = intent.power_states.iter().position(|s| {
                    !s.illegal
                        && !s.update
                        && s.state_name == *nc_name
                        && target_matches_ss(intent, s, norm_ss)
                }) {
                    intent.power_states[pos] = def;
                }
            }
        }

        upsert_cpf_pst_row(intent, pm, &ss_cond_map);
    }
    cs.finalized = !deferred_any;
    cs.updated_nominal_conditions.clear();

    if !allow_fallback {
        cs.updated_domain_conditions.retain(|d| {
            let ss = cpf_ss_name(d);
            let norm_ss = ss.trim_start_matches('/');
            cs.emitted_fallback_states
                .contains(&(norm_ss.to_string(), "ON".to_string()))
                || cs
                    .emitted_fallback_states
                    .contains(&(norm_ss.to_string(), "OFF".to_string()))
        });
        return;
    }

    let domain_snapshots: Vec<(String, String, Option<String>, Option<UpfLoc>)> = intent
        .domains
        .iter()
        .map(|d| {
            (
                d.name.clone(),
                d.scope.clone(),
                d.legacy_primary_power_net.clone(),
                d.loc.clone(),
            )
        })
        .collect();
    for (pd_name, pd_scope, pwr_net_name, pd_loc) in domain_snapshots {
        let ss_name = cpf_ss_name(&pd_name);
        let norm_ss = ss_name.trim_start_matches('/');
        let has_explicit_mode = cs.power_modes.iter().any(|pm| {
            !pm.illegal
                && pm
                    .domain_conditions
                    .iter()
                    .any(|(d, _)| cpf_ss_name(d).trim_start_matches('/') == norm_ss)
        });
        if !has_explicit_mode && cs.updated_domain_conditions.contains(&pd_name) {
            retain_power_states_by(intent, |s, s_norm_ss| {
                !(s_norm_ss == norm_ss
                    && !s.illegal
                    && !s.update
                    && cs
                        .emitted_fallback_states
                        .contains(&(norm_ss.to_string(), s.state_name.clone())))
            });
            cs.emitted_fallback_states
                .remove(&(norm_ss.to_string(), "ON".to_string()));
            cs.emitted_fallback_states
                .remove(&(norm_ss.to_string(), "OFF".to_string()));
        }
        let has_any = intent
            .power_states
            .iter()
            .any(|s| !s.illegal && !s.update && target_matches_ss(intent, s, norm_ss));
        if has_any {
            continue;
        }
        let insert_pos = intent
            .power_states
            .iter()
            .position(|s| s.update && target_matches_ss(intent, s, norm_ss));
        let nom_v = cpf_domain_nominal_voltage(intent, pwr_net_name.as_deref());
        let fallback_defs = if let Some((en_cond, sc)) = cs.domain_enable.get(&pd_name) {
            let off_cond = negate_bool_expr(en_cond);
            cs.emitted_fallback_states
                .insert((norm_ss.to_string(), "ON".to_string()));
            cs.emitted_fallback_states
                .insert((norm_ss.to_string(), "OFF".to_string()));
            vec![
                PowerStateDef {
                    target: ss_name.clone(),
                    scope: sc.clone(),
                    state_name: "ON".to_string(),
                    simstate: Some(SimState::Normal),
                    supply_expr: Some(format_voltage_supply_expr(nom_v)),
                    logic_expr: Some(en_cond.clone()),
                    rail_states: vec![
                        SupplyRailState {
                            function: "power".to_string(),
                            is_off: false,
                            voltage: nom_v,
                        },
                        SupplyRailState {
                            function: "ground".to_string(),
                            is_off: false,
                            voltage: VoltageSpec::Nominal(0),
                        },
                    ],
                    illegal: false,
                    illegal_override: None,
                    complete: false,
                    update: false,
                    loc: pd_loc.clone(),
                },
                PowerStateDef {
                    target: ss_name,
                    scope: sc.clone(),
                    state_name: "OFF".to_string(),
                    simstate: Some(SimState::Corrupt),
                    supply_expr: Some(format_voltage_supply_expr(VoltageSpec::Off)),
                    logic_expr: Some(off_cond),
                    rail_states: vec![
                        SupplyRailState {
                            function: "power".to_string(),
                            is_off: true,
                            voltage: VoltageSpec::Off,
                        },
                        SupplyRailState {
                            function: "ground".to_string(),
                            is_off: false,
                            voltage: VoltageSpec::Nominal(0),
                        },
                    ],
                    illegal: false,
                    illegal_override: None,
                    complete: false,
                    update: false,
                    loc: pd_loc,
                },
            ]
        } else if let Some((off_cond, sc)) = cs.domain_shutoff.get(&pd_name) {
            let on_cond = negate_bool_expr(off_cond);
            cs.emitted_fallback_states
                .insert((norm_ss.to_string(), "ON".to_string()));
            cs.emitted_fallback_states
                .insert((norm_ss.to_string(), "OFF".to_string()));
            vec![
                PowerStateDef {
                    target: ss_name.clone(),
                    scope: sc.clone(),
                    state_name: "ON".to_string(),
                    simstate: Some(SimState::Normal),
                    supply_expr: Some(format_voltage_supply_expr(nom_v)),
                    logic_expr: Some(on_cond),
                    rail_states: vec![
                        SupplyRailState {
                            function: "power".to_string(),
                            is_off: false,
                            voltage: nom_v,
                        },
                        SupplyRailState {
                            function: "ground".to_string(),
                            is_off: false,
                            voltage: VoltageSpec::Nominal(0),
                        },
                    ],
                    illegal: false,
                    illegal_override: None,
                    complete: false,
                    update: false,
                    loc: pd_loc.clone(),
                },
                PowerStateDef {
                    target: ss_name,
                    scope: sc.clone(),
                    state_name: "OFF".to_string(),
                    simstate: Some(SimState::Corrupt),
                    supply_expr: Some(format_voltage_supply_expr(VoltageSpec::Off)),
                    logic_expr: Some(off_cond.clone()),
                    rail_states: vec![
                        SupplyRailState {
                            function: "power".to_string(),
                            is_off: true,
                            voltage: VoltageSpec::Off,
                        },
                        SupplyRailState {
                            function: "ground".to_string(),
                            is_off: false,
                            voltage: VoltageSpec::Nominal(0),
                        },
                    ],
                    illegal: false,
                    illegal_override: None,
                    complete: false,
                    update: false,
                    loc: pd_loc,
                },
            ]
        } else if nom_v.is_on_known() {
            cs.emitted_fallback_states
                .insert((norm_ss.to_string(), "ON".to_string()));
            vec![PowerStateDef {
                target: ss_name,
                scope: pd_scope,
                state_name: "ON".to_string(),
                simstate: Some(SimState::Normal),
                supply_expr: Some(format_voltage_supply_expr(nom_v)),
                logic_expr: None,
                rail_states: vec![
                    SupplyRailState {
                        function: "power".to_string(),
                        is_off: false,
                        voltage: nom_v,
                    },
                    SupplyRailState {
                        function: "ground".to_string(),
                        is_off: false,
                        voltage: VoltageSpec::Nominal(0),
                    },
                ],
                illegal: false,
                illegal_override: None,
                complete: false,
                update: false,
                loc: pd_loc,
            }]
        } else {
            Vec::new()
        };
        if !fallback_defs.is_empty() {
            if let Some(pos) = insert_pos {
                intent.power_states.splice(pos..pos, fallback_defs);
            } else {
                intent.power_states.extend(fallback_defs);
            }
        }
    }
    cs.updated_domain_conditions.clear();

    for sw in &mut intent.power_switches {
        if sw.output_supply_ports.is_empty()
            && sw
                .on_states
                .iter()
                .any(|s| s.supply_port.as_deref() == Some("vdd_in"))
        {
            let dom_key = sw.domain.trim_start_matches('/');
            if let Some(pd) = intent.domains.iter().find(|d| d.name == dom_key) {
                if let Some(ref pwr) = pd.legacy_primary_power_net {
                    sw.output_supply_ports
                        .push(("vdd_out".to_string(), pwr.clone()));
                }
            }
        }
    }
}
