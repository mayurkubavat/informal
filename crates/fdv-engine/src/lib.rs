//! fdv-engine: the engine plug-in API and the parallel portfolio runner.
//!
//! Every engine (in-process or an external binary) implements [`Engine`].
//! The orchestrator only sees verdicts, progress and capabilities, so new
//! algorithms plug in without touching the rest of the tool.
//!
//! Soundness rule: a `Safe` or `Unsafe` verdict is a claim. From phase 2
//! the orchestrator checks it with an independent certificate checker
//! (Certifaiger for invariants, BtorSim for witnesses) before reporting it.

pub mod memory;
pub mod portfolio;

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

/// One verification job: a model file and a wall-clock budget.
#[derive(Debug, Clone)]
pub struct CheckRequest {
    pub model: PathBuf,
    pub format: ModelFormat,
    pub timeout: Duration,
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
}

/// What an engine can do. The scheduler uses this to route models.
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
}

/// A progress report, streamed to the scheduler and the UI dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub engine: String,
    /// BMC depth or IC3 frame reached, when known.
    pub bound: Option<u32>,
    pub note: String,
}

/// Cooperative cancellation shared between the scheduler and engines.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst)
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Everything an engine gets besides the request itself.
#[derive(Debug, Clone)]
pub struct RunContext {
    pub cancel: CancelToken,
    pub deadline: Instant,
    pub progress: Option<Sender<Progress>>,
}

impl RunContext {
    pub fn new(timeout: Duration) -> Self {
        RunContext {
            cancel: CancelToken::new(),
            deadline: Instant::now() + timeout,
            progress: None,
        }
    }
    /// True once the run should stop: cancelled or out of time.
    pub fn should_stop(&self) -> bool {
        self.cancel.is_cancelled() || Instant::now() >= self.deadline || memory::exceeded()
    }
    /// Why [`Self::should_stop`] said stop: "memory" or "timeout".
    pub fn stop_reason(&self) -> &'static str {
        if memory::exceeded() {
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
}
