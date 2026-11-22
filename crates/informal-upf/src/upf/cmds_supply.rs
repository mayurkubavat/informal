//! Supply ports, supply nets, supply sets, power switches, and equivalence commands.

use std::collections::BTreeMap;

use informal_tcl::Value;

use crate::ir::{
    split_tcl_list, trim_hier_path, DeferredOp, PowerSwitchDef, PowerSwitchId, SupplyDirection,
    SupplyEquivalence, SupplyNet, SupplyNetId, SupplyPort, SupplyPortId, SupplySet, SupplySetId,
    SwitchStateExpr, UpfLoc,
};

use super::eval::{loc_err, parse_bool_literal, UpfArgs, UpfEvalState};

pub(crate) fn cmd_implicit_supply_sets(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[],
        &["-enable", "-disable"],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    if args.flag("-enable") {
        st.intent.create_implicit_supply_sets = true;
    } else if args.flag("-disable") {
        st.intent.create_implicit_supply_sets = false;
    } else if let Some(b) = args.pos.first() {
        st.intent.create_implicit_supply_sets = parse_bool_literal(b)
            .ok_or_else(|| loc_err(loc.as_ref(), &format!("invalid boolean `{b}`")))?;
    }
    Ok(().into())
}

pub(crate) fn cmd_create_supply_port(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-direction", 1), ("-domain", 1), ("-resolve", 1)],
        &[],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let raw = args.req_pos(0, "supply port name")?;
    let name = st.qualify_decl(raw);
    let direction = match args.val("-direction") {
        Some(d) => SupplyDirection::parse(d).map_err(|e| loc_err(loc.as_ref(), &e))?,
        None => SupplyDirection::In,
    };
    let domain = args.val("-domain").map(|d| st.qualify_ref(d));
    let resolve = args.val("-resolve").map(str::to_string);
    let id = SupplyPortId(st.intent.supply_ports.len() as u32);
    st.intent.supply_ports.push(SupplyPort {
        id,
        name: name.clone(),
        short_name: trim_hier_path(raw).to_string(),
        scope: st.current_scope.clone(),
        direction,
        domain,
        resolve,
        loc,
    });
    Ok(Value::Str(name))
}

pub(crate) fn cmd_create_supply_net(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-domain", 1), ("-resolve", 1)],
        &["-reuse"],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let raw = args.req_pos(0, "supply net name")?;
    let name = st.qualify_decl(raw);
    let reuse = args.bool_val("-reuse")?.unwrap_or(false);
    let domain = args.val("-domain").map(|d| st.qualify_ref(d));
    let resolve = args.val("-resolve").map(str::to_string);
    if reuse {
        if let Some(existing) = st.intent.supply_nets.iter().find(|n| n.name == name) {
            return Ok(Value::Str(existing.name.clone()));
        }
    }
    let id = SupplyNetId(st.intent.supply_nets.len() as u32);
    st.intent.supply_nets.push(SupplyNet {
        id,
        name: name.clone(),
        short_name: trim_hier_path(raw).to_string(),
        scope: st.current_scope.clone(),
        domain,
        resolve,
        reuse,
        connected_ports: Vec::new(),
        connected_pins: Vec::new(),
        port_states: Vec::new(),
        nominal_voltage: None,
        is_ground: false,
        is_internal: false,
        loc,
    });
    Ok(Value::Str(name))
}

pub(crate) fn cmd_connect_supply_net(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-ports", 1),
            ("-pins", 1),
            ("-domain", 1),
            ("-elements", 1),
            ("-pg_type", 1),
        ],
        &["-transitive"],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let raw_net = args.req_pos(0, "supply net name")?;
    let net = st.qualify_ref(raw_net);
    let ports = args
        .all("-ports")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    let pins = args
        .all("-pins")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    let elements = args
        .all("-elements")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    let domain = args.val("-domain").map(|d| st.qualify_ref(d));
    let transitive = args.bool_val("-transitive")?.unwrap_or(true);
    let pg_type = args.val("-pg_type").map(str::to_string);
    st.intent.deferred_ops.push(DeferredOp::ConnectSupplyNet {
        net,
        scope: st.current_scope.clone(),
        ports,
        pins,
        domain,
        elements,
        transitive,
        pg_type,
        loc,
    });
    Ok(().into())
}

