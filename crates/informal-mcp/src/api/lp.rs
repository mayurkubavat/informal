use super::{
    err, opt_bool, opt_f64, opt_str, opt_u64, req_str, strings, wrong, Api, ApiError, Reply,
};
use crate::{cone, diagnose};
use informal_fpv::lp::{self, CheckLpOpts, LpStage, LpStatus, LpViolation};
use informal_fpv::{Session, Status};
use informal_upf::{DiagSeverity, PowerDomainId, ResolvedPowerIntent, UpfVersion, DEFAULT_VOLTAGE_TOL_UV};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

impl Api {
    pub(super) fn load_upf(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let file = req_str(args, "file")?;
        let path = self.below_root(&file)?;
        let scope = opt_str(args, "scope")?;
        let format = opt_str(args, "format")?;
        let cpf_flag = opt_bool(args, "cpf")?.unwrap_or(false);
        let is_cpf = match format.as_deref() {
            Some("cpf") => true,
            Some("upf") => false,
            Some(other) => return Err(wrong("format", &format!("upf or cpf (got {other})"))),
            None => {
                cpf_flag
                    || path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("cpf"))
            }
        };
        let version = match opt_str(args, "version")? {
            Some(v) => {
                let parsed = if is_cpf {
                    UpfVersion::parse_cpf(&v)
                } else {
                    UpfVersion::parse_upf(&v)
                };
                Some(parsed.map_err(|e| err("arguments", e))?)
            }
            None => None,
        };
        let liberty_raw = strings(args, "liberty")?;
        let mut liberty_paths = Vec::with_capacity(liberty_raw.len());
        for lf in &liberty_raw {
            liberty_paths.push(self.below_root(lf)?);
        }

