//! Native word-level unbounded IC3 with syntax-guided abstraction and
//! counterexample-guided prophecy (`informal-ic3sa`).
//!
//! Solves unbounded safety properties directly over bit-vectors, wide
//! arithmetic datapaths, and unblasted Btor2 arrays (`Sort::Array`) without
//! bit-blasting. Every `Safe` verdict is independently re-verified via 3
//! fresh-solver SMT checks (Initiation, Safety, Consecution), and every
//! `Unsafe` verdict is confirmed by a fresh concrete BMC query on the
//! original model.

#[cfg(informal_bitwuzla)]
mod certify;
#[cfg(informal_bitwuzla)]
mod frames;
#[cfg(informal_bitwuzla)]
mod wts;

#[cfg(informal_bitwuzla)]
use crate::ffi::with_cancel;
use informal_engine::{
    Capabilities, CheckRequest, Effort, Engine, EngineDomain, EngineKind, EngineResult,
    ModelFormat, RunContext, Verdict,
};
use informal_ir::Model;
#[cfg(informal_bitwuzla)]
use informal_ir::Op;
use std::time::Instant;

/// Word-level IC3SA + prophecy engine on Bitwuzla.
pub struct Ic3Sa;

impl Engine for Ic3Sa {
    fn name(&self) -> &str {
        "informal-ic3sa"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: vec![ModelFormat::Btor2],
            proves: true,
            finds_bugs: true,
            domain: EngineDomain::WordLevel,
            kind: EngineKind::Ic3,
            arrays: true,
            min_effort: Effort::Standard,
            ..Default::default()
        }
    }
    fn shed_rank(&self) -> u32 {
        2
    }
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let text = std::fs::read_to_string(&req.model)?;
        let m = informal_ir::parse(&text)?;
        let (verdict, witness) = run(&m, ctx, self.name())?;
        Ok(EngineResult {
            engine: self.name().into(),
            verdict,
            wall_ms: t0.elapsed().as_millis(),
            witness,
            certificate: None,
        })
    }
}

#[cfg(not(informal_bitwuzla))]
fn run(_: &Model, _: &RunContext, _: &str) -> anyhow::Result<(Verdict, Option<String>)> {
    Ok((
        Verdict::Unknown {
            reason: "built without Bitwuzla".into(),
        },
        None,
    ))
}

#[cfg(informal_bitwuzla)]
fn run(m: &Model, ctx: &RunContext, name: &str) -> anyhow::Result<(Verdict, Option<String>)> {
    if m.with_op(Op::Bad).next().is_none() {
        return Ok((
            Verdict::Unknown {
                reason: "model has no bad-state property".into(),
            },
            None,
        ));
    }
    if m.with_op(Op::Fair).next().is_some() || m.with_op(Op::Justice).next().is_some() {
        return Ok((
            Verdict::Unknown {
                reason: "liveness is not supported by this engine".into(),
            },
            None,
        ));
    }

    with_cancel(ctx, |left_ms, cancel| {
        // Outer CEGAR refinement loop: start with prophecies seeded from array
        // reads in `Bad`, adding up to 2 extra unconstrained prophecies if a
        // spurious abstract counterexample occurs.
        for extra_prophecies in 0..=2usize {
            if ctx.should_stop() {
                return Ok((
                    Verdict::Unknown {
                        reason: ctx.stop_reason().into(),
                    },
                    None,
                ));
            }
            let wts = wts::Wts::from_model(m, extra_prophecies)?;
            let mut solver = frames::Ic3Solver::new(m, &wts, ctx, left_ms, cancel, name)?;
            match solver.run()? {
                frames::Ic3Outcome::Safe { invariant } => {
                    if std::env::var_os("INFORMAL_NO_CERT").is_some() {
                        return Ok((Verdict::Safe, None));
                    }
                    let rem = ctx
                        .deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis()
                        .max(1) as i64;
                    match certify::certify_invariant(m, &wts, &invariant, rem, cancel) {
                        Ok(()) => return Ok((Verdict::Safe, None)),
                        Err(e) if e.ends_with("stopped") => {
                            return Ok((
                                Verdict::Unknown {
                                    reason: ctx.stop_reason().into(),
                                },
                                None,
                            ))
                        }
                        Err(e) => {
                            eprintln!("informal-ic3sa: verdict withdrawn, certificate {e}");
                            return Ok((
                                Verdict::Unknown {
                                    reason: format!("certificate {e}"),
                                },
                                None,
                            ));
                        }
                    }
                }
                frames::Ic3Outcome::Unsafe { depth, witness } => {
                    return Ok((Verdict::Unsafe { depth: Some(depth) }, witness));
                }
                frames::Ic3Outcome::Spurious => {
                    if wts.arr_states.is_empty() {
                        break;
                    }
                    continue;
                }
                frames::Ic3Outcome::Stopped => {
                    return Ok((
                        Verdict::Unknown {
                            reason: ctx.stop_reason().into(),
                        },
                        None,
                    ));
                }
            }
        }
        Ok((
            Verdict::Unknown {
                reason: "ic3sa refinement limit reached".into(),
            },
            None,
        ))
    })
}
