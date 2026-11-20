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

/// Result of proving one property, in the vendor tools' vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Status {
    Unprocessed,
    Proven {
        engine: String,
        ms: u128,
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
    /// Whether a proof means anything (docs/vacuity.md): set by the
    /// vacuity checks of a prove, `None` when they did not look at it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vacuity: Option<Vacuity>,
    /// The task the property belongs to ([`EMBEDDED`] unless a `task`
    /// command made another). Outside the embedded task the name is
    /// qualified, `<task>::<name>`, as JasperGold names it.
    pub task: String,
    /// A copy, made by `task -create`/`task -edit -copy`, of a property
    /// written in the RTL: the name of that property, whose check in the
    /// model this one is. `None` for every other property (a copy of a
    /// property given by command is bound again under its own name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_of: Option<String>,
    /// What the last prove measured for this property (times, depth,
    /// invariant size, certificate): `None` before a prove, and when no
    /// engine ran on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<crate::stats::ProofStats>,
}

impl Property {
    /// The name without its task prefix (`t::a_x` -> `a_x`).
    pub fn short_name(&self) -> &str {
        self.name
            .strip_prefix(self.task.as_str())
            .and_then(|r| r.strip_prefix("::"))
            .unwrap_or(&self.name)
    }

    /// Back to unprocessed, forgetting what the last prove found.
    pub fn forget_result(&mut self) {
        self.status = Status::Unprocessed;
        self.trace = None;
        self.engines.clear();
        self.under_ai.clear();
        self.vacuity = None;
        self.stats = None;
    }
}

/// The task every session starts with: what JasperGold calls the embedded
/// task. It holds the properties written in the RTL and, until a `task`
/// command makes another, every property given by command.
pub const EMBEDDED: &str = "<embedded>";

/// A JasperGold task: a named set of properties, the assumptions among
/// them, and its own cut points. A prove runs each task's properties on a
/// model with that task's assumptions and cut points only (decision D20).
#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub name: String,
    /// Cut points (`stopat`), as paths from the top module (`u_core.sig`):
    /// each is cut from its drivers and free on every step.
    pub stopats: Vec<String>,
    /// The cut points among `stopats` that were added through the AI
    /// interface (decision D17).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ai_stopats: Vec<String>,
}

impl Task {
    pub fn new(name: &str) -> Self {
        Task {
            name: name.to_string(),
            stopats: Vec::new(),
            ai_stopats: Vec::new(),
        }
    }
}

/// What the vacuity checks found for one proven property. The status word
/// stays `proven`; this is said beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Vacuity {
    /// The assumptions and the reset allow no trace as long as the check
    /// asked for: the proof holds from some step on whatever the design does.
    VacuousEnvironment,
    /// The precondition cover was hit by a trace that replays: some attempt
    /// of the assertion was checked.
    NonVacuous,
    /// The same, in a model with cut points or black boxes: the trace may
    /// use values the real logic cannot produce there.
    NonVacuousCutPoints,
    /// The precondition cover is unreachable: no attempt is ever checked.
    Vacuous,
    /// The precondition cover was neither hit nor proved unreachable in
    /// the time it had.
    NotChecked,
}

impl Vacuity {
    pub fn word(self) -> &'static str {
        match self {
            Vacuity::VacuousEnvironment => "vacuous (environment)",
            Vacuity::NonVacuous => "non-vacuous",
            Vacuity::NonVacuousCutPoints => "non-vacuous (cut points)",
            Vacuity::Vacuous => "vacuous",
            Vacuity::NotChecked => "vacuity not checked",
        }
    }

    /// The proof says nothing (or nothing beyond some step).
    pub fn is_vacuous(self) -> bool {
        matches!(self, Vacuity::Vacuous | Vacuity::VacuousEnvironment)
    }
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
    /// The tasks, [`EMBEDDED`] first; never empty.
    pub tasks: Vec<Task>,
    /// The task commands without `-task` apply to (`task -set`).
    pub current_task: String,
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
    /// Steps the environment check asks the assumptions and the reset to
    /// allow before each prove (`informal::check_assumptions -depth`); 0
    /// switches the check off.
    pub environment_depth: u32,
    /// The last environment check: how many model steps the assumptions and
    /// the reset allow. Fewer than asked means every proof is vacuous from
    /// that step on.
    pub environment: Option<Environment>,
    /// Cover the precondition of every proven assertion after a prove
    /// (`informal::set_vacuity`, `elaborate -no_preconditions`).
    pub preconditions: bool,
}

/// What the environment check found (docs/vacuity.md).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Environment {
    /// The task whose model was checked.
    pub task: String,
    /// Steps asked for.
    pub asked: u32,
    /// The longest trace the assumptions and the reset allow, in model
    /// steps, up to `asked`; `None` when the check could not decide (no
    /// in-house BMC, or it ran out of time).
    pub allowed: Option<u32>,
    /// The assumptions in force when it ran.
    pub assumptions: Vec<String>,
    /// On a conflict, a smallest set of them (and `reset`) that conflicts
    /// on its own: dropping any one of these allows a longer trace. Empty
    /// when there is no conflict or the model's constraints carry no names.
    pub conflict_set: Vec<String>,
}

