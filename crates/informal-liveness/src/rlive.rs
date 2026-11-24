//! SCC-guided IC3 liveness engine (`informal-rlive`).
//!
//! Avoids both `l2s` shadow latch duplication and `k`-liveness counter blowup
//! by decomposing the constrained transition graph into certified dead traps
//! and forward-invariant SCC half-spaces using only 1 extra latch (`left`):
//!
//! 1. **Certified Fairness-Free Trap Peeling:** from a reachable state `s`,
//!    checks `bad = left & h_j` (using IC3 + independent `certify::check`).
//!    When UNSAT with invariant `Inv(state, left)`, both `T_trap = Inv(·, 1)`
//!    (forward-invariant without `h_j`) and `U_trap = Inv(·, 0)` (forced into
//!    `T_trap` in 1 step) cannot start any infinite fair trace and are
//!    permanently excluded from the constrained search (`C_D <- C_D & !T & !U`).
//! 2. **Certified `O(log N)` SCC Bisection:** when `s` can still reach every
//!    `h_j`, checks whether a midpoint state `s_mid` along a forward trajectory
//!    can return to itself (`bad = left & (state == s_mid)`).
//!    - SAT -> concrete cycle (if fair, verified via `l2s::check_lasso` +
//!      `certify::replay_unsafe`; if non-fair, triggers Trap Peeling on the
//!      missing `h_j`).
//!    - UNSAT -> certified forward-invariant half-space `P = Inv_ret(·, 1)`
//!      strictly excluding `s_mid` (`P & C_D & T & C_D' => P'`). Any fair cycle
//!      entering `P` must remain in `P` forever; recursively proving `P`
//!      fair-cycle-free turns the entire half-space `P ∪ Inv_ret(·, 0)` into a
//!      certified dead trap in one step!

use crate::cycle::{probe_forward, prove_no_fair_cycle_by_monotonic_freeze, ForwardProbe};
use crate::lasso::{prove_raw_safety_bads, try_extract_counterexample, verify_lasso};
use informal_aig::{lit_val, neg, Aig, Init, Lit, FALSE, TRUE};
use informal_bmc::{certify, ic3, result, solver, unknown, Checked, FORMATS};
use informal_engine::{
    Capabilities, Certificate, CheckRequest, Effort, Engine, EngineDomain, EngineKind,
    EngineResult, Progress, RunContext, Verdict,
};
use std::time::{Duration, Instant};

pub struct RLive;

/// Certified result of running IC3 on a safety AIG.
enum SafetyOutcome {
    Safe(Vec<certify::Clause>),
    Unsafe(certify::Trace),
    Unknown,
}

fn run_certified_ic3(g: &Aig, ctx: &RunContext, slice_cap: Duration) -> SafetyOutcome {
    if ctx.should_stop() {
        return SafetyOutcome::Unknown;
    }
    let (simp, _, map, _) = informal_aig::preprocess::simplify_maps(g);
    if simp.bads.iter().all(|&b| b == FALSE) {
        let Ok(inv) = certify::lift(g, &map, &simp, &[]) else {
            return SafetyOutcome::Unknown;
        };
        if std::env::var_os("INFORMAL_NO_CERT").is_none()
            && certify::check(g, &inv, &|| solver(ctx)).is_err()
        {
            return SafetyOutcome::Unknown;
        }
        return SafetyOutcome::Safe(inv);
    }

    let mut sctx = ctx.clone();
    sctx.deadline = ctx.deadline.min(Instant::now() + slice_cap);
    let c_make = sctx.clone();
    let cfg = ic3::Config {
        houdini_every: 4,
        ..ic3::Config::default()
    };
    let Some(mut ic3) = ic3::Ic3::with_config(&simp, Box::new(move || solver(&c_make)), cfg) else {
        return SafetyOutcome::Unknown;
    };
    match ic3.check(&sctx) {
        Verdict::Safe => {
            let Some(lemmas) = ic3.invariant() else {
                return SafetyOutcome::Unknown;
            };
            let Ok(inv) = certify::lift(g, &map, &simp, &lemmas) else {
                return SafetyOutcome::Unknown;
            };
            if std::env::var_os("INFORMAL_NO_CERT").is_none()
                && certify::check(g, &inv, &|| solver(ctx)).is_err()
            {
                return SafetyOutcome::Unknown;
            }
            SafetyOutcome::Safe(inv)
        }
        Verdict::Unsafe { depth: Some(d) } => {
            match certify::find_unsafe(g, d as usize, &|| solver(&sctx), &|| sctx.should_stop()) {
                Ok(tr) => SafetyOutcome::Unsafe(tr),
                Err(_) => SafetyOutcome::Unknown,
            }
        }
        _ => SafetyOutcome::Unknown,
    }
}