pub(crate) fn cmd_create_supply_set(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-function", 1), ("-reference_gnd", 1)],
        &["-update"],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let raw = args.req_pos(0, "supply set name")?;
    let name = st.qualify_decl(raw);
    let update = args.bool_val("-update")?.unwrap_or(false);
    let mut funcs = Vec::new();
    for v in args.all("-function") {
        let parts = split_tcl_list(&v[0]);
        if parts.len() != 2 {
            return Err(loc_err(
                loc.as_ref(),
                &format!(
                    "create_supply_set: -function expects `{{func net}}`, got `{}`",
                    v[0]
                ),
            ));
        }
        funcs.push((parts[0].clone(), st.qualify_ref(&parts[1])));
    }
    let ref_gnd = args.val("-reference_gnd").map(|g| st.qualify_ref(g));

    if update {
        if let Some(existing) = st.intent.supply_sets.iter_mut().find(|s| s.name == name) {
            for (func, net) in funcs {
                existing
                    .set_function(&func, net)
                    .map_err(|e| loc_err(loc.as_ref(), &e))?;
            }
            if let Some(rg) = ref_gnd {
                existing.reference_gnd = Some(rg);
            }
            return Ok(Value::Str(name));
        }
    }

    let id = SupplySetId(st.intent.supply_sets.len() as u32);
    let mut ss = SupplySet {
        id,
        name: name.clone(),
        scope: st.current_scope.clone(),
        power: None,
        ground: None,
        pwell: None,
        nwell: None,
        deeppwell: None,
        deepnwell: None,
        extra_functions: BTreeMap::new(),
        is_implicit: false,
        reference_gnd: ref_gnd,
        loc: loc.clone(),
    };
    for (func, net) in funcs {
        ss.set_function(&func, net)
            .map_err(|e| loc_err(loc.as_ref(), &e))?;
    }
    st.intent.supply_sets.push(ss);
    Ok(Value::Str(name))
}

pub(crate) fn cmd_associate_supply_set(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(a, &[("-handle", 1)], &[], &[], &mut st.intent, loc.clone())?;
    args.check_max_pos(1)?;
    let ss_name = st.qualify_handle(args.req_pos(0, "supply set name")?);
    let handles: Vec<String> = args
        .all("-handle")
        .flat_map(|v| split_tcl_list(&v[0]))
        .collect();
    if handles.is_empty() {
        return Err(loc_err(
            loc.as_ref(),
            "associate_supply_set: missing -handle",
        ));
    }
    for h in handles {
        st.intent.deferred_ops.push(DeferredOp::AssociateSupplySet {
            supply_set: ss_name.clone(),
            handle: st.qualify_handle(&h),
            scope: st.current_scope.clone(),
            loc: loc.clone(),
        });
    }
    Ok(().into())
}

pub(crate) fn cmd_set_domain_supply_net(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-primary_power_net", 1), ("-primary_ground_net", 1)],
        &[],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let domain = st.qualify_ref(args.req_pos(0, "domain name")?);
    let pwr = args.val("-primary_power_net").map(|n| st.qualify_ref(n));
    let gnd = args.val("-primary_ground_net").map(|n| st.qualify_ref(n));
    st.intent.deferred_ops.push(DeferredOp::SetDomainSupplyNet {
        domain,
        scope: st.current_scope.clone(),
        primary_power_net: pwr,
        primary_ground_net: gnd,
        loc,
    });
    Ok(().into())
}

