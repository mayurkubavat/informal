//! InFormal Power Verification Tcl shell commands (`load_upf`, `read_liberty`, `check_lp`,
//! `report_lp`, `set_lp_waiver`, `compare_upf`, `check_lpv`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use informal_tcl::Value;
use informal_upf::{
    compare_upf_with_config, load_cpf, load_upf as upf_load_upf, CompareConfig, UpfVersion,
    DEFAULT_VOLTAGE_TOL_UV,
};

use super::{expand_env, note, Args};
use crate::lp::finder_bridge::DesignFinder;
use crate::lp::report::{self, ReportFilter};
use crate::lp::{CheckLpOpts, LpStage, LpStatus};
use crate::session::{parse_duration, Session};

fn resolve_script_rel_path(raw: &str) -> PathBuf {
    let p = PathBuf::from(expand_env(raw.trim()));
    if p.is_absolute() || p.exists() {
        return p;
    }
    if let Some(base) =
        informal_tcl::current_script().and_then(|sc| Path::new(&sc).parent().map(Path::to_path_buf))
    {
        let joined = base.join(&p);
        if joined.exists() {
            return joined;
        }
    }
    p
}

pub(super) fn load_upf(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-scope", 1), ("-version", 1), ("-format", 1), ("-cpf", 0)],
        &[("-silent", 0)],
    )?;
    if a.pos.len() != 1 {
        return Err("load_upf: expected exactly one <file> path".to_string());
    }
    let is_cpf = match a.val("-format") {
        Some(fmt) => match fmt.trim().to_ascii_lowercase().as_str() {
            "upf" => false,
            "cpf" => true,
            _ => {
                return Err(format!(
                    "load_upf: invalid -format `{fmt}` (expected upf|cpf)"
                ))
            }
        },
        None => a.flag("-cpf"),
    };
    let version = match a.val("-version") {
        Some(v) => {
            let parsed = if is_cpf {
                UpfVersion::parse_cpf(v).or_else(|_| UpfVersion::parse_upf(v))
            } else {
                UpfVersion::parse_upf(v).or_else(|_| UpfVersion::parse_cpf(v))
            };
            Some(parsed.map_err(|_| {
                format!("load_upf: invalid -version `{v}` (expected 1.0|2.0|2.1|3.0|3.1|4.0|cpf1.1|cpf2.0)")
            })?)
        }
        None => None,
    };
    let scope = a.val("-scope").map(str::trim).filter(|sc| !sc.is_empty());
    if s.dry_run {
        return Ok(().into());
    }
    let path = resolve_script_rel_path(&a.pos[0]);
    crate::lp::load_upf(s, &path, scope, version, is_cpf)?;
    Ok(().into())
}

pub(super) fn read_liberty(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(a, &[("-lib", 1)], &[("-silent", 0)])?;
    let mut raw_paths: Vec<String> = Vec::new();
    for p in &a.pos {
        raw_paths.extend(p.split_whitespace().map(String::from));
    }
    for v in a.all("-lib") {
        raw_paths.extend(v[0].split_whitespace().map(String::from));
    }
    if raw_paths.is_empty() {
        return Err("read_liberty: at least one .lib file path is required".to_string());
    }
    if s.dry_run {
        return Ok(().into());
    }
    let paths: Vec<PathBuf> = raw_paths
        .iter()
        .map(|p| resolve_script_rel_path(p))
        .collect();
    crate::lp::read_liberty(s, &paths)?;
    Ok(().into())
}

pub(super) fn check_lp(s: &mut Session, a: &[String]) -> Result<Value, String> {
    check_lp_impl(s, a, LpStage::All)
}

pub(super) fn check_lpv(s: &mut Session, a: &[String]) -> Result<Value, String> {
    check_lp_impl(s, a, LpStage::Formal)
}

