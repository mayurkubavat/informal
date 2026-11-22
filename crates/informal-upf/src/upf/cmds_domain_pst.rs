//! Power domain, power/port state, and PST commands (`create_power_domain`, `add_power_state`, `add_port_state`, `create_pst`, `add_pst_state`).

use std::collections::{BTreeMap, BTreeSet};

use informal_tcl::Value;

use crate::expr_bv::normalize_logic_expr_sep;
use crate::ir::{
    split_tcl_list, trim_hier_path, DeferredOp, DiagSeverity, PortStateDef, PowerDomain,
    PowerDomainId, PowerStateDef, PstId, PstRow, PstTable, SimState, SupplyRailState, UpfDiag,
    UpfLoc, VoltageSpec,
};
use crate::resolve::{lookup_scoped, resolve_power_state_target_ss};

use super::eval::{loc_err, parse_bool_literal, UpfArgs, UpfEvalState};

pub(crate) fn cmd_create_power_domain(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-elements", 1),
            ("-exclude_elements", 1),
            ("-scope", 1),
            ("-supply", 1),
        ],
        &["-include_scope", "-transitive", "-update", "-atomic"],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let raw = args.req_pos(0, "power domain name")?;
    let name = st.qualify_decl(raw);
    let include_scope = args.bool_val("-include_scope")?.unwrap_or(false);
    let transitive = args.bool_val("-transitive")?.unwrap_or(true);
    let update = args.bool_val("-update")?.unwrap_or(false);
    let atomic = args.bool_val("-atomic")?.unwrap_or(false);
    let domain_scope = match args.val("-scope") {
        Some(sc) => st.qualify(sc),
        None => st.current_scope.clone(),
    };
    if args.val("-scope").is_some()
        && st.finder.has_design()
        && !domain_scope.is_empty()
        && !st.finder.scope_exists(&domain_scope)
    {
        let msg = format!("create_power_domain: unresolved scope `{domain_scope}` in design");
        st.intent.diagnostics.push(UpfDiag {
            rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
            severity: DiagSeverity::Error,
            message: msg.clone(),
            loc: loc.clone(),
        });
        return Err(loc_err(loc.as_ref(), &msg));
    }
    let elements: Vec<String> = args
        .all("-elements")
        .flat_map(|v| st.qualify_list_in_scope(&domain_scope, &v[0]))
        .collect();
    let exclude_elements: Vec<String> = args
        .all("-exclude_elements")
        .flat_map(|v| st.qualify_list_in_scope(&domain_scope, &v[0]))
        .collect();
    let mut supply_handles = BTreeMap::new();
    let mut primary_supply_set = None;
    for v in args.all("-supply") {
        let parts = split_tcl_list(&v[0]);
        if parts.len() != 2 {
            return Err(loc_err(
                loc.as_ref(),
                &format!(
                    "create_power_domain: -supply expects `{{handle supply_set}}`, got `{}`",
                    v[0]
                ),
            ));
        }
        let handle = parts[0].trim().to_string();
        let ss = st.qualify_handle(&parts[1]);
        if handle == "primary" {
            primary_supply_set = Some(ss.clone());
        }
        supply_handles.insert(handle, ss);
    }

    let is_top = include_scope && domain_scope.is_empty();
    let id = PowerDomainId(st.intent.domains.len() as u32);
    st.intent.domains.push(PowerDomain {
        id,
        name: name.clone(),
        short_name: trim_hier_path(raw).to_string(),
        scope: domain_scope,
        include_scope,
        elements,
        exclude_elements,
        transitive,
        atomic,
        primary_supply_set,
        supply_handles,
        legacy_primary_power_net: None,
        legacy_primary_ground_net: None,
        is_top,
        update,
        loc,
    });
    Ok(Value::Str(name))
}

