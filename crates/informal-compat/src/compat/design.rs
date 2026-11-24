use super::{expand_env, note, Args};
use crate::session::{glob, Clock, Session};
use informal_tcl::Value;
use std::path::{Path, PathBuf};

/// Apply VCS / Jasper style compile words: files, `+define+`, `+incdir+`,
/// `-f list`, `-incdir d`, `-define X`.
fn apply_compile_words(
    s: &mut Session,
    words: &[String],
    base: Option<&Path>,
) -> Result<(), String> {
    // Absolute paths, so that files resolve the same from the work directory.
    let resolve = |w: &str| -> PathBuf {
        let p = PathBuf::from(expand_env(w));
        let p = match base {
            Some(b) if p.is_relative() && !p.exists() && b.join(&p).exists() => b.join(p),
            _ => p,
        };
        std::fs::canonicalize(&p).unwrap_or(p)
    };
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if let Some(rest) = w.strip_prefix("+define+") {
            for d in rest.split('+').filter(|d| !d.is_empty()) {
                let (k, v) = d
                    .split_once('=')
                    .map_or((d, None), |(k, v)| (k, Some(v.to_string())));
                s.design.defines.push((k.to_string(), v));
            }
        } else if let Some(rest) = w.strip_prefix("+incdir+") {
            for d in rest.split('+').filter(|d| !d.is_empty()) {
                s.design.incdirs.push(resolve(d));
            }
        } else if w == "-f" || w == "-F" || w == "-file" {
            let f = resolve(words.get(i + 1).ok_or("-f needs a file")?);
            if s.dry_run && !f.exists() {
                i += 2;
                continue;
            }
            let text = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
            let inner: Vec<String> = text
                .lines()
                .map(|l| l.split("//").next().unwrap_or(""))
                .filter(|l| !l.trim_start().starts_with('#'))
                .flat_map(|l| l.split_whitespace().map(String::from).collect::<Vec<_>>())
                .collect();
            apply_compile_words(s, &inner, f.parent())?;
            i += 1;
        } else if w == "-incdir" {
            s.design.incdirs.push(resolve(
                words.get(i + 1).ok_or("-incdir needs a directory")?,
            ));
            i += 1;
        } else if w == "-define" || w == "-D" {
            let d = words.get(i + 1).ok_or("-define needs a name")?;
            let (k, v) = d
                .split_once('=')
                .map_or((d.as_str(), None), |(k, v)| (k, Some(v.to_string())));
            s.design.defines.push((k.to_string(), v));
            i += 1;
        } else if w == "-v" || w == "-y" {
            note(&format!(
                "ignoring library option {w} {}",
                words.get(i + 1).map_or("", |x| x)
            ));
            i += 1;
        } else if w.starts_with('-') || w.starts_with('+') {
            // VCS switches such as -sverilog, -assert svaext, +libext+.v
            if w == "-assert" || w == "-timescale" {
                i += 1;
            }
        } else {
            let p = resolve(w);
            // (Check mode looks at the script, not at the design files.)
            if !p.exists() && !s.dry_run {
                return Err(format!("file not found: {}", p.display()));
            }
            s.design.files.push(p);
        }
        i += 1;
    }
    s.elaborated = false;
    s.compiled = None;
    s.fast_gate = None;
    s.lp.invalidate_design();
    Ok(())
}

pub(super) fn set_lowering(s: &mut Session, a: &[String]) -> Result<Value, String> {
    // informal's own: constructs the front end reports as unsupported
    // unless the user says to leave them out. Nothing is on by default.
    let state = |on: bool| if on { "on" } else { "off" };
    let Some(name) = a.get(1) else {
        return Ok(Value::from(format!(
            "skip_initial_checks {} isunknown_is_false {}",
            state(s.design.skip_initial_checks),
            state(s.design.isunknown_is_false)
        )));
    };
    let option = match name.as_str() {
        "skip_initial_checks" => &mut s.design.skip_initial_checks,
        "isunknown_is_false" => &mut s.design.isunknown_is_false,
        _ => {
            return Err(format!(
            "set_lowering: unknown option {name}; options: skip_initial_checks, isunknown_is_false"
        ))
        }
    };
    match a.get(2).map(String::as_str) {
        Some("on") => *option = true,
        Some("off") => *option = false,
        None => return Ok(Value::from(state(*option))),
        Some(v) => return Err(format!("set_lowering {name}: on or off, not `{v}`")),
    }
    // The design is read again with the new setting at the next
    // elaborate or prove.
    s.elaborated = false;
    s.compiled = None;
    s.fast_gate = None;
    s.lp.invalidate_design();
    Ok(().into())
}

