//! Domain `-update` merging, hierarchical RTL instance membership, and target validation.

use std::collections::{BTreeMap, BTreeSet};

use crate::ir::{
    escape_glob, escape_rtl_brackets, escape_sv_escaped_backslashes_for_glob, glob,
    has_unescaped_glob_wildcards, split_parent_leaf, strip_sv_escapes_in_glob,
    strip_sv_escapes_in_path, unescape_rtl_brackets, DiagSeverity, PowerDomain, PowerDomainId,
    PowerIntent, UpfDiag, UpfLoc,
};
use crate::upf::{ObjectFinder, ObjectType};

use super::supply_graph::lookup_scoped;

pub(crate) fn merge_power_domains(intent: &mut PowerIntent) {
    let mut merged: Vec<PowerDomain> = Vec::new();
    let mut idx_by_name: BTreeMap<String, usize> = BTreeMap::new();
    for pd in intent.domains.drain(..) {
        if pd.update {
            if let Some(&idx) = idx_by_name.get(&pd.name) {
                let target = &mut merged[idx];
                target.elements.extend(pd.elements);
                target.exclude_elements.extend(pd.exclude_elements);
                if pd.include_scope {
                    target.include_scope = true;
                }
                if let Some(pss) = pd.primary_supply_set {
                    target.primary_supply_set = Some(pss);
                }
                for (k, v) in pd.supply_handles {
                    target.supply_handles.insert(k, v);
                }
                continue;
            }
        }
        let idx = merged.len();
        idx_by_name.insert(pd.name.clone(), idx);
        merged.push(pd);
    }
    intent.domains = merged;
}

pub fn matches_any_element_in_scope(patterns: &[String], scope: &str, target: &str) -> bool {
    let norm_target = target.trim_start_matches('/');
    let norm_scope = scope.trim_matches('/');
    let target_unesc = (!norm_target.contains('\\')).then_some(norm_target);
    patterns.iter().any(|p| {
        if p == "/" {
            return true;
        }
        if p == "." {
            return norm_scope.is_empty()
                || norm_target == norm_scope
                || norm_target.starts_with(&format!("{norm_scope}/"));
        }
        let pat = p.trim_start_matches('/');
        if pat == norm_target || norm_target.starts_with(&format!("{pat}/")) {
            return true;
        }
        if has_unescaped_glob_wildcards(pat) {
            let esc = escape_sv_escaped_backslashes_for_glob(&escape_rtl_brackets(pat));
            if glob(&esc, norm_target) || glob(&format!("{esc}/*"), norm_target) {
                return true;
            }
            if let Some(t_un) = target_unesc {
                let esc_un = strip_sv_escapes_in_glob(&escape_rtl_brackets(pat));
                return glob(&esc_un, t_un) || glob(&format!("{esc_un}/*"), t_un);
            }
            return false;
        }
        let lit = unescape_rtl_brackets(pat);
        if lit == norm_target || norm_target.starts_with(&format!("{lit}/")) {
            return true;
        }
        if let Some(t_un) = target_unesc {
            let lit_un = strip_sv_escapes_in_path(&lit);
            return lit_un == t_un || t_un.starts_with(&format!("{lit_un}/"));
        }
        false
    })
}

pub(crate) fn design_object_exists(finder: &dyn ObjectFinder, path: &str) -> bool {
    let norm = path.trim_start_matches('/');
    if finder.scope_exists(norm) {
        return true;
    }
    let lit = unescape_rtl_brackets(norm);
    let lit_unesc = strip_sv_escapes_in_path(&lit);
    if (lit != norm && finder.scope_exists(&lit))
        || (lit_unesc != lit && finder.scope_exists(&lit_unesc))
    {
        return true;
    }
    let matches_lit = |o: &str| {
        let on = o.trim_start_matches('/');
        on == lit || (!on.contains('\\') && on == lit_unesc)
    };
    let escaped = escape_glob(&lit);
    if finder
        .find_objects("", std::slice::from_ref(&escaped), None, true)
        .iter()
        .any(|o| matches_lit(o))
    {
        return true;
    }
    if escaped != lit || escaped.contains('\\') {
        let parent = split_parent_leaf(&lit).map(|(p, _)| p).unwrap_or("");
        if finder
            .find_objects(parent, &["*".to_string()], None, false)
            .iter()
            .any(|o| matches_lit(o))
        {
            return true;
        }
        if !parent.is_empty()
            && finder
                .find_objects("", &["*".to_string()], None, true)
                .iter()
                .any(|o| matches_lit(o))
        {
            return true;
        }
    }
    false
}

