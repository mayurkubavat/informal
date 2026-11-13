//! In-house engines on SAT: bounded model checking and k-induction.
//!
//! Both load the model at bit level (AIGER directly, Btor2 bit-blasted by
//! `informal-aig`), simplify it, and unroll it frame by frame into CaDiCaL (see
//! `informal-sat`) with [`unroll::Unroll`]. They are the stage-1 engines of the
//! round-2 plan; IC3 follows in stage 2. Arrays and liveness are not
//! supported at bit level yet and report unknown.

pub mod certify;
pub mod cnf;
pub mod ic3;
pub mod invariant;
pub mod mine;
pub mod scorr;
pub mod unroll;

use informal_aig::preprocess::{InputMap, LatchMap};
use informal_aig::Aig;
use informal_engine::{
    Capabilities, CheckRequest, Engine, EngineResult, ModelFormat, Progress, RunContext, Verdict,
};
use informal_sat::{Answer, Sat};
use std::sync::Arc;
use std::time::Instant;
use unroll::Unroll;

pub use informal_aig::load as load_aig;

/// The simplified model and the map from the loaded model's latches to it,
/// or the reason this engine cannot take it.
fn prepare(req: &CheckRequest) -> Result<(Aig, LatchMap, InputMap), String> {
    let g = load_aig(&req.model).map_err(|e| format!("{e:#}"))?;
    if g.bads.is_empty() {
        return Err("model has no bad-state property".into());
    }
    let (s, _, map, imap) = informal_aig::preprocess::simplify_maps(&g);
    Ok((s, map, imap))
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
    Ready(std::sync::Weak<Prepared>),
    /// The model cannot be prepared (a load error): the same for everyone.
    Failed(String),
}

struct Slot {
    state: std::sync::Mutex<SlotState>,
    ready: std::sync::Condvar,
}

/// Model file, its length and mtime, and whether scorr is on.
type SlotKey = (std::path::PathBuf, u64, Option<std::time::SystemTime>, bool);

static SLOTS: std::sync::Mutex<Vec<(SlotKey, Arc<Slot>)>> = std::sync::Mutex::new(Vec::new());

/// [`prepare`], then latch correspondence ([`scorr`]) within 5% of the
/// time left (at most 10 s), then the CNF template; see [`Prepared`].
/// On the big CPU models of HWMCC'24 1-induction peels off about one
/// scorr candidate per SAT call and never finishes, so the budget only
/// bounds the waste. `INFORMAL_NO_SCORR` turns scorr off.
///
/// The first engine to ask builds, with its own stop and deadline (a
/// stop ends scorr and variable elimination early; both stay sound); the
/// others wait, polling their own stop. Before this, every engine, IC3
/// worker and k-induction unrolling built its own template (37 s and
/// 1.5 GB each on sosylab Problem07, about 15 copies per portfolio).
fn prepare_ctx(req: &CheckRequest, ctx: &RunContext) -> Result<Arc<Prepared>, String> {
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
        std::env::var_os("INFORMAL_NO_SCORR").is_none(),
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
                    state: std::sync::Mutex::new(SlotState::Empty),
                    ready: std::sync::Condvar::new(),
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
                    .wait_timeout(st, std::time::Duration::from_millis(50))
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
                if ctx.should_stop() {
                    return Err(ctx.stop_reason().to_string());
                }
            }
            SlotState::Empty => {
                *st = SlotState::Building;
                drop(st);
                let built = build_prepared(req, ctx);
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

/// Experiment H1: mine two-literal invariants by simulation, prove them
/// with Houdini (up to 10% of the time left, at most 30 s) and give them
/// to `ic3` up front.
fn mine_into(g: &Aig, ic3: &mut ic3::Ic3, ctx: &RunContext, name: &str) {
    let t = Instant::now();
    let k = std::env::var("INFORMAL_MINE_K")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(1500);
    let cap = std::env::var("INFORMAL_MINE_CAP")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(50_000);
    let cands = mine::candidates(g, k, cap, std::time::Duration::from_secs(10));
    let nc = cands.len();
    let mut h = ctx.clone();
    let rest = ctx.deadline.saturating_duration_since(Instant::now());
    let cap_s: f64 = std::env::var("INFORMAL_MINE_TL")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(30.0);
    h.deadline = Instant::now() + (rest / 10).min(std::time::Duration::from_secs_f64(cap_s));
    if std::env::var_os("INFORMAL_MINE_TL").is_some() {
        h.deadline = Instant::now() + std::time::Duration::from_secs_f64(cap_s);
    }
    let inv = invariant::houdini(g, cands, || solver(&h), &h).unwrap_or_default();
    if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
        eprintln!(
            "{name}: mined {} candidates, {} proved invariant, {:.2} s",
            nc,
            inv.len(),
            t.elapsed().as_secs_f64()
        );
    }
    ic3.assume_invariant(inv);
}

/// INFORMAL_IC3_SEED: seed for the first phases in IC3's own solver
/// (0 or unset: the fixed default path). IC3's path, not its verdict,
/// depends on which models the solver returns, so one run says little
/// about a change; scripts/ic3_seeds.py scores a build over several seeds.
fn ic3_seed() -> u64 {
    static SEED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *SEED.get_or_init(|| {
        std::env::var("INFORMAL_IC3_SEED")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(0)
    })
}

