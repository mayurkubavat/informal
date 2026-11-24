//! Shared bit-level model preparation, caching, and solver construction.
//!
//! Every bit-level in-house engine (`Bmc`, `KInduction`, `Ic3Engine`, `Ic3Mt`,
//! `KindIc3`, and future BDD/IMC/CAR engines) shares [`Prepared`]: a simplified,
//! optionally clock-folded and latch-correspondence-merged AIG together with its
//! cached CNF template and back-maps to the loaded model for independent
//! certification.

use crate::{cnf, scorr, store};
use informal_aig::preprocess::{self as pre, InputMap, LatchMap};
use informal_aig::Aig;
use informal_engine::{
    Certificate, CheckRequest, Effort, EngineResult, ModelFormat, RunContext, Verdict,
};
use informal_sat::Sat;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime};

/// Formats accepted by bit-level AIG engines.
pub const FORMATS: [ModelFormat; 2] = [ModelFormat::Aiger, ModelFormat::Btor2];

/// How a prepared model was clock-folded (see `preprocess::fold_clock`):
/// the clock's latch index and phase in the loaded model, and the
/// prepared model's literal for the fold's `ok` latch.
#[derive(Debug, Clone, Copy)]
pub struct Folded {
    pub clock: usize,
    pub phase: bool,
    pub ok: Option<informal_aig::Lit>,
}

/// The parts of a prepared model before scorr and the CNF template.
pub(crate) struct Loaded {
    pub(crate) aig: Aig,
    pub(crate) latch_map: LatchMap,
    pub(crate) input_map: InputMap,
    pub(crate) design_hash: u64,
    pub(crate) folded: Option<Folded>,
    /// The loaded model's latch literals, by latch index.
    pub(crate) latch_lits: Vec<informal_aig::Lit>,
}

/// The simplified model and the maps from the loaded model to it, or the
/// reason this engine cannot take it. With `fold`, a free-running clock
/// is folded first (H17): one step is then two of the loaded model.
pub(crate) fn prepare(req: &CheckRequest, fold: bool) -> Result<Loaded, String> {
    let g = informal_aig::load(&req.model).map_err(|e| format!("{e:#}"))?;
    if g.bads.is_empty() {
        return Err("model has no bad-state property".into());
    }
    // Only needed (and only computed) when the invariant store is on.
    let design_hash = store::dir().map_or(0, |_| store::design_hash(&g));
    if fold {
        if let Some((f, info, fl, fi)) = pre::fold_clock(&g) {
            let (s, _, map, imap) = pre::simplify_maps(&f);
            if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
                eprintln!(
                    "fold: clock latch {} (phase {}), {} -> {} ands; {} latches after simplification",
                    info.clock,
                    u8::from(info.phase),
                    g.num_ands(),
                    f.num_ands(),
                    s.latches.len()
                );
            }
            // The ok latch through the same map as the other latches.
            let ok = info.ok.and_then(|l| {
                let i = f.latches.iter().position(|x| x.lit == l)?;
                map[i]
            });
            return Ok(Loaded {
                latch_map: pre::compose(&fl, &f, &map),
                input_map: pre::compose_inputs(&fi, &f, &imap),
                aig: s,
                design_hash,
                folded: Some(Folded {
                    clock: info.clock,
                    phase: info.phase,
                    ok,
                }),
                latch_lits: g.latches.iter().map(|l| l.lit).collect(),
            });
        }
    }
    let (s, _, map, imap) = pre::simplify_maps(&g);
    Ok(Loaded {
        aig: s,
        latch_map: map,
        input_map: imap,
        design_hash,
        folded: None,
        latch_lits: g.latches.iter().map(|l| l.lit).collect(),
    })
}

/// The model as the engines see it: simplified, latch-merged, and its CNF
/// template, built once per model and process and shared by every engine
/// and worker that runs on it at the same time (derefs to the AIG).
pub struct Prepared {
    // Field order: the AIG is dropped before the template it keys.
    aig: Aig,
    _template: Arc<cnf::Template>,
    /// From the loaded model's latches to `aig`'s literals (certificates).
    pub latch_map: LatchMap,
    /// From the loaded model's inputs to `aig`'s (UNSAFE certificates).
    pub input_map: InputMap,
    /// [`store::design_hash`] of the loaded model (0 when the store is off).
    pub design_hash: u64,
    /// Set when the model was clock-folded: certificates then check two
    /// steps of the loaded model per step of this one.
    pub folded: Option<Folded>,
    /// What latch correspondence merged, as equalities between literals
    /// of the loaded model's latches (see [`crate::certify::equalities`]).
    pub merged: Vec<(informal_aig::Lit, informal_aig::Lit)>,
}

