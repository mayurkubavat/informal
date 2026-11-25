use super::{
    counts, err, find, no_trace_err, opt_bool, opt_str, opt_u64, origin_of, prop_view, req_str,
    strings, Api, ApiError, Mode, Proposal, Reply, State,
};
use crate::cone;
use informal_fpv::session::glob;
use informal_fpv::{detect_flavor, Flavor, Kind, Session, Shell};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

/// The most of anything one reply carries.
const MAX_SOURCE_LINES: usize = 400;
const MAX_SCOPE_SIGNALS: usize = 200;

impl Api {
    // ------------------------------------------------------------ session

    pub(super) fn session_open(&self, args: &Value) -> Reply {
        let script = match opt_str(args, "script")? {
            Some(s) => Some(self.below_root(&s)?),
            None => None,
        };
        let files = strings(args, "files")?
            .iter()
            .map(|f| self.below_root(f))
            .collect::<Result<Vec<_>, _>>()?;
        // A model's common slip, answered with what to do instead.
        if let Some(f) = files
            .iter()
            .find(|f| f.extension().is_some_and(|e| e == "tcl"))
        {
            return Err(err(
                "arguments",
                format!(
                    "{} is a Tcl script: pass it as `script`, not in `files`",
                    f.display()
                ),
            ));
        }
        let tcl = match (&script, files.is_empty()) {
            (Some(_), false) => return Err(err("arguments", "give `script` or `files`, not both")),
            (None, true) => return Err(err("arguments", "give `script` or `files`")),
            (Some(_), true) => None,
            (None, false) => Some(setup_script(&files, args)?),
        };
        let flavor = match (opt_str(args, "flavor")?, &script) {
            (Some(f), _) => Flavor::parse(&f)
                .ok_or_else(|| err("arguments", format!("unknown flavor {f} (jg or vcf)")))?,
            (None, Some(p)) => detect_flavor(&std::fs::read_to_string(p).unwrap_or_default()),
            (None, None) => Flavor::Jg,
        };
        {
            let mut i = self.state.inner.lock().unwrap();
            i.proposals.clear();
            i.progress = None;
            i.last_prove = None;
        }
        let mode = self.opts.mode;
        let state = self.state.clone();
        self.on_shell(true, move |sh| {
            sh.activate(flavor);
            {
                let mut s = sh.session.borrow_mut();
                s.clear();
                s.stop.store(false, Ordering::SeqCst);
            }
            let run = match (&script, &tcl) {
                (Some(p), _) => sh.source(p),
                (None, Some(t)) => sh.eval(t),
                (None, None) => unreachable!(),
            };
            let mut reply = json!({
                "schema": "informal.open/1",
                "ok": run.is_ok(),
                "summary": summary(&sh.session.borrow(), mode, &state),
            });
            if let Err(e) = run {
                reply["error"] = json!(e.message);
            }
            reply
        })
    }

    pub(super) fn script_check(&self, args: &Value) -> Reply {
        let script = self.below_root(&req_str(args, "script")?)?;
        let text = std::fs::read_to_string(&script)
            .map_err(|e| err("path", format!("{}: {e}", script.display())))?;
        let flavor = match opt_str(args, "flavor")? {
            Some(f) => Flavor::parse(&f)
                .ok_or_else(|| err("arguments", format!("unknown flavor {f} (jg or vcf)")))?,
            None => detect_flavor(&text),
        };
        // A shell of its own: the session being served is not touched.
        let shell = Shell::new(
            Session::new(Vec::new(), self.opts.workdir.join("check")),
            flavor,
        );
        let report = shell.check(&script);
        Ok(json!({
            "schema": "informal.check/1",
            "script": script,
            "clean": report.clean(),
            "findings": report.findings,
            "commands": report.commands,
        }))
    }

    pub(super) fn session_summary(&self) -> Reply {
        if self.state.is_busy() {
            return self.live_session_summary();
        }
        let (mode, state) = (self.opts.mode, self.state.clone());
        self.on_shell(false, move |sh| summary(&sh.session.borrow(), mode, &state))
    }

    pub(super) fn session_log(&self, args: &Value) -> Reply {
        let last = opt_u64(args, "last")?.unwrap_or(50) as usize;
        let i = self.state.inner.lock().unwrap();
        let skip = i.log.len().saturating_sub(last);
        Ok(json!({
            "schema": "informal.log/1",
            "lines": i.log.iter().skip(skip).collect::<Vec<_>>(),
        }))
    }

    // --------------------------------------------------------- properties