/// `v`, IC3's verdict, with a SAFE kept only when its invariant passes the
/// independent check on the model as loaded ([`certify`]); otherwise it
/// becomes unknown, loudly. `INFORMAL_NO_CERT` skips the check (for
/// measuring its cost only).
fn certified(
    req: &CheckRequest,
    p: &Prepared,
    ic3: &ic3::Ic3,
    v: Verdict,
    ctx: &RunContext,
) -> Verdict {
    if v != Verdict::Safe || std::env::var_os("INFORMAL_NO_CERT").is_some() {
        return v;
    }
    let t = Instant::now();
    let r = (|| {
        let lemmas = ic3
            .invariant()
            .ok_or("missing: IC3 said safe without an invariant")?;
        let orig = load_aig(&req.model).map_err(|e| format!("cannot reload the model: {e:#}"))?;
        let inv = certify::lift(&orig, &p.latch_map, p, &lemmas)?;
        certify::check(&orig, &inv, &|| solver(ctx))
    })();
    report_certificate(req, r, t, ctx)
}

/// `v` with an UNSAFE kept only when [`certify::check_unsafe`] finds a
/// counterexample of that depth on the model as loaded and simulates it
/// to a bad state; otherwise unknown, loudly (a stop on the way also
/// gives unknown). `INFORMAL_NO_CERT` skips the check.
fn unsafe_certified(
    req: &CheckRequest,
    v: Verdict,
    ctx: &RunContext,
    trace: Option<(&Prepared, PreparedTrace)>,
) -> Verdict {
    let Verdict::Unsafe { depth } = v else {
        return v;
    };
    if std::env::var_os("INFORMAL_NO_CERT").is_some() {
        return v;
    }
    let t = Instant::now();
    let r = (|| {
        let d = depth.ok_or("missing: UNSAFE without a depth")? as usize;
        let orig = load_aig(&req.model).map_err(|e| format!("cannot reload the model: {e:#}"))?;
        // The engine's own trace, carried back to the loaded model, is
        // checked by simulation alone; without one (IC3), or if it does
        // not replay, a fresh search at the same depth decides.
        if let Some((p, pt)) = &trace {
            let t = pt.lift(p, &orig);
            match certify::replay_unsafe(&orig, d, &t) {
                Ok(()) => return Ok(()),
                Err(e) => eprintln!(
                    "informal: engine trace did not replay on the loaded model ({e}); searching: {}",
                    req.model.display()
                ),
            }
        }
        certify::check_unsafe(&orig, d, &|| solver(ctx), &|| ctx.should_stop())
    })();
    match report_certificate(req, r, t, ctx) {
        Verdict::Safe => Verdict::Unsafe { depth },
        other => other,
    }
}

/// A counterexample on the prepared model: each latch's initial value and
/// each input's value per frame, by index.
struct PreparedTrace {
    init: Vec<bool>,
    inputs: Vec<Vec<bool>>,
}

impl PreparedTrace {
    fn from_unroll(g: &Aig, u: &Unroll, d: usize) -> Self {
        PreparedTrace {
            init: g
                .latches
                .iter()
                .map(|l| u.sat.value(u.lit(0, l.lit)))
                .collect(),
            inputs: (0..=d)
                .map(|f| g.inputs.iter().map(|&i| u.sat.value(u.lit(f, i))).collect())
                .collect(),
        }
    }

    /// The same trace on the loaded model, through `p`'s maps; inputs and
    /// latches preprocessing dropped get 0.
    fn lift(&self, p: &Prepared, orig: &Aig) -> certify::Trace {
        use informal_aig::{is_neg, var, FALSE, TRUE};
        let lat: std::collections::HashMap<u32, usize> = p
            .latches
            .iter()
            .enumerate()
            .map(|(i, l)| (var(l.lit), i))
            .collect();
        let inp: std::collections::HashMap<u32, usize> = p
            .inputs
            .iter()
            .enumerate()
            .map(|(i, &l)| (var(l), i))
            .collect();
        let init = p
            .latch_map
            .iter()
            .map(|m| match *m {
                None | Some(FALSE) => false,
                Some(TRUE) => true,
                Some(l) => lat.get(&var(l)).is_some_and(|&i| self.init[i] ^ is_neg(l)),
            })
            .collect();
        let inputs = self
            .inputs
            .iter()
            .map(|frame| {
                p.input_map
                    .iter()
                    .map(|m| match *m {
                        Some(l) => inp.get(&var(l)).is_some_and(|&i| frame[i] ^ is_neg(l)),
                        None => false,
                    })
                    .collect()
            })
            .collect();
        let _ = orig;
        certify::Trace { init, inputs }
    }
}

/// [`certified`] for k-induction: `v` with the depth it was proved at,
/// `strengthening` the invariant cubes it blocked in every frame.
fn kind_certified(
    req: &CheckRequest,
    p: &Prepared,
    v: (Verdict, Option<usize>),
    strengthening: &[ic3::Cube],
    ctx: &RunContext,
) -> Verdict {
    let (v, k) = v;
    if v != Verdict::Safe || std::env::var_os("INFORMAL_NO_CERT").is_some() {
        return v;
    }
    let t = Instant::now();
    let r = (|| {
        let k = k.ok_or("missing: k-induction said safe without a depth")?;
        let orig = load_aig(&req.model).map_err(|e| format!("cannot reload the model: {e:#}"))?;
        let (inv, reps) = certify::lift_reps(&orig, &p.latch_map, p, strengthening)?;
        // Depth k here is the number of good frames before the bad one.
        certify::check_kind(&orig, &inv, &reps, k.max(1), &|| solver(ctx), &|| {
            ctx.should_stop()
        })
    })();
    report_certificate(req, r, t, ctx)
}

/// Turn a certificate check's outcome into the verdict to report.
fn report_certificate(
    req: &CheckRequest,
    r: Result<(), String>,
    t: Instant,
    ctx: &RunContext,
) -> Verdict {
    if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
        eprintln!(
            "certificate: {:?} in {:.3} s {}",
            r,
            t.elapsed().as_secs_f64(),
            req.model.display()
        );
    }
    match r {
        Ok(()) => Verdict::Safe,
        Err(e) if e.ends_with("check stopped") => unknown(ctx.stop_reason()),
        Err(e) => {
            eprintln!(
                "informal: verdict withdrawn, certificate {e}: {}",
                req.model.display()
            );
            unknown(format!("certificate {e}"))
        }
    }
}

