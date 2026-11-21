//! The tools behind the AI interface: each takes JSON arguments and gives
//! one versioned JSON reply about the FPV session, or changes it in the
//! way its mode allows (decision D17).

use crate::{cone, diagnose, spec, vcd};
use informal_engine::Engine;
use informal_fpv::session::glob;
use informal_fpv::{
    detect_flavor, Flavor, Kind, Property, ProveProgress, ProveSink, Session, Shell, Status, Worker,
};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, VecDeque};
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
/// Optional Poka-Yoke fields (`hint`, `suggested_tool`, `candidates`) guide
/// agents directly to the right recovery step without trial-and-error loops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
    pub hint: Option<String>,
    pub suggested_tool: Option<&'static str>,
    pub candidates: Vec<String>,
}

impl ApiError {
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_tool(mut self, tool: &'static str) -> Self {
        self.suggested_tool = Some(tool);
        self
    }

    pub fn with_candidates(mut self, candidates: Vec<String>) -> Self {
        self.candidates = candidates;
        self
    }

    pub fn to_json(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("schema".into(), json!("informal.error/1"));
        obj.insert("code".into(), json!(self.code));
        obj.insert("message".into(), json!(self.message));
        if let Some(hint) = &self.hint {
            obj.insert("hint".into(), json!(hint));
        }
        if let Some(tool) = self.suggested_tool {
            obj.insert("suggested_tool".into(), json!(tool));
        }
        if !self.candidates.is_empty() {
            obj.insert("candidates".into(), json!(self.candidates));
        }
        Value::Object(obj)
    }
}