impl std::ops::Deref for Prepared {
    type Target = Aig;
    fn deref(&self) -> &Aig {
        &self.aig
    }
}

enum SlotState {
    Empty,
    Building,
    Ready(Weak<Prepared>),
    /// The model cannot be prepared (a load error): the same for everyone.
    Failed(String),
}

struct Slot {
    state: Mutex<SlotState>,
    ready: Condvar,
}

/// Model file, its length and mtime, scorr tier (`0` = off, `1` = standard,
/// `2` = deep), and whether the clock is folded.
type SlotKey = (PathBuf, u64, Option<SystemTime>, u8, bool);

/// Latch-correspondence tier for `effort` (`0` = off, `1` = standard, `2` = deep).
pub fn scorr_tier(effort: Effort) -> u8 {
    if std::env::var_os("INFORMAL_NO_SCORR").is_some() || effort == Effort::Low {
        0
    } else if matches!(effort, Effort::High | Effort::Max) {
        2
    } else {
        1
    }
}

static SLOTS: Mutex<Vec<(SlotKey, Arc<Slot>)>> = Mutex::new(Vec::new());

/// Prepare, run latch correspondence (`scorr`) within 5% of the time left
/// (at most 10 s), and build the shared CNF template; see [`Prepared`].
pub fn prepare_ctx(req: &CheckRequest, ctx: &RunContext) -> Result<Arc<Prepared>, String> {
    prepare_ctx_fold(req, ctx, false)
}

/// The IC3 and k-induction engines fold a free-running clock when the model has
/// one (H17: 13.3 against 6.0 solved on the seeded hard set).
/// `INFORMAL_FOLD=0` turns it off.
pub fn fold_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("INFORMAL_FOLD").map_or(true, |x| x != "0"))
}

/// [`prepare_ctx`], with the clock folded when `fold` is set and the
/// model has one. A folded model is a different model (other engines'
/// certificates do not apply to it), so it has its own slot.
pub fn prepare_ctx_fold(
    req: &CheckRequest,
    ctx: &RunContext,
    fold: bool,
) -> Result<Arc<Prepared>, String> {
    let stop = || {
        if ctx.should_stop() {
            Err(ctx.stop_reason().to_string())
        } else {
            Ok(())
        }
    };
    stop()?;
    let meta = std::fs::metadata(&req.model).ok();
    let key: SlotKey = (
        req.model.clone(),
        meta.as_ref().map_or(0, |m| m.len()),
        meta.and_then(|m| m.modified().ok()),
        scorr_tier(req.effort),
        fold,
    );
    let slot = {
        let mut slots = SLOTS.lock().unwrap_or_else(|p| p.into_inner());
        // Forget models nobody is using any more.
        slots.retain(|(_, s)| {
            Arc::strong_count(s) > 1
                || match &*s.state.lock().unwrap_or_else(|p| p.into_inner()) {
                    SlotState::Ready(w) => w.strong_count() > 0,
                    _ => false,
                }
        });
        match slots.iter().find(|(k, _)| *k == key) {
            Some((_, s)) => s.clone(),
            None => {
                let s = Arc::new(Slot {
                    state: Mutex::new(SlotState::Empty),
                    ready: Condvar::new(),
                });
                slots.push((key.clone(), s.clone()));
                s
            }
        }
    };
    let mut st = slot.state.lock().unwrap_or_else(|p| p.into_inner());
    loop {
        match &*st {
            SlotState::Ready(w) => {
                if let Some(p) = w.upgrade() {
                    drop(st);
                    stop()?;
                    return Ok(p);
                }
                *st = SlotState::Empty;
            }
            SlotState::Failed(e) => return Err(e.clone()),
            SlotState::Building => {
                st = slot
                    .ready
                    .wait_timeout(st, Duration::from_millis(50))
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
                if ctx.should_stop() {
                    return Err(ctx.stop_reason().to_string());
                }
            }
            SlotState::Empty => {
                *st = SlotState::Building;
                drop(st);
                let built = build_prepared(req, ctx, fold);
                let mut s = slot.state.lock().unwrap_or_else(|p| p.into_inner());
                let out = match built {
                    Ok(p) => {
                        let p = Arc::new(p);
                        cnf::share(&p.aig, &p._template);
                        *s = SlotState::Ready(Arc::downgrade(&p));
                        Ok(p)
                    }
                    Err(e) => {
                        *s = SlotState::Failed(e.clone());
                        Err(e)
                    }
                };
                drop(s);
                slot.ready.notify_all();
                let p = out?;
                stop()?;
                return Ok(p);
            }
        }
    }
}

