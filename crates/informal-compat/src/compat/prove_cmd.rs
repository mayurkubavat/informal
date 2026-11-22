use super::props::{prove_and_report, select, select_in};
use super::{note, Args};
use crate::session::{parse_duration, Effort, Kind, Orchestration, Session};
use informal_tcl::Value;

pub(super) fn prove(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-all", 0),
            ("-property", 1),
            ("-time_limit", 1),
            ("-per_property_time_limit", 1),
            ("-total_time_limit", 1),
            ("-effort", 1),
            ("-orchestration", 1),
            ("-task", 1),
            ("-bound", 1),
            ("-with_proof_core", 0),
        ],
        &[
            ("-bg", 0),
            ("-engine_mode", 1),
            ("-dump_trace", 0),
            ("-sst", 1),
        ],
    )?;
    if let Some(t) = a.val("-time_limit") {
        s.time_limit = parse_duration(t)?;
    }
    let run_per_prop = a
        .val("-per_property_time_limit")
        .map(parse_duration)
        .transpose()?;
    let run_total = a
        .val("-total_time_limit")
        .map(|v| parse_opt_duration("prove -total_time_limit", v))
        .transpose()?;
    let run_effort = a
        .val("-effort")
        .map(|v| Effort::parse(v).map_err(|e| format!("prove -effort: {e}")))
        .transpose()?;
    let run_orch = a
        .val("-orchestration")
        .map(|v| Orchestration::parse(v).map_err(|e| format!("prove -orchestration: {e}")))
        .transpose()?;
    let run_bound =
        match a.val("-bound") {
            None => None,
            Some(v) => Some(v.parse::<u32>().ok().filter(|&n| n >= 1).ok_or_else(|| {
                format!("prove -bound: positive cycle bound expected, not `{v}`")
            })?),
        };
    let task = a.val("-task").map(|t| s.task_name(t)).transpose()?;
    let which: Vec<String> = a.all("-property").flatten().cloned().collect();
    let which = match (&task, which.is_empty()) {
        // -task alone: every enabled assertion and cover of that task.
        (Some(t), true) => {
            let members: Vec<usize> = s
                .props_of(t)
                .into_iter()
                .filter(|&i| s.props[i].enabled && s.props[i].kind != Kind::Assume)
                .collect();
            if members.is_empty() {
                note(&format!("prove -task {t}: the task has nothing to prove"));
                return Ok(().into());
            }
            members
        }
        (_, true) => {
            if !s.dry_run {
                let cur_has_any = s
                    .props
                    .iter()
                    .any(|p| p.enabled && p.kind != Kind::Assume && p.task == s.current_task);
                if !cur_has_any {
                    let mut other_tasks: Vec<String> = s
                        .tasks
                        .iter()
                        .map(|t| t.name.clone())
                        .filter(|t| {
                            t != &s.current_task
                                && s.props
                                    .iter()
                                    .any(|p| p.enabled && p.kind != Kind::Assume && p.task == *t)
                        })
                        .collect();
                    other_tasks.dedup();
                    if let Some(first) = other_tasks.first() {
                        s.log(&format!(
                            "note: current task `{}` has 0 provable properties; non-empty task(s): {} (use `prove -task {first}` or `task -set {first}`)",
                            s.current_task,
                            other_tasks.join(", ")
                        ));
                    }
                }
            }
            Vec::new()
        }
        (t, false) => select_in(s, t.as_deref(), &which)?,
    };
    let saved_bound = s.prove_bound;
    let saved_pc = s.with_proof_core;
    let saved_tl = s.time_limit;
    let saved_total = s.total_time_limit;
    let saved_effort = s.effort;
    let saved_orch = s.orchestration;
    s.prove_bound = run_bound;
    s.with_proof_core = a.flag("-with_proof_core");
    if let Some(d) = run_per_prop {
        s.time_limit = d;
    }
    if let Some(tot) = run_total {
        s.total_time_limit = tot;
    }
    if let Some(eff) = run_effort {
        s.effort = eff;
    }
    if let Some(orch) = run_orch {
        s.orchestration = orch;
    }
    let res = prove_and_report(s, &which);
    s.prove_bound = saved_bound;
    s.with_proof_core = saved_pc;
    if run_per_prop.is_some() {
        s.time_limit = saved_tl;
    }
    s.total_time_limit = saved_total;
    s.effort = saved_effort;
    s.orchestration = saved_orch;
    res
}

pub(super) fn set_prove_time_limit(s: &mut Session, a: &[String]) -> Result<Value, String> {
    s.time_limit = parse_duration(a.get(1).ok_or("set_prove_time_limit: time required")?)?;
    Ok(().into())
}

