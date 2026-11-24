use crate::progress::ProveSink;
use informal_engine::portfolio::EngineRun;
pub use informal_engine::Effort;
use informal_engine::Engine;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

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

/// Multi-property proof scheduling strategy (`set_prove_orchestration`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Orchestration {
    #[default]
    Auto,
    On,
    Off,
}

impl Orchestration {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" | "default" => Ok(Self::Auto),
            "on" | "true" | "1" | "two_pass" | "twopass" => Ok(Self::On),
            "off" | "false" | "0" | "single" => Ok(Self::Off),
            other => Err(format!(
                "unknown orchestration mode `{other}` (expected `auto`, `on`, or `off`)"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
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
    BoundedProven {
        engine: String,
        bound: u32,
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
            Status::BoundedProven { .. } => "bounded_proven",
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
    /// VCD written for `<name>:witness` when `elaborate -create_related_covers
    /// {witness}` is enabled and a proven assertion's witness cover is hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness_trace: Option<String>,
    /// Optional bounded-proof depth cap (`assert -bound K`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound: Option<u32>,
    /// Minimal proof core & needed assumptions (`prove -with_proof_core`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof_core: Option<crate::proof_core::ProofCore>,
    /// Target name of the assertion this assumption was promoted from (`assume -from_assert`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_assert: Option<String>,
    /// True when `from_assert` references an RTL-compiled assertion target in `l.btor2`.
    #[serde(skip)]
    pub rtl_from_assert: bool,
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
        self.witness_trace = None;
        self.proof_core = None;
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
    /// Black boxes added on `analyze -bbox_m` (persisting across `elaborate`
    /// until `analyze -clear` or `clear`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub analyze_bbox: Vec<String>,
    /// Black boxes by instance path from the top module (`-bbox_i`).
    pub bbox_instances: Vec<String>,
    /// Complexity auto-boxing width threshold for multipliers/dividers (`elaborate -bbox_mul`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox_mul: Option<u32>,
    /// Complexity auto-boxing word-count threshold for memories (`elaborate -bbox_a`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox_a: Option<usize>,
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
    /// Optional wall-clock limit across an entire `prove` batch (`set_prove_total_time_limit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_time_limit: Option<Duration>,
    /// Engine proof effort level (`set_prove_effort`, VCF `set_fml_var fml_effort`).
    pub effort: Effort,
    /// Two-pass sweep/converge orchestration mode (`set_prove_orchestration`).
    pub orchestration: Orchestration,
    /// Persistent BMC trace unrolling bound (`set_max_trace_length`, VCF `set_fml_var fml_max_depth`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_trace_length: Option<u32>,
    /// Engine names to use; empty means every loaded engine.
    pub engine_filter: Vec<String>,
    pub workdir: PathBuf,
    #[serde(skip)]
    pub engines: Vec<Arc<dyn Engine>>,
    /// slang's view of the design, after elaboration (when linked).
    #[serde(skip)]
    pub elab: Option<informal_svlang::Design>,
    /// Cached two-tier compilation, elaboration, and lowered netlists.
    #[serde(skip)]
    pub compiled: Option<Arc<informal_svlang::CompiledDesign>>,
    /// Cached Liberty-backed structural fast-path elaboration for gate/PG netlists.
    #[serde(skip)]
    pub fast_gate: Option<Arc<informal_svlang::FastGateDesign>>,
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
    /// Cover `<name>:witness` (antecedent + consequent) for every proven
    /// assertion after a prove (`elaborate -create_related_covers {witness}`).
    pub witnesses: bool,
    /// Content-addressed proof cache (`set_prove_cache`).
    pub prove_cache: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prove_cache_dir: Option<PathBuf>,
    pub prove_cache_mode: crate::cache::CacheMode,
    /// Formal coverage & UNR state (`check_cov`).
    #[serde(default)]
    pub cov: crate::cov::CovState,
    /// Formal X-propagation state (`check_xprop`).
    #[serde(default)]
    pub xprop: crate::xprop::XpropState,
    /// Formal Security Path Verification rules (`check_spv`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spv: Vec<crate::spv::SpvRule>,
    /// Formal Sequential & Combinational Equivalence Checking state (`check_sec`).
    #[serde(default, skip_serializing_if = "crate::sec::SecState::is_empty")]
    pub sec: crate::sec::SecState,
    /// Low-Power Verification (`lp_static` / `lp_formal`) state (`load_upf`, `check_lp`).
    #[serde(default, skip_serializing_if = "crate::lp::LpState::is_empty")]
    pub lp: crate::lp::LpState,
    /// Provenance of helper assumptions created via `assume -from_assert`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumed_from_assert: Vec<String>,
    /// Over-approximate counter jump abstractions (`abstract -counter`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub counter_abstractions: Vec<crate::complexity::CounterAbstraction>,
    /// Per-run bound cap set during `prove -bound K`.
    #[serde(skip)]
    pub prove_bound: Option<u32>,
    /// Extract minimal proof cores during `prove -with_proof_core`.
    #[serde(skip)]
    pub with_proof_core: bool,
    /// Telemetry from the most recent task's cooperative [`informal_engine::LemmaHub`].
    #[serde(skip)]
    pub last_lemma_stats: Option<informal_engine::HubStats>,
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

/// What the fairness check (`check_assumptions -live`, docs/vacuity.md)
/// found: whether an infinite trace exists that meets every constraint of
/// the task and every fairness assumption infinitely often.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FairEnvironment {
    /// The task whose model was checked.
    pub task: String,
    /// The fairness assumptions (the model's `fair` lines, by the name of
    /// the assumption each comes from).
    pub fairness: Vec<String>,
    pub verdict: Fairness,
    /// When vacuous and narrowing was asked for (`-conflict`): a smallest
    /// set of assumptions (and `reset`) that allows no fair trace on its
    /// own, as for [`Environment::conflict_set`].
    pub conflict_set: Vec<String>,
}

/// The verdict of the fairness check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fairness {
    /// The task has no fairness assumption: nothing to check.
    NoFairness,
    /// A fair infinite trace exists (an engine found a lasso).
    Satisfiable,
    /// No fair infinite trace exists: the fairness assumptions are
    /// unsatisfiable together with the constraints, so every liveness
    /// proof under them is vacuous. Without them an infinite trace exists,
    /// or that run did not decide.
    Vacuous,
    /// No infinite trace exists even without the fairness assumptions: the
    /// constraints alone end every trace (a conflict or a dead end), which
    /// makes every liveness proof vacuous.
    VacuousConstraints,
    /// Not decided (a bounded result, out of time, or no engine for
    /// liveness), with the engines' reason.
    Unknown(String),
}

impl Fairness {
    /// No fair trace exists, whatever the cause.
    pub fn vacuous(&self) -> bool {
        matches!(self, Fairness::Vacuous | Fairness::VacuousConstraints)
    }
}
