//! The tools behind the AI interface: each takes JSON arguments and gives
//! one versioned JSON reply about the FPV session, or changes it in the
//! way its mode allows (decision D17).

use crate::{cone, vcd};
use informal_engine::Engine;
use informal_fpv::session::glob;
use informal_fpv::{
    detect_flavor, Flavor, Kind, Property, ProveProgress, ProveSink, Session, Shell, Status, Worker,
};
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// What a client of the interface may do to the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Read and prove; nothing is added or changed.
    ReadOnly,
    /// Changes are recorded as proposals and wait for a person.
    Propose,
    /// Changes apply at once, tagged AI-added: for an evaluation sandbox.
    Auto,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "read-only" | "readonly" => Some(Mode::ReadOnly),
            "propose" => Some(Mode::Propose),
            "auto" | "automatic" => Some(Mode::Auto),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::ReadOnly => "read-only",
            Mode::Propose => "propose",
            Mode::Auto => "auto",
        }
    }
}

/// The window of the front end a person works in, for the tools that
/// show them something or read what they have selected (the `gui_` tools).
/// The desktop app implements it; a server on its own has none.
pub trait Gui: Send + Sync {
    /// What the window shows: the selected property, the open view, the
    /// open trace with its cursor and signals, the source lines in view.
    fn state(&self) -> Result<Value, String>;
    /// Do one thing in the window (`{"action": "open_trace", ...}`) and
    /// return its state afterwards. `Err` says why it could not be done.
    fn act(&self, action: &Value) -> Result<Value, String>;
}

/// Why a tool call gave no reply. `code` is one of `arguments`, `not_found`,
/// `busy`, `mode`, `rejected`, `no_trace`, `path`, `no_gui`, `internal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
}

fn err(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError {
        code,
        message: message.into(),
    }
}

pub type Reply = Result<Value, ApiError>;

/// A change an AI asked for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Proposal {
    pub id: u32,
    /// `assume`, `assert`, `cover`, `cut_point` or `command`.
    pub kind: String,
    /// The property's name, when the proposal gave one or once it is applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The property expression, the signal to cut, or the Tcl command.
    pub text: String,
    pub rationale: String,
    /// `pending`, `applied`, `rejected`, `withdrawn`, `undone`, `failed`,
    /// or `gone` (applied, then the session was cleared).
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

type ProgressHook = Arc<dyn Fn(&ProveProgress) + Send + Sync>;
type ChangeHook = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
struct Inner {
    proposals: Vec<Proposal>,
    next_id: u32,
    log: VecDeque<String>,
    progress: Option<ProveProgress>,
    hook: Option<ProgressHook>,
    changed: Option<ChangeHook>,
    /// The reply of the last prove started without waiting.
    last_prove: Option<Reply>,
}

/// What the interface keeps beside the session: the proposals, the last
/// console lines and the progress of a running prove. It is the session's
/// [`ProveSink`]; a front end with its own sink forwards to it.
#[derive(Default)]
pub struct State {
    inner: Mutex<Inner>,
    /// A prove, a script or a Tcl command is running on the shell thread.
    busy: AtomicBool,
    /// Commands of the session's other front end (a person at the app)
    /// that are running or waiting on the shell thread.
    foreign: AtomicUsize,
}

/// Held by the session's other front end while a command of its own has
/// the shell thread ([`State::foreign`]).
pub struct Foreign(Arc<State>);

impl Drop for Foreign {
    fn drop(&mut self) {
        self.0.foreign.fetch_sub(1, Ordering::SeqCst);
    }
}

impl State {
    /// Say that the other front end is running a command: until the guard
    /// is dropped, tools answer `busy` at once, not after the command.
    pub fn foreign(self: &Arc<Self>) -> Foreign {
        self.foreign.fetch_add(1, Ordering::SeqCst);
        Foreign(self.clone())
    }

    fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst) || self.foreign.load(Ordering::SeqCst) > 0
    }
}

const LOG_LINES: usize = 500;

impl ProveSink for State {
    fn progress(&self, p: &ProveProgress) {
        let hook = {
            let mut i = self.inner.lock().unwrap();
            i.progress = Some(p.clone());
            i.hook.clone()
        };
        if let Some(h) = hook {
            h(p);
        }
    }

