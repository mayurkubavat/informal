//! Cycle-invariant monotonic-bit freezing and fast forward probing for `RLive`.
//!
//! Any state literal `b` that can never transition `1 -> 0` (or `0 -> 1`) across
//! any legal step in a constrained graph `C` must remain **constant** (`b' == b`)
//! along every step of every cycle in `C`. Iteratively freezing such latches to
//! a fixed point collapses acyclic counters and one-way phase transitions in
//! `O(W)` single-step SAT queries without adding a `k`-liveness counter or
//! shadow-latch copy.

use informal_aig::{is_neg, lit_val, var, Aig, Init, Lit, Node};
use informal_bmc::cnf::{self, Template};
use informal_bmc::solver;
use informal_bmc::unroll::Unroll;
use informal_engine::RunContext;
use informal_sat::{Answer, Sat};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Maximum latch count for running the bit-monotonicity cycle-freeze pass.
const MAX_FREEZE_LATCHES: usize = 256;

/// Returns `true` when monotonic-bit cycle freezing proves that **no** fair
/// cycle for `conds` can exist anywhere inside `work_g.constraints` (even
/// starting from arbitrary free initial states).
///
/// Soundness: every consecutive triple `(s_k, i_k) -> (s_{k+1}, i_{k+1}) ->
/// (s_{k+2}, i_{k+2})` on any cycle in `work_g` satisfies `work_g.constraints`
/// on all three frames and preserves every previously frozen latch (`l@0 == l@1
/// == l@2`). If `b@0 & !b@1` (or `!b@0 & b@1`) is UNSAT under those clauses, `b`
/// can never change polarity on a cycle, so `b@0 == b@1 == b@2` is a sound
/// cycle-only invariant. Once frozen:
/// 1. If any `conds[j]` is UNSAT at frame 0, no cycle can witness `conds[j]`.
/// 2. If no single constant valuation of the frozen latches can witness every
///    `conds[j]` across `conds.len()` copies of the cycle step, no single cycle
///    can visit all `conds[j]`.
pub(crate) fn prove_no_fair_cycle_by_monotonic_freeze(
    work_g: &Aig,
    conds: &[Lit],
    ctx: &RunContext,
) -> bool {
    let n = work_g.latches.len();
    if n == 0 || n > MAX_FREEZE_LATCHES || conds.is_empty() || ctx.should_stop() {
        return false;
    }

    let mut cycle_g = work_g.clone();
    cycle_g.bads = conds.to_vec();
    cycle_g.justice.clear();
    cycle_g.fairness.clear();
    for l in &mut cycle_g.latches {
        l.init = Init::Free;
    }

    let mut u = Unroll::new(&cycle_g, solver(ctx), true);
    u.add_frame(); // frame 0
    u.add_frame(); // frame 1
    u.add_frame(); // frame 2

    let mut is_frozen = vec![false; n];
    let mut frozen_indices: Vec<usize> = Vec::new();

    // Fixed-point freeze: each frozen MSB often cascades to freeze lower bits.
    loop {
        if ctx.should_stop() {
            return false;
        }
        let mut changed = false;
        for i in (0..n).rev() {
            if is_frozen[i] {
                continue;
            }
            if ctx.should_stop() {
                return false;
            }
            let l = cycle_g.latches[i].lit;
            let l0 = u.lit(0, l);
            let l1 = u.lit(1, l);
            let l2 = u.lit(2, l);
            let mono = matches!(u.sat.solve(&[l0, -l1]), Answer::Unsat)
                || matches!(u.sat.solve(&[-l0, l1]), Answer::Unsat);
            if mono {
                is_frozen[i] = true;
                frozen_indices.push(i);
                // Force l@0 == l@1 == l@2 on every cycle triple.
                u.sat.add_clause(&[-l0, l1]);
                u.sat.add_clause(&[l0, -l1]);
                u.sat.add_clause(&[-l1, l2]);
                u.sat.add_clause(&[l1, -l2]);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    if frozen_indices.is_empty() {
        return false;
    }

    // 1. Check whether any single condition is now unsatisfiable on a cycle edge.
    for &c in conds {
        if ctx.should_stop() {
            return false;
        }
        if matches!(u.sat.solve(&[u.lit(0, c)]), Answer::Unsat) {
            return true;
        }
    }

    // 2. When multiple conditions exist, check whether a single constant
    //    valuation of the frozen latches can witness every `conds[j]`.
    if conds.len() > 1 {
        return !frozen_valuation_covers_all_conds(&cycle_g, conds, &frozen_indices, ctx);
    }
    false
}

/// Instantiates `conds.len()` 3-frame copies of `cycle_g` in one SAT solver,
/// locking all `frozen_indices` to stay constant within each copy AND equal
/// across all copies. Returns `false` iff UNSAT (no cycle can satisfy every
/// condition under a single frozen-latch valuation).
fn frozen_valuation_covers_all_conds(
    cycle_g: &Aig,
    conds: &[Lit],
    frozen_indices: &[usize],
    ctx: &RunContext,
) -> bool {
    let tpl = cnf::template(cycle_g);
    let mut sat = solver(ctx);
    sat.reserve(1);
    sat.add_clause(&[1]);
    let tru = 1i32;
    let mut next_var = 2i32;

    let mut copy0_frozen: Vec<i32> = Vec::with_capacity(frozen_indices.len());

    for (j, &cond) in conds.iter().enumerate() {
        if ctx.should_stop() {
            return true;
        }
        let frames = instantiate_three_frames(cycle_g, &tpl, &mut *sat, tru, &mut next_var);
        // Assert `cond` at frame 0 of copy `j`.
        let c_lit = frame_lit(&frames[0], cond);
        sat.add_clause(&[c_lit]);

        // Freeze each frozen latch across frames 0..=2 and tie to copy 0.
        for (fi_pos, &li) in frozen_indices.iter().enumerate() {
            let l = cycle_g.latches[li].lit;
            let l0 = frame_lit(&frames[0], l);
            let l1 = frame_lit(&frames[1], l);
            let l2 = frame_lit(&frames[2], l);
            sat.add_clause(&[-l0, l1]);
            sat.add_clause(&[l0, -l1]);
            sat.add_clause(&[-l1, l2]);
            sat.add_clause(&[l1, -l2]);
            if j == 0 {
                copy0_frozen.push(l0);
            } else {
                let ref_l = copy0_frozen[fi_pos];
                sat.add_clause(&[-ref_l, l0]);
                sat.add_clause(&[ref_l, -l0]);
            }
        }
    }

    !matches!(sat.solve(&[]), Answer::Unsat)
}

fn frame_lit(frame: &[i32], l: Lit) -> i32 {
    let x = frame[var(l) as usize];
    if is_neg(l) {
        -x
    } else {
        x
    }
}

fn instantiate_three_frames(
    g: &Aig,
    tpl: &Template,
    sat: &mut dyn Sat,
    tru: i32,
    next_var: &mut i32,
) -> [Vec<i32>; 3] {
    let live = tpl.live.iter().filter(|&&x| x).count() as i32;
    let mut frames: Vec<Vec<i32>> = Vec::with_capacity(3);
    let mut buf = Vec::new();
    for k in 0..3 {
        sat.reserve(live);
        let mut f = vec![0i32; g.nodes.len()];
        let mut li = 0usize;
        for (v, n) in g.nodes.iter().enumerate() {
            f[v] = match *n {
                Node::Const => -tru,
                Node::Input => {
                    let x = *next_var;
                    *next_var += 1;
                    x
                }
                Node::Latch => {
                    let latch = &g.latches[li];
                    li += 1;
                    if k > 0 {
                        frame_lit(&frames[k - 1], latch.next)
                    } else {
                        let x = *next_var;
                        *next_var += 1;
                        x
                    }
                }
                Node::And(..) if tpl.live[v + 1] => {
                    let x = *next_var;
                    *next_var += 1;
                    x
                }
                Node::And(..) => 0,
            };
        }
        for c in &tpl.clauses {
            buf.clear();
            buf.extend(c.iter().map(|&t| {
                let x = f[t.unsigned_abs() as usize - 1];
                if t < 0 {
                    -x
                } else {
                    x
                }
            }));
            sat.add_clause(&buf);
        }
        for &c in &g.constraints {
            sat.add_clause(&[frame_lit(&f, c)]);
        }
        frames.push(f);
    }
    let mut it = frames.into_iter();
    [it.next().unwrap(), it.next().unwrap(), it.next().unwrap()]
}

/// Forward probe result from a reachable state under `work_g.constraints`.
pub(crate) enum ForwardProbe {
    FairLoop {
        loop_start: usize,
        inputs: Vec<Vec<bool>>,
    },
    NonFairCycle {
        state: Vec<bool>,
        missing_cond_idx: usize,
    },
    Trail {
        states: Vec<Vec<bool>>,
    },
    DeadEnd,
}

/// Fast concrete simulation (all-false inputs) followed by bounded SAT unroll
/// fallback when constraints reject default inputs.
pub(crate) fn probe_forward(
    base: &Aig,
    start_state: &[bool],
    conds: &[Lit],
    max_steps: usize,
    ctx: &RunContext,
) -> ForwardProbe {
    if let Some(p) = probe_concrete(base, start_state, conds, max_steps.max(256)) {
        return p;
    }

    let mut g = base.clone();
    g.bads.clear();
    g.justice.clear();
    g.fairness.clear();
    for (l, &b) in g.latches.iter_mut().zip(start_state) {
        l.init = if b { Init::One } else { Init::Zero };
    }
    let mut pctx = ctx.clone();
    pctx.deadline = ctx
        .deadline
        .min(Instant::now() + Duration::from_millis(150));
    let mut u = Unroll::new(&g, solver(&pctx), false);
    let mut last_sat_depth: Option<usize> = None;
    let mut last_inputs: Vec<Vec<bool>> = Vec::new();
    let mut check_at = 1usize;

    for f in 0..=max_steps {
        if pctx.should_stop() {
            break;
        }
        u.add_frame();
        if f == check_at || f == max_steps {
            match u.sat.solve(&[]) {
                Answer::Sat => {
                    last_sat_depth = Some(f);
                    last_inputs = (0..=f)
                        .map(|t| g.inputs.iter().map(|&i| u.sat.value(u.lit(t, i))).collect())
                        .collect();
                }
                Answer::Unsat => {
                    if f == 1 && last_sat_depth.is_none() {
                        return ForwardProbe::DeadEnd;
                    }
                    break;
                }
                Answer::Unknown => break,
            }
            check_at = (check_at * 2).min(max_steps);
        }
    }

    let Some(d) = last_sat_depth else {
        return ForwardProbe::DeadEnd;
    };
    classify_trace(base, start_state, conds, &last_inputs[..=d])
}

fn probe_concrete(
    base: &Aig,
    start_state: &[bool],
    conds: &[Lit],
    steps: usize,
) -> Option<ForwardProbe> {
    let inp = vec![false; base.inputs.len()];
    let mut state = start_state.to_vec();
    let mut inputs: Vec<Vec<bool>> = Vec::with_capacity(steps);
    let mut seen_at: HashMap<Vec<bool>, usize> = HashMap::new();
    let mut cond_vals: Vec<Vec<bool>> = Vec::with_capacity(steps);

    for t in 0..steps {
        if let Some(&prev) = seen_at.get(&state) {
            return Some(finish_cycle(prev, t, &cond_vals, &inputs, state));
        }
        let vals = informal_aig::l2s::eval(base, &state, &inp);
        if base.constraints.iter().any(|&c| !lit_val(&vals, c)) {
            return None;
        }
        seen_at.insert(state.clone(), t);
        inputs.push(inp.clone());
        cond_vals.push(conds.iter().map(|&c| lit_val(&vals, c)).collect());
        state = base
            .latches
            .iter()
            .map(|l| lit_val(&vals, l.next))
            .collect();
    }
    if let Some(&prev) = seen_at.get(&state) {
        return Some(finish_cycle(prev, steps, &cond_vals, &inputs, state));
    }
    Some(classify_trace(base, start_state, conds, &inputs))
}

fn classify_trace(
    base: &Aig,
    start_state: &[bool],
    conds: &[Lit],
    inputs: &[Vec<bool>],
) -> ForwardProbe {
    let mut state = start_state.to_vec();
    let mut states = Vec::with_capacity(inputs.len() + 1);
    let mut cond_vals: Vec<Vec<bool>> = Vec::with_capacity(inputs.len());
    let mut seen_at: HashMap<Vec<bool>, usize> = HashMap::new();

    for (t, inp) in inputs.iter().enumerate() {
        if let Some(&prev) = seen_at.get(&state) {
            return finish_cycle(prev, t, &cond_vals, inputs, state);
        }
        seen_at.insert(state.clone(), t);
        states.push(state.clone());
        let vals = informal_aig::l2s::eval(base, &state, inp);
        cond_vals.push(conds.iter().map(|&c| lit_val(&vals, c)).collect());
        state = base
            .latches
            .iter()
            .map(|l| lit_val(&vals, l.next))
            .collect();
    }
    states.push(state);
    ForwardProbe::Trail { states }
}

fn finish_cycle(
    prev: usize,
    t: usize,
    cond_vals: &[Vec<bool>],
    inputs: &[Vec<bool>],
    state: Vec<bool>,
) -> ForwardProbe {
    let n_conds = cond_vals.first().map_or(0, Vec::len);
    if let Some(ci) = (0..n_conds).find(|&ci| !(prev..t).any(|s| cond_vals[s][ci])) {
        return ForwardProbe::NonFairCycle {
            state,
            missing_cond_idx: ci,
        };
    }
    ForwardProbe::FairLoop {
        loop_start: prev,
        inputs: inputs[..t].to_vec(),
    }
}