fn build_prepared(req: &CheckRequest, ctx: &RunContext) -> Result<Prepared, String> {
    let (g, map, imap) = prepare(req)?;
    let (g, map, imap) = if std::env::var_os("INFORMAL_NO_SCORR").is_some() || ctx.should_stop() {
        (g, map, imap)
    } else {
        let now = Instant::now();
        let left = ctx.deadline.saturating_duration_since(now);
        let mut c = ctx.clone();
        // INFORMAL_SCORR_TL=<seconds> overrides the budget (experiments).
        let budget = std::env::var("INFORMAL_SCORR_TL")
            .ok()
            .and_then(|x| x.parse().ok())
            .map(std::time::Duration::from_secs_f64)
            .unwrap_or_else(|| (left / 20).min(std::time::Duration::from_secs(10)));
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
                let m = informal_aig::preprocess::compose(&map, &g, &hmap);
                let im = informal_aig::preprocess::compose_inputs(&imap, &g, &himap);
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
    })
}

fn solver(ctx: &RunContext) -> Box<dyn Sat> {
    let mut sat = informal_sat::best();
    let (cancel, deadline) = (ctx.cancel.clone(), ctx.deadline);
    sat.set_terminate(Box::new(move || {
        cancel.is_cancelled() || Instant::now() >= deadline || informal_engine::memory::exceeded()
    }));
    sat
}

fn unknown(reason: impl Into<String>) -> Verdict {
    Verdict::Unknown {
        reason: reason.into(),
    }
}

fn result(name: &str, verdict: Verdict, t0: Instant) -> EngineResult {
    EngineResult {
        engine: name.into(),
        verdict,
        wall_ms: t0.elapsed().as_millis(),
        witness: None,
    }
}

const FORMATS: [ModelFormat; 2] = [ModelFormat::Aiger, ModelFormat::Btor2];

/// Bounded model checking: is a bad state reachable in exactly k steps,
/// for k = 0, 1, 2, ...? Never proves. With `step` > 1 it unrolls `step`
/// frames per SAT call and asks for a bad state in any of them (rIC3's
/// `bmc --step N`): far fewer calls on deep bugs, and the depth reported
/// is the first bad frame of the model found, not always the shortest.
pub struct Bmc {
    pub max_depth: Option<usize>,
    pub step: usize,
}

impl Engine for Bmc {
    fn name(&self) -> &str {
        match self.step {
            0 | 1 => "informal-bmc",
            10 => "informal-bmc-10",
            65 => "informal-bmc-65",
            _ => "informal-bmc-n",
        }
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: FORMATS.to_vec(),
            finds_bugs: true,
            ..Default::default()
        }
    }
    /// BMC grows with depth: shed first, deepest steps before shallow.
    fn shed_rank(&self) -> u32 {
        match self.step {
            0 | 1 => 3,
            2..=10 => 4,
            _ => 5,
        }
    }
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx(req, ctx) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let step = self.step.max(1);
        let mut witness = None;
        let mut cert_trace = None;
        let mut u = Unroll::new(&g, solver(ctx), false);
        let mut k = 0; // first frame not yet checked
        let v = loop {
            if ctx.should_stop() {
                break unknown(ctx.stop_reason());
            }
            if self.max_depth.is_some_and(|m| k > m) {
                break unknown(format!("bound {} reached", k - 1));
            }
            let mut last = k + step - 1;
            if let Some(m) = self.max_depth {
                last = last.min(m);
            }
            let mut bads = Vec::new();
            for f in k..=last {
                // A deep step adds many frames before its one SAT call.
                if ctx.should_stop() {
                    break;
                }
                u.add_frame();
                bads.push(u.bad(f));
            }
            if bads.len() < last + 1 - k {
                break unknown(ctx.stop_reason());
            }
            // One call: is any of frames k..=last bad?
            let assume = if let [b] = bads[..] {
                b
            } else {
                let a = u.new_var();
                let mut c = vec![-a];
                c.extend(&bads);
                u.sat.add_clause(&c);
                a
            };
            match u.sat.solve(&[assume]) {
                Answer::Sat => {
                    let d = (k..=last)
                        .zip(&bads)
                        .find(|(_, &b)| u.sat.value(b))
                        .map_or(last, |(f, _)| f);
                    // Never report a trace we cannot replay.
                    if let Err(e) = replay(&g, &u, d) {
                        break unknown(format!(
                            "internal error: counterexample does not replay: {e}"
                        ));
                    }
                    witness = Some(named_witness(&g, &u, d));
                    cert_trace = Some(PreparedTrace::from_unroll(&g, &u, d));
                    break Verdict::Unsafe {
                        depth: Some(d as u32),
                    };
                }
                Answer::Unsat => {
                    for &b in &bads {
                        u.sat.add_clause(&[-b]);
                    }
                    if assume != bads[0] || bads.len() > 1 {
                        u.sat.add_clause(&[-assume]);
                    }
                }
                Answer::Unknown => break unknown(ctx.stop_reason()),
            }
            ctx.report(Progress {
                engine: self.name().into(),
                bound: Some(last as u32),
                note: String::new(),
                output: None,
            });
            k = last + 1;
        };
        let mut r = result(
            self.name(),
            unsafe_certified(req, v, ctx, cert_trace.map(|t| (&*g, t))),
            t0,
        );
        r.witness = witness;
        Ok(r)
    }
}

