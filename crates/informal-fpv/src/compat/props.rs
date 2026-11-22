use super::{note, Args};
use crate::session::{glob, qualify, Kind, Session, SourceLoc, EMBEDDED};
use informal_tcl::Value;
use std::path::Path;

fn add_props(
    s: &mut Session,
    task: &str,
    kind: Kind,
    name: Option<String>,
    expr: &str,
) -> Result<Value, String> {
    let name = s.add_property_in(task, kind, name, expr)?;
    let loc = match informal_tcl::caller_location() {
        Some((file, line)) => Some(SourceLoc::new(&file, line)),
        None => informal_tcl::current_script().and_then(|f| SourceLoc::search(&f, &name)),
    };
    if let Some(p) = s.props.iter_mut().find(|p| p.name == name) {
        p.source = loc;
    }
    if kind == Kind::Assume {
        s.lp.invalidate_formal_waivers();
    }
    Ok(name.into())
}

/// Properties by name or glob. A property of the RTL can also be named
/// without its path (`a_never` for `top.a_never`, `u.a_x` for
/// `top.u.a_x`); a short name that is not a glob must then name exactly one.
pub(super) fn select(s: &Session, patterns: &[String]) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for p in patterns.iter().flat_map(|p| p.split_whitespace()) {
        let mut m = s.matching(p);
        if m.is_empty() {
            let suffix = format!("*.{p}");
            m = s
                .props
                .iter()
                .enumerate()
                .filter(|(_, q)| q.rtl && glob(&suffix, &q.name))
                .map(|(i, _)| i)
                .collect();
            if m.len() > 1 && !p.contains(['*', '?']) {
                let names: Vec<&str> = m.iter().map(|&i| s.props[i].name.as_str()).collect();
                return Err(format!(
                    "`{p}` is ambiguous: it names {}; give the full name",
                    names.join(", ")
                ));
            }
        }
        if m.is_empty() {
            return Err(format!("no property matches `{p}`"));
        }
        out.extend(m);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Properties whose names a regular expression matches anywhere in them,
/// as Tcl's `regexp` does (`-regexp`; anchor with `^` and `$`).
fn select_regexp(s: &Session, patterns: &[String]) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for p in patterns.iter().flat_map(|p| p.split_whitespace()) {
        let re = regex::Regex::new(p).map_err(|e| format!("bad regular expression `{p}`: {e}"))?;
        // An embedded property is also matched by its full name,
        // `<embedded>::<name>`, as JasperGold scripts write it.
        let m: Vec<usize> = s
            .props
            .iter()
            .enumerate()
            .filter(|(_, q)| {
                re.is_match(&q.name)
                    || (q.task == EMBEDDED && re.is_match(&format!("{EMBEDDED}::{}", q.name)))
            })
            .map(|(i, _)| i)
            .collect();
        if m.is_empty() {
            return Err(format!("no property matches the regular expression `{p}`"));
        }
        out.extend(m);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// [`select`] with `-task`: a name is first looked for in that task
/// (`a_x` there is `<task>::a_x`).
pub(super) fn select_in(
    s: &Session,
    task: Option<&str>,
    patterns: &[String],
) -> Result<Vec<usize>, String> {
    let Some(t) = task else {
        return select(s, patterns);
    };
    let mut out = Vec::new();
    for p in patterns.iter().flat_map(|p| p.split_whitespace()) {
        let q = qualify(t, p);
        let mut m: Vec<usize> = s.matching(&q);
        m.retain(|&i| s.props[i].task == t);
        if m.is_empty() {
            m = select(s, &[p.to_string()])?;
        }
        out.extend(m);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

pub(super) fn set_enabled(s: &mut Session, patterns: &[String], on: bool) -> Result<Value, String> {
    let mut changed_assume = false;
    for i in select(s, patterns)? {
        if s.props[i].kind == Kind::Assume && s.props[i].enabled != on {
            changed_assume = true;
        }
        s.props[i].enabled = on;
    }
    if changed_assume {
        s.lp.invalidate_formal_waivers();
    }
    Ok(().into())
}

pub(super) fn prove_and_report(s: &mut Session, which: &[usize]) -> Result<Value, String> {
    let done = s.prove(which).map_err(|e| format!("{e:#}"))?;
    if s.dry_run {
        return Ok(().into());
    }
    print!("{}", s.report());
    let summary = |w: &str| {
        done.iter()
            .filter(|&&i| s.props[i].status.word() == w)
            .count()
    };
    Ok(format!(
        "proven {} cex {} covered {} unreachable {} undetermined {} error {}",
        summary("proven"),
        summary("cex"),
        summary("covered"),
        summary("unreachable"),
        summary("undetermined"),
        summary("error")
    )
    .into())
}

/// Property names filtered by `{key value ...}` pairs on type and status.
fn filter_props(
    s: &Session,
    include: Option<&str>,
    exclude: Option<&str>,
    regexp: bool,
    task: Option<&str>,
) -> Result<Vec<String>, String> {
    let task = task.map(|t| s.task_name(t)).transpose()?;
    let pairs = |spec: Option<&str>| -> Vec<(String, String)> {
        let w: Vec<String> = spec
            .unwrap_or("")
            .split_whitespace()
            .map(String::from)
            .collect();
        w.chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| (c[0].clone(), c[1].clone()))
            .collect()
    };
    let (inc, exc) = (pairs(include), pairs(exclude));
    // With -regexp, a name value is a regular expression.
    let mut res = std::collections::HashMap::new();
    if regexp {
        for (k, v) in inc
            .iter()
            .chain(&exc)
            .filter(|(k, _)| k == "name" || k == "-name")
        {
            let re =
                regex::Regex::new(v).map_err(|e| format!("bad regular expression `{v}`: {e}"))?;
            res.insert((k.clone(), v.clone()), re);
        }
    }
    let matches = |p: &crate::session::Property, (k, v): &(String, String)| match k.as_str() {
        "type" | "-type" => p.kind.label() == v || (v == "assertion" && p.kind == Kind::Assert),
        "status" | "-status" => {
            glob(v, p.status.word()) || (v == "falsified" && p.status.word() == "cex")
        }
        "name" | "-name" => match res.get(&(k.clone(), v.clone())) {
            Some(re) => re.is_match(&p.name),
            None => glob(v, &p.name),
        },
        "disabled" | "-disabled" => (v == "1" || v == "true") != p.enabled,
        "task" | "-task" => p.task == *v,
        _ => true,
    };
    Ok(s.props
        .iter()
        .filter(|p| task.as_deref().is_none_or(|t| p.task == t))
        .filter(|p| inc.iter().all(|kv| matches(p, kv)) && !exc.iter().any(|kv| matches(p, kv)))
        .map(|p| p.name.clone())
        .collect())
}

pub(super) fn set_engines(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let names: Vec<String> = a[1..]
        .iter()
        .flat_map(|w| w.split_whitespace().map(String::from))
        .collect();
    // A name that matches nothing would otherwise run every engine.
    let known = s.engine_names();
    let unknown: Vec<&str> = names
        .iter()
        .filter(|n| !known.contains(n))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "set_engines: unknown engine(s) {}; available: {}",
            unknown.join(", "),
            known.join(", ")
        ));
    }
    s.engine_filter = names;
    Ok(().into())
}