pub(crate) fn find_design_element_hits(finder: &dyn ObjectFinder, elem: &str) -> Vec<String> {
    let norm = elem.trim_start_matches('/');
    if !has_unescaped_glob_wildcards(norm) {
        let lit = unescape_rtl_brackets(norm);
        let lit_unesc = strip_sv_escapes_in_path(&lit);
        let escaped = escape_glob(&lit);
        if escaped == lit && !escaped.contains('\\') {
            return finder.find_objects("", &[lit], None, true);
        }
        let esc_hits = finder.find_objects("", std::slice::from_ref(&escaped), None, true);
        if !esc_hits.is_empty() {
            return esc_hits;
        }
        let matches_lit = |o: &str| {
            let on = o.trim_start_matches('/');
            on == lit || (!on.contains('\\') && on == lit_unesc)
        };
        let parent = split_parent_leaf(&lit).map(|(p, _)| p).unwrap_or("");
        let hits: Vec<String> = finder
            .find_objects(parent, &["*".to_string()], None, false)
            .into_iter()
            .filter(|o| matches_lit(o))
            .collect();
        if !hits.is_empty() || parent.is_empty() {
            return hits;
        }
        return finder
            .find_objects("", &["*".to_string()], None, true)
            .into_iter()
            .filter(|o| matches_lit(o))
            .collect();
    }
    let rtl_esc = escape_sv_escaped_backslashes_for_glob(&escape_rtl_brackets(norm));
    let rtl_unesc = strip_sv_escapes_in_glob(&escape_rtl_brackets(norm));
    let lit = unescape_rtl_brackets(norm);
    let matches_glob = |o: &str| {
        let on = o.trim_start_matches('/');
        glob(&rtl_esc, on) || (!on.contains('\\') && glob(&rtl_unesc, on))
    };
    let hits = finder.find_objects("", std::slice::from_ref(&rtl_esc), None, true);
    if !hits.is_empty() {
        return hits;
    }
    if let Some((parent, leaf)) = split_parent_leaf(norm) {
        if !has_unescaped_glob_wildcards(parent) {
            let parent_lit = unescape_rtl_brackets(parent);
            let leaf_esc = escape_sv_escaped_backslashes_for_glob(&escape_rtl_brackets(leaf));
            let hits: Vec<String> = finder
                .find_objects(&parent_lit, std::slice::from_ref(&leaf_esc), None, true)
                .into_iter()
                .filter(|o| matches_glob(o))
                .collect();
            if !hits.is_empty() {
                return hits;
            }
            let leaf_lit = unescape_rtl_brackets(leaf);
            if leaf_esc != leaf_lit || leaf_esc.contains('\\') {
                let hits: Vec<String> = finder
                    .find_objects(&parent_lit, &["*".to_string()], None, true)
                    .into_iter()
                    .filter(|o| matches_glob(o))
                    .collect();
                if !hits.is_empty() {
                    return hits;
                }
            }
        }
    }
    if rtl_esc != lit || rtl_esc.contains('\\') {
        let hits: Vec<String> = finder
            .find_objects("", &["*".to_string()], None, true)
            .into_iter()
            .filter(|o| matches_glob(o))
            .collect();
        if !hits.is_empty() {
            return hits;
        }
    }
    Vec::new()
}

fn collect_transitive_descendants(
    finder: &dyn ObjectFinder,
    hit: &str,
    out: &mut BTreeSet<String>,
) {
    let hit_norm = hit.trim_start_matches('/');
    let lit = unescape_rtl_brackets(hit_norm);
    let unesc = strip_sv_escapes_in_path(&lit);
    let scope_opt = if finder.scope_exists(hit_norm) {
        Some(hit_norm)
    } else if lit != hit_norm && finder.scope_exists(&lit) {
        Some(lit.as_str())
    } else if unesc != lit && finder.scope_exists(&unesc) {
        Some(unesc.as_str())
    } else {
        None
    };
    if let Some(sc) = scope_opt {
        for desc in finder.find_objects(sc, &["*".to_string()], Some(ObjectType::Inst), true) {
            out.insert(desc);
        }
    }
}

