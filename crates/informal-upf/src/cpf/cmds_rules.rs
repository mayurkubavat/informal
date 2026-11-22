//! CPF switch, isolation, level-shifter, and state-retention rule commands (`C1`, `Fix 9`).

use std::collections::BTreeMap;

use informal_tcl::Value;

use crate::expr_bv::qualify_logic_expr_in_scope;
use crate::ir::{
    normalize_scope, split_parent_leaf, split_tcl_list, trim_hier_path, AppliesTo, ClampValue,
    DeferredOp, DiagSeverity, IsoId, IsolationStrategy, LevelShifterStrategy, LsId, LsRule,
    PowerSwitchDef, PowerSwitchId, RetId, RetentionStrategy, Sense, StrategyLocation,
    SwitchStateExpr, UpfDiag, UpfLoc,
};
use crate::upf::{loc_err, UpfArgs, UpfEvalState};

use super::{negate_bool_expr, parse_cpf_signal_sense, qualify_cpf_bool_expr};

fn wrap_bool_atom(expr: &str) -> String {
    if super::is_compound_bool_expr(expr, '/') {
        if expr.contains('\\') && !expr.ends_with(char::is_whitespace) {
            format!("({expr} )")
        } else {
            format!("({expr})")
        }
    } else {
        expr.to_string()
    }
}

fn combine_bool_ands(parts: &[String]) -> String {
    if parts.len() == 1 {
        parts[0].clone()
    } else {
        parts
            .iter()
            .map(|p| wrap_bool_atom(p))
            .collect::<Vec<_>>()
            .join(" && ")
    }
}