/// The solver's counterexample, frames 0..=d, by signal name: free initial
/// latch values (`init`) and input values per frame (`@f`), for every input
/// and latch the model names (Btor2 symbols survive bit-blasting,
/// simplification and scorr as `name` or `name[i]`). Unnamed ones are left
/// out. Format `informalw 1`, read by informal-fpv to build a VCD on the original
/// model (docs/ui/trace-format.md). Export only: nothing here is searched.
fn named_witness(g: &Aig, u: &Unroll, d: usize) -> String {
    let bit = |b: bool| if b { '1' } else { '0' };
    let mut s = format!("informalw 1\ndepth {d}\ninit");
    for l in &g.latches {
        if matches!(l.init, informal_aig::Init::Zero | informal_aig::Init::One) {
            continue;
        }
        if let Some(name) = g.names.get(&informal_aig::var(l.lit)) {
            s += &format!(" {name}={}", bit(u.sat.value(u.lit(0, l.lit))));
        }
    }
    for f in 0..=d {
        s += &format!("\n@{f}");
        for &i in &g.inputs {
            if let Some(name) = g.names.get(&informal_aig::var(i)) {
                s += &format!(" {name}={}", bit(u.sat.value(u.lit(f, i))));
            }
        }
    }
    s.push('\n');
    s
}

/// Simulate `g` with the inputs (and free initial latch values) of the
/// solver's model, frames 0..=d: the constraints must hold in every frame
/// and some bad output in frame d. Checks the SAT encoding end to end.
fn replay(g: &Aig, u: &Unroll, d: usize) -> Result<(), String> {
    let n = g.nodes.len();
    let lv = |val: &[bool], l: informal_aig::Lit| {
        val[informal_aig::var(l) as usize] ^ informal_aig::is_neg(l)
    };
    let mut state: Vec<bool> = g
        .latches
        .iter()
        .map(|l| match l.init {
            informal_aig::Init::Zero => false,
            informal_aig::Init::One => true,
            _ => u.sat.value(u.lit(0, l.lit)),
        })
        .collect();
    let mut val = vec![false; n];
    for f in 0..=d {
        let mut li = 0;
        for v in 0..n {
            val[v] = match g.nodes[v] {
                informal_aig::Node::Const => false,
                informal_aig::Node::Input => u.sat.value(u.lit(f, 2 * v as u32)),
                informal_aig::Node::Latch => {
                    li += 1;
                    state[li - 1]
                }
                informal_aig::Node::And(a, b) => lv(&val, a) && lv(&val, b),
            };
        }
        if f == 0 {
            for l in &g.latches {
                if let informal_aig::Init::Expr(e) = l.init {
                    if lv(&val, l.lit) != lv(&val, e) {
                        return Err("initial value expression".into());
                    }
                }
            }
        }
        if let Some(c) = g.constraints.iter().position(|&c| !lv(&val, c)) {
            return Err(format!("constraint {c} fails at frame {f}"));
        }
        if f == d && !g.bads.iter().any(|&b| lv(&val, b)) {
            return Err(format!("no bad output at frame {d}"));
        }
        for (i, l) in g.latches.iter().enumerate() {
            state[i] = lv(&val, l.next);
        }
    }
    Ok(())
}

/// k-induction with lazy simple-path constraints: alongside BMC, checks
/// whether k consecutive good states always lead to a good state. When the
/// step finds a path that repeats a state, only that pair is constrained
/// to differ and the step is retried (lazy, as the round-2 research
/// recommends). Sound, and complete for finite-state systems.
pub struct KInduction {
    pub max_depth: Option<usize>,
}

impl Engine for KInduction {
    fn name(&self) -> &str {
        "informal-kind"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: FORMATS.to_vec(),
            proves: true,
            finds_bugs: true,
            ..Default::default()
        }
    }
    fn shed_rank(&self) -> u32 {
        2
    }
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx(req, ctx) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let (v, k, tr) = kind(&g, ctx, self.name(), self.max_depth, &[]);
        let v = kind_certified(req, &g, (v, k), &[], ctx);
        Ok(result(
            self.name(),
            unsafe_certified(req, v, ctx, tr.map(|t| (&*g, t))),
            t0,
        ))
    }
}