pub(crate) fn resolve_design_membership(
    intent: &mut PowerIntent,
    finder: &dyn ObjectFinder,
    domain_instances: &mut [BTreeSet<String>],
    instance_domain: &mut BTreeMap<String, PowerDomainId>,
) {
    for (idx, pd) in intent.domains.iter().enumerate() {
        let pd_id = PowerDomainId(idx as u32);
        let mut matched: BTreeSet<String> = BTreeSet::new();
        if pd.include_scope {
            if !pd.scope.is_empty() && !finder.scope_exists(&pd.scope) {
                intent.diagnostics.push(UpfDiag {
                    rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!(
                        "power domain `{}` scope `{}` not found in design",
                        pd.name, pd.scope
                    ),
                    loc: pd.loc.clone(),
                });
            } else {
                for inst in finder.find_objects(
                    &pd.scope,
                    &["*".to_string()],
                    Some(ObjectType::Inst),
                    pd.transitive,
                ) {
                    matched.insert(inst);
                }
                if !pd.scope.is_empty() {
                    matched.insert(pd.scope.clone());
                }
            }
        }
        for elem in &pd.elements {
            if elem == "/" || (elem == "." && pd.scope.is_empty()) {
                for inst in finder.find_objects(
                    "",
                    &["*".to_string()],
                    Some(ObjectType::Inst),
                    pd.transitive,
                ) {
                    matched.insert(inst);
                }
                continue;
            }
            if elem == "." {
                if pd.transitive {
                    collect_transitive_descendants(finder, &pd.scope, &mut matched);
                }
                matched.insert(pd.scope.clone());
                continue;
            }
            let norm = elem.trim_start_matches('/');
            let lit = unescape_rtl_brackets(norm);
            let lit_unesc = strip_sv_escapes_in_path(&lit);
            let hits = find_design_element_hits(finder, norm);
            let scope_hit = finder.scope_exists(norm)
                || finder.scope_exists(&lit)
                || finder.scope_exists(&lit_unesc);
            let expand_transitive = pd.transitive;
            if hits.is_empty() && !scope_hit {
                intent.diagnostics.push(UpfDiag {
                    rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!(
                        "power domain `{}` element `{elem}` matched no object in design",
                        pd.name
                    ),
                    loc: pd.loc.clone(),
                });
            } else if hits.is_empty() {
                let inserted = if finder.scope_exists(&lit_unesc) && !finder.scope_exists(&lit) {
                    lit_unesc
                } else {
                    lit
                };
                if expand_transitive {
                    collect_transitive_descendants(finder, &inserted, &mut matched);
                }
                matched.insert(inserted);
            } else {
                for h in hits {
                    if expand_transitive {
                        collect_transitive_descendants(finder, &h, &mut matched);
                    }
                    matched.insert(h);
                }
            }
        }

        matched.retain(|inst| !matches_any_element_in_scope(&pd.exclude_elements, &pd.scope, inst));
        for inst in matched {
            if let Some(&prev_pd) = instance_domain.get(&inst) {
                if prev_pd == pd_id {
                    domain_instances[idx].insert(inst);
                    continue;
                }
                let prev_dom = &intent.domains[prev_pd.0 as usize];
                let prev_scope = &prev_dom.scope;
                let cur_spec = explicit_match_specificity(pd, &inst);
                let prev_spec = explicit_match_specificity(prev_dom, &inst);
                match (cur_spec, prev_spec) {
                    (Some(c), Some(p)) if c == p => {
                        intent.diagnostics.push(UpfDiag {
                            rule_id: "UPF_DOMAIN_CONFLICT".to_string(),
                            severity: DiagSeverity::Error,
                            message: format!(
                                "instance `{inst}` claimed by both `{}` and `{}`",
                                prev_dom.name, pd.name
                            ),
                            loc: pd.loc.clone(),
                        });
                    }
                    (Some(c), Some(p)) if c > p => {
                        domain_instances[prev_pd.0 as usize].remove(&inst);
                        domain_instances[idx].insert(inst.clone());
                        instance_domain.insert(inst, pd_id);
                    }
                    (Some(_), Some(_)) | (None, Some(_)) => {}
                    (Some(_), None) => {
                        domain_instances[prev_pd.0 as usize].remove(&inst);
                        domain_instances[idx].insert(inst.clone());
                        instance_domain.insert(inst, pd_id);
                    }
                    (None, None) if pd.scope.len() > prev_scope.len() => {
                        domain_instances[prev_pd.0 as usize].remove(&inst);
                        domain_instances[idx].insert(inst.clone());
                        instance_domain.insert(inst, pd_id);
                    }
                    (None, None) => {}
                }
            } else {
                domain_instances[idx].insert(inst.clone());
                instance_domain.insert(inst, pd_id);
            }
        }
    }
}

