//! Supply-set power state normalization, UPF 1.0 `add_port_state` bridging,
//! and implicit supply-set state inheritance.

use std::collections::BTreeMap;

use crate::ir::{
    DiagSeverity, PowerDomainId, PowerIntent, PowerStateDef, SimState, SupplyNetId,
    SupplyRailState, SupplySetId, UpfDiag, VoltageSpec,
};

use super::supply_graph::{lookup_scoped, resolve_ss_handle};

fn push_or_update_power_state(list: &mut Vec<PowerStateDef>, st: &PowerStateDef) {
    if st.update {
        if let Some(prev) = list.iter_mut().find(|p| p.state_name == st.state_name) {
            if st.simstate.is_some() {
                prev.simstate = st.simstate;
            }
            if st.supply_expr.is_some() {
                prev.supply_expr = st.supply_expr.clone();
                prev.rail_states = st.rail_states.clone();
            } else if !st.rail_states.is_empty() {
                prev.rail_states = st.rail_states.clone();
            }
            if st.logic_expr.is_some() {
                prev.logic_expr = st.logic_expr.clone();
                prev.scope = st.scope.clone();
            }
            if let Some(ill) = st.illegal_override {
                prev.illegal = ill;
            } else if st.illegal {
                prev.illegal = true;
            }
            if st.complete {
                prev.complete = true;
            }
            if st.loc.is_some() {
                prev.loc = st.loc.clone();
            }
            return;
        }
    }
    list.push(st.clone());
}

