//! Word-level k-induction (`informal-wkind`) on Bitwuzla.

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

/// Word-level k-induction (with simple-path state uniqueness) on Bitwuzla.
/// Proves safety properties directly over bit-vectors, wide arithmetic, and
/// unblasted Btor2 arrays (`Sort::Array`).
pub struct WordKind {
    pub max_depth: Option<usize>,
}

impl Engine for WordKind {
    fn name(&self) -> &str {
        "informal-wkind"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: vec![ModelFormat::Btor2],
            proves: true,
            finds_bugs: true,
            domain: EngineDomain::WordLevel,
            kind: EngineKind::KInduction,
            arrays: true,
            min_effort: Effort::Standard,
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
        let verdict = run_kind(&m, ctx, self.max_depth, self.name())?;
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
fn run_kind(_: &Model, _: &RunContext, _: Option<usize>, _: &str) -> anyhow::Result<Verdict> {
    Ok(Verdict::Unknown {
        reason: "built without Bitwuzla".into(),
    })
}

#[cfg(informal_bitwuzla)]
fn run_kind(
    m: &Model,
    ctx: &RunContext,
    max: Option<usize>,
    name: &str,
) -> anyhow::Result<Verdict> {
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
        let bz_base = Bz::new(left, cancel);
        let bz_step = Bz::new(left, cancel);
        unroll_kind(m, &bz_base, &bz_step, ctx, max, name)
    })
}

#[cfg(informal_bitwuzla)]
fn unroll_kind(
    m: &Model,
    bz_base: &Bz,
    bz_step: &Bz,
    ctx: &RunContext,
    max: Option<usize>,
    name: &str,
) -> anyhow::Result<Verdict> {
    let sorts_base = build_sorts(m, bz_base)?;
    let sorts_step = build_sorts(m, bz_step)?;
    let next: HashMap<u32, i64> = m
        .with_op(Op::Next)
        .map(|l| (l.args[0] as u32, l.args[1]))
        .collect();
    let init: HashMap<u32, i64> = m
        .with_op(Op::Init)
        .map(|l| (l.args[0] as u32, l.args[1]))
        .collect();
    let state_ids: Vec<u32> = m.with_op(Op::State).map(|l| l.id).collect();

    let mut prev_base: HashMap<u32, T> = HashMap::new();
    let mut prev_step: HashMap<u32, T> = HashMap::new();
    let mut step_states: Vec<Vec<T>> = Vec::new();
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

        let (f_base, bad_base) =
            unroll_step(m, bz_base, &sorts_base, &init, &next, &prev_base, k, true)?;
        match bz_base.solve(&[bad_base])? {
            10 => {
                return Ok(Verdict::Unsafe {
                    depth: Some(k as u32),
                })
            }
            20 => {
                if state_ids.is_empty() {
                    return Ok(Verdict::Safe);
                }
                let nb = bz_base.op("not", &[bad_base], 0, 0)?;
                bz_base.assert(nb)?;
            }
            _ => {
                return Ok(Verdict::Unknown {
                    reason: ctx.stop_reason().into(),
                })
            }
        }
        prev_base = f_base;

        let (f_step, bad_step) =
            unroll_step(m, bz_step, &sorts_step, &init, &next, &prev_step, k, false)?;
        let cur_snap: Vec<T> = state_ids.iter().map(|s| f_step[s]).collect();
        if !state_ids.is_empty() && k <= 32 {
            for earlier in &step_states {
                let mut diffs = Vec::with_capacity(state_ids.len());
                for (&t_old, &t_new) in earlier.iter().zip(&cur_snap) {
                    let eq = bz_step.op("eq", &[t_old, t_new], 0, 0)?;
                    diffs.push(bz_step.op("not", &[eq], 0, 0)?);
                }
                let mut any_diff = diffs[0];
                for &d in &diffs[1..] {
                    any_diff = bz_step.op("or", &[any_diff, d], 0, 0)?;
                }
                bz_step.assert(any_diff)?;
            }
        }
        step_states.push(cur_snap);

        match bz_step.solve(&[bad_step])? {
            20 => return Ok(Verdict::Safe),
            10 => {
                let nb = bz_step.op("not", &[bad_step], 0, 0)?;
                bz_step.assert(nb)?;
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
        prev_step = f_step;
        k += 1;
    }
}