        self.on_shell(long, move |sh| {
            let mut s = sh.session.borrow_mut();
            if !liberty_paths.is_empty() {
                lp::read_liberty(&mut s, &liberty_paths).map_err(|e| err("rejected", e))?;
            }
            lp::load_upf(&mut s, &path, scope.as_deref(), version, is_cpf)
                .map_err(|e| err("rejected", e))?;

            let Some(ref resolved) = s.lp.resolved else {
                return Err(err("internal", "load_upf succeeded without resolved intent"));
            };
            Ok(build_upf_reply(&s, resolved, &path))
        })?
    }

    pub(super) fn check_lp(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let stage = match opt_str(args, "stage")? {
            Some(st) => LpStage::parse(&st).map_err(|e| err("arguments", e))?,
            None => LpStage::All,
        };
        let prove_corruption = opt_bool(args, "prove_corruption")?.unwrap_or(false);
        let prove_waivers = opt_bool(args, "prove_waivers")?.unwrap_or(false);
        let sec = opt_bool(args, "sec")?.unwrap_or(false);
        let lec = opt_bool(args, "lec")?.unwrap_or(false);
        let tol_uv = match opt_f64(args, "tol_mv")? {
            Some(mv) if mv.is_finite() && mv >= 0.0 => (mv * 1000.0).round() as i64,
            Some(_) => return Err(wrong("tol_mv", "a non-negative finite number")),
            None => DEFAULT_VOLTAGE_TOL_UV,
        };
        let opts = CheckLpOpts {
            stage,
            tol_uv,
            prove_corruption,
            prove_waivers,
            sec,
            lec,
        };

        self.on_shell(long, move |sh| {
            let mut s = sh.session.borrow_mut();
            lp::check_lp(&mut s, &opts).map_err(|e| err("rejected", e))?;
            let list = lp::report::filtered_violations(&s.lp, &lp::report::ReportFilter::default());
            let summary = lp::report::build_summary(&list);
            Ok(json!({
                "schema": "informal.lp/1",
                "stages_run": s.lp.stages_run,
                "pst_consistent": s.lp.pst_consistent,
                "summary": summary,
                "stats": s.lp.last_stats,
                "violations": s.lp.violations,
                "waivers": s.lp.waivers,
            }))
        })?
    }

    pub(super) fn lp_diagnose(&self, args: &Value) -> Reply {
        let id_selector = parse_id_selector(args)?;
        let violation_alias = opt_str(args, "violation")?;
        let rule_filter = opt_str(args, "rule")?;
        let signal_filter = opt_str(args, "signal")?;
        let ctx_lines = opt_u64(args, "source_context_lines")?.unwrap_or(4).min(12) as usize;
        let max_events = opt_u64(args, "max_events")?.unwrap_or(24).min(64) as usize;

        if id_selector.is_none()
            && violation_alias.is_none()
            && rule_filter.is_none()
            && signal_filter.is_none()
        {
            return Err(err(
                "arguments",
                "give `id` (or `violation` / `rule` / `signal`) to select a violation",
            )
            .with_tool("check_lp")
            .with_hint("Run `check_lp` first and pass one violation's `id` to `lp_diagnose`."));
        }

        let (v, resolved_opt, pst_consistent, upf_files, why_opt, sv_req) =
            self.on_shell(false, move |sh| {
                let s = sh.session.borrow();
                let found = select_violation(
                    &s.lp.violations,
                    id_selector.as_ref(),
                    violation_alias.as_deref(),
                    rule_filter.as_deref(),
                    signal_filter.as_deref(),
                )?;

                let why_prop = found.formal_prop.clone().or_else(|| {
                    if found.status == LpStatus::FormalCex {
                        s.props
                            .iter()
                            .find(|p| {
                                p.task == lp::formal::aag::LOW_POWER_TASK
                                    && matches!(p.status, Status::Cex { .. })
                                    && p.trace.is_some()
                            })
                            .map(|p| p.name.clone())
                    } else {
                        None
                    }
                });
                let why_opt = why_prop.and_then(|pname| {
                    informal_fpv::why_cex::slice_property_cex(&s, &pname, max_events).ok()
                });
                let sv_req = informal_svlang::Request {
                    files: s.design.files.clone(),
                    defines: s.design.defines.clone(),
                    incdirs: s.design.incdirs.clone(),
                    top: s.design.top.clone(),
                    params: s.design.params.clone(),
                    black_box_modules: s.design.bbox.clone(),
                    ..Default::default()
                };
                Ok((
                    found.clone(),
                    s.lp.resolved.clone(),
                    s.lp.pst_consistent,
                    s.lp.upf_files.clone(),
                    why_opt,
                    sv_req,
                ))
            })??;

        let pst_witness = build_pst_witness(&v, resolved_opt.as_ref(), pst_consistent);
        let (upf_loc, upf_snippet) =
            build_upf_evidence(&v, resolved_opt.as_ref(), &upf_files, ctx_lines);
        let rtl_crossing_path = build_rtl_crossing_path(&v);
        let driver_snippets = build_driver_snippets(&v, &sv_req, ctx_lines);
        let why_cex_val = why_opt.as_ref().map(why_cex_to_json);
        let hint = build_lp_remediation_hint(&v);

        Ok(json!({
            "schema": "informal.lp_diagnose/1",
            "violation": v,
            "pst_witness": pst_witness,
            "upf_loc": upf_loc,
            "upf_snippet": upf_snippet,
            "rtl_loc": v.rtl_loc,
            "rtl_crossing_path": rtl_crossing_path,
            "driver_snippets": driver_snippets,
            "why_cex": why_cex_val,
            "hint": hint,
        }))
    }
}

#[derive(Debug, Clone)]
enum IdSelector {
    Num(usize),
    Text(String),
}

fn parse_id_selector(args: &Value) -> Result<Option<IdSelector>, ApiError> {
    match args.get("id") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(num)) => num
            .as_u64()
            .map(|n| Some(IdSelector::Num(n as usize)))
            .ok_or_else(|| wrong("id", "a positive integer or string violation ID")),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Err(wrong("id", "a non-empty violation ID"));
            }
            if let Ok(n) = trimmed.parse::<usize>() {
                return Ok(Some(IdSelector::Num(n)));
            }
            if let Some(rest) = trimmed
                .strip_prefix("LP-")
                .or_else(|| trimmed.strip_prefix("lp-"))
            {
                if let Ok(n) = rest.parse::<usize>() {
                    return Ok(Some(IdSelector::Num(n)));
                }
            }
            Ok(Some(IdSelector::Text(trimmed.to_string())))
        }
        Some(_) => Err(wrong("id", "an integer or string violation ID")),
    }
}