    pub(super) fn property_list(&self, args: &Value) -> Reply {
        let kind = opt_str(args, "kind")?;
        let status = opt_str(args, "status")?;
        let name = opt_str(args, "name")?;
        let origin = opt_str(args, "origin")?;
        let task = opt_str(args, "task")?;
        if self.state.is_busy() {
            return self.live_property_list(kind, status, name, origin, task);
        }
        self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            if let Some(t) = &task {
                s.task_name(t).map_err(|e| err("not_found", e))?;
            }
            let props: Vec<Value> = s
                .props
                .iter()
                .filter(|p| task.as_deref().is_none_or(|t| p.task == t))
                .filter(|p| kind.as_deref().is_none_or(|k| p.kind.label() == k))
                .filter(|p| status.as_deref().is_none_or(|w| p.status.word() == w))
                .filter(|p| name.as_deref().is_none_or(|n| glob(n, &p.name)))
                .filter(|p| origin.as_deref().is_none_or(|o| origin_of(p) == o))
                .map(|p| prop_view(p, false))
                .collect();
            Ok(json!({
                "schema": "informal.properties/1",
                "total": s.props.len(),
                "properties": props,
            }))
        })?
    }

    pub(super) fn property_get(&self, args: &Value) -> Reply {
        let name = req_str(args, "name")?;
        if self.state.is_busy() {
            return self.live_property_get(&name);
        }
        self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            let p = find(&s, &name)?;
            Ok(json!({
                "schema": "informal.property/1",
                "property": prop_view(&s.props[p], true),
            }))
        })?
    }

    pub(super) fn property_cone(&self, args: &Value) -> Reply {
        let name = req_str(args, "name")?;
        let helpers = opt_bool(args, "helpers")?.unwrap_or(false);
        let model = self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            let p = find(&s, &name)?;
            if s.props[p].kind == Kind::Assume {
                return Err(err(
                    "rejected",
                    "an assumption is not a target and has no cone",
                ));
            }
            let model = s.model_for(p).map_err(|e| err("rejected", e))?;
            Ok((s.props[p].name.clone(), model))
        })??;
        let signals: Vec<Value> = cone::cone(&model.1)
            .map_err(|e| err("internal", e))?
            .iter()
            .filter(|c| helpers || !c.helper)
            .map(|c| {
                json!({
                    "name": c.name, "width": c.width, "kind": c.kind,
                    "distance": c.distance, "helper": c.helper,
                })
            })
            .collect();
        Ok(json!({
            "schema": "informal.cone/1",
            "property": model.0,
            "signals": signals,
        }))
    }

    // ------------------------------------------------------------- design

    pub(super) fn design_info(&self) -> Reply {
        self.on_shell(false, move |sh| {
            let mut s = sh.session.borrow_mut();
            let info = s.design_info();
            let mut v = serde_json::to_value(&info).map_err(|e| err("internal", e.to_string()))?;
            v["schema"] = json!("informal.design_info/1");
            Ok(v)
        })?
    }

    pub(super) fn design_hierarchy(&self, args: &Value) -> Reply {
        let scope = opt_str(args, "scope")?;
        let depth = opt_u64(args, "depth")?.unwrap_or(2);
        let signals = opt_bool(args, "signals")?.unwrap_or(true);
        if self.state.is_busy() {
            return self.live_design_hierarchy(scope, depth, signals);
        }
        self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            if !s.elaborated {
                return Err(err("rejected", "no design is elaborated"));
            }
            let roots: Vec<&informal_svlang::Scope> = match &scope {
                None => s.design.hierarchy.iter().collect(),
                Some(path) => {
                    let all = s.design.hierarchy.iter().flat_map(|r| r.walk());
                    let found: Vec<_> = all.filter(|sc| &sc.path == path).collect();
                    if found.is_empty() {
                        return Err(err("not_found", format!("no scope {path}")));
                    }
                    found
                }
            };
            Ok(json!({
                "schema": "informal.hierarchy/1",
                "top": s.design.top,
                "scopes": roots.iter().map(|r| scope_view(r, depth, signals)).collect::<Vec<_>>(),
            }))
        })?
    }

    pub(super) fn design_source(&self, args: &Value) -> Reply {
        let file = req_str(args, "file")?;
        let first = opt_u64(args, "first_line")?.unwrap_or(1).max(1) as usize;
        let last = opt_u64(args, "last_line")?.map(|l| l as usize);
        // A file of the design or a script that made a property may lie
        // outside the root; anything else must be below it.
        let known: Vec<PathBuf> = if self.state.is_busy() {
            self.live_known_sources()
        } else {
            self.on_shell(false, |sh| {
                let s = sh.session.borrow();
                let sources = s.props.iter().filter_map(|p| p.source.as_ref());
                s.design
                    .files
                    .iter()
                    .cloned()
                    .chain(sources.map(|l| PathBuf::from(&l.file)))
                    .filter_map(|p| p.canonicalize().ok())
                    .collect()
            })?
        };
        let named = self.opts.root.join(&file);
        let path = match named.canonicalize() {
            Ok(p) if known.contains(&p) => p,
            _ => self.below_root(&file)?,
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|e| err("path", format!("{}: {e}", path.display())))?;
        let total = text.lines().count();
        let last = last
            .unwrap_or(first + MAX_SOURCE_LINES - 1)
            .min(first + MAX_SOURCE_LINES - 1)
            .min(total);
        let lines: Vec<&str> = text
            .lines()
            .skip(first - 1)
            .take((last + 1).saturating_sub(first))
            .collect();
        Ok(json!({
            "schema": "informal.source/1",
            "file": path,
            "first_line": first,
            "last_line": last,
            "total_lines": total,
            "lines": lines,
        }))
    }

    pub(super) fn open_workbench(&self, args: &Value) -> Reply {
        let focus = opt_str(args, "focus_property")?;
        let view = opt_str(args, "view")?;
        if self.state.is_busy() {
            return self.live_open_workbench(focus, view);
        }
        let snap = self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            serde_json::to_value(&*s).map_err(|e| err("internal", e.to_string()))
        })??;
        Ok(json!({
            "schema": "informal.workbench/1",
            "focus_property": focus,
            "view": view,
            "session": snap,
        }))
    }

    // ---------------------------------------------------------------- gui

    /// The tools that show the person something, or read what they see.
    /// They change the window, never the session.
    pub(super) fn gui_tool(&self, tool: &str, args: &Value) -> Reply {
        let gui = self.gui.get().ok_or_else(|| {
            err(
                "no_gui",
                "no window is attached: these tools work on a desktop app's session",
            )
            .with_tool("open_workbench")
            .with_hint(
                "Call open_workbench for the embedded MCP App, or launch with `informal mcp --connect` while InFormal.app is running.",
            )
        })?;
        let action = match tool {
            "gui_present_cex" => Some(self.gui_present_cex_action(args)?),
            "gui_state" => None,
            "gui_select_property" => {
                Some(json!({"action": "select_property", "name": req_str(args, "name")?}))
            }
            "gui_open_trace" => Some(json!({
                "action": "open_trace",
                "name": req_str(args, "name")?,
                "frame": opt_u64(args, "frame")?,
            })),
            "gui_add_signals" => {
                let signals = strings(args, "signals")?;
                if signals.is_empty() {
                    return Err(err("arguments", "`signals` is required"));
                }
                Some(json!({"action": "add_signals", "signals": signals}))
            }
            "gui_highlight_source" => {
                let first = opt_u64(args, "first_line")?
                    .filter(|l| *l > 0)
                    .ok_or_else(|| err("arguments", "`first_line` is required, from 1"))?;
                let last = opt_u64(args, "last_line")?.unwrap_or(first).max(first);
                Some(json!({
                    "action": "highlight_source",
                    "file": req_str(args, "file")?,
                    "first_line": first,
                    "last_line": last,
                }))
            }
            "gui_set_view" => Some(json!({
                "action": "set_view",
                "view": opt_str(args, "view")?,
                "layout_preset": opt_str(args, "layout_preset")?,
                "app_tab": opt_str(args, "app_tab")?,
                "bottom_tab": opt_str(args, "bottom_tab")?,
                "filter": opt_str(args, "filter")?,
                "status_filter": args.get("status_filter").cloned(),
            })),
            "gui_toggle_why_cex" => {
                let open = opt_bool(args, "open")?
                    .ok_or_else(|| err("arguments", "`open` boolean is required"))?;
                Some(json!({
                    "action": "toggle_why_cex",
                    "open": open,
                    "slice_only": opt_bool(args, "slice_only")?,
                }))
            }
            "gui_annotate_trace" => Some(json!({
                "action": "annotate_trace",
                "annotations": args.get("annotations").cloned().unwrap_or(json!([])),
                "clear": opt_bool(args, "clear")?.unwrap_or(false),
            })),
            "gui_present_walkthrough" => Some(json!({
                "action": "present_walkthrough",
                "title": opt_str(args, "title")?,
                "steps": args.get("steps").cloned(),
                "step": opt_u64(args, "step")?,
                "clear": opt_bool(args, "clear")?.unwrap_or(false),
            })),
            "gui_open_schematic" => Some(json!({
                "action": "open_schematic",
                "name": opt_str(args, "name")?,
                "max_depth": opt_u64(args, "max_depth")?,
            })),
            "gui_capture_view" => Some(json!({
                "action": "capture_view",
                "target": opt_str(args, "target")?.unwrap_or_else(|| "trace".to_string()),
            })),
            _ => return Err(err("not_found", format!("no tool named {tool}"))),
        };
        let state = match &action {
            None => gui.state(),
            Some(a) => gui.act(a),
        }
        .map_err(|e| err("rejected", e))?;
        let mut reply = json!({"schema": "informal.gui/1", "window": state});
        if let Some(a) = action {
            reply["done"] = a["action"].clone();
        }
        Ok(reply)
    }

    /// Build the atomic `present_cex` window payload, auto-synthesizing
    /// walkthrough steps and waveform callouts from `why_cex` when omitted.
    fn gui_present_cex_action(&self, args: &Value) -> Result<Value, ApiError> {
        let name = req_str(args, "name")?;
        let layout_preset = opt_str(args, "layout_preset")?.unwrap_or_else(|| "cex-debug".into());
        let slice_only = opt_bool(args, "slice_only")?.unwrap_or(false);
        let capture = opt_bool(args, "capture")?.unwrap_or(false);
        let title_arg = opt_str(args, "title")?;
        let steps_arg = args.get("steps").filter(|v| v.is_array()).cloned();
        let anns_arg = args.get("annotations").filter(|v| v.is_array()).cloned();

        let (prop_name, top_file, why_opt) = self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            let idx = find(&s, &name)?;
            let p = &s.props[idx];
            if p.trace.is_none() {
                return Err(no_trace_err(p));
            }
            let top_file = s
                .design
                .files
                .iter()
                .find(|f| f.ends_with(".sv") || f.ends_with(".svh") || f.ends_with(".v"))
                .or_else(|| s.design.files.first())
                .cloned();
            let why = informal_fpv::why_cex::slice_property_cex(&s, &p.name, 12).ok();
            Ok((p.name.clone(), top_file, why))
        })??;

        let title = title_arg.unwrap_or_else(|| format!("Why {prop_name} fails"));
        let steps = match steps_arg {
            Some(s) => s,
            None => match &why_opt {
                Some(rep) if !rep.events.is_empty() => {
                    let n = rep.events.len();
                    let list: Vec<Value> = rep
                        .events
                        .iter()
                        .take(6)
                        .enumerate()
                        .map(|(i, ev)| {
                            let label = if i + 1 == n.min(6) {
                                format!("Failure @C{}", ev.frame)
                            } else {
                                format!("Step {} @C{}", i + 1, ev.frame)
                            };
                            let val_txt = match &ev.prev_value {
                                Some(prev) => format!("{prev} -> {}", ev.value),
                                None => ev.value.clone(),
                            };
                            let mut st = json!({
                                "label": label,
                                "note": format!("{} = {} ({})", ev.signal, val_txt, ev.reason.replace('_', " ")),
                                "property": prop_name,
                                "frame": ev.frame,
                                "signals": [ev.signal],
                            });
                            if let Some(ref f) = top_file {
                                st["file"] = json!(f);
                            }
                            st
                        })
                        .collect();
                    Value::Array(list)
                }
                _ => json!([]),
            },
        };
        let annotations = match anns_arg {
            Some(a) => a,
            None => match &why_opt {
                Some(rep) => {
                    let list: Vec<Value> = rep
                        .events
                        .iter()
                        .take(4)
                        .enumerate()
                        .map(|(i, ev)| {
                            let is_fail = ev.frame == rep.failure_frame;
                            json!({
                                "frame": ev.frame,
                                "signal": ev.signal,
                                "label": if is_fail { format!("fail: {}", ev.value) } else { format!("cause #{}: {}", i + 1, ev.value) },
                                "tone": if is_fail { "danger" } else { "accent" },
                            })
                        })
                        .collect();
                    Value::Array(list)
                }
                None => json!([]),
            },
        };
        let frame =
            opt_u64(args, "frame")?.or_else(|| why_opt.as_ref().map(|r| r.failure_frame as u64));

        Ok(json!({
            "action": "present_cex",
            "name": prop_name,
            "frame": frame,
            "layout_preset": layout_preset,
            "slice_only": slice_only,
            "title": title,
            "steps": steps,
            "annotations": annotations,
            "capture": capture,
        }))
    }

    // ---------------------------------------------------------------- tcl

    pub(super) fn tcl_eval(&self, args: &Value) -> Reply {
        self.changing()?;
        let command = req_str(args, "command")?;
        if self.opts.mode == Mode::Propose {
            let rationale = req_str(args, "rationale").map_err(|_| {
                err(
                    "arguments",
                    "in propose mode a command waits for a person: say why in `rationale`",
                )
            })?;
            return self.propose(Proposal {
                id: 0,
                kind: "command".into(),
                name: None,
                text: command,
                rationale,
                state: "pending".into(),
                error: None,
            });
        }
        self.on_shell(true, move |sh| {
            sh.session.borrow().stop.store(false, Ordering::SeqCst);
            match sh.eval(&command) {
                Ok(result) => json!({"schema": "informal.tcl/1", "ok": true, "result": result}),
                Err(e) => json!({"schema": "informal.tcl/1", "ok": false, "error": e.message}),
            }
        })
    }
}