fn build_prepared(req: &CheckRequest, ctx: &RunContext, fold: bool) -> Result<Prepared, String> {
    let Loaded {
        aig: g,
        latch_map: map,
        input_map: imap,
        design_hash,
        mut folded,
        latch_lits,
    } = prepare(req, fold)?;
    let mut merged = Vec::new();
    let tier = scorr_tier(req.effort);
    let (g, map, imap) = if tier == 0 || ctx.should_stop() {
        (g, map, imap)
    } else {
        let now = Instant::now();
        let left = ctx.deadline.saturating_duration_since(now);
        let mut c = ctx.clone();
        // INFORMAL_SCORR_TL=<seconds> overrides the budget (experiments).
        let default_budget = if tier >= 2 {
            (left / 10).min(Duration::from_secs(15))
        } else {
            (left / 20).min(Duration::from_secs(10))
        };
        let budget = std::env::var("INFORMAL_SCORR_TL")
            .ok()
            .and_then(|x| x.parse().ok())
            .map(Duration::from_secs_f64)
            .unwrap_or(default_budget);
        c.deadline = now + budget;
        match scorr::latch_correspondence(&g, solver(&c), &c) {
            Some((h, st, hmap, himap)) => {
                if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
                    eprintln!(
                        "scorr: merged {} latches ({} -> {}), {} refinements, {:.2} s",
                        st.merged,
                        g.latches.len(),
                        h.latches.len(),
                        st.refinements,
                        now.elapsed().as_secs_f64()
                    );
                }
                // The equalities in terms of the loaded model: one loaded
                // literal for each latch of the model scorr ran on.
                let mut back: HashMap<u32, informal_aig::Lit> = HashMap::new();
                for (&x, m) in latch_lits.iter().zip(&map) {
                    if let Some(l) = *m {
                        if l != informal_aig::TRUE && l != informal_aig::FALSE {
                            back.entry(informal_aig::var(l)).or_insert(x ^ (l & 1));
                        }
                    }
                }
                let to_loaded = |l: informal_aig::Lit| {
                    if l == informal_aig::TRUE || l == informal_aig::FALSE {
                        Some(l)
                    } else {
                        back.get(&informal_aig::var(l)).map(|&x| x ^ (l & 1))
                    }
                };
                for &(x, y) in &st.equalities {
                    if let (Some(x), Some(y)) = (to_loaded(x), to_loaded(y)) {
                        if x != informal_aig::TRUE && x != informal_aig::FALSE {
                            merged.push((x, y));
                        }
                    }
                }
                let m = pre::compose(&map, &g, &hmap);
                let im = pre::compose_inputs(&imap, &g, &himap);
                if let Some(f) = folded.as_mut() {
                    f.ok = pre::compose(&[f.ok], &g, &hmap)[0];
                }
                (h, m, im)
            }
            None => (g, map, imap),
        }
    };
    let t = cnf::build_until(&g, &|| ctx.should_stop());
    Ok(Prepared {
        aig: g,
        _template: Arc::new(t),
        latch_map: map,
        input_map: imap,
        design_hash,
        folded,
        merged,
    })
}

/// Build the default SAT solver wired to `ctx`'s cancel token, deadline, and memory guard.
pub fn solver(ctx: &RunContext) -> Box<dyn Sat> {
    let mut sat = informal_sat::best();
    let (cancel, deadline) = (ctx.cancel.clone(), ctx.deadline);
    sat.set_terminate(Box::new(move || {
        cancel.is_cancelled() || Instant::now() >= deadline || informal_engine::memory::exceeded()
    }));
    sat
}

/// Convenience constructor for [`Verdict::Unknown`].
pub fn unknown(reason: impl Into<String>) -> Verdict {
    Verdict::Unknown {
        reason: reason.into(),
    }
}

/// A verdict after its independent check, with what an outside checker
/// needs to confirm it (only kept with a definitive verdict).
pub struct Checked {
    pub verdict: Verdict,
    pub certificate: Option<Certificate>,
}

impl Checked {
    pub fn new(verdict: Verdict, certificate: Option<Certificate>) -> Self {
        let certificate = certificate.filter(|_| verdict.is_definitive());
        Checked {
            verdict,
            certificate,
        }
    }
}

impl From<Verdict> for Checked {
    fn from(verdict: Verdict) -> Self {
        Checked::new(verdict, None)
    }
}

/// Assemble an [`EngineResult`] from a [`Checked`] verdict and start timestamp.
pub fn result(name: &str, v: impl Into<Checked>, t0: Instant) -> EngineResult {
    let v: Checked = v.into();
    EngineResult {
        engine: name.into(),
        verdict: v.verdict,
        wall_ms: t0.elapsed().as_millis(),
        witness: None,
        certificate: v.certificate,
    }
}