fn parse_trailing_bit_slice(s: &str) -> Option<(&str, i128, i128)> {
    let trimmed = s.trim();
    if !trimmed.ends_with(']') {
        return None;
    }
    let open = trimmed.rfind('[')?;
    let inner = &trimmed[open + 1..trimmed.len() - 1];
    if inner.contains('/') {
        return None;
    }
    let base = trimmed[..open].trim_end();
    if base.is_empty() {
        return None;
    }
    if let Some((a, b)) = inner.split_once(':') {
        let msb: i128 = a.trim().parse().ok()?;
        let lsb: i128 = b.trim().parse().ok()?;
        Some((base, msb, lsb))
    } else {
        let idx: i128 = inner.trim().parse().ok()?;
        Some((base, idx, idx))
    }
}

pub(crate) fn validate_strategy_elements(
    intent: &mut PowerIntent,
    finder: &dyn ObjectFinder,
    unmerged_deferred: &[(String, String, String, Option<UpfLoc>)],
) {
    let elem_exists = |elem: &str, scope: &str| -> bool {
        let norm = elem.trim_start_matches('/');
        if elem == "/" {
            return true;
        }
        if elem == "." || norm.is_empty() {
            return scope.is_empty() || finder.scope_exists(scope);
        }
        let lit = unescape_rtl_brackets(norm);
        let hits = find_design_element_hits(finder, norm);
        if !hits.is_empty()
            || finder.scope_exists(norm)
            || finder.scope_exists(&lit)
            || finder.scope_exists(&strip_sv_escapes_in_path(&lit))
        {
            return true;
        }
        if let Some((base, msb, lsb)) = parse_trailing_bit_slice(&lit) {
            let unesc_base = strip_sv_escapes_in_path(base);
            if finder.signal_slice_exists(base, msb, lsb)
                || finder.signal_slice_exists(&unesc_base, msb, lsb)
            {
                return true;
            }
            if has_unescaped_glob_wildcards(base) {
                let base_hits = find_design_element_hits(finder, base);
                if base_hits
                    .iter()
                    .any(|h| finder.signal_slice_exists(h.trim_start_matches('/'), msb, lsb))
                {
                    return true;
                }
            }
        }
        false
    };
    for iso in &intent.isolation_strategies {
        for elem in &iso.elements {
            if !elem_exists(elem, &iso.scope) {
                intent.diagnostics.push(UpfDiag {
                    rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!(
                        "set_isolation `{}` element `{elem}` matched no object in design",
                        iso.name
                    ),
                    loc: iso.loc.clone(),
                });
            }
        }
    }
    for ret in &intent.retention_strategies {
        for elem in &ret.elements {
            if !elem_exists(elem, &ret.scope) {
                intent.diagnostics.push(UpfDiag {
                    rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!(
                        "set_retention `{}` element `{elem}` matched no object in design",
                        ret.name
                    ),
                    loc: ret.loc.clone(),
                });
            }
        }
    }
    for ls in &intent.level_shifter_strategies {
        for elem in &ls.elements {
            if !elem_exists(elem, &ls.scope) {
                intent.diagnostics.push(UpfDiag {
                    rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!(
                        "set_level_shifter `{}` element `{elem}` matched no object in design",
                        ls.name
                    ),
                    loc: ls.loc.clone(),
                });
            }
        }
    }
    for rep in &intent.repeater_strategies {
        for elem in &rep.elements {
            if !elem_exists(elem, &rep.scope) {
                intent.diagnostics.push(UpfDiag {
                    rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                    severity: DiagSeverity::Error,
                    message: format!(
                        "set_repeater `{}` element `{elem}` matched no object in design",
                        rep.name
                    ),
                    loc: rep.loc.clone(),
                });
            }
        }
    }
    for (owner, scope, elem, loc) in unmerged_deferred {
        if !elem_exists(elem, scope) {
            intent.diagnostics.push(UpfDiag {
                rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                severity: DiagSeverity::Error,
                message: format!("{owner} element `{elem}` matched no object in design"),
                loc: loc.clone(),
            });
        }
    }
}

