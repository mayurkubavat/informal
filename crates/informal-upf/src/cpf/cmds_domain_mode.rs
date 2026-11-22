//! CPF version, design scope, supply nets, power domains, nominal conditions, and power modes.

use std::collections::BTreeMap;

use informal_tcl::Value;

use crate::expr_bv::{normalize_logic_expr_sep, qualify_logic_expr_in_scope};
use crate::ir::{
    split_tcl_list, DeferredOp, DiagSeverity, PowerDomain, PowerDomainId, SupplyNet, SupplyNetId,
    UpfDiag, UpfLoc, UpfVersion, VoltageSpec,
};
use crate::upf::{loc_err, UpfArgs, UpfEvalState};

use super::{
    ensure_cpf_domain_supply_set, finalize_cpf_ex, CpfCondState, CpfPowerMode, NominalCondition,
};

pub(crate) fn cmd_set_cpf_version(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(a, &[], &[], &[], &mut st.intent, loc.clone())?;
    args.check_max_pos(1)?;
    let v = args.req_pos(0, "CPF version")?;
    let ver = UpfVersion::parse_cpf(v).map_err(|e| loc_err(loc.as_ref(), &e))?;
    st.intent.upf_version = Some(ver);
    Ok(Value::Str(ver.as_str().to_string()))
}

pub(crate) fn cmd_set_hierarchy_separator(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(a, &[], &[], &[], &mut st.intent, loc.clone())?;
    args.check_max_pos(1)?;
    let sep_str = args.req_pos(0, "hierarchy separator")?;
    let ch = sep_str
        .chars()
        .next()
        .filter(|_| sep_str.chars().count() == 1)
        .ok_or_else(|| {
            loc_err(
                loc.as_ref(),
                &format!("set_hierarchy_separator: expected single char, got `{sep_str}`"),
            )
        })?;
    st.intent.hierarchy_separator = ch;
    Ok(Value::Str(ch.to_string()))
}

pub(crate) fn cmd_set_array_naming_style(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(a, &[], &[], &[], &mut st.intent, loc.clone())?;
    args.check_max_pos(1)?;
    let style = args.req_pos(0, "array naming style")?;
    st.intent.diagnostics.push(UpfDiag {
        rule_id: "UPF_D13_NOTE".to_string(),
        severity: DiagSeverity::Note,
        message: format!("set_array_naming_style `{style}` recorded"),
        loc,
    });
    Ok(().into())
}