fn check_lp_impl(s: &mut Session, a: &[String], default_stage: LpStage) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-stage", 1),
            ("-prove_corruption", 0),
            ("-prove_waivers", 0),
            ("-sec", 0),
            ("-lec", 0),
            ("-timeout", 1),
            ("-time_limit", 1),
            ("-tol_mv", 1),
            ("-clear", 0),
            ("-json", 0),
            ("-report", 0),
            ("-file", 1),
            ("-force", 0),
            ("-aag", 0),
            ("-prove", 0),
        ],
        &[("-bg", 0), ("-silent", 0)],
    )?;
    if !a.pos.is_empty() {
        return Err(format!(
            "{}: unexpected positional argument(s): {}",
            a.cmd,
            a.pos.join(" ")
        ));
    }
    let stage = match a.val("-stage") {
        Some(st) => LpStage::parse(st)?,
        None => default_stage,
    };
    if let Some(dur) = a.val("-timeout").or_else(|| a.val("-time_limit")) {
        s.time_limit = parse_duration(dur)?;
    }
    let tol_uv = match a.val("-tol_mv") {
        Some(mv) => parse_tol_mv_uv(&a.cmd, mv)?,
        None => DEFAULT_VOLTAGE_TOL_UV,
    };
    if s.dry_run {
        return Ok(().into());
    }
    if a.flag("-clear") {
        s.lp.clear_results();
        return Ok(().into());
    }
    let opts = CheckLpOpts {
        stage,
        tol_uv,
        prove_corruption: a.flag("-prove_corruption"),
        prove_waivers: a.flag("-prove_waivers"),
        sec: a.flag("-sec"),
        lec: a.flag("-lec"),
    };
    if opts.prove_corruption
        || opts.prove_waivers
        || opts.sec
        || opts.lec
        || a.flag("-aag")
        || a.flag("-prove")
    {
        note(&format!(
            "{}: formal/LEC options (-prove_corruption/-prove_waivers/-sec/-lec/-aag/-prove) run Stage 1–2 static analysis in WS3 (formal engines arrive in WS4–WS5)",
            a.cmd
        ));
    }
    crate::lp::check_lp(s, &opts)?;

    let text = if a.flag("-json") {
        report::format_json(&s.lp, &ReportFilter::default())?
    } else {
        report::format_summary(&s.lp, &ReportFilter::default())
    };
    if let Some(f) = a.val("-file") {
        let p = Path::new(f);
        if p.exists() && !a.flag("-force") {
            return Err(format!("{}: {f} exists; add -force to overwrite", a.cmd));
        }
        std::fs::write(p, &text).map_err(|e| format!("{}: {f}: {e}", a.cmd))?;
    } else if !a.flag("-json") {
        print!("{text}");
    }
    Ok(text.into())
}

pub(super) fn report_lp(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-summary", 0),
            ("-verbose", 0),
            ("-rule", 1),
            ("-status", 1),
            ("-stage", 1),
            ("-json", 0),
            ("-file", 1),
            ("-force", 0),
        ],
        &[("-silent", 0)],
    )?;
    if !a.pos.is_empty() {
        return Err(format!(
            "report_lp: unexpected positional argument(s): {}",
            a.pos.join(" ")
        ));
    }
    let status = match a.val("-status") {
        Some(st) => Some(LpStatus::parse(st)?),
        None => None,
    };
    let stage_str = match a.val("-stage") {
        Some(stg) => Some(LpStage::parse(stg)?.word().to_string()),
        None => None,
    };
    if s.dry_run {
        return Ok(().into());
    }
    let filter = ReportFilter {
        rule_glob: a.val("-rule"),
        status,
        stage: stage_str.as_deref(),
    };
    let text = if a.flag("-json") {
        report::format_json(&s.lp, &filter)?
    } else if a.flag("-verbose") {
        report::format_verbose(&s.lp, &filter)
    } else {
        report::format_summary(&s.lp, &filter)
    };
    if let Some(f) = a.val("-file") {
        let p = Path::new(f);
        if p.exists() && !a.flag("-force") {
            return Err(format!("report_lp: {f} exists; add -force to overwrite"));
        }
        std::fs::write(p, &text).map_err(|e| format!("report_lp: {f}: {e}"))?;
    } else if !a.flag("-json") {
        print!("{text}");
    }
    Ok(text.into())
}

pub(super) fn set_lp_waiver(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-add", 0),
            ("-list", 0),
            ("-remove", 1),
            ("-clear", 0),
            ("-rule", 1),
            ("-signal", 1),
            ("-src_domain", 1),
            ("-dst_domain", 1),
            ("-comment", 1),
        ],
        &[],
    )?;
    if !a.pos.is_empty() {
        return Err(format!(
            "set_lp_waiver: unexpected positional argument(s): {}",
            a.pos.join(" ")
        ));
    }
    let mode_count = [
        a.flag("-add"),
        a.flag("-list"),
        a.val("-remove").is_some(),
        a.flag("-clear"),
    ]
    .into_iter()
    .filter(|&b| b)
    .count();
    if mode_count != 1 {
        return Err(
            "set_lp_waiver: specify exactly one of -add, -list, -remove <id>, or -clear"
                .to_string(),
        );
    }
    if a.flag("-add") {
        let rule = a
            .val("-rule")
            .ok_or_else(|| "set_lp_waiver -add: -rule <glob> is required".to_string())?
            .to_string();
        let id = s.lp.add_waiver(
            rule,
            a.val("-signal").map(String::from),
            a.val("-src_domain").map(String::from),
            a.val("-dst_domain").map(String::from),
            a.val("-comment").map(String::from),
        );
        return Ok(id.to_string().into());
    }
    if let Some(id_str) = a.val("-remove") {
        let id: usize = id_str
            .parse()
            .map_err(|_| format!("set_lp_waiver -remove: invalid waiver id `{id_str}`"))?;
        if !s.lp.remove_waiver(id) {
            return Err(format!("set_lp_waiver -remove: unknown waiver id `{id}`"));
        }
        return Ok(().into());
    }
    if a.flag("-clear") {
        s.lp.waivers.clear();
        s.lp.reapply_waivers();
        return Ok(().into());
    }
    // `-list`
    let items: Vec<String> =
        s.lp.waivers
            .iter()
            .map(|w| {
                format!(
                    "{}: rule={} signal={} src={} dst={}",
                    w.id,
                    w.rule_glob,
                    w.signal_glob.as_deref().unwrap_or("*"),
                    w.src_domain.as_deref().unwrap_or("*"),
                    w.dst_domain.as_deref().unwrap_or("*"),
                )
            })
            .collect();
    Ok(Value::List(items))
}

