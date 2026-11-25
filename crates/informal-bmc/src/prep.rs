//! Shared bit-level model preparation, caching, and solver construction.
//!
//! Every bit-level in-house engine (`Bmc`, `KInduction`, `Ic3Engine`, `Ic3Mt`,
//! `KindIc3`, and future BDD/IMC/CAR engines) shares [`Prepared`]: a simplified,
//! optionally clock-folded and latch-correspondence-merged AIG together with its
//! cached CNF template and back-maps to the loaded model for independent
//! certification.
//!
//! Preparation is two-stage:
//! 1. [`BaseLoaded`] parses the model from disk, hashes it, runs initial COI +
//!    constant-latch + strash simplification (`simplify_maps`), optional AIG
//!    passes (`tier >= 2`), and latch correspondence (`scorr`, `tier >= 1`)
//!    **once** per `(path, size, mtime, tier)`.
//! 2. Clock folding (`fold_clock`) runs on the already COI-reduced and
//!    `scorr`-merged [`BaseLoaded`] AIG rather than the full unsimplified chip,
//!    inheriting `scorr`'s 1-step (hence also 2-step) latch equalities without
//!    running `scorr` a second time. When no free-running toggle clock survives
//!    in the COI, `fold = true` aliases the `fold = false` [`Prepared`] slot
//!    directly so even DAG-CNF elimination runs once.

use crate::{cnf, scorr, store};
use informal_aig::preprocess::{self as pre, InputMap, LatchMap};
use informal_aig::{is_neg, var, Aig, Init};
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

/// The parts of a prepared model before the CNF template.
pub(crate) struct Loaded {
    pub(crate) aig: Aig,
    pub(crate) latch_map: LatchMap,
    pub(crate) input_map: InputMap,
    pub(crate) design_hash: u64,
    pub(crate) property_hash: u64,
    pub(crate) folded: Option<Folded>,
    pub(crate) merged: Vec<(informal_aig::Lit, informal_aig::Lit)>,
}

/// Stage-1 shared base: parsed from disk, hashed, COI-simplified, and
/// `scorr`-merged once per `(path, size, mtime, tier)` so folded and unfolded
/// engines share both disk I/O and latch correspondence.
struct BaseLoaded {
    aig: Aig,
    latch_map: LatchMap,
    input_map: InputMap,
    design_hash: u64,
    property_hash: u64,
    merged: Vec<(informal_aig::Lit, informal_aig::Lit)>,
    can_fold: bool,
}

fn has_free_clock(g: &Aig) -> bool {
    !g.latches.iter().any(|l| matches!(l.init, Init::Expr(_)))
        && g.latches
            .iter()
            .any(|l| l.next == (l.lit ^ 1) && matches!(l.init, Init::Zero | Init::One))
}

fn load_base(req: &CheckRequest, ctx: &RunContext, tier: u8) -> Result<BaseLoaded, String> {
    if ctx.should_stop() {
        return Err(ctx.stop_reason().to_string());
    }
    let g = informal_aig::load(&req.model).map_err(|e| format!("{e:#}"))?;
    if g.bads.is_empty() {
        return Err("model has no bad-state property".into());
    }
    // Always compute design & property hashes so cached `Prepared` slots carry
    // stable identities for both `INFORMAL_INV_STORE` and `LemmaHub`.
    let design_hash = store::design_hash(&g);
    let property_hash = store::property_hash(&g);
    let latch_lits: Vec<informal_aig::Lit> = g.latches.iter().map(|l| l.lit).collect();
    let (g, _, map, imap) = pre::simplify_maps(&g);

    let run_passes = (tier >= 2 || std::env::var_os("INFORMAL_AIG_PASSES").is_some())
        && !ctx.should_stop()
        && g.num_ands() <= 100_000;
    let (g, map, imap) = if run_passes {
        let cfg = informal_aig::passes::PassConfig {
            rewrite: true,
            fraig: true,
            temp_decomp: true,
            phase_abs: false,
            max_fraig_checks: 1_000,
        };
        let (opt, lm, im, _rep) = informal_aig::passes::run_pipeline(&g, &cfg);
        let next_map = pre::compose(&map, &g, &lm);
        let next_imap = pre::compose_inputs(&imap, &g, &im);
        (opt, next_map, next_imap)
    } else {
        (g, map, imap)
    };

    let mut merged = Vec::new();
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
                // Lift scorr equalities back to the loaded model's latch literals.
                let mut back: HashMap<u32, informal_aig::Lit> = HashMap::new();
                for (&x, m) in latch_lits.iter().zip(&map) {
                    if let Some(l) = *m {
                        if l != informal_aig::TRUE && l != informal_aig::FALSE {
                            back.entry(var(l)).or_insert(x ^ (l & 1));
                        }
                    }
                }
                let to_loaded = |l: informal_aig::Lit| {
                    if l == informal_aig::TRUE || l == informal_aig::FALSE {
                        Some(l)
                    } else {
                        back.get(&var(l)).map(|&x| x ^ (l & 1))
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
                (h, m, im)
            }
            None => (g, map, imap),
        }
    };
    let can_fold = has_free_clock(&g);
    Ok(BaseLoaded {
        aig: g,
        latch_map: map,
        input_map: imap,
        design_hash,
        property_hash,
        merged,
        can_fold,
    })
}

