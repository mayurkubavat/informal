//! An FPV session: the design setup, clocks, reset and properties that
//! JasperGold and VC Formal scripts build up command by command, and the
//! prove step that turns them into engine runs.
//!
//! Elaboration runs svlang (the pure-Rust SystemVerilog front end, crate
//! `informal-svlang`) over the design: SystemVerilog errors come back with
//! file and line, the top module is taken from it, and property expressions
//! are checked against the names the top module declares. svlang lowers the
//! whole design to one Btor2 model (decision D14), binding each property
//! given by command in the top module's scope, so it sees every signal
//! there, and compiling it with the properties written in the RTL; the
//! reset is part of the model too. Each assertion or cover gets its own
//! copy with only its target line and all assumptions, and runs through
//! the engine portfolio on its own.

use crate::progress::{ProveSink, Tracker};
use anyhow::{anyhow, bail};
use informal_engine::portfolio::EngineRun;
use informal_engine::{portfolio, CheckRequest, Engine, ModelFormat, Progress, Verdict};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use crate::trace;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Assert,
    Assume,
    Cover,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Assert => "assert",
            Kind::Assume => "assume",
            Kind::Cover => "cover",
        }
    }
}

/// Name of the root proof task that holds RTL properties and any command
/// property added without `-task` (until `task -set` selects another).
pub const EMBEDDED_TASK: &str = "<embedded>";

fn default_task() -> String {
    EMBEDDED_TASK.to_string()
}

fn is_default_task(task: &str) -> bool {
    task == EMBEDDED_TASK
}

/// Result of proving one property, in the vendor tools' vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Status {
    Unprocessed,
    Proven {
        engine: String,
        ms: u128,
        /// True when the assertion's precondition is unreachable or the
        /// active assumptions allow no trace past reset.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        vacuous: bool,
    },
    Cex {
        engine: String,
        depth: Option<u32>,
        ms: u128,
    },
    Covered {
        engine: String,
        depth: Option<u32>,
        ms: u128,
    },
    Unreachable {
        engine: String,
        ms: u128,
    },
    Undetermined {
        reason: String,
    },
    Error {
        message: String,
    },
}

impl Status {
    /// The short word JasperGold prints and `-include {status ...}` filters on.
    pub fn word(&self) -> &'static str {
        match self {
            Status::Unprocessed => "unprocessed",
            Status::Proven { .. } => "proven",
            Status::Cex { .. } => "cex",
            Status::Covered { .. } => "covered",
            Status::Unreachable { .. } => "unreachable",
            Status::Undetermined { .. } => "undetermined",
            Status::Error { .. } => "error",
        }
    }

    /// True when an assertion was proved vacuously.
    pub fn is_vacuous(&self) -> bool {
        matches!(self, Status::Proven { vacuous: true, .. })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Property {
    pub name: String,
    pub kind: Kind,
    pub expr: String,
    pub enabled: bool,
    pub status: Status,
    /// The VCD written for a CEX or cover (`<workdir>/<name>.vcd`, relative
    /// to the working directory when inside it); `None` otherwise or when
    /// no witness could be replayed. See docs/ui/trace-format.md.
    pub trace: Option<String>,
    /// Where the property was written: the script line of the Tcl command
    /// that created it, when that ran from a sourced file. `None` for
    /// properties typed at the console.
    pub source: Option<SourceLoc>,
    /// Written in the RTL (`assert property` inside a module) and found at
    /// elaboration, not created by a command. `name` is then its full
    /// hierarchical name, `expr` is its text as written there (empty when
    /// that cannot be read), and `source` is where it stands in the design.
    pub rtl: bool,
    /// The last prove's per-engine record: each portfolio engine's outcome
    /// (won/lost/timeout/killed/error), time and depth reached. Empty
    /// before a prove, or when no engine ran (elaboration, errors).
    pub engines: Vec<EngineRun>,
    /// Added through the AI interface (decision D17), not by the user's
    /// script or console: tagged in every listing, and removable in one
    /// step ([`Session::remove_ai_property`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ai: bool,
    /// The AI-added changes that were in the model of the last prove:
    /// names of AI-added assumptions, and AI-added cut points as
    /// `stopat <signal>`. A status found under them is reset when one of
    /// them is removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub under_ai: Vec<String>,
    /// Proof task this property belongs to (`"<embedded>"` by default).
    #[serde(default = "default_task", skip_serializing_if = "is_default_task")]
    pub task: String,
}

/// An isolated proof task created with `task -create`. Assumptions and cut
/// points added to a task apply only when proving targets in that task, never
/// leaking into `<embedded>` or sibling tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Task {
    pub name: String,
    /// Task-scoped cut points (`stopat -task <name>`), relative to `top`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stopats: Vec<String>,
    /// Inherit `<embedded>` command assumptions and cut points.
    pub inherit_embedded: bool,
}

/// A file and 1-based line (absolute path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceLoc {
    pub file: String,
    pub line: u32,
}

