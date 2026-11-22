use super::super::props::prove_and_report;
use super::super::{expand_env, Args};
use crate::session::{parse_duration, Kind, Session};
use informal_tcl::Value;
use std::path::{Path, PathBuf};

pub(in crate::compat) fn check_conn(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-init", 0),
            ("-load", 1),
            ("-prove", 0),
            ("-report", 0),
            ("-list", 0),
            ("-clear", 0),
            ("-task", 1),
            ("-time_limit", 1),
            ("-file", 1),
            ("-force", 0),
        ],
        &[
            ("-generate_candidates", 0),
            ("-validate", 0),
            ("-bg", 0),
            ("-silent", 0),
        ],
    )?;
    let task_name = a.val("-task").unwrap_or(crate::conn::DEFAULT_CONN_TASK);
    if a.flag("-clear") {
        if s.tasks.iter().any(|t| t.name == task_name) {
            s.props
                .retain(|p| !(p.task == task_name && p.kind == Kind::Assert));
        }
        return Ok(().into());
    }
    if a.flag("-init") {
        crate::conn::ensure_conn_task(s, task_name)?;
    }
    for files in a.all("-load") {
        let p = PathBuf::from(expand_env(&files[0]));
        let resolved = match informal_tcl::current_script()
            .and_then(|sc| Path::new(&sc).parent().map(Path::to_path_buf))
        {
            Some(base) if p.is_relative() && !p.exists() && base.join(&p).exists() => base.join(&p),
            _ => p,
        };
        if s.dry_run {
            crate::conn::ensure_conn_task(s, task_name)?;
            continue;
        }
        crate::conn::load_csv_into_session(s, &resolved, task_name)?;
    }
    if a.flag("-list") {
        if s.dry_run {
            return Ok(Value::List(Vec::new()));
        }
        let names: Vec<String> = s
            .props
            .iter()
            .filter(|p| p.task == task_name && p.kind == Kind::Assert)
            .map(|p| p.name.clone())
            .collect();
        return Ok(Value::List(names));
    }
    if a.flag("-prove") {
        if let Some(tl) = a.val("-time_limit") {
            s.time_limit = parse_duration(tl)?;
        }
        if s.dry_run {
            return Ok(().into());
        }
        let t = s.task_name(task_name)?;
        let which: Vec<usize> = s
            .props_of(&t)
            .into_iter()
            .filter(|&i| s.props[i].enabled && s.props[i].kind != Kind::Assume)
            .collect();
        if which.is_empty() {
            return Ok(().into());
        }
        let _ = prove_and_report(s, &which)?;
    }
    if a.flag("-report") {
        if s.dry_run {
            return Ok(().into());
        }
        let t = s.task_name(task_name)?;
        let text = s.report_of(Some(&t));
        if let Some(f) = a.val("-file") {
            let path = Path::new(f);
            if path.exists() && !a.flag("-force") {
                return Err(format!(
                    "check_conn: {f} exists; add -force to overwrite it"
                ));
            }
            std::fs::write(path, &text).map_err(|e| format!("check_conn: {f}: {e}"))?;
        } else {
            print!("{text}");
        }
        return Ok(text.into());
    }
    Ok(().into())
}