pub(super) fn get_property_list(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-include", 1),
            ("-exclude", 1),
            ("-regexp", 0),
            ("-task", 1),
        ],
        &[],
    )?;
    Ok(filter_props(
        s,
        a.val("-include"),
        a.val("-exclude"),
        a.flag("-regexp"),
        a.val("-task"),
    )?
    .into())
}

pub(super) fn get_status(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let n = a.get(1).ok_or("get_status: property name required")?;
    let i = *select(s, std::slice::from_ref(n))?.first().unwrap();
    Ok(s.props[i].status.word().into())
}

pub(super) fn set_engine_mode(s: &mut Session, a: &[String]) -> Result<Value, String> {
    // Jasper engine letters to the closest informal engines.
    let mut names = Vec::new();
    for m in a[1..].iter().flat_map(|w| w.split_whitespace()) {
        let mapped: &[&str] = match m {
            "Ht" | "Hp" | "Hps" | "Hts" | "Mp" | "AM" | "AD" | "M" | "L" | "R" => &[
                "informal-kind-ic3",
                "informal-ic3-mt",
                "ric3-ic3",
                "abc-pdr",
            ],
            "B" | "Bm" | "K" | "C" | "C2" | "I" | "Tri" | "Q3" | "U" => {
                &["informal-bmc", "informal-wbmc", "ric3-bmc"]
            }
            "N" | "D" | "G" | "G2" => &["informal-kind", "ric3"],
            _ => {
                note(&format!("set_engine_mode: no informal engine for `{m}`"));
                &[]
            }
        };
        names.extend(mapped.iter().map(|n| n.to_string()));
    }
    names.dedup();
    // Vendor engine letters are a hint: keep the ones this build has.
    let known = s.engine_names();
    names.retain(|n| known.contains(n));
    if names.is_empty() {
        note("set_engine_mode: none of the matching engines is available; using all engines");
    } else {
        note(&format!("set_engine_mode: using {}", names.join(", ")));
    }
    s.engine_filter = names;
    Ok(().into())
}