impl SourceLoc {
    /// Absolute form of a path as Tcl saw it (relative to the process's
    /// working directory).
    pub fn new(file: &str, line: u32) -> Self {
        let file = std::path::absolute(file)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| file.to_string());
        SourceLoc { file, line }
    }

    /// Fallback when Tcl has no line for the command (it was built with
    /// `eval` or `uplevel` from a string): the first line of `file` that
    /// mentions the property name as a whole word.
    pub fn search(file: &str, name: &str) -> Option<Self> {
        let text = std::fs::read_to_string(file).ok()?;
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let line = text.lines().position(|l| {
            l.match_indices(name).any(|(i, _)| {
                !l[..i].chars().next_back().is_some_and(is_word)
                    && !l[i + name.len()..].chars().next().is_some_and(is_word)
            })
        })?;
        Some(SourceLoc::new(file, line as u32 + 1))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Clock {
    pub signal: String,
    pub edge: &'static str,
    /// The clock's period, in periods of the fastest clock (`clock
    /// -factor`, 1 by default). Used when the design has several clocks.
    pub factor: u32,
    /// Model steps (half periods of the fastest clock) the clock's wave is
    /// moved later (`clock -phase`, 0 by default: every clock starts low
    /// and rises together for the first time).
    pub phase: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Design {
    pub files: Vec<PathBuf>,
    pub defines: Vec<(String, Option<String>)>,
    pub incdirs: Vec<PathBuf>,
    pub top: Option<String>,
    pub params: Vec<(String, String)>,
    /// Black boxes by module name (`elaborate -bbox_m`): instances of these
    /// keep their ports, nothing inside them is modelled, and their outputs
    /// are free on every step.
    pub bbox: Vec<String>,
    /// Black boxes by instance path from the top module (`-bbox_i`).
    pub bbox_instances: Vec<String>,
    /// Cut points (`stopat`), as paths from the top module (`u_core.sig`):
    /// each is cut from its drivers and free on every step.
    pub stopats: Vec<String>,
    /// The cut points among `stopats` that were added through the AI
    /// interface (decision D17).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ai_stopats: Vec<String>,
    /// `set_lowering skip_initial_checks on`: time-zero checks of signals in
    /// `initial` blocks are left out of the model, not reported as
    /// unsupported.
    pub skip_initial_checks: bool,
    /// `set_lowering isunknown_is_false on`: `$isunknown(x)` is 0 in the
    /// model, not reported as unsupported.
    pub isunknown_is_false: bool,
    /// The scope tree after elaboration: instances with their ports and
    /// named signals (the app's Design browser). Also written to
    /// `<workdir>/design.hierarchy.json`.
    pub hierarchy: Vec<informal_svlang::Scope>,
}

#[derive(Serialize)]
pub struct Session {
    pub design: Design,
    pub elaborated: bool,
    pub clocks: Vec<Clock>,
    /// Active-high "in reset" condition, held for the first cycle only.
    pub reset: Option<String>,
    pub props: Vec<Property>,
    pub time_limit: Duration,
    /// Engine names to use; empty means every loaded engine.
    pub engine_filter: Vec<String>,
    pub workdir: PathBuf,
    #[serde(skip)]
    pub engines: Vec<Arc<dyn Engine>>,
    /// slang's view of the design, after elaboration (when linked).
    #[serde(skip)]
    pub elab: Option<informal_svlang::Design>,
    /// Instance tree from slang, for the UI.
    pub hierarchy: Vec<informal_svlang::Instance>,
    /// Front end that built the last prove's models (`svlang` or `slang`).
    pub frontend: String,
    /// Active task for commands without `-task` (`"<embedded>"` by default).
    #[serde(default = "default_task", skip_serializing_if = "is_default_task")]
    pub current_task: String,
    /// User-created proof tasks (`task -create`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<Task>,
    /// Set when the last `prove` or `check_assumptions` found that the active
    /// assumptions allow no trace past reset.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub assumption_conflict: bool,
    /// Set from another thread (the app's Stop) to end a running prove:
    /// checked between properties and forwarded to the engine portfolio,
    /// whose external engines then kill their process groups. Cleared when
    /// a prove starts.
    #[serde(skip)]
    pub stop: Arc<AtomicBool>,
    /// Script check mode (`informal shell --check`): commands are parsed and
    /// recorded, but nothing is elaborated or proved.
    #[serde(skip)]
    pub dry_run: bool,
    /// Receives live progress while a prove runs (the desktop app).
    #[serde(skip)]
    pub sink: Option<Arc<dyn ProveSink>>,
    /// BMC re-runs made only to get a trace (the winner gave no usable
    /// witness). Tests and diagnostics read it.
    #[serde(skip)]
    pub trace_reruns: Arc<std::sync::atomic::AtomicUsize>,
    /// Where each settled property's certificate is written for an outside
    /// checker (`informal shell --certificate`; docs/certificates.md).
    #[serde(skip)]
    pub certificates: Option<PathBuf>,
}

impl Session {
    pub fn new(engines: Vec<Arc<dyn Engine>>, workdir: PathBuf) -> Self {
        // Absolute, so that paths written into it stay valid.
        let workdir = std::path::absolute(&workdir).unwrap_or(workdir);
        Session {
            design: Design::default(),
            elaborated: false,
            clocks: Vec::new(),
            reset: None,
            props: Vec::new(),
            time_limit: Duration::from_secs(3600),
            engine_filter: Vec::new(),
            workdir,
            engines,
            elab: None,
            hierarchy: Vec::new(),
            frontend: String::new(),
            current_task: default_task(),
            tasks: Vec::new(),
            assumption_conflict: false,
            stop: Arc::default(),
            dry_run: false,
            sink: None,
            trace_reruns: Arc::default(),
            certificates: None,
        }
    }

    /// `clear -all`: forget the design and properties, keep engines (and
    /// the front end's stop flag and progress sink).
    pub fn clear(&mut self) {
        let engines = std::mem::take(&mut self.engines);
        let workdir = std::mem::take(&mut self.workdir);
        let (stop, sink, dry_run) = (self.stop.clone(), self.sink.take(), self.dry_run);
        let certificates = self.certificates.take();
        *self = Session::new(engines, workdir);
        self.certificates = certificates;
        self.dry_run = dry_run;
        self.stop = stop;
        self.sink = sink;
    }

    pub fn has_task(&self, name: &str) -> bool {
        name == EMBEDDED_TASK || self.tasks.iter().any(|t| t.name == name)
    }

    pub fn ensure_task(&self, name: &str) -> Result<(), String> {
        if self.has_task(name) {
            Ok(())
        } else {
            Err(format!(
                "unknown task `{name}` (create it first with `task -create {name}`)"
            ))
        }
    }

    pub fn task_names(&self) -> Vec<String> {
        std::iter::once(EMBEDDED_TASK.to_string())
            .chain(self.tasks.iter().map(|t| t.name.clone()))
            .collect()
    }

    pub fn set_current_task(&mut self, name: &str) -> Result<(), String> {
        self.ensure_task(name)?;
        self.current_task = name.to_string();
        Ok(())
    }

    pub fn create_task(
        &mut self,
        name: &str,
        copy_from: Option<&str>,
        isolated: bool,
        set_current: bool,
    ) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("task name cannot be empty".into());
        }
        if self.has_task(name) {
            return Err(format!("task `{name}` already exists"));
        }
        let (stopats, inherit_embedded) = match copy_from {
            None => (Vec::new(), !isolated),
            Some(src) => {
                self.ensure_task(src)?;
                if src == EMBEDDED_TASK {
                    (self.design.stopats.clone(), !isolated)
                } else {
                    let t = self.tasks.iter().find(|t| t.name == src).unwrap();
                    (t.stopats.clone(), !isolated && t.inherit_embedded)
                }
            }
        };
        if let Some(src) = copy_from {
            let copied: Vec<Property> = self
                .props
                .iter()
                .filter(|p| !p.rtl && p.task == src)
                .map(|p| {
                    let local = p.name.rsplit("::").next().unwrap_or(&p.name);
                    let mut c = p.clone();
                    c.name = format!("{name}::{local}");
                    c.task = name.to_string();
                    c.status = Status::Unprocessed;
                    c.trace = None;
                    c.engines.clear();
                    c.under_ai.clear();
                    c
                })
                .collect();
            self.props.extend(copied);
        }
        self.tasks.push(Task {
            name: name.to_string(),
            stopats,
            inherit_embedded,
        });
        if set_current {
            self.current_task = name.to_string();
        }
        Ok(())
    }

    pub fn add_property(
        &mut self,
        kind: Kind,
        name: Option<String>,
        expr: &str,
    ) -> Result<String, String> {
        let task = self.current_task.clone();
        self.add_property_in_task(kind, name, expr, &task)
    }

    pub fn add_property_in_task(
        &mut self,
        kind: Kind,
        name: Option<String>,
        expr: &str,
        task: &str,
    ) -> Result<String, String> {
        self.ensure_task(task)?;
        // Reject up front what the front end's property compiler is known
        // not to take, and names the design does not have. The front end
        // binds the property at prove and has the last word on both.
        check_operators(expr)?;
        self.check_names(expr)?;
        let name = name.unwrap_or_else(|| {
            let n = self.props.iter().filter(|p| p.kind == kind).count();
            format!("{}_{}", kind.label(), n)
        });
        if self.props.iter().any(|p| p.name == name) {
            return Err(format!("property `{name}` already exists"));
        }
        self.props.push(Property {
            name: name.clone(),
            kind,
            expr: expr.to_string(),
            enabled: true,
            status: Status::Unprocessed,
            trace: None,
            source: None,
            rtl: false,
            engines: Vec::new(),
            ai: false,
            under_ai: Vec::new(),
            task: task.to_string(),
        });
        Ok(name)
    }

    /// What [`Self::add_property`] would reject, without adding anything.
    pub fn check_property(&self, name: Option<&str>, expr: &str) -> Result<(), String> {
        check_operators(expr)?;
        self.check_names(expr)?;
        match name {
            Some(n) if self.props.iter().any(|p| p.name == n) => {
                Err(format!("property `{n}` already exists"))
            }
            _ => Ok(()),
        }
    }

    /// Add a property through the AI interface: as [`Self::add_property`],
    /// tagged AI-added and written to the log.
    pub fn add_ai_property(
        &mut self,
        kind: Kind,
        name: Option<String>,
        expr: &str,
    ) -> Result<String, String> {
        let name = self.add_property(kind, name, expr)?;
        if let Some(p) = self.props.iter_mut().find(|p| p.name == name) {
            p.ai = true;
        }
        self.log(&format!("AI-added {} {name}: {expr}", kind.label()));
        Ok(name)
    }

    /// Remove an AI-added property. Returns the properties whose status
    /// was found with it in the model (an assumption) and is now reset.
    pub fn remove_ai_property(&mut self, name: &str) -> Result<Vec<String>, String> {
        let at = self
            .props
            .iter()
            .position(|p| p.name == name && p.ai)
            .ok_or_else(|| format!("no AI-added property `{name}`"))?;
        self.props.remove(at);
        self.log(&format!("AI-added property {name} removed"));
        Ok(self.reset_under(name))
    }

    /// Cut a signal from its drivers through the AI interface: as
    /// `stopat`, tagged AI-added and written to the log. Returns the cut
    /// point as the session lists it.
    pub fn add_ai_cut_point(&mut self, signal: &str) -> Result<String, String> {
        let cut = self.cut_point(signal)?;
        if self.design.stopats.contains(&cut) {
            return Err(format!("`{cut}` is already a cut point"));
        }
        self.design.stopats.push(cut.clone());
        self.design.ai_stopats.push(cut.clone());
        self.log(&format!("AI-added cut point (stopat) {cut}"));
        Ok(cut)
    }

    /// Remove an AI-added cut point. Returns the properties whose status
    /// was found with it in the model and is now reset.
    pub fn remove_ai_cut_point(&mut self, cut: &str) -> Result<Vec<String>, String> {
        if !self.design.ai_stopats.iter().any(|c| c == cut) {
            return Err(format!("no AI-added cut point `{cut}`"));
        }
        self.design.ai_stopats.retain(|c| c != cut);
        self.design.stopats.retain(|c| c != cut);
        self.log(&format!("AI-added cut point {cut} removed"));
        Ok(self.reset_under(&format!("stopat {cut}")))
    }

    /// Back to unprocessed: every property whose last prove had the
    /// AI-added change `item` in its model.
    fn reset_under(&mut self, item: &str) -> Vec<String> {
        let mut reset = Vec::new();
        for p in &mut self.props {
            if p.under_ai.iter().any(|u| u == item) {
                p.status = Status::Unprocessed;
                p.trace = None;
                p.engines.clear();
                p.under_ai.clear();
                reset.push(p.name.clone());
            }
        }
        reset
    }

    /// The AI-added changes a prove started now would have in its model
    /// (see [`Property::under_ai`]).
    pub fn ai_in_force(&self) -> Vec<String> {
        self.ai_in_force_for_task(EMBEDDED_TASK)
    }

    pub fn ai_in_force_for_task(&self, task: &str) -> Vec<String> {
        let inherit = self.task_inherits_embedded(task);
        self.props
            .iter()
            .filter(|p| {
                p.ai && p.enabled
                    && p.kind == Kind::Assume
                    && (p.task == task || (inherit && p.task == EMBEDDED_TASK))
            })
            .map(|p| p.name.clone())
            .chain(
                self.design
                    .ai_stopats
                    .iter()
                    .filter(|_| task == EMBEDDED_TASK || inherit)
                    .map(|c| format!("stopat {c}")),
            )
            .collect()
    }

    fn task_inherits_embedded(&self, task: &str) -> bool {
        task == EMBEDDED_TASK
            || self
                .tasks
                .iter()
                .find(|t| t.name == task)
                .is_none_or(|t| t.inherit_embedded)
    }

    /// Cut points listed in `task` itself (not counting inherited ones).
    pub fn stopats_in_task(&self, task: &str) -> Result<Vec<String>, String> {
        self.ensure_task(task)?;
        if task == EMBEDDED_TASK {
            Ok(self.design.stopats.clone())
        } else {
            Ok(self
                .tasks
                .iter()
                .find(|t| t.name == task)
                .map(|t| t.stopats.clone())
                .unwrap_or_default())
        }
    }

    pub fn add_stopats_in_task(&mut self, task: &str, names: &[String]) -> Result<(), String> {
        self.ensure_task(task)?;
        let dst = if task == EMBEDDED_TASK {
            &mut self.design.stopats
        } else {
            &mut self
                .tasks
                .iter_mut()
                .find(|t| t.name == task)
                .unwrap()
                .stopats
        };
        for n in names {
            if !dst.contains(n) {
                dst.push(n.clone());
            }
        }
        Ok(())
    }

    pub fn clear_stopats_in_task(&mut self, task: &str, names: &[String]) -> Result<(), String> {
        self.ensure_task(task)?;
        let dst = if task == EMBEDDED_TASK {
            &mut self.design.stopats
        } else {
            &mut self
                .tasks
                .iter_mut()
                .find(|t| t.name == task)
                .unwrap()
                .stopats
        };
        if names.is_empty() {
            dst.clear();
        } else {
            dst.retain(|x| !names.contains(x));
        }
        Ok(())
    }

    /// The Btor2 model property `i` is proved on: the whole design with
    /// every assumption of property `i`'s task and this one target. Lowers
    /// the design.
    pub fn model_for(&self, i: usize) -> Result<String, String> {
        let task = &self.props[i].task;
        let lowered = self.lower_for_task(task, false)?;
        let keep = target_name(&self.props[i].name);
        if !has_target(&lowered.btor2, &keep) {
            return Err(format!("no check named {keep} in the model"));
        }
        let (model, _) = informal_svlang::split_trace(&lowered.btor2);
        Ok(only_target(model, &keep))
    }

    /// With slang's view of the design, reject names the top module does
    /// not declare (typos would otherwise become implicit nets that make a
    /// property vacuous).
    fn check_names(&self, expr: &str) -> Result<(), String> {
        if let (Some(d), Some(top)) = (&self.elab, &self.design.top) {
            let unknown = d.unknown_names(top, expr);
            if !unknown.is_empty() {
                return Err(format!(
                    "unknown name(s) in module {top}: {}",
                    unknown.join(", ")
                ));
            }
        }
        Ok(())
    }

    /// With slang's view of the design, reject a reset expression or clock
    /// that names a signal the top module does not declare. (Unchecked, a
    /// typo would leave the real reset free and give a wrong verdict.)
    pub(crate) fn check_reset_and_clocks(&self) -> Result<(), String> {
        if let Some(r) = &self.reset {
            self.check_names(r)
                .map_err(|e| format!("reset {{{r}}}: {e}"))?;
        }
        for c in &self.clocks {
            self.check_names(&c.signal)
                .map_err(|e| format!("clock {}: {e}", c.signal))?;
        }
        Ok(())
    }

    /// `stopat`: the path of a signal to cut, from the top module (a
    /// leading `<top>.` is accepted). After elaboration the name must be a
    /// signal of the design; before it, the check happens at prove.
    pub fn cut_point(&self, name: &str) -> Result<String, String> {
        let top = self.design.top.as_deref().unwrap_or("");
        let rel = name
            .strip_prefix(top)
            .and_then(|r| r.strip_prefix('.'))
            .filter(|_| !top.is_empty())
            .unwrap_or(name);
        if let (Some(d), false) = (&self.elab, top.is_empty()) {
            let full = format!("{top}.{rel}");
            // A whole signal, or a part of one (`pkt.valid`, `mem[3]`).
            let known = d.signals.iter().filter(|s| !s.constant).any(|s| {
                full.strip_prefix(s.path.as_str())
                    .is_some_and(|r| r.is_empty() || r.starts_with(['.', '[']))
            });
            if !known {
                return Err(format!("`{name}` is not a signal of {top}"));
            }
        }
        Ok(rel.to_string())
    }

    /// The cut points of `task` as the front end wants them: full paths.
    fn cut_points_for_task(&self, task: &str) -> Vec<String> {
        let top = self.design.top.as_deref().unwrap_or("");
        let mut rels: Vec<&str> = Vec::new();
        if self.task_inherits_embedded(task) {
            for s in &self.design.stopats {
                if !rels.contains(&s.as_str()) {
                    rels.push(s.as_str());
                }
            }
        }
        if task != EMBEDDED_TASK {
            if let Some(t) = self.tasks.iter().find(|t| t.name == task) {
                for s in &t.stopats {
                    if !rels.contains(&s.as_str()) {
                        rels.push(s.as_str());
                    }
                }
            }
        }
        rels.into_iter().map(|s| format!("{top}.{s}")).collect()
    }

    /// Names matching a Tcl-style glob (`*`, `?`).
    pub fn matching(&self, pattern: &str) -> Vec<usize> {
        self.props
            .iter()
            .enumerate()
            .filter(|(_, p)| glob(pattern, &p.name))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn elaborate(&mut self, top: Option<String>) -> anyhow::Result<()> {
        if let Some(t) = top {
            self.design.top = Some(t);
        }
        if self.dry_run {
            // Check mode: the design is not read (its files may not exist here).
            self.elaborated = true;
            return Ok(());
        }
        if self.design.files.is_empty() {
            bail!("no design files: run analyze / read_file first");
        }
        let req = informal_svlang::Request {
            files: self.design.files.clone(),
            defines: self.design.defines.clone(),
            incdirs: self.design.incdirs.clone(),
            top: self.design.top.clone(),
            params: self.design.params.clone(),
            trace_outputs: false,
            skip_initial_checks: self.design.skip_initial_checks,
            isunknown_is_false: self.design.isunknown_is_false,
            ..Default::default()
        };
        self.design.hierarchy.clear();
        let d = informal_svlang::compile(&req);
        let errors: Vec<String> = d.errors().take(20).map(|e| e.to_string()).collect();
        if !errors.is_empty() {
            bail!("SystemVerilog errors:\n{}", errors.join("\n"));
        }
        if self.design.top.is_none() {
            match d.tops.as_slice() {
                [t] => self.design.top = Some(t.clone()),
                tops => bail!("cannot tell the top module (candidates: {tops:?}); use -top"),
            }
        }
        self.hierarchy = d.instances.clone();
        self.design.hierarchy = d.hierarchy.clone();
        // A design with no model lists no properties of its own. Say so here,
        // with the reasons: an empty table must not read as "nothing to
        // prove". A property given by command gets the same reasons as its
        // error, and a prove with nothing to run fails with them.
        if !d.not_lowered.is_empty() {
            self.log("NOT MODELLED: the design does not lower, so its own properties are not listed and nothing can be proved until this is resolved:");
            for line in d.not_lowered.iter().take(20) {
                self.log(&format!("  {line}"));
            }
        }
        self.rtl_properties(&d.properties);
        if !d.unlisted_checks.is_empty() {
            self.log(&format!(
                "NOT CHECKED: {} immediate assertion(s) or cover(s) in the RTL are not in the property table (give each a label of its own, `name: assert (...)`):",
                d.unlisted_checks.len()
            ));
            for line in d.unlisted_checks.iter().take(20) {
                self.log(&format!("  {line}"));
            }
            if d.unlisted_checks.len() > 20 {
                self.log(&format!("  ... and {} more", d.unlisted_checks.len() - 20));
            }
        }
        self.elab = Some(d);
        std::fs::create_dir_all(&self.workdir)?;
        let tree = self.workdir.join("design.hierarchy.json");
        if self.design.hierarchy.is_empty() {
            let _ = std::fs::remove_file(&tree);
        } else {
            std::fs::write(&tree, serde_json::to_string_pretty(&self.design.hierarchy)?)?;
        }
        self.check_black_boxes()?;
        self.elaborated = true;
        Ok(())
    }

    /// List the concurrent properties written in the RTL beside the ones
    /// commands created, replacing those of an earlier elaboration.
    fn rtl_properties(&mut self, found: &[informal_svlang::RtlProperty]) {
        self.props.retain(|p| !p.rtl);
        for f in found {
            if self.props.iter().any(|p| p.name == f.name) {
                self.log(&format!(
                    "{} in the RTL is not listed: a property of that name exists",
                    f.name
                ));
                continue;
            }
            self.props.push(Property {
                name: f.name.clone(),
                kind: match f.kind.as_str() {
                    "assume" => Kind::Assume,
                    "cover" => Kind::Cover,
                    _ => Kind::Assert,
                },
                // As written in the design, for the reader: the model has
                // its own compiled check.
                expr: f.text.clone().unwrap_or_default(),
                enabled: true,
                status: Status::Unprocessed,
                trace: None,
                source: f.file.as_ref().map(|file| SourceLoc {
                    file: file.clone(),
                    line: f.line.unwrap_or(0),
                }),
                rtl: true,
                engines: Vec::new(),
                ai: false,
                under_ai: Vec::new(),
                task: default_task(),
            });
        }
    }

    /// Match the requested black boxes against the elaborated design. A
    /// module with no instance here is dropped with a note (flow scripts
    /// list boxes for every configuration); an instance path that does not
    /// exist, or the top module, is an error.
    fn check_black_boxes(&mut self) -> anyhow::Result<()> {
        let (Some(d), Some(top)) = (&self.elab, self.design.top.clone()) else {
            return Ok(());
        };
        if self.design.bbox.contains(&top) {
            bail!("the top module {top} cannot be a black box");
        }
        let (used, unused): (Vec<String>, Vec<String>) = std::mem::take(&mut self.design.bbox)
            .into_iter()
            .partition(|m| d.instances.iter().any(|i| &i.module == m));
        self.design.bbox = used;
        let mut paths = Vec::new();
        for p in std::mem::take(&mut self.design.bbox_instances) {
            let rel = p
                .strip_prefix(top.as_str())
                .and_then(|r| r.strip_prefix('.'))
                .unwrap_or(&p)
                .to_string();
            if !d.instances.iter().any(|i| i.path == format!("{top}.{rel}")) {
                bail!("black box `{p}` is not an instance of {top}");
            }
            paths.push(rel);
        }
        self.design.bbox_instances = paths;
        for m in unused {
            self.log(&format!(
                "black box {m}: the design has no instance of that module (ignored)"
            ));
        }
        Ok(())
    }

    /// How long the reset lasts in a model that drives the clocks (a design
    /// on both clock edges, several clocks or computed clocks), where one
    /// model step is half a period of a `-factor 1` clock: until every
    /// declared clock has had a whole period, and then on to the first
    /// clock's next rising edge, so that registers on any clock and edge
    /// see the reset and the properties start with a clock cycle. With it,
    /// for each declared clock and edge, how many of its edges fall in the
    /// reset: a property on that clock and edge skips those attempts.
    fn tick_reset(&self) -> informal_svlang::TickReset {
        if self.reset.is_none() {
            return informal_svlang::TickReset::default();
        }
        // With no clock declared (a property names its own), the one clock
        // in use toggles on every step.
        let default = Clock {
            signal: String::new(),
            edge: "posedge",
            factor: 1,
            phase: 0,
        };
        let c0 = self.clocks.first().unwrap_or(&default);
        // Clock c first completes a period at step phase + 2 * factor, and
        // the first clock's n-th rising edge ends step phase0 + factor0 - 1
        // + (n - 1) * period0.
        let whole = |c: &Clock| c.phase + 2 * c.factor;
        let window = self.clocks.iter().map(whole).max().unwrap_or(whole(c0));
        let rest = window.saturating_sub(c0.phase + c0.factor);
        let cycles = rest.div_ceil(2 * c0.factor) + 1;
        let distinct = self.clocks.iter().any(|c| c.signal != c0.signal);
        // One clock: the reset ends with its first whole period, as soon as
        // its falling edge has passed.
        let steps = match distinct {
            true => c0.phase + c0.factor + (cycles - 1) * 2 * c0.factor,
            false => window,
        };
        let mut by_clock = Vec::new();
        for c in &self.clocks {
            let period = 2 * c.factor;
            let shift = (period - c.phase % period) % period;
            for (posedge, last) in [(true, c.factor - 1), (false, period - 1)] {
                let edges = (0..steps).filter(|t| (t + shift) % period == last).count();
                by_clock.push((c.signal.clone(), posedge, edges as u32));
            }
        }
        informal_svlang::TickReset {
            steps,
            cycles: if distinct { cycles } else { 1 },
            by_clock,
        }
    }

    fn engines(&self) -> Vec<Arc<dyn Engine>> {
        let picked: Vec<_> = self
            .engines
            .iter()
            .filter(|e| {
                self.engine_filter.is_empty() || self.engine_filter.iter().any(|n| n == e.name())
            })
            .filter(|e| e.capabilities().formats.contains(&ModelFormat::Btor2))
            .cloned()
            .collect();
        if picked.is_empty() {
            // set_engines rejects unknown names, so this is a filter whose
            // engines cannot read Btor2: say so rather than switch quietly.
            if !self.engine_filter.is_empty() {
                self.log(&format!(
                    "none of the selected engines ({}) reads Btor2; using all engines",
                    self.engine_filter.join(", ")
                ));
            }
            return self.engines.clone();
        }
        picked
    }

    /// Names of the engines this session can run.
    pub fn engine_names(&self) -> Vec<String> {
        self.engines.iter().map(|e| e.name().to_string()).collect()
    }

    /// Prove the selected assertions and covers (all enabled ones when
    /// `which` is empty). Returns their indices.
    ///
    /// Stop (`self.stop`) ends the run: the running property's portfolio is
    /// cancelled, and it and every property not reached yet become
    /// undetermined with reason "stopped". Elaboration and model building
    /// (svlang) are not interrupted; a Stop during them takes effect
    /// at the first engine run.
    pub fn prove(&mut self, which: &[usize]) -> anyhow::Result<Vec<usize>> {
        if self.dry_run {
            return Ok(Vec::new());
        }
        if !self.elaborated {
            self.elaborate(None)?;
        }
        self.check_reset_and_clocks().map_err(|e| anyhow!(e))?;
        let targets: Vec<usize> = if which.is_empty() {
            (0..self.props.len())
                .filter(|&i| self.props[i].enabled && self.props[i].kind != Kind::Assume)
                .collect()
        } else {
            which
                .iter()
                .copied()
                .filter(|&i| self.props[i].kind != Kind::Assume)
                .collect()
        };
        if targets.is_empty() {
            if let Some(why) = self
                .elab
                .as_ref()
                .map(|d| &d.not_lowered)
                .filter(|w| !w.is_empty())
            {
                bail!(
                    "nothing was proved: the design cannot be modelled\n{}",
                    why.iter().take(20).cloned().collect::<Vec<_>>().join("\n")
                );
            }
        }
        let tracker = self.sink.clone().map(|sink| {
            Tracker::start(
                sink,
                targets
                    .iter()
                    .map(|&i| (self.props[i].name.clone(), self.props[i].kind.label())),
            )
        });
        let engines = self.engines();
        // Lower one isolated model per distinct task present in `targets` so
        // task-scoped assumptions and cut points never leak across tasks.
        let mut distinct_tasks: Vec<String> = Vec::new();
        for &i in &targets {
            let t = &self.props[i].task;
            if !distinct_tasks.iter().any(|x| x == t) {
                distinct_tasks.push(t.clone());
            }
        }
        if distinct_tasks.is_empty() {
            distinct_tasks.push(self.current_task.clone());
        }
        let mut by_task: std::collections::HashMap<
            String,
            Result<informal_svlang::Lowered, String>,
        > = Default::default();
        for t in &distinct_tasks {
            let under_ai = self.ai_in_force_for_task(t);
            for &i in &targets {
                if &self.props[i].task == t {
                    self.props[i].under_ai = under_ai.clone();
                }
            }
            by_task.insert(t.clone(), self.lower_for_task(t, !engines.is_empty()));
        }
        self.frontend = "svlang".into();
        self.assumption_conflict = false;

        // Check once per task whether the active assumptions contradict each
        // other (no trace survives past reset).
        let mut conflict_tasks: std::collections::HashSet<String> = Default::default();
        if !engines.is_empty() {
            for t in &distinct_tasks {
                if self.stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(l) = &by_task[t] {
                    let (model, _) = informal_svlang::split_trace(&l.btor2);
                    let constraints = model
                        .lines()
                        .filter(|ln| ln.split_whitespace().nth(1) == Some("constraint"))
                        .count();
                    if constraints > usize::from(self.reset.is_some()) {
                        let min_steps = if l.half_cycle && self.reset.is_some() {
                            self.tick_reset().steps + 4
                        } else {
                            u32::from(self.reset.is_some()) + 4
                        };
                        if let Some(alive_btor) = assume_alive_btor2(model, min_steps) {
                            let path = self.workdir.join("assume_consistency.btor2");
                            if std::fs::write(&path, alive_btor).is_ok() {
                                let v = self.check_aux_reachability(
                                    &engines,
                                    &path,
                                    Some(min_steps as usize),
                                );
                                let _ = std::fs::remove_file(&path);
                                if matches!(v, Verdict::Safe) {
                                    self.assumption_conflict = true;
                                    conflict_tasks.insert(t.clone());
                                    let where_in = if t == EMBEDDED_TASK {
                                        String::new()
                                    } else {
                                        format!(" in task {t}")
                                    };
                                    self.log(&format!(
                                        "ASSUMPTION CONFLICT{where_in}: the active assumptions allow no trace past reset; assertions proved under them are vacuous"
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        // Heartbeat for the UI while the loop runs, so elapsed times move
        // between property events.
        let looping = AtomicBool::new(true);
        let reached = std::thread::scope(|sc| {
            if let Some(t) = &tracker {
                let looping = &looping;
                sc.spawn(move || {
                    let mut n = 0u32;
                    while looping.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(100));
                        n += 1;
                        if n.is_multiple_of(5) && looping.load(Ordering::SeqCst) {
                            t.tick();
                        }
                    }
                });
            }
            let r = self.prove_targets(
                &targets,
                &by_task,
                &conflict_tasks,
                &engines,
                tracker.as_ref(),
            );
            looping.store(false, Ordering::SeqCst);
            r
        });
        // A Stop that came while the last property finished still counts.
        let stopped = reached < targets.len() || self.stop.load(Ordering::SeqCst);
        for &i in &targets[reached..] {
            self.props[i].status = Status::Undetermined {
                reason: "stopped".into(),
            };
            self.props[i].trace = None;
            self.props[i].engines.clear();
        }
        if let Some(t) = &tracker {
            t.end(stopped);
        }
        Ok(targets)
    }

    /// Check whether the active assumptions of `task` (or `self.current_task`)
    /// allow a valid post-reset trace (`live = false`) or an infinite fair
    /// trace (`live = true`). Returns `true` when consistent, `false` when
    /// contradictory / deadlocked.
    pub fn check_assumptions(&mut self, task: Option<&str>, live: bool) -> anyhow::Result<bool> {
        if self.dry_run {
            return Ok(true);
        }
        if !self.elaborated {
            self.elaborate(None)?;
        }
        self.check_reset_and_clocks().map_err(|e| anyhow!(e))?;
        let task = task
            .map(String::from)
            .unwrap_or_else(|| self.current_task.clone());
        self.ensure_task(&task).map_err(|e| anyhow!(e))?;
        let engines = self.engines();
        if engines.is_empty() {
            bail!("check_assumptions: no engines available");
        }
        let lowered = self
            .lower_for_task(&task, false)
            .map_err(|e| anyhow!("{e}"))?;
        let (model, _) = informal_svlang::split_trace(&lowered.btor2);
        std::fs::create_dir_all(&self.workdir)?;
        let path = self.workdir.join("assume_consistency.btor2");
        let consistent = if live {
            let btor = assume_live_btor2(model)
                .ok_or_else(|| anyhow!("could not build liveness consistency model"))?;
            std::fs::write(&path, btor)?;
            let own: Vec<Arc<dyn Engine>> = engines
                .iter()
                .filter(|e| e.name().starts_with("informal-") && !e.name().contains("wbmc"))
                .cloned()
                .collect();
            let use_engines = if own.is_empty() {
                &engines[..]
            } else {
                &own[..]
            };
            let req = CheckRequest {
                model: path.clone(),
                format: ModelFormat::Btor2,
                timeout: self.time_limit,
            };
            let o = portfolio::run_until(use_engines, &req, None, Some(self.stop.as_ref()));
            let _ = std::fs::remove_file(&path);
            !matches!(portfolio::verdict(&o), Verdict::Safe)
        } else {
            let min_steps = if lowered.half_cycle && self.reset.is_some() {
                self.tick_reset().steps + 4
            } else {
                u32::from(self.reset.is_some()) + 4
            };
            let btor = assume_alive_btor2(model, min_steps)
                .ok_or_else(|| anyhow!("could not build assumption consistency model"))?;
            std::fs::write(&path, btor)?;
            let v = self.check_aux_reachability(&engines, &path, Some(min_steps as usize));
            let _ = std::fs::remove_file(&path);
            !matches!(v, Verdict::Safe)
        };
        self.assumption_conflict = !consistent;
        if !consistent {
            let where_in = if task == EMBEDDED_TASK {
                String::new()
            } else {
                format!(" in task {task}")
            };
            self.log(&format!(
                "ASSUMPTION CONFLICT{where_in}: the active assumptions are inconsistent"
            ));
        }
        Ok(consistent)
    }

    /// Auxiliary target reachability check (for assumption consistency and
    /// assertion precondition vacuity). When `exact_bound` is `Some(k)` (step
    /// counter targets), BMC up to `k` is both sound and complete; when `None`
    /// (general precondition covers), a shallow BMC pre-check finds witness
    /// traces in ~1 ms before falling back to the full engine portfolio.
    fn check_aux_reachability(
        &self,
        engines: &[Arc<dyn Engine>],
        model: &Path,
        exact_bound: Option<usize>,
    ) -> Verdict {
        let has_inhouse_bmc = engines.iter().any(|e| e.name() == "informal-bmc");
        if has_inhouse_bmc {
            let bmc = informal_bmc::Bmc {
                max_depth: Some(exact_bound.unwrap_or(16)),
                step: 1,
            };
            let req = CheckRequest {
                model: model.to_path_buf(),
                format: ModelFormat::Btor2,
                timeout: self.time_limit.min(Duration::from_secs(5)),
            };
            if let Ok(r) = bmc.check(&req, &informal_engine::RunContext::new(req.timeout)) {
                match (&r.verdict, exact_bound) {
                    (Verdict::Unsafe { .. } | Verdict::Safe, _) => return r.verdict,
                    // Step counter `_informal_alive_cnt == k` is reachable iff
                    // reachable at step `k`, so bounded exhaustion at `k`
                    // proves unreachability unless timed out.
                    (Verdict::Unknown { reason }, Some(_))
                        if !reason.contains("timeout") && !reason.contains("stop") =>
                    {
                        return Verdict::Safe;
                    }
                    _ => {}
                }
            }
        }
        let req = CheckRequest {
            model: model.to_path_buf(),
            format: ModelFormat::Btor2,
            timeout: self.time_limit,
        };
        let o = portfolio::run_until(engines, &req, None, Some(self.stop.as_ref()));
        portfolio::verdict(&o)
    }

    /// The per-property loop of [`Self::prove`]. Returns how many targets
    /// it got through; fewer than all means a Stop ended it.
    fn prove_targets(
        &mut self,
        targets: &[usize],
        by_task: &std::collections::HashMap<String, Result<informal_svlang::Lowered, String>>,
        conflict_tasks: &std::collections::HashSet<String>,
        engines: &[Arc<dyn Engine>],
        tracker: Option<&Tracker>,
    ) -> usize {
        for (slot, &i) in targets.iter().enumerate() {
            if self.stop.load(Ordering::SeqCst) {
                return slot;
            }
            if let Some(t) = tracker {
                t.running(slot);
            }
            let task_name = self.props[i].task.clone();
            let lowered = by_task
                .get(&task_name)
                .map(|r| r.as_ref().map_err(String::as_str))
                .unwrap_or(Err("task was not lowered"));
            // Properties added before elaboration are checked now.
            // (One written in the RTL had its names bound in its own scope.)
            let names = match self.props[i].rtl {
                true => Ok(()),
                false => self.check_names(&self.props[i].expr),
            };
            if let Err(e) = names.and_then(|()| {
                // The property must be in this model, compiled.
                let Ok(l) = lowered else {
                    return Ok(());
                };
                match l.properties.iter().find(|f| f.name == self.props[i].name) {
                    None if self.props[i].rtl => {
                        Err("not in the model (inside a black box)".to_string())
                    }
                    None => Err("not in the model (disabled)".to_string()),
                    Some(f) => match (&f.unsupported, &f.error) {
                        (Some(why), _) => Err(format!("not supported yet: {why}")),
                        (None, Some(e)) => Err(e.clone()),
                        (None, None) => Ok(()),
                    },
                }
            }) {
                let status = Status::Error { message: e };
                if let Some(t) = tracker {
                    t.finished(slot, &status, &[]);
                }
                self.props[i].status = status;
                self.props[i].engines.clear();
                continue;
            }
            let keep = target_name(&self.props[i].name);
            let out = self.workdir.join(format!("prop_{i}.btor2"));
            let built = match lowered {
                Err(e) => Err(anyhow!("{e}")),
                Ok(l) => {
                    let btor = l.btor2.as_str();
                    if !has_target(btor, &keep) {
                        // Without its line the model would hold trivially.
                        Err(anyhow!("no check named {keep} in the model"))
                    } else {
                        let (model, section) = informal_svlang::split_trace(btor);
                        let mut r = std::fs::write(&out, only_target(model, &keep));
                        // The same model plus `output` lines naming the
                        // combinational nets, for the VCD (trace_model).
                        if r.is_ok() && !section.is_empty() {
                            r = std::fs::write(trace_model(&out), only_target(btor, &keep));
                        }
                        r.map_err(anyhow::Error::from)
                    }
                }
            };
            self.props[i].trace = None;
            let (mut status, trace, runs) = match built {
                Err(e) => (
                    Status::Error {
                        message: format!("{e:#}"),
                    },
                    None,
                    Vec::new(),
                ),
                Ok(()) => self.check_one(engines, i, &out, tracker.map(|t| (t, slot))),
            };
            // Vacuity check on proven assertions: either the task's active
            // assumptions are inconsistent, or the assertion's antecedent
            // (`<keep>:precondition`) is unreachable.
            if self.props[i].kind == Kind::Assert {
                if let Status::Proven { vacuous, .. } = &mut status {
                    if conflict_tasks.contains(&task_name) {
                        *vacuous = true;
                    } else if !engines.is_empty() && !self.stop.load(Ordering::SeqCst) {
                        if let Ok(l) = lowered {
                            let pre_keep = format!("{keep}:precondition");
                            if l.preconditions.iter().any(|n| n == &pre_keep) {
                                if !has_target(&l.preconditions_btor2, &pre_keep) {
                                    *vacuous = true;
                                } else {
                                    let vac_path = self.workdir.join(format!("vacuity_{i}.btor2"));
                                    if std::fs::write(
                                        &vac_path,
                                        only_target(&l.preconditions_btor2, &pre_keep),
                                    )
                                    .is_ok()
                                    {
                                        let v =
                                            self.check_aux_reachability(engines, &vac_path, None);
                                        let _ = std::fs::remove_file(&vac_path);
                                        if matches!(v, Verdict::Safe) {
                                            *vacuous = true;
                                        }
                                    }
                                }
                                if *vacuous {
                                    self.log(&format!(
                                        "{}: proven vacuously (its precondition is unreachable)",
                                        self.props[i].name
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            // Stopped mid-run: the portfolio came back without a verdict.
            // A verdict that landed just before the Stop is kept.
            let halt = self.stop.load(Ordering::SeqCst);
            if halt && matches!(status, Status::Undetermined { .. }) {
                status = Status::Undetermined {
                    reason: "stopped".into(),
                };
            }
            if let Some(t) = tracker {
                t.finished(slot, &status, &runs);
            }
            self.props[i].status = status;
            self.props[i].trace = trace;
            self.props[i].engines = runs;
            if halt {
                return slot + 1;
            }
        }
        targets.len()
    }

    /// The enabled command properties in force for `task` (its own properties
    /// plus `<embedded>` command assumptions when `task` inherits them), each
    /// as the name of its check and a whole directive on its clock.
    fn bound_properties_for_task(&self, task: &str) -> Result<Vec<(String, String)>, String> {
        let inherit = self.task_inherits_embedded(task);
        let mut out = Vec::new();
        for p in self.props.iter().filter(|p| {
            !p.rtl
                && p.enabled
                && (p.task == task
                    || (p.kind == Kind::Assume && inherit && p.task == EMBEDDED_TASK))
        }) {
            let expr = p.expr.trim();
            let clocked = if expr.starts_with('@') {
                expr.to_string()
            } else {
                let c = self.clocks.first().ok_or_else(|| {
                    format!(
                        "{}: no clock: declare one with `clock` / `create_clock`",
                        p.name
                    )
                })?;
                format!("@({} {}) {expr}", c.edge, c.signal)
            };
            out.push((
                target_name(&p.name),
                format!("{} property ({clocked})", p.kind.label()),
            ));
        }
        Ok(out)
    }

    /// Isolated Btor2 lowering for `task`, with only `task`'s active
    /// assumptions and cut points.
    fn lower_for_task(
        &self,
        task: &str,
        precondition_covers: bool,
    ) -> Result<informal_svlang::Lowered, String> {
        let cut_points = self.cut_points_for_task(task);
        let req = informal_svlang::Request {
            files: self.design.files.clone(),
            defines: self.design.defines.clone(),
            incdirs: self.design.incdirs.clone(),
            top: self.design.top.clone(),
            params: self.design.params.clone(),
            // Appends named outputs for combinational nets after
            // informal_svlang::TRACE_MARKER: engines get the text before it
            // (unchanged), the trace replay all of it.
            trace_outputs: true,
            cut_points,
            black_box_modules: self.design.bbox.clone(),
            black_box_instances: self
                .design
                .bbox_instances
                .iter()
                .map(|i| format!("{}.{i}", self.design.top.as_deref().unwrap_or("")))
                .collect(),
            properties: self.bound_properties_for_task(task)?,
            clocks: self
                .clocks
                .iter()
                .map(|c| (c.signal.clone(), c.factor, c.phase))
                .collect(),
            cover_targets: true,
            reset: self.reset.clone(),
            tick: self.tick_reset(),
            skip_initial_checks: self.design.skip_initial_checks,
            isunknown_is_false: self.design.isunknown_is_false,
            precondition_covers,
        };
        // What the user chose to leave out is said with every model.
        if req.skip_initial_checks {
            self.log("set_lowering skip_initial_checks is on: assertions in initial blocks that check a signal at time zero are left out of this model");
        }
        if req.isunknown_is_false {
            self.log("set_lowering isunknown_is_false is on: $isunknown is 0 in this model, so a property that only says a signal is known holds without saying anything about the design");
        }
        if !(req.black_box_modules.is_empty() && req.black_box_instances.is_empty()) {
            self.log(&format!(
                "black boxes in this model (outputs free, contents not modelled): {}. A proof holds for the design; a counterexample may use output values the boxed logic cannot produce",
                self.design
                    .bbox
                    .iter()
                    .chain(&self.design.bbox_instances)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !req.cut_points.is_empty() {
            let top_prefix = format!("{}.", self.design.top.as_deref().unwrap_or(""));
            let shown: Vec<&str> = req
                .cut_points
                .iter()
                .map(|c| c.strip_prefix(&top_prefix).unwrap_or(c.as_str()))
                .collect();
            self.log(&format!(
                "{} cut point(s) (stopat) are free in this model: {}. A proof holds for the design; a counterexample may use values the design cannot produce there",
                shown.len(),
                shown.join(", ")
            ));
        }
        let l = informal_svlang::lower(&req);
        for n in &l.notes {
            self.log(&format!("svlang: {n}"));
        }
        if l.ok {
            Ok(l)
        } else {
            Err(format!("svlang: {}", l.errors.join("; ")))
        }
    }

    /// Run the portfolio on one property's model. With a tracker, engine
    /// progress (k-bound) is forwarded to it for `slot`. A CEX or cover
    /// also gets its trace written (the path is returned with the status),
    /// and every engine's run is returned for the proof profiler.
    fn check_one(
        &self,
        engines: &[Arc<dyn Engine>],
        i: usize,
        model: &Path,
        tracker: Option<(&Tracker, usize)>,
    ) -> (Status, Option<String>, Vec<EngineRun>) {
        let kind = self.props[i].kind;
        let text = std::fs::read_to_string(model).unwrap_or_default();
        let has = |op: &str| {
            text.lines()
                .filter(|l| l.split_whitespace().nth(1) == Some(op))
                .count()
        };
        let liveness = has("justice") > 0;
        let bads = has("bad") + has("justice");
        // A liveness property is a `justice` line, which informal's own
        // engines read as a safety property (liveness to safety, when the
        // model is loaded). An external engine reads the file itself.
        let own: Vec<Arc<dyn Engine>>;
        let engines = if liveness {
            own = engines
                .iter()
                .filter(|e| e.name().starts_with("informal-") && !e.name().contains("wbmc"))
                .cloned()
                .collect();
            &own[..]
        } else {
            engines
        };
        if bads == 0 {
            // The front end folded the check to a constant true.
            let status = match kind {
                Kind::Cover => Status::Unreachable {
                    engine: "elaboration".into(),
                    ms: 0,
                },
                _ => Status::Proven {
                    engine: "elaboration".into(),
                    ms: 0,
                    vacuous: false,
                },
            };
            return (status, None, Vec::new());
        }
        let req = CheckRequest {
            model: model.to_path_buf(),
            format: ModelFormat::Btor2,
            timeout: self.time_limit,
        };
        let stop = Some(self.stop.as_ref());
        let o = match tracker {
            None => portfolio::run_until(engines, &req, None, stop),
            Some((t, slot)) => std::thread::scope(|sc| {
                let (tx, rx) = mpsc::channel::<Progress>();
                // Ends when the portfolio returns and drops the last sender.
                sc.spawn(move || {
                    for p in rx {
                        match p.output {
                            Some((text, err)) => t.engine_line(slot, &p.engine, text, err),
                            None => t.bound(slot, &p.engine, p.bound),
                        }
                    }
                });
                portfolio::run_until(engines, &req, Some(tx), stop)
            }),
        };
        let engine = o
            .winner
            .as_ref()
            .map(|w| w.engine.clone())
            .unwrap_or_default();
        let ms = o.wall_ms;
        let winner = engine.clone();
        let verdict = portfolio::verdict(&o);
        if let (Some(dir), Some(w), true) = (&self.certificates, &o.winner, verdict.is_definitive())
        {
            // A cover's trace and an unreachable cover's proof are
            // certificates like an assertion's.
            let cert = w.certificate.as_ref();
            match informal_bmc::certificate::write(dir, model, &w.engine, &verdict, cert) {
                Ok(s) => self.log(&format!("{}: {}", self.props[i].name, s.line())),
                Err(e) => self.log(&format!(
                    "{}: certificate not written: {e:#}",
                    self.props[i].name
                )),
            }
        }
        let status = match (verdict, kind) {
            (Verdict::Safe, Kind::Cover) => Status::Unreachable { engine, ms },
            (Verdict::Safe, _) => Status::Proven {
                engine,
                ms,
                vacuous: false,
            },
            (Verdict::Unsafe { depth }, Kind::Cover) => Status::Covered { engine, depth, ms },
            (Verdict::Unsafe { depth }, _) => Status::Cex { engine, depth, ms },
            (Verdict::Unknown { reason }, _) => Status::Undetermined { reason },
        };
        let mut status = status;
        let mut trace = None;
        let mut withdrawn = None;
        if let Status::Cex { depth, .. } | Status::Covered { depth, .. } = &mut status {
            if !self.stop.load(Ordering::SeqCst) {
                let witness = o.winner.as_ref().and_then(|w| w.witness.as_deref());
                match self.write_trace(i, model, witness, *depth) {
                    Ok((path, d)) => {
                        // IC3 and external engines often report no depth; the
                        // replayed trace has one.
                        depth.get_or_insert(d as u32);
                        trace = Some(path);
                    }
                    Err(reason) => withdrawn = reason,
                }
            }
        }
        // A counterexample nothing here can reproduce is not reported: the
        // replay is the only check an UNSAFE verdict gets.
        if let Some(why) = withdrawn {
            self.log(&format!(
                "{}: {winner} reported a counterexample that does not replay ({why}); verdict withdrawn",
                self.props[i].name
            ));
            status = Status::Undetermined {
                reason: format!("counterexample did not replay ({winner})"),
            };
        }
        // The profiler's winner row gets the same depth when it had none.
        let mut runs = o.profile;
        if let Status::Cex {
            depth: Some(d),
            engine,
            ..
        }
        | Status::Covered {
            depth: Some(d),
            engine,
            ..
        } = &status
        {
            for r in runs
                .iter_mut()
                .filter(|r| &r.engine == engine && r.depth.is_none())
            {
                r.depth = Some(*d);
            }
        }
        (status, trace, runs)
    }

    /// Write property `i`'s trace as `<workdir>/<name>.vcd` from the
    /// winner's named witness (informal's own engines, and external engines
    /// whose counterexample informal converts: rIC3 `--cex`, ABC `write_cex`) or,
    /// when the winner gave none or it does not replay, from a BMC re-run to
    /// the reported depth that only serves to produce one. Returns the path
    /// and the trace's depth. `Err(Some(why))` when no counterexample could
    /// be replayed on the model (the caller withdraws the verdict);
    /// `Err(None)` when one was but its file could not be written (logged;
    /// the verdict stands).
    fn write_trace(
        &self,
        i: usize,
        model: &Path,
        witness: Option<&str>,
        depth: Option<u32>,
    ) -> Result<(String, usize), Option<String>> {
        let name = &self.props[i].name;
        let clock = self.clocks.first().map_or("clk", |c| c.signal.as_str());
        // Replay on the copy with named combinational nets when there is one.
        let with_nets = trace_model(model);
        let replay = if with_nets.is_file() {
            with_nets.as_path()
        } else {
            model
        };
        let from_winner = witness.and_then(trace::parse_witness).and_then(|w| {
            match trace::vcd_from_witness(replay, &w, name, clock) {
                Ok(v) => Some((v, w.depth)),
                Err(e) => {
                    self.log(&format!("{name}: the engine's witness does not replay ({e:#}); re-running BMC for the trace"));
                    None
                }
            }
        });
        let (vcd, trace_depth) = match from_winner {
            Some(t) => t,
            None => {
                self.trace_reruns.fetch_add(1, Ordering::SeqCst);
                if witness.is_none() {
                    self.note(&format!(
                        "{name}: the winning engine gave no witness; tracing with a BMC re-run"
                    ));
                }
                let w = match self.trace_bmc(model, depth) {
                    Ok(w) => w,
                    Err(e) => return Err(Some(format!("{e:#}"))),
                };
                match trace::vcd_from_witness(replay, &w, name, clock) {
                    Ok(v) => (v, w.depth),
                    Err(e) => return Err(Some(format!("{e:#}"))),
                }
            }
        };
        let path = self.workdir.join(trace::file_name(name));
        if let Err(e) = std::fs::write(&path, vcd) {
            self.log(&format!("{name}: writing {}: {e}", path.display()));
            return Err(None);
        }
        let shown = std::env::current_dir()
            .ok()
            .and_then(|d| path.strip_prefix(d).ok().map(Path::to_path_buf))
            .unwrap_or(path);
        Ok((shown.display().to_string(), trace_depth))
    }

    /// A witness from informal-bmc on `model`, searching up to `depth` (64
    /// frames when the winner did not say), within 30 s or the prove time
    /// limit if shorter.
    fn trace_bmc(&self, model: &Path, depth: Option<u32>) -> anyhow::Result<trace::Witness> {
        let bmc = informal_bmc::Bmc {
            max_depth: Some(depth.unwrap_or(64) as usize),
            step: 1,
        };
        let req = CheckRequest {
            model: model.to_path_buf(),
            format: ModelFormat::Btor2,
            timeout: self.time_limit.min(Duration::from_secs(30)),
        };
        let r = bmc.check(&req, &informal_engine::RunContext::new(req.timeout))?;
        match (
            &r.verdict,
            r.witness.as_deref().and_then(trace::parse_witness),
        ) {
            (Verdict::Unsafe { .. }, Some(w)) => Ok(w),
            (Verdict::Unsafe { .. }, None) => anyhow::bail!("informal-bmc gave no named witness"),
            (v, _) => anyhow::bail!("informal-bmc could not reproduce it ({})", v.label()),
        }
    }

    /// An informational console line (the app's sink only).
    fn note(&self, line: &str) {
        if let Some(s) = &self.sink {
            s.line(line, false);
        }
    }

    /// A line for the console: the app's sink when there is one, else stderr.
    fn log(&self, line: &str) {
        match &self.sink {
            Some(s) => s.line(line, true),
            None => eprintln!("informal: {line}"),
        }
    }

    /// A JasperGold-style summary table.
    pub fn report(&self) -> String {
        self.report_for_task(None)
    }

    pub fn report_for_task(&self, task: Option<&str>) -> String {
        let mut s = format!(
            "{:<28} {:<7} {:<13} {:<10} {}\n",
            "property", "type", "status", "engine", "detail"
        );
        for p in self
            .props
            .iter()
            .filter(|p| task.is_none_or(|t| p.task == t))
        {
            let (engine, detail) = match &p.status {
                Status::Proven {
                    engine,
                    ms,
                    vacuous,
                } => {
                    let vac = if *vacuous { " [vacuous]" } else { "" };
                    (engine.clone(), format!("{:.2} s{vac}", *ms as f64 / 1000.0))
                }
                Status::Unreachable { engine, ms } => {
                    (engine.clone(), format!("{:.2} s", *ms as f64 / 1000.0))
                }
                Status::Cex { engine, depth, ms } | Status::Covered { engine, depth, ms } => (
                    engine.clone(),
                    format!(
                        "depth {} in {:.2} s",
                        depth.map_or("?".into(), |d| d.to_string()),
                        *ms as f64 / 1000.0
                    ),
                ),
                Status::Undetermined { reason } => (String::new(), reason.clone()),
                Status::Error { message } => {
                    (String::new(), message.lines().next().unwrap_or("").into())
                }
                Status::Unprocessed => (String::new(), String::new()),
            };
            let status = if p.enabled {
                p.status.word()
            } else {
                "disabled"
            };
            // Said in the table: what an AI added, and which results
            // stand on an AI-added assumption or cut point.
            let mut detail = match (p.ai, p.under_ai.is_empty()) {
                (true, _) => format!("{detail} [AI-added]").trim_start().to_string(),
                (false, false) => format!("{detail} [under AI-added: {}]", p.under_ai.join(", ")),
                (false, true) => detail,
            };
            if task.is_none() && p.task != EMBEDDED_TASK {
                detail = format!("{detail} [task: {}]", p.task)
                    .trim_start()
                    .to_string();
            }
            s += &format!(
                "{:<28} {:<7} {:<13} {:<10} {}\n",
                p.name,
                p.kind.label(),
                status,
                engine,
                detail
            );
        }
        s
    }
}

/// The Btor2 text with every `bad` line dropped except the one named `keep`.
/// `prop_N.trace.btor2` next to `prop_N.btor2`: the property's model with
/// the slang lowering's named-output section, used only to replay traces.
fn trace_model(model: &Path) -> PathBuf {
    model.with_extension("trace.btor2")
}

/// The name of the property a model line is the target of: a `bad` line
/// (`<id> bad <node> <name>`) or a `justice` line (`<id> justice <n>
/// <nodes> <name>`, a liveness property). `None` for any other line, and
/// `Some("")` for a target with no name.
fn target_of(line: &str) -> Option<(&str, &str)> {
    let w: Vec<&str> = line.split_whitespace().collect();
    match w.get(1).copied() {
        Some("bad") => Some(("bad", w.get(3).copied().unwrap_or(""))),
        Some("justice") => {
            let n: usize = w.get(2)?.parse().ok()?;
            Some(("justice", w.get(3 + n).copied().unwrap_or("")))
        }
        _ => None,
    }
}

/// True when the model has a target (see [`target_of`]) named `name`.
fn has_target(btor: &str, name: &str) -> bool {
    btor.lines()
        .any(|l| target_of(l).is_some_and(|(_, n)| n == name))
}

/// The model with `keep` as its only target. Fairness lines only matter to
/// a liveness target, so they go with the others when `keep` is a `bad`
/// (an external engine may not read them).
fn only_target(btor: &str, keep: &str) -> String {
    let liveness = btor
        .lines()
        .any(|l| target_of(l) == Some(("justice", keep)));
    btor.lines()
        .filter(|l| match target_of(l) {
            Some((_, name)) => name == keep,
            None => liveness || l.split_whitespace().nth(1) != Some("fair"),
        })
        .flat_map(|l| [l, "\n"])
        .collect()
}

/// A property's name as its target line in the model has it: a Btor2
/// symbol has no white space.
fn target_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect()
}

/// Build a single-target Btor2 model from `btor` (with all `bad`, `justice`,
/// `fair` and `output` lines stripped, keeping every `constraint`) whose
/// `bad` line fires when a saturating counter reaches `min_steps`. If the
/// target is `Safe` (unreachable), no trace of length `min_steps + 1`
/// satisfies the model's assumptions.
fn assume_alive_btor2(btor: &str, min_steps: u32) -> Option<String> {
    let mut max_id = 0u64;
    let mut sort1 = None;
    let mut sort8 = None;
    let mut kept = String::new();
    for l in btor.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        if let Some(id) = w.first().and_then(|s| s.parse::<u64>().ok()) {
            max_id = max_id.max(id);
            if w.get(1) == Some(&"sort") && w.get(2) == Some(&"bitvec") {
                match w.get(3).copied() {
                    Some("1") => sort1 = Some(id),
                    Some("8") => sort8 = Some(id),
                    _ => {}
                }
            }
            if matches!(
                w.get(1).copied(),
                Some("bad" | "justice" | "fair" | "output")
            ) {
                continue;
            }
        }
        kept.push_str(l);
        kept.push('\n');
    }
    let mut next_id = max_id + 1;
    let mut alloc = || {
        let id = next_id;
        next_id += 1;
        id
    };
    let s1 = sort1.unwrap_or_else(|| {
        let id = alloc();
        kept.push_str(&format!("{id} sort bitvec 1\n"));
        id
    });
    let s8 = sort8.unwrap_or_else(|| {
        let id = alloc();
        kept.push_str(&format!("{id} sort bitvec 8\n"));
        id
    });
    let z8 = alloc();
    let o8 = alloc();
    let k8 = alloc();
    let cnt = alloc();
    let ini = alloc();
    let eq_k = alloc();
    let inc = alloc();
    let nxt_v = alloc();
    let nxt = alloc();
    let bad = alloc();
    let goal = min_steps.min(254);
    kept.push_str(&format!(
        "{z8} zero {s8}\n\
         {o8} one {s8}\n\
         {k8} constd {s8} {goal}\n\
         {cnt} state {s8} _informal_alive_cnt\n\
         {ini} init {s8} {cnt} {z8}\n\
         {eq_k} eq {s1} {cnt} {k8}\n\
         {inc} add {s8} {cnt} {o8}\n\
         {nxt_v} ite {s8} {eq_k} {cnt} {inc}\n\
         {nxt} next {s8} {cnt} {nxt_v}\n\
         {bad} bad {eq_k} _informal_assume_alive\n"
    ));
    Some(kept)
}

/// Build a liveness consistency Btor2 model (`justice 1 <true>`) keeping all
/// `constraint` and `fair` lines: `Safe` iff no infinite fair trace exists.
fn assume_live_btor2(btor: &str) -> Option<String> {
    let mut max_id = 0u64;
    let mut sort1 = None;
    let mut kept = String::new();
    for l in btor.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        if let Some(id) = w.first().and_then(|s| s.parse::<u64>().ok()) {
            max_id = max_id.max(id);
            if w.get(1) == Some(&"sort") && w.get(2) == Some(&"bitvec") && w.get(3) == Some(&"1") {
                sort1 = Some(id);
            }
            if matches!(w.get(1).copied(), Some("bad" | "justice" | "output")) {
                continue;
            }
        }
        kept.push_str(l);
        kept.push('\n');
    }
    let mut next_id = max_id + 1;
    let mut alloc = || {
        let id = next_id;
        next_id += 1;
        id
    };
    let s1 = sort1.unwrap_or_else(|| {
        let id = alloc();
        kept.push_str(&format!("{id} sort bitvec 1\n"));
        id
    });
    let t1 = alloc();
    let j = alloc();
    kept.push_str(&format!(
        "{t1} one {s1}\n{j} justice 1 {t1} _informal_assume_live\n"
    ));
    Some(kept)
}

/// Operators the front end's property compiler does not take, checked
/// when a property is given so that the command fails and not the prove.
/// (svlang compiles bounded and unbounded sequences including `[->m:n]`,
/// `[=m:n]`, `##[m:$]`, `[*m:$]`, `strong(...)`, the `until` family,
/// `s_eventually`, and bounded `nexttime`, `eventually` and `always`.)
const NOT_COMPILED: &[&str] = &["accept_on", "reject_on", "#-#", "#=#", "not "];

/// `Err` for a property text that is empty or uses one of [`NOT_COMPILED`].
fn check_operators(expr: &str) -> Result<(), String> {
    if expr.trim().is_empty() {
        return Err("empty property".into());
    }
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    // The operator as a whole word where it is one (`until` in `s_until`
    // or in a signal name is not it).
    let used = |word: &str| {
        expr.match_indices(word).any(|(i, _)| {
            let before = expr[..i].chars().next_back();
            let after = expr[i + word.len()..].chars().next();
            let (first, last) = (word.chars().next(), word.chars().next_back());
            (!first.is_some_and(ident) || !before.is_some_and(ident))
                && (!last.is_some_and(ident) || !after.is_some_and(ident))
        })
    };
    match NOT_COMPILED.iter().find(|w| used(w)) {
        Some(w) => Err(format!("`{}` is not supported yet in a property", w.trim())),
        None => Ok(()),
    }
}

/// Tcl `string match` style glob.
pub fn glob(pat: &str, s: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some(b'?'), Some(_)) => go(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    go(pat.as_bytes(), s.as_bytes())
}

/// `1h`, `30m`, `90s`, `2H`, or plain seconds.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, unit) = s.split_at(
        s.find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(s.len()),
    );
    let n: f64 = num.parse().map_err(|_| format!("bad time `{s}`"))?;
    let secs = match unit.to_ascii_lowercase().as_str() {
        "" | "s" => n,
        "m" => n * 60.0,
        "h" => n * 3600.0,
        "d" => n * 86400.0,
        _ => return Err(format!("bad time unit in `{s}`")),
    };
    Ok(Duration::from_secs_f64(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_and_durations() {
        assert!(glob("a_*", "a_le9"));
        assert!(!glob("a_?", "a_le9"));
        assert_eq!(parse_duration("1h").unwrap().as_secs(), 3600);
        assert_eq!(parse_duration("30M").unwrap().as_secs(), 1800);
        assert_eq!(parse_duration("45").unwrap().as_secs(), 45);
        assert!(parse_duration("3x").is_err());
    }
}