// informal's limit is per property already.
pub(super) fn set_prove_per_property_time_limit(
    s: &mut Session,
    a: &[String],
) -> Result<Value, String> {
    s.time_limit = parse_duration(
        a.get(1)
            .ok_or("set_prove_per_property_time_limit: time required")?,
    )?;
    Ok(().into())
}

pub(super) fn set_prove_total_time_limit(s: &mut Session, a: &[String]) -> Result<Value, String> {
    match a.len() {
        1 => Ok(s
            .total_time_limit
            .map(format_duration_short)
            .unwrap_or_default()
            .into()),
        2 => {
            s.total_time_limit = parse_opt_duration("set_prove_total_time_limit", &a[1])?;
            Ok(().into())
        }
        _ => Err("set_prove_total_time_limit: expected at most 1 argument".into()),
    }
}

pub(super) fn set_prove_effort(s: &mut Session, a: &[String]) -> Result<Value, String> {
    match a.len() {
        1 => Ok(s.effort.as_str().into()),
        2 => {
            s.effort = Effort::parse(&a[1]).map_err(|e| format!("set_prove_effort: {e}"))?;
            Ok(().into())
        }
        _ => Err("set_prove_effort: expected at most 1 argument".into()),
    }
}

pub(super) fn set_prove_orchestration(s: &mut Session, a: &[String]) -> Result<Value, String> {
    match a.len() {
        1 => Ok(s.orchestration.as_str().into()),
        2 => {
            s.orchestration =
                Orchestration::parse(&a[1]).map_err(|e| format!("set_prove_orchestration: {e}"))?;
            Ok(().into())
        }
        _ => Err("set_prove_orchestration: expected at most 1 argument".into()),
    }
}

pub(super) fn set_max_trace_length(s: &mut Session, a: &[String]) -> Result<Value, String> {
    match a.len() {
        1 => Ok(s
            .max_trace_length
            .map(|n| n.to_string())
            .unwrap_or_default()
            .into()),
        2 => {
            s.max_trace_length = parse_opt_trace_len("set_max_trace_length", &a[1])?;
            Ok(().into())
        }
        _ => Err("set_max_trace_length: expected at most 1 argument".into()),
    }
}

pub(super) fn set_fml_var(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let (k, v) = (
        a.get(1).ok_or("set_fml_var: name required")?,
        a.get(2).ok_or("set_fml_var: value required")?,
    );
    match k.as_str() {
        "fml_max_time" => s.time_limit = parse_duration(v)?,
        "fml_effort" => {
            s.effort = Effort::parse(v).map_err(|e| format!("set_fml_var fml_effort: {e}"))?;
        }
        "fml_max_depth" => {
            s.max_trace_length = parse_opt_trace_len("set_fml_var fml_max_depth", v)?;
        }
        _ => note(&format!("set_fml_var: ignoring {k}")),
    }
    Ok(().into())
}

pub(super) fn check_fv(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-property", 1), ("-effort", 1), ("-time_limit", 1)],
        &[("-block", 0), ("-run_finish", 1), ("-break", 1)],
    )?;
    let run_effort = a
        .val("-effort")
        .map(|v| Effort::parse(v).map_err(|e| format!("check_fv -effort: {e}")))
        .transpose()?;
    let run_tl = a.val("-time_limit").map(parse_duration).transpose()?;
    let which: Vec<String> = a.all("-property").flatten().cloned().collect();
    let which = if which.is_empty() {
        Vec::new()
    } else {
        select(s, &which)?
    };
    let saved_effort = s.effort;
    let saved_tl = s.time_limit;
    if let Some(eff) = run_effort {
        s.effort = eff;
    }
    if let Some(tl) = run_tl {
        s.time_limit = tl;
    }
    let res = prove_and_report(s, &which);
    s.effort = saved_effort;
    if run_tl.is_some() {
        s.time_limit = saved_tl;
    }
    res
}

fn format_duration_short(d: std::time::Duration) -> String {
    if d.subsec_nanos() == 0 {
        format!("{}s", d.as_secs())
    } else {
        format!("{}ms", d.as_millis())
    }
}

fn parse_opt_duration(cmd: &str, raw: &str) -> Result<Option<std::time::Duration>, String> {
    if matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "0" | "0s" | "0ms" | "off" | "none" | "unlimited"
    ) {
        return Ok(None);
    }
    let d = parse_duration(raw).map_err(|e| format!("{cmd}: {e}"))?;
    Ok((!d.is_zero()).then_some(d))
}

fn parse_opt_trace_len(cmd: &str, raw: &str) -> Result<Option<u32>, String> {
    if matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "0" | "off" | "none" | "unlimited"
    ) {
        return Ok(None);
    }
    raw.trim()
        .parse::<u32>()
        .ok()
        .filter(|&n| n >= 1)
        .map(Some)
        .ok_or_else(|| format!("{cmd}: positive integer or `0`/`off` expected, not `{raw}`"))
}