/// Evaluate a CNF invariant `clauses` as an AIG literal in `g`.
/// Returns `FALSE` if any clause is empty, `TRUE` if `clauses` is empty.
fn clauses_to_aig(g: &mut Aig, clauses: &[certify::Clause]) -> Lit {
    let mut all = TRUE;
    for cl in clauses {
        if cl.is_empty() {
            return FALSE;
        }
        let mut dis = FALSE;
        for &l in cl {
            dis = g.or(dis, l);
        }
        all = g.and(all, dis);
    }
    all
}

/// Specialize an invariant over `(raw_latches, left)` by fixing `left` to `val`.
fn specialize_left(inv: &[certify::Clause], left_lit: Lit, val: bool) -> Vec<certify::Clause> {
    let true_lit = if val { left_lit } else { neg(left_lit) };
    let false_lit = neg(true_lit);
    let mut out = Vec::new();
    for cl in inv {
        if cl.contains(&true_lit) {
            continue;
        }
        let filtered: Vec<Lit> = cl.iter().copied().filter(|&l| l != false_lit).collect();
        out.push(filtered);
    }
    out
}

/// Build an AIG identical to `base` (keeping its constraints), with all latches
/// initialized to concrete state `state_bits`, 1 extra latch `saw_h` (`0 -> h`),
/// and `bad = saw_h`.
///
/// Soundness: if `Inv(state, saw_h)` is certified on this AIG, then `T(s) =
/// Inv(s, 0)` contains `state_bits`, is forward-invariant under `base.constraints`,
/// and no `C`-legal transition `(s, i) -> s'` from `T` can satisfy `h(s, i)`
/// while keeping `C(s')`. Thus no infinite `C`-legal trace entering `T` can
/// ever witness `h`.
fn build_trap_aig(base: &Aig, state_bits: &[bool], h: Lit) -> (Aig, Lit) {
    let mut g = base.clone();
    g.bads.clear();
    g.justice.clear();
    g.fairness.clear();
    for (l, &b) in g.latches.iter_mut().zip(state_bits) {
        l.init = if b { Init::One } else { Init::Zero };
    }
    let saw_h = g.latch(Init::Zero);
    g.set_next(saw_h, h);
    g.bads.push(saw_h);
    (g, saw_h)
}

/// Build an AIG identical to `base` (keeping its constraints), with all latches
/// initialized to concrete state `state_bits`, 1 extra latch `left` (`0 -> 1`),
/// and `bad = left & target(state)`.
fn build_from_state_aig(
    base: &Aig,
    state_bits: &[bool],
    target: impl FnOnce(&mut Aig) -> Lit,
) -> (Aig, Lit) {
    let mut g = base.clone();
    g.bads.clear();
    g.justice.clear();
    g.fairness.clear();
    for (l, &b) in g.latches.iter_mut().zip(state_bits) {
        l.init = if b { Init::One } else { Init::Zero };
    }
    let left = g.latch(Init::Zero);
    g.set_next(left, TRUE);
    let tgt = target(&mut g);
    let bad = g.and(left, tgt);
    g.bads.push(bad);
    (g, left)
}

fn state_eq_lit(g: &mut Aig, state_bits: &[bool]) -> Lit {
    let mut eq = TRUE;
    for (i, &b) in state_bits.iter().enumerate() {
        let l = g.latches[i].lit;
        eq = g.and(eq, if b { l } else { neg(l) });
    }
    eq
}