/// The setup of `session_open` with `files`, as JasperGold commands.
fn setup_script(files: &[PathBuf], args: &Value) -> Result<String, ApiError> {
    let word = |what: &str, s: &str| {
        if s.contains(['{', '}', '\\']) || s.trim().is_empty() {
            return Err(err("arguments", format!("{what} `{s}` cannot be used")));
        }
        Ok(format!("{{{s}}}"))
    };
    let mut tcl = String::from("clear -all\n");
    for f in files {
        tcl += &format!("analyze -sv12 {}\n", word("file", &f.to_string_lossy())?);
    }
    tcl += "elaborate";
    if let Some(top) = opt_str(args, "top")? {
        tcl += &format!(" -top {}", word("top", &top)?);
    }
    tcl += "\n";
    if let Some(clock) = opt_str(args, "clock")? {
        tcl += &format!("clock {}\n", word("clock", &clock)?);
    }
    if let Some(reset) = opt_str(args, "reset")? {
        tcl += &format!("reset -expression {}\n", word("reset", &reset)?);
    }
    Ok(tcl)
}

fn summary(s: &Session, mode: Mode, state: &State) -> Value {
    let of = |k: Kind| s.props.iter().filter(|p| p.kind == k).count();
    let targets = s.props.iter().filter(|p| p.kind != Kind::Assume);
    let i = state.inner.lock().unwrap();
    json!({
        "schema": "informal.summary/1",
        "mode": mode.label(),
        "elaborated": s.elaborated,
        "top": s.design.top,
        "files": s.design.files,
        "clocks": s.clocks,
        "reset": s.reset,
        "cut_points": s.stopats_of(&s.current_task),
        "current_task": s.current_task,
        "tasks": s.tasks.iter().map(|t| json!({
            "name": t.name,
            "properties": s.props.iter().filter(|p| p.task == t.name).count(),
            "cut_points": t.stopats,
        })).collect::<Vec<_>>(),
        "black_boxes": s.design.bbox.iter().chain(&s.design.bbox_instances).collect::<Vec<_>>(),
        "time_limit_s": s.time_limit.as_secs_f64(),
        "engines": s.engine_names(),
        "properties": {
            "assert": of(Kind::Assert),
            "assume": of(Kind::Assume),
            "cover": of(Kind::Cover),
            "by_status": counts(targets),
        },
        "ai": {
            "properties": s.props.iter().filter(|p| p.ai).count(),
            "cut_points": s.tasks.iter().map(|t| t.ai_stopats.len()).sum::<usize>(),
            "pending_proposals": i.proposals.iter().filter(|p| p.state == "pending").count(),
        },
    })
}

fn scope_view(sc: &informal_svlang::Scope, depth: u64, signals: bool) -> Value {
    let mut v = json!({"path": sc.path, "instance": sc.name, "module": sc.module});
    if signals {
        v["signals"] = json!(sc
            .signals
            .iter()
            .take(MAX_SCOPE_SIGNALS)
            .collect::<Vec<_>>());
        if sc.signals.len() > MAX_SCOPE_SIGNALS {
            v["signals_omitted"] = json!(sc.signals.len() - MAX_SCOPE_SIGNALS);
        }
    }
    if depth == 0 {
        v["children"] = json!([]);
        if !sc.children.is_empty() {
            v["children_omitted"] = json!(sc.children.len());
        }
    } else {
        v["children"] = sc
            .children
            .iter()
            .map(|c| scope_view(c, depth - 1, signals))
            .collect();
    }
    v
}