pub(super) fn clear(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(a, &[("-all", 0)], &[])?;
    let _ = a.flag("-all");
    s.clear();
    Ok(().into())
}

pub(super) fn jg_analyze(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let langs = [
        "-sv", "-sv09", "-sv12", "-sv17", "-sv2009", "-sv2012", "-sv2017", "-v2k", "-verilog",
        "-sva", "-v95",
    ];
    let mut words = Vec::new();
    let mut i = 1;
    while i < a.len() {
        let w = a[i].as_str();
        if langs.contains(&w) {
        } else if w.starts_with("-vhdl") {
            return Err("analyze: VHDL is not supported yet".into());
        } else if w == "-bbox_m" {
            let mods = a.get(i + 1).ok_or("analyze: -bbox_m needs a module name")?;
            for m in mods.split_whitespace() {
                if !s.design.analyze_bbox.iter().any(|x| x == m) {
                    s.design.analyze_bbox.push(m.to_string());
                }
                if !s.design.bbox.iter().any(|x| x == m) {
                    s.design.bbox.push(m.to_string());
                }
            }
            s.compiled = None;
            s.fast_gate = None;
            s.lp.invalidate_design();
            i += 1;
        } else if w == "-lib" || w == "-L" {
            note(&format!(
                "analyze: ignoring {w} {}",
                a.get(i + 1).map_or("", |x| x)
            ));
            i += 1;
        } else if w == "-clear" {
            s.design.files.clear();
            s.design.analyze_bbox.clear();
            s.design.bbox.clear();
            s.compiled = None;
            s.fast_gate = None;
            s.lp.invalidate_design();
        } else {
            words.push(a[i].clone());
        }
        i += 1;
    }
    apply_compile_words(s, &words, None)?;
    Ok(().into())
}

pub(super) fn jg_elaborate(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-top", 1),
            ("-parameter", 2),
            ("-param", 2),
            ("-bbox_m", 1),
            ("-bbox_i", 1),
            ("-bbox_mul", 1),
            ("-bbox_a", 1),
            ("-disable_auto_bbox", 0),
            ("-create_related_covers", 1),
            ("-no_preconditions", 0),
        ],
        &[
            ("-loop_limit", 1),
            ("-multiple_tops", 0),
            ("-extract_covergroup", 0),
            ("-x_value", 1),
            ("-auto_hr_info", 0),
        ],
    )?;
    for p in a.all("-parameter").chain(a.all("-param")) {
        s.design.params.push((p[0].clone(), p[1].clone()));
    }
    // The black boxes of this elaboration (each option takes a name or
    // a list, and may repeat), combined with any `analyze -bbox_m`.
    let list = |o: &str| -> Vec<String> {
        a.all(o)
            .flat_map(|v| {
                v[0].split_whitespace()
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    // Precondition covers are made for every proven assertion unless
    // this elaboration asks for none; witness covers run when asked.
    if a.flag("-no_preconditions") {
        s.preconditions = false;
    }
    for kinds in a.all("-create_related_covers") {
        for k in kinds[0].split_whitespace() {
            match k {
                "precondition" => s.preconditions = true,
                "witness" => s.witnesses = true,
                _ => {
                    return Err(format!(
                        "elaborate -create_related_covers: precondition or witness, not `{k}`"
                    ))
                }
            }
        }
    }
    if a.flag("-disable_auto_bbox") {
        s.design.bbox_mul = None;
        s.design.bbox_a = None;
    }
    if let Some(v) = a.val("-bbox_mul") {
        let n = v.parse::<u32>().ok().filter(|&n| n >= 1).ok_or_else(|| {
            format!("elaborate -bbox_mul: positive bit width expected, not `{v}`")
        })?;
        s.design.bbox_mul = Some(n);
    }
    if let Some(v) = a.val("-bbox_a") {
        let n =
            v.parse::<usize>().ok().filter(|&n| n >= 1).ok_or_else(|| {
                format!("elaborate -bbox_a: positive word count expected, not `{v}`")
            })?;
        s.design.bbox_a = Some(n);
    }
    let mut bbox = s.design.analyze_bbox.clone();
    for m in list("-bbox_m") {
        if !bbox.contains(&m) {
            bbox.push(m);
        }
    }
    s.design.bbox = bbox;
    s.design.bbox_instances = list("-bbox_i");
    s.elaborate(a.val("-top").map(String::from))
        .map_err(|e| format!("{e:#}"))?;
    Ok(s.design.top.clone().unwrap_or_default().into())
}

pub(super) fn clock_cmd(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-both_edges", 0),
            ("-none", 0),
            ("-clear", 0),
            ("-factor", 1),
            ("-phase", 1),
        ],
        &[("-infer", 0)],
    )?;
    if a.flag("-none") || a.flag("-clear") {
        s.clocks.clear();
        return Ok(().into());
    }
    if a.flag("-both_edges") {
        note("clock: -both_edges is not needed; a design that uses both edges is modelled with two steps per cycle");
    }
    let number = |o: &str, least: u32| -> Result<Option<u32>, String> {
        match a.val(o) {
            None => Ok(None),
            Some(v) => match v.parse::<u32>() {
                Ok(n) if n >= least && n <= 4096 => Ok(Some(n)),
                _ => Err(format!(
                    "clock: {o} takes a whole number from {least} to 4096, not `{v}`"
                )),
            },
        }
    };
    let factor = number("-factor", 1)?.unwrap_or(1);
    let phase = number("-phase", 0)?.unwrap_or(0);
    for sig in &a.pos {
        // Declaring a clock again replaces its ratio.
        s.clocks.retain(|c| &c.signal != sig);
        s.clocks.push(Clock {
            signal: sig.clone(),
            edge: "posedge",
            factor,
            phase,
        });
    }
    if let Err(e) = s.check_reset_and_clocks() {
        s.clocks.truncate(s.clocks.len() - a.pos.len());
        return Err(e);
    }
    if s.clocks.len() > 1 {
        note("clock: several clocks; when the design uses more than one, the model drives them with one step per half period of a -factor 1 clock");
    }
    Ok(().into())
}

