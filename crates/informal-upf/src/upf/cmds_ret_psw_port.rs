//! Version/scope commands, retention strategies, port/design attributes, repeaters, and interface cells.

use std::collections::BTreeMap;

use informal_tcl::Value;

use crate::expr_bv::qualify_logic_expr_in_scope;
use crate::ir::{
    split_tcl_list, AppliesTo, ClampValue, DeferredOp, DesignAttribute, InterfaceCellRule,
    PortAttribute, RepeaterStrategy, RetId, RetentionStrategy, Sense, UpfLoc, UpfVersion,
};

use super::eval::{loc_err, parse_bool_literal, resolve_scope_arg, UpfArgs, UpfEvalState};

pub(crate) fn cmd_upf_version(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(a, &[], &[], &[], &mut st.intent, loc.clone())?;
    args.check_max_pos(1)?;
    if let Some(v) = args.pos.first() {
        let ver = UpfVersion::parse_upf(v).map_err(|e| loc_err(loc.as_ref(), &e))?;
        st.intent.upf_version = Some(ver);
        Ok(Value::Str(ver.as_str().to_string()))
    } else {
        Ok(Value::Str(
            st.intent
                .upf_version
                .unwrap_or(UpfVersion::V2_1)
                .as_str()
                .to_string(),
        ))
    }
}

pub(crate) fn cmd_set_design_top(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(a, &[], &[], &[], &mut st.intent, loc.clone())?;
    args.check_max_pos(1)?;
    let top = args.req_pos(0, "top module name")?;
    st.intent.design_top = Some(top.to_string());
    Ok(Value::Str(top.to_string()))
}

pub(crate) fn cmd_set_scope(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(a, &[], &[], &[], &mut st.intent, loc.clone())?;
    args.check_max_pos(1)?;
    let Some(raw) = args.pos.first() else {
        return Ok(Value::Str(if st.current_scope.is_empty() {
            "/".to_string()
        } else {
            st.current_scope.clone()
        }));
    };
    let sep = st.intent.hierarchy_separator;
    let new_scope = resolve_scope_arg(&st.current_scope, raw, sep);
    if st.finder.has_design() && !new_scope.is_empty() && !st.finder.scope_exists(&new_scope) {
        return Err(loc_err(
            loc.as_ref(),
            &format!("set_scope: unresolved scope `{new_scope}` in design"),
        ));
    }
    st.current_scope = new_scope;
    Ok(Value::Str(if st.current_scope.is_empty() {
        "/".to_string()
    } else {
        st.current_scope.clone()
    }))
}

fn parse_signal_sense_pair(
    raw: &str,
    st: &UpfEvalState,
    loc: Option<&UpfLoc>,
) -> Result<(String, Sense), String> {
    let parts = split_tcl_list(raw);
    if parts.len() != 2 {
        return Err(loc_err(
            loc,
            &format!("expected `{{signal sense}}`, got `{raw}`"),
        ));
    }
    let sig = st.qualify_signal_ref(&parts[0]);
    let sense = Sense::parse(&parts[1]).map_err(|e| loc_err(loc, &e))?;
    Ok((sig, sense))
}