/// Derive a [`Loaded`] model (folded or unfolded) from a simplified + merged [`BaseLoaded`].
fn derive_loaded(base: &BaseLoaded, fold: bool) -> Loaded {
    if fold && base.can_fold {
        if let Some((f, info, fl, fi)) = pre::fold_clock(&base.aig) {
            let (s, _, map, imap) = pre::simplify_maps(&f);
            if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
                eprintln!(
                    "fold: clock latch {} (phase {}), {} -> {} ands; {} latches after simplification",
                    info.clock,
                    u8::from(info.phase),
                    base.aig.num_ands(),
                    f.num_ands(),
                    s.latches.len()
                );
            }
            // Map the clock latch in `base.aig` back to its loaded latch
            // index and polarity (for LemmaHub scoping and folded certificates).
            let clk_var = var(base.aig.latches[info.clock].lit);
            let (orig_clock, orig_phase) = base
                .latch_map
                .iter()
                .enumerate()
                .find_map(|(i, m)| {
                    let l = (*m)?;
                    if l != informal_aig::TRUE && l != informal_aig::FALSE && var(l) == clk_var {
                        Some((i, info.phase ^ is_neg(l)))
                    } else {
                        None
                    }
                })
                .unwrap_or((info.clock, info.phase));
            let ok = info.ok.and_then(|l| {
                let i = f.latches.iter().position(|x| x.lit == l)?;
                map[i]
            });
            let fl_map = pre::compose(&fl, &f, &map);
            let fi_map = pre::compose_inputs(&fi, &f, &imap);
            return Loaded {
                latch_map: pre::compose(&base.latch_map, &base.aig, &fl_map),
                input_map: pre::compose_inputs(&base.input_map, &base.aig, &fi_map),
                aig: s,
                design_hash: base.design_hash,
                property_hash: base.property_hash,
                folded: Some(Folded {
                    clock: orig_clock,
                    phase: orig_phase,
                    ok,
                }),
                merged: base.merged.clone(),
            };
        }
    }
    Loaded {
        aig: base.aig.clone(),
        latch_map: base.latch_map.clone(),
        input_map: base.input_map.clone(),
        design_hash: base.design_hash,
        property_hash: base.property_hash,
        folded: None,
        merged: base.merged.clone(),
    }
}

