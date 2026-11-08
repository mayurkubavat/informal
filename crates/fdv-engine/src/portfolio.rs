//! Parallel portfolio: run several engines on one model, take the first
//! definitive verdict, cancel the rest.
//!
//! Phase 0 runs every eligible engine at once. Phase 3 replaces "all at
//! once" with the learned selector and online time-slicing.

use crate::{CheckRequest, Engine, EngineResult, Progress, RunContext, Verdict};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
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
    /// One record per engine that ran, in the order the engines were given:
    /// how it ended, its time and the depth it reached (the proof profiler).
    pub profile: Vec<EngineRun>,
}

/// How one engine's run in a portfolio ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunOutcome {
    /// Gave the verdict the portfolio reported.
    Won,
    /// Finished without winning: a later definitive verdict, or it gave up.
    Lost,
    /// Ran out of the time budget.
    Timeout,
    /// Cancelled because another engine won or the user stopped the run.
    Killed,
    /// Failed to run (the engine's error text is in `runs`).
    Error,
}

/// One engine's part in a portfolio run (see [`PortfolioOutcome::profile`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngineRun {
    pub engine: String,
    pub outcome: RunOutcome,
    /// Wall time from the portfolio's start until the engine returned.
    pub ms: u128,
    /// The CEX depth for an unsafe verdict, else the deepest bound (BMC
    /// depth, IC3 frame) the engine reported while it ran, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
}

/// An engine's run as received: its slot, name, ms, outcome when already
/// known (definitive verdicts are sorted out once the winner is), depth.
type RawRun = (usize, String, u128, Option<RunOutcome>, Option<u32>);

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
    // Engines report to this run; a forwarder keeps each engine's deepest
    // bound for the profile and passes reports on to the caller.
    let (ptx, prx) = mpsc::channel::<Progress>();
    ctx.progress = Some(ptx);
    let bounds: Mutex<HashMap<String, u32>> = Mutex::default();
    let (tx, rx) = mpsc::channel();

    let eligible: Vec<_> = engines
        .iter()
        .filter(|e| e.capabilities().formats.contains(&req.format))
        .cloned()
        .collect();

    let finished = AtomicBool::new(false);
    let (mut out, mut profile) = std::thread::scope(|s| {
        let bounds_w = &bounds;
        s.spawn(move || {
            for p in prx {
                if let (Some(b), None) = (p.bound, &p.output) {
                    let mut m = bounds_w.lock().unwrap();
                    let e = m.entry(p.engine.clone()).or_insert(b);
                    *e = (*e).max(b);
                }
                if let Some(out) = &progress {
                    let _ = out.send(p);
                }
            }
        });
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
        for (slot, engine) in eligible.iter().enumerate() {
            let (tx, ctx, engine) = (tx.clone(), ctx.clone(), engine.clone());
            s.spawn(move || {
                let r = engine
                    .check(req, &ctx)
                    .map_err(|e| format!("{}: {e:#}", engine.name()));
                let _ = tx.send((slot, engine.name().to_string(), start.elapsed().as_millis(), r));
            });
        }
        drop(tx);
        // Only the engines' clones keep the forwarder alive now.
        ctx.progress = None;

        let mut out = PortfolioOutcome {
            winner: None,
            runs: Vec::new(),
            wall_ms: 0,
            conflicts: Vec::new(),
            profile: Vec::new(),
        };
        let mut profile: Vec<RawRun> = Vec::new();
        for (slot, name, ms, r) in rx {
            // Decided before this result can cancel the others.
            let cancelled = ctx.cancel.is_cancelled();
            let (outcome, depth) = match &r {
                Err(_) => (Some(RunOutcome::Error), None),
                Ok(res) => match &res.verdict {
                    Verdict::Unsafe { depth } => (None, *depth),
                    Verdict::Safe => (None, None),
                    Verdict::Unknown { reason } => {
                        let o = if cancelled {
                            RunOutcome::Killed
                        } else if Instant::now() >= ctx.deadline || reason.contains("timeout") {
                            RunOutcome::Timeout
                        } else {
                            RunOutcome::Lost
                        };
                        (Some(o), None)
                    }
                },
            };
            profile.push((slot, name, ms, outcome, depth));
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
        (out, profile)
    });
    // Every thread has joined, so the forwarder has seen every bound.
    out.wall_ms = start.elapsed().as_millis();
    let winner = out.winner.as_ref().map(|w| w.engine.clone());
    let bounds = bounds.into_inner().unwrap();
    profile.sort_by_key(|p| p.0);
    out.profile = profile
        .into_iter()
        .map(|(_, engine, ms, outcome, depth)| {
            // Definitive verdicts: the winner won, the rest lost.
            let outcome = outcome.unwrap_or(if winner.as_deref() == Some(engine.as_str()) {
                RunOutcome::Won
            } else {
                RunOutcome::Lost
            });
            let depth = depth.or_else(|| bounds.get(&engine).copied());
            EngineRun { engine, outcome, ms, depth }
        })
        .collect();
    out
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
            if self.name == "broken" {
                anyhow::bail!("cannot start");
            }
            let t = Instant::now();
            ctx.report(Progress {
                engine: self.name.into(),
                bound: Some(3),
                note: String::new(),
                output: None,
            });
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
    fn the_profile_records_each_engines_outcome_time_and_depth() {
        let engines: Vec<Arc<dyn Engine>> = vec![
            Arc::new(Fake { name: "slow", delay_ms: 5_000, verdict: Verdict::Safe }),
            Arc::new(Fake { name: "fast", delay_ms: 20, verdict: Verdict::Unsafe { depth: Some(7) } }),
            Arc::new(Fake { name: "broken", delay_ms: 0, verdict: Verdict::Safe }),
        ];
        let (tx, rx) = mpsc::channel();
        let o = run(&engines, &req(), Some(tx));
        let p: Vec<_> = o.profile.iter().map(|r| (r.engine.as_str(), r.outcome, r.depth)).collect();
        assert_eq!(
            p,
            [
                ("slow", RunOutcome::Killed, Some(3)),
                ("fast", RunOutcome::Won, Some(7)),
                ("broken", RunOutcome::Error, None),
            ]
        );
        assert!(o.profile[1].ms >= 20 && o.profile[1].ms <= o.wall_ms);
        // Reports still reach the caller.
        assert_eq!(rx.iter().filter(|p| p.bound == Some(3)).count(), 2);
        let json = serde_json::to_value(&o.profile[2]).unwrap();
        assert_eq!(json, serde_json::json!({"engine": "broken", "outcome": "error", "ms": o.profile[2].ms}));
    }

    #[test]
    fn engines_out_of_time_are_timeouts() {
        let engines: Vec<Arc<dyn Engine>> = vec![Arc::new(Fake { name: "slow", delay_ms: 5_000, verdict: Verdict::Safe })];
        let mut r = req();
        r.timeout = Duration::from_millis(50);
        let o = run(&engines, &r, None);
        assert_eq!(o.profile[0].outcome, RunOutcome::Timeout);
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