pub(in crate::compat) fn check_cov(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-init", 0),
            ("-model", 1),
            ("-type", 1),
            ("-enable_prove_based_proof_core", 0),
            ("-waiver", 0),
            ("-add", 0),
            ("-clear", 0),
            ("-remove", 1),
            ("-comment", 1),
            ("-instance", 1),
            ("-source_file", 1),
            ("-start_line", 1),
            ("-end_line", 1),
            ("-expression", 1),
            ("-cover_item_id", 1),
            ("-measure", 0),
            ("-list", 0),
            ("-cid", 0),
            ("-report", 0),
            ("-task", 1),
            ("-tasks", 1),
            ("-exclude", 1),
            ("-report_file", 1),
            ("-html", 0),
            ("-no_return", 0),
            ("-force", 0),
            ("-time_limit", 1),
        ],
        &[
            ("-exclude_bind_hierarchies", 0),
            ("-bg", 0),
            ("-silent", 0),
            ("-view", 0),
        ],
    )?;

    let mut parsed_models: Vec<crate::cov::CovModel> = Vec::new();
    for m in a.all("-model").chain(a.all("-type")) {
        for cm in crate::cov::CovModel::parse_list(&m[0])? {
            if !parsed_models.contains(&cm) {
                parsed_models.push(cm);
            }
        }
    }

    if a.flag("-init") {
        s.cov = crate::cov::CovState {
            initialized: true,
            models: parsed_models,
            prove_based: a.flag("-enable_prove_based_proof_core"),
            waivers: Vec::new(),
            items: Vec::new(),
            measured: false,
        };
        return Ok(().into());
    }

    if a.flag("-waiver") {
        if a.flag("-clear") {
            s.cov.waivers.clear();
            return Ok(().into());
        }
        if let Some(rem) = a.val("-remove") {
            if let Ok(id) = rem.parse::<usize>() {
                s.cov.waivers.retain(|w| w.id != id);
            }
            return Ok(().into());
        }
        if a.flag("-list") && !a.flag("-add") {
            let list: Vec<String> = s
                .cov
                .waivers
                .iter()
                .map(|w| format!("{}: {}", w.id, w.comment.as_deref().unwrap_or("waiver")))
                .collect();
            return Ok(Value::List(list));
        }
        if a.flag("-add") {
            let parse_line = |opt: &str| -> Result<Option<u32>, String> {
                match a.val(opt) {
                    None => Ok(None),
                    Some(v) => v.parse::<u32>().map(Some).map_err(|_| {
                        format!("check_cov -waiver: {opt} takes a line number, not `{v}`")
                    }),
                }
            };
            let id = s.cov.waivers.last().map_or(1, |w| w.id + 1);
            s.cov.waivers.push(crate::cov::CovWaiver {
                id,
                models: parsed_models,
                instance: a.val("-instance").map(String::from),
                source_file: a.val("-source_file").map(String::from),
                start_line: parse_line("-start_line")?,
                end_line: parse_line("-end_line")?,
                expression: a.val("-expression").map(String::from),
                cover_item_id: a.val("-cover_item_id").map(String::from),
                comment: a.val("-comment").map(String::from),
            });
            s.cov.reapply_waivers();
            return Ok(id.to_string().into());
        }
        return Err("check_cov -waiver: expected -add, -list, -remove or -clear".into());
    }

    let tasks: Vec<String> = a
        .all("-task")
        .chain(a.all("-tasks"))
        .flat_map(|v| {
            v[0].split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .collect();
    let excludes: Vec<String> = a
        .all("-exclude")
        .flat_map(|v| {
            v[0].split(|c: char| c.is_whitespace() || c == ',')
                .filter(|x| !x.is_empty())
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .collect();

    if a.flag("-measure") {
        let timeout = a.val("-time_limit").map(parse_duration).transpose()?;
        if s.dry_run {
            return Ok(().into());
        }
        crate::cov::measure(s, &tasks, &parsed_models, timeout)?;
        return Ok(().into());
    }

    if a.flag("-list") {
        if s.dry_run {
            return Ok(Value::List(Vec::new()));
        }
        if !s.cov.measured && s.elaborated {
            crate::cov::measure(s, &tasks, &parsed_models, None)?;
        }
        let model_filter = (!parsed_models.is_empty()).then_some(parsed_models.as_slice());
        let task_filter = (!tasks.is_empty()).then_some(tasks.as_slice());
        let shown = crate::cov::filter_items(&s.cov, model_filter, task_filter, &excludes);
        let use_cid = a.flag("-cid");
        return Ok(Value::List(
            shown
                .into_iter()
                .map(|it| {
                    if use_cid {
                        it.id.clone()
                    } else {
                        it.name.clone()
                    }
                })
                .collect(),
        ));
    }

    if a.flag("-report") {
        if s.dry_run {
            return Ok(().into());
        }
        if !s.cov.measured && s.elaborated {
            crate::cov::measure(s, &tasks, &parsed_models, None)?;
        }
        let model_filter = (!parsed_models.is_empty()).then_some(parsed_models.as_slice());
        let task_filter = (!tasks.is_empty()).then_some(tasks.as_slice());
        let report = crate::cov::format_report(
            &s.cov,
            model_filter,
            task_filter,
            &excludes,
            a.flag("-html"),
        );
        if let Some(f) = a.val("-report_file") {
            let path = PathBuf::from(expand_env(f));
            if path.exists() && !a.flag("-force") {
                return Err(format!(
                    "check_cov: {} exists; add -force to overwrite it",
                    path.display()
                ));
            }
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("check_cov: {}: {e}", path.display()))?;
            }
            std::fs::write(&path, &report)
                .map_err(|e| format!("check_cov: {}: {e}", path.display()))?;
        } else if !a.flag("-no_return") {
            print!("{report}");
        }
        if a.flag("-no_return") {
            return Ok(().into());
        }
        return Ok(report.into());
    }

    Err("check_cov: expected -init, -waiver, -measure, -list or -report".into())
}
