use super::schema::{FvpSpec, SignoffGates};
use crate::diagnose;
use informal_fpv::{Kind, Property, Session, Status, EMBEDDED};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Auto-discover `fvp.spec.json` next to elaborated design files, property
/// source scripts, or `root`.
pub fn discover_spec_file(s: &Session, root: &Path) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for f in &s.design.files {
        if let Some(parent) = f.parent() {
            if !dirs.iter().any(|d| d == parent) {
                dirs.push(parent.to_path_buf());
            }
        }
    }
    for p in &s.props {
        if let Some(src) = &p.source {
            if let Some(parent) = Path::new(&src.file).parent() {
                if !dirs.iter().any(|d| d == parent) {
                    dirs.push(parent.to_path_buf());
                }
            }
        }
    }
    if !dirs.iter().any(|d| d == root) {
        dirs.push(root.to_path_buf());
    }
    for dir in dirs {
        let cand = dir.join("fvp.spec.json");
        if cand.is_file() {
            if let Ok(canon) = cand.canonicalize() {
                return Some(canon);
            }
            return Some(cand);
        }
    }
    None
}

/// Simple wildcard matcher supporting `*` and `?`.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_pi, mut star_ti) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            pi += 1;
            star_ti = ti;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

pub(super) fn signal_matches_any(sig: &str, top: Option<&str>, patterns: &[String]) -> bool {
    let leaf = top
        .and_then(|t| sig.strip_prefix(&format!("{t}.")))
        .unwrap_or(sig);
    let tail = sig.rsplit('.').next().unwrap_or(sig);
    patterns.iter().any(|pat| {
        let pat_leaf = top
            .and_then(|t| pat.strip_prefix(&format!("{t}.")))
            .unwrap_or(pat.as_str());
        wildcard_match(pat, sig)
            || wildcard_match(pat_leaf, leaf)
            || (!pat.contains('.') && wildcard_match(pat, tail))
    })
}

pub(super) fn norm_expr(e: &str) -> String {
    e.chars().filter(|c| !c.is_whitespace()).collect()
}

pub(super) fn name_matches(pattern: &str, name: &str) -> bool {
    if pattern.contains('*') || pattern.contains('?') {
        wildcard_match(pattern, name)
    } else {
        pattern == name
    }
}

pub(super) fn matching_props_any_state<'a>(
    props: &'a [Property],
    task: &str,
    kind: Kind,
    pattern: &str,
) -> Vec<&'a Property> {
    let emb_prefix = format!("{EMBEDDED}::");
    if let Some(rest) = pattern.strip_prefix(&emb_prefix) {
        return props
            .iter()
            .filter(|p| p.kind == kind && p.task == EMBEDDED && name_matches(rest, &p.name))
            .collect();
    }
    if pattern.contains("::") {
        return props
            .iter()
            .filter(|p| p.kind == kind && name_matches(pattern, &p.name))
            .collect();
    }
    let in_task: Vec<&Property> = props
        .iter()
        .filter(|p| p.kind == kind && p.task == task && name_matches(pattern, p.short_name()))
        .collect();
    if !in_task.is_empty() || task == EMBEDDED {
        return in_task;
    }
    props
        .iter()
        .filter(|p| p.kind == kind && p.task == EMBEDDED && name_matches(pattern, &p.name))
        .collect()
}

pub(super) fn match_props<'a>(
    props: &'a [Property],
    task: &str,
    kind: Kind,
    pattern: &str,
) -> Vec<&'a Property> {
    matching_props_any_state(props, task, kind, pattern)
        .into_iter()
        .filter(|p| p.enabled)
        .collect()
}

pub(super) fn find_disabled_prop<'a>(
    props: &'a [Property],
    task: &str,
    kind: Kind,
    pattern: &str,
) -> Option<&'a Property> {
    matching_props_any_state(props, task, kind, pattern)
        .into_iter()
        .find(|p| !p.enabled)
}