/// Simulate `base` along `tr` and return `states[0..=d]` where `d = tr.inputs.len()`.
fn simulate_states(base: &Aig, tr: &certify::Trace) -> Vec<Vec<bool>> {
    let n = base.latches.len();
    let mut state: Vec<bool> = tr.init[..n].to_vec();
    let mut out = Vec::with_capacity(tr.inputs.len() + 1);
    for inp in &tr.inputs {
        out.push(state.clone());
        let raw_in = &inp[..base.inputs.len().min(inp.len())];
        let vals = informal_aig::l2s::eval(base, &state, raw_in);
        state = base
            .latches
            .iter()
            .map(|l| lit_val(&vals, l.next))
            .collect();
    }
    out.push(state);
    out
}

/// Try certified fairness-free trap peeling from `seed_state` over `conds` in
/// `order`. Returns `Ok(true)` if a dead trap was peeled into `work_g.constraints`,
/// or `Ok(false)` along with any post-`h` successor states reached when `seed_state`
/// could still witness `h`.
fn try_peel_trap(
    work_g: &mut Aig,
    seed_state: &[bool],
    conds: &[Lit],
    order: &[usize],
    ctx: &RunContext,
    slice_cap: Duration,
) -> (bool, Vec<Vec<bool>>) {
    let mut after_h_states = Vec::new();
    for &ci in order {
        let h = conds[ci];
        let (trap_g, saw_h) = build_trap_aig(work_g, seed_state, h);
        match run_certified_ic3(&trap_g, ctx, slice_cap) {
            SafetyOutcome::Safe(inv) => {
                let t_clauses = specialize_left(&inv, saw_h, false);
                let t_lit = clauses_to_aig(work_g, &t_clauses);
                if t_lit != FALSE {
                    work_g.constraints.push(neg(t_lit));
                    return (true, after_h_states);
                }
            }
            SafetyOutcome::Unsafe(tr) => {
                let states = simulate_states(work_g, &tr);
                // Frame `d = tr.inputs.len() - 1` is where `saw_h == 1` held under `work_g.constraints`.
                let d = states.len().saturating_sub(2);
                if states[d] != seed_state {
                    after_h_states.push(states[d].clone());
                }
            }
            SafetyOutcome::Unknown => {}
        }
    }
    (false, after_h_states)
}

/// Result of checking whether a constrained region `work_g` contains a fair cycle
/// for `conds` (with `prop_idx` in `raw_g`).
enum RegionResult {
    NoFairCycle,
    /// Found a concrete fair lasso verified on `raw_g`.
    Lasso(Verdict, Option<Certificate>, Option<String>),
    /// Found a fair cycle starting at `head_state` inside a sub-region (`Init` was
    /// relaxed to a half-space `P`); caller must verify reachability from true `Init`.
    CandidateCycle {
        head_state: Vec<bool>,
        loop_inputs: Vec<Vec<bool>>,
    },
    Unknown,
}

