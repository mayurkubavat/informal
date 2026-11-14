//! `informal split`: prove a property that is a wide conjunction part by
//! part.
//!
//! When the bad output is an OR of several terms
//! ([`informal_aig::split::terms`]), each term on the same design is a
//! property of its own. The driver writes one model per term, proves
//! them with IC3 a few at a time with the invariant store on (every
//! certified invariant is offered to the terms that follow, see
//! `docs/engines.md`), and then proves the original property with the
//! store holding all of them. Terms run in rounds of doubling length, so
//! the easy ones finish first and feed the hard ones.
//!
//! Nothing here is trusted for the verdict. A term's SAFE is certified by
//! the engine on the term's model; the final SAFE or UNSAFE is an
//! ordinary certified engine run on the model as given.

use anyhow::{bail, Context};
use informal_aig::{Aig, Init, Lit};
use informal_engine::{portfolio, CheckRequest, Engine, ModelFormat, RunContext, Verdict};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The engine that proves the terms and the original.
const PROVER: &str = "informal-ic3";

pub struct Options {
    pub model: PathBuf,
    pub timeout: Duration,
    /// Terms proved at once.
    pub jobs: usize,
    /// Stop opening OR nodes at this many terms.
    pub max_terms: usize,
    /// Length of the first round's slice per term; it doubles each round.
    pub slice: Duration,
    pub workdir: PathBuf,
    /// Only write the term models into `workdir` and list them.
    pub write_only: bool,
}

/// One term's state after the rounds.
#[derive(Debug, Clone)]
pub struct Term {
    pub file: PathBuf,
    pub verdict: Verdict,
    /// The round that gave the verdict (from 1), and its time.
    pub round: u32,
    pub wall_ms: u128,
}

pub struct Outcome {
    pub verdict: Verdict,
    pub by: String,
    pub terms: Vec<Term>,
}

/// The term models of `g` in `dir`, as `term-NNN.aig`.
fn write_terms(g: &Aig, terms: &[Lit], dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    terms
        .iter()
        .enumerate()
        .map(|(i, &t)| {
            let f = dir.join(format!("term-{i:03}.aig"));
            let h = informal_aig::split::with_bad(g, t);
            std::fs::write(&f, informal_aig::aiger::write_binary(&h))
                .with_context(|| format!("writing {}", f.display()))?;
            Ok(f)
        })
        .collect()
}

fn request(model: &Path, timeout: Duration) -> CheckRequest {
    CheckRequest {
        model: model.to_path_buf(),
        format: ModelFormat::Aiger,
        timeout,
    }
}

fn unknown(reason: impl Into<String>) -> Verdict {
    Verdict::Unknown {
        reason: reason.into(),
    }
}