pub(crate) fn normalize_supply_states(
    intent: &mut PowerIntent,
    domain_by_name: &BTreeMap<String, PowerDomainId>,
    ss_by_name: &BTreeMap<String, SupplySetId>,
    ss_power_net: &[Option<SupplyNetId>],
    ss_ground_net: &[Option<SupplyNetId>],
) -> (Vec<Vec<PowerStateDef>>, Vec<PowerStateDef>) {
    let mut ss_states: Vec<Vec<PowerStateDef>> = vec![Vec::new(); intent.supply_sets.len()];
    let mut global_illegal = Vec::new();

    for st in &intent.power_states {
        if let Some(ssid) = resolve_ss_handle(
            &st.target,
            &st.scope,
            &intent.domains,
            domain_by_name,
            ss_by_name,
        ) {
            push_or_update_power_state(&mut ss_states[ssid.0 as usize], st);
        } else if let Some(pd_id) = lookup_scoped(domain_by_name, &st.target, &st.scope) {
            if let Some(prim) = &intent.domains[pd_id.0 as usize].primary_supply_set {
                let pd_scope = &intent.domains[pd_id.0 as usize].scope;
                if let Some(ssid) =
                    resolve_ss_handle(prim, pd_scope, &intent.domains, domain_by_name, ss_by_name)
                {
                    push_or_update_power_state(&mut ss_states[ssid.0 as usize], st);
                    continue;
                }
            }
            if st.illegal {
                global_illegal.push(st.clone());
            } else {
                intent.diagnostics.push(UpfDiag {
                    rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!(
                        "add_power_state: target `{}` has no resolved supply set",
                        st.target
                    ),
                    loc: st.loc.clone(),
                });
            }
        } else if st.illegal {
            global_illegal.push(st.clone());
        } else {
            intent.diagnostics.push(UpfDiag {
                rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                severity: DiagSeverity::Error,
                message: format!("add_power_state: unknown target `{}`", st.target),
                loc: st.loc.clone(),
            });
        }
    }

    for (idx, ss) in intent.supply_sets.iter().enumerate() {
        if !ss_states[idx].is_empty() {
            continue;
        }
        let Some(p_nid) = ss_power_net[idx] else {
            continue;
        };
        let p_states = &intent.supply_nets[p_nid.0 as usize].port_states;
        if p_states.is_empty() {
            continue;
        }
        let g_states_opt = ss_ground_net[idx]
            .map(|g_nid| &intent.supply_nets[g_nid.0 as usize].port_states)
            .filter(|v| !v.is_empty());

        match g_states_opt {
            Some(g_states) => {
                for ps in p_states {
                    for gs in g_states {
                        let simstate = if ps.is_off || gs.is_off {
                            SimState::Corrupt
                        } else if ps.voltage == VoltageSpec::Undetermined {
                            SimState::NotNormal
                        } else {
                            SimState::Normal
                        };
                        let supply_expr =
                            format!("power == {} && ground == {}", ps.state_name, gs.state_name);
                        ss_states[idx].push(PowerStateDef {
                            target: ss.name.clone(),
                            scope: ss.scope.clone(),
                            state_name: format!("{}_{}", ps.state_name, gs.state_name),
                            simstate: Some(simstate),
                            supply_expr: Some(supply_expr),
                            logic_expr: None,
                            rail_states: vec![
                                SupplyRailState {
                                    function: "power".to_string(),
                                    is_off: ps.is_off,
                                    voltage: ps.voltage,
                                },
                                SupplyRailState {
                                    function: "ground".to_string(),
                                    is_off: gs.is_off,
                                    voltage: gs.voltage,
                                },
                            ],
                            illegal: false,
                            illegal_override: None,
                            complete: false,
                            update: false,
                            loc: ps.loc.clone(),
                        });
                    }
                }
            }
            None => {
                for ps in p_states {
                    let simstate = if ps.is_off {
                        SimState::Corrupt
                    } else if ps.voltage == VoltageSpec::Undetermined {
                        SimState::NotNormal
                    } else {
                        SimState::Normal
                    };
                    let supply_expr = format!("power == {}", ps.state_name);
                    ss_states[idx].push(PowerStateDef {
                        target: ss.name.clone(),
                        scope: ss.scope.clone(),
                        state_name: ps.state_name.clone(),
                        simstate: Some(simstate),
                        supply_expr: Some(supply_expr),
                        logic_expr: None,
                        rail_states: vec![SupplyRailState {
                            function: "power".to_string(),
                            is_off: ps.is_off,
                            voltage: ps.voltage,
                        }],
                        illegal: false,
                        illegal_override: None,
                        complete: false,
                        update: false,
                        loc: ps.loc.clone(),
                    });
                }
            }
        }
    }

    let mut bridged: Vec<(usize, Vec<PowerStateDef>)> = Vec::new();
    for (idx, ss) in intent.supply_sets.iter().enumerate() {
        if !ss.is_implicit || !ss_states[idx].is_empty() {
            continue;
        }
        let Some(p_nid) = ss_power_net[idx] else {
            continue;
        };
        let g_nid_opt = ss_ground_net[idx];
        let donor_idx = (0..intent.supply_sets.len())
            .filter(|&d| d != idx && !ss_states[d].is_empty() && ss_power_net[d] == Some(p_nid))
            .min_by_key(|&d| {
                let exact_gnd = ss_ground_net[d] == g_nid_opt;
                let compatible_gnd = g_nid_opt.is_none() || exact_gnd;
                (!exact_gnd, !compatible_gnd, d)
            });

        if let Some(d_idx) = donor_idx {
            let mut cloned = Vec::new();
            for st in &ss_states[d_idx] {
                if st.illegal {
                    continue;
                }
                let mut copy = st.clone();
                copy.target = ss.name.clone();
                if copy.logic_expr.is_none() {
                    copy.scope = ss.scope.clone();
                }
                if g_nid_opt.is_none() {
                    copy.rail_states.retain(|r| r.function != "ground");
                }
                cloned.push(copy);
            }
            if !cloned.is_empty() {
                bridged.push((idx, cloned));
                continue;
            }
        }

        if let Some(nom_v) = intent.supply_nets[p_nid.0 as usize].nominal_voltage {
            if nom_v.is_on_known() {
                bridged.push((
                    idx,
                    vec![PowerStateDef {
                        target: ss.name.clone(),
                        scope: ss.scope.clone(),
                        state_name: "ON".to_string(),
                        simstate: Some(SimState::Normal),
                        supply_expr: None,
                        logic_expr: None,
                        rail_states: vec![SupplyRailState {
                            function: "power".to_string(),
                            is_off: false,
                            voltage: nom_v,
                        }],
                        illegal: false,
                        illegal_override: None,
                        complete: false,
                        update: false,
                        loc: ss.loc.clone(),
                    }],
                ));
            }
        }
    }
    for (idx, states) in bridged {
        ss_states[idx] = states;
    }

    (ss_states, global_illegal)
}
