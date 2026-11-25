use super::{
    err, find, no_trace_err, opt_bool, opt_str, opt_u64, prop_view, req_str, strings, Api, Reply,
};
use crate::{cone, diagnose, vcd};
use informal_fpv::session::glob;
use informal_fpv::Kind;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const MAX_FRAMES: usize = 256;
const MAX_SIGNALS: usize = 64;

impl Api {
    // -------------------------------------------------------------- trace

    pub(super) fn trace_get(&self, args: &Value) -> Reply {
        let name = req_str(args, "name")?;
        let globs = strings(args, "signals")?;
        let first = opt_u64(args, "first")?.map(|f| f as usize);
        let last = opt_u64(args, "last")?.map(|l| l as usize);
        let helpers = opt_bool(args, "helpers")?.unwrap_or(false);
        let (view, expr, trace_path, model, clocks) = if self.state.is_busy() {
            let t = self.live_trace_target(&name)?;
            (t.view, t.expr, t.path, t.model, t.clocks)
        } else {
            self.on_shell(false, move |sh| {
                let s = sh.session.borrow();
                let i = find(&s, &name)?;
                let p = &s.props[i];
                let Some(path) = &p.trace else {
                    return Err(no_trace_err(p));
                };
                let model = std::fs::read_to_string(s.workdir.join(format!("prop_{i}.btor2"))).ok();
                let top = s.design.top.as_deref().unwrap_or_default();
                let clocks: Vec<String> = s
                    .clocks
                    .iter()
                    .map(|c| format!("{top}.{}", c.signal))
                    .collect();
                Ok((
                    prop_view(p, false),
                    p.expr.clone(),
                    path.clone(),
                    model,
                    clocks,
                ))
            })??
        };
        if let Some(fst_path) = vcd::fst_for(std::path::Path::new(&trace_path)) {
            if let Ok(reply) = trace_get_fst(
                &fst_path,
                &view,
                &expr,
                model.as_deref(),
                &clocks,
                &globs,
                first,
                last,
                helpers,
            ) {
                return Ok(reply);
            }
        }
        let vcd_text = std::fs::read_to_string(&trace_path)
            .map_err(|e| err("internal", format!("{trace_path}: {e}")))?;
        let trace = vcd::parse(&vcd_text).map_err(|e| err("internal", e))?;
        let prop = view["name"].as_str().unwrap_or_default().to_string();
        let lane = format!("informal.{prop}");
        let hit_at: Vec<usize> = trace
            .signals
            .iter()
            .find(|s| s.name == lane)
            .map(|s| (0..trace.frames).filter(|&f| s.values[f] == "1").collect())
            .unwrap_or_default();
        // In a model of one step per cycle a clock input carries no value.
        let helper =
            |n: &str| n.starts_with("informal.") || (!trace.steps && clocks.iter().any(|c| c == n));
        // Chosen by name, or everything, the property's own signals first:
        // those its expression names, then the rest of its cone.
        let mut chosen: Vec<&vcd::Signal> = trace
            .signals
            .iter()
            .filter(|s| s.name != lane && (helpers || !helper(&s.name)))
            .filter(|s| {
                let tail = s.name.split_once('.').map_or("", |(_, t)| t);
                let leaf = s.name.rsplit('.').next().unwrap_or("");
                globs.is_empty()
                    || globs
                        .iter()
                        .any(|g| glob(g, &s.name) || glob(g, tail) || glob(g, leaf))
            })
            .collect();
        let in_cone: Vec<String> = model
            .and_then(|m| cone::cone(&m).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|c| c.name)
            .collect();
        let words: Vec<&str> = expr
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
            .collect();
        let rank = |s: &vcd::Signal| {
            let tail = s.name.split_once('.').map_or("", |(_, t)| t);
            if words.contains(&tail) {
                0
            } else if in_cone.contains(&s.name) {
                1
            } else {
                2
            }
        };
        chosen.sort_by_key(|s| rank(s));
        let matched = chosen.len();
        chosen.truncate(MAX_SIGNALS);
        let end = trace.frames.saturating_sub(1);
        let last = last.unwrap_or(end).min(end);
        let first = first
            .unwrap_or(0)
            .max((last + 1).saturating_sub(MAX_FRAMES))
            .min(last);
        let signals: Vec<Value> = chosen
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "width": s.width,
                    "values": s.values[first..=last].iter().map(|b| vcd::value(b)).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(json!({
            "schema": "informal.trace/1",
            "property": view,
            "unit": if trace.steps { "model_step" } else { "cycle" },
            "frames": trace.frames,
            "first": first,
            "last": last,
            "hit_at": hit_at,
            "signals": signals,
            "signals_omitted": matched - chosen.len(),
            "notes": trace.notes,
        }))
    }