pub(crate) fn cmd_set_design(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-ports", 1), ("-parameters", 1)],
        &[],
        &[("-honor_boundary_port_domain", 0)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let top = args.req_pos(0, "design top module")?;
    if st.cpf_state.active_design.is_some() {
        st.cpf_state.design_stack.push((
            st.cpf_state.active_design.clone(),
            st.intent.design_top.clone(),
        ));
    }
    st.intent.design_top = Some(top.to_string());
    st.cpf_state.active_design = Some(top.to_string());
    st.cpf_state.finalized = false;
    Ok(Value::Str(top.to_string()))
}

pub(crate) fn cmd_end_design(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(a, &[], &[], &[], &mut st.intent, loc.clone())?;
    args.check_max_pos(1)?;
    if let Some(top) = args.pos.first() {
        let cur_opt = st
            .cpf_state
            .active_design
            .as_ref()
            .or(st.intent.design_top.as_ref());
        if let Some(cur) = cur_opt {
            if cur != top {
                return Err(loc_err(
                    loc.as_ref(),
                    &format!("end_design `{top}` does not match set_design `{cur}`"),
                ));
            }
        }
    }
    let defer_fallback = st.should_defer_cpf_fallback();
    let defer_other_scopes = st
        .scope_stack
        .last()
        .filter(|f| !f.is_sequential_sibling)
        .map(|_| st.current_scope.clone());
    {
        let UpfEvalState {
            intent, cpf_state, ..
        } = st;
        finalize_cpf_ex(
            intent,
            cpf_state,
            !defer_fallback,
            defer_other_scopes.as_deref(),
        );
    }
    if let Some((prev_active, prev_intent_top)) = st.cpf_state.design_stack.pop() {
        st.cpf_state.active_design = prev_active;
        st.intent.design_top = prev_intent_top;
        st.cpf_state.finalized = false;
    } else {
        st.cpf_state.active_design = None;
    }
    Ok(().into())
}

pub(crate) fn cmd_create_power_nets(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-nets", 1), ("-voltage", 1), ("-user_attributes", 1)],
        &["-internal"],
        &[("-peak_ir_drop_limit", 1), ("-average_ir_drop_limit", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let nets_str = args
        .val("-nets")
        .ok_or_else(|| loc_err(loc.as_ref(), "create_power_nets: missing -nets"))?;
    let voltage = match args.val("-voltage") {
        Some(v) => Some(VoltageSpec::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let is_internal = args.bool_val("-internal")?.unwrap_or(false);
    for raw in split_tcl_list(nets_str) {
        let name = st.qualify_decl(&raw);
        let id = SupplyNetId(st.intent.supply_nets.len() as u32);
        st.intent.supply_nets.push(SupplyNet {
            id,
            name,
            short_name: raw,
            scope: st.current_scope.clone(),
            domain: None,
            resolve: None,
            reuse: false,
            connected_ports: Vec::new(),
            connected_pins: Vec::new(),
            port_states: Vec::new(),
            nominal_voltage: voltage,
            is_ground: false,
            is_internal,
            loc: loc.clone(),
        });
    }
    st.cpf_state.finalized = false;
    Ok(().into())
}

pub(crate) fn cmd_create_ground_nets(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-nets", 1), ("-voltage", 1)],
        &["-internal"],
        &[("-peak_ir_drop_limit", 1), ("-average_ir_drop_limit", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let nets_str = args
        .val("-nets")
        .ok_or_else(|| loc_err(loc.as_ref(), "create_ground_nets: missing -nets"))?;
    let voltage = match args.val("-voltage") {
        Some(v) => VoltageSpec::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?,
        None => VoltageSpec::Nominal(0),
    };
    let is_internal = args.bool_val("-internal")?.unwrap_or(false);
    for raw in split_tcl_list(nets_str) {
        let name = st.qualify_decl(&raw);
        let id = SupplyNetId(st.intent.supply_nets.len() as u32);
        st.intent.supply_nets.push(SupplyNet {
            id,
            name,
            short_name: raw,
            scope: st.current_scope.clone(),
            domain: None,
            resolve: None,
            reuse: false,
            connected_ports: Vec::new(),
            connected_pins: Vec::new(),
            port_states: Vec::new(),
            nominal_voltage: Some(voltage),
            is_ground: true,
            is_internal,
            loc: loc.clone(),
        });
    }
    st.cpf_state.finalized = false;
    Ok(().into())
}

pub(crate) fn cmd_create_global_connection(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-net", 1), ("-pins", 1), ("-domain", 1), ("-instances", 1)],
        &[],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let net = args
        .val("-net")
        .ok_or_else(|| loc_err(loc.as_ref(), "create_global_connection: missing -net"))?;
    let pins = args
        .all("-pins")
        .flat_map(|v| split_tcl_list(&v[0]))
        .collect();
    let instances = args
        .all("-instances")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    st.intent.deferred_ops.push(DeferredOp::GlobalConnection {
        net: st.qualify_ref(net),
        pins,
        domain: args.val("-domain").map(|d| st.qualify_ref(d)),
        instances,
        scope: st.current_scope.clone(),
        loc,
    });
    st.cpf_state.finalized = false;
    Ok(().into())
}

pub(crate) fn cmd_create_power_domain(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-name", 1),
            ("-instances", 1),
            ("-exclude_instances", 1),
            ("-boundary_ports", 1),
            ("-shutoff_condition", 1),
            ("-active_state_conditions", 1),
            ("-base_domains", 1),
        ],
        &["-default"],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let raw_name = args
        .val("-name")
        .ok_or_else(|| loc_err(loc.as_ref(), "create_power_domain: missing -name"))?;
    let name = st.qualify_decl(raw_name);
    let is_default = args.bool_val("-default")?.unwrap_or(false);
    let elements = args
        .all("-instances")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    let exclude_elements = args
        .all("-exclude_instances")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    if let Some(cond) = args.val("-shutoff_condition") {
        let q = if st.current_scope.is_empty() {
            qualify_logic_expr_in_scope(cond.trim(), "", st.intent.hierarchy_separator)
        } else {
            normalize_logic_expr_sep(cond.trim(), st.intent.hierarchy_separator)
        };
        st.cpf_state.domain_enable.remove(&name);
        st.cpf_state
            .domain_shutoff
            .insert(name.clone(), (q, st.current_scope.clone()));
        st.cpf_state.updated_domain_conditions.insert(name.clone());
    }
    let ss_name =
        ensure_cpf_domain_supply_set(&mut st.intent, &name, &st.current_scope, loc.clone());
    st.cpf_state
        .cpf_ss_names
        .insert(ss_name.trim_start_matches('/').to_string());
    let mut supply_handles = BTreeMap::new();
    supply_handles.insert("primary".to_string(), ss_name.clone());

    let id = PowerDomainId(st.intent.domains.len() as u32);
    st.intent.domains.push(PowerDomain {
        id,
        name: name.clone(),
        short_name: raw_name.to_string(),
        scope: st.current_scope.clone(),
        include_scope: is_default,
        elements,
        exclude_elements,
        transitive: true,
        atomic: false,
        primary_supply_set: Some(ss_name),
        supply_handles,
        legacy_primary_power_net: None,
        legacy_primary_ground_net: None,
        is_top: is_default && st.current_scope.is_empty(),
        update: false,
        loc,
    });
    st.cpf_state.finalized = false;
    st.cpf_state.fallback_applied = false;
    Ok(Value::Str(name))
}

pub(crate) fn cmd_update_power_domain(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-name", 1),
            ("-primary_power_net", 1),
            ("-primary_ground_net", 1),
            ("-equivalent_power_nets", 1),
            ("-equivalent_ground_nets", 1),
            ("-pmos_bias_net", 1),
            ("-nmos_bias_net", 1),
        ],
        &[],
        &[("-transition_slope", 1), ("-transition_latency", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let raw_name = args
        .val("-name")
        .ok_or_else(|| loc_err(loc.as_ref(), "update_power_domain: missing -name"))?;
    let name = st.qualify(raw_name);
    let ss_name =
        ensure_cpf_domain_supply_set(&mut st.intent, &name, &st.current_scope, loc.clone());
    st.cpf_state
        .cpf_ss_names
        .insert(ss_name.trim_start_matches('/').to_string());
    let pwr = args.val("-primary_power_net").map(|n| st.qualify_ref(n));
    let gnd = args.val("-primary_ground_net").map(|n| st.qualify_ref(n));
    let nwell = args.val("-pmos_bias_net").map(|n| st.qualify_ref(n));
    let pwell = args.val("-nmos_bias_net").map(|n| st.qualify_ref(n));

    if let Some(ss) = st.intent.supply_sets.iter_mut().find(|s| s.name == ss_name) {
        if let Some(p) = &pwr {
            ss.power = Some(p.clone());
        }
        if let Some(g) = &gnd {
            ss.ground = Some(g.clone());
        }
        if let Some(nw) = nwell {
            ss.nwell = Some(nw);
        }
        if let Some(pw) = pwell {
            ss.pwell = Some(pw);
        }
    }
    if let Some(pd) = st.intent.domains.iter_mut().find(|d| d.name == name) {
        if let Some(p) = pwr {
            pd.legacy_primary_power_net = Some(p);
        }
        if let Some(g) = gnd {
            pd.legacy_primary_ground_net = Some(g);
        }
    }
    for (opt, is_ground) in [
        ("-equivalent_power_nets", false),
        ("-equivalent_ground_nets", true),
    ] {
        let nets: Vec<String> = args
            .all(opt)
            .flat_map(|v| split_tcl_list(&v[0]))
            .map(|n| st.qualify_ref(&n))
            .collect();
        if !nets.is_empty() {
            st.intent.deferred_ops.push(DeferredOp::CpfEquivalentNets {
                domain: st.qualify_ref(raw_name),
                scope: st.current_scope.clone(),
                is_ground,
                nets,
                function_only: false,
                loc: loc.clone(),
            });
        }
    }
    st.cpf_state.finalized = false;
    st.cpf_state.fallback_applied = false;
    Ok(().into())
}

pub(crate) fn cmd_create_nominal_condition(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-name", 1),
            ("-voltage", 1),
            ("-state", 1),
            ("-pmos_bias_voltage", 1),
            ("-nmos_bias_voltage", 1),
        ],
        &[],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let name = args
        .val("-name")
        .ok_or_else(|| loc_err(loc.as_ref(), "create_nominal_condition: missing -name"))?
        .to_string();
    let state = match args
        .val("-state")
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("off") => CpfCondState::Off,
        Some("standby") => CpfCondState::Standby,
        Some("on") | None => CpfCondState::On,
        Some(other) => {
            return Err(loc_err(
                loc.as_ref(),
                &format!("create_nominal_condition: invalid -state `{other}`"),
            ))
        }
    };
    let voltage = match (state, args.val("-voltage")) {
        (CpfCondState::Off, _) => VoltageSpec::Off,
        (_, Some(v)) => VoltageSpec::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?,
        (_, None) => {
            return Err(loc_err(
                loc.as_ref(),
                "create_nominal_condition: missing -voltage",
            ))
        }
    };
    let pmos_bias = match args.val("-pmos_bias_voltage") {
        Some(v) => Some(VoltageSpec::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let nmos_bias = match args.val("-nmos_bias_voltage") {
        Some(v) => Some(VoltageSpec::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let key = (st.current_scope.clone(), name.clone());
    let has_mode_ref = st.cpf_state.power_modes.iter().any(|pm| {
        pm.domain_conditions
            .iter()
            .any(|(_, nc)| nc.trim_start_matches('/') == name)
    });
    if st.cpf_state.nominal_conditions.contains_key(&key) || has_mode_ref {
        st.cpf_state.updated_nominal_conditions.insert(key.clone());
    }
    st.cpf_state.direct_nominal_conditions.insert(key.clone());
    st.cpf_state.subtree_nominal_conditions.insert(key.clone());
    st.cpf_state
        .explicit_overwrite_nominal_conditions
        .remove(&key);
    st.cpf_state.nominal_conditions.insert(
        key,
        NominalCondition {
            voltage,
            state,
            pmos_bias,
            nmos_bias,
            loc,
        },
    );
    st.cpf_state.finalized = false;
    Ok(Value::Str(name))
}

pub(crate) fn cmd_create_operating_corner(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-name", 1), ("-voltage", 1)],
        &[],
        &[("-process", 1), ("-temperature", 1), ("-library_set", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let name = args
        .val("-name")
        .ok_or_else(|| loc_err(loc.as_ref(), "create_operating_corner: missing -name"))?
        .to_string();
    let voltage = match args.val("-voltage") {
        Some(v) => Some(VoltageSpec::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    st.intent.diagnostics.push(UpfDiag {
        rule_id: "UPF_D13_NOTE".to_string(),
        severity: DiagSeverity::Note,
        message: format!("operating corner `{name}` ({voltage:?}) recorded"),
        loc,
    });
    Ok(Value::Str(name))
}

pub(crate) fn cmd_create_power_mode(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-name", 1), ("-domain_conditions", 1), ("-condition", 1)],
        &["-default", "-illegal"],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let name = args
        .val("-name")
        .ok_or_else(|| loc_err(loc.as_ref(), "create_power_mode: missing -name"))?
        .to_string();
    let mut domain_conditions = Vec::new();
    if let Some(dcs) = args.val("-domain_conditions") {
        for item in split_tcl_list(dcs) {
            let Some((pd, nc)) = item.split_once('@') else {
                return Err(loc_err(
                    loc.as_ref(),
                    &format!("create_power_mode: expected `PD@NC`, got `{item}`"),
                ));
            };
            domain_conditions.push((st.qualify(pd.trim()), nc.trim().to_string()));
        }
    }
    let condition = args.val("-condition").map(|c| {
        qualify_logic_expr_in_scope(c.trim(), &st.current_scope, st.intent.hierarchy_separator)
    });
    st.cpf_state.power_modes.push(CpfPowerMode {
        name: name.clone(),
        scope: st.current_scope.clone(),
        design_top: st.cpf_state.active_design.clone(),
        domain_conditions,
        condition,
        condition_scope: String::new(),
        is_default: args.bool_val("-default")?.unwrap_or(false),
        illegal: args.bool_val("-illegal")?.unwrap_or(false),
        emitted: false,
        loc,
    });
    st.cpf_state.finalized = false;
    Ok(Value::Str(name))
}

pub(crate) fn cmd_update_power_mode(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-name", 1), ("-condition", 1)],
        &["-illegal"],
        &[("-activity_file", 1), ("-sdc_files", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let name = args
        .val("-name")
        .ok_or_else(|| loc_err(loc.as_ref(), "update_power_mode: missing -name"))?;
    let illegal_flag = if args.flag("-illegal") {
        Some(args.bool_val("-illegal")?.unwrap_or(true))
    } else {
        None
    };
    let cond_q = args.val("-condition").map(|c| {
        qualify_logic_expr_in_scope(c.trim(), &st.current_scope, st.intent.hierarchy_separator)
    });
    let cur_scope = &st.current_scope;
    let cur_design = &st.cpf_state.active_design;
    let matched_pm = st
        .cpf_state
        .power_modes
        .iter_mut()
        .rfind(|m| m.name == name && m.scope == *cur_scope && m.design_top == *cur_design);
    let matched_pm = match matched_pm {
        Some(pm) => Some(pm),
        None if cur_design.is_none() => st
            .cpf_state
            .power_modes
            .iter_mut()
            .rfind(|m| m.name == name && m.scope == *cur_scope),
        None => None,
    };
    if let Some(pm) = matched_pm {
        if let Some(c) = cond_q {
            pm.condition = Some(c);
            pm.condition_scope = String::new();
        }
        if let Some(ill) = illegal_flag {
            pm.illegal = ill;
        }
        pm.emitted = false;
        st.cpf_state.finalized = false;
    }
    Ok(().into())
}