fn select_violation<'a>(
    violations: &'a [LpViolation],
    id_sel: Option<&IdSelector>,
    alias: Option<&str>,
    rule: Option<&str>,
    signal: Option<&str>,
) -> Result<&'a LpViolation, ApiError> {
    if let Some(sel) = id_sel {
        match sel {
            IdSelector::Num(num) => {
                if let Some(v) = violations.iter().find(|v| v.id == *num) {
                    return Ok(v);
                }
                return Err(err(
                    "not_found",
                    format!("no low-power violation with id `{num}`"),
                )
                .with_tool("check_lp")
                .with_hint("Run `check_lp` to list active low-power violation IDs."));
            }
            IdSelector::Text(txt) => {
                if let Some(v) = violations.iter().find(|v| {
                    v.rule_id.eq_ignore_ascii_case(txt)
                        || v.formal_prop.as_deref() == Some(txt.as_str())
                        || v.signal == *txt
                }) {
                    return Ok(v);
                }
                return Err(err(
                    "not_found",
                    format!("no low-power violation matching `{txt}`"),
                )
                .with_tool("check_lp")
                .with_hint("Run `check_lp` to inspect available violation IDs."));
            }
        }
    }
    if let Some(a) = alias {
        let trimmed = a.trim();
        let parsed_num = trimmed.parse::<usize>().ok().or_else(|| {
            trimmed
                .strip_prefix("LP-")
                .or_else(|| trimmed.strip_prefix("lp-"))
                .and_then(|r| r.parse::<usize>().ok())
        });
        if let Some(num) = parsed_num {
            if let Some(v) = violations.iter().find(|v| v.id == num) {
                return Ok(v);
            }
        }
        if let Some(v) = violations.iter().find(|v| {
            v.rule_id.eq_ignore_ascii_case(trimmed)
                || v.formal_prop.as_deref() == Some(trimmed)
                || v.signal == trimmed
        }) {
            return Ok(v);
        }
        return Err(err(
            "not_found",
            format!("no low-power violation matching `{trimmed}`"),
        )
        .with_tool("check_lp"));
    }
    let found = violations.iter().find(|v| {
        let rule_ok = rule.is_none_or(|r| v.rule_id.eq_ignore_ascii_case(r) || v.rule_id.contains(r));
        let sig_ok = signal.is_none_or(|sig| v.signal.contains(sig));
        rule_ok && sig_ok
    });
    found.ok_or_else(|| {
        err("not_found", "no low-power violation matched the given filter")
            .with_tool("check_lp")
    })
}

fn build_upf_reply(s: &Session, resolved: &ResolvedPowerIntent, path: &std::path::Path) -> Value {
    let intent = &resolved.intent;
    let domains: Vec<Value> = intent
        .domains
        .iter()
        .map(|pd| {
            let primary_ss = resolved
                .primary_ss(pd.id)
                .map(|ssid| intent.supply_sets[ssid.0 as usize].name.clone())
                .or_else(|| pd.primary_supply_set.clone());
            let inst_count = resolved
                .domain_instances
                .get(pd.id.0 as usize)
                .map_or(0, |set| set.len());
            json!({
                "name": pd.name,
                "scope": pd.scope,
                "is_top": pd.is_top,
                "primary_supply_set": primary_ss,
                "elements": pd.elements,
                "instance_count": inst_count,
            })
        })
        .collect();
    let supply_sets: Vec<Value> = intent
        .supply_sets
        .iter()
        .map(|ss| {
            let states: Vec<&str> = resolved
                .ss_states
                .get(ss.id.0 as usize)
                .map(|defs| defs.iter().map(|d| d.state_name.as_str()).collect())
                .unwrap_or_default();
            json!({
                "name": ss.name,
                "power_net": ss.power,
                "ground_net": ss.ground,
                "states": states,
            })
        })
        .collect();
    let supply_nets: Vec<Value> = intent
        .supply_nets
        .iter()
        .map(|sn| {
            json!({
                "name": sn.name,
                "scope": sn.scope,
                "ports": sn.connected_ports,
            })
        })
        .collect();
    let diagnostics: Vec<Value> = intent
        .diagnostics
        .iter()
        .map(|d| {
            let sev = match d.severity {
                DiagSeverity::Error => "error",
                DiagSeverity::Warning => "warning",
                DiagSeverity::Note => "note",
            };
            json!({
                "code": d.rule_id,
                "severity": sev,
                "message": d.message,
                "file": d.loc.as_ref().map(|l| l.file.as_str()),
                "line": d.loc.as_ref().map(|l| l.line),
            })
        })
        .collect();

    json!({
        "schema": "informal.upf/1",
        "file": path.display().to_string(),
        "upf_version": intent.upf_version.map(|v| v.as_str()),
        "domains": domains,
        "supply_sets": supply_sets,
        "supply_nets": supply_nets,
        "pst": {
            "consistent": s.lp.pst_consistent,
            "tables": intent.pst_tables.len(),
            "power_states": intent.power_states.len(),
            "illegal_combinations": intent.illegal_combinations.len(),
        },
        "strategies": {
            "isolation": intent.isolation_strategies.len(),
            "level_shifter": intent.level_shifter_strategies.len(),
            "retention": intent.retention_strategies.len(),
            "power_switch": intent.power_switches.len(),
            "repeater": intent.repeater_strategies.len(),
        },
        "diagnostics": diagnostics,
    })
}