pub(super) fn compare_upf(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-golden", 1),
            ("-revised", 1),
            ("-golden_cpf", 0),
            ("-revised_cpf", 0),
            ("-scope", 1),
            ("-tol_uv", 1),
            ("-tol_mv", 1),
            ("-json", 0),
        ],
        &[("-silent", 0)],
    )?;
    if !a.pos.is_empty() {
        return Err(format!(
            "compare_upf: unexpected positional argument(s): {}",
            a.pos.join(" ")
        ));
    }
    let golden_raw = a
        .val("-golden")
        .ok_or_else(|| "compare_upf: -golden <file> is required".to_string())?;
    let revised_raw = a
        .val("-revised")
        .ok_or_else(|| "compare_upf: -revised <file> is required".to_string())?;
    let tol_uv = if let Some(uv) = a.val("-tol_uv") {
        let v: i64 = uv
            .parse()
            .map_err(|_| format!("compare_upf: invalid -tol_uv `{uv}`"))?;
        if v < 0 {
            return Err(format!("compare_upf: invalid -tol_uv `{uv}`"));
        }
        v
    } else if let Some(mv) = a.val("-tol_mv") {
        parse_tol_mv_uv("compare_upf", mv)?
    } else {
        DEFAULT_VOLTAGE_TOL_UV
    };
    if s.dry_run {
        return Ok(().into());
    }
    let golden_path = resolve_script_rel_path(golden_raw);
    let revised_path = resolve_script_rel_path(revised_raw);
    let scope = a.val("-scope").map(str::trim).filter(|sc| !sc.is_empty());
    let finder = Arc::new(DesignFinder::from_compiled(
        s.compiled.as_ref(),
        s.design.top.as_deref(),
    ));

    let g_res = if a.flag("-golden_cpf") {
        load_cpf(&golden_path, scope, None, finder.clone())
    } else {
        upf_load_upf(&golden_path, scope, None, finder.clone())
    }
    .map_err(|e| format!("compare_upf (-golden): {e}"))?;

    let r_res = if a.flag("-revised_cpf") {
        load_cpf(&revised_path, scope, None, finder.clone())
    } else {
        upf_load_upf(&revised_path, scope, None, finder.clone())
    }
    .map_err(|e| format!("compare_upf (-revised): {e}"))?;

    let cfg = CompareConfig { tol_uv };
    let report = compare_upf_with_config(&g_res, &r_res, finder.as_ref(), &cfg);
    if a.flag("-json") {
        let js = serde_json::to_string_pretty(&report).map_err(|e| format!("compare_upf: {e}"))?;
        return Ok(js.into());
    }
    if report.equivalent {
        let msg = "compare_upf: equivalent (0 differences)\n";
        print!("{msg}");
        return Ok(msg.into());
    }
    let mut details = String::new();
    for d in &report.diffs {
        details.push_str(&format!(
            "  - {:?} `{}`: golden={} vs revised={}\n",
            d.kind, d.subject, d.golden_detail, d.revised_detail
        ));
    }
    Err(format!(
        "compare_upf: UPF mismatch ({} difference(s)):\n{details}",
        report.diffs.len()
    ))
}

fn parse_tol_mv_uv(cmd: &str, mv: &str) -> Result<i64, String> {
    let f: f64 = mv
        .parse()
        .map_err(|_| format!("{cmd}: invalid -tol_mv `{mv}`"))?;
    let scaled = f * 1000.0;
    if !f.is_finite() || f < 0.0 || !scaled.is_finite() || scaled > (i64::MAX as f64) {
        return Err(format!("{cmd}: invalid -tol_mv `{mv}`"));
    }
    Ok(scaled.round() as i64)
}