/// Core recursive SCC-guided liveness solver.
/// - `is_root`: true when `work_g.latches[i].init` is the true initial state of `raw_g`.
///   False when checking a forward-invariant half-space `P` (where latches start free
///   inside `work_g.constraints`, which already includes `P`).
#[allow(clippy::too_many_arguments)]
fn check_region(
    req: &CheckRequest,
    raw_g: &Aig,
    orig: &Aig,
    prop_idx: usize,
    conds: &[Lit],
    work_g: &mut Aig,
    is_root: bool,
    depth_limit: usize,
    ctx: &RunContext,
) -> RegionResult {
    if conds.is_empty() {
        return RegionResult::Unknown;
    }
    let slice_ms = if is_root { 4000 } else { 2000 };
    let mut iter = 0usize;

    loop {
        if ctx.should_stop() || iter >= 48 {
            return RegionResult::Unknown;
        }
        iter += 1;
        if is_root {
            ctx.report(Progress {
                engine: "informal-rlive".into(),
                bound: Some(iter as u32),
                note: format!("rlive scc-iter {iter}"),
                output: None,
            });
        }

        // 0. Fast `O(W)` monotonic-bit cycle-freeze: collapses saturating counters
        //    and one-way phase transitions without a `k`-liveness counter or shadow
        //    latch copy.
        if prove_no_fair_cycle_by_monotonic_freeze(work_g, conds, ctx) {
            return RegionResult::NoFairCycle;
        }

        // 1. Check whether ANY `h_0` state is reachable in `work_g`.
        let mut reach_g = work_g.clone();
        reach_g.bads.clear();
        reach_g.justice.clear();
        reach_g.fairness.clear();
        reach_g.bads.push(conds[0]);

        let stem_tr = match run_certified_ic3(&reach_g, ctx, Duration::from_millis(slice_ms)) {
            SafetyOutcome::Safe(_) => return RegionResult::NoFairCycle,
            SafetyOutcome::Unsafe(tr) => tr,
            SafetyOutcome::Unknown => return RegionResult::Unknown,
        };

        // Check immediately if the stem itself already contains a verified lasso (root only).
        if is_root {
            if let Some((v, c, w)) =
                try_extract_counterexample(req, raw_g, &stem_tr.init, &stem_tr.inputs)
            {
                return RegionResult::Lasso(v, c, w);
            }
        }

        let stem_states = simulate_states(work_g, &stem_tr);
        let hit_idx = stem_states.len().saturating_sub(2);
        let s_hit = stem_states[hit_idx].clone();

        // 2. Forward probe from `s_hit` under `work_g.constraints`.
        let (candidate, preferred_cond) = match probe_forward(work_g, &s_hit, conds, 64, ctx) {
            ForwardProbe::FairLoop { loop_start, inputs } => {
                if is_root {
                    let mut full_in =
                        stem_tr.inputs[..stem_tr.inputs.len().saturating_sub(1)].to_vec();
                    let full_start = full_in.len() + loop_start;
                    full_in.extend(inputs);
                    let raw_init = stem_tr.init[..raw_g.latches.len()].to_vec();
                    if let Some((v, c, w)) =
                        verify_lasso(raw_g, orig, prop_idx, &raw_init, &full_in, full_start)
                    {
                        return RegionResult::Lasso(v, c, w);
                    }
                } else {
                    // Extract the loop head state and loop inputs for root reachability check.
                    let mut st = s_hit.clone();
                    for inp in &inputs[..loop_start] {
                        let vals = informal_aig::l2s::eval(work_g, &st, inp);
                        st = work_g
                            .latches
                            .iter()
                            .map(|l| lit_val(&vals, l.next))
                            .collect();
                    }
                    return RegionResult::CandidateCycle {
                        head_state: st,
                        loop_inputs: inputs[loop_start..].to_vec(),
                    };
                }
                (s_hit.clone(), 0)
            }
            ForwardProbe::NonFairCycle {
                state,
                missing_cond_idx,
            } => (state, missing_cond_idx),
            ForwardProbe::Trail { states } => {
                // Pick the midpoint state to bisect long acyclic chains in O(log N)!
                let mid = states.len() / 2;
                (states[mid].clone(), 0)
            }
            ForwardProbe::DeadEnd => (s_hit.clone(), 0),
        };

        // 3. Certified Fairness-Free Trap Peeling (`saw_h` encoding): try
        //    `candidate`, and if `candidate` can leave into a downstream trap
        //    via `h`, peel that downstream trap too.
        let order: Vec<usize> = std::iter::once(preferred_cond)
            .chain((0..conds.len()).filter(|&i| i != preferred_cond))
            .collect();
        let (peeled, after_h) = try_peel_trap(
            work_g,
            &candidate,
            conds,
            &order,
            ctx,
            Duration::from_millis(slice_ms),
        );
        if peeled {
            continue;
        }
        let mut peeled_downstream = false;
        for next_seed in after_h.into_iter().take(2) {
            let (p, _) = try_peel_trap(
                work_g,
                &next_seed,
                conds,
                &order,
                ctx,
                Duration::from_millis(slice_ms),
            );
            if p {
                peeled_downstream = true;
                break;
            }
        }
        if peeled_downstream {
            continue;
        }

        // 4. Certified SCC-Boundary Bisection: test if `candidate` can return to itself.
        let (ret_g, left) =
            build_from_state_aig(work_g, &candidate, |g| state_eq_lit(g, &candidate));
        match run_certified_ic3(&ret_g, ctx, Duration::from_millis(slice_ms)) {
            SafetyOutcome::Safe(inv) => {
                // `P = Inv(·, 1)` is a certified forward-invariant half-space excluding `candidate`!
                let p_clauses = specialize_left(&inv, left, true);
                let u_clauses = specialize_left(&inv, left, false);
                if depth_limit == 0 {
                    return RegionResult::Unknown;
                }
                let mut sub_g = work_g.clone();
                // Free-init inside `work_g.constraints & P`: proving no fair cycle exists
                // anywhere inside `P` makes `P` a sound dead trap for `work_g`!
                for l in &mut sub_g.latches {
                    l.init = Init::Free;
                }
                let p_sub = clauses_to_aig(&mut sub_g, &p_clauses);
                if p_sub == FALSE {
                    // No successor of `candidate` remains in `work_g` -> `U` is dead!
                    let u_lit = clauses_to_aig(work_g, &u_clauses);
                    if u_lit != FALSE {
                        work_g.constraints.push(neg(u_lit));
                        continue;
                    }
                    return RegionResult::Unknown;
                }
                sub_g.constraints.push(p_sub);
                match check_region(
                    req,
                    raw_g,
                    orig,
                    prop_idx,
                    conds,
                    &mut sub_g,
                    false,
                    depth_limit - 1,
                    ctx,
                ) {
                    RegionResult::NoFairCycle => {
                        // Entire forward-invariant half-space `P` and its 1-step basin `U` are dead!
                        let p_lit = clauses_to_aig(work_g, &p_clauses);
                        let u_lit = clauses_to_aig(work_g, &u_clauses);
                        if p_lit != FALSE {
                            work_g.constraints.push(neg(p_lit));
                        }
                        if u_lit != FALSE {
                            work_g.constraints.push(neg(u_lit));
                        }
                        continue;
                    }
                    RegionResult::CandidateCycle {
                        head_state,
                        loop_inputs,
                    } => {
                        if !is_root {
                            return RegionResult::CandidateCycle {
                                head_state,
                                loop_inputs,
                            };
                        }
                        // At root: check whether `head_state` is truly reachable from `Init`!
                        let mut h_reach = work_g.clone();
                        h_reach.bads.clear();
                        h_reach.justice.clear();
                        h_reach.fairness.clear();
                        let eq = state_eq_lit(&mut h_reach, &head_state);
                        h_reach.bads.push(eq);
                        match run_certified_ic3(&h_reach, ctx, Duration::from_millis(slice_ms)) {
                            SafetyOutcome::Safe(inv_unreach) => {
                                // `head_state` is unreachable from `Init`! Strengthen `work_g`
                                // with this certified global inductive invariant and continue.
                                let inv_lit = clauses_to_aig(work_g, &inv_unreach);
                                work_g.constraints.push(inv_lit);
                                continue;
                            }
                            SafetyOutcome::Unsafe(h_tr) => {
                                let raw_init = h_tr.init[..raw_g.latches.len()].to_vec();
                                let mut full_in =
                                    h_tr.inputs[..h_tr.inputs.len().saturating_sub(1)].to_vec();
                                let start = full_in.len();
                                full_in.extend(loop_inputs);
                                if let Some((v, c, w)) =
                                    verify_lasso(raw_g, orig, prop_idx, &raw_init, &full_in, start)
                                {
                                    return RegionResult::Lasso(v, c, w);
                                }
                                return RegionResult::Unknown;
                            }
                            SafetyOutcome::Unknown => return RegionResult::Unknown,
                        }
                    }
                    RegionResult::Lasso(v, c, w) => return RegionResult::Lasso(v, c, w),
                    RegionResult::Unknown => return RegionResult::Unknown,
                }
            }
            SafetyOutcome::Unsafe(ret_tr) => {
                // `candidate` returned to `candidate`! Check if the cycle visits all `conds`.
                let mut loop_in: Vec<Vec<bool>> = ret_tr.inputs
                    [..ret_tr.inputs.len().saturating_sub(1)]
                    .iter()
                    .map(|r| r[..raw_g.inputs.len().min(r.len())].to_vec())
                    .collect();
                if loop_in.is_empty() {
                    loop_in.push(ret_tr.inputs[0][..raw_g.inputs.len()].to_vec());
                }
                let mut st = candidate.clone();
                let mut saw = vec![false; conds.len()];
                for inp in &loop_in {
                    let vals = informal_aig::l2s::eval(work_g, &st, inp);
                    for (ci, &c) in conds.iter().enumerate() {
                        if lit_val(&vals, c) {
                            saw[ci] = true;
                        }
                    }
                    st = work_g
                        .latches
                        .iter()
                        .map(|l| lit_val(&vals, l.next))
                        .collect();
                }
                if saw.iter().all(|&b| b) {
                    if !is_root {
                        return RegionResult::CandidateCycle {
                            head_state: candidate,
                            loop_inputs: loop_in,
                        };
                    }
                    // Verify full lasso from Init -> candidate ->+ candidate.
                    let mut h_reach = work_g.clone();
                    h_reach.bads.clear();
                    h_reach.justice.clear();
                    h_reach.fairness.clear();
                    let eq = state_eq_lit(&mut h_reach, &candidate);
                    h_reach.bads.push(eq);
                    if let SafetyOutcome::Unsafe(h_tr) =
                        run_certified_ic3(&h_reach, ctx, Duration::from_millis(slice_ms))
                    {
                        let raw_init = h_tr.init[..raw_g.latches.len()].to_vec();
                        let mut full_in: Vec<Vec<bool>> = h_tr.inputs
                            [..h_tr.inputs.len().saturating_sub(1)]
                            .iter()
                            .map(|r| r[..raw_g.inputs.len().min(r.len())].to_vec())
                            .collect();
                        let start = full_in.len();
                        full_in.extend(loop_in);
                        if let Some((v, c, w)) =
                            verify_lasso(raw_g, orig, prop_idx, &raw_init, &full_in, start)
                        {
                            return RegionResult::Lasso(v, c, w);
                        }
                    }
                }
                return RegionResult::Unknown;
            }
            SafetyOutcome::Unknown => return RegionResult::Unknown,
        }
    }
}