fn build_pst_witness(
    v: &LpViolation,
    resolved: Option<&ResolvedPowerIntent>,
    pst_consistent: Option<bool>,
) -> Value {
    if let Some(ref w) = v.pst_witness {
        if let Ok(val) = serde_json::to_value(w) {
            return val;
        }
    }
    let mut supply_states = BTreeMap::new();
    if let Some(res) = resolved {
        for dom_name in [v.domain_src.as_deref(), v.domain_dst.as_deref()]
            .into_iter()
            .flatten()
        {
            if let Some(&pd_id) = res.domain_by_name.get(dom_name) {
                if let Some(ss_id) = res.primary_ss(pd_id) {
                    let ss_name = &res.intent.supply_sets[ss_id.0 as usize].name;
                    let state_names: Vec<&str> = res
                        .ss_states
                        .get(ss_id.0 as usize)
                        .map(|defs| defs.iter().map(|d| d.state_name.as_str()).collect())
                        .unwrap_or_default();
                    supply_states.insert(
                        ss_name.clone(),
                        if state_names.is_empty() {
                            "declared".to_string()
                        } else {
                            state_names.join("|")
                        },
                    );
                }
            }
        }
    }
    json!({
        "supply_states": supply_states,
        "pst_consistent": pst_consistent,
        "domain_src": v.domain_src,
        "domain_dst": v.domain_dst,
    })
}

fn build_upf_evidence(
    v: &LpViolation,
    resolved: Option<&ResolvedPowerIntent>,
    upf_files: &[lp::LpLoadSpec],
    ctx_lines: usize,
) -> (Option<Value>, Option<Value>) {
    let loc_opt = v.upf_loc.clone().or_else(|| {
        let res = resolved?;
        for dom_name in [v.domain_src.as_deref(), v.domain_dst.as_deref()]
            .into_iter()
            .flatten()
        {
            if let Some(&pd_id) = res.domain_by_name.get(dom_name) {
                if let Some(ref l) = res.intent.domains[pd_id.0 as usize].loc {
                    return Some(l.clone());
                }
            }
        }
        upf_files.first().map(|f| informal_upf::UpfLoc {
            file: f.path.display().to_string(),
            line: 1,
        })
    });
    match loc_opt {
        Some(loc) => {
            let snippet = diagnose::read_snippet(&loc.file, loc.line as usize, ctx_lines);
            (Some(json!({"file": loc.file, "line": loc.line})), snippet)
        }
        None => (None, None),
    }
}

fn build_rtl_crossing_path(v: &LpViolation) -> Value {
    let summary = match (&v.domain_src, &v.domain_dst) {
        (Some(src), Some(dst)) => format!(
            "{src} -> {dst} via `{}` ({}b, {} raw crossing(s))",
            v.signal, v.width_bits, v.raw_crossings
        ),
        (Some(src), None) => format!("{src}: `{}` ({}b)", v.signal, v.width_bits),
        _ => format!("`{}` ({}b)", v.signal, v.width_bits),
    };
    json!({
        "signal": v.signal,
        "domain_src": v.domain_src,
        "domain_dst": v.domain_dst,
        "width_bits": v.width_bits,
        "raw_crossings": v.raw_crossings,
        "rtl_loc": v.rtl_loc,
        "summary": summary,
    })
}

