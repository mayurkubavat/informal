//! Strategy `-update` merging, deferred `_control`/`_elements`/`map_*` refinement,
//! and CPF `C1` dual-separator rule name matching.

use std::collections::BTreeMap;

use crate::ir::{
    escape_rtl_brackets, escape_sv_escaped_backslashes_for_glob, glob, normalize_scope,
    push_norm_segments, split_hier_segments, trim_hier_path, DeferredOp, DiagSeverity,
    IsolationStrategy, LevelShifterStrategy, PowerDomainId, PowerIntent, RetentionStrategy,
    SupplySetId, UpfDiag, UpfLoc,
};

use super::supply_graph::{lookup_scoped, resolve_ss_handle};

pub(crate) fn same_domain_ref(
    domain_by_name: &BTreeMap<String, PowerDomainId>,
    a_dom: &str,
    a_scope: &str,
    b_dom: &str,
    b_scope: &str,
) -> bool {
    if a_dom == b_dom {
        return true;
    }
    match (
        lookup_scoped(domain_by_name, a_dom, a_scope),
        lookup_scoped(domain_by_name, b_dom, b_scope),
    ) {
        (Some(id_a), Some(id_b)) => id_a == id_b,
        _ => false,
    }
}

pub(crate) fn apply_strategy_refinements(
    intent: &mut PowerIntent,
    ops: &[DeferredOp],
    domain_by_name: &BTreeMap<String, PowerDomainId>,
    ss_by_name: &BTreeMap<String, SupplySetId>,
) -> Vec<(String, String, String, Option<UpfLoc>)> {
    let mut unmerged_deferred: Vec<(String, String, String, Option<UpfLoc>)> = Vec::new();

    let mut merged_iso: Vec<IsolationStrategy> = Vec::new();
    for iso in intent.isolation_strategies.drain(..) {
        if iso.update {
            if let Some(base) = merged_iso.iter_mut().find(|b| {
                b.name == iso.name
                    && same_domain_ref(domain_by_name, &b.domain, &b.scope, &iso.domain, &iso.scope)
            }) {
                if iso.applies_to.is_some() {
                    base.applies_to = iso.applies_to;
                }
                if iso.applies_to_clamp.is_some() {
                    base.applies_to_clamp = iso.applies_to_clamp;
                }
                if !iso.elements.is_empty() {
                    base.elements.extend(iso.elements);
                }
                if !iso.exclude_elements.is_empty() {
                    base.exclude_elements.extend(iso.exclude_elements);
                }
                if iso.source_filter.is_some() {
                    base.source_filter = iso.source_filter;
                }
                if iso.sink_filter.is_some() {
                    base.sink_filter = iso.sink_filter;
                }
                if iso.diff_supply_only {
                    base.diff_supply_only = true;
                }
                if iso.no_isolation {
                    base.no_isolation = true;
                }
                if iso.force_isolation {
                    base.force_isolation = true;
                }
                if iso.clamp_value.is_some() {
                    base.clamp_value = iso.clamp_value;
                }
                if iso.isolation_supply_set.is_some() {
                    base.isolation_supply_set = iso.isolation_supply_set;
                }
                if iso.isolation_power_net.is_some() {
                    base.isolation_power_net = iso.isolation_power_net;
                }
                if iso.isolation_ground_net.is_some() {
                    base.isolation_ground_net = iso.isolation_ground_net;
                }
                if iso.isolation_signal.is_some() {
                    base.isolation_signal = iso.isolation_signal;
                    base.ctrl_scope = iso.ctrl_scope.or(Some(iso.scope.clone()));
                }
                if iso.isolation_sense.is_some() {
                    base.isolation_sense = iso.isolation_sense;
                }
                if iso.location.is_some() {
                    base.location = iso.location;
                }
                if !iso.mapped_cells.is_empty() {
                    base.mapped_cells.extend(iso.mapped_cells);
                }
                if iso.mapped_lib_model.is_some() {
                    base.mapped_lib_model = iso.mapped_lib_model;
                }
                base.precedence_order = iso.precedence_order;
                continue;
            }
        }
        merged_iso.push(iso);
    }
    intent.isolation_strategies = merged_iso;

    let mut merged_ls: Vec<LevelShifterStrategy> = Vec::new();
    for ls in intent.level_shifter_strategies.drain(..) {
        if ls.update {
            if let Some(base) = merged_ls.iter_mut().find(|b| {
                b.name == ls.name
                    && same_domain_ref(domain_by_name, &b.domain, &b.scope, &ls.domain, &ls.scope)
            }) {
                if ls.applies_to.is_some() {
                    base.applies_to = ls.applies_to;
                }
                if ls.rule.is_some() {
                    base.rule = ls.rule;
                }
                if ls.threshold_uv.is_some() {
                    base.threshold_uv = ls.threshold_uv;
                }
                if !ls.elements.is_empty() {
                    base.elements.extend(ls.elements);
                }
                if !ls.exclude_elements.is_empty() {
                    base.exclude_elements.extend(ls.exclude_elements);
                }
                if ls.source_filter.is_some() {
                    base.source_filter = ls.source_filter;
                }
                if ls.sink_filter.is_some() {
                    base.sink_filter = ls.sink_filter;
                }
                if ls.no_shift {
                    base.no_shift = true;
                }
                if ls.force_shift {
                    base.force_shift = true;
                }
                if ls.location.is_some() {
                    base.location = ls.location;
                }
                if ls.input_supply_set.is_some() {
                    base.input_supply_set = ls.input_supply_set;
                }
                if ls.output_supply_set.is_some() {
                    base.output_supply_set = ls.output_supply_set;
                }
                if ls.internal_supply_set.is_some() {
                    base.internal_supply_set = ls.internal_supply_set;
                }
                if !ls.mapped_cells.is_empty() {
                    base.mapped_cells.extend(ls.mapped_cells);
                }
                base.precedence_order = ls.precedence_order;
                continue;
            }
        }
        merged_ls.push(ls);
    }
    intent.level_shifter_strategies = merged_ls;

    let mut merged_ret: Vec<RetentionStrategy> = Vec::new();
    for ret in intent.retention_strategies.drain(..) {
        if ret.update {
            if let Some(base) = merged_ret.iter_mut().find(|b| {
                b.name == ret.name
                    && same_domain_ref(domain_by_name, &b.domain, &b.scope, &ret.domain, &ret.scope)
            }) {
                if !ret.elements.is_empty() {
                    base.elements.extend(ret.elements);
                }
                if !ret.exclude_elements.is_empty() {
                    base.exclude_elements.extend(ret.exclude_elements);
                }
                if ret.no_retention {
                    base.no_retention = true;
                }
                if ret.retention_supply_set.is_some() {
                    base.retention_supply_set = ret.retention_supply_set;
                }
                if ret.retention_power_net.is_some() {
                    base.retention_power_net = ret.retention_power_net;
                }
                if ret.retention_ground_net.is_some() {
                    base.retention_ground_net = ret.retention_ground_net;
                }
                if ret.save_signal.is_some() {
                    base.save_signal = ret.save_signal;
                    base.save_sense = ret.save_sense;
                    base.save_scope = ret.save_scope.or_else(|| Some(ret.scope.clone()));
                }
                if ret.restore_signal.is_some() {
                    base.restore_signal = ret.restore_signal;
                    base.restore_sense = ret.restore_sense;
                    base.restore_scope = ret.restore_scope.or_else(|| Some(ret.scope.clone()));
                }
                if ret.save_condition.is_some() {
                    base.save_condition = ret.save_condition;
                    base.save_cond_scope = ret.save_cond_scope.or_else(|| Some(ret.scope.clone()));
                }
                if ret.restore_condition.is_some() {
                    base.restore_condition = ret.restore_condition;
                    base.restore_cond_scope =
                        ret.restore_cond_scope.or_else(|| Some(ret.scope.clone()));
                }
                if ret.retention_condition.is_some() {
                    base.retention_condition = ret.retention_condition;
                    base.retention_cond_scope =
                        ret.retention_cond_scope.or_else(|| Some(ret.scope.clone()));
                }
                if ret.use_retention_as_primary {
                    base.use_retention_as_primary = true;
                }
                if !ret.mapped_cells.is_empty() {
                    base.mapped_cells.extend(ret.mapped_cells);
                }
                if ret.mapped_lib_model.is_some() {
                    base.mapped_lib_model = ret.mapped_lib_model;
                }
                base.precedence_order = ret.precedence_order;
                continue;
            }
        }
        merged_ret.push(ret);
    }
    intent.retention_strategies = merged_ret;

    for op in ops {
        match op {
            DeferredOp::SetIsolationControl {
                name,
                scope,
                domain,
                isolation_signal,
                isolation_sense,
                location,
                loc,
            } => {
                if let Some(iso) = intent.isolation_strategies.iter_mut().find(|i| {
                    i.name == *name
                        && same_domain_ref(domain_by_name, &i.domain, &i.scope, domain, scope)
                }) {
                    if isolation_signal.is_some() {
                        iso.isolation_signal = isolation_signal.clone();
                        iso.ctrl_scope = Some(scope.clone());
                    }
                    if isolation_sense.is_some() {
                        iso.isolation_sense = *isolation_sense;
                    }
                    if location.is_some() {
                        iso.location = *location;
                    }
                } else {
                    intent.diagnostics.push(UpfDiag {
                        rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                        severity: DiagSeverity::Error,
                        message: format!(
                            "set_isolation_control: unknown isolation strategy `{name}` on `{domain}`"
                        ),
                        loc: loc.clone(),
                    });
                }
            }
            DeferredOp::MapIsolationCell {
                name,
                scope,
                domain,
                lib_cells,
                lib_model_name,
                elements,
                loc,
                ..
            } => {
                if let Some(iso) = intent.isolation_strategies.iter_mut().find(|i| {
                    i.name == *name
                        && same_domain_ref(domain_by_name, &i.domain, &i.scope, domain, scope)
                }) {
                    iso.mapped_cells.extend(lib_cells.clone());
                    if lib_model_name.is_some() {
                        iso.mapped_lib_model = lib_model_name.clone();
                    }
                    iso.elements.extend(elements.clone());
                } else {
                    for elem in elements {
                        unmerged_deferred.push((
                            format!("map_isolation_cell `{name}`"),
                            scope.clone(),
                            elem.clone(),
                            loc.clone(),
                        ));
                    }
                }
            }
            DeferredOp::MapLevelShifterCell {
                name,
                scope,
                domain,
                lib_cells,
                elements,
                loc,
            } => {
                if let Some(ls) = intent.level_shifter_strategies.iter_mut().find(|l| {
                    l.name == *name
                        && same_domain_ref(domain_by_name, &l.domain, &l.scope, domain, scope)
                }) {
                    ls.mapped_cells.extend(lib_cells.clone());
                    ls.elements.extend(elements.clone());
                } else {
                    for elem in elements {
                        unmerged_deferred.push((
                            format!("map_level_shifter_cell `{name}`"),
                            scope.clone(),
                            elem.clone(),
                            loc.clone(),
                        ));
                    }
                }
            }
            DeferredOp::SetRetentionControl {
                name,
                scope,
                domain,
                save_signal,
                restore_signal,
                save_condition,
                restore_condition,
                loc,
                ..
            } => {
                if let Some(ret) = intent.retention_strategies.iter_mut().find(|r| {
                    r.name == *name
                        && same_domain_ref(domain_by_name, &r.domain, &r.scope, domain, scope)
                }) {
                    if let Some((sig, sense)) = save_signal {
                        ret.save_signal = Some(sig.clone());
                        ret.save_sense = Some(*sense);
                        ret.save_scope = Some(scope.clone());
                    }
                    if let Some((sig, sense)) = restore_signal {
                        ret.restore_signal = Some(sig.clone());
                        ret.restore_sense = Some(*sense);
                        ret.restore_scope = Some(scope.clone());
                    }
                    if save_condition.is_some() {
                        ret.save_condition = save_condition.clone();
                        ret.save_cond_scope = Some(scope.clone());
                    }
                    if restore_condition.is_some() {
                        ret.restore_condition = restore_condition.clone();
                        ret.restore_cond_scope = Some(scope.clone());
                    }
                } else {
                    intent.diagnostics.push(UpfDiag {
                        rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                        severity: DiagSeverity::Error,
                        message: format!(
                            "set_retention_control: unknown retention strategy `{name}` on `{domain}`"
                        ),
                        loc: loc.clone(),
                    });
                }
            }
            DeferredOp::SetRetentionElements {
                name,
                scope,
                domain,
                elements,
                exclude_elements,
                loc,
            } => {
                let mut matched = false;
                for ret in &mut intent.retention_strategies {
                    if (domain.is_empty()
                        || same_domain_ref(domain_by_name, &ret.domain, &ret.scope, domain, scope))
                        && (ret.name == *name || ret.elements.contains(name))
                    {
                        ret.elements.extend(elements.clone());
                        ret.exclude_elements.extend(exclude_elements.clone());
                        matched = true;
                    }
                }
                if !matched && !domain.is_empty() {
                    if let Some(ret) = intent.retention_strategies.iter_mut().find(|r| {
                        same_domain_ref(domain_by_name, &r.domain, &r.scope, domain, scope)
                    }) {
                        ret.elements.extend(elements.clone());
                        ret.exclude_elements.extend(exclude_elements.clone());
                        matched = true;
                    }
                }
                if !matched {
                    for elem in elements {
                        unmerged_deferred.push((
                            format!("set_retention_elements `{name}`"),
                            scope.clone(),
                            elem.clone(),
                            loc.clone(),
                        ));
                    }
                }
            }
            DeferredOp::MapRetentionCell {
                name,
                scope,
                domain,
                lib_cells,
                lib_model_name,
                elements,
                loc,
                ..
            } => {
                if let Some(ret) = intent.retention_strategies.iter_mut().find(|r| {
                    r.name == *name
                        && same_domain_ref(domain_by_name, &r.domain, &r.scope, domain, scope)
                }) {
                    ret.mapped_cells.extend(lib_cells.clone());
                    if lib_model_name.is_some() {
                        ret.mapped_lib_model = lib_model_name.clone();
                    }
                    ret.elements.extend(elements.clone());
                } else {
                    for elem in elements {
                        unmerged_deferred.push((
                            format!("map_retention_cell `{name}`"),
                            scope.clone(),
                            elem.clone(),
                            loc.clone(),
                        ));
                    }
                }
            }
            DeferredOp::MapPowerSwitch {
                switches,
                lib_cells,
                ..
            } => {
                for sw_name in switches {
                    if let Some(sw) = intent
                        .power_switches
                        .iter_mut()
                        .find(|s| s.name == *sw_name)
                    {
                        sw.mapped_cells.extend(lib_cells.clone());
                    }
                }
            }
            DeferredOp::UpdateCpfIso {
                names,
                scope,
                sep,
                location,
                lib_cells,
                ..
            } => {
                for iso in &mut intent.isolation_strategies {
                    if cpf_rule_matches(scope, *sep, &iso.scope, &iso.name, names) {
                        if location.is_some() {
                            iso.location = *location;
                        }
                        iso.mapped_cells.extend(lib_cells.clone());
                    }
                }
            }
            DeferredOp::UpdateCpfLs {
                names,
                scope,
                sep,
                location,
                lib_cells,
                ..
            } => {
                for ls in &mut intent.level_shifter_strategies {
                    if cpf_rule_matches(scope, *sep, &ls.scope, &ls.name, names) {
                        if location.is_some() {
                            ls.location = *location;
                        }
                        ls.mapped_cells.extend(lib_cells.clone());
                    }
                }
            }
            DeferredOp::UpdateCpfRet {
                names,
                scope,
                sep,
                lib_cells,
                use_secondary_for_output,
                ..
            } => {
                for ret in &mut intent.retention_strategies {
                    if cpf_rule_matches(scope, *sep, &ret.scope, &ret.name, names) {
                        ret.mapped_cells.extend(lib_cells.clone());
                        if *use_secondary_for_output {
                            ret.use_retention_as_primary = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    for iso in &intent.isolation_strategies {
        if let Some(ref ss) = iso.isolation_supply_set {
            if resolve_ss_handle(ss, &iso.scope, &intent.domains, domain_by_name, ss_by_name)
                .is_none()
            {
                intent.diagnostics.push(UpfDiag {
                    rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!(
                        "set_isolation `{}` references unknown supply set `{ss}`",
                        iso.name
                    ),
                    loc: iso.loc.clone(),
                });
            }
        }
    }
    for ret in &intent.retention_strategies {
        if let Some(ref ss) = ret.retention_supply_set {
            if resolve_ss_handle(ss, &ret.scope, &intent.domains, domain_by_name, ss_by_name)
                .is_none()
            {
                intent.diagnostics.push(UpfDiag {
                    rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!(
                        "set_retention `{}` references unknown supply set `{ss}`",
                        ret.name
                    ),
                    loc: ret.loc.clone(),
                });
            }
        }
    }
    unmerged_deferred
}

fn resolve_rule_pattern_segments<'a>(
    op_norm: &'a str,
    raw_pat: &'a str,
    pat_sep: char,
) -> (bool, Vec<&'a str>) {
    let trimmed = trim_hier_path(raw_pat);
    let is_abs = trimmed.starts_with('/')
        || (pat_sep != '/' && pat_sep != '.' && trimmed.starts_with(pat_sep));
    let mut segs: Vec<&str> = if is_abs {
        Vec::new()
    } else {
        split_hier_segments(op_norm)
    };
    push_norm_segments(&mut segs, trimmed, pat_sep);
    (is_abs, segs)
}

fn match_resolved_rule_segments(
    is_abs: bool,
    psegs: &[&str],
    op_segs: &[&str],
    op_norm: &str,
    strat_norm: &str,
    strat_name: &str,
    full_name: &str,
) -> bool {
    if !is_abs && psegs.len() == op_segs.len() + 1 && psegs[..op_segs.len()] == *op_segs {
        let pat =
            escape_sv_escaped_backslashes_for_glob(&escape_rtl_brackets(psegs[op_segs.len()]));
        strat_norm == op_norm && glob(&pat, strat_name)
    } else {
        let pat = escape_sv_escaped_backslashes_for_glob(&escape_rtl_brackets(&psegs.join("/")));
        glob(&pat, full_name)
    }
}

fn cpf_rule_matches(
    op_scope: &str,
    sep: char,
    strat_scope: &str,
    strat_name: &str,
    patterns: &[String],
) -> bool {
    let op_norm = normalize_scope(op_scope, '/');
    let strat_norm = normalize_scope(strat_scope, '/');
    let full_name = if strat_norm.is_empty() {
        strat_name.to_string()
    } else {
        format!("{strat_norm}/{strat_name}")
    };
    let op_segs = split_hier_segments(&op_norm);
    let strat_segs = split_hier_segments(&strat_norm);
    patterns.iter().any(|raw_pat| {
        let (is_abs, psegs) = resolve_rule_pattern_segments(&op_norm, raw_pat, sep);
        if match_resolved_rule_segments(
            is_abs,
            &psegs,
            &op_segs,
            &op_norm,
            &strat_norm,
            strat_name,
            &full_name,
        ) {
            return true;
        }
        if sep != '/' {
            let (is_abs_slash, psegs_slash) = resolve_rule_pattern_segments(&op_norm, raw_pat, '/');
            if match_resolved_rule_segments(
                is_abs_slash,
                &psegs_slash,
                &op_segs,
                &op_norm,
                &strat_norm,
                strat_name,
                &full_name,
            ) {
                return true;
            }
            if !strat_norm.is_empty() && strat_name.contains(sep) && !psegs_slash.is_empty() {
                let prefix_len = psegs_slash.len() - 1;
                if strat_segs.len() > prefix_len
                    && strat_segs[..prefix_len] == psegs_slash[..prefix_len]
                {
                    let mut sep_buf = [0u8; 4];
                    let sep_str = sep.encode_utf8(&mut sep_buf);
                    let rem_scope = strat_segs[prefix_len..].join(sep_str);
                    let prefix = format!("{rem_scope}{sep}");
                    if let Some(rest) = psegs_slash[prefix_len].strip_prefix(&prefix) {
                        let pat =
                            escape_sv_escaped_backslashes_for_glob(&escape_rtl_brackets(rest));
                        if glob(&pat, strat_name) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    })
}
