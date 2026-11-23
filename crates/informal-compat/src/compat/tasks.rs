use super::{expand_env, note, Args};
use crate::session::{glob, Fairness, Kind, Session};
use informal_tcl::Value;
use std::path::PathBuf;

/// `task`: JasperGold's tasks (decision D20).
///
/// - `task -create <name> [-source_task <t>] [-copy <names>] [-regexp]
///   [-copy_asserts] [-copy_assumes] [-copy_covers] [-copy_stopats] [-set]`
/// - `task -edit <name>` with the same copy options
/// - `task -set <name>`, `task -list`, `task -delete <name>`
/// - `task` alone: the current task's name.
///
/// A copy is named `<task>::<name>` and starts unprocessed. Copies come
/// from the source task (`-source_task`, else the current task).
pub(super) fn task(s: &mut Session, a: &[String]) -> Result<Value, String> {
    // `-set <name>` alone switches; with -create or -edit it is a flag.
    if a.len() == 3 && a[1] == "-set" {
        s.set_task(&a[2]).map_err(|e| format!("task: {e}"))?;
        return Ok(a[2].clone().into());
    }
    let a = Args::parse(
        a,
        &[
            ("-create", 1),
            ("-edit", 1),
            ("-delete", 1),
            ("-list", 0),
            ("-set", 0),
            ("-source_task", 1),
            ("-copy", 1),
            ("-regexp", 0),
            ("-copy_asserts", 0),
            ("-copy_assumes", 0),
            ("-copy_covers", 0),
            ("-copy_stopats", 0),
            ("-copy_all", 0),
        ],
        &[
            ("-copy_ratings", 0),
            ("-copy_abstractions", 1),
            ("-copy_related_covers", 0),
            ("-copy_proof_settings", 0),
        ],
    )?;
    if a.flag("-list") {
        return Ok(Value::List(
            s.tasks.iter().map(|t| t.name.clone()).collect(),
        ));
    }
    if let Some(t) = a.val("-delete") {
        s.delete_task(t).map_err(|e| format!("task: {e}"))?;
        return Ok(().into());
    }
    let name = match (a.val("-create"), a.val("-edit")) {
        (Some(n), None) => {
            s.create_task(n).map_err(|e| format!("task: {e}"))?;
            n.to_string()
        }
        (None, Some(n)) => s.task_name(n).map_err(|e| format!("task: {e}"))?,
        (Some(_), Some(_)) => return Err("task: -create and -edit together".into()),
        (None, None) => {
            if !a.pos.is_empty() || a.opts.iter().any(|(k, _)| k != "-set") {
                return Err("task: -create, -edit, -set, -list or -delete is needed".into());
            }
            return Ok(s.current_task.clone().into());
        }
    };
    let source = match a.val("-source_task") {
        Some(t) => s.task_name(t).map_err(|e| format!("task: {e}"))?,
        None => s.current_task.clone(),
    };
    if source == name && a.val("-create").is_some() {
        return Err(format!(
            "task: -source_task {source} is the task being made"
        ));
    }
    let from = s.props_of(&source);
    let all = a.flag("-copy_all");
    let mut which: Vec<usize> = from
        .iter()
        .copied()
        .filter(|&i| match s.props[i].kind {
            Kind::Assert => all || a.flag("-copy_asserts"),
            Kind::Assume => all || a.flag("-copy_assumes"),
            Kind::Cover => all || a.flag("-copy_covers"),
        })
        .collect();
    for pats in a.all("-copy") {
        let words: Vec<String> = pats[0].split_whitespace().map(String::from).collect();
        // Names are looked for in the source task.
        let hit = |p: &crate::session::Property, w: &str| -> Result<bool, String> {
            Ok(if a.flag("-regexp") {
                let re = regex::Regex::new(w)
                    .map_err(|e| format!("task: bad regular expression `{w}`: {e}"))?;
                re.is_match(&p.name) || re.is_match(p.short_name())
            } else {
                glob(w, &p.name) || glob(w, p.short_name())
            })
        };
        for w in &words {
            let mut found = false;
            for &i in &from {
                if hit(&s.props[i], w)? {
                    which.push(i);
                    found = true;
                }
            }
            if !found {
                note(&format!(
                    "task: -copy {w}: no property of task {source} matches"
                ));
            }
        }
    }
    which.sort();
    which.dedup();
    s.copy_into(&name, &which)
        .map_err(|e| format!("task: {e}"))?;
    if all || a.flag("-copy_stopats") {
        s.copy_stopats(&source, &name)
            .map_err(|e| format!("task: {e}"))?;
    }
    if a.flag("-set") {
        s.set_task(&name).map_err(|e| format!("task: {e}"))?;
    }
    Ok(name.into())
}