pub(super) fn report(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-summary", 0),
            ("-results", 0),
            ("-csv", 0),
            ("-file", 1),
            ("-force", 0),
            ("-task", 1),
            ("-design", 0),
            ("-detail", 0),
            ("-property", 1),
        ],
        &[],
    )?;
    let task = a.val("-task").map(|t| s.task_name(t)).transpose()?;
    let r = if a.flag("-design") {
        if s.dry_run {
            return Ok(().into());
        }
        s.design_info().text()
    } else if a.flag("-detail") || a.val("-property").is_some() {
        // One block per property: what its last prove measured.
        let which: Vec<usize> = match a.val("-property") {
            Some(p) => select_in(s, task.as_deref(), &[p.to_string()])?,
            None => (0..s.props.len())
                .filter(|&i| task.as_deref().is_none_or(|t| s.props[i].task == t))
                .collect(),
        };
        which
            .iter()
            .map(|&i| s.property_info(i, false).text())
            .collect::<Vec<_>>()
            .join("\n")
    } else if a.flag("-csv") {
        s.report_csv_of(task.as_deref())
    } else {
        s.report_of(task.as_deref())
    };
    if s.dry_run {
        return Ok(r.into());
    }
    match a.val("-file") {
        Some(f) => {
            let path = Path::new(f);
            if path.exists() && !a.flag("-force") {
                return Err(format!("report: {f} exists; add -force to overwrite it"));
            }
            std::fs::write(path, &r).map_err(|e| format!("report: {f}: {e}"))?;
        }
        None => print!("{r}"),
    }
    Ok(r.into())
}

/// `get_property_info <name> [-json] [-cone] [-proof_core]`: what the last
/// prove measured for one property; `-cone` adds its cone of influence and
/// `-proof_core` computes its minimal needed-assumption & state core.
pub(super) fn get_property_info(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-json", 0), ("-cone", 0), ("-proof_core", 0), ("-task", 1)],
        &[],
    )?;
    let [name] = a.pos.as_slice() else {
        return Err("get_property_info: name one property".into());
    };
    let task = a.val("-task").map(|t| s.task_name(t)).transpose()?;
    let i = match select_in(s, task.as_deref(), std::slice::from_ref(name))?.as_slice() {
        [i] => *i,
        many => {
            return Err(format!(
                "get_property_info: `{name}` names {} properties; name one",
                many.len()
            ))
        }
    };
    let info = s.property_info_ext(i, a.flag("-cone"), a.flag("-proof_core"));
    if a.flag("-json") {
        return serde_json::to_string_pretty(&info)
            .map(Value::from)
            .map_err(|e| e.to_string());
    }
    Ok(info.text().into())
}

pub(super) fn get_stats(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(a, &[("-task", 1)], &[])?;
    let task = a.val("-task").map(|t| s.task_name(t)).transpose()?;
    let rows: Vec<serde_json::Value> = s
        .props
        .iter()
        .filter(|p| task.as_deref().is_none_or(|t| p.task == t))
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "task": p.task,
                "kind": p.kind.label(),
                "status": p.status.word(),
                "vacuity": p.vacuity.map(|v| v.word()),
                "witness_trace": p.witness_trace,
                "stats": p.stats,
            })
        })
        .collect();
    serde_json::to_string_pretty(&rows)
        .map(Value::from)
        .map_err(|e| e.to_string())
}

