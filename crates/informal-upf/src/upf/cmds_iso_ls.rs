//! Isolation and level-shifter strategy, control, and cell-mapping commands.

use informal_tcl::Value;

use crate::ir::{
    parse_uv_scalar, split_tcl_list, AppliesTo, ClampValue, DeferredOp, IsoId, IsolationStrategy,
    LevelShifterStrategy, LsId, LsRule, Sense, StrategyLocation, UpfLoc,
};

use super::eval::{loc_err, UpfArgs, UpfEvalState};

pub(crate) fn cmd_set_isolation(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-domain", 1),
            ("-applies_to", 1),
            ("-applies_to_clamp", 1),
            ("-clamp_value", 1),
            ("-elements", 1),
            ("-exclude_elements", 1),
            ("-source", 1),
            ("-sink", 1),
            ("-isolation_supply_set", 1),
            ("-isolation_power_net", 1),
            ("-isolation_ground_net", 1),
            ("-isolation_signal", 1),
            ("-isolation_sense", 1),
            ("-location", 1),
        ],
        &[
            "-diff_supply_only",
            "-no_isolation",
            "-force_isolation",
            "-transitively_included",
            "-update",
        ],
        &[
            ("-name_prefix", 1),
            ("-name_suffix", 1),
            ("-instance", 1),
            ("-async_set_reset", 1),
        ],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let name = args.req_pos(0, "isolation strategy name")?.to_string();
    let domain = st.qualify_ref(
        args.val("-domain")
            .ok_or_else(|| loc_err(loc.as_ref(), "set_isolation: missing -domain"))?,
    );
    let applies_to = match args.val("-applies_to") {
        Some(v) => Some(AppliesTo::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let applies_to_clamp = match args.val("-applies_to_clamp") {
        Some(v) => Some(ClampValue::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let clamp_value = match args.val("-clamp_value") {
        Some(v) => Some(ClampValue::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let isolation_sense = match args.val("-isolation_sense") {
        Some(v) => Some(Sense::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let location = match args.val("-location") {
        Some(v) => Some(StrategyLocation::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
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
    let id = IsoId(st.intent.isolation_strategies.len() as u32);
    st.intent.isolation_strategies.push(IsolationStrategy {
        id,
        name: name.clone(),
        scope: st.current_scope.clone(),
        domain,
        applies_to,
        applies_to_clamp,
        elements,
        exclude_elements,
        source_filter: args.val("-source").map(|s| st.qualify_handle(s)),
        sink_filter: args.val("-sink").map(|s| st.qualify_handle(s)),
        diff_supply_only: args.bool_val("-diff_supply_only")?.unwrap_or(false),
        no_isolation: args.bool_val("-no_isolation")?.unwrap_or(false),
        force_isolation: args.bool_val("-force_isolation")?.unwrap_or(false),
        clamp_value,
        isolation_supply_set: args
            .val("-isolation_supply_set")
            .map(|s| st.qualify_handle(s)),
        isolation_power_net: args.val("-isolation_power_net").map(|s| st.qualify_ref(s)),
        isolation_ground_net: args.val("-isolation_ground_net").map(|s| st.qualify_ref(s)),
        isolation_signal: args.val("-isolation_signal").map(|s| {
            let sep = st.intent.hierarchy_separator;
            if crate::cpf::is_compound_bool_expr(s, sep) {
                crate::expr_bv::qualify_logic_expr_in_scope(s, &st.current_scope, sep)
            } else {
                st.qualify_signal_ref(s)
            }
        }),
        isolation_sense,
        ctrl_scope: None,
        location,
        mapped_cells: Vec::new(),
        mapped_lib_model: None,
        precedence_order: order,
        update: args.bool_val("-update")?.unwrap_or(false),
        loc,
    });
    Ok(Value::Str(name))
}

pub(crate) fn cmd_set_isolation_control(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-domain", 1),
            ("-isolation_signal", 1),
            ("-isolation_sense", 1),
            ("-location", 1),
        ],
        &[],
        &[("-name_prefix", 1), ("-name_suffix", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let name = args.req_pos(0, "isolation strategy name")?.to_string();
    let domain = st.qualify_ref(
        args.val("-domain")
            .ok_or_else(|| loc_err(loc.as_ref(), "set_isolation_control: missing -domain"))?,
    );
    let isolation_sense = match args.val("-isolation_sense") {
        Some(v) => Some(Sense::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let location = match args.val("-location") {
        Some(v) => Some(StrategyLocation::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    st.intent
        .deferred_ops
        .push(DeferredOp::SetIsolationControl {
            name,
            scope: st.current_scope.clone(),
            domain,
            isolation_signal: args.val("-isolation_signal").map(|s| {
                let sep = st.intent.hierarchy_separator;
                if crate::cpf::is_compound_bool_expr(s, sep) {
                    crate::expr_bv::qualify_logic_expr_in_scope(s, &st.current_scope, sep)
                } else {
                    st.qualify_signal_ref(s)
                }
            }),
            isolation_sense,
            location,
            loc,
        });
    Ok(().into())
}

pub(crate) fn cmd_map_isolation_cell(
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
    let name = args.req_pos(0, "isolation strategy name")?.to_string();
    let domain = st.qualify_ref(
        args.val("-domain")
            .ok_or_else(|| loc_err(loc.as_ref(), "map_isolation_cell: missing -domain"))?,
    );
    let lib_cells = args
        .all("-lib_cells")
        .flat_map(|v| split_tcl_list(&v[0]))
        .collect();
    let elements = args
        .all("-elements")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    st.intent.deferred_ops.push(DeferredOp::MapIsolationCell {
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

pub(crate) fn cmd_set_level_shifter(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-domain", 1),
            ("-applies_to", 1),
            ("-rule", 1),
            ("-threshold", 1),
            ("-elements", 1),
            ("-exclude_elements", 1),
            ("-source", 1),
            ("-sink", 1),
            ("-location", 1),
            ("-input_supply_set", 1),
            ("-output_supply_set", 1),
            ("-internal_supply_set", 1),
        ],
        &["-no_shift", "-force_shift", "-update"],
        &[("-name_prefix", 1), ("-name_suffix", 1), ("-instance", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let name = args.req_pos(0, "level shifter strategy name")?.to_string();
    let domain = st.qualify_ref(
        args.val("-domain")
            .ok_or_else(|| loc_err(loc.as_ref(), "set_level_shifter: missing -domain"))?,
    );
    let applies_to = match args.val("-applies_to") {
        Some(v) => Some(AppliesTo::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let rule = match args.val("-rule") {
        Some(v) => Some(LsRule::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let threshold_uv = match args.val("-threshold") {
        Some(v) => Some(parse_uv_scalar(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let location = match args.val("-location") {
        Some(v) => Some(StrategyLocation::parse(v).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
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
    let id = LsId(st.intent.level_shifter_strategies.len() as u32);
    st.intent
        .level_shifter_strategies
        .push(LevelShifterStrategy {
            id,
            name: name.clone(),
            scope: st.current_scope.clone(),
            domain,
            applies_to,
            rule,
            threshold_uv,
            elements,
            exclude_elements,
            source_filter: args.val("-source").map(|s| st.qualify_handle(s)),
            sink_filter: args.val("-sink").map(|s| st.qualify_handle(s)),
            no_shift: args.bool_val("-no_shift")?.unwrap_or(false),
            force_shift: args.bool_val("-force_shift")?.unwrap_or(false),
            location,
            input_supply_set: args.val("-input_supply_set").map(|s| st.qualify_handle(s)),
            output_supply_set: args.val("-output_supply_set").map(|s| st.qualify_handle(s)),
            internal_supply_set: args
                .val("-internal_supply_set")
                .map(|s| st.qualify_handle(s)),
            mapped_cells: Vec::new(),
            precedence_order: order,
            update: args.bool_val("-update")?.unwrap_or(false),
            loc,
        });
    Ok(Value::Str(name))
}

pub(crate) fn cmd_map_level_shifter_cell(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-domain", 1), ("-lib_cells", 1), ("-elements", 1)],
        &[],
        &[("-name_prefix", 1), ("-name_suffix", 1)],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let name = args.req_pos(0, "level shifter strategy name")?.to_string();
    let domain = st.qualify_ref(
        args.val("-domain")
            .ok_or_else(|| loc_err(loc.as_ref(), "map_level_shifter_cell: missing -domain"))?,
    );
    let lib_cells = args
        .all("-lib_cells")
        .flat_map(|v| split_tcl_list(&v[0]))
        .collect();
    let elements = args
        .all("-elements")
        .flat_map(|v| st.qualify_list(&v[0]))
        .collect();
    st.intent
        .deferred_ops
        .push(DeferredOp::MapLevelShifterCell {
            name,
            scope: st.current_scope.clone(),
            domain,
            lib_cells,
            elements,
            loc,
        });
    Ok(().into())
}
