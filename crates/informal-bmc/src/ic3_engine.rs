//! Single-threaded (`informal-ic3`, `informal-ic3-cadical`) and multi-worker
//! (`informal-ic3-mt`) IC3/PDR engine wrappers.

use crate::hub::{seed_from_hub, HubBridge};
use crate::prep::{fold_on, prepare_ctx_fold, result, solver, unknown, Checked, FORMATS};
use crate::verify::{certified, mine_invariant, stored_invariant, unsafe_certified_at};
use crate::{cnf, ic3, invariant};
use informal_engine::{
    Capabilities, CheckRequest, Effort, Engine, EngineDomain, EngineKind, EngineResult, RunContext,
    Verdict,
};
use informal_sat::Sat;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// `INFORMAL_IC3_SEED`: seed for the first phases in IC3's own solver
/// (0 or unset: the fixed default path).
fn ic3_seed() -> u64 {
    static SEED: OnceLock<u64> = OnceLock::new();
    *SEED.get_or_init(|| {
        std::env::var("INFORMAL_IC3_SEED")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(0)
    })
}

/// H14: `informal-ic3-mt`'s workers get different seeds, and the third one
/// the default settings with derived predicates in place of the no-CTG
/// settings. On by default; `INFORMAL_IC3MT_DIVERSE=0` restores four
/// settings on one seed.
fn ic3mt_diverse() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("INFORMAL_IC3MT_DIVERSE").map_or(true, |x| x != "0"))
}

/// IC3 / PDR on the AIG (see [`ic3`]). `mini` picks informal's own small CDCL
/// solver for the frame and lifting solvers instead of CaDiCaL.
pub struct Ic3Engine {
    pub mini: bool,
}

impl Engine for Ic3Engine {
    fn name(&self) -> &str {
        if self.mini {
            "informal-ic3"
        } else {
            "informal-ic3-cadical"
        }
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: FORMATS.to_vec(),
            proves: true,
            finds_bugs: true,
            liveness: true,
            lemma_export: true,
            lemma_import: true,
            domain: EngineDomain::BitLevel,
            kind: EngineKind::Ic3,
            min_effort: if self.mini { Effort::Low } else { Effort::High },
            certifies: true,
            ..Default::default()
        }
    }
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx_fold(req, ctx, fold_on()) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let mini = self.mini;
        if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
            let t = cnf::template(&g);
            eprintln!(
                "template: {} latches {} inputs {} ands, {} live vars, {} clauses, {} lits",
                g.latches.len(),
                g.inputs.len(),
                g.num_ands(),
                t.live.iter().filter(|&&x| x).count(),
                t.clauses.len(),
                t.clauses.iter().map(|c| c.len()).sum::<usize>()
            );
        }
        // H16 (INFORMAL_IC3_RESTART=<seconds>): run in slices of that
        // length, doubling; between slices keep only the lemmas that are
        // inductive together and start again on another seed.
        let high_effort = matches!(req.effort, Effort::High | Effort::Max);
        let total_left = ctx.deadline.saturating_duration_since(Instant::now());
        let restart: f64 = std::env::var("INFORMAL_IC3_RESTART")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or_else(|| {
                if high_effort && total_left >= Duration::from_secs(12) {
                    5.0
                } else {
                    0.0
                }
            });
        let mut kept = stored_invariant(&g, ctx, self.name());
        let (mut hub_bridge, from_hub) = seed_from_hub(&g, ctx, self.name());
        if !from_hub.is_empty() {
            kept.extend(from_hub);
            kept.sort();
            kept.dedup();
        }
        let mut attempt = 0u32;
        let (ic3, v) = loop {
            let mut slice = ctx.clone();
            if restart > 0.0 {
                let len = Duration::from_secs_f64(restart * f64::from(1u32 << attempt.min(16)));
                slice.deadline = ctx.deadline.min(Instant::now() + len);
            }
            let c = slice.clone();
            let seed = ic3_seed().wrapping_add(u64::from(attempt) * 7919);
            let make = move || -> Box<dyn Sat> {
                if !mini {
                    return solver(&c);
                }
                let mut s = informal_sat::Mini::with_seed(seed);
                let (cancel, deadline) = (c.cancel.clone(), c.deadline);
                s.set_terminate(Box::new(move || {
                    cancel.is_cancelled()
                        || Instant::now() >= deadline
                        || informal_engine::memory::exceeded()
                }));
                Box::new(s)
            };
            let Some(mut ic3) =
                ic3::Ic3::with_config_until(&g, Box::new(make), ic3::Config::default(), &|| {
                    slice.should_stop()
                })
            else {
                let reason = if slice.should_stop() {
                    slice.stop_reason()
                } else {
                    "latch inits that are not constants"
                };
                return Ok(result(self.name(), unknown(reason), t0));
            };
            if attempt == 0
                && (std::env::var_os("INFORMAL_IC3_MINE").is_some()
                    || (high_effort
                        && ctx.deadline.saturating_duration_since(Instant::now())
                            >= Duration::from_millis(500)))
            {
                let mined = mine_invariant(&g, ctx, self.name());
                if let Some(h) = &hub_bridge {
                    h.publish_standing(&mined);
                }
                ic3.assume_invariant(mined);
            }
            if std::env::var_os("INFORMAL_IC3_HOUDINI").is_none() {
                // H2 on by default here: gap set 1 -> 3 at 300 s, sample 20 -> 21.
                let d = ic3::Config::default();
                let cfg = if high_effort {
                    ic3::Config {
                        houdini_every: 4,
                        max_ctgs: d.max_ctgs.max(8),
                        max_ctg_depth: d.max_ctg_depth.max(3),
                        derive: true,
                        cti_mine: true,
                        coupled_push: true,
                        ..d
                    }
                } else {
                    ic3::Config {
                        houdini_every: 4,
                        ..d
                    }
                };
                ic3.set_config(cfg);
            }
            ic3.assume_invariant(kept.clone());
            if let Some(h) = hub_bridge.take() {
                ic3.attach_hub(h);
            }
            let v = ic3.check(&slice);
            if restart == 0.0 || !matches!(v, Verdict::Unknown { .. }) || ctx.should_stop() {
                break (ic3, v);
            }
            hub_bridge = ic3.take_hub();
            // The slice ran out: what survives is the inductive core.
            let th = Instant::now();
            let mut cands = ic3.lemmas();
            cands.extend(ic3.standing_invariant());
            let n = cands.len();
            let mut h = ctx.clone();
            let rest = ctx.deadline.saturating_duration_since(th);
            h.deadline = th + (rest / 10).min(Duration::from_secs(20));
            if let Some(core) = invariant::houdini(&g, cands, || solver(&h), &h) {
                if let Some(hub) = &hub_bridge {
                    hub.publish_standing(&core);
                }
                kept = core;
            }
            if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
                eprintln!(
                    "{}: restart {} at frame {}: {} of {n} lemmas kept, {:.2} s",
                    self.name(),
                    attempt + 1,
                    ic3.depth(),
                    kept.len(),
                    th.elapsed().as_secs_f64()
                );
            }
            attempt += 1;
        };
        let v = certified(req, &g, &ic3, v, ctx);
        if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
            print_ic3_stats(self.name(), &ic3);
        }
        Ok(result(
            self.name(),
            unsafe_certified_at(req, v, ctx, None, g.folded.is_some()),
            t0,
        ))
    }
}