/// `stopat sig ...` (JasperGold) / `snip_driver sig ...` (VC Formal): cut
/// each signal from its drivers, so it is free on every step. `-clear`
/// removes the named cut points (all without names); `-list` returns them.
pub(super) fn stopat(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-clear", 0), ("-list", 0), ("-task", 1)],
        &[("-env", 0)],
    )?;
    // The task's own cut points: `-task`, else the current task.
    let task = match a.val("-task") {
        Some(t) => s.task_name(t).map_err(|e| format!("{}: {e}", a.cmd))?,
        None => s.current_task.clone(),
    };
    let names: Vec<String> = a
        .pos
        .iter()
        .flat_map(|w| w.split_whitespace().map(String::from))
        .collect();
    if a.flag("-list") {
        return Ok(Value::List(s.stopats_of(&task).to_vec()));
    }
    let names = names
        .iter()
        .map(|n| s.cut_point(n).map_err(|e| format!("{}: {e}", a.cmd)))
        .collect::<Result<Vec<_>, _>>()?;
    let cuts = s.stopats_mut(&task)?;
    if a.flag("-clear") {
        if names.is_empty() {
            cuts.clear();
        } else {
            cuts.retain(|x| !names.contains(x));
        }
        s.lp.invalidate_formal_waivers();
        return Ok(().into());
    }
    if names.is_empty() {
        return Err(format!("{}: signal name(s) required", a.cmd));
    }
    for n in names {
        if !cuts.contains(&n) {
            cuts.push(n);
        }
    }
    s.lp.invalidate_formal_waivers();
    Ok(().into())
}

pub(super) fn set_vacuity(s: &mut Session, a: &[String]) -> Result<Value, String> {
    match a.get(1).map(String::as_str) {
        None => {
            return Ok(Value::from(if s.environment_depth > 0 {
                "on"
            } else {
                "off"
            }));
        }
        Some("on") => {
            if s.environment_depth == 0 {
                s.environment_depth = 20;
            }
            s.preconditions = true;
        }
        Some("off") => {
            s.environment_depth = 0;
            s.preconditions = false;
        }
        Some(v) => return Err(format!("set_vacuity: on or off, not `{v}`")),
    }
    Ok(().into())
}

pub(super) fn get_task_list(s: &mut Session, _: &[String]) -> Result<Value, String> {
    Ok(Value::List(
        s.tasks.iter().map(|t| t.name.clone()).collect(),
    ))
}

/// `check_assumptions [-depth N] [-conflict] [-dead_end]`: checks whether
/// the assumptions and reset allow traces of N model steps (`-conflict`
/// returns the minimal conflicting assumption core) or reach a trap state
/// from which no next input satisfies the assumptions (`-dead_end`).
/// `check_assumptions -live [-conflict]` checks instead whether an infinite
/// trace meets the constraints and the fairness assumptions (docs/vacuity.md).
pub(super) fn check_assumptions(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-depth", 1),
            ("-conflict", 0),
            ("-dead_end", 0),
            ("-live", 0),
        ],
        &[],
    )?;
    if a.flag("-live") {
        return check_fairness(s, &a);
    }
    let depth = match a.val("-depth") {
        None => s.environment_depth.max(1),
        Some(v) => v
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=10_000).contains(n))
            .ok_or_else(|| format!("check_assumptions: -depth takes 1 to 10000, not `{v}`"))?,
    };
    if s.dry_run {
        return Ok(().into());
    }
    if a.flag("-dead_end") {
        let de = s
            .dead_end_now(depth)
            .map_err(|e| format!("check_assumptions -dead_end: {e}"))?;
        return match de {
            None => Ok(Value::from("alive")),
            Some(found) => {
                let bits: Vec<String> = found
                    .trap_state
                    .iter()
                    .map(|(n, b)| format!("{n}={}", u8::from(*b)))
                    .collect();
                let desc = if bits.is_empty() {
                    format!("dead_end at step {}", found.depth)
                } else {
                    format!("dead_end at step {}: {}", found.depth, bits.join(", "))
                };
                s.log(&format!("DEAD-END ENVIRONMENT: {desc}"));
                Ok(Value::from(desc))
            }
        };
    }
    let env = s
        .environment_now(depth)
        .map_err(|e| format!("check_assumptions: {e}"))?;
    match env.allowed {
        // -conflict: the assumptions that conflict, as a list (empty when
        // none do).
        Some(_) if a.flag("-conflict") => Ok(Value::List(env.conflict_set)),
        Some(n) => Ok(Value::from(n.to_string())),
        None => Err("check_assumptions: could not decide (no in-house BMC, or out of time)".into()),
    }
}