/// The k-induction loop of [`KInduction`]; `invariants` (cubes whose
/// negation holds in every reachable state) strengthen every step frame.
fn kind(
    g: &Aig,
    ctx: &RunContext,
    name: &str,
    max_depth: Option<usize>,
    invariants: &[ic3::Cube],
) -> (Verdict, Option<usize>, Option<PreparedTrace>) {
    let mut base = Unroll::new(g, solver(ctx), false);
    let mut step = Unroll::new(g, solver(ctx), true);
    let mut k = 0;
    let mut proved_at = None;
    let mut trace = None;
    let v = 'outer: loop {
        if ctx.should_stop() {
            break unknown(ctx.stop_reason());
        }
        if max_depth.is_some_and(|m| k > m) {
            break unknown(format!("bound {} reached", k - 1));
        }
        // Base: no counterexample of length k.
        base.add_frame();
        let bad = base.bad(k);
        match base.sat.solve(&[bad]) {
            Answer::Sat => {
                trace = Some(PreparedTrace::from_unroll(g, &base, k));
                break Verdict::Unsafe {
                    depth: Some(k as u32),
                };
            }
            Answer::Unsat => base.sat.add_clause(&[-bad]),
            Answer::Unknown => break unknown(ctx.stop_reason()),
        }
        // Step: good frames 0..k-1, then bad at k?
        step.add_frame();
        for c in invariants {
            step.block_cube(k, c);
        }
        let bad = step.bad(k);
        loop {
            match step.sat.solve(&[bad]) {
                Answer::Unsat => {
                    proved_at = Some(k);
                    break 'outer Verdict::Safe;
                }
                Answer::Unknown => break 'outer unknown(ctx.stop_reason()),
                Answer::Sat => match step.repeated_states(k) {
                    Some((i, j)) => step.differ(i, j),
                    None => break,
                },
            }
        }
        step.sat.add_clause(&[-bad]);
        ctx.report(Progress {
            engine: name.into(),
            bound: Some(k as u32),
            note: String::new(),
            output: None,
        });
        k += 1;
    };
    (v, proved_at, trace)
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
            ..Default::default()
        }
    }
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx(req, ctx) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let c = ctx.clone();
        let mini = self.mini;
        let make = move || -> Box<dyn Sat> {
            if !mini {
                return solver(&c);
            }
            let mut s = informal_sat::Mini::with_seed(ic3_seed());
            let (cancel, deadline) = (c.cancel.clone(), c.deadline);
            s.set_terminate(Box::new(move || {
                cancel.is_cancelled()
                    || Instant::now() >= deadline
                    || informal_engine::memory::exceeded()
            }));
            Box::new(s)
        };
        if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
            let t = cnf::build(&g);
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
        let Some(mut ic3) = ic3::Ic3::new(&g, Box::new(make)) else {
            return Ok(result(
                self.name(),
                unknown("latch inits that are not constants"),
                t0,
            ));
        };
        if std::env::var_os("INFORMAL_IC3_MINE").is_some() {
            mine_into(&g, &mut ic3, ctx, self.name());
        }
        if std::env::var_os("INFORMAL_IC3_HOUDINI").is_none() {
            // H2 on by default here: gap set 1 -> 3 at 300 s, sample 20 -> 21.
            ic3.set_config(ic3::Config {
                houdini_every: 4,
                ..ic3::Config::default()
            });
        }
        let v = ic3.check(ctx);
        let v = certified(req, &g, &ic3, v, ctx);
        if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
            eprintln!(
                "{}: frames {} lemmas {} ctps {} queries {} ({:.2} s) lift {:.2} s ({} fallbacks)",
                self.name(),
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
                "promotions: {} of {} tried",
                ic3.stats_promoted, ic3.stats_promote_tried
            );

            eprintln!(
                "ctg lemmas: {} in drops that succeeded, {} in drops that failed",
                ic3.stats_ctg_ok, ic3.stats_ctg_fail
            );
            let mic = ic3.stats_mic.max(1) as f64;
            eprintln!(
                "mic: {} calls ({} parent hits), cube {:.1} -> lemma {:.1} literals, drops ok {} fail {} ({:.1}%)",
                ic3.stats_mic,
                ic3.stats_parent_hits,
                ic3.stats_mic_in_len as f64 / mic,
                ic3.stats_mic_len as f64 / mic,
                ic3.stats_drop_ok,
                ic3.stats_drop_fail,
                100.0 * ic3.stats_drop_ok as f64
                    / (ic3.stats_drop_ok + ic3.stats_drop_fail).max(1) as f64
            );
            use std::sync::atomic::Ordering::Relaxed;
            let n = informal_sat::mini::SOLVES.load(Relaxed).max(1);
            eprintln!(
                "mini: solves {} decisions/solve {:.1} domain/solve {:.0} trail-at-end/solve {:.0} conflicts {}",
                n,
                informal_sat::mini::DECISIONS.load(Relaxed) as f64 / n as f64,
                informal_sat::mini::DOMAIN_VARS.load(Relaxed) as f64 / n as f64,
                informal_sat::mini::PROPS.load(Relaxed) as f64 / n as f64,
                0
            );
            let dn = ic3::DIAG_N.load(Relaxed).max(1) as f64;
            eprintln!(
                "domain: latches+inputs {:.0}, live (not eliminated) {:.0} per query",
                ic3::DIAG_LI.load(Relaxed) as f64 / dn,
                ic3::DIAG_LIVE.load(Relaxed) as f64 / dn
            );
            let ns = informal_sat::mini::SAT_SOLVES.load(Relaxed).max(1);
            eprintln!(
                "mini: SAT answers {} ({:.1} s, {:.0} decisions each); all solves {:.1} s; max vars {}",
                ns,
                informal_sat::mini::SAT_NANOS.load(Relaxed) as f64 / 1e9,
                informal_sat::mini::SAT_DECISIONS.load(Relaxed) as f64 / ns as f64,
                informal_sat::mini::ALL_NANOS.load(Relaxed) as f64 / 1e9,
                informal_sat::mini::MAX_VARS.load(Relaxed)
            );
        }
        Ok(result(self.name(), unsafe_certified(req, v, ctx, None), t0))
    }
}

/// IC3 for a share of the time, then k-induction strengthened by IC3's
/// lemmas once Houdini has cut them down to a proved invariant. IC3's
/// frames often hold most of an invariant that IC3 itself has not closed
/// yet; k-induction can finish the proof from there.
pub struct KindIc3 {
    /// Share of the time budget given to IC3 first.
    pub ic3_share: f64,
}