fn print_ic3_stats(name: &str, ic3: &ic3::Ic3) {
    eprintln!(
        "{name}: frames {} lemmas {} ctps {} queries {} ({:.2} s) lift {:.2} s ({} fallbacks)",
        ic3.depth(),
        ic3.stats_lemmas,
        ic3.stats_ctps,
        ic3.stats_queries,
        ic3.stats_query_time.as_secs_f64(),
        ic3.stats_lift_time.as_secs_f64(),
        ic3.stats_lift_fallbacks
    );
    eprintln!("frames: {:?}", ic3.frame_sizes());
    eprintln!(
        "derived predicates: {} defined, {} lemmas merged into them, {} of {} lemmas widened",
        ic3.stats_aux, ic3.stats_aux_merged, ic3.stats_widened, ic3.stats_widen_tried
    );
    eprintln!(
        "promotions: {} of {} tried",
        ic3.stats_promoted, ic3.stats_promote_tried
    );
    eprintln!(
        "ctg lemmas: {} in drops that succeeded, {} in drops that failed",
        ic3.stats_ctg_ok, ic3.stats_ctg_fail
    );
    let mic = ic3.stats_mic.max(1) as f64;
    eprintln!(
        "mic: {} calls ({} parent hits), cube {:.1} -> lemma {:.1} literals, drops ok {} fail {} ({:.1}%), {} failed after 20 failures in a row, {} skipped",
        ic3.stats_mic,
        ic3.stats_parent_hits,
        ic3.stats_mic_in_len as f64 / mic,
        ic3.stats_mic_len as f64 / mic,
        ic3.stats_drop_ok,
        ic3.stats_drop_fail,
        100.0 * ic3.stats_drop_ok as f64 / (ic3.stats_drop_ok + ic3.stats_drop_fail).max(1) as f64,
        ic3.stats_drop_fail_streak,
        ic3.stats_drop_skipped
    );
    let n = informal_sat::mini::SOLVES.load(Ordering::Relaxed).max(1);
    eprintln!(
        "mini: solves {} decisions/solve {:.1} domain/solve {:.0} trail-at-end/solve {:.0} conflicts {}",
        n,
        informal_sat::mini::DECISIONS.load(Ordering::Relaxed) as f64 / n as f64,
        informal_sat::mini::DOMAIN_VARS.load(Ordering::Relaxed) as f64 / n as f64,
        informal_sat::mini::PROPS.load(Ordering::Relaxed) as f64 / n as f64,
        0
    );
    let dn = ic3::DIAG_N.load(Ordering::Relaxed).max(1) as f64;
    eprintln!(
        "domain: latches+inputs {:.0}, live (not eliminated) {:.0} per query",
        ic3::DIAG_LI.load(Ordering::Relaxed) as f64 / dn,
        ic3::DIAG_LIVE.load(Ordering::Relaxed) as f64 / dn
    );
    let ns = informal_sat::mini::SAT_SOLVES
        .load(Ordering::Relaxed)
        .max(1);
    eprintln!(
        "mini: SAT answers {} ({:.1} s, {:.0} decisions each); all solves {:.1} s; max vars {}",
        ns,
        informal_sat::mini::SAT_NANOS.load(Ordering::Relaxed) as f64 / 1e9,
        informal_sat::mini::SAT_DECISIONS.load(Ordering::Relaxed) as f64 / ns as f64,
        informal_sat::mini::ALL_NANOS.load(Ordering::Relaxed) as f64 / 1e9,
        informal_sat::mini::MAX_VARS.load(Ordering::Relaxed)
    );
}

