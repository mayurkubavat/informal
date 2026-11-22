//! The tools behind the AI interface: each takes JSON arguments and gives
//! one versioned JSON reply about the FPV session, or changes it in the
//! way its mode allows (decision D17).

mod apps;
mod diagnose;
mod prove;
mod session;
mod spec_lint;

use crate::{diagnose as rtl_diag, spec};
use informal_engine::Engine;
use informal_fpv::{
    Flavor, Kind, Property, ProveProgress, ProveSink, Session, Shell, Status, Worker,
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

pub(super) fn err(code: &'static str, message: impl Into<String>) -> ApiError {
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
pub(super) struct Inner {
    pub(super) proposals: Vec<Proposal>,
    pub(super) next_id: u32,
    pub(super) log: VecDeque<String>,
    pub(super) progress: Option<ProveProgress>,
    hook: Option<ProgressHook>,
    changed: Option<ChangeHook>,
    /// The reply of the last prove started without waiting.
    pub(super) last_prove: Option<Reply>,
}

/// What the interface keeps beside the session: the proposals, the last
/// console lines and the progress of a running prove. It is the session's
/// [`ProveSink`]; a front end with its own sink forwards to it.
#[derive(Default)]
pub struct State {
    pub(super) inner: Mutex<Inner>,
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

    pub(super) fn is_busy(&self) -> bool {
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
    pub(super) state: Arc<State>,
    pub(super) opts: Options,
    pub(super) gui: OnceLock<Arc<dyn Gui>>,
}

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

    pub(super) fn changed(&self, tool: &str) {
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
            let (ins, outs, _) = rtl_diag::top_ports(&s.design.hierarchy);
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
    pub(super) fn on_shell<R: Send + 'static>(
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
    pub(super) fn below_root(&self, path: &str) -> Result<PathBuf, ApiError> {
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

    /// `Err` in read-only mode.
    pub(super) fn changing(&self) -> Result<(), ApiError> {
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
}

fn busy() -> ApiError {
    err(
        "busy",
        "a prove, a script or a Tcl command is running: wait for it, ask prove_status, or prove_stop",
    )
    .with_tool("prove_status")
    .with_hint("Poll prove_status until idle, or call prove_stop to cancel the active run.")
}

pub(super) fn no_trace_err(p: &Property) -> ApiError {
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

pub(super) fn find(s: &Session, name: &str) -> Result<usize, ApiError> {
    s.props.iter().position(|p| p.name == name).ok_or_else(|| {
        let cands = rtl_diag::closest_matches(
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

pub(super) fn origin_of(p: &Property) -> &'static str {
    match (p.ai, p.rtl) {
        (true, _) => "ai",
        (_, true) => "rtl",
        _ => "user",
    }
}

/// One property as every reply gives it. `full` adds each engine's run.
pub(super) fn prop_view(p: &Property, full: bool) -> Value {
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

/// How many properties have each status word.
pub(super) fn counts<'a>(props: impl Iterator<Item = &'a Property>) -> Value {
    let mut m = Map::new();
    for p in props {
        let n = m.entry(p.status.word()).or_insert(json!(0));
        *n = json!(n.as_u64().unwrap_or(0) + 1);
    }
    Value::Object(m)
}

// ------------------------------------------------------------ arguments

pub(super) fn wrong(key: &str, want: &str) -> ApiError {
    err("arguments", format!("`{key}` must be {want}"))
}

pub(super) fn opt_str(args: &Value, key: &str) -> Result<Option<String>, ApiError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(wrong(key, "a string")),
    }
}

pub(super) fn req_str(args: &Value, key: &str) -> Result<String, ApiError> {
    opt_str(args, key)?
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| err("arguments", format!("`{key}` is required")))
}

pub(super) fn opt_u64(args: &Value, key: &str) -> Result<Option<u64>, ApiError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| wrong(key, "a whole number, 0 or more")),
    }
}

pub(super) fn opt_f64(args: &Value, key: &str) -> Result<Option<f64>, ApiError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().map(Some).ok_or_else(|| wrong(key, "a number")),
    }
}

pub(super) fn opt_bool(args: &Value, key: &str) -> Result<Option<bool>, ApiError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| wrong(key, "true or false")),
    }
}

/// A list of strings; a single string counts as a list of one.
pub(super) fn strings(args: &Value, key: &str) -> Result<Vec<String>, ApiError> {
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