    pub(super) fn why_cex(&self, args: &Value) -> Reply {
        let name = req_str(args, "name")?;
        let max_events = opt_u64(args, "max_events")?.unwrap_or(64) as usize;
        let rep = self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            let top = s.design.top.as_deref();
            let match_sec = |key: &str| {
                s.sec
                    .checks
                    .iter()
                    .find(|c| c.name == key || c.imp_sig == key)
                    .map(|chk| {
                        if chk.trace.is_none() {
                            Err(err(
                                "no_trace",
                                format!(
                                    "{} has no trace: its status is {}",
                                    chk.name,
                                    chk.status.word()
                                ),
                            ))
                        } else {
                            Ok(format!("sec::{}", chk.name))
                        }
                    })
            };
            let match_xprop = |key: &str| {
                s.xprop
                    .items
                    .iter()
                    .find(|it| {
                        it.name == key
                            || top.is_some_and(|t| it.name == format!("{t}.{key}"))
                            || format!("xprop_{}", it.name) == key
                    })
                    .map(|it| {
                        if !matches!(it.status, informal_fpv::xprop::XpropStatus::Cex { .. })
                            || it.trace.is_none()
                        {
                            Err(err(
                                "no_trace",
                                format!(
                                    "{} has no trace: its status is {}",
                                    it.name,
                                    it.status.word()
                                ),
                            ))
                        } else {
                            Ok(format!("xprop::{}", it.name))
                        }
                    })
            };
            let match_spv = |key: &str| {
                s.spv
                    .iter()
                    .find(|r| r.name == key || format!("spv_{}", r.name) == key)
                    .map(|rule| {
                        if !matches!(rule.status, informal_fpv::spv::SpvStatus::Cex { .. })
                            || rule.trace.is_none()
                        {
                            Err(err(
                                "no_trace",
                                format!(
                                    "{} has no trace: its status is {}",
                                    rule.name,
                                    rule.status.word()
                                ),
                            ))
                        } else {
                            Ok(format!("spv::{}", rule.name))
                        }
                    })
            };

            let match_prop = |prop_name: &str| {
                find(&s, prop_name).map(|i| {
                    let p = &s.props[i];
                    if p.trace.is_none() {
                        Err(no_trace_err(p))
                    } else {
                        Ok(p.name.clone())
                    }
                })
            };

            let target_name = if let Some(key) = name.strip_prefix("sec::") {
                match_sec(key)
                    .or_else(|| match_prop(&name).ok())
                    .ok_or_else(|| err("not_found", format!("no SEC check named {key}")))??
            } else if let Some(key) = name.strip_prefix("xprop::") {
                match_xprop(key)
                    .or_else(|| match_prop(&name).ok())
                    .ok_or_else(|| err("not_found", format!("no X-prop item named {key}")))??
            } else if let Some(key) = name.strip_prefix("spv::") {
                match_spv(key)
                    .or_else(|| match_prop(&name).ok())
                    .ok_or_else(|| err("not_found", format!("no SPV rule named {key}")))??
            } else if let Ok(res) = match_prop(&name) {
                res?
            } else if let Some(res) = match_sec(&name) {
                res?
            } else if let Some(res) = match_xprop(&name) {
                res?
            } else if let Some(res) = match_spv(&name) {
                res?
            } else {
                let _ = find(&s, &name)?;
                unreachable!()
            };
            informal_fpv::why_cex::slice_property_cex(&s, &target_name, max_events)
                .map_err(|e| err("internal", e))
        })??;
        let events: Vec<Value> = rep
            .events
            .iter()
            .map(|ev| {
                let mut o = serde_json::Map::new();
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
        Ok(json!({
            "schema": "informal.why_cex/1",
            "property": rep.property,
            "status": rep.status,
            "failure_frame": rep.failure_frame,
            "total_frames": rep.total_frames,
            "static_cone_signals": rep.static_cone_signals,
            "active_slice_signals": rep.active_slice_signals,
            "active_signals": rep.active_signals,
            "events": events,
        }))
    }

    /// High-signal single-call CEX dossier: fuses property status, backward
    /// `why_cex` causal slice, trimmed trace on active signals only, primary
    /// input vs DUT state roles, RTL driver snippets, and formal-lint hits.
    pub(super) fn cex_diagnose(&self, args: &Value) -> Reply {
        let name = req_str(args, "name")?;
        let window = opt_u64(args, "window")?.unwrap_or(6).min(32) as usize;
        let max_events = opt_u64(args, "max_events")?.unwrap_or(24).min(64) as usize;
        let ctx_lines = opt_u64(args, "source_context_lines")?.unwrap_or(4).min(12) as usize;

        let (prop_clone, view, rep, vcd_text, scopes, sv_req, rst) =
            self.on_shell(false, move |sh| {
                let s = sh.session.borrow();
                let i = find(&s, &name)?;
                let p = &s.props[i];
                let Some(path) = &p.trace else {
                    return Err(no_trace_err(p));
                };
                let rep = informal_fpv::why_cex::slice_property_cex(&s, &p.name, max_events)
                    .map_err(|e| err("internal", e))?;
                let vcd_text = std::fs::read_to_string(path)
                    .map_err(|e| err("internal", format!("{path}: {e}")))?;
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
                    p.clone(),
                    prop_view(p, false),
                    rep,
                    vcd_text,
                    s.design.hierarchy.clone(),
                    sv_req,
                    s.reset.clone(),
                ))
            })??;

        let mut signal_roles = BTreeMap::new();
        for sig in &rep.active_signals {
            signal_roles.insert(
                sig.clone(),
                diagnose::classify_signal_role(sig, &scopes).to_string(),
            );
        }

        let trace = vcd::parse(&vcd_text).map_err(|e| err("internal", e))?;
        let end = trace.frames.saturating_sub(1);
        let last = rep.failure_frame.min(end);
        let first = last.saturating_sub(window);
        let matches_active = |vcd_sig: &str| {
            rep.active_signals.iter().any(|a| {
                a == vcd_sig
                    || vcd_sig.ends_with(&format!(".{a}"))
                    || a.ends_with(&format!(".{vcd_sig}"))
            })
        };
        let mut trace_signals: Vec<Value> = trace
            .signals
            .iter()
            .filter(|s| !s.name.starts_with("informal.") && matches_active(&s.name))
            .take(24)
            .map(|s| {
                let role = diagnose::classify_signal_role(&s.name, &scopes);
                json!({
                    "name": s.name,
                    "width": s.width,
                    "role": role,
                    "values": s.values[first..=last].iter().map(|b| vcd::value(b)).collect::<Vec<_>>(),
                })
            })
            .collect();
        if trace_signals.is_empty() {
            trace_signals = trace
                .signals
                .iter()
                .filter(|s| !s.name.starts_with("informal."))
                .take(12)
                .map(|s| {
                    let role = diagnose::classify_signal_role(&s.name, &scopes);
                    json!({
                        "name": s.name,
                        "width": s.width,
                        "role": role,
                        "values": s.values[first..=last].iter().map(|b| vcd::value(b)).collect::<Vec<_>>(),
                    })
                })
                .collect();
        }

        // Prioritize DUT state/outputs first when pulling RTL driver snippets.
        let mut driver_targets: Vec<&String> = rep.active_signals.iter().collect();
        driver_targets.sort_by_key(|s| match signal_roles.get(*s).map(String::as_str) {
            Some("dut_output" | "internal_state") => 0,
            Some("primary_input") => 1,
            _ => 2,
        });
        let mut driver_snippets = Vec::new();
        for sig in driver_targets.into_iter().take(6) {
            if let Ok(nav) = informal_svlang::assist::navigate_signal(&sv_req, sig) {
                let refs = if !nav.drivers.is_empty() {
                    &nav.drivers
                } else {
                    &nav.declarations
                };
                let mut entries = Vec::new();
                for r in refs.iter().take(2) {
                    let mut entry = json!({
                        "file": r.file,
                        "line": r.line,
                        "role": r.role,
                        "kind": r.kind,
                    });
                    if let Some(sn) = diagnose::read_snippet(&r.file, r.line as usize, ctx_lines) {
                        entry["snippet"] = sn["snippet"].clone();
                    } else {
                        entry["snippet"] = json!(r.snippet);
                    }
                    entries.push(entry);
                }
                if !entries.is_empty() {
                    driver_snippets.push(json!({
                        "signal": sig,
                        "role": signal_roles.get(sig).cloned().unwrap_or_else(|| "unknown".into()),
                        "drivers": entries,
                    }));
                }
            }
        }

        let lint_raw =
            informal_svlang::assist::lint_formal_ready(&sv_req, rst.as_deref()).unwrap_or_default();
        let lint_rules: Vec<String> = lint_raw.iter().map(|f| f.rule.clone()).collect();
        let lint_findings: Vec<Value> = lint_raw
            .into_iter()
            .take(8)
            .map(|f| {
                json!({
                    "rule": f.rule,
                    "severity": f.severity,
                    "file": f.file,
                    "line": f.line,
                    "message": f.message,
                    "has_quick_fix": !f.fixes.is_empty(),
                })
            })
            .collect();

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

        let triage_hint =
            diagnose::cex_triage_hint(&prop_clone, &rep.active_signals, &signal_roles, &lint_rules);

        Ok(json!({
            "schema": "informal.cex_diagnose/1",
            "property": view,
            "causal_slice": {
                "failure_frame": rep.failure_frame,
                "total_frames": rep.total_frames,
                "static_cone_signals": rep.static_cone_signals,
                "active_slice_signals": rep.active_slice_signals,
                "active_signals": rep.active_signals,
                "events": events,
            },
            "active_trace": {
                "unit": if trace.steps { "model_step" } else { "cycle" },
                "first": first,
                "last": last,
                "signals": trace_signals,
            },
            "signal_roles": signal_roles,
            "driver_snippets": driver_snippets,
            "lint_findings": lint_findings,
            "triage_hint": triage_hint,
        }))
    }

    /// High-signal single-call proof-convergence dossier for `undetermined` or
    /// slow properties: engine telemetry, COI register/input counts,
    /// `fv_complexity` hotspots, active assumptions/cutpoints, and ranked tactics.
    pub(super) fn convergence_diagnose(&self, args: &Value) -> Reply {
        let name = req_str(args, "name")?;
        let (prop_clone, view, model_opt, complexity, assumptions, cut_points) =
            self.on_shell(false, move |sh| {
                let (prop_clone, view, model_opt, assumptions, cut_points, target_name) = {
                    let s = sh.session.borrow();
                    let idx = find(&s, &name)?;
                    let p = &s.props[idx];
                    if p.kind == Kind::Assume {
                        return Err(err(
                            "rejected",
                            "an assumption is not a verification target and has no convergence profile",
                        )
                        .with_tool("property_list")
                        .with_hint("Call convergence_diagnose on an assert or cover property."));
                    }
                    let task = p.task.clone();
                    let assumptions: Vec<String> = s
                        .props
                        .iter()
                        .filter(|q| q.enabled && q.kind == Kind::Assume && q.task == task)
                        .map(|q| q.name.clone())
                        .collect();
                    let cut_points = s.stopats_of(&task).to_vec();
                    let model_opt = s.model_for(idx).ok();
                    (
                        p.clone(),
                        prop_view(p, true),
                        model_opt,
                        assumptions,
                        cut_points,
                        p.name.clone(),
                    )
                };
                let cmd = format!("get_fv_complexity -json {{{target_name}}}");
                let complexity = sh
                    .eval(&cmd)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                    .unwrap_or_else(|| json!({}));
                Ok((
                    prop_clone,
                    view,
                    model_opt,
                    complexity,
                    assumptions,
                    cut_points,
                ))
            })??;

        let cone_sigs = model_opt
            .as_deref()
            .and_then(|m| cone::cone(m).ok())
            .unwrap_or_default();
        let inputs = cone_sigs
            .iter()
            .filter(|c| !c.helper && c.kind == "input")
            .count();
        let registers: Vec<Value> = cone_sigs
            .iter()
            .filter(|c| !c.helper && c.kind == "register")
            .map(|c| json!({"name": c.name, "width": c.width, "distance": c.distance}))
            .collect();
        let reg_count = registers.len();
        let total_signals = cone_sigs.iter().filter(|c| !c.helper).count();
        let sample_registers: Vec<Value> = registers.into_iter().take(24).collect();

        let recommendations = diagnose::convergence_recommendations(
            &prop_clone,
            reg_count,
            inputs,
            &complexity,
            assumptions.len(),
            cut_points.len(),
        );

        Ok(json!({
            "schema": "informal.convergence_diagnose/1",
            "property": view,
            "cone": {
                "inputs": inputs,
                "registers": reg_count,
                "total_signals": total_signals,
                "sample_registers": sample_registers,
            },
            "complexity": complexity,
            "assumptions_in_force": assumptions,
            "cut_points_in_force": cut_points,
            "recommendations": recommendations,
        }))
    }

    pub(super) fn schematic_get(&self, args: &Value) -> Reply {
        let name = req_str(args, "name")?;
        let max_depth = opt_u64(args, "max_depth")?.unwrap_or(2) as u32;
        let max_nodes = opt_u64(args, "max_nodes")?.unwrap_or(80) as usize;
        let (prop_name, model, active_signals, top, resolved_lp) =
            self.on_shell(false, move |sh| {
                let s = sh.session.borrow();
                let p = find(&s, &name)?;
                if s.props[p].kind == Kind::Assume {
                    return Err(err(
                        "rejected",
                        "an assumption is not a target and has no schematic cone",
                    ));
                }
                let model = s.model_for(p).map_err(|e| err("rejected", e))?;
                let active = informal_fpv::why_cex::slice_property_cex(&s, &s.props[p].name, 64)
                    .map(|r| r.active_signals)
                    .unwrap_or_default();
                Ok((
                    s.props[p].name.clone(),
                    model,
                    active,
                    s.design.top.clone(),
                    s.lp.resolved.clone(),
                ))
            })??;
        let mut g = cone::schematic(&model, max_depth, max_nodes, &active_signals)
            .map_err(|e| err("internal", e))?;
        if let Some(ref resolved) = resolved_lp {
            super::lp::annotate_schematic_domains(&mut g, top.as_deref(), resolved);
        }
        Ok(json!({
            "schema": "informal.schematic/1",
            "property": prop_name,
            "max_depth": g.max_depth,
            "truncated": g.truncated,
            "nodes": g.nodes,
            "edges": g.edges,
        }))
    }

    pub(super) fn fv_complexity(&self, args: &Value) -> Reply {
        let prop = opt_str(args, "property")?;
        let raw = self.on_shell(false, move |sh| {
            let cmd = match prop {
                Some(ref p) => format!("get_fv_complexity -json {{{p}}}"),
                None => "get_fv_complexity -json".to_string(),
            };
            sh.eval(&cmd).map_err(|e| err("rejected", e.message))
        })??;
        let parsed: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({"text": raw}));
        Ok(json!({
            "schema": "informal.complexity/1",
            "report": parsed,
        }))
    }
}