fn explicit_match_specificity(pd: &PowerDomain, target: &str) -> Option<(u8, usize)> {
    let norm_target = target.trim_start_matches('/');
    let target_unesc = (!norm_target.contains('\\')).then_some(norm_target);
    let target_depth = norm_target.split('/').count();
    let mut best: Option<(u8, usize)> = None;
    let mut bump = |spec: (u8, usize)| {
        if best.is_none_or(|b| spec > b) {
            best = Some(spec);
        }
    };
    for p in &pd.elements {
        if p == "/" {
            bump((1, 0));
            continue;
        }
        if p == "." {
            if !pd.scope.is_empty() {
                if norm_target == pd.scope {
                    bump((2, target_depth));
                } else if norm_target.starts_with(&format!("{}/", pd.scope)) {
                    bump((1, pd.scope.split('/').count()));
                }
            } else {
                bump((1, 0));
            }
            continue;
        }
        let pat = p.trim_start_matches('/');
        if pat == norm_target {
            bump((2, target_depth));
            continue;
        }
        if norm_target.starts_with(&format!("{pat}/")) {
            bump((1, pat.split('/').count()));
            continue;
        }
        if has_unescaped_glob_wildcards(pat) {
            let esc = escape_sv_escaped_backslashes_for_glob(&escape_rtl_brackets(pat));
            if glob(&esc, norm_target) {
                bump((2, target_depth));
            } else if glob(&format!("{esc}/*"), norm_target) {
                bump((1, pat.split('/').count()));
            } else if let Some(t_un) = target_unesc {
                let esc_un = strip_sv_escapes_in_glob(&escape_rtl_brackets(pat));
                if glob(&esc_un, t_un) {
                    bump((2, target_depth));
                } else if glob(&format!("{esc_un}/*"), t_un) {
                    bump((1, pat.split('/').count()));
                }
            }
            continue;
        }
        let lit = unescape_rtl_brackets(pat);
        if lit == norm_target {
            bump((2, target_depth));
        } else if norm_target.starts_with(&format!("{lit}/")) {
            bump((1, lit.split('/').count()));
        } else if let Some(t_un) = target_unesc {
            let lit_un = strip_sv_escapes_in_path(&lit);
            if lit_un == t_un {
                bump((2, target_depth));
            } else if t_un.starts_with(&format!("{lit_un}/")) {
                bump((1, lit_un.split('/').count()));
            }
        }
    }
    best
}

pub(crate) fn validate_strategy_domains(
    intent: &mut PowerIntent,
    domain_by_name: &BTreeMap<String, PowerDomainId>,
) {
    for iso in &intent.isolation_strategies {
        if lookup_scoped(domain_by_name, &iso.domain, &iso.scope).is_none() {
            intent.diagnostics.push(UpfDiag {
                rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                severity: DiagSeverity::Error,
                message: format!(
                    "set_isolation `{}` references unknown domain `{}`",
                    iso.name, iso.domain
                ),
                loc: iso.loc.clone(),
            });
        }
    }
    for ls in &intent.level_shifter_strategies {
        if lookup_scoped(domain_by_name, &ls.domain, &ls.scope).is_none() {
            intent.diagnostics.push(UpfDiag {
                rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                severity: DiagSeverity::Error,
                message: format!(
                    "set_level_shifter `{}` references unknown domain `{}`",
                    ls.name, ls.domain
                ),
                loc: ls.loc.clone(),
            });
        }
    }
    for ret in &intent.retention_strategies {
        if lookup_scoped(domain_by_name, &ret.domain, &ret.scope).is_none() {
            intent.diagnostics.push(UpfDiag {
                rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                severity: DiagSeverity::Error,
                message: format!(
                    "set_retention `{}` references unknown domain `{}`",
                    ret.name, ret.domain
                ),
                loc: ret.loc.clone(),
            });
        }
    }
    for rep in &intent.repeater_strategies {
        if lookup_scoped(domain_by_name, &rep.domain, &rep.scope).is_none() {
            intent.diagnostics.push(UpfDiag {
                rule_id: "UPF_UNRESOLVED_TARGET".to_string(),
                severity: DiagSeverity::Error,
                message: format!(
                    "set_repeater `{}` references unknown domain `{}`",
                    rep.name, rep.domain
                ),
                loc: rep.loc.clone(),
            });
        }
    }
}