impl Environment {
    /// The assumptions and reset allow fewer steps than asked.
    pub fn conflict(&self) -> bool {
        self.allowed.is_some_and(|a| a < self.asked)
    }
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
            tasks: vec![Task::new(EMBEDDED)],
            current_task: EMBEDDED.to_string(),
            time_limit: Duration::from_secs(3600),
            engine_filter: Vec::new(),
            workdir,
            engines,
            elab: None,
            hierarchy: Vec::new(),
            frontend: String::new(),
            stop: Arc::default(),
            dry_run: false,
            sink: None,
            trace_reruns: Arc::default(),
            certificates: None,
            environment_depth: 20,
            environment: None,
            preconditions: true,
        }
    }

    /// `clear -all`: forget the design and properties, keep engines (and
    /// the front end's stop flag and progress sink).
    pub fn clear(&mut self) {
        let engines = std::mem::take(&mut self.engines);
        let workdir = std::mem::take(&mut self.workdir);
        let (stop, sink, dry_run) = (self.stop.clone(), self.sink.take(), self.dry_run);
        let certificates = self.certificates.take();
        let (depth, pre) = (self.environment_depth, self.preconditions);
        *self = Session::new(engines, workdir);
        self.certificates = certificates;
        self.environment_depth = depth;
        self.preconditions = pre;
        self.dry_run = dry_run;
        self.stop = stop;
        self.sink = sink;
    }

    /// Add a property given by command to the current task.
    pub fn add_property(
        &mut self,
        kind: Kind,
        name: Option<String>,
        expr: &str,
    ) -> Result<String, String> {
        let task = self.current_task.clone();
        self.add_property_in(&task, kind, name, expr)
    }

    /// Add a property given by command to `task`. Outside the embedded
    /// task its name is qualified (`<task>::<name>`); a name given already
    /// qualified with that task is taken as it is.
    pub fn add_property_in(
        &mut self,
        task: &str,
        kind: Kind,
        name: Option<String>,
        expr: &str,
    ) -> Result<String, String> {
        // Reject up front what the front end's property compiler is known
        // not to take, and names the design does not have. The front end
        // binds the property at prove and has the last word on both.
        check_operators(expr)?;
        self.check_names(expr)?;
        let task = self.task_name(task)?;
        let short = match name {
            Some(n) => match n.split_once("::") {
                Some((t, rest)) if t == task => rest.to_string(),
                Some((t, _)) => {
                    return Err(format!(
                        "property `{n}` names task {t}, but it is being added to task {task}"
                    ))
                }
                None => n,
            },
            None => {
                let n = self
                    .props
                    .iter()
                    .filter(|p| p.kind == kind && p.task == task)
                    .count();
                format!("{}_{}", kind.label(), n)
            }
        };
        let name = qualify(&task, &short);
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
            vacuity: None,
            task,
            copy_of: None,
            stats: None,
        });
        Ok(name)
    }

    // ------------------------------------------------------------ tasks

    /// The task called `name`, as the session spells it; `Err` when there
    /// is none.
    pub fn task_name(&self, name: &str) -> Result<String, String> {
        self.tasks
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.name.clone())
            .ok_or_else(|| {
                format!(
                    "no task `{name}` (tasks: {})",
                    self.tasks
                        .iter()
                        .map(|t| t.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    fn task_mut(&mut self, name: &str) -> Result<&mut Task, String> {
        let name = self.task_name(name)?;
        Ok(self.tasks.iter_mut().find(|t| t.name == name).unwrap())
    }

    /// The cut points of `task`.
    pub fn stopats_of(&self, task: &str) -> &[String] {
        self.tasks
            .iter()
            .find(|t| t.name == task)
            .map_or(&[][..], |t| &t.stopats)
    }

    /// The cut points of `task`, to change.
    pub fn stopats_mut(&mut self, task: &str) -> Result<&mut Vec<String>, String> {
        Ok(&mut self.task_mut(task)?.stopats)
    }

    /// `task -create`: a new, empty task. The assumptions written in the
    /// RTL are in every task's model (they are part of the design: the
    /// front end cannot leave one out yet), so they are listed in the new
    /// task from the start, as copies.
    pub fn create_task(&mut self, name: &str) -> Result<(), String> {
        if name.is_empty() || name.contains("::") || name.chars().any(char::is_whitespace) {
            return Err(format!(
                "task name `{name}`: a name has no white space and no `::`"
            ));
        }
        if self.tasks.iter().any(|t| t.name == name) {
            return Err(format!("task `{name}` already exists"));
        }
        self.tasks.push(Task::new(name));
        let design_assumes: Vec<usize> = (0..self.props.len())
            .filter(|&i| self.props[i].rtl && self.props[i].kind == Kind::Assume)
            .collect();
        self.copy_into(name, &design_assumes)?;
        Ok(())
    }

    /// Copy properties `which` into `task` (`task -create/-edit -copy`):
    /// each copy is named `<task>::<name without its task>`, starts
    /// unprocessed, and is enabled. One that is there already is left as it
    /// is. Returns the names of the copies made.
    pub fn copy_into(&mut self, task: &str, which: &[usize]) -> Result<Vec<String>, String> {
        let task = self.task_name(task)?;
        let mut made = Vec::new();
        for &i in which {
            let src = &self.props[i];
            let name = qualify(&task, src.short_name());
            if src.task == task || self.props.iter().any(|p| p.name == name) {
                continue;
            }
            let mut p = src.clone();
            p.forget_result();
            p.copy_of = match (src.rtl, &src.copy_of) {
                (true, _) => Some(src.name.clone()),
                (false, c) => c.clone(),
            };
            p.rtl = false;
            p.ai = false;
            p.enabled = true;
            p.name = name.clone();
            p.task = task.clone();
            self.props.push(p);
            made.push(name);
        }
        Ok(made)
    }

    /// Copy `from`'s cut points into `to`.
    pub fn copy_stopats(&mut self, from: &str, to: &str) -> Result<(), String> {
        let cuts = self.task_mut(from)?.stopats.clone();
        let dst = self.stopats_mut(to)?;
        for c in cuts {
            if !dst.contains(&c) {
                dst.push(c);
            }
        }
        Ok(())
    }

    /// `task -set`: the task that commands without `-task` apply to.
    pub fn set_task(&mut self, name: &str) -> Result<(), String> {
        self.current_task = self.task_name(name)?;
        Ok(())
    }

    /// `task -delete`: a task and its properties. The embedded task stays.
    pub fn delete_task(&mut self, name: &str) -> Result<(), String> {
        let name = self.task_name(name)?;
        if name == EMBEDDED {
            return Err(format!("the {EMBEDDED} task cannot be deleted"));
        }
        self.tasks.retain(|t| t.name != name);
        self.props.retain(|p| p.task != name);
        if self.current_task == name {
            self.current_task = EMBEDDED.to_string();
        }
        Ok(())
    }

    /// Indices of the properties of `task`.
    pub fn props_of(&self, task: &str) -> Vec<usize> {
        (0..self.props.len())
            .filter(|&i| self.props[i].task == task)
            .collect()
    }

    /// What [`Self::add_property`] would reject, without adding anything.
    pub fn check_property(&self, name: Option<&str>, expr: &str) -> Result<(), String> {
        check_operators(expr)?;
        self.check_names(expr)?;
        match name.map(|n| qualify(&self.current_task, n)) {
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
        let task = self.current_task.clone();
        let t = self.task_mut(&task)?;
        if t.stopats.contains(&cut) {
            return Err(format!("`{cut}` is already a cut point"));
        }
        t.stopats.push(cut.clone());
        t.ai_stopats.push(cut.clone());
        self.log(&format!("AI-added cut point (stopat) {cut}"));
        Ok(cut)
    }

    /// Remove an AI-added cut point. Returns the properties whose status
    /// was found with it in the model and is now reset.
    pub fn remove_ai_cut_point(&mut self, cut: &str) -> Result<Vec<String>, String> {
        if !self
            .tasks
            .iter()
            .any(|t| t.ai_stopats.iter().any(|c| c == cut))
        {
            return Err(format!("no AI-added cut point `{cut}`"));
        }
        for t in &mut self.tasks {
            if t.ai_stopats.iter().any(|c| c == cut) {
                t.ai_stopats.retain(|c| c != cut);
                t.stopats.retain(|c| c != cut);
            }
        }
        self.log(&format!("AI-added cut point {cut} removed"));
        Ok(self.reset_under(&format!("stopat {cut}")))
    }

    /// Back to unprocessed: every property whose last prove had the
    /// AI-added change `item` in its model.
    fn reset_under(&mut self, item: &str) -> Vec<String> {
        let mut reset = Vec::new();
        for p in &mut self.props {
            if p.under_ai.iter().any(|u| u == item) {
                p.forget_result();
                reset.push(p.name.clone());
            }
        }
        reset
    }

    /// The AI-added changes a prove of the current task started now would
    /// have in its model (see [`Property::under_ai`]).
    pub fn ai_in_force(&self) -> Vec<String> {
        self.ai_in_force_for(&self.current_task)
    }

    /// [`Self::ai_in_force`] for `task`.
    pub fn ai_in_force_for(&self, task: &str) -> Vec<String> {
        let ai_cuts = self
            .tasks
            .iter()
            .filter(|t| t.name == task)
            .flat_map(|t| t.ai_stopats.iter());
        self.props
            .iter()
            .filter(|p| p.ai && p.enabled && p.kind == Kind::Assume && p.task == task)
            .map(|p| p.name.clone())
            .chain(ai_cuts.map(|c| format!("stopat {c}")))
            .collect()
    }

    /// The name of property `i`'s check in its task's model: its own name,
    /// or for a copy of a property in the RTL, the original's.
    pub fn check_name(&self, i: usize) -> String {
        let p = &self.props[i];
        target_name(p.copy_of.as_deref().unwrap_or(&p.name))
    }

    /// The Btor2 model property `i` is proved on: the whole design with
    /// every assumption of its task and this one target. Lowers the design.
    pub fn model_for(&self, i: usize) -> Result<String, String> {
        let (btor, _) = self.lower(&self.props[i].task)?;
        let keep = self.check_name(i);
        if !has_target(&btor, &keep) {
            return Err(format!("no check named {keep} in the model"));
        }
        let (model, _) = informal_svlang::split_trace(&btor);
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

    /// `task`'s cut points as the front end wants them: full paths.
    fn cut_points(&self, task: &str) -> Vec<String> {
        let top = self.design.top.as_deref().unwrap_or("");
        self.stopats_of(task)
            .iter()
            .map(|s| format!("{top}.{s}"))
            .collect()
    }

    /// Names matching a Tcl-style glob (`*`, `?`). A pattern that matches
    /// no name as it stands is tried as `<embedded>::<name>` (JasperGold's
    /// full name of an embedded property) and then in the current task.
    pub fn matching(&self, pattern: &str) -> Vec<usize> {
        let by = |pat: &str, task: Option<&str>| -> Vec<usize> {
            self.props
                .iter()
                .enumerate()
                .filter(|(_, p)| task.is_none_or(|t| p.task == t) && glob(pat, &p.name))
                .map(|(i, _)| i)
                .collect()
        };
        let direct = by(pattern, None);
        if !direct.is_empty() {
            return direct;
        }
        if let Some(rest) = pattern.strip_prefix(&format!("{EMBEDDED}::")) {
            return by(rest, Some(EMBEDDED));
        }
        if self.current_task != EMBEDDED {
            return by(&qualify(&self.current_task, pattern), None);
        }
        Vec::new()
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
        // Every task lists the design's assumptions (see create_task).
        let design_assumes: Vec<usize> = (0..self.props.len())
            .filter(|&i| self.props[i].rtl && self.props[i].kind == Kind::Assume)
            .collect();
        let others: Vec<String> = self.tasks[1..].iter().map(|t| t.name.clone()).collect();
        for t in others {
            let _ = self.copy_into(&t, &design_assumes);
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
        self.mark_boxed_properties();
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
                vacuity: None,
                task: EMBEDDED.to_string(),
                copy_of: None,
                stats: None,
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

    /// Cover assertion `i`'s precondition (`<name>:precondition` in the
    /// model): hit by a trace that replays means some attempt was checked;
    /// unreachable means none ever is. `None` when the model has no such
    /// cover (an assertion with no antecedent, an `until`).
    fn check_precondition(
        &self,
        i: usize,
        btor: &str,
        engines: &[Arc<dyn Engine>],
    ) -> Option<Vacuity> {
        let name = format!("{}:precondition", self.props[i].name);
        let keep = format!("{}:precondition", self.check_name(i));
        if !has_target(btor, &keep) {
            return None;
        }
        let out = self.workdir.join(format!("prop_{i}.precondition.btor2"));
        let (model, section) = informal_svlang::split_trace(btor);
        let mut w = std::fs::write(&out, only_target(model, &keep));
        if w.is_ok() && !section.is_empty() {
            w = std::fs::write(trace_model(&out), only_target(btor, &keep));
        }
        if w.is_err() {
            return Some(Vacuity::NotChecked);
        }
        let req = CheckRequest {
            model: out.clone(),
            format: ModelFormat::Btor2,
            timeout: self.time_limit,
        };
        let o = portfolio::run_until(engines, &req, None, Some(self.stop.as_ref()));
        let verdict = portfolio::verdict(&o);
        if let (Some(dir), Some(w), true) = (&self.certificates, &o.winner, verdict.is_definitive())
        {
            match informal_bmc::certificate::write(
                dir,
                &out,
                &w.engine,
                &verdict,
                w.certificate.as_ref(),
            ) {
                Ok(s) => self.log(&format!("{name}: {}", s.line())),
                Err(e) => self.log(&format!("{name}: certificate not written: {e:#}")),
            }
        }
        let cut = !(self.stopats_of(&self.props[i].task).is_empty()
            && self.design.bbox.is_empty()
            && self.design.bbox_instances.is_empty());
        Some(match verdict {
            Verdict::Safe => Vacuity::Vacuous,
            Verdict::Unsafe { depth } => {
                // The replay is the only check a hit gets, as for a cover.
                let witness = o.winner.as_ref().and_then(|w| w.witness.as_deref());
                match self.write_trace(&name, &out, witness, depth) {
                    Ok(_) if cut => Vacuity::NonVacuousCutPoints,
                    Ok(_) => Vacuity::NonVacuous,
                    Err(_) => Vacuity::NotChecked,
                }
            }
            Verdict::Unknown { .. } => Vacuity::NotChecked,
        })
    }

    /// The environment check: can the assumptions and the reset run for
    /// `depth` model steps? A trace of n steps that meets every constraint
    /// has prefixes of every shorter length that do too, so the longest
    /// allowed trace is found by bisection, each probe one bounded BMC run
    /// of informal-bmc on the model with no targets but a step counter.
    /// Unless that is shorter than `depth`, the assumptions do not conflict
    /// within it (they may still over-constrain: see docs/vacuity.md).
    pub fn check_environment(&self, task: &str, btor: &str, depth: u32) -> Environment {
        let assumptions: Vec<String> = self
            .props
            .iter()
            .filter(|p| p.enabled && p.kind == Kind::Assume && p.task == task)
            .map(|p| p.name.clone())
            .collect();
        let mut env = Environment {
            task: task.to_string(),
            asked: depth,
            allowed: None,
            assumptions,
            conflict_set: Vec::new(),
        };
        let (model, _) = informal_svlang::split_trace(btor);
        let probe = |n: u32| -> Option<bool> { self.trace_of_length(model, n) };
        match probe(depth) {
            Some(true) => env.allowed = Some(depth),
            None => {}
            Some(false) => {
                // Longest n in [0, depth) with a trace: lo has one (or is
                // -1 for none at all), hi has none.
                let (mut lo, mut hi) = (-1i64, depth as i64);
                while hi - lo > 1 {
                    let mid = (lo + hi) / 2;
                    match probe(mid as u32) {
                        Some(true) => lo = mid,
                        Some(false) => hi = mid,
                        None => return env,
                    }
                }
                // No trace of even one step: report 0 allowed steps.
                env.allowed = Some(lo.max(0) as u32);
                if lo < 0 {
                    env.allowed = Some(0);
                    env.asked = env.asked.max(1);
                }
                env.conflict_set = self.conflict_set(model, (lo + 1) as u32);
            }
        }
        env
    }

    /// The assumptions (and the reset) that conflict within `n` steps: of
    /// the named constraint lines, drop each owner in turn and keep it out
    /// while the rest still allow no trace of `n` steps. What is left is a
    /// smallest conflicting set in the sense that dropping any one member
    /// allows a trace (not always the smallest of all such sets). A probe
    /// that cannot decide keeps its owner in the set.
    fn conflict_set(&self, model: &str, n: u32) -> Vec<String> {
        let owners = self.constraint_owners(model);
        let mut names: Vec<&str> = Vec::new();
        for (_, o) in &owners {
            if !names.contains(&o.as_str()) {
                names.push(o);
            }
        }
        // One probe each; past this many, list them all rather than wait.
        if names.is_empty() || names.len() > 64 {
            return names.into_iter().map(String::from).collect();
        }
        let mut dropped: Vec<&str> = Vec::new();
        let mut kept = Vec::new();
        for &name in &names {
            dropped.push(name);
            let without: String = model
                .lines()
                .enumerate()
                .filter(|(i, _)| {
                    !owners
                        .iter()
                        .any(|(j, o)| j == i && dropped.contains(&o.as_str()))
                })
                .flat_map(|(_, l)| [l, "\n"])
                .collect();
            if self.trace_of_length(&without, n) != Some(false) {
                dropped.pop();
                kept.push(name.to_string());
            }
        }
        kept
    }

    /// The constraint lines of `model` that belong to an assumption or to
    /// the reset, by line index, with the owner's name: the assumption's
    /// property name, or `reset`. svlang names each constraint line after
    /// its assumption's label (`top.m_x`, or `m_x` for a script
    /// assumption); a clock's wave is the model's own and is never listed.
    fn constraint_owners(&self, model: &str) -> Vec<(usize, String)> {
        let assumes: Vec<&str> = self
            .props
            .iter()
            .filter(|p| p.enabled && p.kind == Kind::Assume)
            .map(|p| p.name.as_str())
            .collect();
        model
            .lines()
            .enumerate()
            .filter_map(|(i, l)| {
                let w: Vec<&str> = l.split_whitespace().collect();
                if w.get(1) != Some(&"constraint") {
                    return None;
                }
                let sym = *w.get(3)?;
                if sym.ends_with("_reset") && sym.starts_with("svlang") {
                    return Some((i, "reset".to_string()));
                }
                assumes
                    .iter()
                    .find(|&&a| {
                        a == sym
                            || a.ends_with(&format!(".{sym}"))
                            || sym.ends_with(&format!(".{a}"))
                    })
                    .map(|a| (i, a.to_string()))
            })
            .collect()
    }

    /// Whether `model` (Btor2, its targets ignored) has a trace of `n`
    /// steps (states 0 to n) that meets every constraint. `None` when the
    /// in-house BMC is not available or did not finish.
    fn trace_of_length(&self, model: &str, n: u32) -> Option<bool> {
        let text = environment_model(model, n);
        let path = self.workdir.join(format!("environment_{n}.btor2"));
        std::fs::create_dir_all(&self.workdir).ok()?;
        std::fs::write(&path, text).ok()?;
        let bmc = informal_bmc::Bmc {
            max_depth: Some(n as usize),
            step: 1,
        };
        let req = CheckRequest {
            model: path.clone(),
            format: ModelFormat::Btor2,
            timeout: self.time_limit.min(Duration::from_secs(60)),
        };
        let r = bmc.check(&req, &informal_engine::RunContext::new(req.timeout));
        let _ = std::fs::remove_file(&path);
        match r.ok()?.verdict {
            Verdict::Unsafe { .. } => Some(true),
            // The counter reaches n only at step n, so "not within the bound"
            // and "never" both mean no trace of n steps.
            Verdict::Safe => Some(false),
            Verdict::Unknown { reason } if reason.starts_with("bound") => Some(false),
            Verdict::Unknown { .. } => None,
        }
    }

    /// Elaborate and lower if needed, then run the environment check to
    /// `depth` steps and keep (and report) its result.
    pub fn environment_now(&mut self, depth: u32) -> anyhow::Result<Environment> {
        if !self.elaborated {
            self.elaborate(None)?;
        }
        self.check_reset_and_clocks().map_err(|e| anyhow!(e))?;
        let task = self.current_task.clone();
        let (btor, _) = self.lower(&task).map_err(|e| anyhow!(e))?;
        let env = self.check_environment(&task, &btor, depth);
        self.report_environment(&env);
        self.environment = Some(env.clone());
        Ok(env)
    }

    /// Say what the environment check found; a conflict is said loudly.
    fn report_environment(&self, env: &Environment) {
        let named = if env.assumptions.is_empty() {
            "the reset and the assumptions of the design".to_string()
        } else {
            format!("the reset and {}", env.assumptions.join(", "))
        };
        let named = match env.task.as_str() {
            EMBEDDED => named,
            t => format!("{named} (task {t})"),
        };
        match env.allowed {
            None => self.log(&format!(
                "environment check: could not tell whether {named} allow {} steps (no in-house BMC, or out of time); proofs are not checked for vacuity",
                env.asked
            )),
            Some(a) if a < env.asked => self.log(&format!(
                "VACUOUS ENVIRONMENT: {named} allow no trace longer than {a} step(s) (asked for {}). Every assertion holds from step {} on whatever the design does, so its proof says nothing beyond that step; {}",
                env.asked,
                a + 1,
                match env.conflict_set.as_slice() {
                    [] => "the constraints carry no names, so the conflict is not narrowed".to_string(),
                    set => format!(
                        "these conflict, and dropping any one of them allows a longer trace: {}",
                        set.join(", ")
                    ),
                }
            )),
            Some(_) => self.note(&format!(
                "environment check: {named} allow traces of {} steps",
                env.asked
            )),
        }
    }

    /// Mark the properties written inside a black box at elaboration: the
    /// box has no contents, so they are not in the model. They stay in the
    /// list (no property is dropped without a word), with the reason as
    /// their status, and the log says which they are.
    fn mark_boxed_properties(&mut self) {
        let (Some(d), Some(top)) = (&self.elab, self.design.top.as_deref()) else {
            return;
        };
        let boxed: Vec<String> = d
            .instances
            .iter()
            .filter(|i| self.design.bbox.contains(&i.module))
            .map(|i| format!("{}.", i.path))
            .chain(
                self.design
                    .bbox_instances
                    .iter()
                    .map(|r| format!("{top}.{r}.")),
            )
            .collect();
        if boxed.is_empty() {
            return;
        }
        let mut inside = Vec::new();
        for p in self.props.iter_mut() {
            if p.rtl && boxed.iter().any(|b| p.name.starts_with(b.as_str())) {
                p.status = Status::Error {
                    message: "not in the model (inside a black box)".to_string(),
                };
                inside.push(p.name.clone());
            }
        }
        if !inside.is_empty() {
            self.log(&format!(
                "{} propert{} inside black boxes cannot be proved (not in the model): {}",
                inside.len(),
                if inside.len() == 1 { "y" } else { "ies" },
                inside.join(", ")
            ));
        }
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

    /// Prove the selected assertions and covers (the enabled ones of the
    /// current task when `which` is empty). Returns their indices.
    ///
    /// Properties are proved task by task, each task on its own model: the
    /// design with that task's assumptions and cut points (decision D20).
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
                .filter(|&i| {
                    let p = &self.props[i];
                    p.enabled && p.kind != Kind::Assume && p.task == self.current_task
                })
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
        // Task by task, in the order the tasks were made; the tracker's
        // slots follow that order.
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for t in self.tasks.iter().map(|t| t.name.clone()) {
            let members: Vec<usize> = targets
                .iter()
                .copied()
                .filter(|&i| self.props[i].task == t)
                .collect();
            if !members.is_empty() {
                groups.push((t, members));
            }
        }
        let targets: Vec<usize> = groups.iter().flat_map(|g| g.1.clone()).collect();
        let tracker = self.sink.clone().map(|sink| {
            Tracker::start(
                sink,
                targets
                    .iter()
                    .map(|&i| (self.props[i].name.clone(), self.props[i].kind.label())),
            )
        });
        let engines = self.engines();
        self.environment = None;
        self.frontend = "svlang".into();

        // Heartbeat for the UI while the loop runs, so elapsed times move
        // between property events.
        let looping = AtomicBool::new(true);
        let stopped = std::thread::scope(|sc| {
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
            let mut base = 0;
            let mut stopped = false;
            for (task, members) in &groups {
                if stopped || self.stop.load(Ordering::SeqCst) {
                    stopped = true;
                    self.mark_stopped(members);
                    continue;
                }
                stopped = self.prove_task(task, members, &engines, tracker.as_ref(), base);
                base += members.len();
            }
            looping.store(false, Ordering::SeqCst);
            stopped
        });
        if let Some(t) = &tracker {
            t.end(stopped || self.stop.load(Ordering::SeqCst));
        }
        Ok(targets)
    }

    /// Properties a Stop kept from running.
    fn mark_stopped(&mut self, which: &[usize]) {
        for &i in which {
            self.props[i].status = Status::Undetermined {
                reason: "stopped".into(),
            };
            self.props[i].trace = None;
            self.props[i].engines.clear();
            self.props[i].stats = None;
        }
    }

    /// Prove `targets`, all of `task`, on that task's model: the
    /// environment check, the per-property loop and the vacuity checks.
    /// `base` is the first target's slot in the tracker. Returns whether a
    /// Stop ended it.
    fn prove_task(
        &mut self,
        task: &str,
        targets: &[usize],
        engines: &[Arc<dyn Engine>],
        tracker: Option<&Tracker>,
        base: usize,
    ) -> bool {
        let lowered = self.lower(task);
        let mut environment = None;
        if let Ok((btor, _)) = &lowered {
            let assumes = self
                .props
                .iter()
                .any(|p| p.enabled && p.kind == Kind::Assume && p.task == task);
            if self.environment_depth > 0 && assumes && has_constraints(btor) {
                let env = self.check_environment(task, btor, self.environment_depth);
                self.report_environment(&env);
                environment = Some(env);
            }
        }
        let under_ai = self.ai_in_force_for(task);
        for &i in targets {
            self.props[i].under_ai = under_ai.clone();
        }
        let reached = self.prove_targets(
            targets,
            lowered
                .as_ref()
                .map(|(btor, rtl)| (btor.as_str(), rtl.as_slice()))
                .map_err(String::as_str),
            engines,
            tracker,
            base,
        );
        // A Stop that came while the last property finished still counts.
        let stopped = reached < targets.len() || self.stop.load(Ordering::SeqCst);
        self.mark_stopped(&targets[reached..]);
        // A conflict in the environment empties every proof of this run;
        // otherwise each proven assertion's precondition is covered.
        let conflict = environment.as_ref().is_some_and(Environment::conflict);
        if environment.is_some() {
            // The last check made is the one the session shows.
            self.environment = environment;
        }
        for &i in targets {
            self.props[i].vacuity = match (&self.props[i].status, conflict) {
                (Status::Proven { .. }, true) => Some(Vacuity::VacuousEnvironment),
                _ => None,
            };
        }
        if let (Ok((btor, _)), false, true) = (&lowered, conflict, self.preconditions) {
            for &i in &targets[..reached] {
                if self.stop.load(Ordering::SeqCst) {
                    break;
                }
                let proven = matches!(self.props[i].status, Status::Proven { .. });
                if proven && self.props[i].kind == Kind::Assert {
                    self.props[i].vacuity = self.check_precondition(i, btor, engines);
                }
            }
            let vacuous: Vec<&str> = targets
                .iter()
                .filter(|&&i| self.props[i].vacuity == Some(Vacuity::Vacuous))
                .map(|&i| self.props[i].name.as_str())
                .collect();
            if !vacuous.is_empty() {
                self.log(&format!(
                    "VACUOUS: {} proven assertion(s) never check anything, because no trace reaches their antecedent (precondition unreachable): {}",
                    vacuous.len(),
                    vacuous.join(", ")
                ));
            }
        }
        stopped
    }

    /// The per-property loop of [`Self::prove`]. Returns how many targets
    /// it got through; fewer than all means a Stop ended it.
    fn prove_targets(
        &mut self,
        targets: &[usize],
        lowered: Result<(&str, &[informal_svlang::RtlProperty]), &str>,
        engines: &[Arc<dyn Engine>],
        tracker: Option<&Tracker>,
        base: usize,
    ) -> usize {
        for (k, &i) in targets.iter().enumerate() {
            let slot = base + k;
            if self.stop.load(Ordering::SeqCst) {
                return k;
            }
            if let Some(t) = tracker {
                t.running(slot);
            }
            // Properties added before elaboration are checked now.
            // (One written in the RTL had its names bound in its own scope.)
            let in_rtl = self.props[i].rtl || self.props[i].copy_of.is_some();
            let check = self.check_name(i);
            let names = match in_rtl {
                true => Ok(()),
                false => self.check_names(&self.props[i].expr),
            };
            if let Err(e) = names.and_then(|()| {
                // The property must be in this model, compiled.
                let Ok((_, found)) = lowered else {
                    return Ok(());
                };
                match found.iter().find(|f| target_name(&f.name) == check) {
                    None if in_rtl => Err("not in the model (inside a black box)".to_string()),
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
                self.props[i].stats = None;
                continue;
            }
            let out = self.workdir.join(format!("prop_{i}.btor2"));
            let built = match lowered {
                Err(e) => Err(anyhow!("{e}")),
                Ok((btor, _)) => {
                    let keep = check.clone();
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
            let (mut status, trace, runs, mut stats) = match built {
                Err(e) => (
                    Status::Error {
                        message: format!("{e:#}"),
                    },
                    None,
                    Vec::new(),
                    None,
                ),
                Ok(()) => self.check_one(engines, i, &out, tracker.map(|t| (t, slot))),
            };
            // Stopped mid-run: the portfolio came back without a verdict.
            // A verdict that landed just before the Stop is kept.
            let halt = self.stop.load(Ordering::SeqCst);
            if halt && matches!(status, Status::Undetermined { .. }) {
                status = Status::Undetermined {
                    reason: "stopped".into(),
                };
            }
            // The verdict may have been withdrawn or stopped since the
            // statistics were taken: the engine named is the status's.
            if let Some(st) = &mut stats {
                if matches!(status, Status::Undetermined { .. } | Status::Error { .. }) {
                    st.engine = None;
                    if st.depth_kind != Some("bound") {
                        st.depth = st.max_bound;
                        st.depth_kind = st.max_bound.map(|_| "bound");
                    }
                }
            }
            if let Some(t) = tracker {
                t.finished(slot, &status, &runs);
            }
            self.props[i].status = status;
            self.props[i].trace = trace;
            self.props[i].engines = runs;
            self.props[i].stats = stats;
            if halt {
                return k + 1;
            }
        }
        targets.len()
    }

    /// The enabled properties given by command, each as the name of its
    /// check and a whole directive on its clock: its own clocking event, or
    /// the first declared clock.
    fn bound_properties(&self, task: &str) -> Result<Vec<(String, String)>, String> {
        let mut out = Vec::new();
        let given = |p: &&Property| !p.rtl && p.copy_of.is_none() && p.enabled && p.task == task;
        for p in self.props.iter().filter(given) {
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

    /// One Btor2 model with every enabled property, and the properties as
    /// the front end compiled them: those written in the RTL, then those
    /// given by command. `Err` rejects the design: a compile error, a reset
    /// that does not bind, or a construct the front end does not model.
    /// Nothing is guessed: a guess would turn a typo into a free signal
    /// (D13).
    fn lower(&self, task: &str) -> Result<(String, Vec<informal_svlang::RtlProperty>), String> {
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
            cut_points: self.cut_points(task),
            black_box_modules: self.design.bbox.clone(),
            black_box_instances: self
                .design
                .bbox_instances
                .iter()
                .map(|i| format!("{}.{i}", self.design.top.as_deref().unwrap_or("")))
                .collect(),
            properties: self.bound_properties(task)?,
            clocks: self
                .clocks
                .iter()
                .map(|c| (c.signal.clone(), c.factor, c.phase))
                .collect(),
            cover_targets: true,
            precondition_covers: self.preconditions,
            reset: self.reset.clone(),
            tick: self.tick_reset(),
            skip_initial_checks: self.design.skip_initial_checks,
            isunknown_is_false: self.design.isunknown_is_false,
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
            let cuts = self.stopats_of(task);
            let of = match task {
                EMBEDDED => String::new(),
                t => format!(" of task {t}"),
            };
            self.log(&format!(
                "{} cut point(s) (stopat){of} are free in this model: {}. A proof holds for the design; a counterexample may use values the design cannot produce there",
                cuts.len(),
                cuts.join(", ")
            ));
        }
        let l = informal_svlang::lower(&req);
        for n in &l.notes {
            self.log(&format!("svlang: {n}"));
        }
        if l.ok {
            Ok((l.btor2, l.properties))
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
    ) -> (
        Status,
        Option<String>,
        Vec<EngineRun>,
        Option<crate::stats::ProofStats>,
    ) {
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
                },
            };
            return (status, None, Vec::new(), None);
        }
        let req = CheckRequest {
            model: model.to_path_buf(),
            format: ModelFormat::Btor2,
            timeout: self.time_limit,
        };
        let stop = Some(self.stop.as_ref());
        let before = crate::stats::Usage::now();
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
        let usage = before.zip(crate::stats::Usage::now());
        let engine = o
            .winner
            .as_ref()
            .map(|w| w.engine.clone())
            .unwrap_or_default();
        let ms = o.wall_ms;
        let mut certificate = None;
        let winner = engine.clone();
        let verdict = portfolio::verdict(&o);
        if let (Some(dir), Some(w), true) = (&self.certificates, &o.winner, verdict.is_definitive())
        {
            // A cover's trace and an unreachable cover's proof are
            // certificates like an assertion's.
            let cert = w.certificate.as_ref();
            match informal_bmc::certificate::write(dir, model, &w.engine, &verdict, cert) {
                Ok(s) => {
                    certificate = Some(s.line());
                    self.log(&format!("{}: {}", self.props[i].name, s.line()))
                }
                Err(e) => {
                    certificate = Some(format!("not written: {e:#}"));
                    self.log(&format!(
                        "{}: certificate not written: {e:#}",
                        self.props[i].name
                    ))
                }
            }
        }
        let status = match (verdict, kind) {
            (Verdict::Safe, Kind::Cover) => Status::Unreachable { engine, ms },
            (Verdict::Safe, _) => Status::Proven { engine, ms },
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
                match self.write_trace(&self.props[i].name, model, witness, *depth) {
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
        let winner_cert = o.winner.as_ref().and_then(|w| w.certificate.as_ref());
        let mut stats =
            crate::stats::ProofStats::of_run(&status, &runs, o.wall_ms, winner_cert, usage);
        stats.certificate = certificate;
        (status, trace, runs, Some(stats))
    }

    /// The Btor2 model of `task` as the engines get it: the design, the
    /// task's assumptions and cut points, and every target of the task.
    pub fn task_model(&self, task: &str) -> Result<String, String> {
        let (btor, _) = self.lower(task)?;
        Ok(informal_svlang::split_trace(&btor).0.to_string())
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
        name: &str,
        model: &Path,
        witness: Option<&str>,
        depth: Option<u32>,
    ) -> Result<(String, usize), Option<String>> {
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
    pub(crate) fn log(&self, line: &str) {
        match &self.sink {
            Some(s) => s.line(line, true),
            None => eprintln!("informal: {line}"),
        }
    }

    /// One row per property of the table `report` prints: name, type,
    /// status, engine and detail.
    pub fn report_rows(&self) -> Vec<[String; 5]> {
        self.report_rows_of(None)
    }

    /// [`Self::report_rows`] of one task's properties, or of all.
    pub fn report_rows_of(&self, task: Option<&str>) -> Vec<[String; 5]> {
        let mut rows = Vec::new();
        for p in self
            .props
            .iter()
            .filter(|p| task.is_none_or(|t| p.task == t))
        {
            let (engine, detail) = match &p.status {
                Status::Proven { engine, ms } | Status::Unreachable { engine, ms } => {
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
            let detail = match p.vacuity {
                Some(v) => format!("{detail} [{}]", v.word()),
                None => detail,
            };
            let detail = match (p.ai, p.under_ai.is_empty()) {
                (true, _) => format!("{detail} [AI-added]").trim_start().to_string(),
                (false, false) => format!("{detail} [under AI-added: {}]", p.under_ai.join(", ")),
                (false, true) => detail,
            };
            rows.push([
                p.name.clone(),
                p.kind.label().to_string(),
                status.to_string(),
                engine,
                detail,
            ]);
        }
        rows
    }

    /// A JasperGold-style summary table.
    pub fn report(&self) -> String {
        self.report_of(None)
    }

    /// [`Self::report`] of one task's properties, or of all.
    pub fn report_of(&self, task: Option<&str>) -> String {
        let mut s = format!(
            "{:<28} {:<7} {:<13} {:<10} {}\n",
            "property", "type", "status", "engine", "detail"
        );
        for [name, kind, status, engine, detail] in self.report_rows_of(task) {
            s += &format!("{name:<28} {kind:<7} {status:<13} {engine:<10} {detail}\n");
        }
        s
    }

    /// The same table as comma-separated values (`report -csv`).
    pub fn report_csv(&self) -> String {
        self.report_csv_of(None)
    }

    /// [`Self::report_csv`] of one task's properties, or of all.
    pub fn report_csv_of(&self, task: Option<&str>) -> String {
        let field = |f: &str| {
            if f.contains([',', '"', '\n']) {
                format!("\"{}\"", f.replace('"', "\"\""))
            } else {
                f.to_string()
            }
        };
        let mut s = String::from("property,type,status,engine,detail\n");
        for row in self.report_rows_of(task) {
            s += &row.iter().map(|f| field(f)).collect::<Vec<_>>().join(",");
            s.push('\n');
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

/// The model has `constraint` lines (assumptions, and the reset).
fn has_constraints(btor: &str) -> bool {
    let (model, _) = informal_svlang::split_trace(btor);
    model
        .lines()
        .any(|l| l.split_whitespace().nth(1) == Some("constraint"))
}

/// `model` with its targets (bad, justice, fair) dropped, plus a step
/// counter and one `bad` that holds exactly at step `n`: BMC finds it iff
/// some trace of n steps meets every constraint.
fn environment_model(model: &str, n: u32) -> String {
    let mut out = String::new();
    let mut top = 0u64;
    for l in model.lines() {
        let mut w = l.split_whitespace();
        if let Some(id) = w.next().and_then(|x| x.parse::<u64>().ok()) {
            top = top.max(id);
        }
        if matches!(w.next(), Some("bad" | "justice" | "fair")) {
            continue;
        }
        out += l;
        out.push('\n');
    }
    let width = (32 - n.leading_zeros()).max(1) + 1;
    let id = |k: u64| top + k;
    out += &format!(
        "{} sort bitvec {width}\n{} sort bitvec 1\n{} zero {}\n{} state {} informal_environment_step\n{} init {} {} {}\n{} one {}\n{} add {} {} {}\n{} next {} {} {}\n{} constd {} {n}\n{} eq {} {} {}\n{} bad {} informal_environment\n",
        id(1),
        id(2),
        id(3), id(1),
        id(4), id(1),
        id(5), id(1), id(4), id(3),
        id(6), id(1),
        id(7), id(1), id(4), id(6),
        id(8), id(1), id(4), id(7),
        id(9), id(1),
        id(10), id(2), id(4), id(9),
        id(11), id(10),
    );
    out
}

/// A property's full name in `task`: `<task>::<name>`, or the name alone
/// in the embedded task.
pub fn qualify(task: &str, name: &str) -> String {
    match task {
        EMBEDDED => name.to_string(),
        t => format!("{t}::{name}"),
    }
}

/// A property's name as its target line in the model has it: a Btor2
/// symbol has no white space.
fn target_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect()
}

/// Operators the front end's property compiler does not take, checked
/// when a property is given so that the command fails and not the prove.
/// (It takes bounded and unbounded sequences, goto `[->]` and
/// non-consecutive `[=]` repetition, `strong`, `not`, `#-#` / `#=#`,
/// `accept_on` / `reject_on` / `sync_accept_on` / `sync_reject_on`, the
/// `until` family, `s_eventually`, and bounded `nexttime`, `eventually`
/// and `always`.)
const NOT_COMPILED: &[&str] = &["implies"];

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

/// `1h`, `30m`, `90s`, `500ms`, `2H`, or plain seconds.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, unit) = s.split_at(
        s.find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(s.len()),
    );
    let n: f64 = num.parse().map_err(|_| format!("bad time `{s}`"))?;
    let secs = match unit.to_ascii_lowercase().as_str() {
        "" | "s" => n,
        "ms" => n / 1000.0,
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
