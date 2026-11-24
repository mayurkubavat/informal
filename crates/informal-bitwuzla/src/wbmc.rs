//! Word-level bounded model checking (`informal-wbmc`) on Bitwuzla.

#[cfg(informal_bitwuzla)]
use crate::ffi::{build_sorts, unroll_step, with_cancel, Bz, T};
#[cfg(informal_bitwuzla)]
use informal_engine::Progress;
use informal_engine::{
    Capabilities, CheckRequest, Effort, Engine, EngineDomain, EngineKind, EngineResult,
    ModelFormat, RunContext, Verdict,
};
use informal_ir::Model;
#[cfg(informal_bitwuzla)]
use informal_ir::Op;
#[cfg(informal_bitwuzla)]
use std::collections::HashMap;
use std::time::Instant;

/// Word-level BMC on Bitwuzla.
pub struct WordBmc {
    pub max_depth: Option<usize>,
}

impl Engine for WordBmc {
    fn name(&self) -> &str {
        "informal-wbmc"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: vec![ModelFormat::Btor2],
            finds_bugs: true,
            domain: EngineDomain::WordLevel,
            kind: EngineKind::Bmc,
            arrays: true,
            min_effort: Effort::Low,
            ..Default::default()
        }
    }
    fn shed_rank(&self) -> u32 {
        3
    }
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let text = std::fs::read_to_string(&req.model)?;
        let m = informal_ir::parse(&text)?;
        let eff_max = self.max_depth.or_else(|| req.max_depth.map(|d| d as usize));
        let verdict = run(&m, ctx, eff_max, self.name())?;
        Ok(EngineResult {
            engine: self.name().into(),
            verdict,
            wall_ms: t0.elapsed().as_millis(),
            witness: None,
            certificate: None,
        })
    }
}

#[cfg(not(informal_bitwuzla))]
fn run(_: &Model, _: &RunContext, _: Option<usize>, _: &str) -> anyhow::Result<Verdict> {
    Ok(Verdict::Unknown {
        reason: "built without Bitwuzla".into(),
    })
}

#[cfg(informal_bitwuzla)]
fn run(m: &Model, ctx: &RunContext, max: Option<usize>, name: &str) -> anyhow::Result<Verdict> {
    if m.with_op(Op::Bad).next().is_none() {
        return Ok(Verdict::Unknown {
            reason: "model has no bad-state property".into(),
        });
    }
    if m.with_op(Op::Fair).next().is_some() || m.with_op(Op::Justice).next().is_some() {
        return Ok(Verdict::Unknown {
            reason: "liveness is not supported by this engine".into(),
        });
    }
    with_cancel(ctx, |left, cancel| {
        let bz = Bz::new(left, cancel);
        unroll(m, &bz, ctx, max, name)
    })
}

#[cfg(informal_bitwuzla)]
fn unroll(
    m: &Model,
    bz: &Bz,
    ctx: &RunContext,
    max: Option<usize>,
    name: &str,
) -> anyhow::Result<Verdict> {
    let sorts = build_sorts(m, bz)?;
    let next: HashMap<u32, i64> = m
        .with_op(Op::Next)
        .map(|l| (l.args[0] as u32, l.args[1]))
        .collect();
    let init: HashMap<u32, i64> = m
        .with_op(Op::Init)
        .map(|l| (l.args[0] as u32, l.args[1]))
        .collect();
    let mut prev: HashMap<u32, T> = HashMap::new();
    let mut k = 0usize;
    loop {
        if ctx.should_stop() {
            return Ok(Verdict::Unknown {
                reason: ctx.stop_reason().into(),
            });
        }
        if max.is_some_and(|d| k > d) {
            return Ok(Verdict::Unknown {
                reason: format!("bound {} reached", k - 1),
            });
        }
        let (f, any) = unroll_step(m, bz, &sorts, &init, &next, &prev, k, true)?;
        match bz.solve(&[any])? {
            10 => {
                return Ok(Verdict::Unsafe {
                    depth: Some(k as u32),
                })
            }
            20 => {
                let nb = bz.op("not", &[any], 0, 0)?;
                bz.assert(nb)?;
            }
            _ => {
                return Ok(Verdict::Unknown {
                    reason: ctx.stop_reason().into(),
                })
            }
        }
        ctx.report(Progress {
            engine: name.into(),
            bound: Some(k as u32),
            note: String::new(),
            output: None,
        });
        prev = f;
        k += 1;
    }
}