pub(super) fn reset(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-expression", 1), ("-expr", 1), ("-none", 0)],
        &[("-non_resettable_regs", 1), ("-analyze_only", 0)],
    )?;
    let old = s.reset.clone();
    s.reset = if a.flag("-none") {
        None
    } else if let Some(e) = a.val("-expression").or(a.val("-expr")) {
        Some(e.to_string())
    } else if !a.pos.is_empty() {
        Some(a.pos.join(" "))
    } else {
        return Ok(s.reset.clone().unwrap_or_default().into());
    };
    if let Err(e) = s.check_reset_and_clocks() {
        s.reset = old;
        return Err(e);
    }
    s.lp.invalidate_formal_waivers();
    Ok(().into())
}

/// `get_design_info [-json] [-list instance|memory|clock|reset [-filter P
/// [-regexp]]]`: the design's counts and the current task's model size
/// (stats.rs). The table by default, JSON with `-json`, names with `-list`.
pub(super) fn get_design_info(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-json", 0), ("-list", 1), ("-filter", 1), ("-regexp", 0)],
        &[("-verbosity", 1), ("-silent", 0)],
    )?;
    if s.dry_run {
        return Ok(().into());
    }
    if let Some(what) = a.val("-list") {
        let d = s.elab.as_ref().ok_or("get_design_info: elaborate first")?;
        let stats = d.stats.clone().unwrap_or_default();
        let names: Vec<String> = match what {
            "instance" | "instances" => d.instances.iter().map(|i| i.path.clone()).collect(),
            "memory" | "memories" => stats.memories.iter().map(|m| m.name.clone()).collect(),
            "clock" | "clocks" => stats.clocks.iter().map(|c| c.signal.clone()).collect(),
            "reset" | "resets" => stats.resets.iter().map(|r| r.signal.clone()).collect(),
            other => {
                return Err(format!(
                "get_design_info -list {other}: not supported yet (instance, memory, clock, reset)"
            ))
            }
        };
        let keep = |n: &str| -> Result<bool, String> {
            match (a.val("-filter"), a.flag("-regexp")) {
                (None, _) => Ok(true),
                (Some(f), true) => regex::Regex::new(f)
                    .map(|re| re.is_match(n))
                    .map_err(|e| format!("get_design_info: bad regular expression `{f}`: {e}")),
                (Some(f), false) => Ok(glob(f, n)),
            }
        };
        let mut out = Vec::new();
        for n in names {
            if keep(&n)? {
                out.push(n);
            }
        }
        return Ok(Value::List(out));
    }
    let info = s.design_info();
    if a.flag("-json") {
        return serde_json::to_string_pretty(&info)
            .map(Value::from)
            .map_err(|e| e.to_string());
    }
    let text = info.text();
    print!("{text}");
    Ok(text.into())
}