/// `check_assumptions -live [-conflict]`: whether an infinite trace meets
/// every constraint of the current task and every fairness assumption
/// infinitely often. Returns `consistent` (a fair lasso was found),
/// `vacuous` (none exists: every liveness proof under these assumptions is
/// vacuous), `undetermined` (a bounded or timed-out search, which shows
/// nothing either way) or `none` (no fairness assumption to check). With
/// `-conflict`, the assumptions that allow no fair trace, as a list (empty
/// when none was shown). `-depth` and `-dead_end` bound finite traces and
/// do not combine with it.
fn check_fairness(s: &mut Session, a: &Args) -> Result<Value, String> {
    for other in ["-depth", "-dead_end"] {
        if a.flag(other) {
            return Err(format!(
                "check_assumptions: -live checks infinite fair traces and takes no {other}; run check_assumptions {other} on its own"
            ));
        }
    }
    if s.dry_run {
        return Ok(().into());
    }
    let narrow = a.flag("-conflict");
    let env = s
        .fairness_now(narrow)
        .map_err(|e| format!("check_assumptions -live: {e}"))?;
    Ok(match (&env.verdict, narrow) {
        (Fairness::Unknown(reason), true) => {
            return Err(format!(
                "check_assumptions -live: could not decide ({reason})"
            ))
        }
        (_, true) => Value::List(env.conflict_set),
        (Fairness::NoFairness, false) => Value::from("none"),
        (Fairness::Satisfiable, false) => Value::from("consistent"),
        (Fairness::Vacuous | Fairness::VacuousConstraints, false) => Value::from("vacuous"),
        (Fairness::Unknown(_), false) => Value::from("undetermined"),
    })
}

pub(super) fn set_prove_cache(s: &mut Session, a: &[String]) -> Result<Value, String> {
    match a.get(1).map(|v| v.to_ascii_lowercase()).as_deref() {
        None => Ok(Value::from(if s.prove_cache { "on" } else { "off" })),
        Some("on" | "1" | "true" | "yes") => {
            s.prove_cache = true;
            Ok(().into())
        }
        Some("off" | "0" | "false" | "no") => {
            s.prove_cache = false;
            Ok(().into())
        }
        Some(v) => Err(format!("set_prove_cache: expected on or off, not `{v}`")),
    }
}

pub(super) fn set_prove_cache_path(s: &mut Session, a: &[String]) -> Result<Value, String> {
    match a.get(1) {
        None => {
            let dir = s
                .prove_cache_dir
                .clone()
                .unwrap_or_else(|| s.workdir.join("prove_cache"));
            Ok(Value::from(dir.to_string_lossy().into_owned()))
        }
        Some(p) => {
            let expanded = PathBuf::from(expand_env(p));
            s.prove_cache_dir = Some(std::path::absolute(&expanded).unwrap_or(expanded));
            Ok(().into())
        }
    }
}

pub(super) fn set_prove_cache_mode(s: &mut Session, a: &[String]) -> Result<Value, String> {
    match a.get(1) {
        None => Ok(Value::from(s.prove_cache_mode.as_str())),
        Some(m) => {
            let mode =
                crate::cache::CacheMode::parse(&m.to_ascii_lowercase()).ok_or_else(|| {
                    format!(
                    "set_prove_cache_mode: expected read_write, read_only or write_only, not `{m}`"
                )
                })?;
            s.prove_cache_mode = mode;
            Ok(().into())
        }
    }
}
