//! In-house engines on SAT: bounded model checking and k-induction.
//!
//! Both load the model at bit level (AIGER directly, Btor2 bit-blasted by
//! `fdv-aig`), simplify it, and unroll it frame by frame into CaDiCaL (see
//! `fdv-sat`) with [`unroll::Unroll`]. They are the stage-1 engines of the
//! round-2 plan; IC3 follows in stage 2. Arrays and liveness are not
//! supported at bit level yet and report unknown.

pub mod cnf;
pub mod ic3;
pub mod unroll;

use anyhow::Context;
use fdv_aig::Aig;
use fdv_engine::{
    Capabilities, CheckRequest, Engine, EngineResult, ModelFormat, Progress, RunContext, Verdict,
};
use fdv_sat::{Answer, Sat};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use unroll::Unroll;

/// Load a model at bit level: AIGER as is, Btor2 bit-blasted.
pub fn load_aig(path: &Path) -> anyhow::Result<Aig> {
    match ModelFormat::of_path(path) {
        Some(ModelFormat::Aiger) => fdv_aig::aiger::read_file(path),
        Some(ModelFormat::Btor2) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            fdv_aig::btor::from_btor2(&fdv_ir::parse(&text)?)
        }
        None => anyhow::bail!("unknown model format: {}", path.display()),
    }
}

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
        cancel.is_cancelled() || Instant::now() >= deadline
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
                break unknown("timeout");
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
                Answer::Unknown => break unknown("timeout"),
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
        let mut base = Unroll::new(&g, solver(ctx), false);
        let mut step = Unroll::new(&g, solver(ctx), true);
        let mut k = 0;
        let v = 'outer: loop {
            if ctx.should_stop() {
                break unknown("timeout");
            }
            if self.max_depth.is_some_and(|m| k > m) {
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
                Answer::Unknown => break unknown("timeout"),
            }
            // Step: good frames 0..k-1, then bad at k?
            step.add_frame();
            let bad = step.bad(k);
            loop {
                match step.sat.solve(&[bad]) {
                    Answer::Unsat => break 'outer Verdict::Safe,
                    Answer::Unknown => break 'outer unknown("timeout"),
                    Answer::Sat => match step.repeated_states(k) {
                        Some((i, j)) => step.differ(i, j),
                        None => break,
                    },
                }
            }
            step.sat.add_clause(&[-bad]);
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

/// IC3 / PDR on the AIG (see [`ic3`]).
pub struct Ic3Engine;

impl Engine for Ic3Engine {
    fn name(&self) -> &str {
        "fdv-ic3"
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
        let Some(mut ic3) = ic3::Ic3::new(&g, Box::new(move || solver(&c))) else {
            return Ok(result(
                self.name(),
                unknown("latch inits that are not constants"),
                t0,
            ));
        };
        let v = ic3.check(ctx);
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
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
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
        let e = Ic3Engine;
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

    #[test]
    fn repo_models() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/models");
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
