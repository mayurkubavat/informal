//! Parallel and staged-wave portfolio runner.
//!
//! Runs engines on one model, takes the first definitive verdict, and cancels
//! the rest. Supports Phase 2 cooperative [`LemmaHub`] proof sharing and
//! Phase 3 core-aware concurrency capping and wave-staged launches
//! ([`run_scheduled`]).

use crate::{
    memory, CancelToken, CheckRequest, Engine, EngineResult, HubStats, LemmaHub, Progress,
    RunContext, Verdict,
};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
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
    /// One record per engine that ran, in schedule order: how it ended, its
    /// time and the depth it reached (the proof profiler).
    pub profile: Vec<EngineRun>,
    /// Telemetry from the cooperative [`LemmaHub`], when sharing was active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lemma_stats: Option<HubStats>,
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

/// One engine entry in a staged portfolio schedule.
#[derive(Clone)]
pub struct ScheduledEngine {
    pub engine: Arc<dyn Engine>,
    /// Earliest launch offset from portfolio start (`Duration::ZERO` = Wave 0).
    pub delay: Duration,
    /// Optional per-engine time cap (clamped to the portfolio deadline).
    pub time_cap: Option<Duration>,
}

impl ScheduledEngine {
    pub fn immediate(engine: Arc<dyn Engine>) -> Self {
        Self {
            engine,
            delay: Duration::ZERO,
            time_cap: None,
        }
    }

    pub fn delayed(engine: Arc<dyn Engine>, delay: Duration) -> Self {
        Self {
            engine,
            delay,
            time_cap: None,
        }
    }
}

/// Concurrency and wave-staging controls for [`run_scheduled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleConfig {
    /// Maximum concurrent worker slots active at once (`0` = unlimited).
    pub max_slots: usize,
}

impl ScheduleConfig {
    /// Unlimited concurrency (backward-compatible all-at-once mode and explicit
    /// `--only` engine filters).
    pub const fn unlimited() -> Self {
        Self { max_slots: 0 }
    }

    /// Core-aware slot budget for the current host. Override with
    /// `INFORMAL_PORTFOLIO_SLOTS=<n>` (`0` = unlimited).
    pub fn default_for_host() -> Self {
        if let Ok(raw) = std::env::var("INFORMAL_PORTFOLIO_SLOTS") {
            if let Ok(n) = raw.trim().parse::<usize>() {
                return Self { max_slots: n };
            }
        }
        let cores = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(4);
        // Keep active SAT/SMT solver threads within ~2/3 of hardware threads
        // (matching P-core count on hybrid CPUs) while preserving concurrency.
        let cap = ((cores * 2) / 3).clamp(4, 12);
        Self { max_slots: cap }
    }
}

/// Whether Phase 3 wave-staged portfolio scheduling is enabled.
/// `INFORMAL_PORTFOLIO_STAGED=0` reverts to Wave-0-only all-at-once launches.
pub fn staged_enabled() -> bool {
    std::env::var("INFORMAL_PORTFOLIO_STAGED").map_or(true, |v| v.trim() != "0")
}

/// An engine's run as received: slot, name, ms, outcome when already known, depth.
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

/// [`run`], also cancelled when `stop` becomes true (a user's Stop).
pub fn run_until(
    engines: &[Arc<dyn Engine>],
    req: &CheckRequest,
    progress: Option<mpsc::Sender<Progress>>,
    stop: Option<&AtomicBool>,
) -> PortfolioOutcome {
    run_with_hub(engines, req, progress, stop, None)
}

/// [`run_until`], optionally reusing a caller-supplied [`LemmaHub`] (for
/// multi-property shared-cone sessions). Launches all eligible engines in
/// Wave 0 without slot caps (backward-compatible default).
pub fn run_with_hub(
    engines: &[Arc<dyn Engine>],
    req: &CheckRequest,
    progress: Option<mpsc::Sender<Progress>>,
    stop: Option<&AtomicBool>,
    hub: Option<Arc<LemmaHub>>,
) -> PortfolioOutcome {
    let sched: Vec<ScheduledEngine> = engines
        .iter()
        .cloned()
        .map(ScheduledEngine::immediate)
        .collect();
    run_scheduled(
        &sched,
        req,
        progress,
        stop,
        hub,
        ScheduleConfig::unlimited(),
    )
}

