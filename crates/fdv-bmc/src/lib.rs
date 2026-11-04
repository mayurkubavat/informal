//! In-house engines on SAT: bounded model checking and k-induction.
//!
//! Both load the model at bit level (AIGER directly, Btor2 bit-blasted by
//! `fdv-aig`), simplify it, and unroll it frame by frame into CaDiCaL (see
//! `fdv-sat`) with [`unroll::Unroll`]. They are the stage-1 engines of the
//! round-2 plan; IC3 follows in stage 2. Arrays and liveness are not
//! supported at bit level yet and report unknown.

pub mod cnf;
pub mod ic3;
pub mod invariant;
pub mod unroll;

use fdv_aig::Aig;
use fdv_engine::{
    Capabilities, CheckRequest, Engine, EngineResult, ModelFormat, Progress, RunContext, Verdict,
};
use fdv_sat::{Answer, Sat};
use std::sync::Arc;
use std::time::Instant;
use unroll::Unroll;

pub use fdv_aig::load as load_aig;

/// The simplified model, or the reason this engine cannot take it.
fn prepare(req: &CheckRequest) -> Result<Aig, String> {
    let g = load_aig(&req.model).map_err(|e| format!("{e:#}"))?;
    if g.bads.is_empty() {
        return Err("model has no bad-state property".into());
    }
    Ok(fdv_aig::preprocess::simplify(&g).0)
}

fn solver(ctx: &RunContext) -> Box<dyn Sat> {
    let mut sat = fdv_sat::best();
    let (cancel, deadline) = (ctx.cancel.clone(), ctx.deadline);
    sat.set_terminate(Box::new(move || {
        cancel.is_cancelled() || Instant::now() >= deadline || fdv_engine::memory::exceeded()
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
/// for k = 0, 1, 2, ...? Finds the shortest counterexample; never proves.
pub struct Bmc {
    pub max_depth: Option<usize>,
}

impl Engine for Bmc {
    fn name(&self) -> &str {
        "fdv-bmc"
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
        let g = match prepare(req) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let mut u = Unroll::new(&g, solver(ctx), false);
        let mut k = 0;
        let v = loop {
            if ctx.should_stop() {
                break unknown(ctx.stop_reason());
            }
            if self.max_depth.is_some_and(|m| k > m) {
                break unknown(format!("bound {} reached", k - 1));
            }
            u.add_frame();
            let bad = u.bad(k);
            match u.sat.solve(&[bad]) {
                Answer::Sat => {
                    break Verdict::Unsafe {
                        depth: Some(k as u32),
                    }
                }
                Answer::Unsat => u.sat.add_clause(&[-bad]),
                Answer::Unknown => break unknown(ctx.stop_reason()),
            }
            ctx.report(Progress {
                engine: self.name().into(),
                bound: Some(k as u32),
                note: String::new(),
            });
            k += 1;
        };
        Ok(result(self.name(), v, t0))
    }
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
        "fdv-kind"
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
        let g = match prepare(req) {
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
    let mut base = Unroll::new(&g, solver(ctx), false);
    let mut step = Unroll::new(&g, solver(ctx), true);
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
        });
        k += 1;
    };
    v
}

/// IC3 / PDR on the AIG (see [`ic3`]). `mini` picks fdv's own small CDCL
/// solver for the frame and lifting solvers instead of CaDiCaL.
pub struct Ic3Engine {
    pub mini: bool,
}

impl Engine for Ic3Engine {
    fn name(&self) -> &str {
        if self.mini {
            "fdv-ic3"
        } else {
            "fdv-ic3-cadical"
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
        let g = match prepare(req) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let c = ctx.clone();
        let mini = self.mini;
        let make = move || -> Box<dyn Sat> {
            if !mini {
                return solver(&c);
            }
            let mut s = fdv_sat::Mini::default();
            let (cancel, deadline) = (c.cancel.clone(), c.deadline);
            s.set_terminate(Box::new(move || {
                cancel.is_cancelled()
                    || Instant::now() >= deadline
                    || fdv_engine::memory::exceeded()
            }));
            Box::new(s)
        };
        let Some(mut ic3) = ic3::Ic3::new(&g, Box::new(make)) else {
            return Ok(result(
                self.name(),
                unknown("latch inits that are not constants"),
                t0,
            ));
        };
        let v = ic3.check(ctx);
        if std::env::var_os("FDV_IC3_STATS").is_some() {
            eprintln!(
                "{}: frames {} lemmas {} queries {} ({:.2} s) lift {:.2} s",
                self.name(),
                ic3.depth(),
                ic3.stats_lemmas,
                ic3.stats_queries,
                ic3.stats_query_time.as_secs_f64(),
                ic3.stats_lift_time.as_secs_f64()
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
        "fdv-kind-ic3"
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
        let g = match prepare(req) {
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
        if std::env::var_os("FDV_IC3_STATS").is_some() {
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
                    },
                ),
                (
                    true,
                    Config {
                        max_ctgs: 5,
                        max_ctg_depth: 2,
                        reverse_drop: true,
                    },
                ),
            ],
        }
    }
}

impl Engine for Ic3Mt {
    fn name(&self) -> &str {
        "fdv-ic3-mt"
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
        let g = match prepare(req) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let pool = Arc::new(ic3::LemmaPool::default());
        // Workers stop on our token: cancelled by the winner or by `ctx`.
        let mut inner = ctx.clone();
        inner.cancel = fdv_engine::CancelToken::new();
        let done = std::sync::atomic::AtomicBool::new(false);
        let stats = std::env::var_os("FDV_IC3_STATS").is_some();
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
                            let mut s = fdv_sat::Mini::default();
                            let (cancel, deadline) = (c.cancel.clone(), c.deadline);
                            s.set_terminate(Box::new(move || {
                                cancel.is_cancelled()
                                    || Instant::now() >= deadline
                                    || fdv_engine::memory::exceeded()
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
                                "fdv-ic3-mt[{id}]: {:?} frames {} lemmas {} imported {}",
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
    if fdv_sat::Cadical::signature().is_none() {
        return Vec::new();
    }
    vec![
        Arc::new(Bmc { max_depth: None }),
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
        let dir = std::env::temp_dir().join(format!("fdv-bmc-{}-{}", std::process::id(), e.name()));
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
        let bmc = Bmc { max_depth: Some(0) };
        let w = 6u32;
        let mask = (1u64 << w) - 1;
        let sx = |v: u64| -> i64 { ((v << (64 - w)) as i64) >> (64 - w) };
        let cases: &[(&str, fn(u64, u64, u32) -> u64)] = &[
            ("add", |a, b, _| a + b),
            ("sub", |a, b, _| a.wrapping_sub(b)),
            ("mul", |a, b, _| a * b),
            ("udiv", |a, b, _| if b == 0 { u64::MAX } else { a / b }),
            ("urem", |a, b, _| if b == 0 { a } else { a % b }),
            ("sll", |a, b, w| if b >= w as u64 { 0 } else { a << b }),
            ("srl", |a, b, w| if b >= w as u64 { 0 } else { a >> b }),
        ];
        let signed: &[(&str, fn(i64, i64) -> i64)] = &[
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
        let dir = std::env::temp_dir().join(format!("fdv-houdini-{}", std::process::id()));
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
                        fdv_aig::neg(bits[i])
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
