//! In-house engines on SAT: bounded model checking and k-induction.
//!
//! Both load the model at bit level (AIGER directly, Btor2 bit-blasted by
//! `informal-aig`), simplify it, and unroll it frame by frame into CaDiCaL (see
//! `informal-sat`) with [`unroll::Unroll`]. They are the stage-1 engines of the
//! round-2 plan; IC3 follows in stage 2. Arrays and liveness are not
//! supported at bit level yet and report unknown.

pub mod cnf;
pub mod ic3;
pub mod invariant;
pub mod scorr;
pub mod unroll;

use informal_aig::Aig;
use informal_engine::{
    Capabilities, CheckRequest, Engine, EngineResult, ModelFormat, Progress, RunContext, Verdict,
};
use informal_sat::{Answer, Sat};
use std::sync::Arc;
use std::time::Instant;
use unroll::Unroll;

pub use informal_aig::load as load_aig;

/// The simplified model, or the reason this engine cannot take it.
fn prepare(req: &CheckRequest) -> Result<Aig, String> {
    let g = load_aig(&req.model).map_err(|e| format!("{e:#}"))?;
    if g.bads.is_empty() {
        return Err("model has no bad-state property".into());
    }
    Ok(informal_aig::preprocess::simplify(&g).0)
}

/// [`prepare`], then latch correspondence ([`scorr`]) within 5% of the
/// time left (at most 10 s): on the big CPU models of HWMCC'24 1-induction
/// peels off about one candidate per SAT call and never finishes, so the
/// budget only bounds the waste. `INFORMAL_NO_SCORR` turns it off.
///
/// Every engine prepares its own copy, so with a portfolio over budget
/// this checks between phases: stopping only in the SAT loop lets the
/// load, scorr and CNF build of each engine run on past the budget.
fn prepare_ctx(req: &CheckRequest, ctx: &RunContext) -> Result<Aig, String> {
    // TODO(D7, Gemini P7): build the prepared AIG once and share it as
    // Arc<Aig> across portfolio workers instead of one copy per engine.
    let stop = || {
        if ctx.should_stop() {
            Err(ctx.stop_reason().to_string())
        } else {
            Ok(())
        }
    };
    stop()?;
    let g = prepare(req)?;
    stop()?;
    if std::env::var_os("INFORMAL_NO_SCORR").is_some() {
        return Ok(g);
    }
    let now = Instant::now();
    let left = ctx.deadline.saturating_duration_since(now);
    let mut c = ctx.clone();
    c.deadline = now + (left / 20).min(std::time::Duration::from_secs(10));
    match scorr::latch_correspondence(&g, solver(&c), &c) {
        Some((h, st)) => {
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
            Ok(h)
        }
        None => Ok(g),
    }
    .and_then(|g| stop().map(|()| g))
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
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx(req, ctx) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let step = self.step.max(1);
        let mut witness = None;
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
                u.add_frame();
                bads.push(u.bad(f));
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
        let mut r = result(self.name(), v, t0);
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
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx(req, ctx) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let v = kind(&g, ctx, self.name(), self.max_depth, &[]);
        Ok(result(self.name(), v, t0))
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
) -> Verdict {
    let mut base = Unroll::new(g, solver(ctx), false);
    let mut step = Unroll::new(g, solver(ctx), true);
    let mut k = 0;
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
                break Verdict::Unsafe {
                    depth: Some(k as u32),
                }
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
                Answer::Unsat => break 'outer Verdict::Safe,
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
    v
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
            let mut s = informal_sat::Mini::default();
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
        let v = ic3.check(ctx);
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
        Ok(result(self.name(), v, t0))
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
            match ic3.check(&phase) {
                Verdict::Unknown { .. } => {}
                v => return Ok(result(self.name(), v, t0)),
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
        let v = kind(&g, ctx, self.name(), None, &inv);
        Ok(result(self.name(), v, t0))
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
                (true, d),
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
                            let mut s = informal_sat::Mini::default();
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
        Ok(result(self.name(), v, t0))
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
        let g = prepare(&req).unwrap();
        assert_eq!(g.latches.len(), 6);
        let (h, st) = scorr::latch_correspondence(&g, solver(&ctx), &ctx).unwrap();
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
        .unwrap();
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