    fn line(&self, text: &str, _err: bool) {
        let mut i = self.inner.lock().unwrap();
        if i.log.len() == LOG_LINES {
            i.log.pop_front();
        }
        i.log.push_back(text.to_string());
    }
}

/// Held while a long call has the shell thread.
pub struct Long(Arc<State>);

impl Drop for Long {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub mode: Mode,
    /// Scripts and design files are only opened below this directory.
    pub root: PathBuf,
    /// Scratch directory for script checks.
    pub workdir: PathBuf,
    /// The longest time limit a prove call may ask for.
    pub max_time: Duration,
}

pub struct Api {
    worker: Worker,
    state: Arc<State>,
    opts: Options,
    gui: OnceLock<Arc<dyn Gui>>,
}

/// The most of anything one reply carries.
const MAX_SOURCE_LINES: usize = 400;
const MAX_FRAMES: usize = 256;
const MAX_SIGNALS: usize = 64;
const MAX_SCOPE_SIGNALS: usize = 200;

impl Api {
    /// A session of its own on a shell thread.
    pub fn spawn(engines: Vec<Arc<dyn Engine>>, session_workdir: PathBuf, opts: Options) -> Api {
        let state = Arc::new(State::default());
        let sink = state.clone();
        let worker = Worker::spawn(Flavor::Jg, move || {
            let mut s = Session::new(engines, session_workdir);
            s.sink = Some(sink);
            s
        });
        Api::attach(worker, state, opts)
    }

    /// Serve a session another front end owns. Its sink must forward
    /// progress and lines to `state`.
    pub fn attach(worker: Worker, state: Arc<State>, mut opts: Options) -> Api {
        opts.root = opts.root.canonicalize().unwrap_or(opts.root);
        Api {
            worker,
            state,
            opts,
            gui: OnceLock::new(),
        }
    }

    /// Attach the window (once): the `gui_` tools are offered from then on.
    pub fn set_gui(&self, gui: Arc<dyn Gui>) {
        let _ = self.gui.set(gui);
    }

    pub fn has_gui(&self) -> bool {
        self.gui.get().is_some()
    }

    pub fn mode(&self) -> Mode {
        self.opts.mode
    }

    /// What the interface keeps beside the session.
    pub fn state(&self) -> &Arc<State> {
        &self.state
    }

    /// Ask a running prove to stop.
    pub fn stop(&self) {
        self.worker.stop();
    }

    /// Called with every progress event of a prove (`None` to end it).
    pub fn set_progress_hook(&self, hook: Option<ProgressHook>) {
        self.state.inner.lock().unwrap().hook = hook;
    }

    /// Called with the tool's name after every call that may have changed
    /// the session or the proposals, so that another front end on the same
    /// session can redraw.
    pub fn set_change_hook(&self, hook: Option<ChangeHook>) {
        self.state.inner.lock().unwrap().changed = hook;
    }

    fn changed(&self, tool: &str) {
        let hook = self.state.inner.lock().unwrap().changed.clone();
        if let Some(h) = hook {
            h(tool);
        }
    }

    /// True for the tools that hold the shell thread for long: a caller
    /// that must stay responsive runs them on a thread of their own, after
    /// [`Api::begin_long`].
    pub fn is_long(tool: &str, args: &Value) -> bool {
        match tool {
            "session_open" | "tcl_eval" => true,
            "prove" => args.get("wait").and_then(Value::as_bool).unwrap_or(true),
            _ => false,
        }
    }

    /// Reserve the shell thread for a long call, or say it is busy.
    pub fn begin_long(&self) -> Result<Long, ApiError> {
        if self.state.foreign.load(Ordering::SeqCst) > 0
            || self.state.busy.swap(true, Ordering::SeqCst)
        {
            return Err(busy());
        }
        Ok(Long(self.state.clone()))
    }