impl Engine for RLive {
    fn name(&self) -> &str {
        "informal-rlive"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: FORMATS.to_vec(),
            proves: true,
            finds_bugs: true,
            liveness: true,
            domain: EngineDomain::BitLevel,
            kind: EngineKind::Liveness,
            min_effort: Effort::Standard,
            certifies: true,
            ..Default::default()
        }
    }

    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let raw_g = match informal_aig::load_raw(&req.model) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(format!("{e:#}")), t0)),
        };
        if raw_g.justice.is_empty() {
            return Ok(result(
                self.name(),
                unknown("model has no liveness property"),
                t0,
            ));
        }
        if raw_g
            .latches
            .iter()
            .any(|l| matches!(l.init, Init::Expr(_)))
        {
            return Ok(result(
                self.name(),
                unknown("latch inits that are not constants"),
                t0,
            ));
        }
        let orig = match informal_aig::load(&req.model) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(format!("{e:#}")), t0)),
        };

        // First discharge any raw safety `bad` obligations (e.g. `s_until`).
        match prove_raw_safety_bads(req, &raw_g, ctx, t0, self.name()) {
            Some(Err(cex)) => return Ok(cex),
            Some(Ok(())) => {}
            None => return Ok(result(self.name(), unknown(ctx.stop_reason()), t0)),
        }

        for (prop_idx, j_conds) in raw_g.justice.iter().enumerate() {
            let conds: Vec<Lit> = j_conds.iter().chain(&raw_g.fairness).copied().collect();
            let mut work_g = raw_g.clone();
            work_g.bads.clear();
            work_g.justice.clear();
            work_g.fairness.clear();
            match check_region(
                req,
                &raw_g,
                &orig,
                prop_idx,
                &conds,
                &mut work_g,
                true,
                16,
                ctx,
            ) {
                RegionResult::NoFairCycle => continue,
                RegionResult::Lasso(verdict, cert, witness) => {
                    return Ok(EngineResult {
                        engine: self.name().into(),
                        verdict,
                        wall_ms: t0.elapsed().as_millis(),
                        witness,
                        certificate: cert,
                    });
                }
                RegionResult::CandidateCycle { .. } | RegionResult::Unknown => {
                    return Ok(result(self.name(), unknown(ctx.stop_reason()), t0));
                }
            }
        }

        Ok(result(self.name(), Checked::new(Verdict::Safe, None), t0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_engine::ModelFormat;

    fn run_btor2(tag: &str, text: &str) -> EngineResult {
        let dir = std::env::temp_dir().join(format!("informal-rlive-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.btor2");
        std::fs::write(&path, text).unwrap();
        let req = CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(30));
        RLive.check(&req, &RunContext::new(req.timeout)).unwrap()
    }

    #[test]
    fn rlive_proves_multiphase_recurring_liveness_without_k_counter_and_finds_lassos() {
        if informal_sat::Cadical::signature().is_none() {
            return;
        }
        // 1. Repo `liveness.btor2`: Unsafe with verified lasso + witness.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/models");
        let liv = std::fs::read_to_string(root.join("liveness.btor2")).unwrap();
        let r_bad = run_btor2("repo-liv", &liv);
        assert!(matches!(r_bad.verdict, Verdict::Unsafe { .. }), "{r_bad:?}");
        assert!(r_bad.certificate.is_some());
        assert!(r_bad.witness.is_some());

        // 2. Multi-phase recurring liveness (killer for naive k-liveness!):
        //    8-bit counter counts 0..=200 then saturates at 200 forever.
        //    Justice `h = (cnt < 200) && cnt[0]` toggles 0->1->0->1 100 times
        //    before settling to 0 forever! RLive peels cnt==200 and bisects in O(log N).
        let multiphase = "\
1 sort bitvec 8
2 sort bitvec 1
3 zero 1
4 one 1
5 constd 1 200
6 state 1 cnt
7 init 1 6 3
8 ult 2 6 5
9 add 1 6 4
10 ite 1 8 9 6
11 next 1 6 10
12 slice 2 6 0 0
13 and 2 8 12
14 justice 1 13 j_oscillate_100_times
";
        let r_multi = run_btor2("multiphase", multiphase);
        assert_eq!(r_multi.verdict, Verdict::Safe);

        // 3. Recurring fairness + justice model.
        let recurring_safe = "\
1 sort bitvec 1
2 zero 1
3 one 1
4 input 1 en
5 state 1 st
6 init 1 5 3
7 and 1 5 4
8 next 1 5 7
9 not 1 4
10 fair 9 f_drop
11 justice 1 7 j_both
";
        let r_rec = run_btor2("recurring-safe", recurring_safe);
        assert_eq!(r_rec.verdict, Verdict::Safe);

        // 4. Safe wrapping counter where justice target is unreachable.
        let safe_wrap = "\
1 sort bitvec 2
2 sort bitvec 1
3 zero 1
4 state 1 cnt
5 init 1 4 3
6 one 1
7 add 1 4 6
8 constd 1 2
9 eq 2 4 8
10 ite 1 9 3 7
11 next 1 4 10
12 constd 1 3
13 eq 2 4 12
14 justice 1 13 never_three
";
        let r_wrap = run_btor2("safe-wrap", safe_wrap);
        assert_eq!(r_wrap.verdict, Verdict::Safe);

        // 5. 16-bit counter counting 0..=60000 (30,000 oscillations of `h`):
        //    proves in O(W) via monotonic-bit cycle freezing without a k-counter.
        let multiphase_16 = "\
1 sort bitvec 16
2 sort bitvec 1
3 zero 1
4 one 1
5 constd 1 60000
6 state 1 cnt
7 init 1 6 3
8 ult 2 6 5
9 add 1 6 4
10 ite 1 8 9 6
11 next 1 6 10
12 slice 2 6 0 0
13 and 2 8 12
14 justice 1 13 j_oscillate_30k_times
";
        let r_m16 = run_btor2("multiphase-16", multiphase_16);
        assert_eq!(r_m16.verdict, Verdict::Safe);

        // 6. Soundness check: monotonic latch `phase` flips 0 -> 1 on the stem,
        //    then `toggle` oscillates 0 <-> 1 in a real fair loop!
        //    Cycle-only freezing must NOT kill the stem transition 0 -> 1.
        let stem_then_fair_loop = "\
1 sort bitvec 1
2 zero 1
3 one 1
4 state 1 phase
5 init 1 4 2
6 next 1 4 3
7 state 1 toggle
8 init 1 7 2
9 not 1 7
10 ite 1 4 9 2
11 next 1 7 10
12 and 1 4 7
13 and 1 4 9
14 fair 13 f_low
15 justice 1 12 j_high
";
        let r_stem = run_btor2("stem-fair-loop", stem_then_fair_loop);
        assert!(
            matches!(r_stem.verdict, Verdict::Unsafe { .. }),
            "{r_stem:?}"
        );
        assert!(r_stem.certificate.is_some());
        assert!(r_stem.witness.is_some());
    }
}