pub fn run(o: &Options, engines: &[Arc<dyn Engine>]) -> anyhow::Result<Outcome> {
    let t0 = Instant::now();
    let deadline = t0 + o.timeout;
    let g = informal_bmc::load_aig(&o.model)?;
    if g.latches.iter().any(|l| matches!(l.init, Init::Expr(_))) {
        // AIGER cannot carry such a reset, so the term models would differ
        // from the model given.
        bail!("the model has latches with a non-constant reset value; split needs constant or free resets");
    }
    let lits = informal_aig::split::terms(&g, o.max_terms);
    if lits.len() < 2 {
        bail!(
            "the bad output of {} is not an OR of several terms; nothing to split",
            o.model.display()
        );
    }
    let dir = o.workdir.join(format!("{}", std::process::id()));
    let files = write_terms(&g, &lits, &dir)?;
    let mut terms: Vec<Term> = lits
        .iter()
        .zip(&files)
        .map(|(_, f)| Term {
            file: f.clone(),
            verdict: unknown("not run"),
            round: 0,
            wall_ms: 0,
        })
        .collect();
    if o.write_only {
        return Ok(Outcome {
            verdict: unknown("term models written, nothing proved"),
            by: String::new(),
            terms,
        });
    }
    let prover = engines
        .iter()
        .find(|e| e.name() == PROVER)
        .with_context(|| format!("this build has no {PROVER} engine (CaDiCaL missing?)"))?
        .clone();
    // The store carries invariants between the terms; a private one unless
    // the caller named one. Set before any engine thread starts.
    if std::env::var_os("INFORMAL_INV_STORE").is_none_or(|d| d.is_empty()) {
        let store = dir.join("store");
        std::fs::create_dir_all(&store)?;
        std::env::set_var("INFORMAL_INV_STORE", &store);
    }
    // The last tenth of the budget (5 s at least) is for the original.
    let reserve = (o.timeout / 10).max(Duration::from_secs(5)).min(o.timeout);
    let rounds_end = deadline - reserve;
    let mut round = 0u32;
    let mut refuted: Option<usize> = None;
    'rounds: loop {
        let pending: Vec<usize> = (0..terms.len())
            .filter(|&i| !terms[i].verdict.is_definitive())
            .collect();
        if pending.is_empty() || Instant::now() >= rounds_end {
            break;
        }
        round += 1;
        let slice = o.slice * (1u32 << (round - 1).min(16));
        let next = AtomicUsize::new(0);
        let found = Mutex::new(Vec::<(usize, Verdict, u128)>::new());
        let stop = informal_engine::CancelToken::new();
        std::thread::scope(|sc| {
            for _ in 0..o.jobs.max(1).min(pending.len()) {
                sc.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&i) = pending.get(k) else { return };
                    let now = Instant::now();
                    if now >= rounds_end || stop.is_cancelled() {
                        return;
                    }
                    let end = rounds_end.min(now + slice);
                    let ctx = RunContext {
                        cancel: stop.clone(),
                        deadline: end,
                        progress: None,
                    };
                    let v = match prover.check(&request(&files[i], end - now), &ctx) {
                        Ok(r) => r.verdict,
                        Err(e) => unknown(format!("{e:#}")),
                    };
                    if matches!(v, Verdict::Unsafe { .. }) {
                        // One reachable term settles the property.
                        stop.cancel();
                    }
                    found
                        .lock()
                        .unwrap()
                        .push((i, v, now.elapsed().as_millis()));
                });
            }
        });
        // A round in which nothing was settled and no run used up its
        // slice would repeat itself: the engine gave up on its own.
        let mut again = false;
        for (i, v, ms) in found.into_inner().unwrap() {
            again |= v.is_definitive() || ms * 10 >= slice.as_millis() * 9;
            if v.is_definitive() || !terms[i].verdict.is_definitive() {
                terms[i] = Term {
                    verdict: v,
                    round,
                    wall_ms: ms,
                    ..terms[i].clone()
                };
            }
            if matches!(terms[i].verdict, Verdict::Unsafe { .. }) {
                refuted = Some(i);
            }
        }
        let proved = terms.iter().filter(|t| t.verdict == Verdict::Safe).count();
        eprintln!(
            "split: round {round} ({:.0} s per term): {proved} of {} terms proved, {:.1} s",
            slice.as_secs_f64(),
            terms.len(),
            t0.elapsed().as_secs_f64()
        );
        if refuted.is_some() || !again {
            break 'rounds;
        }
    }
    // The verdict comes from an ordinary run on the model as given.
    let left = deadline.saturating_duration_since(Instant::now());
    let all = terms.iter().all(|t| t.verdict == Verdict::Safe);
    let (verdict, by) = if refuted.is_some() {
        // A state where a term holds is a bad state: the bug finders get
        // the rest of the budget on the original.
        let bmc: Vec<Arc<dyn Engine>> = engines
            .iter()
            .filter(|e| e.name().starts_with("informal-bmc"))
            .cloned()
            .collect();
        let out = portfolio::run(&bmc, &request(&o.model, left), None);
        let by = out
            .winner
            .as_ref()
            .map_or(String::new(), |w| w.engine.clone());
        (portfolio::verdict(&out), by)
    } else if all {
        let ctx = RunContext::new(left);
        let v = match prover.check(&request(&o.model, left), &ctx) {
            Ok(r) => r.verdict,
            Err(e) => unknown(format!("{e:#}")),
        };
        (v, PROVER.to_string())
    } else {
        (unknown("timeout"), String::new())
    };
    Ok(Outcome { verdict, by, terms })
}

/// Remove the term models and the private store (kept with `write_only`).
pub fn clean(o: &Options) {
    if !o.write_only {
        let _ = std::fs::remove_dir_all(o.workdir.join(format!("{}", std::process::id())));
    }
}