/// Several IC3s with different generalization settings and solvers, on
/// threads, sharing lemmas through one [`ic3::LemmaPool`]. The first
/// definitive verdict wins and stops the rest.
pub struct Ic3Mt {
    pub workers: Vec<(bool, ic3::Config)>,
}

impl Default for Ic3Mt {
    fn default() -> Self {
        use ic3::Config;
        let d = Config::default();
        let diverse = ic3mt_diverse();
        Ic3Mt {
            workers: vec![
                // Worker 0: clean reference Mini IC3 with periodic Houdini (H2).
                (
                    true,
                    Config {
                        houdini_every: if d.houdini_every == 0 {
                            4
                        } else {
                            d.houdini_every
                        },
                        ..d
                    },
                ),
                (false, d),
                if diverse {
                    // H14: diverse seed with derived predicates (H13), DynAMic,
                    // CTI mining, and H3 coupled push.
                    (
                        true,
                        Config {
                            houdini_every: 4,
                            derive: true,
                            dynamic_order: true,
                            cti_mine: true,
                            coupled_push: true,
                            ..d
                        },
                    )
                } else {
                    (
                        true,
                        Config {
                            max_ctgs: 0,
                            max_ctg_depth: 0,
                            reverse_drop: true,
                            parent_lemma: true,
                            parent_ctp: true,
                            dynamic_order: true,
                            ..d
                        },
                    )
                },
                (
                    true,
                    Config {
                        max_ctgs: 5,
                        max_ctg_depth: 2,
                        reverse_drop: true,
                        max_ctps: 3,
                        parent_lemma: true,
                        parent_ctp: true,
                        exctg: true,
                        coupled_push: true,
                        ..d
                    },
                ),
            ],
        }
    }
}