pub(crate) fn cmd_set_retention(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-domain", 1),
            ("-elements", 1),
            ("-exclude_elements", 1),
            ("-retention_supply_set", 1),
            ("-retention_power_net", 1),
            ("-retention_ground_net", 1),
            ("-save_signal", 1),
            ("-restore_signal", 1),
            ("-save_condition", 1),
            ("-restore_condition", 1),
            ("-retention_condition", 1),
            ("-parameters", 1),
        ],
        &["-no_retention", "-use_retention_as_primary", "-update"],
        &[("-name_prefix", 1), ("-name_suffix", 1), ("-instance", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let name = args.req_pos(0, "retention strategy name")?.to_string();
    let domain = st.qualify_ref(
        args.val("-domain")
            .ok_or_else(|| loc_err(loc.as_ref(), "set_retention: missing -domain"))?,
    );
    let (save_signal, save_sense) = match args.val("-save_signal") {
        Some(v) => {
            let (s, sn) = parse_signal_sense_pair(v, st, loc.as_ref())?;
            (Some(s), Some(sn))
        }
        None => (None, None),
    };
    let (restore_signal, restore_sense) = match args.val("-restore_signal") {
        Some(v) => {
            let (s, sn) = parse_signal_sense_pair(v, st, loc.as_ref())?;
            (Some(s), Some(sn))
        }
        None => (None, None),
    };
    let elements: Vec<String> = args
        .all("-elements")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    let exclude_elements: Vec<String> = args
        .all("-exclude_elements")
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
        no_retention: args.bool_val("-no_retention")?.unwrap_or(false),
        retention_supply_set: args
            .val("-retention_supply_set")
            .map(|s| st.qualify_handle(s)),
        retention_power_net: args.val("-retention_power_net").map(|s| st.qualify_ref(s)),
        retention_ground_net: args.val("-retention_ground_net").map(|s| st.qualify_ref(s)),
        save_signal,
        save_sense,
        save_condition: args.val("-save_condition").map(|s| {
            qualify_logic_expr_in_scope(s, &st.current_scope, st.intent.hierarchy_separator)
        }),
        save_scope: None,
        save_cond_scope: None,
        restore_signal,
        restore_sense,
        restore_condition: args.val("-restore_condition").map(|s| {
            qualify_logic_expr_in_scope(s, &st.current_scope, st.intent.hierarchy_separator)
        }),
        restore_scope: None,
        restore_cond_scope: None,
        retention_condition: args.val("-retention_condition").map(|s| {
            qualify_logic_expr_in_scope(s, &st.current_scope, st.intent.hierarchy_separator)
        }),
        retention_cond_scope: None,
        use_retention_as_primary: args.bool_val("-use_retention_as_primary")?.unwrap_or(false),
        mapped_cells: Vec::new(),
        mapped_lib_model: None,
        precedence_order: order,
        update: args.bool_val("-update")?.unwrap_or(false),
        loc,
    });
    Ok(Value::Str(name))
}

pub(crate) fn cmd_set_retention_control(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-domain", 1),
            ("-save_signal", 1),
            ("-restore_signal", 1),
            ("-save_condition", 1),
            ("-restore_condition", 1),
            ("-assert_r_mutex", 1),
            ("-assert_s_mutex", 1),
            ("-assert_rs_mutex", 1),
        ],
        &[],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let name = args.req_pos(0, "retention strategy name")?.to_string();
    let domain = st.qualify_ref(
        args.val("-domain")
            .ok_or_else(|| loc_err(loc.as_ref(), "set_retention_control: missing -domain"))?,
    );
    let save_signal = match args.val("-save_signal") {
        Some(v) => Some(parse_signal_sense_pair(v, st, loc.as_ref())?),
        None => None,
    };
    let restore_signal = match args.val("-restore_signal") {
        Some(v) => Some(parse_signal_sense_pair(v, st, loc.as_ref())?),
        None => None,
    };
    st.intent
        .deferred_ops
        .push(DeferredOp::SetRetentionControl {
            name,
            scope: st.current_scope.clone(),
            domain,
            save_signal,
            restore_signal,
            save_condition: args.val("-save_condition").map(|s| {
                qualify_logic_expr_in_scope(s, &st.current_scope, st.intent.hierarchy_separator)
            }),
            restore_condition: args.val("-restore_condition").map(|s| {
                qualify_logic_expr_in_scope(s, &st.current_scope, st.intent.hierarchy_separator)
            }),
            assert_r_mutex: args.val("-assert_r_mutex").map(str::to_string),
            assert_s_mutex: args.val("-assert_s_mutex").map(str::to_string),
            assert_rs_mutex: args.val("-assert_rs_mutex").map(str::to_string),
            loc,
        });
    Ok(().into())
}

