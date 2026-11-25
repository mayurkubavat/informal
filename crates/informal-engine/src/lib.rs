//! informal-engine: the engine plug-in API and the parallel portfolio runner.
//!
//! Every engine (in-process or an external binary) implements [`Engine`].
//! The orchestrator only sees verdicts, progress and capabilities, so new
//! algorithms plug in without touching the rest of the tool.
//!
//! Soundness rule: a `Safe` or `Unsafe` verdict is a claim. The in-house
//! engines check theirs on the model as loaded before reporting it, and
//! hand back a [`Certificate`] that an independent checker (Certifaiger
//! for proofs, aigsim for counterexamples) can confirm from files.

pub mod certificate;
pub mod learned;
pub mod lemma_hub;
pub mod memory;
pub mod portfolio;
pub mod selector;
pub mod witness;

/// For a test that needs a tool this build or machine may lack (CaDiCaL,
/// Bitwuzla, rIC3). Under the gate (`INFORMAL_REQUIRE_TOOLS` set, as
/// `scripts/check.sh` does) a missing tool fails the test, since a test
/// that did not run must not count as passed. Otherwise this prints why
/// and the caller returns.
pub fn tool_missing(what: &str) {
    if std::env::var_os("INFORMAL_REQUIRE_TOOLS").is_some_and(|v| !v.is_empty()) {
        panic!("{what} (INFORMAL_REQUIRE_TOOLS is set: a missing tool fails the test)");
    }
    eprintln!("skipping: {what}");
}

pub use certificate::Certificate;
pub use lemma_hub::{CanonicalClause, HubStats, LemmaHub, LemmaScope, SharedLemma};
pub use portfolio::{ScheduleConfig, ScheduledEngine};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Input format of a model file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelFormat {
    Btor2,
    Aiger,
}

impl ModelFormat {
    /// Guess the format from a file extension (`.btor2`, `.btor`, `.aig`, `.aag`).
    pub fn of_path(p: &Path) -> Option<ModelFormat> {
        match p.extension()?.to_str()? {
            "btor2" | "btor" => Some(ModelFormat::Btor2),
            "aig" | "aag" => Some(ModelFormat::Aiger),
            _ => None,
        }
    }
}

/// Proof effort level controlling engine portfolio breadth, preprocessing
/// (`scorr`), invariant mining, CTG depth, and restart/worker budgets.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    #[default]
    Standard,
    High,
    Max,
}

impl Effort {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "low" | "quick" => Ok(Self::Low),
            "standard" | "medium" | "default" => Ok(Self::Standard),
            "high" | "deep" => Ok(Self::High),
            "max" | "signoff" | "ultra" => Ok(Self::Max),
            other => Err(format!(
                "unknown effort `{other}` (expected `low`, `standard`, `high`, or `max`)"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Standard => "standard",
            Self::High => "high",
            Self::Max => "max",
        }
    }
}

impl std::fmt::Display for Effort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One verification job: a model file, wall-clock budget, effort level, and
/// optional bounded-exploration cap.
#[derive(Debug, Clone)]
pub struct CheckRequest {
    pub model: PathBuf,
    pub format: ModelFormat,
    pub timeout: Duration,
    pub effort: Effort,
    pub max_depth: Option<u32>,
}

impl CheckRequest {
    pub fn new(model: PathBuf, format: ModelFormat, timeout: Duration) -> Self {
        Self {
            model,
            format,
            timeout,
            effort: Effort::Standard,
            max_depth: None,
        }
    }
}

/// Outcome of a check. `Unsafe.depth` is the counterexample length when the
/// engine reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "lowercase")]
pub enum Verdict {
    Safe,
    Unsafe { depth: Option<u32> },
    Unknown { reason: String },
}

impl Verdict {
    pub fn is_definitive(&self) -> bool {
        !matches!(self, Verdict::Unknown { .. })
    }
    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Safe => "safe",
            Verdict::Unsafe { .. } => "unsafe",
            Verdict::Unknown { .. } => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineResult {
    pub engine: String,
    #[serde(flatten)]
    pub verdict: Verdict,
    pub wall_ms: u128,
    /// Raw witness text (Btor2 or AIGER witness format) when the engine printed one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub witness: Option<String>,
    /// What an outside checker needs to confirm a definitive verdict (see
    /// [`certificate`]); `None` from engines that give none.
    #[serde(skip)]
    pub certificate: Option<Certificate>,
}

/// Representation domain an engine reasons over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineDomain {
    /// Bit-level circuit / AIG / SAT / BDD reasoning.
    #[default]
    BitLevel,
    /// Word-level bit-vector and array SMT reasoning.
    WordLevel,
}

/// Algorithmic family for declarative portfolio routing and engine modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineKind {
    Bmc,
    KInduction,
    Ic3,
    Hybrid,
    Liveness,
    Bdd,
    Interpolation,
    Car,
    Swarm,
    #[default]
    Other,
}