impl Engine for KindIc3 {
    fn name(&self) -> &str {
        "informal-kind-ic3"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: FORMATS.to_vec(),
            proves: true,
            finds_bugs: true,
            ..Default::default()
        }
    }
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx(req, ctx) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let left = ctx.deadline.saturating_duration_since(t0);
        let mut phase = ctx.clone();
        phase.deadline = t0 + left.mul_f64(self.ic3_share);
        let mut inv = Vec::new();
        let c = phase.clone();
        if let Some(mut ic3) = ic3::Ic3::new(&g, Box::new(move || solver(&c))) {
            // A SAFE whose certificate fails is withdrawn; k-induction
            // still gets its turn.
            let v = ic3.check(&phase);
            match certified(req, &g, &ic3, v, ctx) {
                Verdict::Unknown { .. } => {}
                v => return Ok(result(self.name(), unsafe_certified(req, v, ctx, None), t0)),
            }
            // Houdini gets up to half of what is left.
            let mut h = ctx.clone();
            let rest = ctx.deadline.saturating_duration_since(Instant::now());
            h.deadline = Instant::now() + rest / 2;
            inv = invariant::houdini(&g, ic3.lemmas(), || solver(&h), &h).unwrap_or_default();
        }
        if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
            eprintln!("{}: invariant of {} clauses", self.name(), inv.len());
        }
        let (v, k, tr) = kind(&g, ctx, self.name(), None, &inv);
        let v = kind_certified(req, &g, (v, k), &inv, ctx);
        Ok(result(
            self.name(),
            unsafe_certified(req, v, ctx, tr.map(|t| (&*g, t))),
            t0,
        ))
    }
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
        Ic3Mt {
            workers: vec![
                // H2 (docs/ic3-research.md) in one worker: Houdini every
                // 4 frames costs too much in all four at short timeouts.
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
                (
                    true,
                    Config {
                        max_ctgs: 0,
                        max_ctg_depth: 0,
                        reverse_drop: true,
                        parent_lemma: true,
                        ..d
                    },
                ),
                (
                    true,
                    Config {
                        max_ctgs: 5,
                        max_ctg_depth: 2,
                        reverse_drop: true,
                        max_ctps: 3,
                        parent_lemma: true,
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
            ..Default::default()
        }
    }
    /// Four IC3 workers, each with its own solvers.
    fn shed_rank(&self) -> u32 {
        2
    }
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx(req, ctx) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let pool = Arc::new(ic3::LemmaPool::default());
        // Workers stop on our token: cancelled by the winner or by `ctx`.
        let mut inner = ctx.clone();
        inner.cancel = informal_engine::CancelToken::new();
        let done = std::sync::atomic::AtomicBool::new(false);
        let stats = std::env::var_os("INFORMAL_IC3_STATS").is_some();
        let verdicts: Vec<Verdict> = std::thread::scope(|sc| {
            let (g, inner, done) = (&g, &inner, &done);
            sc.spawn(move || {
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    if ctx.cancel.is_cancelled() {
                        inner.cancel.cancel();
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            });
            let hs: Vec<_> = self
                .workers
                .iter()
                .enumerate()
                .map(|(id, &(mini, cfg))| {
                    let pool = pool.clone();
                    sc.spawn(move || {
                        let c = inner.clone();
                        let make = move || -> Box<dyn Sat> {
                            if !mini {
                                return solver(&c);
                            }
                            let mut s = informal_sat::Mini::with_seed(ic3_seed());
                            let (cancel, deadline) = (c.cancel.clone(), c.deadline);
                            s.set_terminate(Box::new(move || {
                                cancel.is_cancelled()
                                    || Instant::now() >= deadline
                                    || informal_engine::memory::exceeded()
                            }));
                            Box::new(s)
                        };
                        let Some(mut ic3) = ic3::Ic3::new(g, Box::new(make)) else {
                            return unknown("latch inits that are not constants");
                        };
                        ic3.set_config(cfg);
                        ic3.share(pool, id);
                        let v = ic3.check(inner);
                        let v = certified(req, g, &ic3, v, inner);
                        if !matches!(v, Verdict::Unknown { .. }) {
                            inner.cancel.cancel();
                        }
                        if stats {
                            eprintln!(
                                "informal-ic3-mt[{id}]: {:?} frames {} lemmas {} imported {}",
                                v,
                                ic3.depth(),
                                ic3.stats_lemmas,
                                ic3.stats_imported
                            );
                        }
                        v
                    })
                })
                .collect();
            let vs = hs.into_iter().map(|h| h.join().unwrap()).collect();
            done.store(true, std::sync::atomic::Ordering::Relaxed);
            vs
        });
        let v = verdicts
            .iter()
            .find(|v| !matches!(v, Verdict::Unknown { .. }))
            .cloned()
            .unwrap_or_else(|| unknown(ctx.stop_reason()));
        Ok(result(self.name(), unsafe_certified(req, v, ctx, None), t0))
    }
}

/// The in-house engines, for adding to a portfolio. Empty when this build
/// has no CaDiCaL: the fallback solver is too slow to be worth a thread.
pub fn engines() -> Vec<Arc<dyn Engine>> {
    // TODO(D7/D11, Gemini P7): pick 3-4 engines per model from structural
    // features (learned selector trained on the run database) instead of
    // the same fixed lineup for every model.
    if informal_sat::Cadical::signature().is_none() {
        return Vec::new();
    }
    vec![
        Arc::new(Bmc {
            max_depth: None,
            step: 1,
        }),
        Arc::new(Bmc {
            max_depth: None,
            step: 10,
        }),
        Arc::new(Bmc {
            max_depth: None,
            step: 65,
        }),
        Arc::new(KInduction { max_depth: None }),
        Arc::new(Ic3Engine { mini: true }),
        Arc::new(Ic3Engine { mini: false }),
        Arc::new(KindIc3 { ic3_share: 0.5 }),
        Arc::new(Ic3Mt::default()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn check(e: &dyn Engine, text: &str) -> Verdict {
        let dir =
            std::env::temp_dir().join(format!("informal-bmc-{}-{}", std::process::id(), e.name()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("m{}.btor2", text.len()));
        std::fs::write(&path, text).unwrap();
        let req = CheckRequest {
            model: path,
            format: ModelFormat::Btor2,
            timeout: Duration::from_secs(60),
        };
        e.check(&req, &RunContext::new(req.timeout))
            .unwrap()
            .verdict
    }

    /// 3-bit counter from 0, bad when it reaches `target`; `wrap` makes it
    /// count to 5 then return to 0.
    fn counter(target: u32, wrap: bool) -> String {
        let mut s = String::from("1 sort bitvec 3\n2 sort bitvec 1\n3 zero 1\n4 state 1 cnt\n5 init 1 4 3\n6 one 1\n7 add 1 4 6\n");
        if wrap {
            s += "8 constd 1 5\n9 eq 2 4 8\n10 ite 1 9 3 7\n11 next 1 4 10\n";
        } else {
            s += "8 constd 1 0\n9 eq 2 4 8\n11 next 1 4 7\n";
        }
        s += &format!("12 constd 1 {target}\n13 eq 2 4 12\n14 bad 13\n");
        s
    }

    /// H1 (feedback report): asserting "no bad output" on a model with
    /// several bad outputs must rule out each of them.
    #[test]
    fn no_bad_rules_out_every_bad_output() {
        let mut g = Aig::new();
        let (a, b) = (g.input(), g.input());
        g.bads.push(a);
        g.bads.push(b);
        let mut u = Unroll::new(&g, informal_sat::best(), false);
        u.add_frame();
        let x = u.bad(0);
        u.sat.add_clause(&[-x]);
        for l in [a, b] {
            let y = u.lit(0, l);
            assert_eq!(u.sat.solve(&[y]), Answer::Unsat);
        }
    }

    #[test]
    fn bmc_finds_shortest_counterexample() {
        let bmc = Bmc {
            max_depth: Some(10),
            step: 1,
        };
        assert_eq!(
            check(&bmc, &counter(4, false)),
            Verdict::Unsafe { depth: Some(4) }
        );
        assert!(matches!(
            check(&bmc, &counter(7, true)),
            Verdict::Unknown { .. }
        ));
    }

    #[test]
    fn stepped_bmc_finds_bugs_and_respects_bounds() {
        let b = Bmc {
            max_depth: Some(30),
            step: 10,
        };
        assert_eq!(
            check(&b, &counter(4, false)),
            Verdict::Unsafe { depth: Some(4) }
        );
        assert_eq!(
            check(&b, &counter(0, false)),
            Verdict::Unsafe { depth: Some(0) }
        );
        assert!(matches!(
            check(&b, &counter(7, true)),
            Verdict::Unknown { .. }
        ));
    }

    #[test]
    fn kinduction_proves_and_refutes() {
        let kind = KInduction {
            max_depth: Some(20),
        };
        assert_eq!(
            check(&kind, &counter(6, false)),
            Verdict::Unsafe { depth: Some(6) }
        );
        assert_eq!(check(&kind, &counter(7, true)), Verdict::Safe);
        assert_eq!(check(&kind, &counter(6, true)), Verdict::Safe);
        assert_eq!(
            check(&kind, &counter(5, true)),
            Verdict::Unsafe { depth: Some(5) }
        );
    }

    /// Word operators against Rust arithmetic: with constant operands,
    /// `bad = (a op b != expected)` must be unreachable at depth 0.
    #[test]
    fn operators_match_reference() {
        let bmc = Bmc {
            max_depth: Some(0),
            step: 1,
        };
        let w = 6u32;
        let mask = (1u64 << w) - 1;
        let sx = |v: u64| -> i64 { ((v << (64 - w)) as i64) >> (64 - w) };
        type Unsigned = fn(u64, u64, u32) -> u64;
        type Signed = fn(i64, i64) -> i64;
        let cases: &[(&str, Unsigned)] = &[
            ("add", |a, b, _| a + b),
            ("sub", |a, b, _| a.wrapping_sub(b)),
            ("mul", |a, b, _| a * b),
            ("udiv", |a, b, _| a.checked_div(b).unwrap_or(u64::MAX)),
            ("urem", |a, b, _| if b == 0 { a } else { a % b }),
            ("sll", |a, b, w| if b >= w as u64 { 0 } else { a << b }),
            ("srl", |a, b, w| if b >= w as u64 { 0 } else { a >> b }),
        ];
        let signed: &[(&str, Signed)] = &[
            ("sdiv", |a, b| {
                if b == 0 {
                    if a < 0 {
                        1
                    } else {
                        -1
                    }
                } else {
                    a.wrapping_div(b)
                }
            }),
            ("srem", |a, b| if b == 0 { a } else { a.wrapping_rem(b) }),
            // SMT-LIB bvsmod: the sign follows the divisor.
            ("smod", |a, b| {
                if b == 0 {
                    return a;
                }
                let r = a % b;
                if r != 0 && (r < 0) != (b < 0) {
                    r + b
                } else {
                    r
                }
            }),
        ];
        let mut seed = 12345u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed & mask
        };
        let model = |op: &str, a: u64, b: u64, c: u64| {
            format!("1 sort bitvec {w}\n2 sort bitvec 1\n3 constd 1 {a}\n4 constd 1 {b}\n5 {op} 1 3 4\n6 constd 1 {c}\n7 neq 2 5 6\n8 bad 7\n")
        };
        for _ in 0..12 {
            let (a, b) = (rnd(), rnd());
            for (op, f) in cases {
                let c = f(a, b, w) & mask;
                assert!(
                    matches!(check(&bmc, &model(op, a, b, c)), Verdict::Unknown { .. }),
                    "{op} {a} {b} -> {c}"
                );
            }
            for (op, f) in signed {
                let (x, y) = (sx(a), sx(b));
                let r = f(x, y);
                let c = (r as u64) & mask;
                assert!(
                    matches!(check(&bmc, &model(op, a, b, c)), Verdict::Unknown { .. }),
                    "{op} {x} {y} -> {r}"
                );
            }
        }
    }

    #[test]
    fn ic3_proves_and_refutes() {
        for e in [Ic3Engine { mini: true }, Ic3Engine { mini: false }] {
            assert_eq!(check(&e, &counter(7, true)), Verdict::Safe);
            assert_eq!(check(&e, &counter(6, true)), Verdict::Safe);
            assert_eq!(
                check(&e, &counter(5, true)),
                Verdict::Unsafe { depth: Some(5) }
            );
            assert_eq!(
                check(&e, &counter(4, false)),
                Verdict::Unsafe { depth: Some(4) }
            );
            assert_eq!(
                check(&e, &counter(0, false)),
                Verdict::Unsafe { depth: Some(0) }
            );
        }
    }

    #[test]
    fn ic3_mt_proves_and_refutes() {
        let e = Ic3Mt::default();
        for _ in 0..5 {
            assert_eq!(check(&e, &counter(7, true)), Verdict::Safe);
            assert_eq!(check(&e, &counter(6, true)), Verdict::Safe);
            assert_eq!(
                check(&e, &counter(5, true)),
                Verdict::Unsafe { depth: Some(5) }
            );
            assert_eq!(
                check(&e, &counter(4, false)),
                Verdict::Unsafe { depth: Some(4) }
            );
        }
    }

    /// Two copies of the wrapping counter, bad when they differ or when
    /// the first reaches 7: scorr merges the copies (3 latches left) and
    /// every engine still proves it; a reachable target stays unsafe.
    #[test]
    fn scorr_merges_copies() {
        let twin = |target: u32| {
            format!(
                "1 sort bitvec 3\n2 sort bitvec 1\n3 zero 1\n4 state 1 a\n5 init 1 4 3\n\
                 6 one 1\n7 add 1 4 6\n8 constd 1 5\n9 eq 2 4 8\n10 ite 1 9 3 7\n11 next 1 4 10\n\
                 12 state 1 b\n13 init 1 12 3\n14 add 1 12 6\n15 eq 2 12 8\n16 ite 1 15 3 14\n\
                 17 next 1 12 16\n18 neq 2 4 12\n19 constd 1 {target}\n20 eq 2 4 19\n21 or 2 18 20\n22 bad 21\n"
            )
        };
        let dir = std::env::temp_dir().join(format!("informal-scorr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("twin.btor2");
        std::fs::write(&path, twin(7)).unwrap();
        let req = CheckRequest {
            model: path,
            format: ModelFormat::Btor2,
            timeout: Duration::from_secs(10),
        };
        let ctx = RunContext::new(req.timeout);
        let g = prepare(&req).unwrap().0;
        assert_eq!(g.latches.len(), 6);
        let (h, st, _, _) = scorr::latch_correspondence(&g, solver(&ctx), &ctx).unwrap();
        assert_eq!(st.merged, 3);
        assert_eq!(h.latches.len(), 3);
        let ic3 = Ic3Engine { mini: true };
        let kind = KInduction {
            max_depth: Some(20),
        };
        assert_eq!(check(&ic3, &twin(7)), Verdict::Safe);
        assert_eq!(check(&kind, &twin(7)), Verdict::Safe);
        assert_eq!(check(&ic3, &twin(4)), Verdict::Unsafe { depth: Some(4) });
        assert_eq!(check(&kind, &twin(4)), Verdict::Unsafe { depth: Some(4) });
    }

    #[test]
    fn kind_ic3_proves_and_refutes() {
        let e = KindIc3 { ic3_share: 0.5 };
        assert_eq!(check(&e, &counter(7, true)), Verdict::Safe);
        assert_eq!(check(&e, &counter(6, true)), Verdict::Safe);
        assert_eq!(
            check(&e, &counter(5, true)),
            Verdict::Unsafe { depth: Some(5) }
        );
    }

    /// Houdini keeps exactly the inductive lemmas: for the counter that
    /// wraps at 5, "not 6" and "not 7" are inductive together, "not 3" is
    /// not even true.
    #[test]
    fn houdini_keeps_inductive_subset() {
        let dir = std::env::temp_dir().join(format!("informal-houdini-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.btor2");
        std::fs::write(&path, counter(7, true)).unwrap();
        let g = prepare(&CheckRequest {
            model: path,
            format: ModelFormat::Btor2,
            timeout: Duration::from_secs(10),
        })
        .unwrap()
        .0;
        let ctx = RunContext::new(Duration::from_secs(10));
        // Latch bits b0..b2 as AIG literals, lowest bit first.
        let bits: Vec<_> = g.latches.iter().map(|l| l.lit).collect();
        assert_eq!(bits.len(), 3);
        let cube = |v: u32| -> ic3::Cube {
            let mut c: Vec<_> = (0..3)
                .map(|i| {
                    if v >> i & 1 == 1 {
                        bits[i]
                    } else {
                        informal_aig::neg(bits[i])
                    }
                })
                .collect();
            c.sort();
            c
        };
        let inv =
            invariant::houdini(&g, vec![cube(6), cube(7), cube(3)], || solver(&ctx), &ctx).unwrap();
        let mut want = vec![cube(6), cube(7)];
        want.sort();
        assert_eq!(inv, want);
    }

    #[test]
    fn repo_models() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/models");
        let kind = KInduction {
            max_depth: Some(40),
        };
        let read = |f: &str| std::fs::read_to_string(root.join(f)).unwrap();
        assert_eq!(
            check(&kind, &read("counter.btor2")),
            Verdict::Unsafe { depth: Some(7) }
        );
        // Arrays and liveness are reported, not guessed.
        assert!(
            matches!(check(&kind, &read("memory.btor2")), Verdict::Unknown { reason } if reason.contains("arrays"))
        );
        assert!(matches!(
            check(&kind, &read("liveness.btor2")),
            Verdict::Unknown { .. }
        ));
    }
}
