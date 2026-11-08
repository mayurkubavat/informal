//! Parallel portfolio: run several engines on one model, take the first
//! definitive verdict, cancel the rest.
//!
//! Phase 0 runs every eligible engine at once. Phase 3 replaces "all at
//! once" with the learned selector and online time-slicing.

use crate::{CheckRequest, Engine, EngineResult, Progress, RunContext, Verdict};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Serialize)]
pub struct PortfolioOutcome {
    /// The first definitive result, if any engine produced one.
    pub winner: Option<EngineResult>,
    /// One entry per engine that ran: its result, or the error text.
    pub runs: Vec<Result<EngineResult, String>>,
    pub wall_ms: u128,
    /// Engines that gave contradicting definitive verdicts. Non-empty
    /// means an engine is unsound and the result must not be trusted.
    pub conflicts: Vec<String>,
}

/// Run `engines` on `req` in parallel threads. Engines that do not accept
/// the model format are skipped. `progress` receives every engine's reports.
pub fn run(
    engines: &[Arc<dyn Engine>],
    req: &CheckRequest,
    progress: Option<mpsc::Sender<Progress>>,
) -> PortfolioOutcome {
    run_until(engines, req, progress, None)
}

/// [`run`], also cancelled when `stop` becomes true (a user's Stop). The
/// flag belongs to the caller and is only read here: the run's own
/// [`crate::CancelToken`] is still what engines see, so taking a winner does
/// not set `stop`. External engines poll it and kill their process group.
pub fn run_until(
    engines: &[Arc<dyn Engine>],
    req: &CheckRequest,
    progress: Option<mpsc::Sender<Progress>>,
    stop: Option<&AtomicBool>,
) -> PortfolioOutcome {
    let start = Instant::now();
    let mut ctx = RunContext::new(req.timeout);
    ctx.progress = progress;
    let (tx, rx) = mpsc::channel();

    let eligible: Vec<_> = engines
        .iter()
        .filter(|e| e.capabilities().formats.contains(&req.format))
        .cloned()
        .collect();

    let finished = AtomicBool::new(false);
    std::thread::scope(|s| {
        if let Some(stop) = stop {
            // Forward the caller's flag to this run's token until it ends.
            let (cancel, finished) = (ctx.cancel.clone(), &finished);
            s.spawn(move || {
                while !finished.load(Ordering::SeqCst) {
                    if stop.load(Ordering::SeqCst) {
                        cancel.cancel();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
        }
        for engine in &eligible {
            let (tx, ctx, engine) = (tx.clone(), ctx.clone(), engine.clone());
            s.spawn(move || {
                let r = engine
                    .check(req, &ctx)
                    .map_err(|e| format!("{}: {e:#}", engine.name()));
                let _ = tx.send(r);
            });
        }
        drop(tx);

        let mut out = PortfolioOutcome {
            winner: None,
            runs: Vec::new(),
            wall_ms: 0,
            conflicts: Vec::new(),
        };
        for r in rx {
            if let Ok(res) = &r {
                if res.verdict.is_definitive() {
                    match &out.winner {
                        None => {
                            out.winner = Some(res.clone());
                            ctx.cancel.cancel();
                        }
                        Some(w) if w.verdict.label() != res.verdict.label() => {
                            out.conflicts.push(format!(
                                "{} says {} but {} says {}",
                                w.engine,
                                w.verdict.label(),
                                res.engine,
                                res.verdict.label()
                            ))
                        }
                        Some(_) => {}
                    }
                }
            }
            out.runs.push(r);
        }
        finished.store(true, Ordering::SeqCst);
        out.wall_ms = start.elapsed().as_millis();
        out
    })
}

/// Convenience: the verdict to report for a portfolio outcome.
pub fn verdict(o: &PortfolioOutcome) -> Verdict {
    match (&o.winner, o.conflicts.is_empty()) {
        (_, false) => Verdict::Unknown {
            reason: format!("engine conflict: {}", o.conflicts.join("; ")),
        },
        (Some(w), true) => w.verdict.clone(),
        (None, true) => Verdict::Unknown {
            reason: "no engine reached a verdict".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Capabilities, ModelFormat};
    use std::time::Duration;

    /// Test engine: sleeps `delay_ms` in small steps, honoring cancel.
    struct Fake {
        name: &'static str,
        delay_ms: u64,
        verdict: Verdict,
    }

    impl Engine for Fake {
        fn name(&self) -> &str {
            self.name
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                formats: vec![ModelFormat::Btor2],
                ..Default::default()
            }
        }
        fn check(&self, _req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
            let t = Instant::now();
            while t.elapsed() < Duration::from_millis(self.delay_ms) {
                if ctx.should_stop() {
                    let reason = "cancelled".to_string();
                    return Ok(EngineResult {
                        engine: self.name.into(),
                        verdict: Verdict::Unknown { reason },
                        wall_ms: t.elapsed().as_millis(),
                        witness: None,
                    });
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(EngineResult {
                engine: self.name.into(),
                verdict: self.verdict.clone(),
                wall_ms: t.elapsed().as_millis(),
                witness: None,
            })
        }
    }

    fn req() -> CheckRequest {
        CheckRequest {
            model: "m.btor2".into(),
            format: ModelFormat::Btor2,
            timeout: Duration::from_secs(10),
        }
    }

    #[test]
    fn first_definitive_verdict_wins_and_cancels_the_rest() {
        let engines: Vec<Arc<dyn Engine>> = vec![
            Arc::new(Fake {
                name: "slow",
                delay_ms: 5_000,
                verdict: Verdict::Safe,
            }),
            Arc::new(Fake {
                name: "fast",
                delay_ms: 20,
                verdict: Verdict::Unsafe { depth: Some(7) },
            }),
        ];
        let o = run(&engines, &req(), None);
        assert_eq!(o.winner.as_ref().unwrap().engine, "fast");
        assert_eq!(verdict(&o), Verdict::Unsafe { depth: Some(7) });
        assert!(
            o.wall_ms < 2_000,
            "slow engine was not cancelled: {} ms",
            o.wall_ms
        );
        assert_eq!(o.runs.len(), 2);
    }

    #[test]
    fn a_stop_flag_cancels_the_run_and_is_left_alone() {
        let engines: Vec<Arc<dyn Engine>> = vec![Arc::new(Fake {
            name: "slow",
            delay_ms: 5_000,
            verdict: Verdict::Safe,
        })];
        let stop = AtomicBool::new(false);
        let o = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                stop.store(true, Ordering::SeqCst);
            });
            run_until(&engines, &req(), None, Some(&stop))
        });
        assert!(o.winner.is_none());
        assert!(o.wall_ms < 2_000, "stop was not honoured: {} ms", o.wall_ms);
        assert!(matches!(verdict(&o), Verdict::Unknown { .. }));
        // A winner cancelling its rivals must not raise the caller's flag.
        let stop = AtomicBool::new(false);
        let fast: Vec<Arc<dyn Engine>> = vec![Arc::new(Fake {
            name: "fast",
            delay_ms: 0,
            verdict: Verdict::Safe,
        })];
        run_until(&fast, &req(), None, Some(&stop));
        assert!(!stop.load(Ordering::SeqCst));
    }

    #[test]
    fn contradicting_engines_are_flagged() {
        let engines: Vec<Arc<dyn Engine>> = vec![
            Arc::new(Fake {
                name: "a",
                delay_ms: 0,
                verdict: Verdict::Safe,
            }),
            Arc::new(Fake {
                name: "b",
                delay_ms: 0,
                verdict: Verdict::Unsafe { depth: None },
            }),
        ];
        // Both finish before either can be cancelled most of the time; force it
        // by checking the outcome only when both were definitive.
        let o = run(&engines, &req(), None);
        let definitive = o
            .runs
            .iter()
            .filter(|r| {
                r.as_ref()
                    .map(|r| r.verdict.is_definitive())
                    .unwrap_or(false)
            })
            .count();
        if definitive == 2 {
            assert_eq!(o.conflicts.len(), 1);
            assert!(matches!(verdict(&o), Verdict::Unknown { .. }));
        }
    }

    #[test]
    fn skips_engines_that_cannot_read_the_format() {
        let engines: Vec<Arc<dyn Engine>> = vec![Arc::new(Fake {
            name: "btor-only",
            delay_ms: 0,
            verdict: Verdict::Safe,
        })];
        let mut r = req();
        r.format = ModelFormat::Aiger;
        let o = run(&engines, &r, None);
        assert!(o.runs.is_empty());
        assert!(matches!(verdict(&o), Verdict::Unknown { .. }));
    }
}