pub(crate) fn cmd_add_power_state(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-state", 1), ("-domain", 1), ("-supply_set", 1)],
        &["-supply", "-complete", "-update"],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let target_raw = if let Some(p) = args.pos.first() {
        p.as_str()
    } else if let Some(d) = args.val("-domain") {
        d
    } else if let Some(ss) = args.val("-supply_set") {
        ss
    } else {
        return Err(loc_err(
            loc.as_ref(),
            "add_power_state: missing target supply set or power domain",
        ));
    };
    let target = st.qualify_handle(target_raw);
    let top_update = args.bool_val("-update")?.unwrap_or(false);
    let top_complete = args.bool_val("-complete")?.unwrap_or(false);

    for state_arg in args.all("-state") {
        let tokens = split_tcl_list(&state_arg[0]);
        if tokens.is_empty() {
            return Err(loc_err(loc.as_ref(), "add_power_state: empty -state spec"));
        }
        let sub = UpfArgs::parse_named(
            "add_power_state",
            &tokens,
            &[("-supply_expr", 1), ("-logic_expr", 1), ("-simstate", 1)],
            &["-illegal", "-legal", "-complete", "-update"],
            &[],
            &mut st.intent,
            loc.clone(),
        )?;
        let state_name = sub.req_pos(0, "state name")?.to_string();
        let supply_expr = sub.val("-supply_expr").map(str::to_string);
        let logic_expr = sub
            .val("-logic_expr")
            .map(|s| normalize_logic_expr_sep(s, st.intent.hierarchy_separator));
        let simstate = match sub.val("-simstate") {
            Some(s) => Some(SimState::parse(s).map_err(|e| loc_err(loc.as_ref(), &e))?),
            None => None,
        };
        let _ = sub.bool_val("-illegal")?;
        let _ = sub.bool_val("-legal")?;
        let mut illegal_override = None;
        for (opt_name, vals) in &sub.opts {
            if opt_name == "-illegal" {
                if let Some(b) = vals.first().and_then(|v| parse_bool_literal(v)) {
                    illegal_override = Some(b);
                }
            } else if opt_name == "-legal" {
                if let Some(b) = vals.first().and_then(|v| parse_bool_literal(v)) {
                    illegal_override = Some(!b);
                }
            }
        }
        let illegal = illegal_override.unwrap_or(false);
        let sub_update = sub.bool_val("-update")?.unwrap_or(false);
        let rail_states = match &supply_expr {
            Some(expr) => parse_supply_expr_rails(expr),
            None => Vec::new(),
        };
        let fallback_ss = resolve_cpf_fallback_ss(st, &target);
        if !illegal
            && !top_update
            && !sub_update
            && (!st.cpf_state.emitted_fallback_states.is_empty()
                || !st.cpf_state.cpf_ss_names.is_empty())
        {
            let mut emitted_names = BTreeSet::new();
            if !st.cpf_state.emitted_fallback_states.is_empty() {
                for name in ["ON", "OFF"] {
                    if st
                        .cpf_state
                        .emitted_fallback_states
                        .contains(&(fallback_ss.clone(), name.to_string()))
                    {
                        emitted_names.insert(name.to_string());
                    }
                }
            }
            if !emitted_names.is_empty() {
                let keep: Vec<bool> = st
                    .intent
                    .power_states
                    .iter()
                    .map(|s| {
                        let same_ss =
                            resolve_power_state_target_ss(&st.intent, &s.target, &s.scope)
                                .trim_start_matches('/')
                                == fallback_ss;
                        !(same_ss
                            && (!s.illegal || s.update)
                            && emitted_names.contains(&s.state_name))
                    })
                    .collect();
                let mut idx = 0;
                st.intent.power_states.retain(|_| {
                    let k = keep[idx];
                    idx += 1;
                    k
                });
                st.cpf_state
                    .emitted_fallback_states
                    .remove(&(fallback_ss.clone(), "ON".to_string()));
                st.cpf_state
                    .emitted_fallback_states
                    .remove(&(fallback_ss, "OFF".to_string()));
            } else if st.cpf_state.cpf_ss_names.contains(&fallback_ss) {
                let deferred_names =
                    deferred_cpf_fallback_names(&st.intent, &st.cpf_state, &fallback_ss);
                if !deferred_names.is_empty() {
                    let keep: Vec<bool> = st
                        .intent
                        .power_states
                        .iter()
                        .map(|s| {
                            let same_ss = s.update
                                && deferred_names.contains(&s.state_name)
                                && resolve_power_state_target_ss(&st.intent, &s.target, &s.scope)
                                    .trim_start_matches('/')
                                    == fallback_ss;
                            !same_ss
                        })
                        .collect();
                    let mut idx = 0;
                    st.intent.power_states.retain(|_| {
                        let k = keep[idx];
                        idx += 1;
                        k
                    });
                }
            }
        }
        st.intent.power_states.push(PowerStateDef {
            target: target.clone(),
            scope: st.current_scope.clone(),
            state_name,
            simstate,
            supply_expr,
            logic_expr,
            rail_states,
            illegal,
            illegal_override,
            complete: top_complete || sub.bool_val("-complete")?.unwrap_or(false),
            update: top_update || sub_update,
            loc: loc.clone(),
        });
    }
    Ok(().into())
}

fn resolve_cpf_fallback_ss(st: &UpfEvalState, target: &str) -> String {
    if !st.cpf_state.emitted_fallback_states.is_empty() || !st.cpf_state.cpf_ss_names.is_empty() {
        return resolve_power_state_target_ss(&st.intent, target, &st.current_scope)
            .trim_start_matches('/')
            .to_string();
    }
    target.trim_start_matches('/').to_string()
}