impl Engine for Ic3Mt {
    fn name(&self) -> &str {
        "informal-ic3-mt"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: FORMATS.to_vec(),
            proves: true,
            finds_bugs: true,
            liveness: true,
            lemma_export: true,
            lemma_import: true,
            domain: EngineDomain::BitLevel,
            kind: EngineKind::Ic3,
            min_effort: Effort::Standard,
            certifies: true,
            multi_worker: true,
            worker_slots: 4,
            ..Default::default()
        }
    }
    /// Four IC3 workers, each with its own solvers.
    fn shed_rank(&self) -> u32 {
        2
    }
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx_fold(req, ctx, fold_on()) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let pool = Arc::new(ic3::LemmaPool::default());
        // H15 + Phase 2: proved once here, assumed by every worker.
        let mut stored = stored_invariant(&g, ctx, self.name());
        let (hub_seed, from_hub) = seed_from_hub(&g, ctx, self.name());
        let seed_cursor = hub_seed.as_ref().map_or(0, HubBridge::standing_cursor);
        let seed_pub_id = hub_seed.as_ref().map(HubBridge::publisher_id);
        if !from_hub.is_empty() {
            stored.extend(from_hub);
            stored.sort();
            stored.dedup();
        }
        if matches!(req.effort, Effort::High | Effort::Max)
            && ctx.deadline.saturating_duration_since(Instant::now()) >= Duration::from_millis(500)
        {
            let mined = mine_invariant(&g, ctx, self.name());
            if let Some(hub) = &hub_seed {
                hub.publish_standing(&mined);
            }
            stored.extend(mined);
            stored.sort();
            stored.dedup();
        }
        let mut workers = self.workers.clone();
        if matches!(req.effort, Effort::High | Effort::Max) {
            let d = ic3::Config::default();
            if let Some((_, c0)) = workers.first_mut() {
                c0.max_ctgs = c0.max_ctgs.max(8);
                c0.max_ctg_depth = c0.max_ctg_depth.max(3);
                c0.derive = true;
            }
            if req.effort == Effort::Max {
                workers.push((
                    false,
                    ic3::Config {
                        max_ctgs: 8,
                        max_ctg_depth: 3,
                        derive: true,
                        houdini_every: 4,
                        cti_mine: true,
                        coupled_push: true,
                        ..d
                    },
                ));
                workers.push((
                    true,
                    ic3::Config {
                        max_ctgs: 8,
                        max_ctg_depth: 3,
                        activity_order: true,
                        lemma_domain: true,
                        parent_lemma: true,
                        exctg: true,
                        stall_push: true,
                        ..d
                    },
                ));
            }
        }
        // Workers stop on our token: cancelled by the winner or by `ctx`.
        let mut inner = ctx.clone();
        inner.cancel = informal_engine::CancelToken::new();
        let done = AtomicBool::new(false);
        let stats = std::env::var_os("INFORMAL_IC3_STATS").is_some();
        let verdicts: Vec<Checked> = std::thread::scope(|sc| {
            let (g, inner, done) = (&g, &inner, &done);
            sc.spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    if ctx.cancel.is_cancelled() {
                        inner.cancel.cancel();
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
            let hs: Vec<_> = workers
                .iter()
                .enumerate()
                .map(|(id, &(mini, cfg))| {
                    let pool = pool.clone();
                    let stored = stored.clone();
                    sc.spawn(move || {
                        let c = inner.clone();
                        let make = move || -> Box<dyn Sat> {
                            if !mini {
                                return solver(&c);
                            }
                            // H14: every worker on its own path. Worker 0
                            // keeps the run's seed.
                            let seed = if ic3mt_diverse() && id > 0 {
                                ic3_seed().wrapping_mul(1000).wrapping_add(id as u64)
                            } else {
                                ic3_seed()
                            };
                            let mut s = informal_sat::Mini::with_seed(seed);
                            let (cancel, deadline) = (c.cancel.clone(), c.deadline);
                            s.set_terminate(Box::new(move || {
                                cancel.is_cancelled()
                                    || Instant::now() >= deadline
                                    || informal_engine::memory::exceeded()
                            }));
                            Box::new(s)
                        };
                        let Some(mut ic3) = ic3::Ic3::with_config_until(
                            g,
                            Box::new(make),
                            cfg,
                            &|| inner.should_stop(),
                        ) else {
                            let reason = if inner.should_stop() {
                                inner.stop_reason()
                            } else {
                                "latch inits that are not constants"
                            };
                            return unknown(reason).into();
                        };
                        ic3.share(pool, id);
                        if let Some(mut hub) = HubBridge::new(g, inner, self.name()) {
                            hub = hub
                                .with_standing_cursor(seed_cursor)
                                .with_frame_publish(
                                    id == 0 && informal_engine::LemmaHub::frames_enabled(),
                                );
                            if let Some(pid) = seed_pub_id {
                                hub = hub.with_publisher_id(pid);
                            }
                            ic3.attach_hub(hub);
                        }
                        ic3.assume_invariant(stored);
                        let v = ic3.check(inner);
                        let v = certified(req, g, &ic3, v, inner);
                        if v.verdict.is_definitive() {
                            inner.cancel.cancel();
                        }
                        if stats {
                            eprintln!(
                                "informal-ic3-mt[{id}]: {:?} frames {} lemmas {} imported {} of {} seen ({} same cube held, {} raised; {} import queries of {} queries; failed at their level: {} new, {} raises)",
                                v.verdict,
                                ic3.depth(),
                                ic3.stats_lemmas,
                                ic3.stats_imported,
                                ic3.stats_import_seen,
                                ic3.stats_import_dup,
                                ic3.stats_import_raised,
                                ic3.stats_import_queries,
                                ic3.stats_queries,
                                ic3.stats_import_first_fail,
                                ic3.stats_import_raise_fail
                            );
                        }
                        v
                    })
                })
                .collect();
            let vs = hs.into_iter().map(|h| h.join().unwrap()).collect();
            done.store(true, Ordering::Relaxed);
            vs
        });
        let v = verdicts
            .into_iter()
            .find(|v| v.verdict.is_definitive())
            .unwrap_or_else(|| unknown(ctx.stop_reason()).into());
        Ok(result(
            self.name(),
            unsafe_certified_at(req, v, ctx, None, g.folded.is_some()),
            t0,
        ))
    }
}