fn build_driver_snippets(
    v: &LpViolation,
    sv_req: &informal_svlang::Request,
    ctx_lines: usize,
) -> Vec<Value> {
    let mut snippets = Vec::new();
    if let Some(ref rl) = v.rtl_loc {
        if let Some((file, line_str)) = rl.rsplit_once(':') {
            if let Ok(line) = line_str.parse::<usize>() {
                if let Some(sn) = diagnose::read_snippet(file, line, ctx_lines) {
                    snippets.push(json!({
                        "signal": v.signal,
                        "role": "crossing_location",
                        "file": file,
                        "line": line,
                        "snippet": sn["snippet"],
                    }));
                }
            }
        }
    }
    let clean_sig = v
        .signal
        .split('[')
        .next()
        .unwrap_or(&v.signal)
        .trim_start_matches('/')
        .replace('/', ".");
    if !clean_sig.is_empty() && !sv_req.files.is_empty() {
        if let Ok(nav) = informal_svlang::assist::navigate_signal(sv_req, &clean_sig) {
            let refs = if !nav.drivers.is_empty() {
                &nav.drivers
            } else {
                &nav.declarations
            };
            for r in refs.iter().take(2) {
                let sn = diagnose::read_snippet(&r.file, r.line as usize, ctx_lines)
                    .map(|val| val["snippet"].clone())
                    .unwrap_or_else(|| json!(r.snippet));
                snippets.push(json!({
                    "signal": clean_sig,
                    "role": r.role,
                    "kind": r.kind,
                    "file": r.file,
                    "line": r.line,
                    "snippet": sn,
                }));
            }
        }
    }
    snippets
}

fn why_cex_to_json(rep: &informal_fpv::why_cex::WhyCexReport) -> Value {
    let events: Vec<Value> = rep
        .events
        .iter()
        .map(|ev| {
            let mut o = Map::new();
            o.insert("frame".into(), json!(ev.frame));
            o.insert("signal".into(), json!(ev.signal));
            o.insert("kind".into(), json!(ev.kind));
            o.insert("width".into(), json!(ev.width));
            o.insert("value".into(), json!(ev.value));
            if let Some(prev) = &ev.prev_value {
                o.insert("prev_value".into(), json!(prev));
            }
            o.insert("reason".into(), json!(ev.reason));
            Value::Object(o)
        })
        .collect();
    json!({
        "property": rep.property,
        "status": rep.status,
        "failure_frame": rep.failure_frame,
        "total_frames": rep.total_frames,
        "static_cone_signals": rep.static_cone_signals,
        "active_slice_signals": rep.active_slice_signals,
        "active_signals": rep.active_signals,
        "events": events,
    })
}

fn build_lp_remediation_hint(v: &LpViolation) -> String {
    let crossing = match (&v.domain_src, &v.domain_dst) {
        (Some(s), Some(d)) => format!("{s} -> {d}"),
        (Some(s), None) => s.clone(),
        _ => "design".to_string(),
    };
    let base = if v.rule_id.starts_with("ISO_MISSING") {
        format!(
            "Add a UPF `set_isolation` strategy covering `{}` across `{crossing}` (with an always-on `-isolation_supply_set` and active clamp value), or run `check_lp(prove_waivers=true)` if downstream RTL provably masks corruption.",
            v.signal
        )
    } else if v.rule_id.starts_with("ISO_") {
        format!(
            "Check the UPF `set_isolation` supply, clamp value, control polarity, and `-location` on `{}` ({crossing}): {}",
            v.signal, v.message
        )
    } else if v.rule_id.starts_with("LS_") {
        format!(
            "Add or correct the UPF `set_level_shifter` strategy (direction/voltage range/supply) on `{}` ({crossing}).",
            v.signal
        )
    } else if v.rule_id.starts_with("RET_") {
        format!(
            "Verify UPF `set_retention` backup supply, save/restore control signals, and polarity on `{}` ({crossing}).",
            v.signal
        )
    } else if v.rule_id.starts_with("PG_") {
        format!(
            "Fix PG pin net hookup or switch-chain connectivity on `{}` ({crossing}): {}",
            v.signal, v.message
        )
    } else {
        format!(
            "Resolve {} ({}, {}) on `{}`: {}",
            v.rule_id,
            v.severity.word(),
            v.status.word(),
            v.signal,
            v.message
        )
    };
    match v.status {
        LpStatus::FormalCex => format!(
            "{base} Formal 2-copy corruption analysis produced a real counterexample (`why_cex` attached); a manual waiver is unsafe."
        ),
        LpStatus::FormallyWaived => format!(
            "{base} Note: formally certified as masked (`formally_waived`)."
        ),
        LpStatus::Waived => format!(
            "{base} Note: currently manually waived; verify whether `allow_manual_lp_waivers` permits this in signoff."
        ),
        LpStatus::Open => base,
    }
}