pub(crate) fn cmd_set_retention_elements(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-domain", 1),
            ("-elements", 1),
            ("-exclude_elements", 1),
            ("-applies_to", 1),
            ("-retention_purpose", 1),
        ],
        &[],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let name = args.req_pos(0, "retention elements list name")?.to_string();
    let domain = match args.val("-domain") {
        Some(d) => st.qualify_ref(d),
        None => String::new(),
    };
    let elements = args
        .all("-elements")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    let exclude_elements = args
        .all("-exclude_elements")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    st.intent
        .deferred_ops
        .push(DeferredOp::SetRetentionElements {
            name,
            scope: st.current_scope.clone(),
            domain,
            elements,
            exclude_elements,
            loc,
        });
    Ok(().into())
}

pub(crate) fn cmd_map_retention_cell(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-domain", 1),
            ("-lib_cells", 1),
            ("-lib_cell_type", 1),
            ("-lib_model_name", 1),
            ("-elements", 1),
        ],
        &[],
        &[("-name_prefix", 1), ("-name_suffix", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let name = args.req_pos(0, "retention strategy name")?.to_string();
    let domain = st.qualify_ref(
        args.val("-domain")
            .ok_or_else(|| loc_err(loc.as_ref(), "map_retention_cell: missing -domain"))?,
    );
    let lib_cells = args
        .all("-lib_cells")
        .flat_map(|v| split_tcl_list(&v[0]))
        .collect();
    let elements = args
        .all("-elements")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    st.intent.deferred_ops.push(DeferredOp::MapRetentionCell {
        name,
        scope: st.current_scope.clone(),
        domain,
        lib_cells,
        lib_cell_type: args.val("-lib_cell_type").map(str::to_string),
        lib_model_name: args.val("-lib_model_name").map(str::to_string),
        elements,
        loc,
    });
    Ok(().into())
}

pub(crate) fn cmd_set_port_attributes(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-ports", 1),
            ("-elements", 1),
            ("-exclude_elements", 1),
            ("-domains", 1),
            ("-applies_to", 1),
            ("-driver_supply", 1),
            ("-receiver_supply", 1),
            ("-related_power_port", 1),
            ("-related_ground_port", 1),
            ("-related_bias_ports", 1),
            ("-clamp_value", 1),
            ("-repeater_supply", 1),
            ("-attribute", 1),
            ("-model", 1),
        ],
        &["-is_isolated"],
        &[("-unconnected_ports", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let ports = args
        .all("-ports")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    let elements = args
        .all("-elements")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    let exclude_elements = args
        .all("-exclude_elements")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    let domains = args
        .all("-domains")
        .flat_map(|v| st.qualify_ref_list(&v[0]))
        .collect();
    let applies_to = match args.val("-applies_to") {
        Some(v) => Some(AppliesTo::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let clamp_value = match args.val("-clamp_value") {
        Some(v) => Some(ClampValue::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let is_isolated = args.bool_val("-is_isolated")?;
    let mut attributes = BTreeMap::new();
    for v in args.all("-attribute") {
        let p = split_tcl_list(&v[0]);
        if p.len() == 2 {
            attributes.insert(p[0].clone(), p[1].clone());
        } else if p.len() == 1 {
            attributes.insert(p[0].clone(), "true".to_string());
        }
    }
    let is_hard_macro = attributes
        .get("is_hard_macro")
        .and_then(|v| parse_bool_literal(v))
        .unwrap_or(false);
    st.intent.port_attributes.push(PortAttribute {
        ports,
        elements,
        exclude_elements,
        domains,
        applies_to,
        scope: st.current_scope.clone(),
        driver_supply: args.val("-driver_supply").map(|s| st.qualify_handle(s)),
        receiver_supply: args.val("-receiver_supply").map(|s| st.qualify_handle(s)),
        related_power_port: args.val("-related_power_port").map(|s| st.qualify_ref(s)),
        related_ground_port: args.val("-related_ground_port").map(|s| st.qualify_ref(s)),
        related_bias_ports: args
            .all("-related_bias_ports")
            .flat_map(|v| st.qualify_ref_list(&v[0]))
            .collect(),
        clamp_value,
        repeater_supply: args.val("-repeater_supply").map(|s| st.qualify_handle(s)),
        is_isolated,
        is_hard_macro,
        model: args.val("-model").map(str::to_string),
        attributes,
        loc,
    });
    Ok(().into())
}

pub(crate) fn cmd_set_repeater(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-domain", 1),
            ("-elements", 1),
            ("-exclude_elements", 1),
            ("-repeater_supply", 1),
            ("-applies_to", 1),
            ("-source", 1),
            ("-sink", 1),
        ],
        &[],
        &[("-name_prefix", 1), ("-name_suffix", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let name = args.req_pos(0, "repeater strategy name")?.to_string();
    let domain = st.qualify_ref(
        args.val("-domain")
            .ok_or_else(|| loc_err(loc.as_ref(), "set_repeater: missing -domain"))?,
    );
    let applies_to = match args.val("-applies_to") {
        Some(v) => Some(AppliesTo::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    st.intent.repeater_strategies.push(RepeaterStrategy {
        name: name.clone(),
        scope: st.current_scope.clone(),
        domain,
        applies_to,
        elements: args
            .all("-elements")
            .flat_map(|v| st.qualify_list(&v[0]))
            .collect(),
        exclude_elements: args
            .all("-exclude_elements")
            .flat_map(|v| st.qualify_list(&v[0]))
            .collect(),
        source_filter: args.val("-source").map(|s| st.qualify_handle(s)),
        sink_filter: args.val("-sink").map(|s| st.qualify_handle(s)),
        repeater_supply: args.val("-repeater_supply").map(|s| st.qualify_handle(s)),
        mapped_cells: Vec::new(),
        loc,
    });
    Ok(Value::Str(name))
}

pub(crate) fn cmd_set_design_attributes(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-elements", 1),
            ("-exclude_elements", 1),
            ("-models", 1),
            ("-attribute", 1),
        ],
        &["-is_hard_macro"],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(0)?;
    let mut attributes = BTreeMap::new();
    for v in args.all("-attribute") {
        let p = split_tcl_list(&v[0]);
        if p.len() == 2 {
            attributes.insert(p[0].clone(), p[1].clone());
        } else if p.len() == 1 {
            attributes.insert(p[0].clone(), "true".to_string());
        }
    }
    let is_hard_macro = args.bool_val("-is_hard_macro")?.unwrap_or(false)
        || attributes
            .get("is_hard_macro")
            .and_then(|v| parse_bool_literal(v))
            .unwrap_or(false);
    st.intent.design_attributes.push(DesignAttribute {
        elements: args
            .all("-elements")
            .flat_map(|v| st.qualify_list(&v[0]))
            .collect(),
        exclude_elements: args
            .all("-exclude_elements")
            .flat_map(|v| st.qualify_list(&v[0]))
            .collect(),
        models: args
            .all("-models")
            .flat_map(|v| split_tcl_list(&v[0]))
            .collect(),
        scope: st.current_scope.clone(),
        is_hard_macro,
        attributes,
        loc,
    });
    Ok(().into())
}

pub(crate) fn cmd_use_interface_cell(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-domain", 1),
            ("-strategy", 1),
            ("-lib_cells", 1),
            ("-elements", 1),
            ("-map_supply_set", 1),
        ],
        &["-update"],
        &[("-name_prefix", 1), ("-name_suffix", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let name = args.req_pos(0, "interface cell rule name")?.to_string();
    let domain = st.qualify_ref(
        args.val("-domain")
            .ok_or_else(|| loc_err(loc.as_ref(), "use_interface_cell: missing -domain"))?,
    );
    let mut map_supply_set = Vec::new();
    for v in args.all("-map_supply_set") {
        let p = split_tcl_list(&v[0]);
        if p.len() == 2 {
            map_supply_set.push((p[0].clone(), st.qualify_handle(&p[1])));
        }
    }
    st.intent.interface_cells.push(InterfaceCellRule {
        name: name.clone(),
        scope: st.current_scope.clone(),
        domain,
        strategy: args.val("-strategy").map(str::to_string),
        elements: args
            .all("-elements")
            .flat_map(|v| st.qualify_list(&v[0]))
            .collect(),
        lib_cells: args
            .all("-lib_cells")
            .flat_map(|v| split_tcl_list(&v[0]))
            .collect(),
        map_supply_set,
        update: args.bool_val("-update")?.unwrap_or(false),
        loc,
    });
    Ok(Value::Str(name))
}