// Before `elaborate` / `read_file`: the black boxes of the design.
pub(super) fn set_blackbox(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(a, &[("-designs", 1), ("-cells", 1), ("-clear", 0)], &[])?;
    if a.flag("-clear") {
        s.design.bbox.clear();
        s.design.bbox_instances.clear();
    }
    let list = |o: &str| -> Vec<String> {
        a.all(o)
            .flat_map(|v| {
                v[0].split_whitespace()
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    s.design.bbox.extend(list("-designs"));
    s.design.bbox_instances.extend(list("-cells"));
    s.compiled = None;
    s.fast_gate = None;
    s.lp.invalidate_design();
    if s.elaborated {
        note("set_blackbox: takes effect at the next elaborate / read_file");
    }
    Ok(().into())
}

pub(super) fn set_fml_appmode(_: &mut Session, a: &[String]) -> Result<Value, String> {
    match a.get(1).map(String::as_str) {
        Some("FPV") | Some("fpv") => Ok(().into()),
        Some(m) => Err(format!(
            "set_fml_appmode: only FPV is supported yet (got {m})"
        )),
        None => Ok("FPV".into()),
    }
}

pub(super) fn vcf_read_file(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-top", 1),
            ("-format", 1),
            ("-sva", 0),
            ("-vcs", 1),
            ("-define", 1),
            ("-parameters", 1),
        ],
        &[("-cov", 1), ("-inject_fault", 1), ("-work", 1)],
    )?;
    vcf_format(&a)?;
    vcf_compile(s, &a)?;
    vcf_parameters(s, &a)?;
    s.elaborate(a.val("-top").map(String::from))
        .map_err(|e| format!("{e:#}"))?;
    Ok(s.design.top.clone().unwrap_or_default().into())
}

pub(super) fn vcf_analyze(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-format", 1), ("-vcs", 1), ("-define", 1)],
        &[("-library", 1), ("-work", 1)],
    )?;
    vcf_format(&a)?;
    vcf_compile(s, &a)?;
    Ok(().into())
}

pub(super) fn vcf_elaborate(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-sva", 0), ("-vcs", 1), ("-parameters", 1)],
        &[("-cov", 1), ("-work", 1), ("-library", 1)],
    )?;
    vcf_parameters(s, &a)?;
    s.elaborate(a.pos.first().cloned())
        .map_err(|e| format!("{e:#}"))?;
    Ok(s.design.top.clone().unwrap_or_default().into())
}

pub(super) fn create_clock(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(a, &[("-period", 1), ("-initial", 1)], &[("-name", 1)])?;
    let sig = a.pos.first().ok_or("create_clock: clock signal required")?;
    s.clocks.push(Clock {
        signal: sig.clone(),
        edge: "posedge",
        factor: 1,
        phase: 0,
    });
    Ok(().into())
}

pub(super) fn create_reset(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-sense", 1), ("-low", 0), ("-high", 0)],
        &[("-name", 1)],
    )?;
    let sig = a.pos.first().ok_or("create_reset: reset signal required")?;
    let low = a.flag("-low") || a.val("-sense") == Some("low");
    s.reset = Some(if low { format!("!{sig}") } else { sig.clone() });
    s.lp.invalidate_formal_waivers();
    Ok(().into())
}

fn vcf_format(a: &Args) -> Result<(), String> {
    match a.val("-format") {
        None | Some("sverilog") | Some("verilog") => Ok(()),
        Some(f) => Err(format!("{}: format {f} is not supported yet", a.cmd)),
    }
}

fn vcf_compile(s: &mut Session, a: &Args) -> Result<(), String> {
    let mut words: Vec<String> = a
        .pos
        .iter()
        .flat_map(|w| w.split_whitespace().map(String::from))
        .collect();
    if let Some(v) = a.val("-vcs") {
        words.extend(v.split_whitespace().map(String::from));
    }
    for d in a.all("-define") {
        for w in d[0].split_whitespace() {
            words.push(format!("+define+{w}"));
        }
    }
    apply_compile_words(s, &words, None)
}

fn vcf_parameters(s: &mut Session, a: &Args) -> Result<(), String> {
    if let Some(v) = a.val("-vcs") {
        for w in v.split_whitespace() {
            if let Some(p) = w.strip_prefix("-pvalue+") {
                if let Some((k, val)) = p.split_once('=') {
                    s.design.params.push((k.to_string(), val.to_string()));
                }
            }
        }
    }
    for spec in a.all("-parameters") {
        for item in spec[0]
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|w| !w.is_empty())
        {
            let (k, val) = item.split_once('=').ok_or_else(|| {
                format!("{}: -parameters expects NAME=VALUE, got `{item}`", a.cmd)
            })?;
            s.design.params.push((k.to_string(), val.to_string()));
        }
    }
    Ok(())
}
