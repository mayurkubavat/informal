//! k-Induction (`informal-kind`) and IC3-strengthened k-induction (`informal-kind-ic3`).

use crate::prep::{fold_on, prepare_ctx_fold, result, solver, unknown, Checked, FORMATS};
use crate::unroll::Unroll;
use crate::verify::{certified, kind_certified, kind_unsafe, unsafe_certified_at, PreparedTrace};
use crate::{ic3, invariant};
use informal_aig::Aig;
use informal_engine::{
    Capabilities, CheckRequest, Effort, Engine, EngineDomain, EngineKind, EngineResult, Progress,
    RunContext, Verdict,
};
use informal_sat::Answer;
use std::time::Instant;

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
            liveness: true,
            domain: EngineDomain::BitLevel,
            kind: EngineKind::KInduction,
            min_effort: Effort::Low,
            certifies: true,
            ..Default::default()
        }
    }
    fn shed_rank(&self) -> u32 {
        2
    }
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx_fold(req, ctx, fold_on()) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let (v, k, tr) = kind(&g, ctx, self.name(), self.max_depth, &[]);
        let v = kind_certified(req, &g, (v, k), &[], ctx);
        Ok(result(self.name(), kind_unsafe(req, &g, v, ctx, tr), t0))
    }
}

/// The k-induction loop of [`KInduction`]; `invariants` (cubes whose
/// negation holds in every reachable state) strengthen every step frame.
pub(crate) fn kind(
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
            liveness: true,
            domain: EngineDomain::BitLevel,
            kind: EngineKind::Hybrid,
            min_effort: Effort::Standard,
            certifies: true,
            ..Default::default()
        }
    }
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx_fold(req, ctx, fold_on()) {
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
                Checked {
                    verdict: Verdict::Unknown { .. },
                    ..
                } => {}
                v => {
                    return Ok(result(
                        self.name(),
                        unsafe_certified_at(req, v, ctx, None, g.folded.is_some()),
                        t0,
                    ))
                }
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
        Ok(result(self.name(), kind_unsafe(req, &g, v, ctx, tr), t0))
    }
}