    /// Call a tool and wait for its reply.
    pub fn call(self: &Arc<Self>, tool: &str, args: &Value) -> Reply {
        if Api::is_long(tool, args) {
            let long = self.begin_long()?;
            return self.call_long(tool, args, long);
        }
        let reply = match tool {
            "script_check" => self.script_check(args),
            "session_summary" => self.session_summary(),
            "session_log" => self.session_log(args),
            "property_list" => self.property_list(args),
            "property_get" => self.property_get(args),
            "property_cone" => self.property_cone(args),
            "design_hierarchy" => self.design_hierarchy(args),
            "design_info" => self.design_info(),
            "design_source" => self.design_source(args),
            "trace_get" => self.trace_get(args),
            "prove" => self.prove_start(args),
            "prove_status" => self.prove_status(),
            "prove_stop" => {
                self.stop();
                Ok(json!({"schema": "informal.stop/1", "requested": true}))
            }
            "proposal_add" => self.proposal_add(args),
            "proposal_list" => self.proposal_list(args),
            "proposal_undo" => self.proposal_undo(args),
            t if t.starts_with("gui_") => self.gui_tool(t, args),
            _ => Err(err("not_found", format!("no tool named {tool}"))),
        };
        if matches!(tool, "proposal_add" | "proposal_undo") {
            self.changed(tool);
        }
        reply
    }

    /// A long tool, with the reservation from [`Api::begin_long`].
    pub fn call_long(&self, tool: &str, args: &Value, long: Long) -> Reply {
        let reply = match tool {
            "session_open" => self.session_open(args),
            "tcl_eval" => self.tcl_eval(args),
            "prove" => self.prove(args),
            _ => Err(err("not_found", format!("no long tool named {tool}"))),
        };
        drop(long);
        self.changed(tool);
        reply
    }