/// `visualize -violation|-cover -property P`: the trace of a CEX or cover,
/// as the VCD file prove wrote. It is opened with the program named by
/// INFORMAL_WAVE_VIEWER (`surfer`, `gtkwave`) when that is set; the path is
/// returned either way, for the desktop UI's trace view or any VCD viewer.
pub(super) fn visualize(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-violation", 0), ("-cover", 0), ("-property", 1)],
        &[
            ("-new_window", 0),
            ("-window", 1),
            ("-bg", 0),
            ("-min_length", 0),
            ("-max_length", 1),
            ("-task", 1),
        ],
    )?;
    let names: Vec<String> = a
        .val("-property")
        .map(String::from)
        .into_iter()
        .chain(a.pos.iter().cloned())
        .collect();
    let [name] = names.as_slice() else {
        return Err("visualize: name one property with -property".into());
    };
    if s.dry_run {
        return Ok(().into());
    }
    let i = match select(s, std::slice::from_ref(name))?.as_slice() {
        [i] => *i,
        many => {
            return Err(format!(
                "visualize: `{name}` names {} properties; name one",
                many.len()
            ))
        }
    };
    let p = &s.props[i];
    let want = if a.flag("-cover") { "covered" } else { "cex" };
    if p.status.word() != want {
        return Err(format!(
            "visualize: {} is {}; there is a trace only for a {}",
            p.name,
            p.status.word(),
            if want == "cex" {
                "failing assertion (-violation)"
            } else {
                "covered cover (-cover)"
            }
        ));
    }
    let Some(path) = p.trace.clone() else {
        return Err(format!(
            "visualize: no trace was written for {} (see the prove log)",
            p.name
        ));
    };
    match std::env::var("INFORMAL_WAVE_VIEWER") {
        Ok(viewer) if !viewer.trim().is_empty() => {
            let mut words = viewer.split_whitespace();
            let prog = words.next().unwrap_or_default();
            std::process::Command::new(prog)
                .args(words)
                .arg(&path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| format!("visualize: starting {prog}: {e}"))?;
            note(&format!("visualize: {path} opened in {prog}"));
        }
        _ => note(&format!(
            "visualize: trace in {path} (open it in the InFormal UI or a VCD viewer; set INFORMAL_WAVE_VIEWER to open it from here)"
        )),
    }
    Ok(path.into())
}