fn deferred_cpf_fallback_names(
    intent: &crate::ir::PowerIntent,
    cs: &crate::cpf::CpfState,
    fallback_ss: &str,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let has_prior_update = intent.power_states.iter().any(|s| {
        s.update
            && (s.state_name == "ON" || s.state_name == "OFF")
            && resolve_power_state_target_ss(intent, &s.target, &s.scope).trim_start_matches('/')
                == fallback_ss
    });
    if !has_prior_update {
        return names;
    }
    let has_explicit_non_update = intent.power_states.iter().any(|s| {
        !s.illegal
            && !s.update
            && resolve_power_state_target_ss(intent, &s.target, &s.scope).trim_start_matches('/')
                == fallback_ss
    });
    if has_explicit_non_update {
        return names;
    }
    for d in &intent.domains {
        let ss_name = crate::cpf::cpf_ss_name(&d.name);
        if ss_name.trim_start_matches('/') != fallback_ss
            || d.primary_supply_set.as_deref() != Some(ss_name.as_str())
        {
            continue;
        }
        let has_explicit_mode = cs.power_modes.iter().any(|pm| {
            !pm.illegal
                && pm
                    .domain_conditions
                    .iter()
                    .any(|(pd, _)| crate::cpf::cpf_ss_name(pd) == ss_name)
        });
        if has_explicit_mode {
            continue;
        }
        names.extend(crate::cpf::finalize::cpf_domain_fallback_state_names(
            intent,
            &cs.domain_enable,
            &cs.domain_shutoff,
            d,
        ));
    }
    names
}

pub(crate) fn cmd_add_port_state(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(a, &[("-state", 1)], &[], &[], &mut st.intent, loc.clone())?;
    args.check_max_pos(1)?;
    let target = st.qualify_ref(args.req_pos(0, "supply port or net name")?);

    let mut defs = Vec::new();
    for state_arg in args.all("-state") {
        let tokens = split_tcl_list(&state_arg[0]);
        if tokens.len() < 2 {
            return Err(loc_err(
                loc.as_ref(),
                &format!(
                    "add_port_state: -state expects `{{name value}}`, got `{}`",
                    state_arg[0]
                ),
            ));
        }
        let state_name = tokens[0].clone();
        let val_str = tokens[1..].join(" ");
        let voltage = VoltageSpec::parse(&val_str).map_err(|e| loc_err(loc.as_ref(), &e))?;
        defs.push(PortStateDef {
            state_name,
            is_off: voltage.is_off(),
            voltage,
            loc: loc.clone(),
        });
    }

    st.intent.deferred_ops.push(DeferredOp::AddPortState {
        target,
        scope: st.current_scope.clone(),
        states: defs,
        loc,
    });
    Ok(().into())
}

pub(crate) fn cmd_create_pst(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-supplies", 1)],
        &[],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let raw = args.req_pos(0, "PST name")?;
    let name = st.qualify_decl(raw);
    let supplies_str = args
        .val("-supplies")
        .ok_or_else(|| loc_err(loc.as_ref(), "create_pst: missing -supplies"))?;
    let supplies = split_tcl_list(supplies_str)
        .into_iter()
        .map(|s| st.qualify_handle(&s))
        .collect();
    let id = PstId(st.intent.pst_tables.len() as u32);
    st.intent.pst_tables.push(PstTable {
        id,
        name: name.clone(),
        scope: st.current_scope.clone(),
        supplies,
        rows: Vec::new(),
        loc,
    });
    Ok(Value::Str(name))
}

pub(crate) fn cmd_add_pst_state(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[("-pst", 1), ("-state", 1)],
        &[],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let state_name = args.req_pos(0, "PST state name")?.to_string();
    let pst_raw = args
        .val("-pst")
        .ok_or_else(|| loc_err(loc.as_ref(), "add_pst_state: missing -pst"))?;
    let pst_ref = st.qualify_ref(pst_raw);
    let row_str = args
        .val("-state")
        .ok_or_else(|| loc_err(loc.as_ref(), "add_pst_state: missing -state"))?;
    let states = split_tcl_list(row_str);
    let table_idx = {
        let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
        for (i, t) in st.intent.pst_tables.iter().enumerate() {
            by_name.insert(t.name.clone(), i);
        }
        lookup_scoped(&by_name, &pst_ref, &st.current_scope)
    }
    .ok_or_else(|| {
        loc_err(
            loc.as_ref(),
            &format!("add_pst_state: unknown PST `{pst_raw}`"),
        )
    })?;
    let table = &mut st.intent.pst_tables[table_idx];
    table.rows.push(PstRow {
        state_name,
        states,
        logic_expr: None,
        scope: String::new(),
        source_scope: None,
        loc,
    });
    Ok(().into())
}