/// Event-driven staged-wave portfolio runner with core-aware slot budgeting.
///
/// Eligible entries in `schedule` are launched in order of `(delay, index)` as
/// their launch offset arrives and worker slots are available. If all active
/// Wave-0 engines finish early with `Unknown`, delayed reserve engines are
/// promoted immediately rather than idling. A definitive winner, deadline, or
/// external `stop` cancels all in-flight engines and drops unlaunched waves
/// immediately without spawning them.
pub fn run_scheduled(
    schedule: &[ScheduledEngine],
    req: &CheckRequest,
    progress: Option<mpsc::Sender<Progress>>,
    stop: Option<&AtomicBool>,
    hub: Option<Arc<LemmaHub>>,
    cfg: ScheduleConfig,
) -> PortfolioOutcome {
    let start = Instant::now();
    let mut eligible: Vec<(usize, ScheduledEngine)> = schedule
        .iter()
        .filter(|s| s.engine.capabilities().formats.contains(&req.format))
        .cloned()
        .enumerate()
        .collect();
    eligible.sort_by_key(|(idx, s)| (s.delay, *idx));

    let active_hub = hub.or_else(|| {
        (LemmaHub::is_enabled() && eligible.len() >= 2).then(|| Arc::new(LemmaHub::new()))
    });
    let mut base_ctx = RunContext::new(req.timeout);
    base_ctx.lemma_hub = active_hub.clone();

    let (ptx, prx) = mpsc::channel::<Progress>();
    base_ctx.progress = Some(ptx);
    let bounds: Mutex<HashMap<String, u32>> = Mutex::default();
    let (tx, rx) = mpsc::channel::<(usize, String, usize, u128, Result<EngineResult, String>)>();
    let tokens: Mutex<Vec<CancelToken>> = Mutex::default();
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
            let (cancel, finished) = (base_ctx.cancel.clone(), &finished);
            s.spawn(move || {
                while !finished.load(Ordering::SeqCst) {
                    if stop.load(Ordering::SeqCst) {
                        cancel.cancel();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
        }
        {
            let (run, tokens_ref, finished) = (base_ctx.cancel.clone(), &tokens, &finished);
            s.spawn(move || {
                while !finished.load(Ordering::SeqCst) {
                    if run.is_cancelled() {
                        let list = tokens_ref.lock().unwrap();
                        list.iter().for_each(CancelToken::cancel);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
        }

        let mut pending: VecDeque<(usize, ScheduledEngine)> = eligible.into();
        let mut active_slots = 0usize;
        let mut in_flight = 0usize;

        let mut out = PortfolioOutcome {
            winner: None,
            runs: Vec::new(),
            wall_ms: 0,
            conflicts: Vec::new(),
            profile: Vec::new(),
            lemma_stats: None,
        };
        let mut profile: Vec<RawRun> = Vec::new();

        let launch_one = |slot: usize,
                          entry: ScheduledEngine,
                          active_slots: &mut usize,
                          in_flight: &mut usize| {
            let eng_slots = entry.engine.capabilities().slots();
            let tok = CancelToken::new();
            if base_ctx.cancel.is_cancelled() {
                tok.cancel();
            }
            tokens.lock().unwrap().push(tok.clone());
            let (tx, mut ctx, engine) = (tx.clone(), base_ctx.clone(), entry.engine);
            ctx.cancel = tok;
            if let Some(cap) = entry.time_cap {
                ctx.deadline = ctx.deadline.min(Instant::now() + cap);
            }
            *active_slots += eng_slots;
            *in_flight += 1;
            s.spawn(move || {
                let _shed = memory::register(engine.name(), engine.shed_rank(), ctx.cancel.clone());
                let r = engine
                    .check(req, &ctx)
                    .map_err(|e| format!("{}: {e:#}", engine.name()));
                let _ = tx.send((
                    slot,
                    engine.name().to_string(),
                    eng_slots,
                    start.elapsed().as_millis(),
                    r,
                ));
            });
        };

        loop {
            if stop.is_some_and(|f| f.load(Ordering::SeqCst)) {
                base_ctx.cancel.cancel();
            }
            let cancelled = base_ctx.cancel.is_cancelled();
            let now = Instant::now();
            if cancelled || now >= base_ctx.deadline {
                pending.clear();
            } else {
                let elapsed = start.elapsed();
                while let Some((_, front)) = pending.front() {
                    let due = elapsed >= front.delay || in_flight == 0;
                    if !due {
                        break;
                    }
                    let need = front.engine.capabilities().slots();
                    let fits = in_flight == 0
                        || cfg.max_slots == 0
                        || active_slots + need <= cfg.max_slots;
                    if !fits {
                        break;
                    }
                    let (slot, entry) = pending.pop_front().unwrap();
                    launch_one(slot, entry, &mut active_slots, &mut in_flight);
                }
            }

            if in_flight == 0 && pending.is_empty() {
                break;
            }

            let wait = if let Some((_, front)) = pending.front() {
                let until_delay = front.delay.saturating_sub(start.elapsed());
                let until_dl = base_ctx.deadline.saturating_duration_since(Instant::now());
                until_delay.min(until_dl).min(Duration::from_millis(25))
            } else {
                Duration::from_millis(25)
            };

            match rx.recv_timeout(wait) {
                Ok((slot, name, eng_slots, ms, r)) => {
                    in_flight -= 1;
                    active_slots = active_slots.saturating_sub(eng_slots);
                    let was_cancelled = base_ctx.cancel.is_cancelled();
                    let (outcome, depth) = match &r {
                        Err(_) => (Some(RunOutcome::Error), None),
                        Ok(res) => match &res.verdict {
                            Verdict::Unsafe { depth } => (None, *depth),
                            Verdict::Safe => (None, None),
                            Verdict::Unknown { reason } => {
                                let o = if was_cancelled {
                                    RunOutcome::Killed
                                } else if Instant::now() >= base_ctx.deadline
                                    || reason.contains("timeout")
                                {
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
                                    base_ctx.cancel.cancel();
                                    pending.clear();
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
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        // Release senders so the progress forwarder exits once every worker joins.
        drop(tx);
        base_ctx.progress = None;
        finished.store(true, Ordering::SeqCst);
        (out, profile)
    });

    out.wall_ms = start.elapsed().as_millis();
    out.lemma_stats = active_hub.as_ref().map(|h| h.stats());
    let winner = out.winner.as_ref().map(|w| w.engine.clone());
    let bounds = bounds.into_inner().unwrap();
    profile.sort_by_key(|p| p.0);
    out.profile = profile
        .into_iter()
        .map(|(_, engine, ms, outcome, depth)| {
            let outcome = outcome.unwrap_or(if winner.as_deref() == Some(engine.as_str()) {
                RunOutcome::Won
            } else {
                RunOutcome::Lost
            });
            let depth = depth.or_else(|| bounds.get(&engine).copied());
            EngineRun {
                engine,
                outcome,
                ms,
                depth,
            }
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
    use std::sync::atomic::AtomicUsize;
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
                    let reason = ctx.stop_reason().to_string();
                    return Ok(EngineResult {
                        engine: self.name.into(),
                        verdict: Verdict::Unknown { reason },
                        wall_ms: t.elapsed().as_millis(),
                        witness: None,
                        certificate: None,
                    });
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(EngineResult {
                engine: self.name.into(),
                verdict: self.verdict.clone(),
                wall_ms: t.elapsed().as_millis(),
                witness: None,
                certificate: None,
            })
        }
    }

    struct CountingFake {
        name: &'static str,
        delay_ms: u64,
        verdict: Verdict,
        slots: u8,
        launched: Arc<AtomicUsize>,
        concurrent: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    impl Engine for CountingFake {
        fn name(&self) -> &str {
            self.name
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                formats: vec![ModelFormat::Btor2],
                worker_slots: self.slots,
                ..Default::default()
            }
        }
        fn check(&self, _req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
            let slots = usize::from(self.slots.max(1));
            self.launched.fetch_add(1, Ordering::SeqCst);
            let now = self.concurrent.fetch_add(slots, Ordering::SeqCst) + slots;
            self.peak.fetch_max(now, Ordering::SeqCst);
            let t = Instant::now();
            let mut verdict = self.verdict.clone();
            while t.elapsed() < Duration::from_millis(self.delay_ms) {
                if ctx.should_stop() {
                    verdict = Verdict::Unknown {
                        reason: ctx.stop_reason().to_string(),
                    };
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            self.concurrent.fetch_sub(slots, Ordering::SeqCst);
            Ok(EngineResult {
                engine: self.name.into(),
                verdict,
                wall_ms: t.elapsed().as_millis(),
                witness: None,
                certificate: None,
            })
        }
    }

    fn req() -> CheckRequest {
        CheckRequest::new(
            "m.btor2".into(),
            ModelFormat::Btor2,
            Duration::from_secs(10),
        )
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
            Arc::new(Fake {
                name: "broken",
                delay_ms: 0,
                verdict: Verdict::Safe,
            }),
        ];
        let (tx, rx) = mpsc::channel();
        let o = run(&engines, &req(), Some(tx));
        let p: Vec<_> = o
            .profile
            .iter()
            .map(|r| (r.engine.as_str(), r.outcome, r.depth))
            .collect();
        assert_eq!(
            p,
            [
                ("slow", RunOutcome::Killed, Some(3)),
                ("fast", RunOutcome::Won, Some(7)),
                ("broken", RunOutcome::Error, None),
            ]
        );
        assert!(o.profile[1].ms >= 20 && o.profile[1].ms <= o.wall_ms);
        assert_eq!(rx.iter().filter(|p| p.bound == Some(3)).count(), 2);
        let json = serde_json::to_value(&o.profile[2]).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"engine": "broken", "outcome": "error", "ms": o.profile[2].ms})
        );
    }

    #[test]
    fn engines_out_of_time_are_timeouts() {
        let engines: Vec<Arc<dyn Engine>> = vec![Arc::new(Fake {
            name: "slow",
            delay_ms: 5_000,
            verdict: Verdict::Safe,
        })];
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

    #[test]
    fn staged_wave_skips_delayed_engines_when_wave0_wins() {
        let w1_launched = Arc::new(AtomicUsize::new(0));
        let zero = Arc::new(AtomicUsize::new(0));
        let sched = vec![
            ScheduledEngine::immediate(Arc::new(Fake {
                name: "fast-w0",
                delay_ms: 20,
                verdict: Verdict::Safe,
            })),
            ScheduledEngine::delayed(
                Arc::new(CountingFake {
                    name: "reserve-w1",
                    delay_ms: 500,
                    verdict: Verdict::Safe,
                    slots: 1,
                    launched: w1_launched.clone(),
                    concurrent: zero.clone(),
                    peak: zero,
                }),
                Duration::from_millis(500),
            ),
        ];
        let o = run_scheduled(
            &sched,
            &req(),
            None,
            None,
            None,
            ScheduleConfig { max_slots: 4 },
        );
        assert_eq!(verdict(&o), Verdict::Safe);
        assert_eq!(o.winner.as_ref().unwrap().engine, "fast-w0");
        assert_eq!(
            w1_launched.load(Ordering::SeqCst),
            0,
            "Wave-1 engine must never spawn when Wave 0 wins first"
        );
    }

    #[test]
    fn staged_wave_launches_reserve_and_respects_slot_cap() {
        let launched = Arc::new(AtomicUsize::new(0));
        let conc = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mk = |name: &'static str, delay_ms: u64, verdict: Verdict, slots: u8| {
            Arc::new(CountingFake {
                name,
                delay_ms,
                verdict,
                slots,
                launched: launched.clone(),
                concurrent: conc.clone(),
                peak: peak.clone(),
            }) as Arc<dyn Engine>
        };
        let sched = vec![
            ScheduledEngine::immediate(mk(
                "w0-heavy",
                60,
                Verdict::Unknown {
                    reason: "gave up".into(),
                },
                2,
            )),
            ScheduledEngine::immediate(mk(
                "w0-light",
                500,
                Verdict::Unknown {
                    reason: "slow".into(),
                },
                1,
            )),
            // Cannot launch until w0-heavy frees its 2 slots under max_slots=3.
            ScheduledEngine::delayed(
                mk("w1-winner", 20, Verdict::Safe, 2),
                Duration::from_millis(40),
            ),
        ];
        let o = run_scheduled(
            &sched,
            &req(),
            None,
            None,
            None,
            ScheduleConfig { max_slots: 3 },
        );
        assert_eq!(verdict(&o), Verdict::Safe);
        assert_eq!(o.winner.as_ref().unwrap().engine, "w1-winner");
        assert_eq!(launched.load(Ordering::SeqCst), 3);
        assert!(
            peak.load(Ordering::SeqCst) <= 3,
            "peak concurrent slots {} exceeded cap 3",
            peak.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn early_wave0_exhaustion_promotes_delayed_wave_immediately() {
        let sched = vec![
            ScheduledEngine::immediate(Arc::new(Fake {
                name: "w0-abort",
                delay_ms: 10,
                verdict: Verdict::Unknown {
                    reason: "node cap".into(),
                },
            })),
            // Large delay: must be promoted immediately once w0-abort finishes.
            ScheduledEngine::delayed(
                Arc::new(Fake {
                    name: "w1-fast",
                    delay_ms: 15,
                    verdict: Verdict::Safe,
                }),
                Duration::from_secs(5),
            ),
        ];
        let o = run_scheduled(
            &sched,
            &req(),
            None,
            None,
            None,
            ScheduleConfig { max_slots: 4 },
        );
        assert_eq!(verdict(&o), Verdict::Safe);
        assert_eq!(o.winner.as_ref().unwrap().engine, "w1-fast");
        assert!(
            o.wall_ms < 500,
            "must promote delayed wave when Wave 0 empties early, got {} ms",
            o.wall_ms
        );
    }
}