pub(super) fn assertion_meets_target(p: &Property, gates: &SignoffGates) -> bool {
    match (&p.status, gates.proof_target.as_str()) {
        (Status::Proven { .. }, _) => true,
        (Status::BoundedProven { bound, .. }, "bounded_proven") => *bound >= gates.min_bound,
        _ => false,
    }
}

#[derive(Debug, Clone)]
pub(super) struct OverconstraintHit {
    pub(super) name: String,
    pub(super) expr: String,
    pub(super) forbidden_signals: Vec<String>,
    pub(super) roles: Vec<String>,
    pub(super) reason: String,
}

pub(super) fn audit_assumption_boundary(
    s: &Session,
    task: &str,
    spec: &FvpSpec,
) -> Vec<OverconstraintHit> {
    let top = s.design.top.as_deref();
    let clock_reset_ids: BTreeSet<String> = {
        let mut set = BTreeSet::new();
        for c in &s.clocks {
            set.insert(c.signal.clone());
        }
        if let Some(r) = &s.reset {
            for id in diagnose::expr_identifiers(r) {
                set.insert(id);
            }
        }
        for c in &spec.design.clocks {
            set.insert(c.clone());
        }
        if let Some(r) = &spec.design.reset {
            for id in diagnose::expr_identifiers(r) {
                set.insert(id);
            }
        }
        set
    };

    let mut out = Vec::new();
    for p in s
        .props
        .iter()
        .filter(|p| p.enabled && p.task == task && p.kind == Kind::Assume)
    {
        // Assumptions promoted from a proven assertion (`assume -from_assert`)
        // are inductive helper lemmas, not unverified environment constraints.
        if p.from_assert.is_some() {
            continue;
        }

        let mut bad_sigs = Vec::new();
        let mut bad_roles = Vec::new();
        let mut reasons = Vec::new();

        for id in diagnose::expr_identifiers(&p.expr) {
            let leaf = top
                .and_then(|t| id.strip_prefix(&format!("{t}.")))
                .unwrap_or(&id);
            if clock_reset_ids.contains(&id) || clock_reset_ids.contains(leaf) {
                continue;
            }
            let role = diagnose::classify_signal_role(&id, &s.design.hierarchy);
            let explicitly_forbidden =
                signal_matches_any(&id, top, &spec.boundary.forbidden_assume_signals);
            if explicitly_forbidden {
                bad_sigs.push(id.clone());
                bad_roles.push(role.to_string());
                reasons.push(format!(
                    "`{id}` is listed in `boundary.forbidden_assume_signals`"
                ));
                continue;
            }
            match role {
                "dut_output" => {
                    bad_sigs.push(id.clone());
                    bad_roles.push(role.to_string());
                    reasons.push(format!(
                        "`{id}` is a DUT output port (SVA-FPV-001 over-constraint)"
                    ));
                }
                "internal_state"
                    if !signal_matches_any(&id, top, &spec.boundary.allow_internal_assumes) =>
                {
                    bad_sigs.push(id.clone());
                    bad_roles.push(role.to_string());
                    reasons.push(format!(
                        "`{id}` is internal DUT state (prove as `assert` and promote via `assume -from_assert`)"
                    ));
                }
                "primary_input"
                    if !spec.boundary.allowed_assume_inputs.is_empty()
                        && !signal_matches_any(&id, top, &spec.boundary.allowed_assume_inputs) =>
                {
                    bad_sigs.push(id.clone());
                    bad_roles.push(role.to_string());
                    reasons.push(format!(
                        "`{id}` is outside `boundary.allowed_assume_inputs`"
                    ));
                }
                _ => {}
            }
        }

        if !bad_sigs.is_empty() {
            out.push(OverconstraintHit {
                name: p.name.clone(),
                expr: p.expr.clone(),
                forbidden_signals: bad_sigs,
                roles: bad_roles,
                reason: reasons.join("; "),
            });
        }
    }
    out
}