    /// Run `f` on the shell thread. Without `long`, the call is a short
    /// one: it gives up when a long one has the thread.
    fn on_shell<R: Send + 'static>(
        &self,
        long: bool,
        f: impl FnOnce(&Shell) -> R + Send + 'static,
    ) -> Result<R, ApiError> {
        if !long && self.state.is_busy() {
            return Err(busy());
        }
        self.worker
            .with(f)
            .ok_or_else(|| err("internal", "the shell thread has stopped"))
    }

    /// A path a client named, made absolute; `Err` when it is not a file
    /// below the root.
    fn below_root(&self, path: &str) -> Result<PathBuf, ApiError> {
        let p = self.opts.root.join(path);
        let p = p
            .canonicalize()
            .map_err(|e| err("path", format!("{path}: {e}")))?;
        if !p.starts_with(&self.opts.root) {
            return Err(err(
                "path",
                format!("{path} is outside {}", self.opts.root.display()),
            ));
        }
        Ok(p)
    }

    // ------------------------------------------------------------ session

    fn session_open(&self, args: &Value) -> Reply {
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

    fn script_check(&self, args: &Value) -> Reply {
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

    fn session_summary(&self) -> Reply {
        let (mode, state) = (self.opts.mode, self.state.clone());
        self.on_shell(false, move |sh| summary(&sh.session.borrow(), mode, &state))
    }

    fn session_log(&self, args: &Value) -> Reply {
        let last = opt_u64(args, "last")?.unwrap_or(50) as usize;
        let i = self.state.inner.lock().unwrap();
        let skip = i.log.len().saturating_sub(last);
        Ok(json!({
            "schema": "informal.log/1",
            "lines": i.log.iter().skip(skip).collect::<Vec<_>>(),
        }))
    }

    // --------------------------------------------------------- properties

    fn property_list(&self, args: &Value) -> Reply {
        let kind = opt_str(args, "kind")?;
        let status = opt_str(args, "status")?;
        let name = opt_str(args, "name")?;
        let origin = opt_str(args, "origin")?;
        let task = opt_str(args, "task")?;
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

    fn property_get(&self, args: &Value) -> Reply {
        let name = req_str(args, "name")?;
        self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            let p = find(&s, &name)?;
            Ok(json!({
                "schema": "informal.property/1",
                "property": prop_view(&s.props[p], true),
            }))
        })?
    }

    fn property_cone(&self, args: &Value) -> Reply {
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

    fn design_info(&self) -> Reply {
        self.on_shell(false, move |sh| {
            let mut s = sh.session.borrow_mut();
            let info = s.design_info();
            let mut v = serde_json::to_value(&info).map_err(|e| err("internal", e.to_string()))?;
            v["schema"] = json!("informal.design_info/1");
            Ok(v)
        })?
    }

    fn design_hierarchy(&self, args: &Value) -> Reply {
        let scope = opt_str(args, "scope")?;
        let depth = opt_u64(args, "depth")?.unwrap_or(2);
        let signals = opt_bool(args, "signals")?.unwrap_or(true);
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

    fn design_source(&self, args: &Value) -> Reply {
        let file = req_str(args, "file")?;
        let first = opt_u64(args, "first_line")?.unwrap_or(1).max(1) as usize;
        let last = opt_u64(args, "last_line")?.map(|l| l as usize);
        // A file of the design or a script that made a property may lie
        // outside the root; anything else must be below it.
        let known: Vec<PathBuf> = self.on_shell(false, |sh| {
            let s = sh.session.borrow();
            let sources = s.props.iter().filter_map(|p| p.source.as_ref());
            s.design
                .files
                .iter()
                .cloned()
                .chain(sources.map(|l| PathBuf::from(&l.file)))
                .filter_map(|p| p.canonicalize().ok())
                .collect()
        })?;
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

    // -------------------------------------------------------------- trace

    fn trace_get(&self, args: &Value) -> Reply {
        let name = req_str(args, "name")?;
        let globs = strings(args, "signals")?;
        let first = opt_u64(args, "first")?.map(|f| f as usize);
        let last = opt_u64(args, "last")?.map(|l| l as usize);
        let helpers = opt_bool(args, "helpers")?.unwrap_or(false);
        let (view, expr, vcd_text, model, clocks) = self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            let i = find(&s, &name)?;
            let p = &s.props[i];
            let Some(path) = &p.trace else {
                return Err(err(
                    "no_trace",
                    format!(
                        "{} has no trace: its status is {} (a trace comes with cex or covered)",
                        p.name,
                        p.status.word()
                    ),
                ));
            };
            let text = std::fs::read_to_string(path)
                .map_err(|e| err("internal", format!("{path}: {e}")))?;
            let model = std::fs::read_to_string(s.workdir.join(format!("prop_{i}.btor2"))).ok();
            let top = s.design.top.as_deref().unwrap_or_default();
            let clocks: Vec<String> = s
                .clocks
                .iter()
                .map(|c| format!("{top}.{}", c.signal))
                .collect();
            Ok((prop_view(p, false), p.expr.clone(), text, model, clocks))
        })??;
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

    // -------------------------------------------------------------- prove

    fn prove(&self, args: &Value) -> Reply {
        let names = strings(args, "properties")?;
        let task = opt_str(args, "task")?;
        let limit = match opt_f64(args, "time_limit_s")? {
            Some(t) if t > 0.0 => Some(Duration::from_secs_f64(t)),
            Some(_) => return Err(err("arguments", "time_limit_s must be above 0")),
            None => None,
        };
        let max = self.opts.max_time;
        self.state.inner.lock().unwrap().progress = None;
        self.on_shell(true, move |sh| {
            let mut s = sh.session.borrow_mut();
            let task = match &task {
                Some(t) => Some(s.task_name(t).map_err(|e| err("not_found", e))?),
                None => None,
            };
            let mut which = Vec::new();
            for n in &names {
                let mut m = match &task {
                    Some(t) => {
                        let mut m = s.matching(&informal_fpv::session::qualify(t, n));
                        m.retain(|&i| &s.props[i].task == t);
                        m
                    }
                    None => Vec::new(),
                };
                if m.is_empty() {
                    m = s.matching(n);
                }
                if m.is_empty() {
                    return Err(err("not_found", format!("no property matches {n}")));
                }
                which.extend(m);
            }
            if let (Some(t), true) = (&task, names.is_empty()) {
                which = s
                    .props_of(t)
                    .into_iter()
                    .filter(|&i| s.props[i].enabled && s.props[i].kind != Kind::Assume)
                    .collect();
                if which.is_empty() {
                    return Err(err("rejected", format!("task {t} has nothing to prove")));
                }
            }
            which.sort();
            which.dedup();
            which.retain(|&i| s.props[i].kind != Kind::Assume);
            if !names.is_empty() && which.is_empty() {
                return Err(err(
                    "rejected",
                    "only assumptions were named: nothing to prove",
                ));
            }
            let old = s.time_limit;
            s.time_limit = limit.unwrap_or(old).min(max);
            s.stop.store(false, Ordering::SeqCst);
            let done = s.prove(&which);
            let used = s.time_limit;
            s.time_limit = old;
            let done = done.map_err(|e| err("rejected", format!("{e:#}")))?;
            let results: Vec<&Property> = done.iter().map(|&i| &s.props[i]).collect();
            Ok(json!({
                "schema": "informal.prove/1",
                "time_limit_s": used.as_secs_f64(),
                "stopped": s.stop.load(Ordering::SeqCst),
                "counts": counts(results.iter().copied()),
                "results": results.iter().map(|p| prove_view(p)).collect::<Vec<_>>(),
            }))
        })?
    }

    /// `prove` with `wait: false`: start it and return.
    fn prove_start(self: &Arc<Self>, args: &Value) -> Reply {
        let long = self.begin_long()?;
        self.state.inner.lock().unwrap().last_prove = None;
        let (api, args) = (self.clone(), args.clone());
        std::thread::spawn(move || {
            let r = api.prove(&args);
            api.state.inner.lock().unwrap().last_prove = Some(r);
            drop(long);
            api.changed("prove");
        });
        Ok(json!({"schema": "informal.prove/1", "started": true}))
    }

    fn prove_status(&self) -> Reply {
        let i = self.state.inner.lock().unwrap();
        let running = self.state.is_busy();
        let mut reply = json!({"schema": "informal.progress/1", "running": running});
        if let Some(p) = &i.progress {
            reply["total"] = json!(p.total);
            reply["done"] = json!(p.done);
            reply["elapsed_ms"] = json!(p.elapsed_ms as u64);
            reply["properties"] = p
                .props
                .iter()
                .map(|q| {
                    let mut v = json!({"name": q.name, "kind": q.kind, "status": q.status});
                    if let Some(e) = &q.engine {
                        v["engine"] = json!(e);
                    }
                    if let Some(d) = q.depth {
                        v["depth"] = json!(d);
                    }
                    if let Some(ms) = q.ms {
                        v["time_ms"] = json!(ms as u64);
                    }
                    v
                })
                .collect();
        }
        match &i.last_prove {
            Some(Ok(r)) if !running => reply["result"] = r.clone(),
            Some(Err(e)) if !running => {
                reply["error"] = json!({"code": e.code, "message": e.message})
            }
            _ => {}
        }
        Ok(reply)
    }

    // ---------------------------------------------------------- proposals

    fn proposal_add(&self, args: &Value) -> Reply {
        self.changing()?;
        let kind = req_str(args, "kind")?;
        let rationale = req_str(args, "rationale")?;
        let name = opt_str(args, "name")?;
        let text = match kind.as_str() {
            "assume" | "assert" | "cover" => req_str(args, "expr")?,
            "cut_point" => req_str(args, "signal")?,
            _ => {
                return Err(err(
                    "arguments",
                    "kind must be assume, assert, cover or cut_point",
                ))
            }
        };
        self.propose(Proposal {
            id: 0,
            kind,
            name,
            text,
            rationale,
            state: "pending".into(),
            error: None,
        })
    }

    /// `Err` in read-only mode.
    fn changing(&self) -> Result<(), ApiError> {
        match self.opts.mode {
            Mode::ReadOnly => Err(err(
                "mode",
                "the interface is read-only: nothing can be added or changed",
            )),
            _ => Ok(()),
        }
    }

    /// Check a proposal against the session and record it: applied at once
    /// in automatic mode, pending otherwise. One that the session rejects
    /// is not recorded.
    fn propose(&self, mut p: Proposal) -> Reply {
        let auto = self.opts.mode == Mode::Auto;
        let long = p.kind == "command";
        let p = self.on_shell(long, move |sh| {
            if auto {
                apply(sh, &mut p)?;
                p.state = "applied".into();
            } else {
                check(&sh.session.borrow(), &p)?;
            }
            Ok(p)
        })??;
        let mut i = self.state.inner.lock().unwrap();
        i.next_id += 1;
        let p = Proposal { id: i.next_id, ..p };
        i.proposals.push(p.clone());
        Ok(json!({"schema": "informal.proposal/1", "proposal": p}))
    }

    fn proposal_list(&self, args: &Value) -> Reply {
        let state = opt_str(args, "state")?;
        // An applied proposal whose property or cut point the session no
        // longer has (`clear -all`, a new elaboration) is gone.
        let applied: Vec<Proposal> = {
            let i = self.state.inner.lock().unwrap();
            i.proposals
                .iter()
                .filter(|p| p.state == "applied")
                .cloned()
                .collect()
        };
        if !applied.is_empty() {
            let gone: Vec<u32> = self.on_shell(false, move |sh| {
                let s = sh.session.borrow();
                applied
                    .iter()
                    .filter(|p| match p.kind.as_str() {
                        "command" => false,
                        "cut_point" => !s.tasks.iter().any(|t| t.ai_stopats.contains(&p.text)),
                        _ => !s
                            .props
                            .iter()
                            .any(|q| q.ai && Some(&q.name) == p.name.as_ref()),
                    })
                    .map(|p| p.id)
                    .collect()
            })?;
            let mut i = self.state.inner.lock().unwrap();
            for p in i.proposals.iter_mut().filter(|p| gone.contains(&p.id)) {
                p.state = "gone".into();
            }
        }
        let i = self.state.inner.lock().unwrap();
        let list: Vec<&Proposal> = i
            .proposals
            .iter()
            .filter(|p| state.as_deref().is_none_or(|s| p.state == s))
            .collect();
        Ok(
            json!({"schema": "informal.proposals/1", "mode": self.opts.mode.label(), "proposals": list}),
        )
    }

    /// Take back one proposal (`id`) or every pending and applied one: a
    /// pending one is withdrawn, an applied one is removed from the
    /// session, and the results found with it in the model are reset.
    fn proposal_undo(&self, args: &Value) -> Reply {
        self.changing()?;
        let id = opt_u64(args, "id")?.map(|i| i as u32);
        let targets: Vec<Proposal> = {
            let i = self.state.inner.lock().unwrap();
            let live = |p: &&Proposal| p.state == "pending" || p.state == "applied";
            match id {
                None => i.proposals.iter().filter(live).cloned().collect(),
                Some(id) => {
                    let p = i
                        .proposals
                        .iter()
                        .find(|p| p.id == id)
                        .ok_or_else(|| err("not_found", format!("no proposal {id}")))?;
                    if !live(&p) {
                        return Err(err("rejected", format!("proposal {id} is {}", p.state)));
                    }
                    if p.kind == "command" && p.state == "applied" {
                        return Err(err("rejected", "a Tcl command that ran cannot be undone"));
                    }
                    vec![p.clone()]
                }
            }
        };
        // Last added first, so each removal finds the session as it left it.
        let work: Vec<Proposal> = targets
            .iter()
            .rev()
            .filter(|p| p.state == "applied" && p.kind != "command")
            .cloned()
            .collect();
        let (removed, reset) = self.on_shell(false, move |sh| {
            let mut s = sh.session.borrow_mut();
            let (mut removed, mut reset) = (Vec::new(), Vec::new());
            for p in &work {
                let r = match p.kind.as_str() {
                    "cut_point" => s.remove_ai_cut_point(&p.text),
                    _ => s.remove_ai_property(p.name.as_deref().unwrap_or_default()),
                };
                // Already gone with a cleared session: nothing to remove.
                reset.extend(r.unwrap_or_default());
                removed.push(p.id);
            }
            reset.sort();
            reset.dedup();
            (removed, reset)
        })?;
        let mut i = self.state.inner.lock().unwrap();
        let mut undone = Vec::new();
        for p in i.proposals.iter_mut() {
            if !targets.iter().any(|t| t.id == p.id) {
                continue;
            }
            p.state = match p.state.as_str() {
                "pending" => "withdrawn".into(),
                _ if removed.contains(&p.id) => "undone".into(),
                _ => continue,
            };
            undone.push(p.id);
        }
        Ok(json!({"schema": "informal.undo/1", "undone": undone, "reset": reset}))
    }

    /// The proposals as recorded, for the front end that shows them.
    pub fn proposals(&self) -> Vec<Proposal> {
        self.state.inner.lock().unwrap().proposals.clone()
    }

    /// A person's answer to a pending proposal: apply it, or reject it.
    /// Not a tool: only the front end a person works in calls it.
    pub fn decide(&self, id: u32, accept: bool) -> Reply {
        let mut p = {
            let i = self.state.inner.lock().unwrap();
            let p = i.proposals.iter().find(|p| p.id == id);
            p.cloned()
                .ok_or_else(|| err("not_found", format!("no proposal {id}")))?
        };
        if p.state != "pending" {
            return Err(err("rejected", format!("proposal {id} is {}", p.state)));
        }
        if accept {
            let long = p.kind == "command";
            let _held = if long { Some(self.begin_long()?) } else { None };
            p = self.on_shell(long, move |sh| {
                match apply(sh, &mut p) {
                    Ok(()) => p.state = "applied".into(),
                    Err(e) => {
                        p.state = "failed".into();
                        p.error = Some(e.message);
                    }
                }
                p
            })?;
        } else {
            p.state = "rejected".into();
        }
        let mut i = self.state.inner.lock().unwrap();
        if let Some(q) = i.proposals.iter_mut().find(|q| q.id == id) {
            *q = p.clone();
        }
        drop(i);
        self.changed("decide");
        Ok(json!({"schema": "informal.proposal/1", "proposal": p}))
    }

    // ---------------------------------------------------------------- gui

    /// The tools that show the person something, or read what they see.
    /// They change the window, never the session.
    fn gui_tool(&self, tool: &str, args: &Value) -> Reply {
        let gui = self.gui.get().ok_or_else(|| {
            err(
                "no_gui",
                "no window is attached: these tools work on a desktop app's session",
            )
        })?;
        let action = match tool {
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

    // ---------------------------------------------------------------- tcl

    fn tcl_eval(&self, args: &Value) -> Reply {
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

fn busy() -> ApiError {
    err(
        "busy",
        "a prove, a script or a Tcl command is running: wait for it, ask prove_status, or prove_stop",
    )
}

/// What applying `p` would reject, without changing the session.
fn check(s: &Session, p: &Proposal) -> Result<(), ApiError> {
    match p.kind.as_str() {
        "command" => Ok(()),
        "cut_point" => match s.cut_point(&p.text) {
            Ok(cut) if s.stopats_of(&s.current_task).contains(&cut) => {
                Err(format!("`{cut}` is already a cut point"))
            }
            r => r.map(|_| ()),
        },
        _ => s.check_property(p.name.as_deref(), &p.text),
    }
    .map_err(|e| err("rejected", e))
}

/// Make the change, tagged AI-added.
fn apply(sh: &Shell, p: &mut Proposal) -> Result<(), ApiError> {
    let rejected = |e: String| err("rejected", e);
    match p.kind.as_str() {
        "command" => {
            sh.session.borrow().stop.store(false, Ordering::SeqCst);
            sh.eval(&p.text).map_err(|e| rejected(e.message))?;
        }
        "cut_point" => {
            p.text = sh
                .session
                .borrow_mut()
                .add_ai_cut_point(&p.text)
                .map_err(rejected)?;
        }
        kind => {
            let kind = match kind {
                "assume" => Kind::Assume,
                "cover" => Kind::Cover,
                _ => Kind::Assert,
            };
            let mut s = sh.session.borrow_mut();
            p.name = Some(
                s.add_ai_property(kind, p.name.clone(), &p.text)
                    .map_err(rejected)?,
            );
        }
    }
    Ok(())
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

fn find(s: &Session, name: &str) -> Result<usize, ApiError> {
    s.props
        .iter()
        .position(|p| p.name == name)
        .ok_or_else(|| err("not_found", format!("no property named {name}")))
}

fn origin_of(p: &Property) -> &'static str {
    match (p.ai, p.rtl) {
        (true, _) => "ai",
        (_, true) => "rtl",
        _ => "user",
    }
}

/// One property as every reply gives it. `full` adds each engine's run.
fn prop_view(p: &Property, full: bool) -> Value {
    let mut v = Map::new();
    v.insert("name".into(), json!(p.name));
    v.insert("kind".into(), json!(p.kind.label()));
    v.insert("expr".into(), json!(p.expr));
    v.insert("enabled".into(), json!(p.enabled));
    v.insert("origin".into(), json!(origin_of(p)));
    v.insert("status".into(), json!(p.status.word()));
    match &p.status {
        Status::Proven { engine, ms } | Status::Unreachable { engine, ms } => {
            v.insert("engine".into(), json!(engine));
            v.insert("time_ms".into(), json!(*ms as u64));
        }
        Status::Cex { engine, depth, ms } | Status::Covered { engine, depth, ms } => {
            v.insert("engine".into(), json!(engine));
            v.insert("time_ms".into(), json!(*ms as u64));
            if let Some(d) = depth {
                v.insert("depth".into(), json!(d));
            }
        }
        Status::Undetermined { reason } => {
            v.insert("detail".into(), json!(reason));
        }
        Status::Error { message } => {
            v.insert("detail".into(), json!(message));
        }
        Status::Unprocessed => {}
    }
    v.insert("has_trace".into(), json!(p.trace.is_some()));
    v.insert("task".into(), json!(p.task));
    if let Some(c) = &p.copy_of {
        v.insert("copy_of".into(), json!(c));
    }
    if let Some(s) = &p.source {
        v.insert("source".into(), json!({"file": s.file, "line": s.line}));
    }
    if !p.under_ai.is_empty() {
        v.insert("under_ai".into(), json!(p.under_ai));
    }
    if let Some(st) = &p.stats {
        v.insert("stats".into(), json!(st));
    }
    match (p.vacuity, &p.status) {
        (Some(x), _) => {
            v.insert("vacuity".into(), json!(x.word()));
        }
        (None, Status::Proven { .. }) if p.kind == Kind::Assert => {
            v.insert("vacuity".into(), json!("not checked"));
        }
        _ => {}
    }
    if full {
        v.insert("engines".into(), json!(p.engines));
    }
    Value::Object(v)
}

/// A property in a prove reply: without its text and source, which the
/// caller named it by.
fn prove_view(p: &Property) -> Value {
    let mut v = prop_view(p, false);
    if let Some(o) = v.as_object_mut() {
        o.remove("expr");
        o.remove("source");
        o.remove("enabled");
    }
    v
}

/// How many properties have each status word.
fn counts<'a>(props: impl Iterator<Item = &'a Property>) -> Value {
    let mut m = Map::new();
    for p in props {
        let n = m.entry(p.status.word()).or_insert(json!(0));
        *n = json!(n.as_u64().unwrap_or(0) + 1);
    }
    Value::Object(m)
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

// ------------------------------------------------------------ arguments

fn wrong(key: &str, want: &str) -> ApiError {
    err("arguments", format!("`{key}` must be {want}"))
}

fn opt_str(args: &Value, key: &str) -> Result<Option<String>, ApiError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(wrong(key, "a string")),
    }
}

fn req_str(args: &Value, key: &str) -> Result<String, ApiError> {
    opt_str(args, key)?
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| err("arguments", format!("`{key}` is required")))
}

fn opt_u64(args: &Value, key: &str) -> Result<Option<u64>, ApiError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| wrong(key, "a whole number, 0 or more")),
    }
}

fn opt_f64(args: &Value, key: &str) -> Result<Option<f64>, ApiError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().map(Some).ok_or_else(|| wrong(key, "a number")),
    }
}

fn opt_bool(args: &Value, key: &str) -> Result<Option<bool>, ApiError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| wrong(key, "true or false")),
    }
}

/// A list of strings; a single string counts as a list of one.
fn strings(args: &Value, key: &str) -> Result<Vec<String>, ApiError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_str().map(String::from))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| wrong(key, "a list of strings")),
        Some(_) => Err(wrong(key, "a list of strings")),
    }
}

/// The path of a file below `root`, for callers that build [`Options`].
pub fn default_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf())
}