pub(crate) fn cmd_create_power_switch(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-domain", 1),
            ("-supply_set", 1),
            ("-input_supply_port", 1),
            ("-output_supply_port", 1),
            ("-control_port", 1),
            ("-ack_port", 1),
            ("-on_state", 1),
            ("-off_state", 1),
            ("-error_state", 1),
            ("-ack_delay", 1),
        ],
        &[],
        &[("-name_prefix", 1), ("-name_suffix", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let raw = args.req_pos(0, "power switch name")?;
    let name = st.qualify_decl(raw);
    let domain = match args.val("-domain") {
        Some(d) => st.qualify_ref(d),
        None => String::new(),
    };
    let parse_pair = |raw: &str, ref_rhs: bool| -> Result<(String, String), String> {
        let p = split_tcl_list(raw);
        if p.is_empty() || p.len() > 2 {
            return Err(loc_err(
                loc.as_ref(),
                &format!("create_power_switch: expected `{{port ?target?}}`, got `{raw}`"),
            ));
        }
        let rhs = if p.len() == 2 {
            if ref_rhs {
                st.qualify_ref(&p[1])
            } else {
                let sep = st.intent.hierarchy_separator;
                if crate::cpf::is_compound_bool_expr(&p[1], sep) {
                    crate::expr_bv::qualify_logic_expr_in_scope(&p[1], &st.current_scope, sep)
                } else {
                    st.qualify_signal_ref(&p[1])
                }
            }
        } else {
            String::new()
        };
        Ok((p[0].clone(), rhs))
    };
    let mut input_supply_ports = Vec::new();
    for v in args.all("-input_supply_port") {
        input_supply_ports.push(parse_pair(&v[0], true)?);
    }
    let mut output_supply_ports = Vec::new();
    for v in args.all("-output_supply_port") {
        output_supply_ports.push(parse_pair(&v[0], true)?);
    }
    let mut control_ports = Vec::new();
    let mut control_port_scopes = BTreeMap::new();
    for v in args.all("-control_port") {
        let pair = parse_pair(&v[0], false)?;
        control_port_scopes.insert(pair.0.clone(), st.current_scope.clone());
        control_ports.push(pair);
    }
    let mut ack_ports = Vec::new();
    for v in args.all("-ack_port") {
        ack_ports.push(parse_pair(&v[0], false)?);
    }

    let parse_switch_state = |raw: &str, require_port: bool| -> Result<SwitchStateExpr, String> {
        let p = split_tcl_list(raw);
        match (require_port, p.len()) {
            (_, 3) => Ok(SwitchStateExpr {
                state_name: p[0].clone(),
                supply_port: Some(p[1].clone()),
                expr: p[2].clone(),
            }),
            (false, 2) => Ok(SwitchStateExpr {
                state_name: p[0].clone(),
                supply_port: None,
                expr: p[1].clone(),
            }),
            (false, 1) => Ok(SwitchStateExpr {
                state_name: p[0].clone(),
                supply_port: None,
                expr: String::new(),
            }),
            _ => Err(loc_err(
                loc.as_ref(),
                &format!("create_power_switch: malformed state spec `{raw}`"),
            )),
        }
    };

    let mut on_states = Vec::new();
    for v in args.all("-on_state") {
        on_states.push(parse_switch_state(&v[0], true)?);
    }
    let mut off_states = Vec::new();
    for v in args.all("-off_state") {
        off_states.push(parse_switch_state(&v[0], false)?);
    }
    let mut error_states = Vec::new();
    for v in args.all("-error_state") {
        error_states.push(parse_switch_state(&v[0], false)?);
    }

    let id = PowerSwitchId(st.intent.power_switches.len() as u32);
    st.intent.power_switches.push(PowerSwitchDef {
        id,
        name: name.clone(),
        scope: st.current_scope.clone(),
        domain,
        supply_set: args.val("-supply_set").map(|s| st.qualify_handle(s)),
        input_supply_ports,
        output_supply_ports,
        control_ports,
        control_port_scopes,
        shutoff_synthesized_ctrl: false,
        ack_ports,
        on_states,
        off_states,
        error_states,
        mapped_cells: Vec::new(),
        loc,
    });
    Ok(Value::Str(name))
}

pub(crate) fn cmd_map_power_switch(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-domain", 1), ("-lib_cells", 1), ("-port_map", 1)],
        &[],
        &[("-name_prefix", 1), ("-name_suffix", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let switches = split_tcl_list(args.req_pos(0, "power switch name(s)")?)
        .into_iter()
        .map(|s| st.qualify(&s))
        .collect();
    let domain = args.val("-domain").map(|d| st.qualify_ref(d));
    let lib_cells = args
        .all("-lib_cells")
        .flat_map(|v| split_tcl_list(&v[0]))
        .collect();
    let mut port_map = Vec::new();
    for v in args.all("-port_map") {
        let p = split_tcl_list(&v[0]);
        if p.len() == 2 {
            port_map.push((p[0].clone(), p[1].clone()));
        }
    }
    st.intent.deferred_ops.push(DeferredOp::MapPowerSwitch {
        switches,
        scope: st.current_scope.clone(),
        domain,
        lib_cells,
        port_map,
        loc,
    });
    Ok(().into())
}

pub(crate) fn cmd_set_equivalent(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-sets", 1), ("-nets", 1)],
        &["-function_only"],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let sets: Vec<String> = args
        .all("-sets")
        .flat_map(|v| split_tcl_list(&v[0]))
        .map(|s| st.qualify_handle(&s))
        .collect();
    let nets: Vec<String> = args
        .all("-nets")
        .flat_map(|v| st.qualify_ref_list(&v[0]))
        .collect();
    if sets.is_empty() && nets.is_empty() {
        return Err(loc_err(
            loc.as_ref(),
            "set_equivalent: missing -sets or -nets",
        ));
    }
    st.intent.equivalences.push(SupplyEquivalence {
        sets,
        nets,
        function_only: args.bool_val("-function_only")?.unwrap_or(false),
        scope: st.current_scope.clone(),
        loc,
    });
    Ok(().into())
}