/// The simplified model and the maps from the loaded model to it (test helper).
#[cfg(test)]
pub(crate) fn prepare(req: &CheckRequest, fold: bool) -> Result<Loaded, String> {
    let ctx = RunContext::new(req.timeout);
    let base = load_base(req, &ctx, 0)?;
    Ok(derive_loaded(&base, fold))
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
    /// [`store::design_hash`] of the loaded model (transition + constraint cone).
    pub design_hash: u64,
    /// [`store::property_hash`] of the loaded model (design + bad outputs).
    pub property_hash: u64,
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

enum SlotState<T> {
    Empty,
    Building,
    Ready(Weak<T>),
    /// The model cannot be prepared (a load error): the same for everyone.
    Failed(String),
}

struct Slot<T> {
    state: Mutex<SlotState<T>>,
    ready: Condvar,
}

type BaseKey = (PathBuf, u64, Option<SystemTime>, u8);

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

type SlotTable<K, T> = Mutex<Vec<(K, Arc<Slot<T>>)>>;

static BASE_SLOTS: SlotTable<BaseKey, BaseLoaded> = Mutex::new(Vec::new());
static SLOTS: SlotTable<SlotKey, Prepared> = Mutex::new(Vec::new());

/// Single-flight acquire helper for `BASE_SLOTS` and `SLOTS`.
fn acquire_slot<K: Eq + Clone, T>(
    table: &SlotTable<K, T>,
    key: K,
    ctx: &RunContext,
    build: impl FnOnce() -> Result<Arc<T>, String>,
) -> Result<Arc<T>, String> {
    let stop = || {
        if ctx.should_stop() {
            Err(ctx.stop_reason().to_string())
        } else {
            Ok(())
        }
    };
    stop()?;
    let slot = {
        let mut slots = table.lock().unwrap_or_else(|p| p.into_inner());
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
                slots.push((key, s.clone()));
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
                let built = build();
                let mut s = slot.state.lock().unwrap_or_else(|p| p.into_inner());
                let out = match built {
                    Ok(p) => {
                        *s = SlotState::Ready(Arc::downgrade(&p));
                        Ok(p)
                    }
                    Err(e) => {
                        // Cancellation/timeout must not poison the slot for sibling threads.
                        if ctx.should_stop() {
                            *s = SlotState::Empty;
                        } else {
                            *s = SlotState::Failed(e.clone());
                        }
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
/// COI-simplified + `scorr`-merged model has a free-running toggle clock.
/// When no such clock exists, `fold = true` shares the exact same [`Prepared`]
/// slot as `fold = false`.
pub fn prepare_ctx_fold(
    req: &CheckRequest,
    ctx: &RunContext,
    fold: bool,
) -> Result<Arc<Prepared>, String> {
    let meta = std::fs::metadata(&req.model).ok();
    let len = meta.as_ref().map_or(0, |m| m.len());
    let mtime = meta.and_then(|m| m.modified().ok());
    let tier = scorr_tier(req.effort);
    let base_key: BaseKey = (req.model.clone(), len, mtime, tier);
    let base = acquire_slot(&BASE_SLOTS, base_key, ctx, || {
        load_base(req, ctx, tier).map(Arc::new)
    })?;
    let effective_fold = fold && base.can_fold;
    let key: SlotKey = (req.model.clone(), len, mtime, tier, effective_fold);
    acquire_slot(&SLOTS, key, ctx, || {
        let p = Arc::new(build_prepared(ctx, &base, effective_fold)?);
        cnf::share(&p.aig, &p._template);
        Ok(p)
    })
}

fn build_prepared(
    ctx: &RunContext,
    base: &BaseLoaded,
    fold: bool,
) -> Result<Prepared, String> {
    if ctx.should_stop() {
        return Err(ctx.stop_reason().to_string());
    }
    let Loaded {
        aig: g,
        latch_map: map,
        input_map: imap,
        design_hash,
        property_hash,
        folded,
        merged,
    } = derive_loaded(base, fold);
    if ctx.should_stop() {
        return Err(ctx.stop_reason().to_string());
    }
    let t = cnf::build_until(&g, &|| ctx.should_stop());
    if ctx.should_stop() {
        return Err(ctx.stop_reason().to_string());
    }
    Ok(Prepared {
        aig: g,
        _template: Arc::new(t),
        latch_map: map,
        input_map: imap,
        design_hash,
        property_hash,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{certify, ic3};
    use informal_aig::neg;

    fn write_tmp_aag(tag: &str, g: &Aig) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "informal_prep_{tag}_{}_{}.aag",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&p, informal_aig::aiger::write_ascii(g)).unwrap();
        p
    }

    #[test]
    fn unfoldable_model_shares_prepared_slot_across_fold_flag() {
        let mut g = Aig::new();
        let en = g.input();
        let x = g.latch(Init::Zero);
        let nx = g.xor(x, en);
        g.set_next(x, nx);
        let dead = g.latch(Init::Zero);
        g.set_next(dead, neg(dead)); // dead toggle outside COI
        g.bads.push(informal_aig::FALSE);
        let path = write_tmp_aag("unfoldable", &g);
        let req = CheckRequest::new(path.clone(), ModelFormat::Aiger, Duration::from_secs(5));
        let ctx = RunContext::new(Duration::from_secs(5));
        let p_unfolded = prepare_ctx_fold(&req, &ctx, false).unwrap();
        let p_folded = prepare_ctx_fold(&req, &ctx, true).unwrap();
        assert!(p_folded.folded.is_none());
        assert!(
            Arc::ptr_eq(&p_unfolded, &p_folded),
            "models without a COI clock must share one Prepared slot"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn coi_first_scorr_then_fold_clock_certifies_with_merged_and_dead_latches() {
        let mut g = Aig::new();
        let inp = g.input();
        // Dead latches before the real clock so loaded latch index != COI index.
        let dead1 = g.latch(Init::Zero);
        g.set_next(dead1, inp);
        let dead_const = g.latch(Init::Zero);
        let dc_next = g.and(dead_const, inp);
        g.set_next(dead_const, dc_next);
        // Free-running clock in COI.
        let clk = g.latch(Init::Zero);
        g.set_next(clk, neg(clk));
        // Duplicate pair `a` and `b` proved equal by `scorr` before folding.
        let a = g.latch(Init::Zero);
        let b = g.latch(Init::Zero);
        let next_ab = g.xor(a, inp);
        let next_b = g.xor(b, inp);
        g.set_next(a, next_ab);
        g.set_next(b, next_b);
        // Non-constant 2-bit ring (s0=0, s1=0; s0'=clk? !s0&!s1 : s0; s1'=clk? s0 : s1)
        // so `s0 & s1` is reachable-false (needs IC3, survives constant_latches & scorr)
        // while keeping `clk` genuinely inside the COI even after `a == b` merges.
        let s0 = g.latch(Init::Zero);
        let s1 = g.latch(Init::Zero);
        let neither = g.and(neg(s0), neg(s1));
        let s0_next = g.ite(clk, neither, s0);
        let s1_next = g.ite(clk, s0, s1);
        g.set_next(s0, s0_next);
        g.set_next(s1, s1_next);
        // Safe bad = (a ^ b) | (s0 & s1 & clk).
        let diff = g.xor(a, b);
        let both = g.and(s0, s1);
        let both_clk = g.and(both, clk);
        let bad = g.or(diff, both_clk);
        g.bads.push(bad);

        let path = write_tmp_aag("coi_scorr_fold", &g);
        let req = CheckRequest::new(path.clone(), ModelFormat::Aiger, Duration::from_secs(5));
        let ctx = RunContext::new(Duration::from_secs(5));
        let p = prepare_ctx_fold(&req, &ctx, true).unwrap();
        assert!(!p.merged.is_empty(), "scorr must merge duplicate latches");
        let f = p.folded.expect("clock in COI must fold");
        assert_eq!(f.clock, 2, "must map back to loaded latch index 2");
        assert!(!f.phase);

        let mut runner = ic3::Ic3::new(&p, Box::new(|| Box::new(informal_sat::Mini::default())))
            .expect("ic3 init");
        assert_eq!(runner.check(&ctx), Verdict::Safe);
        let lemmas = runner.invariant().expect("invariant");
        let orig = informal_aig::load(&path).unwrap();
        let mut inv = certify::lift_folded(&orig, &p.latch_map, &p, &lemmas, f.ok).unwrap();
        inv.extend(certify::equalities(&p.merged));
        certify::check_folded(&orig, &inv, &|| Box::new(informal_sat::Mini::default()))
            .expect("COI+scorr+folded invariant must pass independent 2-step certification");
        let _ = std::fs::remove_file(&path);
    }
}