fn err(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError {
        code,
        message: message.into(),
        hint: None,
        suggested_tool: None,
        candidates: Vec::new(),
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
            "session_open" | "tcl_eval" | "proposal_validate" => true,
            "spec_audit" => args
                .get("check_dead_end")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            "prove" => args.get("wait").and_then(Value::as_bool).unwrap_or(true),
            "check_cov" => args.get("action").and_then(Value::as_str) == Some("measure"),
            "check_conn" | "check_xprop" | "check_spv" | "check_sec" | "check_csr"
            | "check_protocol" => args.get("action").and_then(Value::as_str) == Some("prove"),
            _ => false,
        }
    }

    /// Lightweight live session snapshot for hydrating MCP `prompts/get`
    /// without blocking when a long prove is running.
    pub fn prompt_context(&self) -> Value {
        let mut ctx = json!({
            "mode": self.opts.mode.label(),
            "has_gui": self.has_gui(),
            "top": "(not elaborated)",
            "clocks": "none",
            "reset": "none",
            "inputs": "(unknown)",
            "outputs": "(unknown)",
            "cex_properties": "none",
            "undetermined_properties": "none",
            "spec_file": Value::Null,
        });
        if self.state.is_busy() {
            return ctx;
        }
        let root = self.opts.root.clone();
        if let Ok(live) = self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            let top = s.design.top.as_deref().unwrap_or("(not elaborated)").to_string();
            let clocks = if s.clocks.is_empty() {
                "none".to_string()
            } else {
                s.clocks
                    .iter()
                    .map(|c| format!("{} ({})", c.signal, c.edge))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let reset = s.reset.clone().unwrap_or_else(|| "none".into());
            let (ins, outs, _) = diagnose::top_ports(&s.design.hierarchy);
            let cex: Vec<String> = s
                .props
                .iter()
                .filter(|p| matches!(p.status, Status::Cex { .. }))
                .map(|p| p.name.clone())
                .take(12)
                .collect();
            let undet: Vec<String> = s
                .props
                .iter()
                .filter(|p| matches!(p.status, Status::Undetermined { .. }))
                .map(|p| p.name.clone())
                .take(12)
                .collect();
            let spec_file = spec::discover_spec_file(&s, &root)
                .map(|p| p.display().to_string());
            json!({
                "top": top,
                "clocks": clocks,
                "reset": reset,
                "inputs": if ins.is_empty() { "(none)".into() } else { ins.join(", ") },
                "outputs": if outs.is_empty() { "(none)".into() } else { outs.join(", ") },
                "cex_properties": if cex.is_empty() { "none".into() } else { cex.join(", ") },
                "undetermined_properties": if undet.is_empty() { "none".into() } else { undet.join(", ") },
                "spec_file": spec_file,
            })
        }) {
            if let (Some(dst), Some(src)) = (ctx.as_object_mut(), live.as_object()) {
                for (k, v) in src {
                    dst.insert(k.clone(), v.clone());
                }
            }
        }
        ctx
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
            "why_cex" => self.why_cex(args),
            "cex_diagnose" => self.cex_diagnose(args),
            "convergence_diagnose" => self.convergence_diagnose(args),
            "open_workbench" => self.open_workbench(args),
            "schematic_get" => self.schematic_get(args),
            "formal_lint" => self.formal_lint(args),
            "formal_fix_apply" => self.formal_fix_apply(args),
            "signal_nav" => self.signal_nav(args),
            "fv_complexity" => self.fv_complexity(args),
            "spec_audit" => self.spec_audit(args, false),
            "prove_cache" => self.prove_cache(args),
            "check_cov" => self.check_cov(args, false),
            "check_conn" => self.check_conn(args, false),
            "check_xprop" => self.check_xprop(args, false),
            "check_spv" => self.check_spv(args, false),
            "check_sec" => self.check_sec(args, false),
            "check_csr" => self.check_csr(args, false),
            "check_protocol" => self.check_protocol(args, false),
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
        if matches!(
            tool,
            "proposal_add"
                | "proposal_undo"
                | "formal_fix_apply"
                | "prove_cache"
                | "check_cov"
                | "check_conn"
                | "check_xprop"
                | "check_spv"
                | "check_sec"
                | "check_csr"
                | "check_protocol"
        ) {
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
            "proposal_validate" => self.proposal_validate(args),
            "spec_audit" => self.spec_audit(args, true),
            "check_cov" => self.check_cov(args, true),
            "check_conn" => self.check_conn(args, true),
            "check_xprop" => self.check_xprop(args, true),
            "check_spv" => self.check_spv(args, true),
            "check_sec" => self.check_sec(args, true),
            "check_csr" => self.check_csr(args, true),
            "check_protocol" => self.check_protocol(args, true),
            _ => Err(err("not_found", format!("no long tool named {tool}"))),
        };
        drop(long);
        if !matches!(tool, "proposal_validate" | "spec_audit") {
            self.changed(tool);
        }
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
            )
            .with_hint(format!(
                "Pass a path below `{}` (or start informal mcp with a broader `--root`).",
                self.opts.root.display()
            )));
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
                return Err(no_trace_err(p));
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

    fn why_cex(&self, args: &Value) -> Reply {
        let name = req_str(args, "name")?;
        let max_events = opt_u64(args, "max_events")?.unwrap_or(64) as usize;
        let rep = self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            let target_name = if let Ok(i) = find(&s, &name) {
                let p = &s.props[i];
                if p.trace.is_none() {
                    return Err(no_trace_err(p));
                }
                p.name.clone()
            } else if let Some(chk) = s
                .sec
                .checks
                .iter()
                .find(|c| c.name == name || c.imp_sig == name)
            {
                if chk.trace.is_none() {
                    return Err(err(
                        "no_trace",
                        format!(
                            "{} has no trace: its status is {}",
                            chk.name,
                            chk.status.word()
                        ),
                    ));
                }
                chk.name.clone()
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
    fn cex_diagnose(&self, args: &Value) -> Reply {
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
    fn convergence_diagnose(&self, args: &Value) -> Reply {
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

    fn open_workbench(&self, args: &Value) -> Reply {
        let focus = opt_str(args, "focus_property")?;
        let view = opt_str(args, "view")?;
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

    fn schematic_get(&self, args: &Value) -> Reply {
        let name = req_str(args, "name")?;
        let max_depth = opt_u64(args, "max_depth")?.unwrap_or(2) as u32;
        let max_nodes = opt_u64(args, "max_nodes")?.unwrap_or(80) as usize;
        let (prop_name, model, active_signals) = self.on_shell(false, move |sh| {
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
            Ok((s.props[p].name.clone(), model, active))
        })??;
        let g = cone::schematic(&model, max_depth, max_nodes, &active_signals)
            .map_err(|e| err("internal", e))?;
        Ok(json!({
            "schema": "informal.schematic/1",
            "property": prop_name,
            "max_depth": g.max_depth,
            "truncated": g.truncated,
            "nodes": g.nodes,
            "edges": g.edges,
        }))
    }

    fn formal_lint(&self, args: &Value) -> Reply {
        let file_filter = opt_str(args, "file")?;
        let (req, rst) = self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            let req = informal_svlang::Request {
                files: s.design.files.clone(),
                defines: s.design.defines.clone(),
                incdirs: s.design.incdirs.clone(),
                top: s.design.top.clone(),
                params: s.design.params.clone(),
                black_box_modules: s.design.bbox.clone(),
                ..Default::default()
            };
            Ok((req, s.reset.clone()))
        })??;
        let mut findings = informal_svlang::assist::lint_formal_ready(&req, rst.as_deref())
            .map_err(|e| err("internal", e))?;
        if let Some(ref f) = file_filter {
            findings.retain(|d| d.file == *f || d.file.ends_with(&format!("/{f}")));
        }
        let val = serde_json::to_value(&findings).map_err(|e| err("internal", e.to_string()))?;
        Ok(json!({
            "schema": "informal.lint/1",
            "count": findings.len(),
            "findings": val,
        }))
    }

    fn formal_fix_apply(&self, args: &Value) -> Reply {
        self.changing()?;
        let file_arg = req_str(args, "file")?;
        let path = self.below_root(&file_arg)?;
        let start = opt_u64(args, "start")?
            .ok_or_else(|| err("arguments", "`start` byte offset is required"))?
            as usize;
        let end = opt_u64(args, "end")?
            .ok_or_else(|| err("arguments", "`end` byte offset is required"))?
            as usize;
        let replacement = req_str(args, "replacement")?;
        if end < start {
            return Err(err("arguments", "`end` must be >= `start`"));
        }
        if self.opts.mode == Mode::Propose {
            let rationale = opt_str(args, "rationale")?.unwrap_or_else(|| {
                format!(
                    "Apply formal-ready quick-fix in {} ({start}..{end})",
                    path.display()
                )
            });
            return self.propose(Proposal {
                id: 0,
                kind: "command".into(),
                name: None,
                text: format!(
                    "# quick-fix {}:{}..{} -> {}",
                    path.display(),
                    start,
                    end,
                    replacement
                ),
                rationale,
                state: "pending".into(),
                error: None,
            });
        }
        // Verify file is part of the session's design files
        let in_design = self.on_shell(false, {
            let check_path = path.clone();
            move |sh| {
                let s = sh.session.borrow();
                s.design
                    .files
                    .iter()
                    .any(|f| f.canonicalize().ok() == check_path.canonicalize().ok())
            }
        })?;
        if !in_design {
            return Err(err(
                "rejected",
                format!(
                    "{} is not in the elaborated design's file list",
                    path.display()
                ),
            ));
        }
        let mut src = std::fs::read_to_string(&path)
            .map_err(|e| err("path", format!("{}: {e}", path.display())))?;
        if end > src.len() || !src.is_char_boundary(start) || !src.is_char_boundary(end) {
            return Err(err(
                "arguments",
                "byte span is out of bounds or not UTF-8 aligned",
            ));
        }
        src.replace_range(start..end, &replacement);
        std::fs::write(&path, &src).map_err(|e| err("path", format!("{}: {e}", path.display())))?;
        self.on_shell(false, move |sh| {
            let _ = sh.eval("elaborate");
            Ok(())
        })??;
        Ok(json!({
            "schema": "informal.fix/1",
            "applied": true,
            "file": path.display().to_string(),
        }))
    }

    fn signal_nav(&self, args: &Value) -> Reply {
        let signal = req_str(args, "signal")?;
        let req = self.on_shell(false, move |sh| {
            let s = sh.session.borrow();
            Ok(informal_svlang::Request {
                files: s.design.files.clone(),
                defines: s.design.defines.clone(),
                incdirs: s.design.incdirs.clone(),
                top: s.design.top.clone(),
                params: s.design.params.clone(),
                black_box_modules: s.design.bbox.clone(),
                ..Default::default()
            })
        })??;
        let nav = informal_svlang::assist::navigate_signal(&req, &signal)
            .map_err(|e| err("internal", e))?;
        let mut v = serde_json::to_value(&nav).map_err(|e| err("internal", e.to_string()))?;
        v["schema"] = json!("informal.signal_nav/1");
        Ok(v)
    }

    fn fv_complexity(&self, args: &Value) -> Reply {
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

    fn spec_audit(&self, args: &Value, long: bool) -> Reply {
        let spec_file_arg = opt_str(args, "spec_file")?;
        let inline_spec = match args.get("spec") {
            None | Some(Value::Null) => None,
            Some(v) if v.is_object() => Some(v.clone()),
            Some(_) => return Err(wrong("spec", "a JSON object matching informal.spec/1")),
        };
        if spec_file_arg.is_some() && inline_spec.is_some() {
            return Err(err(
                "arguments",
                "pass either `spec_file` or inline `spec`, not both",
            ));
        }
        let task_arg = opt_str(args, "task")?;
        let check_dead_end = opt_bool(args, "check_dead_end")?.unwrap_or(false);
        let depth = opt_u64(args, "depth")?.unwrap_or(10).clamp(1, 50) as u32;
        let mutation_score = opt_f64(args, "mutation_score")?;
        if let Some(ms) = mutation_score {
            if !(0.0..=100.0).contains(&ms) || ms.is_nan() {
                return Err(wrong("mutation_score", "a percentage between 0 and 100"));
            }
        }

        // Resolve spec document before holding the shell for solver work.
        let (parsed_spec, spec_source) = if let Some(v) = inline_spec {
            let sp = spec::FvpSpec::parse_value(&v).map_err(|m| {
                err("arguments", m).with_hint(
                    "Ensure `schema` is `\"informal.spec/1\"` and requirement IDs are unique.",
                )
            })?;
            (sp, "<inline>".to_string())
        } else {
            let root = self.opts.root.clone();
            let known_dirs: Vec<PathBuf> = self.on_shell(long, |sh| {
                let s = sh.session.borrow();
                let mut dirs = Vec::new();
                for f in &s.design.files {
                    if let Some(p) = f.parent().and_then(|d| d.canonicalize().ok()) {
                        if !dirs.contains(&p) {
                            dirs.push(p);
                        }
                    }
                }
                for prop in &s.props {
                    if let Some(src) = &prop.source {
                        if let Some(p) = Path::new(&src.file)
                            .parent()
                            .and_then(|d| d.canonicalize().ok())
                        {
                            if !dirs.contains(&p) {
                                dirs.push(p);
                            }
                        }
                    }
                }
                dirs
            })?;
            let path = if let Some(raw) = spec_file_arg {
                let named = root.join(&raw);
                match named.canonicalize() {
                    Ok(p)
                        if p.parent()
                            .is_some_and(|d| known_dirs.iter().any(|kd| kd == d)) =>
                    {
                        p
                    }
                    _ => self.below_root(&raw)?,
                }
            } else {
                let root2 = root.clone();
                let found = self.on_shell(long, move |sh| {
                    let s = sh.session.borrow();
                    spec::discover_spec_file(&s, &root2)
                })?;
                found.ok_or_else(|| {
                    err(
                        "not_found",
                        "no `fvp.spec.json` found next to open design files or workspace root",
                    )
                    .with_hint(
                        "Pass `spec_file` (path to an `informal.spec/1` JSON file) or inline `spec`.",
                    )
                })?
            };
            let text = std::fs::read_to_string(&path)
                .map_err(|e| err("path", format!("{}: {e}", path.display())))?;
            let sp = spec::FvpSpec::parse_str(&text).map_err(|m| {
                err("arguments", format!("{}: {m}", path.display()))
                    .with_hint("Ensure `fvp.spec.json` uses `\"schema\": \"informal.spec/1\"`.")
            })?;
            (sp, path.display().to_string())
        };

        self.on_shell(long, move |sh| {
            let mut s = sh.session.borrow_mut();
            let target_task = match task_arg {
                Some(ref t) => s.task_name(t).map_err(|m| {
                    let cands =
                        diagnose::closest_matches(t, s.tasks.iter().map(|tk| tk.name.as_str()), 4);
                    err("not_found", m)
                        .with_tool("session_summary")
                        .with_candidates(cands)
                })?,
                None => s.current_task.clone(),
            };

            let mut rt = spec::AuditRuntime {
                spec_source,
                task: target_task.clone(),
                mutation_score,
                ..Default::default()
            };

            if check_dead_end && s.elaborated {
                let prev_task = s.current_task.clone();
                let prev_env = s.environment.clone();
                let prev_sink = s.sink.take();
                s.stop.store(false, Ordering::SeqCst);

                let _ = s.set_task(&target_task);
                if let Ok(env) = s.environment_now(depth) {
                    rt.env_conflict = env.conflict();
                    rt.conflict_set = env.conflict_set;
                }
                if !rt.env_conflict {
                    if let Ok(de) = s.dead_end_now(depth.min(10)) {
                        rt.dead_end_checked = true;
                        if let Some(hit) = de {
                            rt.dead_end_found = true;
                            rt.dead_end_depth = Some(hit.depth);
                        }
                    }
                } else {
                    rt.dead_end_checked = true;
                }

                let _ = s.set_task(&prev_task);
                s.environment = prev_env;
                s.sink = prev_sink;
            }

            Ok(spec::audit_session(&s, &parsed_spec, &rt))
        })?
    }

    // -------------------------------------------------------- formal apps

    fn prove_cache(&self, args: &Value) -> Reply {
        self.changing()?;
        let enabled = opt_bool(args, "enabled")?;
        let mode = opt_str(args, "mode")?;
        let path_opt = match opt_str(args, "path")? {
            Some(raw) => {
                if Path::new(&raw)
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return Err(err("path", format!("{raw} must not contain '..'")));
                }
                let joined = self.opts.root.join(&raw);
                if !joined.starts_with(&self.opts.root) {
                    return Err(err(
                        "path",
                        format!("{raw} is outside {}", self.opts.root.display()),
                    ));
                }
                Some(joined)
            }
            None => None,
        };
        if let Some(ref m) = mode {
            if !matches!(m.as_str(), "rw" | "ro" | "wo") {
                return Err(wrong("mode", "one of rw, ro, wo"));
            }
        }
        self.on_shell(false, move |sh| {
            let mut s = sh.session.borrow_mut();
            if let Some(en) = enabled {
                s.prove_cache = en;
            }
            if let Some(p) = path_opt {
                s.prove_cache_dir = Some(p);
            }
            if let Some(m) = mode {
                s.prove_cache_mode = informal_fpv::cache::CacheMode::parse(&m)
                    .ok_or_else(|| wrong("mode", "one of rw, ro, wo"))?;
            }
            let mode_str = match s.prove_cache_mode {
                informal_fpv::cache::CacheMode::ReadWrite => "rw",
                informal_fpv::cache::CacheMode::ReadOnly => "ro",
                informal_fpv::cache::CacheMode::WriteOnly => "wo",
            };
            let mut reply = serde_json::Map::new();
            reply.insert("schema".into(), json!("informal.prove_cache/1"));
            reply.insert("enabled".into(), json!(s.prove_cache));
            reply.insert("mode".into(), json!(mode_str));
            if let Some(ref dir) = s.prove_cache_dir {
                reply.insert("path".into(), json!(dir.display().to_string()));
            }
            Ok(Value::Object(reply))
        })?
    }

    fn check_cov(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let action = req_str(args, "action")?;
        let models = strings(args, "models")?;
        let proof_core = opt_bool(args, "proof_core")?.unwrap_or(false);
        self.on_shell(long, move |sh| {
            match action.as_str() {
                "init" => {
                    let mut cmd = String::from("check_cov -init");
                    if !models.is_empty() {
                        cmd.push_str(&format!(" -model {{{}}}", models.join(" ")));
                    }
                    if proof_core {
                        cmd.push_str(" -enable_prove_based_proof_core");
                    }
                    sh.eval(&cmd).map_err(|e| err("rejected", e.message))?;
                }
                "measure" => {
                    let mut cmd = String::from("check_cov -measure");
                    if !models.is_empty() {
                        cmd.push_str(&format!(" -type {{{}}}", models.join(" ")));
                    }
                    sh.eval(&cmd).map_err(|e| err("rejected", e.message))?;
                }
                "clear" => {
                    sh.session.borrow_mut().cov = Default::default();
                }
                "list" => {}
                other => {
                    return Err(wrong(
                        "action",
                        &format!("init, measure, list, or clear (got {other})"),
                    ))
                }
            }
            let s = sh.session.borrow();
            let active_models: Vec<String> = s
                .cov
                .active_models()
                .into_iter()
                .map(|m| m.as_str().to_string())
                .collect();
            let items: Vec<Value> = s
                .cov
                .items
                .iter()
                .map(|it| {
                    json!({
                        "id": it.id,
                        "task": it.task,
                        "model": it.model.as_str(),
                        "name": it.name,
                        "status": it.status.word(),
                        "detail": it.detail,
                    })
                })
                .collect();
            let report = informal_fpv::cov::format_report(&s.cov, None, None, &[], false);
            Ok(json!({
                "schema": "informal.cov/1",
                "initialized": s.cov.initialized,
                "measured": s.cov.measured,
                "proof_core": s.cov.prove_based,
                "models": active_models,
                "report": report,
                "items": items,
            }))
        })?
    }

    fn check_conn(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let action = req_str(args, "action")?;
        let csv_path = match opt_str(args, "file")? {
            Some(f) => Some(self.below_root(&f)?),
            None => None,
        };
        self.on_shell(long, move |sh| {
            match action.as_str() {
                "load" => {
                    let Some(p) = csv_path else {
                        return Err(err("arguments", "`file` is required when action is `load`"));
                    };
                    let cmd = format!("check_conn -load {{{}}}", p.display());
                    sh.eval(&cmd).map_err(|e| err("rejected", e.message))?;
                }
                "prove" => {
                    sh.eval("check_conn -prove")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "clear" => {
                    sh.eval("check_conn -clear")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "list" => {}
                other => {
                    return Err(wrong(
                        "action",
                        &format!("load, prove, list, or clear (got {other})"),
                    ))
                }
            }
            let s = sh.session.borrow();
            let props: Vec<Value> = s
                .props
                .iter()
                .filter(|p| p.task == informal_fpv::conn::DEFAULT_CONN_TASK)
                .map(|p| prop_view(p, false))
                .collect();
            Ok(json!({
                "schema": "informal.conn/1",
                "count": props.len(),
                "properties": props,
            }))
        })?
    }

    fn check_xprop(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let action = req_str(args, "action")?;
        let targets = strings(args, "targets")?;
        let x_sources = strings(args, "x_sources")?;
        let to = strings(args, "to")?;
        self.on_shell(long, move |sh| {
            match action.as_str() {
                "init" => {
                    let mut cmd = String::from("check_xprop -init");
                    if !targets.is_empty() {
                        cmd.push_str(&format!(" -targets {{{}}}", targets.join(" ")));
                    }
                    for src in &x_sources {
                        cmd.push_str(&format!(" -x_source {{{src}}}"));
                    }
                    for dst in &to {
                        cmd.push_str(&format!(" -to {{{dst}}}"));
                    }
                    sh.eval(&cmd).map_err(|e| err("rejected", e.message))?;
                }
                "prove" => {
                    sh.eval("check_xprop -prove")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "clear" => {
                    sh.eval("check_xprop -clear")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "list" => {}
                other => {
                    return Err(wrong(
                        "action",
                        &format!("init, prove, list, or clear (got {other})"),
                    ))
                }
            }
            let s = sh.session.borrow();
            let items: Vec<Value> = s
                .xprop
                .items
                .iter()
                .map(|it| {
                    let mut o = serde_json::Map::new();
                    o.insert("name".into(), json!(it.name));
                    o.insert("category".into(), json!(it.category));
                    o.insert("status".into(), json!(it.status.word()));
                    if let informal_fpv::xprop::XpropStatus::Cex { depth: Some(d) } = it.status {
                        o.insert("depth".into(), json!(d));
                    }
                    if let Some(ref eng) = it.engine {
                        o.insert("engine".into(), json!(eng));
                    }
                    Value::Object(o)
                })
                .collect();
            let report = informal_fpv::xprop::format_report(&s.xprop);
            Ok(json!({
                "schema": "informal.xprop/1",
                "initialized": s.xprop.initialized,
                "report": report,
                "items": items,
            }))
        })?
    }

    fn check_spv(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let action = req_str(args, "action")?;
        let name = opt_str(args, "name")?;
        let from = strings(args, "from")?;
        let to = strings(args, "to")?;
        let cond = opt_str(args, "cond")?;
        self.on_shell(long, move |sh| {
            match action.as_str() {
                "create" => {
                    if from.is_empty() || to.is_empty() {
                        return Err(err(
                            "arguments",
                            "`from` and `to` are required when action is `create`",
                        ));
                    }
                    let mut cmd = String::from("check_spv -create");
                    if let Some(ref n) = name {
                        cmd.push_str(&format!(" -name {{{n}}}"));
                    }
                    cmd.push_str(&format!(" -from {{{}}}", from.join(" ")));
                    cmd.push_str(&format!(" -to {{{}}}", to.join(" ")));
                    if let Some(ref c) = cond {
                        cmd.push_str(&format!(" -cond {{{c}}}"));
                    }
                    sh.eval(&cmd).map_err(|e| err("rejected", e.message))?;
                }
                "prove" => {
                    sh.eval("check_spv -prove")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "clear" => {
                    sh.eval("check_spv -clear")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "list" => {}
                other => {
                    return Err(wrong(
                        "action",
                        &format!("create, prove, list, or clear (got {other})"),
                    ))
                }
            }
            let s = sh.session.borrow();
            let rules: Vec<Value> = s
                .spv
                .iter()
                .map(|r| {
                    let mut o = serde_json::Map::new();
                    o.insert("name".into(), json!(r.name));
                    o.insert("task".into(), json!(r.task));
                    o.insert("from".into(), json!(r.from));
                    o.insert("to".into(), json!(r.to));
                    if let Some(ref c) = r.cond {
                        o.insert("cond".into(), json!(c));
                    }
                    o.insert("status".into(), json!(r.status.word()));
                    if let informal_fpv::spv::SpvStatus::Cex { depth: Some(d) } = r.status {
                        o.insert("depth".into(), json!(d));
                    }
                    if let Some(ref eng) = r.engine {
                        o.insert("engine".into(), json!(eng));
                    }
                    Value::Object(o)
                })
                .collect();
            let report = informal_fpv::spv::format_report(&s.spv);
            Ok(json!({
                "schema": "informal.spv/1",
                "report": report,
                "rules": rules,
            }))
        })?
    }

    fn check_sec(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let action = req_str(args, "action")?;
        let spec_raw = strings(args, "spec_files")?;
        let imp_raw = strings(args, "imp_files")?;
        let mut spec_files = Vec::with_capacity(spec_raw.len());
        for f in &spec_raw {
            spec_files.push(self.below_root(f)?);
        }
        let mut imp_files = Vec::with_capacity(imp_raw.len());
        for f in &imp_raw {
            imp_files.push(self.below_root(f)?);
        }
        let spec_top = opt_str(args, "spec_top")?;
        let imp_top = opt_str(args, "imp_top")?;
        let clock = opt_str(args, "clock")?;
        let reset = opt_str(args, "reset")?;
        let lec = opt_bool(args, "lec")?.unwrap_or(false);
        let latency = opt_u64(args, "latency")?.unwrap_or(0) as u32;
        let assumptions = strings(args, "assumptions")?;

        self.on_shell(long, move |sh| {
            match action.as_str() {
                "setup" => {
                    if spec_files.is_empty() || imp_files.is_empty() {
                        return Err(err(
                            "arguments",
                            "`spec_files` and `imp_files` are required when action is `setup`",
                        ));
                    }
                    let setup = informal_fpv::sec::SecSetup {
                        spec_files,
                        spec_top,
                        imp_files,
                        imp_top,
                        clock,
                        reset,
                        lec,
                        latency,
                        assumptions,
                        ..Default::default()
                    };
                    let mut s = sh.session.borrow_mut();
                    informal_fpv::sec::setup_sec(&mut s, setup).map_err(|e| err("rejected", e))?;
                }
                "prove" => {
                    sh.eval("check_sec -prove")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "clear" => {
                    sh.eval("check_sec -clear")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "list" => {}
                other => {
                    return Err(wrong(
                        "action",
                        &format!("setup, prove, list, or clear (got {other})"),
                    ))
                }
            }
            let s = sh.session.borrow();
            let configured = s.sec.setup.is_some();
            let cur_lec = s.sec.setup.as_ref().is_some_and(|st| st.lec);
            let cur_lat = s.sec.setup.as_ref().map_or(0, |st| st.latency);
            let checks: Vec<Value> = s
                .sec
                .checks
                .iter()
                .map(|c| {
                    let mut o = serde_json::Map::new();
                    o.insert("name".into(), json!(c.name));
                    o.insert("kind".into(), json!(c.kind));
                    o.insert("spec_sig".into(), json!(c.spec_sig));
                    o.insert("imp_sig".into(), json!(c.imp_sig));
                    o.insert("width".into(), json!(c.width));
                    o.insert("latency".into(), json!(c.latency));
                    o.insert("status".into(), json!(c.status.word()));
                    if let informal_fpv::sec::SecStatus::Cex { depth: Some(d) } = c.status {
                        o.insert("depth".into(), json!(d));
                    }
                    if let Some(ref eng) = c.engine {
                        o.insert("engine".into(), json!(eng));
                    }
                    if let Some(ref tr) = c.trace {
                        o.insert("trace".into(), json!(tr));
                    }
                    Value::Object(o)
                })
                .collect();
            let report = informal_fpv::sec::format_report(&s.sec);
            Ok(json!({
                "schema": "informal.sec/1",
                "configured": configured,
                "lec": cur_lec,
                "latency": cur_lat,
                "report": report,
                "checks": checks,
            }))
        })?
    }

    fn check_csr(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let action = req_str(args, "action")?;
        let spec_path = match opt_str(args, "file")? {
            Some(f) => Some(self.below_root(&f)?),
            None => None,
        };
        let prefix = opt_str(args, "prefix")?;
        let addr = opt_str(args, "addr")?;
        let wdata = opt_str(args, "wdata")?;
        let rdata = opt_str(args, "rdata")?;
        let we = opt_str(args, "we")?;
        let re = opt_str(args, "re")?;

        self.on_shell(long, move |sh| {
            match action.as_str() {
                "load" => {
                    let Some(p) = spec_path else {
                        return Err(err("arguments", "`file` is required when action is `load`"));
                    };
                    let mut cmd = format!("check_csr -load {{{}}}", p.display());
                    if let Some(ref v) = prefix {
                        cmd.push_str(&format!(" -prefix {{{v}}}"));
                    }
                    if let Some(ref v) = addr {
                        cmd.push_str(&format!(" -addr {{{v}}}"));
                    }
                    if let Some(ref v) = wdata {
                        cmd.push_str(&format!(" -wdata {{{v}}}"));
                    }
                    if let Some(ref v) = rdata {
                        cmd.push_str(&format!(" -rdata {{{v}}}"));
                    }
                    if let Some(ref v) = we {
                        cmd.push_str(&format!(" -we {{{v}}}"));
                    }
                    if let Some(ref v) = re {
                        cmd.push_str(&format!(" -re {{{v}}}"));
                    }
                    sh.eval(&cmd).map_err(|e| err("rejected", e.message))?;
                }
                "prove" => {
                    sh.eval("check_csr -prove")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "clear" => {
                    sh.eval("check_csr -clear")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "list" => {}
                other => {
                    return Err(wrong(
                        "action",
                        &format!("load, prove, list, or clear (got {other})"),
                    ))
                }
            }
            let s = sh.session.borrow();
            let props: Vec<Value> = s
                .props
                .iter()
                .filter(|p| p.task == informal_fpv::csr::DEFAULT_CSR_TASK)
                .map(|p| prop_view(p, false))
                .collect();
            Ok(json!({
                "schema": "informal.csr/1",
                "count": props.len(),
                "properties": props,
            }))
        })?
    }

    fn check_protocol(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let action = req_str(args, "action")?;
        let protocol = opt_str(args, "protocol")?;
        let role = opt_str(args, "role")?;
        let name = opt_str(args, "name")?;
        let prefix = opt_str(args, "prefix")?;

        self.on_shell(long, move |sh| {
            match action.as_str() {
                "create" => {
                    let Some(ref proto) = protocol else {
                        return Err(err(
                            "arguments",
                            "`protocol` is required when action is `create`",
                        ));
                    };
                    let mut cmd = format!("check_protocol -create -protocol {{{proto}}}");
                    if let Some(ref r) = role {
                        cmd.push_str(&format!(" -role {{{r}}}"));
                    }
                    if let Some(ref n) = name {
                        cmd.push_str(&format!(" -name {{{n}}}"));
                    }
                    if let Some(ref p) = prefix {
                        cmd.push_str(&format!(" -prefix {{{p}}}"));
                    }
                    sh.eval(&cmd).map_err(|e| err("rejected", e.message))?;
                }
                "prove" => {
                    sh.eval("check_protocol -prove")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "clear" => {
                    sh.eval("check_protocol -clear")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "list" => {}
                other => {
                    return Err(wrong(
                        "action",
                        &format!("create, prove, list, or clear (got {other})"),
                    ))
                }
            }
            let s = sh.session.borrow();
            let props: Vec<Value> = s
                .props
                .iter()
                .filter(|p| p.task == informal_fpv::protocol::DEFAULT_PROTOCOL_TASK)
                .map(|p| prop_view(p, false))
                .collect();
            Ok(json!({
                "schema": "informal.protocol/1",
                "count": props.len(),
                "properties": props,
            }))
        })?
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

    /// Zero-side-effect scratch-task soundness oracle: tests a candidate
    /// `assume`, `assert` (helper lemma), `cover`, or `cut_point` in an
    /// isolated temporary task before calling `proposal_add`.
    fn proposal_validate(&self, args: &Value) -> Reply {
        static VALIDATE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let kind = req_str(args, "kind")?;
        let name = opt_str(args, "name")?;
        let text = match kind.as_str() {
            "assume" | "assert" | "cover" => req_str(args, "expr")?,
            "cut_point" => req_str(args, "signal")?,
            _ => {
                return Err(err(
                    "arguments",
                    "kind must be assume, assert, cover or cut_point",
                )
                .with_hint("Pass `expr` for assume/assert/cover, or `signal` for cut_point."))
            }
        };
        let explicit_targets = args.get("targets").is_some() && !args["targets"].is_null();
        let req_targets = strings(args, "targets")?;
        let check_covers = opt_bool(args, "check_covers")?
            .unwrap_or(matches!(kind.as_str(), "assume" | "cut_point"));
        let check_dead_end = opt_bool(args, "check_dead_end")?.unwrap_or(kind == "assume");
        let depth = opt_u64(args, "depth")?.unwrap_or(20).clamp(1, 100) as u32;
        let max_s = self.opts.max_time.as_secs_f64();
        let limit_s = opt_f64(args, "time_limit_s")?
            .unwrap_or(15.0)
            .clamp(1.0, max_s);
        let limit = Duration::from_secs_f64(limit_s);
        let seq = VALIDATE_SEQ.fetch_add(1, Ordering::Relaxed);
        let scratch = format!("__validate_{seq}");

        self.on_shell(true, move |sh| {
            let mut s = sh.session.borrow_mut();
            if !s.elaborated {
                return Err(err("rejected", "no design is elaborated")
                    .with_tool("session_open")
                    .with_hint("Open and elaborate a design with session_open first."));
            }
            let probe = Proposal {
                id: 0,
                kind: kind.clone(),
                name: name.clone(),
                text: text.clone(),
                rationale: String::new(),
                state: "pending".into(),
                error: None,
            };
            check(&s, &probe)?;

            let overconstraint_warnings = if kind == "assume" {
                diagnose::assume_overconstraint_warnings(&text, &s.design.hierarchy)
            } else {
                Vec::new()
            };

            let prev_task = s.current_task.clone();
            let prev_limit = s.time_limit;
            let prev_env = s.environment.clone();
            let prev_sink = s.sink.take();
            let prev_assumed_len = s.assumed_from_assert.len();
            s.stop.store(false, Ordering::SeqCst);

            let outcome = (|| -> Reply {
                // Resolve targets & covers in the active task before creating scratch.
                let mut target_indices = Vec::new();
                if explicit_targets {
                    for tname in &req_targets {
                        let idx = find(&s, tname)?;
                        if s.props[idx].kind == Kind::Assume {
                            return Err(err(
                                "rejected",
                                format!("target `{tname}` is an assumption, not an assert/cover"),
                            )
                            .with_tool("property_list"));
                        }
                        target_indices.push(idx);
                    }
                } else {
                    target_indices = s
                        .props
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| {
                            p.enabled
                                && p.task == prev_task
                                && p.kind == Kind::Assert
                                && matches!(
                                    p.status,
                                    Status::Cex { .. } | Status::Undetermined { .. }
                                )
                        })
                        .map(|(i, _)| i)
                        .take(6)
                        .collect();
                }

                let cover_indices: Vec<usize> = if check_covers {
                    s.props
                        .iter()
                        .enumerate()
                        .filter(|(i, p)| {
                            p.enabled
                                && p.task == prev_task
                                && p.kind == Kind::Cover
                                && !target_indices.contains(i)
                        })
                        .map(|(i, _)| i)
                        .take(8)
                        .collect()
                } else {
                    Vec::new()
                };

                let assume_indices: Vec<usize> = s
                    .props
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.enabled && p.task == prev_task && p.kind == Kind::Assume)
                    .map(|(i, _)| i)
                    .collect();

                let target_meta: Vec<(String, String)> = target_indices
                    .iter()
                    .map(|&i| (s.props[i].name.clone(), s.props[i].status.word().to_string()))
                    .collect();
                let cover_meta: Vec<(String, String)> = cover_indices
                    .iter()
                    .map(|&i| (s.props[i].name.clone(), s.props[i].status.word().to_string()))
                    .collect();

                s.create_task(&scratch).map_err(|e| err("internal", e))?;
                s.copy_stopats(&prev_task, &scratch)
                    .map_err(|e| err("internal", e))?;
                if !assume_indices.is_empty() {
                    s.copy_into(&scratch, &assume_indices)
                        .map_err(|e| err("internal", e))?;
                }
                let scratch_targets = if !target_indices.is_empty() {
                    s.copy_into(&scratch, &target_indices)
                        .map_err(|e| err("internal", e))?
                } else {
                    Vec::new()
                };
                let scratch_covers = if !cover_indices.is_empty() {
                    s.copy_into(&scratch, &cover_indices)
                        .map_err(|e| err("internal", e))?
                } else {
                    Vec::new()
                };

                s.set_task(&scratch).map_err(|e| err("internal", e))?;
                s.time_limit = limit;

                // Apply candidate in scratch task only.
                let cand_name: Option<String> = match kind.as_str() {
                    "cut_point" => {
                        s.add_ai_cut_point(&text).map_err(|e| err("rejected", e))?;
                        None
                    }
                    "assume" => Some(
                        s.add_property_in(&scratch, Kind::Assume, name.clone(), &text)
                            .map_err(|e| err("rejected", e))?,
                    ),
                    "cover" => Some(
                        s.add_property_in(&scratch, Kind::Cover, name.clone(), &text)
                            .map_err(|e| err("rejected", e))?,
                    ),
                    _ => Some(
                        s.add_property_in(&scratch, Kind::Assert, name.clone(), &text)
                            .map_err(|e| err("rejected", e))?,
                    ),
                };

                // 1. Environment consistency + 2QBF dead-end check for assumptions.
                let mut env_conflict = false;
                let mut env_allowed: Option<u32> = None;
                let mut conflict_set: Vec<String> = Vec::new();
                let mut dead_end_val = json!({"checked": false, "found": false});
                if kind == "assume" {
                    if let Ok(env) = s.environment_now(depth) {
                        env_conflict = env.conflict();
                        env_allowed = env.allowed;
                        conflict_set = env.conflict_set;
                    }
                    if !env_conflict && check_dead_end {
                        match s.dead_end_now(depth.min(10)) {
                            Ok(Some(de)) => {
                                dead_end_val = json!({
                                    "checked": true,
                                    "found": true,
                                    "depth": de.depth,
                                    "trap_state": de.trap_state.iter().take(16).map(|(sig, val)| json!({"signal": sig, "value": val})).collect::<Vec<_>>(),
                                });
                            }
                            Ok(None) => {
                                dead_end_val = json!({"checked": true, "found": false});
                            }
                            Err(e) => {
                                dead_end_val = json!({"checked": false, "found": false, "error": e.to_string()});
                            }
                        }
                    }
                }
                let dead_end_found = dead_end_val["found"].as_bool().unwrap_or(false);

                // 2. If candidate is an assert or cover, prove it directly first.
                let mut candidate_result = Value::Null;
                let mut cand_ok = true;
                let mut cand_vacuous = false;
                let mut cand_promoted = false;
                if let Some(ref cname) = cand_name {
                    if matches!(kind.as_str(), "assert" | "cover") {
                        if let Some(cidx) = s.props.iter().position(|p| p.name == *cname) {
                            let _ = s.prove(&[cidx]);
                            let cp = &s.props[cidx];
                            cand_vacuous = cp.vacuity.is_some_and(|v| v.is_vacuous());
                            cand_ok = match kind.as_str() {
                                "assert" => {
                                    matches!(cp.status, Status::Proven { .. }) && !cand_vacuous
                                }
                                "cover" => matches!(cp.status, Status::Covered { .. }),
                                _ => true,
                            };
                            candidate_result = prop_view(cp, false);
                            // For a proven non-vacuous helper assertion, test promoting it to
                            // see if it closes the requested/failing target assertions!
                            if kind == "assert"
                                && cand_ok
                                && !scratch_targets.is_empty()
                                && s.assume_from_assert(&scratch, &[cidx], None).is_ok()
                            {
                                cand_promoted = true;
                            }
                        }
                    }
                }

                // 3. Prove targets & cover regression in the scratch task.
                let should_check_impact = !env_conflict
                    && !dead_end_found
                    && (matches!(kind.as_str(), "assume" | "cut_point")
                        || (kind == "assert" && cand_promoted));

                let mut newly_proven = Vec::new();
                let mut vacuous_assertions = Vec::new();
                let mut still_failing = Vec::new();
                let mut target_results = Vec::new();
                let mut lost_covers = Vec::new();
                let mut cover_results = Vec::new();

                if should_check_impact {
                    let mut prove_idxs = Vec::new();
                    for sn in &scratch_targets {
                        if let Some(i) = s.props.iter().position(|p| p.name == *sn) {
                            prove_idxs.push(i);
                        }
                    }
                    if matches!(kind.as_str(), "assume" | "cut_point") {
                        for sn in &scratch_covers {
                            if let Some(i) = s.props.iter().position(|p| p.name == *sn) {
                                prove_idxs.push(i);
                            }
                        }
                    }
                    if !prove_idxs.is_empty() {
                        let _ = s.prove(&prove_idxs);
                    }
                    for ((orig_name, orig_status), sn) in
                        target_meta.iter().zip(scratch_targets.iter())
                    {
                        if let Some(p) = s.props.iter().find(|p| p.name == *sn) {
                            let new_status = p.status.word().to_string();
                            let vac = p.vacuity.is_some_and(|v| v.is_vacuous());
                            if matches!(p.status, Status::Proven { .. }) {
                                if vac {
                                    vacuous_assertions.push(orig_name.clone());
                                } else if orig_status != "proven" {
                                    newly_proven.push(orig_name.clone());
                                }
                            } else if matches!(
                                p.status,
                                Status::Cex { .. } | Status::Undetermined { .. } | Status::Error { .. }
                            ) {
                                still_failing.push(orig_name.clone());
                            }
                            target_results.push(json!({
                                "name": orig_name,
                                "before": orig_status,
                                "after": new_status,
                                "vacuity": p.vacuity.map(|v| v.word()).unwrap_or("not checked"),
                            }));
                        }
                    }
                    if matches!(kind.as_str(), "assume" | "cut_point") {
                        for ((orig_name, orig_status), sn) in
                            cover_meta.iter().zip(scratch_covers.iter())
                        {
                            if let Some(p) = s.props.iter().find(|p| p.name == *sn) {
                                let new_status = p.status.word().to_string();
                                if matches!(p.status, Status::Unreachable { .. })
                                    && orig_status != "unreachable"
                                {
                                    lost_covers.push(orig_name.clone());
                                }
                                cover_results.push(json!({
                                    "name": orig_name,
                                    "before": orig_status,
                                    "after": new_status,
                                }));
                            }
                        }
                    }
                }

                // 4. Score verdict + Poka-Yoke next_step guidance.
                let cand_hard_fail = match kind.as_str() {
                    "assert" => {
                        !cand_ok
                            && matches!(
                                candidate_result["status"].as_str(),
                                Some("cex" | "error")
                            )
                            || cand_vacuous
                    }
                    "cover" => matches!(
                        candidate_result["status"].as_str(),
                        Some("unreachable" | "error")
                    ),
                    _ => false,
                };
                let verdict = if env_conflict
                    || dead_end_found
                    || !vacuous_assertions.is_empty()
                    || !lost_covers.is_empty()
                    || cand_hard_fail
                {
                    "reject"
                } else if !overconstraint_warnings.is_empty()
                    || (kind == "assert" && !cand_ok)
                    || (!target_meta.is_empty() && newly_proven.is_empty() && kind != "cover")
                {
                    "warn"
                } else {
                    "pass"
                };

                let ready_to_propose = verdict != "reject";
                let next_step = if env_conflict {
                    format!(
                        "REJECTED: assumption conflicts with the environment (allowed {:?}/{depth} steps; conflict_set: {:?}). Weaken or discard this assumption — do NOT call proposal_add.",
                        env_allowed, conflict_set
                    )
                } else if dead_end_found {
                    "REJECTED: 2QBF dead-end check found a trap state where no legal next input satisfies the assumption. Rewrite as a state-free or protocol-legal primary-input constraint.".into()
                } else if !lost_covers.is_empty() {
                    format!(
                        "REJECTED: assumption over-constrains the design and makes cover(s) [{}] unreachable. Weaken the assumption before proposing.",
                        lost_covers.join(", ")
                    )
                } else if !vacuous_assertions.is_empty() || cand_vacuous {
                    format!(
                        "REJECTED: vacuous proof detected (vacuous assertions: {:?}). Check antecedent reachability and environment constraints.",
                        vacuous_assertions
                    )
                } else if cand_hard_fail {
                    format!(
                        "REJECTED: candidate `{kind}` ended as `{}` (not valid on the RTL). Refine the expression using `cex_diagnose` or `design_source`.",
                        candidate_result["status"].as_str().unwrap_or("failed")
                    )
                } else if verdict == "warn" {
                    if !overconstraint_warnings.is_empty() {
                        "WARNING: candidate passed solver checks, but static hygiene flagged DUT output/internal signal usage in `assume`. Prefer constraining primary inputs only, or justify clearly in `proposal_add` rationale.".into()
                    } else {
                        "WARNING: candidate did not fail, but did not yet close the target assertions. Refine or combine with another helper lemma / abstraction before calling proposal_add.".into()
                    }
                } else {
                    format!(
                        "VALID: passed environment, dead-end, vacuity, and cover regression checks (newly_proven: {:?}). Safe to call proposal_add.",
                        newly_proven
                    )
                };

                Ok(json!({
                    "schema": "informal.validate/1",
                    "verdict": verdict,
                    "ready_to_propose": ready_to_propose,
                    "kind": kind,
                    "text": text,
                    "environment": {
                        "checked": kind == "assume",
                        "asked_depth": depth,
                        "allowed_steps": env_allowed,
                        "conflict": env_conflict,
                        "conflict_set": conflict_set,
                    },
                    "dead_end": dead_end_val,
                    "overconstraint_warnings": overconstraint_warnings,
                    "candidate_result": candidate_result,
                    "newly_proven": newly_proven,
                    "vacuous_assertions": vacuous_assertions,
                    "lost_covers": lost_covers,
                    "still_failing": still_failing,
                    "target_results": target_results,
                    "cover_results": cover_results,
                    "next_step": next_step,
                }))
            })();

            // Unconditionally clean up scratch task & restore session state.
            let _ = s.set_task(&prev_task);
            let _ = s.delete_task(&scratch);
            s.time_limit = prev_limit;
            s.environment = prev_env;
            s.sink = prev_sink;
            s.assumed_from_assert.truncate(prev_assumed_len);
            outcome
        })?
    }

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
            )
            .with_tool("proposal_validate")
            .with_hint(
                "Read-only servers still support cex_diagnose, convergence_diagnose, and zero-side-effect proposal_validate.",
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
    .with_tool("prove_status")
    .with_hint("Poll prove_status until idle, or call prove_stop to cancel the active run.")
}

fn no_trace_err(p: &Property) -> ApiError {
    let base = err(
        "no_trace",
        format!(
            "{} has no trace: its status is {} (a trace comes with cex or covered)",
            p.name,
            p.status.word()
        ),
    );
    match &p.status {
        Status::Unprocessed => base
            .with_tool("prove")
            .with_hint("Run prove on this property first; only cex or covered results generate a trace."),
        Status::Undetermined { .. } => base
            .with_tool("convergence_diagnose")
            .with_hint("This property did not converge; call convergence_diagnose to inspect complexity hotspots and abstraction/lemma options."),
        Status::Proven { .. } | Status::BoundedProven { .. } | Status::Unreachable { .. } => base
            .with_tool("property_get")
            .with_hint("This property is proved (no counterexample trace exists); use property_get for proof stats and vacuity."),
        _ => base
            .with_tool("property_get")
            .with_hint("Inspect property_get for status details."),
    }
}

/// What applying `p` would reject, without changing the session.
fn check(s: &Session, p: &Proposal) -> Result<(), ApiError> {
    match p.kind.as_str() {
        "command" => Ok(()),
        "cut_point" => match s.cut_point(&p.text) {
            Ok(cut) if s.stopats_of(&s.current_task).contains(&cut) => {
                Err(err("rejected", format!("`{cut}` is already a cut point"))
                    .with_tool("session_summary"))
            }
            Ok(_) => Ok(()),
            Err(msg) => {
                let sigs = diagnose::all_signal_paths(&s.design.hierarchy);
                let cands = diagnose::closest_matches(&p.text, sigs.iter().map(String::as_str), 3);
                Err(err("rejected", msg)
                    .with_tool("design_hierarchy")
                    .with_hint("Use a signal name from design_hierarchy or property_cone.")
                    .with_candidates(cands))
            }
        },
        _ => s.check_property(p.name.as_deref(), &p.text).map_err(|msg| {
            let sigs = diagnose::all_signal_paths(&s.design.hierarchy);
            let mut cands = Vec::new();
            for id in diagnose::expr_identifiers(&p.text) {
                for m in diagnose::closest_matches(&id, sigs.iter().map(String::as_str), 2) {
                    if !cands.contains(&m) {
                        cands.push(m);
                    }
                }
            }
            cands.truncate(4);
            err("rejected", msg)
                .with_tool("design_hierarchy")
                .with_hint("Check signal names against design_hierarchy and SVA operator syntax.")
                .with_candidates(cands)
        }),
    }
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
    s.props.iter().position(|p| p.name == name).ok_or_else(|| {
        let cands = diagnose::closest_matches(
            name,
            s.props.iter().map(|p| p.name.as_str()),
            3,
        );
        err("not_found", format!("no property named {name}"))
            .with_tool("property_list")
            .with_hint(
                "Call property_list to see valid property names (non-embedded tasks use `task::name`).",
            )
            .with_candidates(cands)
    })
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
        Status::BoundedProven { engine, bound, ms } => {
            v.insert("engine".into(), json!(engine));
            v.insert("depth".into(), json!(bound));
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
