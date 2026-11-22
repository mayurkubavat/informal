use super::super::props::prove_and_report;
use super::super::{expand_env, Args};
use crate::session::{parse_duration, Kind, Session};
use informal_tcl::Value;
use std::path::{Path, PathBuf};

pub(in crate::compat) fn check_sec(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-setup", 0),
            ("-spec", 1),
            ("-spec_files", 1),
            ("-spec_top", 1),
            ("-spec_define", 1),
            ("-spec_incdir", 1),
            ("-spec_param", 2),
            ("-imp", 1),
            ("-imp_files", 1),
            ("-imp_top", 1),
            ("-imp_define", 1),
            ("-imp_incdir", 1),
            ("-imp_param", 2),
            ("-clock", 1),
            ("-reset", 1),
            ("-lec", 0),
            ("-latency", 1),
            ("-assume", 1),
            ("-map", 0),
            ("-input", 2),
            ("-output", 2),
            ("-state", 2),
            ("-output_latency", 2),
            ("-prove", 0),
            ("-name", 1),
            ("-time_limit", 1),
            ("-list", 0),
            ("-report", 0),
            ("-file", 1),
            ("-force", 0),
            ("-json", 0),
            ("-clear", 0),
        ],
        &[("-auto", 0), ("-task", 1)],
    )?;
    if a.flag("-clear") {
        s.sec = crate::sec::SecState::default();
        return Ok(().into());
    }
    let parse_defines = |opt: &str| -> Vec<(String, Option<String>)> {
        let mut out = Vec::new();
        for d in a.all(opt) {
            for item in d[0].split_whitespace() {
                match item.split_once('=') {
                    Some((k, v)) => out.push((k.to_string(), Some(v.to_string()))),
                    None => out.push((item.to_string(), None)),
                }
            }
        }
        out
    };
    let parse_files = |o1: &str, o2: &str| -> Vec<PathBuf> {
        a.all(o1)
            .chain(a.all(o2))
            .flat_map(|v| {
                v[0].split_whitespace()
                    .map(|p| std::path::absolute(p).unwrap_or_else(|_| PathBuf::from(p)))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    if a.flag("-setup") {
        let latency: u32 = match a.val("-latency") {
            Some(v) => v
                .parse()
                .map_err(|_| format!("check_sec -setup: bad -latency `{v}`"))?,
            None => 0,
        };
        let setup = crate::sec::SecSetup {
            spec_files: parse_files("-spec", "-spec_files"),
            spec_top: a.val("-spec_top").map(String::from),
            spec_defines: parse_defines("-spec_define"),
            spec_incdirs: a
                .all("-spec_incdir")
                .flat_map(|v| {
                    v[0].split_whitespace()
                        .map(PathBuf::from)
                        .collect::<Vec<_>>()
                })
                .collect(),
            spec_params: a
                .all("-spec_param")
                .map(|p| (p[0].clone(), p[1].clone()))
                .collect(),
            imp_files: parse_files("-imp", "-imp_files"),
            imp_top: a.val("-imp_top").map(String::from),
            imp_defines: parse_defines("-imp_define"),
            imp_incdirs: a
                .all("-imp_incdir")
                .flat_map(|v| {
                    v[0].split_whitespace()
                        .map(PathBuf::from)
                        .collect::<Vec<_>>()
                })
                .collect(),
            imp_params: a
                .all("-imp_param")
                .map(|p| (p[0].clone(), p[1].clone()))
                .collect(),
            clock: a.val("-clock").map(String::from),
            reset: a.val("-reset").map(String::from),
            lec: a.flag("-lec"),
            latency,
            input_map: a
                .all("-input")
                .map(|p| (p[0].clone(), p[1].clone()))
                .collect(),
            output_map: a
                .all("-output")
                .map(|p| (p[0].clone(), p[1].clone()))
                .collect(),
            state_map: a
                .all("-state")
                .map(|p| (p[0].clone(), p[1].clone()))
                .collect(),
            output_latency: std::collections::HashMap::new(),
            assumptions: a.all("-assume").map(|v| v[0].clone()).collect(),
        };
        let (st, it) = crate::sec::setup_sec(s, setup)?;
        if !a.flag("-prove") && !a.flag("-report") && !a.flag("-list") {
            return Ok(format!("{st} {it}").into());
        }
    }
    if a.flag("-map") {
        let setup = s
            .sec
            .setup
            .as_mut()
            .ok_or("check_sec -map: run `check_sec -setup ...` first")?;
        for p in a.all("-input") {
            setup.input_map.push((p[0].clone(), p[1].clone()));
        }
        for p in a.all("-output") {
            setup.output_map.push((p[0].clone(), p[1].clone()));
        }
        for p in a.all("-state") {
            setup.state_map.push((p[0].clone(), p[1].clone()));
        }
        for p in a.all("-output_latency") {
            let lat: u32 = p[1]
                .parse()
                .map_err(|_| format!("check_sec -map -output_latency: bad latency `{}`", p[1]))?;
            setup.output_latency.insert(p[0].clone(), lat);
        }
        if !a.flag("-prove") && !a.flag("-report") && !a.flag("-list") {
            return Ok(().into());
        }
    }
    if a.flag("-prove") {
        let timeout = a.val("-time_limit").map(parse_duration).transpose()?;
        if !s.dry_run {
            crate::sec::prove_sec(s, a.val("-name"), timeout)?;
        }
        if !a.flag("-report") && !a.flag("-list") {
            return Ok(().into());
        }
    }
    if a.flag("-list") {
        let names: Vec<String> = s.sec.checks.iter().map(|c| c.name.clone()).collect();
        return Ok(Value::List(names));
    }
    if a.flag("-report") {
        if s.dry_run {
            return Ok(().into());
        }
        if a.flag("-json") {
            let js = serde_json::to_string_pretty(&s.sec).map_err(|e| e.to_string())?;
            return Ok(js.into());
        }
        let text = crate::sec::format_report(&s.sec);
        if let Some(f) = a.val("-file") {
            let path = Path::new(f);
            if path.exists() && !a.flag("-force") {
                return Err(format!("check_sec: {f} exists; add -force to overwrite it"));
            }
            std::fs::write(path, &text).map_err(|e| format!("check_sec: {f}: {e}"))?;
        } else {
            print!("{text}");
        }
        return Ok(text.into());
    }
    Err("check_sec: expected -setup, -map, -prove, -list, -report or -clear".into())
}

pub(in crate::compat) fn check_csr(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-load", 1),
            ("-task", 1),
            ("-addr", 1),
            ("-wdata", 1),
            ("-rdata", 1),
            ("-we", 1),
            ("-re", 1),
            ("-prefix", 1),
            ("-read_latency", 1),
            ("-prove", 0),
            ("-time_limit", 1),
            ("-list", 0),
            ("-report", 0),
            ("-json", 0),
            ("-file", 1),
            ("-force", 0),
            ("-clear", 0),
        ],
        &[("-bg", 0)],
    )?;
    let task_name = a.val("-task").unwrap_or(crate::csr::DEFAULT_CSR_TASK);
    if a.flag("-clear") {
        if s.tasks.iter().any(|t| t.name == task_name) {
            s.props
                .retain(|p| !(p.task == task_name && !p.rtl && p.copy_of.is_none()));
        }
        return Ok(().into());
    }
    let read_latency: u32 = match a.val("-read_latency") {
        Some(v) => v
            .parse()
            .map_err(|_| format!("check_csr: bad -read_latency `{v}`"))?,
        None => 0,
    };
    let bus = crate::csr::CsrBusConfig {
        addr: a.val("-addr").map(String::from),
        wdata: a.val("-wdata").map(String::from),
        rdata: a.val("-rdata").map(String::from),
        we: a.val("-we").map(String::from),
        re: a.val("-re").map(String::from),
        prefix: a.val("-prefix").unwrap_or_default().to_string(),
        read_latency,
    };
    for files in a.all("-load") {
        let p = PathBuf::from(expand_env(&files[0]));
        let resolved = match informal_tcl::current_script()
            .and_then(|sc| Path::new(&sc).parent().map(Path::to_path_buf))
        {
            Some(base) if p.is_relative() && !p.exists() && base.join(&p).exists() => base.join(&p),
            _ => p,
        };
        if s.dry_run {
            continue;
        }
        crate::csr::load_csr_into_session(s, &resolved, task_name, &bus)?;
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
        if !which.is_empty() {
            let _ = prove_and_report(s, &which)?;
        }
        if !a.flag("-report") && !a.flag("-json") {
            return Ok(().into());
        }
    }
    if a.flag("-report") || a.flag("-json") {
        if s.dry_run {
            return Ok(().into());
        }
        let t = s.task_name(task_name)?;
        let text = if a.flag("-json") {
            let checks: Vec<serde_json::Value> = s
                .props
                .iter()
                .filter(|p| p.task == t && p.kind == Kind::Assert && !p.rtl)
                .map(|p| {
                    serde_json::json!({
                        "name": p.name,
                        "expr": p.expr,
                        "status": p.status.word(),
                    })
                })
                .collect();
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": "informal.csr/1",
                "task": t,
                "checks": checks,
            }))
            .map_err(|e| format!("check_csr: {e}"))?
        } else {
            crate::csr::format_csr_report(s, &t)
        };
        if let Some(f) = a.val("-file") {
            let path = Path::new(f);
            if path.exists() && !a.flag("-force") {
                return Err(format!("check_csr: {f} exists; add -force to overwrite it"));
            }
            std::fs::write(path, &text).map_err(|e| format!("check_csr: {f}: {e}"))?;
        } else if !a.flag("-json") {
            print!("{text}");
        }
        return Ok(text.into());
    }
    Ok(().into())
}