pub(super) fn jg_prop(s: &mut Session, a: &[String], kind: Kind) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-name", 1),
            ("-disable", 0),
            ("-enable", 0),
            ("-remove", 0),
            ("-env", 0),
            ("-from_assert", 1),
            ("-bound", 1),
            ("-task", 1),
            ("-regexp", 0),
        ],
        &[("-severity", 1), ("-tag", 1), ("-from_property", 1)],
    )?;
    let task = a.val("-task").map(|t| s.task_name(t)).transpose()?;
    if let Some(pat) = a.val("-from_assert") {
        if kind != Kind::Assume {
            return Err(format!("{}: -from_assert is only valid on `assume`", a.cmd));
        }
        let target_task = task.unwrap_or_else(|| s.current_task.clone());
        if s.dry_run {
            return Ok(().into());
        }
        let found = if a.flag("-regexp") {
            select_regexp(s, &[pat.to_string()])?
        } else {
            select_in(s, None, &[pat.to_string()])?
        };
        let created =
            s.assume_from_assert(&target_task, &found, a.val("-name").map(String::from))?;
        if !created.is_empty() {
            s.lp.invalidate_formal_waivers();
        }
        return match created.as_slice() {
            [single] => Ok(single.clone().into()),
            many => Ok(Value::List(many.to_vec())),
        };
    }
    let pick = |s: &Session| {
        let found = if a.flag("-regexp") {
            select_regexp(s, &a.pos)
        } else {
            select_in(s, task.as_deref(), &a.pos)
        }?;
        Ok::<_, String>(match &task {
            Some(t) => found
                .into_iter()
                .filter(|&i| &s.props[i].task == t)
                .collect(),
            None => found,
        })
    };
    if a.flag("-disable") || a.flag("-enable") {
        let en = a.flag("-enable");
        let mut changed_assume = false;
        for i in pick(s)? {
            if s.props[i].kind == Kind::Assume && s.props[i].enabled != en {
                changed_assume = true;
            }
            s.props[i].enabled = en;
        }
        if changed_assume {
            s.lp.invalidate_formal_waivers();
        }
        return Ok(().into());
    }
    if a.flag("-remove") {
        let drop = pick(s)?;
        let removed_assume = drop.iter().any(|&idx| s.props[idx].kind == Kind::Assume);
        let mut i = 0;
        s.props.retain(|_| {
            i += 1;
            !drop.contains(&(i - 1))
        });
        if removed_assume {
            s.lp.invalidate_formal_waivers();
        }
        return Ok(().into());
    }
    let bound = match a.val("-bound") {
        None => None,
        Some(v) => {
            if kind == Kind::Assume {
                return Err("assume: -bound is not supported on assumptions".into());
            }
            Some(v.parse::<u32>().ok().filter(|&n| n >= 1).ok_or_else(|| {
                format!("{}: -bound expects a positive integer, not `{v}`", a.cmd)
            })?)
        }
    };
    if a.flag("-regexp") {
        return Err(format!(
            "{}: -regexp selects properties with -enable, -disable or -remove",
            a.cmd
        ));
    }
    let expr = match a.pos.as_slice() {
        [e] => e.clone(),
        _ => {
            return Err(format!(
                "{}: expected one property expression in braces",
                a.cmd
            ))
        }
    };
    // `assert -env` turns an assertion into an assumption.
    let kind = if a.flag("-env") && kind == Kind::Assert {
        Kind::Assume
    } else {
        kind
    };
    let task = task.unwrap_or_else(|| s.current_task.clone());
    let created = add_props(s, &task, kind, a.val("-name").map(String::from), &expr)?;
    if let (Some(b), Value::Str(ref pname)) = (bound, &created) {
        if let Some(p) = s.props.iter_mut().find(|p| &p.name == pname) {
            p.bound = Some(b);
        }
    }
    Ok(created)
}

pub(super) fn report_fv(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let _ = Args::parse(a, &[("-list", 0), ("-verbose", 0)], &[("-property", 1)])?;
    let r = s.report();
    if !s.dry_run {
        print!("{r}");
    }
    Ok(r.into())
}

pub(super) fn get_props(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(a, &[("-type", 1), ("-status", 1)], &[])?;
    let mut inc = String::new();
    if let Some(t) = a.val("-type") {
        inc += &format!("type {t} ");
    }
    if let Some(st) = a.val("-status") {
        let st = if st == "falsified" { "cex" } else { st };
        inc += &format!("status {st}");
    }
    let mut names = filter_props(s, Some(&inc), None, false, None)?;
    if let Some(p) = a.pos.first() {
        names.retain(|n| glob(p, n));
    }
    Ok(names.into())
}

pub(super) fn vcf_prop(s: &mut Session, a: &[String], kind: Kind) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-expr", 1), ("-name", 1), ("-from_assert", 1)],
        &[("-type", 1), ("-usage", 1)],
    )?;
    if let Some(pat) = a.val("-from_assert") {
        if kind != Kind::Assume {
            return Err(format!("{}: -from_assert is only valid on fvassume", a.cmd));
        }
        if s.dry_run {
            return Ok(().into());
        }
        let found = select_in(s, None, &[pat.to_string()])?;
        let task = s.current_task.clone();
        let created = s.assume_from_assert(
            &task,
            &found,
            a.val("-name")
                .map(String::from)
                .or_else(|| a.pos.first().cloned()),
        )?;
        if !created.is_empty() {
            s.lp.invalidate_formal_waivers();
        }
        return match created.as_slice() {
            [single] => Ok(single.clone().into()),
            many => Ok(Value::List(many.to_vec())),
        };
    }
    let (name, expr) = match (a.val("-expr"), a.pos.as_slice()) {
        (Some(e), [n]) => (Some(n.clone()), e.to_string()),
        (Some(e), []) => (a.val("-name").map(String::from), e.to_string()),
        _ => {
            return Err(format!(
                "{}: usage `{} <name> -expr {{expression}}`",
                a.cmd, a.cmd
            ))
        }
    };
    let task = s.current_task.clone();
    add_props(s, &task, kind, name, &expr)
}