/// Streamed FST path for `trace_get` (decision D10): opens only the FST
/// header/hierarchy to rank & filter signals, then decodes ONLY the requested
/// `[first..=last]` window for the selected signals (plus the 1-bit property
/// lane for `hit_at`) without ever reading VCD text into memory.
#[allow(clippy::too_many_arguments)]
fn trace_get_fst(
    fst_path: &std::path::Path,
    view: &Value,
    expr: &str,
    model: Option<&str>,
    clocks: &[String],
    globs: &[String],
    first: Option<usize>,
    last: Option<usize>,
    helpers: bool,
) -> Result<Value, String> {
    let mut reader =
        informal_fpv::trace::fst_read::FstReader::open(fst_path).map_err(|e| format!("{e:#}"))?;
    let meta = reader.meta().clone();
    let prop = view["name"].as_str().unwrap_or_default().to_string();
    let lane = format!("informal.{prop}");
    let helper =
        |n: &str| n.starts_with("informal.") || (!meta.steps && clocks.iter().any(|c| c == n));

    let lane_idx = meta.signals.iter().position(|s| s.name == lane);
    let mut chosen_idx: Vec<usize> = meta
        .signals
        .iter()
        .enumerate()
        .filter(|(_, s)| !(s.clock && !meta.steps))
        .filter(|(_, s)| s.name != lane && (helpers || !helper(&s.name)))
        .filter(|(_, s)| {
            let tail = s.name.split_once('.').map_or("", |(_, t)| t);
            let leaf = s.name.rsplit('.').next().unwrap_or("");
            globs.is_empty()
                || globs
                    .iter()
                    .any(|g| glob(g, &s.name) || glob(g, tail) || glob(g, leaf))
        })
        .map(|(idx, _)| idx)
        .collect();

    let in_cone: Vec<String> = model
        .and_then(|m| cone::cone(m).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|c| c.name)
        .collect();
    let words: Vec<&str> = expr
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        .collect();
    chosen_idx.sort_by_key(|&idx| {
        let name = &meta.signals[idx].name;
        let tail = name.split_once('.').map_or("", |(_, t)| t);
        if words.contains(&tail) {
            0
        } else if in_cone.contains(name) {
            1
        } else {
            2
        }
    });
    let matched = chosen_idx.len();
    chosen_idx.truncate(MAX_SIGNALS);

    let end = meta.frames.saturating_sub(1);
    let last = last.unwrap_or(end).min(end);
    let first = first
        .unwrap_or(0)
        .max((last + 1).saturating_sub(MAX_FRAMES))
        .min(last);

    let hit_at: Vec<usize> = if let Some(li) = lane_idx {
        let lw = reader
            .read_window(0, end, Some(&[li]))
            .map_err(|e| format!("{e:#}"))?;
        let mut hits = Vec::new();
        if let Some(sig) = lw.signals.first() {
            for (idx, (rf, val)) in sig.runs.iter().enumerate() {
                if val == "1" {
                    let next_f = sig.runs.get(idx + 1).map_or(meta.frames, |(nf, _)| *nf);
                    for f in *rf..next_f.min(meta.frames) {
                        hits.push(f);
                    }
                }
            }
        }
        hits
    } else {
        Vec::new()
    };

    let win = reader
        .read_window(first, last, Some(&chosen_idx))
        .map_err(|e| format!("{e:#}"))?;
    let signals: Vec<Value> = win
        .signals
        .iter()
        .map(|s| {
            let mut vals = Vec::with_capacity(last - first + 1);
            let mut r_idx = 0usize;
            for f in first..=last {
                while r_idx + 1 < s.runs.len() && s.runs[r_idx + 1].0 <= f {
                    r_idx += 1;
                }
                let bits = s.runs.get(r_idx).map(|(_, b)| b.as_str()).unwrap_or("x");
                vals.push(vcd::value(bits));
            }
            json!({
                "name": s.name,
                "width": s.width,
                "values": vals,
            })
        })
        .collect();

    Ok(json!({
        "schema": "informal.trace/1",
        "property": view,
        "unit": if meta.steps { "model_step" } else { "cycle" },
        "frames": meta.frames,
        "first": first,
        "last": last,
        "hit_at": hit_at,
        "signals": signals,
        "signals_omitted": matched - chosen_idx.len(),
        "notes": meta.notes,
    }))
}