pub(in crate::compat) fn check_protocol(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-create", 0),
            ("-protocol", 1),
            ("-role", 1),
            ("-name", 1),
            ("-prefix", 1),
            ("-map", 1),
            ("-task", 1),
            ("-prove", 0),
            ("-time_limit", 1),
            ("-list", 0),
            ("-report", 0),
            ("-json", 0),
            ("-file", 1),
            ("-force", 0),
            ("-clear", 0),
        ],
        &[("-bg", 0)],
    )?;
    let task_name = a
        .val("-task")
        .unwrap_or(crate::protocol::DEFAULT_PROTOCOL_TASK);
    if a.flag("-clear") {
        if s.tasks.iter().any(|t| t.name == task_name) {
            s.props
                .retain(|p| !(p.task == task_name && !p.rtl && p.copy_of.is_none()));
        }
        return Ok(().into());
    }
    if a.flag("-create") {
        let proto_raw = a
            .val("-protocol")
            .ok_or("check_protocol -create: -protocol <axi4lite|tlul|apb> required")?;
        let protocol = crate::protocol::BusProtocol::parse(proto_raw)?;
        let role = match a.val("-role") {
            Some(r) => crate::protocol::BusRole::parse(r)?,
            None => crate::protocol::BusRole::Slave,
        };
        let name = a
            .val("-name")
            .map(String::from)
            .unwrap_or_else(|| protocol.as_str().to_string());
        let mut map = std::collections::HashMap::new();
        for m in a.all("-map") {
            for pair in m[0].split_whitespace() {
                let (k, v) = pair.split_once('=').ok_or_else(|| {
                    format!("check_protocol -map: expected KEY=SIG, got `{pair}`")
                })?;
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        if !s.dry_run {
            let spec = crate::protocol::ProtocolSpec {
                name,
                protocol,
                role,
                prefix: a.val("-prefix").unwrap_or_default().to_string(),
                map,
                task: task_name.to_string(),
            };
            let created = crate::protocol::create_protocol_vip(s, &spec)?;
            if !a.flag("-prove") && !a.flag("-report") && !a.flag("-json") && !a.flag("-list") {
                return Ok(Value::List(created));
            }
        } else if !a.flag("-prove") && !a.flag("-report") && !a.flag("-json") && !a.flag("-list") {
            return Ok(().into());
        }
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
        if !which.is_empty() {
            let _ = prove_and_report(s, &which)?;
        }
        if !a.flag("-report") && !a.flag("-json") && !a.flag("-list") {
            return Ok(().into());
        }
    }
    if a.flag("-list") {
        if s.dry_run {
            return Ok(Value::List(Vec::new()));
        }
        let names: Vec<String> = s
            .props
            .iter()
            .filter(|p| p.task == task_name && !p.rtl)
            .map(|p| p.name.clone())
            .collect();
        return Ok(Value::List(names));
    }
    if a.flag("-report") || a.flag("-json") {
        if s.dry_run {
            return Ok(().into());
        }
        let t = s.task_name(task_name)?;
        let text = if a.flag("-json") {
            let properties: Vec<serde_json::Value> = s
                .props
                .iter()
                .filter(|p| p.task == t && !p.rtl)
                .map(|p| {
                    serde_json::json!({
                        "name": p.name,
                        "kind": p.kind.label(),
                        "expr": p.expr,
                        "status": p.status.word(),
                    })
                })
                .collect();
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": "informal.protocol/1",
                "task": t,
                "properties": properties,
            }))
            .map_err(|e| format!("check_protocol: {e}"))?
        } else {
            crate::protocol::format_protocol_report(s, &t)
        };
        if let Some(f) = a.val("-file") {
            let path = Path::new(f);
            if path.exists() && !a.flag("-force") {
                return Err(format!(
                    "check_protocol: {f} exists; add -force to overwrite it"
                ));
            }
            std::fs::write(path, &text).map_err(|e| format!("check_protocol: {f}: {e}"))?;
        } else if !a.flag("-json") {
            print!("{text}");
        }
        return Ok(text.into());
    }
    Err("check_protocol: expected -create, -prove, -list, -report or -clear".into())
}