/// What an engine can do. The scheduler uses this to route models declaratively
/// without hardcoding engine names.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Capabilities {
    pub formats: Vec<ModelFormat>,
    pub proves: bool,
    pub finds_bugs: bool,
    pub liveness: bool,
    /// Phase 2: engine can export inductive lemmas for sharing.
    pub lemma_export: bool,
    /// Phase 2: engine can import lemmas found by other engines.
    pub lemma_import: bool,
    /// Bit-level (AIG/SAT/BDD) vs word-level (SMT/arrays).
    pub domain: EngineDomain,
    /// Algorithmic family (BMC, k-induction, IC3, Liveness, BDD, ...).
    pub kind: EngineKind,
    /// Supports unblasted Btor2 arrays (`Sort::Array`).
    pub arrays: bool,
    /// Minimum effort level at which this engine participates in auto-routing.
    /// Defaults to `Effort::Standard`, so heavy/external engines are skipped
    /// during quick `Effort::Low` sweeps unless explicitly requested.
    pub min_effort: Effort,
    /// Optional upper bound on sequential state bits (e.g. `Some(200)` for BDDs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_state_bits: Option<u64>,
    /// Engine independently checks and attaches a machine-verifiable certificate
    /// or replayed witness to definitive verdicts.
    pub certifies: bool,
    /// Deep multi-frame unrolling step (shed on tiny or arithmetic-heavy cones).
    pub deep_step: bool,
    /// Multi-worker parallel engine that subsumes a single-worker peer of the
    /// same domain and kind at `Standard+` effort.
    pub multi_worker: bool,
    /// Concurrent CPU worker slots consumed by this engine (`0` = default: `4`
    /// when `multi_worker` is true, else `1`).
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub worker_slots: u8,
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

impl Capabilities {
    /// Effective concurrent thread slots consumed by this engine.
    pub fn slots(&self) -> usize {
        if self.worker_slots > 0 {
            usize::from(self.worker_slots)
        } else if self.multi_worker {
            4
        } else {
            1
        }
    }
}

/// A progress report, streamed to the scheduler and the UI dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub engine: String,
    /// BMC depth or IC3 frame reached, when known.
    pub bound: Option<u32>,
    pub note: String,
    /// One line of the engine's own output (external engines stream their
    /// stdout and stderr here while they run, for the app's Log tab); the
    /// flag is true for stderr. `None` for ordinary progress reports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<(String, bool)>,
}

impl Progress {
    /// A line of engine output (see [`Progress::output`]).
    pub fn line(engine: &str, text: String, stderr: bool) -> Self {
        Progress {
            engine: engine.to_string(),
            bound: None,
            note: String::new(),
            output: Some((text, stderr)),
        }
    }
}

/// Cooperative cancellation shared between the scheduler and engines.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    /// Wrap an existing shared atomic stop flag (e.g. `Session::stop`).
    pub fn from_shared(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst)
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    /// True when both are clones of the same token.
    pub fn same(&self, other: &CancelToken) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Everything an engine gets besides the request itself.
#[derive(Debug, Clone)]
pub struct RunContext {
    pub cancel: CancelToken,
    pub deadline: Instant,
    pub progress: Option<Sender<Progress>>,
    /// Optional cooperative proof-sharing hub (`Phase 2 — LemmaHub`).
    pub lemma_hub: Option<Arc<LemmaHub>>,
}

impl RunContext {
    pub fn new(timeout: Duration) -> Self {
        RunContext {
            cancel: CancelToken::new(),
            deadline: Instant::now() + timeout,
            progress: None,
            lemma_hub: None,
        }
    }
    /// Attach a shared [`LemmaHub`] to this context.
    pub fn with_lemma_hub(mut self, hub: Arc<LemmaHub>) -> Self {
        self.lemma_hub = Some(hub);
        self
    }
    /// True once the run should stop: cancelled or out of time.
    pub fn should_stop(&self) -> bool {
        self.cancel.is_cancelled() || Instant::now() >= self.deadline || memory::exceeded()
    }
    /// Why [`Self::should_stop`] said stop: "memory" or "timeout".
    pub fn stop_reason(&self) -> &'static str {
        if memory::exceeded() || memory::was_shed(&self.cancel) {
            "memory"
        } else {
            "timeout"
        }
    }
    pub fn report(&self, p: Progress) {
        if let Some(tx) = &self.progress {
            let _ = tx.send(p);
        }
    }
}

/// The plug-in interface every engine implements.
pub trait Engine: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> Capabilities;
    /// Run until a verdict, cancellation or the deadline. Must return
    /// promptly (well under a second) once `ctx.should_stop()` is true.
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult>;
    /// Order in which a portfolio over its memory budget stops engines:
    /// higher first (see [`memory::register`]). 0 is never stopped for
    /// memory: an external engine, whose memory is capped on its own.
    fn shed_rank(&self) -> u32 {
        1
    }
}