/// Annotate schematic nodes with owning UPF power domain names when UPF is loaded.
pub(super) fn annotate_schematic_domains(
    graph: &mut cone::SchematicGraph,
    top: Option<&str>,
    resolved: &ResolvedPowerIntent,
) {
    let top_dom = resolve_top_domain_id(resolved);
    for node in &mut graph.nodes {
        let Some(ref sig) = node.signal else {
            continue;
        };
        if let Some(dom) = domain_for_signal(sig, top, resolved, top_dom) {
            node.power_domain = Some(dom);
        }
    }
}

fn resolve_top_domain_id(resolved: &ResolvedPowerIntent) -> Option<PowerDomainId> {
    let mut best: Option<(u8, PowerDomainId)> = None;
    for pd in &resolved.intent.domains {
        let excludes_root = pd
            .exclude_elements
            .iter()
            .any(|e| e == "/" || (pd.scope.is_empty() && e == "."));
        if excludes_root {
            continue;
        }
        let claims_root = (pd.scope.is_empty() && pd.elements.iter().any(|e| e == "."))
            || pd.elements.iter().any(|e| e == "/");
        let rank = if pd.is_top && !pd.include_scope {
            4
        } else if claims_root {
            3
        } else if pd.is_top || (pd.include_scope && pd.scope.is_empty()) {
            2
        } else if pd.scope.is_empty() && pd.elements.is_empty() {
            1
        } else {
            0
        };
        if rank > 0 && best.is_none_or(|(r, _)| rank > r) {
            best = Some((rank, pd.id));
        }
    }
    best.map(|(_, id)| id)
}

fn domain_for_signal(
    sig: &str,
    top: Option<&str>,
    resolved: &ResolvedPowerIntent,
    top_dom: Option<PowerDomainId>,
) -> Option<String> {
    if sig.starts_with("lp.pd.") {
        return sig.split('.').nth(2).map(String::from);
    }
    if sig.starts_with("informal.") {
        return top_dom.map(|id| resolved.intent.domains[id.0 as usize].name.clone());
    }
    let rel = top
        .and_then(|t| sig.strip_prefix(&format!("{t}.")))
        .unwrap_or(sig);
    let parts: Vec<&str> = rel.split('.').collect();
    if parts.len() >= 2 {
        // Walk from deepest instance prefix up to top-level child instance.
        for end in (1..parts.len()).rev() {
            let inst_slash = parts[..end].join("/");
            if let Some(&pd_id) = resolved.instance_domain.get(&inst_slash) {
                return Some(resolved.intent.domains[pd_id.0 as usize].name.clone());
            }
            let inst_dot = parts[..end].join(".");
            if let Some(&pd_id) = resolved.instance_domain.get(&inst_dot) {
                return Some(resolved.intent.domains[pd_id.0 as usize].name.clone());
            }
            for pd in &resolved.intent.domains {
                if pd.elements.iter().any(|e| {
                    let norm = e.trim_start_matches('/');
                    norm == inst_slash || norm == inst_dot
                }) {
                    return Some(pd.name.clone());
                }
            }
        }
    }
    top_dom.map(|id| resolved.intent.domains[id.0 as usize].name.clone())
}