/// Best-effort parser for `-supply_expr` rail clauses (`power == {FULL_ON 0.90} && ...`).
pub(crate) fn parse_supply_expr_rails(expr: &str) -> Vec<SupplyRailState> {
    fn split_top_level<'a>(s: &'a str, sep: &str) -> Vec<&'a str> {
        let mut parts = Vec::new();
        let bytes = s.as_bytes();
        let mut paren_depth = 0usize;
        let mut brace_depth = 0usize;
        let mut start = 0usize;
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'{' => brace_depth += 1,
                b'}' => brace_depth = brace_depth.saturating_sub(1),
                b'(' if brace_depth == 0 => paren_depth += 1,
                b')' if brace_depth == 0 => paren_depth = paren_depth.saturating_sub(1),
                _ if paren_depth == 0 && brace_depth == 0 && s[i..].starts_with(sep) => {
                    parts.push(&s[start..i]);
                    i += sep.len();
                    start = i;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        parts.push(&s[start..]);
        parts
    }

    fn strip_balanced_parens(mut s: &str) -> &str {
        loop {
            let trimmed = s.trim();
            if !(trimmed.starts_with('(') && trimmed.ends_with(')')) {
                return trimmed;
            }
            let bytes = trimmed.as_bytes();
            let mut depth = 0usize;
            let mut brace_depth = 0usize;
            let mut wraps_all = true;
            for (idx, &b) in bytes.iter().enumerate() {
                match b {
                    b'{' => brace_depth += 1,
                    b'}' => brace_depth = brace_depth.saturating_sub(1),
                    b'(' if brace_depth == 0 => depth += 1,
                    b')' if brace_depth == 0 => {
                        depth -= 1;
                        if depth == 0 && idx + 1 < bytes.len() {
                            wraps_all = false;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if wraps_all && depth == 0 {
                s = &trimmed[1..trimmed.len() - 1];
            } else {
                return trimmed;
            }
        }
    }

    fn collect_rails(expr: &str, positive: bool, out: &mut Vec<(SupplyRailState, bool)>) {
        let s = strip_balanced_parens(expr);
        if s.is_empty() {
            return;
        }

        let or_parts = split_top_level(s, "||");
        if or_parts.len() > 1 {
            if positive {
                for part in or_parts {
                    collect_rails(part, true, out);
                }
            } else {
                let mut branch_rails = Vec::new();
                for part in or_parts {
                    collect_rails(part, false, &mut branch_rails);
                }
                let concrete_non_off_funcs: BTreeSet<String> = branch_rails
                    .iter()
                    .filter(|(r, is_neg_off)| !*is_neg_off && !r.is_off)
                    .map(|(r, _)| r.function.clone())
                    .collect();
                for (r, is_neg_off) in branch_rails {
                    if is_neg_off && concrete_non_off_funcs.contains(&r.function) {
                        continue;
                    }
                    out.push((r, is_neg_off));
                }
            }
            return;
        }

        let and_parts = split_top_level(s, "&&");
        if and_parts.len() > 1 {
            if positive {
                let mut branch_rails = Vec::new();
                for part in and_parts {
                    collect_rails(part, true, &mut branch_rails);
                }
                let concrete_non_off_funcs: BTreeSet<String> = branch_rails
                    .iter()
                    .filter(|(r, is_neg_off)| !*is_neg_off && !r.is_off)
                    .map(|(r, _)| r.function.clone())
                    .collect();
                for (r, is_neg_off) in branch_rails {
                    if is_neg_off && concrete_non_off_funcs.contains(&r.function) {
                        continue;
                    }
                    out.push((r, is_neg_off));
                }
            } else {
                for part in and_parts {
                    collect_rails(part, false, out);
                }
            }
            return;
        }

        if let Some(rest) = s.strip_prefix('!') {
            if !rest.starts_with('=') {
                collect_rails(rest, !positive, out);
                return;
            }
        }

        let (lhs, rhs, eq_polarity) = if let Some((l, r)) = s.split_once("==") {
            (l, r, positive)
        } else if let Some((l, r)) = s.split_once("!=") {
            (l, r, !positive)
        } else {
            return;
        };
        let func = lhs.trim().to_ascii_lowercase();
        if func.is_empty() || !func.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return;
        }
        if let Ok(voltage) = VoltageSpec::parse(rhs.trim()) {
            if eq_polarity {
                out.push((
                    SupplyRailState {
                        function: func,
                        is_off: voltage.is_off(),
                        voltage,
                    },
                    false,
                ));
            } else if voltage.is_off() {
                out.push((
                    SupplyRailState {
                        function: func,
                        is_off: false,
                        voltage: VoltageSpec::Undetermined,
                    },
                    true,
                ));
            }
        }
    }

    let mut collected = Vec::new();
    collect_rails(expr, true, &mut collected);
    collected.into_iter().map(|(r, _)| r).collect()
}
