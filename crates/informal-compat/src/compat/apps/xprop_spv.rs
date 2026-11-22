use super::super::Args;
use crate::session::{parse_duration, Session};
use informal_tcl::Value;
use std::path::Path;

pub(in crate::compat) fn check_xprop(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-init", 0),
            ("-targets", 1),
            ("-type", 1),
            ("-x_source", 1),
            ("-to", 1),
            ("-waiver", 0),
            ("-add", 0),
            ("-source", 0),
            ("-instance", 1),
            ("-comment", 1),
            ("-clear", 0),
            ("-list", 0),
            ("-prove", 0),
            ("-report", 0),
            ("-task", 1),
            ("-time_limit", 1),
            ("-file", 1),
            ("-force", 0),
        ],
        &[("-bg", 0), ("-silent", 0)],
    )?;
    if a.flag("-clear") && !a.flag("-waiver") {
        s.xprop = crate::xprop::XpropState::default();
        return Ok(().into());
    }
    if a.flag("-waiver") {
        if a.flag("-clear") {
            s.xprop.waivers.clear();
            return Ok(().into());
        }
        if a.flag("-list") && !a.flag("-add") {
            let list: Vec<String> = s
                .xprop
                .waivers
                .iter()
                .map(|w| format!("{}: {}", w.id, w.pattern))
                .collect();
            return Ok(Value::List(list));
        }
        if a.flag("-add") {
            let pat = a
                .val("-instance")
                .or_else(|| a.pos.first().map(String::as_str))
                .ok_or("check_xprop -waiver -add: -instance <pattern> required")?;
            let id = s.xprop.waivers.last().map_or(1, |w| w.id + 1);
            s.xprop.waivers.push(crate::xprop::XpropWaiver {
                id,
                pattern: pat.to_string(),
                source_only: a.flag("-source"),
                comment: a.val("-comment").map(String::from),
            });
            s.xprop.reapply_sink_waivers();
            return Ok(id.to_string().into());
        }
        return Err("check_xprop -waiver: expected -add, -list or -clear".into());
    }
    if a.flag("-init") {
        let cats: Vec<String> = a
            .all("-targets")
            .chain(a.all("-type"))
            .flat_map(|v| {
                v[0].split(|c: char| c.is_whitespace() || c == ',')
                    .filter(|x| !x.is_empty())
                    .map(|x| x.to_ascii_lowercase())
                    .collect::<Vec<_>>()
            })
            .collect();
        let x_sources: Vec<String> = a
            .all("-x_source")
            .flat_map(|v| {
                v[0].split_whitespace()
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .collect();
        let explicit_targets: Vec<String> = a
            .all("-to")
            .flat_map(|v| {
                v[0].split_whitespace()
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .collect();
        s.xprop = crate::xprop::XpropState {
            initialized: true,
            categories: cats,
            x_sources,
            explicit_targets,
            waivers: Vec::new(),
            items: Vec::new(),
        };
        if !a.flag("-prove") && !a.flag("-report") && !a.flag("-list") {
            return Ok(().into());
        }
    } else {
        for v in a.all("-x_source") {
            for w in v[0].split_whitespace() {
                s.xprop.x_sources.push(w.to_string());
            }
        }
        for v in a.all("-to") {
            for w in v[0].split_whitespace() {
                s.xprop.explicit_targets.push(w.to_string());
            }
        }
    }
    if a.flag("-prove") {
        let timeout = a.val("-time_limit").map(parse_duration).transpose()?;
        if !s.dry_run {
            crate::xprop::prove(s, a.val("-task"), timeout)?;
        }
        if !a.flag("-report") && !a.flag("-list") {
            return Ok(().into());
        }
    }
    if a.flag("-list") {
        let names: Vec<String> = s.xprop.items.iter().map(|i| i.name.clone()).collect();
        return Ok(Value::List(names));
    }
    if a.flag("-report") {
        if s.dry_run {
            return Ok(().into());
        }
        let text = crate::xprop::format_report(&s.xprop);
        if let Some(f) = a.val("-file") {
            let path = Path::new(f);
            if path.exists() && !a.flag("-force") {
                return Err(format!(
                    "check_xprop: {f} exists; add -force to overwrite it"
                ));
            }
            std::fs::write(path, &text).map_err(|e| format!("check_xprop: {f}: {e}"))?;
        } else {
            print!("{text}");
        }
        return Ok(text.into());
    }
    Err("check_xprop: expected -init, -waiver, -prove, -list, -report or -clear".into())
}

pub(in crate::compat) fn check_spv(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-create", 0),
            ("-name", 1),
            ("-from", 1),
            ("-to", 1),
            ("-cond", 1),
            ("-precond", 1),
            ("-task", 1),
            ("-prove", 0),
            ("-report", 0),
            ("-list", 0),
            ("-clear", 0),
            ("-time_limit", 1),
            ("-file", 1),
            ("-force", 0),
        ],
        &[("-bg", 0), ("-silent", 0)],
    )?;
    if a.flag("-clear") {
        s.spv.clear();
        return Ok(().into());
    }
    if a.flag("-create") {
        let from: Vec<String> = a
            .all("-from")
            .flat_map(|v| {
                v[0].split_whitespace()
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .collect();
        let to: Vec<String> = a
            .all("-to")
            .flat_map(|v| {
                v[0].split_whitespace()
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .collect();
        if from.is_empty() || to.is_empty() {
            return Err(
                "check_spv -create: both -from <secret> and -to <observable> are required".into(),
            );
        }
        let task = match a.val("-task") {
            Some(t) => s.task_name(t)?,
            None => s.current_task.clone(),
        };
        let name = match a.val("-name") {
            Some(n) => n.to_string(),
            None => format!("spv_{}", s.spv.len() + 1),
        };
        if s.spv.iter().any(|r| r.name == name) {
            return Err(format!("check_spv: rule `{name}` already exists"));
        }
        let cond = a
            .val("-cond")
            .or_else(|| a.val("-precond"))
            .map(String::from);
        s.spv.push(crate::spv::SpvRule {
            name: name.clone(),
            task,
            from,
            to,
            cond,
            status: crate::spv::SpvStatus::Unprocessed,
            engine: None,
            ms: 0,
            trace: None,
        });
        return Ok(name.into());
    }
    if a.flag("-prove") {
        let timeout = a.val("-time_limit").map(parse_duration).transpose()?;
        if !s.dry_run {
            crate::spv::prove_spv(s, a.val("-name"), timeout)?;
        }
        if !a.flag("-report") && !a.flag("-list") {
            return Ok(().into());
        }
    }
    if a.flag("-list") {
        let names: Vec<String> = s.spv.iter().map(|r| r.name.clone()).collect();
        return Ok(Value::List(names));
    }
    if a.flag("-report") {
        if s.dry_run {
            return Ok(().into());
        }
        let text = crate::spv::format_report(&s.spv);
        if let Some(f) = a.val("-file") {
            let path = Path::new(f);
            if path.exists() && !a.flag("-force") {
                return Err(format!("check_spv: {f} exists; add -force to overwrite it"));
            }
            std::fs::write(path, &text).map_err(|e| format!("check_spv: {f}: {e}"))?;
        } else {
            print!("{text}");
        }
        return Ok(text.into());
    }
    Err("check_spv: expected -create, -prove, -list, -report or -clear".into())
}

pub(in crate::compat) fn why_cex(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-property", 1), ("-max_events", 1), ("-json", 0)],
        &[],
    )?;
    let prop = a
        .val("-property")
        .or_else(|| a.pos.first().map(String::as_str))
        .ok_or("why_cex: property name required (why_cex [-max_events N] [-json] <property>)")?;
    let max_events: usize = match a.val("-max_events") {
        Some(v) => v
            .parse()
            .map_err(|_| format!("why_cex: bad -max_events '{v}'"))?,
        None => 64,
    };
    if s.dry_run {
        return Ok(().into());
    }
    let rep = crate::why_cex::slice_property_cex(s, prop, max_events)?;
    if a.flag("-json") {
        let js = serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?;
        return Ok(js.into());
    }
    let text = crate::why_cex::format_text(&rep);
    print!("{text}");
    Ok(text.into())
}
