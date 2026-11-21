//! Machine-verifiable Formal Verification Plan & Signoff Specification
//! (`fvp.spec.json`, schema `informal.spec/1`) and `spec_audit` evaluator.
//!
//! Cross-checks an active [`informal_fpv::Session`] against a structured design
//! boundary + requirement-to-SVA mapping + signoff gate contract, returning
//! per-requirement verdicts (`verified`, `cex`, `undetermined`,
//! `missing_assert`, `missing_cover`, `vacuous`, `overconstrained`), uncovered
//! requirements, unmapped assertions, and concrete next-action hints.

use crate::diagnose;
use informal_fpv::cov::{CovModel, CovStatus};
use informal_fpv::{Kind, Property, Session, Status};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const SPEC_SCHEMA: &str = "informal.spec/1";
pub const AUDIT_SCHEMA: &str = "informal.spec_audit/1";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DesignIntent {
    #[serde(default)]
    pub top: Option<String>,
    #[serde(default)]
    pub clocks: Vec<String>,
    #[serde(default)]
    pub reset: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BoundarySpec {
    /// Legal primary inputs (or globs) that environment `assume` properties may
    /// constrain. Empty means all elaborated primary inputs are legal.
    #[serde(default)]
    pub allowed_assume_inputs: Vec<String>,
    /// Signals (or globs) explicitly forbidden inside environment `assume`s.
    #[serde(default)]
    pub forbidden_assume_signals: Vec<String>,
    /// Internal DUT signals explicitly waived for `assume` (default empty:
    /// internal signals require `assert` + `assume -from_assert`).
    #[serde(default)]
    pub allow_internal_assumes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementSpec {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Target safety/liveness assertion names or globs (`*`, `?`).
    #[serde(default)]
    pub assertions: Vec<String>,
    /// Mandatory anti-vacuity / reachability witness cover names or globs.
    #[serde(default)]
    pub covers: Vec<String>,
    /// Optional environment assumption names associated with this requirement.
    #[serde(default)]
    pub assumptions: Vec<String>,
}

fn default_proof_target() -> String {
    "proven".into()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignoffGates {
    /// `"proven"` (unbounded, default) or `"bounded_proven"`.
    #[serde(default = "default_proof_target")]
    pub proof_target: String,
    /// Minimum BMC bound k when `proof_target == "bounded_proven"`.
    #[serde(default)]
    pub min_bound: u32,
    /// Maximum tolerated boundary-violating assumptions (default 0).
    #[serde(default)]
    pub max_overconstraints: usize,
    /// Maximum tolerated 2QBF dead-end trap states (default 0).
    #[serde(default)]
    pub max_dead_ends: usize,
    /// Whether `spec_audit` requires `check_dead_end=true` to pass signoff.
    #[serde(default)]
    pub require_dead_end_check: bool,
    /// Reject proven assertions whose vacuity status is `vacuous` (default true).
    #[serde(default = "default_true")]
    pub forbid_vacuous: bool,
    /// Require every requirement to declare at least one witness cover.
    #[serde(default)]
    pub require_covers: bool,
    /// Block signoff while proofs still stand on AI-added assumptions/cutpoints.
    #[serde(default)]
    pub forbid_under_ai: bool,
    /// Optional COI formal coverage floor percentage (`0.0..=100.0`).
    #[serde(default)]
    pub coi_coverage_floor: Option<f64>,
    /// Optional mutant-kill score floor percentage (`0.0..=100.0`).
    #[serde(default)]
    pub mutation_score_floor: Option<f64>,
}

impl Default for SignoffGates {
    fn default() -> Self {
        Self {
            proof_target: default_proof_target(),
            min_bound: 0,
            max_overconstraints: 0,
            max_dead_ends: 0,
            require_dead_end_check: false,
            forbid_vacuous: true,
            require_covers: false,
            forbid_under_ai: false,
            coi_coverage_floor: None,
            mutation_score_floor: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FvpSpec {
    pub schema: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub design: DesignIntent,
    #[serde(default)]
    pub boundary: BoundarySpec,
    #[serde(default)]
    pub requirements: Vec<RequirementSpec>,
    #[serde(default)]
    pub signoff: SignoffGates,
}

impl FvpSpec {
    pub fn parse_value(v: &Value) -> Result<Self, String> {
        let spec: Self =
            serde_json::from_value(v.clone()).map_err(|e| format!("invalid fvp.spec JSON: {e}"))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn parse_str(text: &str) -> Result<Self, String> {
        let v: Value =
            serde_json::from_str(text).map_err(|e| format!("malformed fvp.spec JSON: {e}"))?;
        Self::parse_value(&v)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SPEC_SCHEMA {
            return Err(format!(
                "unsupported spec `schema`: got `{}`, expected `{SPEC_SCHEMA}`",
                self.schema
            ));
        }
        if !matches!(
            self.signoff.proof_target.as_str(),
            "proven" | "bounded_proven"
        ) {
            return Err(format!(
                "`signoff.proof_target` must be `proven` or `bounded_proven` (got `{}`)",
                self.signoff.proof_target
            ));
        }
        for (label, opt) in [
            (
                "signoff.coi_coverage_floor",
                self.signoff.coi_coverage_floor,
            ),
            (
                "signoff.mutation_score_floor",
                self.signoff.mutation_score_floor,
            ),
        ] {
            if let Some(pct) = opt {
                if !(0.0..=100.0).contains(&pct) || pct.is_nan() {
                    return Err(format!(
                        "`{label}` must be between 0.0 and 100.0 (got {pct})"
                    ));
                }
            }
        }
        let mut seen = BTreeSet::new();
        for req in &self.requirements {
            let id = req.id.trim();
            if id.is_empty() {
                return Err(
                    "every requirement in `requirements` must have a non-empty `id`".into(),
                );
            }
            if !seen.insert(id.to_string()) {
                return Err(format!("duplicate requirement id `{id}` in spec"));
            }
        }
        Ok(())
    }
}

/// Optional live solver checks passed into [`audit_session`].
#[derive(Debug, Clone, Default)]
pub struct AuditRuntime {
    pub spec_source: String,
    pub task: String,
    pub env_conflict: bool,
    pub conflict_set: Vec<String>,
    pub dead_end_checked: bool,
    pub dead_end_found: bool,
    pub dead_end_depth: Option<u32>,
    pub mutation_score: Option<f64>,
}

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

fn signal_matches_any(sig: &str, top: Option<&str>, patterns: &[String]) -> bool {
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

fn norm_expr(e: &str) -> String {
    e.chars().filter(|c| !c.is_whitespace()).collect()
}

fn match_props<'a>(
    props: &'a [Property],
    task: &str,
    kind: Kind,
    pattern: &str,
) -> Vec<&'a Property> {
    let has_glob = pattern.contains('*') || pattern.contains('?');
    let qualified = format!("{task}::{pattern}");
    props
        .iter()
        .filter(|p| p.enabled && p.kind == kind)
        .filter(|p| {
            if has_glob {
                (p.task == task && wildcard_match(pattern, &p.name))
                    || wildcard_match(pattern, &p.name)
                    || wildcard_match(&qualified, &p.name)
            } else {
                (p.task == task && p.name == pattern) || p.name == pattern || p.name == qualified
            }
        })
        .collect()
}

fn assertion_meets_target(p: &Property, gates: &SignoffGates) -> bool {
    match (&p.status, gates.proof_target.as_str()) {
        (Status::Proven { .. }, _) => true,
        (Status::BoundedProven { bound, .. }, "bounded_proven") => *bound >= gates.min_bound,
        _ => false,
    }
}

#[derive(Debug, Clone)]
struct OverconstraintHit {
    name: String,
    expr: String,
    forbidden_signals: Vec<String>,
    roles: Vec<String>,
    reason: String,
}

fn audit_assumption_boundary(s: &Session, task: &str, spec: &FvpSpec) -> Vec<OverconstraintHit> {
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
                        "`{id}` is a DUT output port (SV-FPV-001 over-constraint)"
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

fn coi_coverage_pct(s: &Session, task: &str) -> Option<(f64, usize, usize)> {
    let items: Vec<_> = s
        .cov
        .items
        .iter()
        .filter(|it| it.model == CovModel::Coi && (it.task == task || it.task.is_empty()))
        .collect();
    if items.is_empty() {
        return None;
    }
    let total = items.len();
    let waived = items
        .iter()
        .filter(|it| it.status == CovStatus::Waived)
        .count();
    let covered = items
        .iter()
        .filter(|it| it.status == CovStatus::Covered)
        .count();
    let denom = total.saturating_sub(waived);
    let pct = if denom == 0 {
        100.0
    } else {
        (covered as f64) * 100.0 / (denom as f64)
    };
    Some((pct, covered, denom))
}

/// Evaluate `spec` against the live `Session` and return an
/// `informal.spec_audit/1` JSON document.
pub fn audit_session(s: &Session, spec: &FvpSpec, rt: &AuditRuntime) -> Value {
    let task = if rt.task.is_empty() {
        s.current_task.clone()
    } else {
        rt.task.clone()
    };

    // 1. Design / clock / reset intent check.
    let mut design_mismatches = Vec::new();
    if !s.elaborated {
        design_mismatches.push("session has no elaborated design (`session_open` not run)".into());
    }
    if let Some(exp_top) = &spec.design.top {
        match s.design.top.as_deref() {
            Some(actual) if actual == exp_top => {}
            Some(actual) => design_mismatches.push(format!(
                "top module mismatch: spec expects `{exp_top}`, session has `{actual}`"
            )),
            None => {
                design_mismatches.push(format!("spec expects top `{exp_top}`, session has none"))
            }
        }
    }
    let actual_clocks: Vec<String> = s.clocks.iter().map(|c| c.signal.clone()).collect();
    let mut clocks_ok = true;
    for exp_clk in &spec.design.clocks {
        if !actual_clocks.iter().any(|c| c == exp_clk) {
            clocks_ok = false;
            design_mismatches.push(format!(
                "missing clock `{exp_clk}` (session clocks: [{}])",
                actual_clocks.join(", ")
            ));
        }
    }
    if let Some(exp_rst) = &spec.design.reset {
        match s.reset.as_deref() {
            Some(actual) if norm_expr(actual) == norm_expr(exp_rst) => {}
            Some(actual) => design_mismatches.push(format!(
                "reset mismatch: spec expects `{exp_rst}`, session has `{actual}`"
            )),
            None => design_mismatches.push(format!(
                "spec expects reset `{exp_rst}`, session has no reset configured"
            )),
        }
    }
    let design_ok = design_mismatches.is_empty();

    // 2. Boundary & environment soundness audit.
    let overconstraints = audit_assumption_boundary(s, &task, spec);
    let env_conflict = rt.env_conflict || s.environment.as_ref().is_some_and(|e| e.conflict());
    let conflict_set = if !rt.conflict_set.is_empty() {
        rt.conflict_set.clone()
    } else {
        s.environment
            .as_ref()
            .map(|e| e.conflict_set.clone())
            .unwrap_or_default()
    };
    let dead_end_count = usize::from(rt.dead_end_found);
    let task_tainted_by_env = overconstraints.len() > spec.signoff.max_overconstraints
        || env_conflict
        || dead_end_count > spec.signoff.max_dead_ends;

    // 3. Per-requirement cross-check.
    let mut mapped_assert_names: BTreeSet<String> = BTreeSet::new();
    let mut req_rows: Vec<Value> = Vec::new();
    let mut uncovered_ids: Vec<String> = Vec::new();
    let mut next_actions: Vec<String> = Vec::new();
    let mut push_action = |act: &str| {
        if !act.is_empty() && !next_actions.iter().any(|x| x == act) {
            next_actions.push(act.to_string());
        }
    };

    if !design_ok {
        for m in &design_mismatches {
            push_action(&format!("Fix design setup mismatch: {m}"));
        }
    }
    if env_conflict {
        push_action(&format!(
            "Environment assumptions are contradictory (UNSAT core: [{}]); remove or weaken conflicting `assume`s.",
            conflict_set.join(", ")
        ));
    }
    if rt.dead_end_found {
        push_action(
            "Environment assumptions admit a 2QBF dead-end trap state; weaken sequential input constraints and re-validate with `proposal_validate`.",
        );
    }
    for oc in &overconstraints {
        push_action(&format!(
            "Remove or rewrite over-constraining assumption `{}` ({})",
            oc.name, oc.reason
        ));
    }

    let (
        mut n_verified,
        mut n_cex,
        mut n_undet,
        mut n_missing_assert,
        mut n_missing_cover,
        mut n_vacuous,
        mut n_overconstrained,
    ) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut any_vacuous = 0usize;
    let mut any_under_ai: Vec<String> = Vec::new();

    for req in &spec.requirements {
        let mut issues: Vec<String> = Vec::new();
        let mut assert_views: Vec<Value> = Vec::new();
        let mut cover_views: Vec<Value> = Vec::new();

        let mut missing_assert = req.assertions.is_empty();
        if req.assertions.is_empty() {
            issues.push("requirement maps zero target assertions".into());
        }
        let mut first_cex: Option<String> = None;
        let mut first_vacuous: Option<String> = None;
        let mut first_undet: Option<String> = None;
        let mut first_missing_assert: Option<String> = None;

        for pat in &req.assertions {
            let hits = match_props(&s.props, &task, Kind::Assert, pat);
            if hits.is_empty() {
                missing_assert = true;
                if first_missing_assert.is_none() {
                    first_missing_assert = Some(pat.clone());
                }
                if let Some(disabled) = s.props.iter().find(|p| p.name == *pat && !p.enabled) {
                    issues.push(format!(
                        "assertion `{}` exists but is disabled",
                        disabled.name
                    ));
                } else {
                    issues.push(format!("target assertion `{pat}` is missing"));
                }
                assert_views.push(json!({
                    "name": pat,
                    "found": false,
                    "status": "missing",
                    "vacuity": "not checked",
                    "under_ai": [],
                }));
                continue;
            }
            for p in hits {
                mapped_assert_names.insert(p.name.clone());
                let vac_word = match (p.vacuity, &p.status) {
                    (Some(v), _) => v.word(),
                    (None, Status::Proven { .. }) => "not checked",
                    _ => "not checked",
                };
                let is_vac = p.vacuity.is_some_and(|v| v.is_vacuous());
                if is_vac {
                    any_vacuous += 1;
                    if spec.signoff.forbid_vacuous && first_vacuous.is_none() {
                        first_vacuous = Some(p.name.clone());
                        issues.push(format!(
                            "assertion `{}` proved vacuously (antecedent unreachable)",
                            p.name
                        ));
                    }
                }
                if !p.under_ai.is_empty() {
                    for dep in &p.under_ai {
                        let tag = format!("{} -> {}", p.name, dep);
                        if !any_under_ai.contains(&tag) {
                            any_under_ai.push(tag);
                        }
                    }
                    if spec.signoff.forbid_under_ai {
                        if first_undet.is_none() {
                            first_undet = Some(p.name.clone());
                        }
                        issues.push(format!(
                            "assertion `{}` stands on AI-added assumption/cutpoint [{}]",
                            p.name,
                            p.under_ai.join(", ")
                        ));
                    }
                }
                if matches!(p.status, Status::Cex { .. }) {
                    if first_cex.is_none() {
                        first_cex = Some(p.name.clone());
                    }
                    issues.push(format!("assertion `{}` has a counterexample (cex)", p.name));
                } else if !assertion_meets_target(p, &spec.signoff) {
                    if first_undet.is_none() {
                        first_undet = Some(p.name.clone());
                    }
                    issues.push(format!(
                        "assertion `{}` has status `{}` (requires `{}`)",
                        p.name,
                        p.status.word(),
                        spec.signoff.proof_target
                    ));
                }
                assert_views.push(json!({
                    "name": p.name,
                    "found": true,
                    "status": p.status.word(),
                    "vacuity": vac_word,
                    "under_ai": p.under_ai,
                }));
            }
        }

        let mut missing_or_unreach_cover: Option<String> = None;
        let mut unproved_cover: Option<String> = None;
        if spec.signoff.require_covers && req.covers.is_empty() {
            missing_or_unreach_cover = Some(format!(
                "c_{}",
                req.id.to_ascii_lowercase().replace('-', "_")
            ));
            issues.push("signoff requires at least one witness `cover` per requirement, but `covers` is empty".into());
        }
        for pat in &req.covers {
            let hits = match_props(&s.props, &task, Kind::Cover, pat);
            if hits.is_empty() {
                if missing_or_unreach_cover.is_none() {
                    missing_or_unreach_cover = Some(pat.clone());
                }
                issues.push(format!("mandatory witness cover `{pat}` is missing"));
                cover_views.push(json!({
                    "name": pat,
                    "found": false,
                    "status": "missing",
                }));
                continue;
            }
            for p in hits {
                match p.status {
                    Status::Covered { .. } => {}
                    Status::Unreachable { .. } => {
                        if missing_or_unreach_cover.is_none() {
                            missing_or_unreach_cover = Some(p.name.clone());
                        }
                        issues.push(format!(
                            "mandatory witness cover `{}` is `unreachable` (over-constraint or impossible scenario)",
                            p.name
                        ));
                    }
                    _ => {
                        if unproved_cover.is_none() {
                            unproved_cover = Some(p.name.clone());
                        }
                        issues.push(format!(
                            "witness cover `{}` is `{}` (needs `covered`)",
                            p.name,
                            p.status.word()
                        ));
                    }
                }
                cover_views.push(json!({
                    "name": p.name,
                    "found": true,
                    "status": p.status.word(),
                }));
            }
        }

        let req_has_explicit_bad_assume = req
            .assumptions
            .iter()
            .any(|am| overconstraints.iter().any(|oc| oc.name == *am));
        let req_overconstrained = req_has_explicit_bad_assume || task_tainted_by_env;

        let (status, next_action) = if missing_assert {
            n_missing_assert += 1;
            let miss = first_missing_assert.as_deref().unwrap_or("<assert_name>");
            (
                "missing_assert",
                format!(
                    "Synthesize SVA assertion `{miss}` for `{}` and validate it with `proposal_validate(kind=\"assert\", name=\"{miss}\", ...)`.",
                    req.id
                ),
            )
        } else if let Some(cex_prop) = first_cex {
            n_cex += 1;
            (
                "cex",
                format!(
                    "Call `cex_diagnose(name=\"{cex_prop}\")` to classify RTL bug vs missing primary-input assumption for `{}`.",
                    req.id
                ),
            )
        } else if req_overconstrained {
            n_overconstrained += 1;
            let detail = if let Some(oc) = overconstraints.first() {
                format!(
                    "Remove/rewrite over-constraining assumption `{}` ({}) and re-prove `{}`.",
                    oc.name, oc.reason, req.id
                )
            } else if env_conflict {
                format!(
                    "Resolve contradictory environment assumptions [{}] tainting `{}`.",
                    conflict_set.join(", "),
                    req.id
                )
            } else {
                format!(
                    "Eliminate the 2QBF dead-end trap state in active assumptions tainting `{}`.",
                    req.id
                )
            };
            ("overconstrained", detail)
        } else if let Some(vac_prop) = first_vacuous {
            n_vacuous += 1;
            (
                "vacuous",
                format!(
                    "Assertion `{vac_prop}` is vacuous: inspect its antecedent and active `assume`s, and validate a witness `cover` via `proposal_validate(kind=\"cover\", ...)`."
                ),
            )
        } else if let Some(cov_prop) = missing_or_unreach_cover {
            n_missing_cover += 1;
            (
                "missing_cover",
                format!(
                    "Add or fix witness cover `{cov_prop}` for `{}` so `proposal_validate(kind=\"cover\", name=\"{cov_prop}\", ...)` reaches `covered`.",
                    req.id
                ),
            )
        } else if let Some(u_prop) = first_undet.or(unproved_cover) {
            n_undet += 1;
            (
                "undetermined",
                format!(
                    "Run `prove(names=[\"{u_prop}\"])` or `convergence_diagnose(name=\"{u_prop}\")` to close `{}`.",
                    req.id
                ),
            )
        } else {
            n_verified += 1;
            ("verified", String::new())
        };

        if status != "verified" {
            uncovered_ids.push(req.id.clone());
            push_action(&next_action);
        }

        let title = if !req.title.is_empty() {
            req.title.clone()
        } else {
            req.description.clone()
        };

        req_rows.push(json!({
            "id": req.id,
            "title": title,
            "status": status,
            "assertions": assert_views,
            "covers": cover_views,
            "issues": issues,
            "next_action": next_action,
        }));
    }

    let unmapped_assertions: Vec<String> = s
        .props
        .iter()
        .filter(|p| {
            p.enabled
                && p.task == task
                && p.kind == Kind::Assert
                && !mapped_assert_names.contains(&p.name)
        })
        .map(|p| p.name.clone())
        .collect();

    // 4. Signoff gates table.
    let mut gates: Vec<Value> = Vec::new();
    gates.push(json!({
        "gate": "design_intent",
        "ok": design_ok && clocks_ok,
        "expected": format!(
            "top={}, clocks=[{}], reset={}",
            spec.design.top.as_deref().unwrap_or("*"),
            spec.design.clocks.join(","),
            spec.design.reset.as_deref().unwrap_or("*")
        ),
        "actual": format!(
            "top={}, clocks=[{}], reset={}",
            s.design.top.as_deref().unwrap_or("none"),
            actual_clocks.join(","),
            s.reset.as_deref().unwrap_or("none")
        ),
        "detail": if design_mismatches.is_empty() {
            "Design top, clocks, and reset match specification.".into()
        } else {
            design_mismatches.join("; ")
        },
    }));

    let total_reqs = spec.requirements.len();
    let all_reqs_verified = total_reqs > 0 && n_verified == total_reqs;
    gates.push(json!({
        "gate": "proof_target",
        "ok": all_reqs_verified,
        "expected": format!("{total_reqs}/{total_reqs} requirements verified ({})", spec.signoff.proof_target),
        "actual": format!("{n_verified}/{total_reqs} verified"),
        "detail": if total_reqs == 0 {
            "Specification defines 0 requirements.".into()
        } else if all_reqs_verified {
            "All specification requirements are proven with reachable witness covers.".into()
        } else {
            format!(
                "Remaining: cex={n_cex}, undetermined={n_undet}, missing_assert={n_missing_assert}, missing_cover={n_missing_cover}, vacuous={n_vacuous}, overconstrained={n_overconstrained}"
            )
        },
    }));

    let oc_ok = overconstraints.len() <= spec.signoff.max_overconstraints && !env_conflict;
    gates.push(json!({
        "gate": "zero_overconstraints",
        "ok": oc_ok,
        "expected": format!("<= {} over-constraints and conflict=false", spec.signoff.max_overconstraints),
        "actual": format!("{} over-constraints, conflict={env_conflict}", overconstraints.len()),
        "detail": if oc_ok {
            "All active environment assumptions obey the primary-input boundary.".into()
        } else if env_conflict {
            format!("Contradictory assumptions (UNSAT core: [{}]).", conflict_set.join(", "))
        } else {
            overconstraints
                .iter()
                .map(|o| format!("{}: {}", o.name, o.reason))
                .collect::<Vec<_>>()
                .join("; ")
        },
    }));

    let de_ok = if rt.dead_end_checked {
        dead_end_count <= spec.signoff.max_dead_ends
    } else {
        !spec.signoff.require_dead_end_check
    };
    if !de_ok && !rt.dead_end_checked {
        push_action(
            "Re-run `spec_audit(check_dead_end=true)` to discharge the 2QBF dead-end gate.",
        );
    }
    gates.push(json!({
        "gate": "zero_dead_ends",
        "ok": de_ok,
        "expected": format!(
            "<= {} dead-end states{}",
            spec.signoff.max_dead_ends,
            if spec.signoff.require_dead_end_check { " (2QBF required)" } else { "" }
        ),
        "actual": if rt.dead_end_checked {
            if rt.dead_end_found {
                format!("dead-end trap state found (depth {})", rt.dead_end_depth.unwrap_or(0))
            } else {
                "0 dead-end states (2QBF checked)".into()
            }
        } else {
            "not checked (pass check_dead_end=true)".into()
        },
        "detail": if !rt.dead_end_checked && spec.signoff.require_dead_end_check {
            "Signoff requires 2QBF dead-end verification (`spec_audit(check_dead_end=true)`)."
        } else if rt.dead_end_found {
            "Assumptions can steer into a state with no legal next input."
        } else {
            "No dead-end trap states detected."
        },
    }));

    let vac_ok = !spec.signoff.forbid_vacuous || any_vacuous == 0;
    gates.push(json!({
        "gate": "non_vacuity",
        "ok": vac_ok,
        "expected": if spec.signoff.forbid_vacuous { "0 vacuous assertions" } else { "unrestricted" },
        "actual": format!("{any_vacuous} vacuous assertions"),
        "detail": if vac_ok {
            "Zero mapped assertions proved vacuously.".into()
        } else {
            format!("{any_vacuous} mapped assertion(s) proved vacuously.")
        },
    }));

    if let Some(floor) = spec.signoff.coi_coverage_floor {
        let (cov_ok, actual_str, detail_str) = match coi_coverage_pct(s, &task) {
            Some((pct, cov_n, denom)) => (
                pct + 1e-9 >= floor,
                format!("{pct:.1}% ({cov_n}/{denom})"),
                if pct + 1e-9 >= floor {
                    "COI coverage meets floor.".into()
                } else {
                    format!("COI coverage {pct:.1}% is below floor {floor:.1}%.")
                },
            ),
            None => {
                push_action("Run `check_cov(action=\"init\", models=[\"coi\"])` and `check_cov(action=\"measure\", models=[\"coi\"])` to measure COI coverage.");
                (
                    false,
                    "not measured".into(),
                    "COI coverage has not been measured in this session (`check_cov`).".into(),
                )
            }
        };
        gates.push(json!({
            "gate": "coi_coverage",
            "ok": cov_ok,
            "expected": format!(">= {floor:.1}%"),
            "actual": actual_str,
            "detail": detail_str,
        }));
    }

    if let Some(floor) = spec.signoff.mutation_score_floor {
        let (mut_ok, actual_str, detail_str) = match rt.mutation_score {
            Some(pct) => (
                pct + 1e-9 >= floor,
                format!("{pct:.1}%"),
                if pct + 1e-9 >= floor {
                    "Mutation kill score meets floor.".into()
                } else {
                    format!("Mutation score {pct:.1}% is below floor {floor:.1}%; strengthen SVA assertions.")
                },
            ),
            None => {
                push_action("Run mutant-injection evaluation and pass `mutation_score` to `spec_audit` to discharge `mutation_score_floor`.");
                (
                    false,
                    "not provided".into(),
                    "Pass `mutation_score` from mutant evaluation to verify SVA completeness."
                        .into(),
                )
            }
        };
        gates.push(json!({
            "gate": "mutation_coverage",
            "ok": mut_ok,
            "expected": format!(">= {floor:.1}%"),
            "actual": actual_str,
            "detail": detail_str,
        }));
    }

    if spec.signoff.forbid_under_ai {
        let ai_ok = any_under_ai.is_empty();
        if !ai_ok {
            push_action("Promote verified AI assumptions/assertions into permanent RTL/Tcl SVA and clear `under_ai` dependencies before final signoff.");
        }
        gates.push(json!({
            "gate": "ai_provenance",
            "ok": ai_ok,
            "expected": "0 proofs standing on AI-added assumptions/cutpoints",
            "actual": format!("{} under_ai dependencies", any_under_ai.len()),
            "detail": if ai_ok {
                "All mapped proofs stand strictly on user/RTL properties.".into()
            } else {
                format!("Standing on AI items: {}", any_under_ai.join("; "))
            },
        }));
    }

    let gates_ok = gates.iter().all(|g| g["ok"].as_bool().unwrap_or(false));
    let signoff_ready = s.elaborated && all_reqs_verified && gates_ok;

    let oc_json: Vec<Value> = overconstraints
        .iter()
        .map(|o| {
            json!({
                "name": o.name,
                "expr": o.expr,
                "forbidden_signals": o.forbidden_signals,
                "roles": o.roles,
                "reason": o.reason,
            })
        })
        .collect();

    json!({
        "schema": AUDIT_SCHEMA,
        "spec_source": rt.spec_source,
        "task": task,
        "signoff_ready": signoff_ready,
        "design_check": {
            "ok": design_ok && clocks_ok,
            "expected_top": spec.design.top,
            "actual_top": s.design.top,
            "expected_reset": spec.design.reset,
            "actual_reset": s.reset,
            "clocks_ok": clocks_ok,
            "mismatches": design_mismatches,
        },
        "boundary_audit": {
            "ok": oc_ok && (!rt.dead_end_checked || !rt.dead_end_found),
            "overconstrained_assumptions": oc_json,
            "conflict": env_conflict,
            "conflict_set": conflict_set,
            "dead_end_checked": rt.dead_end_checked,
            "dead_end_found": rt.dead_end_found,
        },
        "summary": {
            "total_requirements": total_reqs,
            "verified": n_verified,
            "cex": n_cex,
            "undetermined": n_undet,
            "missing_assert": n_missing_assert,
            "missing_cover": n_missing_cover,
            "vacuous": n_vacuous,
            "overconstrained": n_overconstrained,
            "unmapped_assertions": unmapped_assertions,
        },
        "gates": gates,
        "requirements": req_rows,
        "uncovered_requirements": uncovered_ids,
        "next_actions": next_actions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matcher_handles_globs_and_exact_names() {
        assert!(wildcard_match("a_*", "a_count_max"));
        assert!(wildcard_match("CSR::a_rw_*", "CSR::a_rw_ctrl"));
        assert!(wildcard_match("c_status?", "c_status3"));
        assert!(!wildcard_match("c_status?", "c_status30"));
        assert!(!wildcard_match("a_max", "a_max2"));
    }

    #[test]
    fn spec_validation_rejects_bad_schema_and_duplicate_ids() {
        let bad_schema = json!({
            "schema": "wrong/1",
            "requirements": []
        });
        assert!(FvpSpec::parse_value(&bad_schema).is_err());

        let dup_ids = json!({
            "schema": "informal.spec/1",
            "requirements": [
                {"id": "REQ-1", "assertions": ["a1"]},
                {"id": "REQ-1", "assertions": ["a2"]}
            ]
        });
        assert!(FvpSpec::parse_value(&dup_ids)
            .unwrap_err()
            .contains("duplicate requirement id"));

        let bad_floor = json!({
            "schema": "informal.spec/1",
            "signoff": {"coi_coverage_floor": 120.0},
            "requirements": []
        });
        assert!(FvpSpec::parse_value(&bad_floor).is_err());
    }
}