pub(crate) fn cmd_create_power_switch_rule(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-name", 1),
            ("-domain", 1),
            ("-external_power_net", 1),
            ("-external_ground_net", 1),
            ("-enable_condition", 1),
            ("-enable_condition_1", 1),
            ("-enable_condition_2", 1),
            ("-acknowledge_receiver", 1),
        ],
        &[],
        &[("-peak_ir_drop_limit", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let raw_name = args
        .val("-name")
        .ok_or_else(|| loc_err(loc.as_ref(), "create_power_switch_rule: missing -name"))?;
    let name = st.qualify_decl(raw_name);
    let raw_domain = args
        .val("-domain")
        .ok_or_else(|| loc_err(loc.as_ref(), "create_power_switch_rule: missing -domain"))?;
    let domain = st.qualify_ref(raw_domain);
    let mut explicit_ports: Vec<(&str, String)> = Vec::new();
    for (flag, port_name) in [
        ("-enable_condition", "ctrl"),
        ("-enable_condition_1", "ctrl1"),
        ("-enable_condition_2", "ctrl2"),
    ] {
        if let Some(s) = args.val(flag) {
            explicit_ports.push((
                port_name,
                qualify_cpf_bool_expr(st, s.trim(), &st.current_scope),
            ));
        }
    }
    let explicit_enable = !explicit_ports.is_empty();
    let (en_q, shutoff_scope) = if explicit_enable {
        let exprs: Vec<String> = explicit_ports.iter().map(|(_, e)| e.clone()).collect();
        (Some(combine_bool_ands(&exprs)), None)
    } else if let Some((off, sc)) = st
        .cpf_state
        .domain_shutoff
        .get(domain.trim_start_matches('/'))
    {
        let negated = negate_bool_expr(off);
        let q = if sc.is_empty() {
            negated
        } else if sc == &st.current_scope {
            qualify_cpf_bool_expr(st, &negated, sc)
        } else {
            qualify_logic_expr_in_scope(&negated, sc, '/')
        };
        (Some(q), Some(sc.clone()))
    } else {
        (None, None)
    };
    if explicit_enable {
        if let Some(en) = &en_q {
            let domain_key = domain.trim_start_matches('/').to_string();
            st.cpf_state.domain_shutoff.remove(&domain_key);
            st.cpf_state
                .domain_enable
                .insert(domain_key.clone(), (en.clone(), String::new()));
            st.cpf_state.updated_domain_conditions.insert(domain_key);
        }
    }
    let ext_pwr = args.val("-external_power_net").map(|n| st.qualify_ref(n));
    let mut input_supply_ports = Vec::new();
    if let Some(p) = ext_pwr {
        input_supply_ports.push(("vdd_in".to_string(), p));
    }
    let mut control_ports = Vec::new();
    let mut control_port_scopes = BTreeMap::new();
    let mut shutoff_synthesized_ctrl = false;
    let mut on_states = Vec::new();
    let mut off_states = Vec::new();
    if let Some(en_q) = en_q {
        let off_q = negate_bool_expr(&en_q);
        if explicit_enable {
            for (port_name, expr) in explicit_ports {
                control_port_scopes.insert(port_name.to_string(), st.current_scope.clone());
                control_ports.push((port_name.to_string(), expr));
            }
        } else {
            control_port_scopes.insert(
                "ctrl".to_string(),
                shutoff_scope.unwrap_or_else(|| st.current_scope.clone()),
            );
            control_ports.push(("ctrl".to_string(), en_q.clone()));
            shutoff_synthesized_ctrl = true;
        }
        on_states.push(SwitchStateExpr {
            state_name: "ON".to_string(),
            supply_port: Some("vdd_in".to_string()),
            expr: en_q,
        });
        off_states.push(SwitchStateExpr {
            state_name: "OFF".to_string(),
            supply_port: None,
            expr: off_q,
        });
    }
    let ack_ports = args
        .val("-acknowledge_receiver")
        .map(|s| vec![("ack".to_string(), st.qualify_signal_ref(s))])
        .unwrap_or_default();

    let id = PowerSwitchId(st.intent.power_switches.len() as u32);
    st.intent.power_switches.push(PowerSwitchDef {
        id,
        name: name.clone(),
        scope: st.current_scope.clone(),
        domain,
        supply_set: None,
        input_supply_ports,
        output_supply_ports: Vec::new(),
        control_ports,
        control_port_scopes,
        shutoff_synthesized_ctrl,
        ack_ports,
        on_states,
        off_states,
        error_states: Vec::new(),
        mapped_cells: Vec::new(),
        loc,
    });
    st.cpf_state.finalized = false;
    st.cpf_state.fallback_applied = false;
    Ok(Value::Str(name))
}

fn find_power_switch_idx(st: &UpfEvalState, raw_name: &str) -> Option<usize> {
    let trimmed = trim_hier_path(raw_name);
    let sep = st.intent.hierarchy_separator;
    let is_abs = trimmed.starts_with('/') || (sep != '/' && sep != '.' && trimmed.starts_with(sep));
    let qualified = st.qualify(raw_name);
    let q_norm = qualified.trim_start_matches('/');
    if let Some(idx) = st
        .intent
        .power_switches
        .iter()
        .position(|s| s.name.trim_start_matches('/') == q_norm)
    {
        return Some(idx);
    }
    if !is_abs && !trimmed.starts_with("..") && !st.current_scope.is_empty() {
        let rel = normalize_scope(trimmed, sep);
        let local_key = rel
            .strip_prefix(&format!("{}/", st.current_scope))
            .unwrap_or(&rel);
        let mut cur = st.current_scope.as_str();
        while let Some((parent, _)) = split_parent_leaf(cur) {
            let cand = format!("{parent}/{local_key}");
            if let Some(idx) = st
                .intent
                .power_switches
                .iter()
                .position(|s| s.name.trim_start_matches('/') == cand)
            {
                return Some(idx);
            }
            cur = parent;
        }
        if let Some(idx) = st
            .intent
            .power_switches
            .iter()
            .position(|s| s.name.trim_start_matches('/') == local_key)
        {
            return Some(idx);
        }
    }
    None
}

pub(crate) fn cmd_update_power_switch_rule(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-name", 1),
            ("-enable_condition", 1),
            ("-enable_condition_1", 1),
            ("-enable_condition_2", 1),
            ("-acknowledge_receiver", 1),
            ("-cells", 1),
        ],
        &[],
        &[("-prefix", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let raw_name = args
        .val("-name")
        .ok_or_else(|| loc_err(loc.as_ref(), "update_power_switch_rule: missing -name"))?;
    let sw_idx = find_power_switch_idx(st, raw_name);
    let ack_qualified = args
        .val("-acknowledge_receiver")
        .map(|ack| st.qualify_signal_ref(ack));
    let mut en_updates: Vec<(&str, String)> = Vec::new();
    for (flag, port_name) in [
        ("-enable_condition", "ctrl"),
        ("-enable_condition_1", "ctrl1"),
        ("-enable_condition_2", "ctrl2"),
    ] {
        if let Some(en) = args.val(flag) {
            en_updates.push((
                port_name,
                qualify_cpf_bool_expr(st, en.trim(), &st.current_scope),
            ));
        }
    }
    let has_ctrl_update = en_updates.iter().any(|(p, _)| *p == "ctrl");
    let update_scope = st.current_scope.clone();
    if let Some(idx) = sw_idx {
        let psw = &mut st.intent.power_switches[idx];
        if !en_updates.is_empty() {
            let domain_key = psw.domain.trim_start_matches('/').to_string();
            if !has_ctrl_update
                && (psw.shutoff_synthesized_ctrl
                    || st.cpf_state.domain_shutoff.contains_key(&domain_key))
            {
                psw.control_ports.retain(|(p, _)| p != "ctrl");
                psw.control_port_scopes.remove("ctrl");
            }
            psw.shutoff_synthesized_ctrl = false;
            for (port_name, expr) in en_updates {
                psw.control_port_scopes
                    .insert(port_name.to_string(), update_scope.clone());
                if let Some((_, existing)) =
                    psw.control_ports.iter_mut().find(|(p, _)| p == port_name)
                {
                    *existing = expr;
                } else {
                    psw.control_ports.push((port_name.to_string(), expr));
                }
            }
            let exprs: Vec<String> = psw
                .control_ports
                .iter()
                .filter(|(p, _)| matches!(p.as_str(), "ctrl" | "ctrl1" | "ctrl2"))
                .map(|(_, e)| e.clone())
                .collect();
            let en_q = combine_bool_ands(&exprs);
            let off_q = negate_bool_expr(&en_q);
            if let Some(on_st) = psw.on_states.iter_mut().find(|s| s.state_name == "ON") {
                on_st.expr = en_q.clone();
            } else {
                psw.on_states.push(SwitchStateExpr {
                    state_name: "ON".to_string(),
                    supply_port: Some("vdd_in".to_string()),
                    expr: en_q.clone(),
                });
            }
            if let Some(off_st) = psw.off_states.iter_mut().find(|s| s.state_name == "OFF") {
                off_st.expr = off_q;
            } else {
                psw.off_states.push(SwitchStateExpr {
                    state_name: "OFF".to_string(),
                    supply_port: None,
                    expr: off_q,
                });
            }
            st.cpf_state.domain_shutoff.remove(&domain_key);
            st.cpf_state
                .domain_enable
                .insert(domain_key.clone(), (en_q, String::new()));
            st.cpf_state.updated_domain_conditions.insert(domain_key);
        }
        if let Some(ack) = ack_qualified {
            if let Some((_, existing)) = psw.ack_ports.iter_mut().find(|(p, _)| p == "ack") {
                *existing = ack;
            } else {
                psw.ack_ports.push(("ack".to_string(), ack));
            }
        }
        if let Some(cells) = args.val("-cells") {
            psw.mapped_cells.extend(split_tcl_list(cells));
        }
        st.cpf_state.finalized = false;
        st.cpf_state.fallback_applied = false;
    }
    Ok(().into())
}

pub(crate) fn cmd_create_isolation_rule(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-name", 1),
            ("-from", 1),
            ("-to", 1),
            ("-pins", 1),
            ("-exclude", 1),
            ("-isolation_condition", 1),
            ("-isolation_output", 1),
            ("-isolation_target", 1),
            ("-secondary_domain", 1),
        ],
        &["-no_condition"],
        &[("-name_prefix", 1), ("-name_suffix", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let name = args
        .val("-name")
        .ok_or_else(|| loc_err(loc.as_ref(), "create_isolation_rule: missing -name"))?
        .to_string();
    let from = args.val("-from").map(|d| st.qualify_ref(d));
    let to = args.val("-to").map(|d| st.qualify_ref(d));
    let (domain, applies_to) = match (&from, &to) {
        (Some(f), _) => (f.clone(), Some(AppliesTo::Outputs)),
        (None, Some(t)) => (t.clone(), Some(AppliesTo::Inputs)),
        (None, None) => {
            return Err(loc_err(
                loc.as_ref(),
                "create_isolation_rule: requires -from or -to",
            ))
        }
    };
    let clamp_value = match args.val("-isolation_output") {
        Some(v) => Some(ClampValue::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let (isolation_signal, isolation_sense) = match args.val("-isolation_condition") {
        Some(cond) => parse_cpf_signal_sense(cond, st),
        None => (None, None),
    };
    let location = match args
        .val("-isolation_target")
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("from") => Some(StrategyLocation::Self_),
        Some("to") => Some(if from.is_some() {
            StrategyLocation::Parent
        } else {
            StrategyLocation::Self_
        }),
        Some(other) => {
            return Err(loc_err(
                loc.as_ref(),
                &format!("create_isolation_rule: invalid -isolation_target `{other}`"),
            ))
        }
        None => None,
    };
    let elements = args
        .all("-pins")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    let exclude_elements = args
        .all("-exclude")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    let isolation_supply_set = args
        .val("-secondary_domain")
        .map(|d| format!("{}.primary", st.qualify_ref(d)))
        .or_else(|| to.as_ref().map(|t| format!("{t}.primary")));

    let order = st.next_order();
    let id = IsoId(st.intent.isolation_strategies.len() as u32);
    st.intent.isolation_strategies.push(IsolationStrategy {
        id,
        name: name.clone(),
        scope: st.current_scope.clone(),
        domain,
        applies_to,
        applies_to_clamp: None,
        elements,
        exclude_elements,
        source_filter: from,
        sink_filter: to,
        diff_supply_only: false,
        no_isolation: false,
        force_isolation: false,
        clamp_value,
        isolation_supply_set,
        isolation_power_net: None,
        isolation_ground_net: None,
        isolation_signal,
        isolation_sense,
        ctrl_scope: None,
        location,
        mapped_cells: Vec::new(),
        mapped_lib_model: None,
        precedence_order: order,
        update: false,
        loc,
    });
    st.cpf_state.finalized = false;
    Ok(Value::Str(name))
}

pub(crate) fn cmd_update_isolation_rules(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-names", 1), ("-location", 1), ("-cells", 1)],
        &[],
        &[("-prefix", 1), ("-within_files", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let sep = st.intent.hierarchy_separator;
    let names = args
        .all("-names")
        .flat_map(|v| split_tcl_list(&v[0]))
        .map(|n| trim_hier_path(&n).to_string())
        .collect();
    let location = match args
        .val("-location")
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("from") => Some(StrategyLocation::Self_),
        Some("to") => Some(StrategyLocation::Parent),
        Some(other) => Some(StrategyLocation::parse(other).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let lib_cells = args
        .all("-cells")
        .flat_map(|v| split_tcl_list(&v[0]))
        .collect();
    st.intent.deferred_ops.push(DeferredOp::UpdateCpfIso {
        names,
        scope: st.current_scope.clone(),
        sep,
        location,
        lib_cells,
        loc,
    });
    st.cpf_state.finalized = false;
    Ok(().into())
}

pub(crate) fn cmd_create_level_shifter_rule(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-name", 1),
            ("-from", 1),
            ("-to", 1),
            ("-pins", 1),
            ("-exclude", 1),
            ("-input_voltage_range", 1),
            ("-output_voltage_range", 1),
        ],
        &[],
        &[("-name_prefix", 1), ("-name_suffix", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let name = args
        .val("-name")
        .ok_or_else(|| loc_err(loc.as_ref(), "create_level_shifter_rule: missing -name"))?
        .to_string();
    let from = args.val("-from").map(|d| st.qualify_ref(d));
    let to = args.val("-to").map(|d| st.qualify_ref(d));
    let (domain, applies_to) = match (&from, &to) {
        (Some(f), _) => (f.clone(), Some(AppliesTo::Outputs)),
        (None, Some(t)) => (t.clone(), Some(AppliesTo::Inputs)),
        (None, None) => {
            return Err(loc_err(
                loc.as_ref(),
                "create_level_shifter_rule: requires -from or -to",
            ))
        }
    };
    let elements = args
        .all("-pins")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    let exclude_elements = args
        .all("-exclude")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    let order = st.next_order();
    let id = LsId(st.intent.level_shifter_strategies.len() as u32);
    st.intent
        .level_shifter_strategies
        .push(LevelShifterStrategy {
            id,
            name: name.clone(),
            scope: st.current_scope.clone(),
            domain,
            applies_to,
            rule: Some(LsRule::Both),
            threshold_uv: None,
            elements,
            exclude_elements,
            source_filter: from.clone(),
            sink_filter: to.clone(),
            no_shift: false,
            force_shift: false,
            location: None,
            input_supply_set: from.as_ref().map(|f| format!("{f}.primary")),
            output_supply_set: to.as_ref().map(|t| format!("{t}.primary")),
            internal_supply_set: None,
            mapped_cells: Vec::new(),
            precedence_order: order,
            update: false,
            loc,
        });
    st.cpf_state.finalized = false;
    Ok(Value::Str(name))
}

pub(crate) fn cmd_update_level_shifter_rules(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-names", 1), ("-location", 1), ("-cells", 1)],
        &[],
        &[("-prefix", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let sep = st.intent.hierarchy_separator;
    let names = args
        .all("-names")
        .flat_map(|v| split_tcl_list(&v[0]))
        .map(|n| trim_hier_path(&n).to_string())
        .collect();
    let location = match args
        .val("-location")
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("from") => Some(StrategyLocation::Self_),
        Some("to") => Some(StrategyLocation::Parent),
        Some(other) => Some(StrategyLocation::parse(other).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let lib_cells = args
        .all("-cells")
        .flat_map(|v| split_tcl_list(&v[0]))
        .collect();
    st.intent.deferred_ops.push(DeferredOp::UpdateCpfLs {
        names,
        scope: st.current_scope.clone(),
        sep,
        location,
        lib_cells,
        loc,
    });
    st.cpf_state.finalized = false;
    Ok(().into())
}

pub(crate) fn cmd_create_state_retention_rule(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-name", 1),
            ("-domain", 1),
            ("-instances", 1),
            ("-exclude", 1),
            ("-save_edge", 1),
            ("-restore_edge", 1),
            ("-save_level", 1),
            ("-restore_level", 1),
            ("-secondary_power_net", 1),
        ],
        &["-required"],
        &[("-target_type", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let name = args
        .val("-name")
        .ok_or_else(|| loc_err(loc.as_ref(), "create_state_retention_rule: missing -name"))?
        .to_string();
    let domain = st
        .qualify_ref(args.val("-domain").ok_or_else(|| {
            loc_err(loc.as_ref(), "create_state_retention_rule: missing -domain")
        })?);
    let (save_signal, save_sense) = if let Some(edge) = args.val("-save_edge") {
        let (sig, s) = parse_cpf_signal_sense(edge, st);
        let edge_sense = match s {
            Some(Sense::Low) => Some(Sense::Negedge),
            Some(Sense::High) => Some(Sense::Posedge),
            other => other,
        };
        (sig, edge_sense)
    } else if let Some(lvl) = args.val("-save_level") {
        parse_cpf_signal_sense(lvl, st)
    } else {
        (None, None)
    };
    let (restore_signal, restore_sense) = if let Some(edge) = args.val("-restore_edge") {
        let (sig, s) = parse_cpf_signal_sense(edge, st);
        let edge_sense = match s {
            Some(Sense::Low) => Some(Sense::Negedge),
            Some(Sense::High) => Some(Sense::Posedge),
            other => other,
        };
        (sig, edge_sense)
    } else if let Some(lvl) = args.val("-restore_level") {
        parse_cpf_signal_sense(lvl, st)
    } else {
        (None, None)
    };
    let retention_power_net = args.val("-secondary_power_net").map(|n| st.qualify_ref(n));
    let elements = args
        .all("-instances")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    let exclude_elements = args
        .all("-exclude")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();

    let order = st.next_order();
    let id = RetId(st.intent.retention_strategies.len() as u32);
    st.intent.retention_strategies.push(RetentionStrategy {
        id,
        name: name.clone(),
        scope: st.current_scope.clone(),
        domain,
        elements,
        exclude_elements,
        no_retention: false,
        retention_supply_set: None,
        retention_power_net,
        retention_ground_net: None,
        save_signal,
        save_sense,
        save_condition: None,
        save_scope: None,
        save_cond_scope: None,
        restore_signal,
        restore_sense,
        restore_condition: None,
        restore_scope: None,
        restore_cond_scope: None,
        retention_condition: None,
        retention_cond_scope: None,
        use_retention_as_primary: false,
        mapped_cells: Vec::new(),
        mapped_lib_model: None,
        precedence_order: order,
        update: false,
        loc,
    });
    st.cpf_state.finalized = false;
    Ok(Value::Str(name))
}

pub(crate) fn cmd_update_state_retention_rules(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-names", 1), ("-cells", 1)],
        &["-use_secondary_for_output"],
        &[("-cell_type", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let sep = st.intent.hierarchy_separator;
    let names = args
        .all("-names")
        .flat_map(|v| split_tcl_list(&v[0]))
        .map(|n| trim_hier_path(&n).to_string())
        .collect();
    let lib_cells = args
        .all("-cells")
        .flat_map(|v| split_tcl_list(&v[0]))
        .collect();
    st.intent.deferred_ops.push(DeferredOp::UpdateCpfRet {
        names,
        scope: st.current_scope.clone(),
        sep,
        lib_cells,
        use_secondary_for_output: args.bool_val("-use_secondary_for_output")?.unwrap_or(false),
        loc,
    });
    st.cpf_state.finalized = false;
    Ok(().into())
}

pub(crate) fn cmd_define_cell_note(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let cmd = a[0].rsplit("::").next().unwrap_or(&a[0]);
    st.intent.diagnostics.push(UpfDiag {
        rule_id: "UPF_D13_NOTE".to_string(),
        severity: DiagSeverity::Note,
        message: format!("{cmd}: cell library definition hint recorded"),
        loc,
    });
    Ok(().into())
}

pub(crate) fn cmd_identify_power_logic(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-type", 1), ("-instances", 1), ("-module", 1)],
        &[],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    st.intent.diagnostics.push(UpfDiag {
        rule_id: "UPF_D13_NOTE".to_string(),
        severity: DiagSeverity::Note,
        message: "identify_power_logic recorded".to_string(),
        loc,
    });
    Ok(().into())
}
