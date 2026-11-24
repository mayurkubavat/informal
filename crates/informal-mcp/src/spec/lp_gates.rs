use super::schema::LowPowerGates;
use informal_fpv::lp::{formal::aag::LOW_POWER_TASK, LpSeverity, LpStatus};
use informal_fpv::{Kind, Session, Status};
use serde_json::{json, Value};

/// Evaluate `fvp.spec.json` `low_power` signoff gates against the live `Session`.
/// Returns `(gate_entry, next_actions)`.
pub(super) fn evaluate_low_power_gate(
    s: &Session,
    lp_gates: &LowPowerGates,
) -> (Value, Vec<String>) {
    let mut failures: Vec<String> = Vec::new();
    let mut actions: Vec<String> = Vec::new();

    let upf_loaded = !s.lp.upf_files.is_empty() || s.lp.intent.is_some();
    let stages_ran = !s.lp.stages_run.is_empty();
    let formal_ran = s.lp.stages_run.iter().any(|st| st == "formal");

    let open_errors = s
        .lp
        .violations
        .iter()
        .filter(|v| {
            (v.status == LpStatus::Open && v.severity == LpSeverity::Error)
                || v.status == LpStatus::FormalCex
        })
        .count();
    let formal_cex_violations = s
        .lp
        .violations
        .iter()
        .filter(|v| v.status == LpStatus::FormalCex)
        .count();
    let manual_waived = s
        .lp
        .violations
        .iter()
        .filter(|v| v.status == LpStatus::Waived)
        .count();
    let formally_waived = s
        .lp
        .violations
        .iter()
        .filter(|v| v.status == LpStatus::FormallyWaived)
        .count();

    let lp_task_failing_asserts: Vec<&str> = s
        .props
        .iter()
        .filter(|p| {
            p.enabled
                && p.task == LOW_POWER_TASK
                && p.kind == Kind::Assert
                && !matches!(p.status, Status::Proven { .. })
        })
        .map(|p| p.name.as_str())
        .collect();

    if lp_gates.require_upf && !upf_loaded {
        failures.push("no UPF/CPF power intent is loaded (`load_upf` required)".into());
        actions.push("Call `load_upf(file=...)` to load power intent before LP signoff.".into());
    }

    if let Some(max_err) = lp_gates.max_open_lp_errors {
        if upf_loaded && !stages_ran {
            failures.push("`check_lp` has not been run yet".into());
            actions.push("Run `check_lp(stage=\"all\")` to evaluate low-power rules.".into());
        } else if open_errors > max_err {
            failures.push(format!(
                "{open_errors} open/CEX LP error(s) exceed `max_open_lp_errors` ({max_err})"
            ));
            actions.push(
                "Call `lp_diagnose(id=...)` on each open/CEX violation, fix UPF/RTL, or run `check_lp(prove_waivers=true)`."
                    .into(),
            );
        }
    }

    if lp_gates.require_formal_lpv {
        if !formal_ran {
            failures.push("formal LPV stage has not run (`require_formal_lpv: true`)".into());
            actions.push(
                "Run `check_lp(stage=\"all\", prove_corruption=true)` to discharge formal LPV signoff."
                    .into(),
            );
        }
        if formal_cex_violations > 0 {
            failures.push(format!(
                "{formal_cex_violations} LP violation(s) failed formal corruption/auto-waiver checks (`cex`)"
            ));
            actions.push(
                "Call `lp_diagnose` on each formal `cex` violation to inspect the `why_cex` causal slice."
                    .into(),
            );
        }
        if !lp_task_failing_asserts.is_empty() {
            failures.push(format!(
                "LowPower task has unproved/failing assertion(s): [{}]",
                lp_task_failing_asserts.join(", ")
            ));
            actions.push(format!(
                "Diagnose failing `LowPower` assertion `{}` via `cex_diagnose` or `why_cex`.",
                lp_task_failing_asserts[0]
            ));
        }
    }

    if !lp_gates.allow_manual_lp_waivers && manual_waived > 0 {
        failures.push(format!(
            "{manual_waived} violation(s) rely on manual waivers (`allow_manual_lp_waivers: false`; only `formally_waived` certified waivers are permitted)"
        ));
        actions.push(
            "Remove manual LP waivers and discharge masking via `check_lp(stage=\"all\", prove_corruption=true, prove_waivers=true)`."
                .into(),
        );
    }

    let ok = failures.is_empty();
    let expected = format!(
        "upf={}, max_open_lp_errors={}, formal_lpv={}, allow_manual_waivers={}",
        lp_gates.require_upf,
        lp_gates
            .max_open_lp_errors
            .map_or_else(|| "*".into(), |n| n.to_string()),
        lp_gates.require_formal_lpv,
        lp_gates.allow_manual_lp_waivers
    );
    let actual = format!(
        "upf_loaded={upf_loaded}, stages=[{}], open_errors={open_errors}, manual_waived={manual_waived}, formally_waived={formally_waived}, formal_cex={formal_cex_violations}",
        s.lp.stages_run.join(",")
    );
    let detail = if ok {
        "All low-power verification signoff gates passed.".to_string()
    } else {
        failures.join("; ")
    };

    let gate = json!({
        "gate": "low_power",
        "ok": ok,
        "expected": expected,
        "actual": actual,
        "detail": detail,
    });
    (gate, actions)
}
