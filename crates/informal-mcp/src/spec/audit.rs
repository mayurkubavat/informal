use super::boundary::{
    assertion_meets_target, audit_assumption_boundary, find_disabled_prop, match_props, norm_expr,
};
use super::schema::{AuditRuntime, FvpSpec, AUDIT_SCHEMA};
use informal_fpv::cov::{CovModel, CovStatus};
use informal_fpv::{Kind, Session, Status, EMBEDDED};
use serde_json::{json, Value};
use std::collections::BTreeSet;

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
                if let Some(disabled) = find_disabled_prop(&s.props, &task, Kind::Assert, pat) {
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
                if let Some(disabled) = find_disabled_prop(&s.props, &task, Kind::Cover, pat) {
                    issues.push(format!(
                        "witness cover `{}` exists but is disabled",
                        disabled.name
                    ));
                } else {
                    issues.push(format!("mandatory witness cover `{pat}` is missing"));
                }
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

        let req_has_explicit_bad_assume = req.assumptions.iter().any(|am| {
            let qual = informal_fpv::session::qualify(&task, am);
            let emb = am
                .strip_prefix(&format!("{EMBEDDED}::"))
                .unwrap_or(am.as_str());
            overconstraints
                .iter()
                .any(|oc| oc.name == *am || oc.name == qual || oc.name == emb)
        });
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
