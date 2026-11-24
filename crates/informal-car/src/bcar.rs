//! Backward Complementary Approximate Reachability (`informal-bcar`).
//!
//! Maintains a persistent under-approximate `U`-sequence of concrete **forward
//! reachable** states from `Init` alongside monotone backward over-approximate
//! `O`-frames (`O_0 = Bad`, `O_i` over-approximates states that can reach `Bad`
//! in `<= i` steps, stored as blocked safe cubes `B_i` that CANNOT reach `Bad`
//! in `<= i` steps). Excels at deep forward bug hunting and backward proofs.

use crate::fcar::{format_named_witness, CarOutcome};
use informal_aig::{is_neg, neg, var, Aig, Init, Lit};
use informal_bmc::unroll::Unroll;
use informal_bmc::{ic3, solver, PreparedTrace};
use informal_engine::{Progress, RunContext};
use informal_sat::{Answer, Sat};

const MAX_CERT_CUBES: usize = 4_096;

/// Concrete state guaranteed to be reachable from `Init` along a `C`-legal path.
struct RState {
    state: Vec<bool>,
    /// Parent `(parent_idx, input_from_parent_to_this)`, or `None` for initial states.
    parent: Option<(usize, Vec<bool>)>,
}

/// Run Backward CAR on prepared AIG `g`.
pub fn run_bcar(g: &Aig, max_depth: Option<usize>, ctx: &RunContext) -> CarOutcome {
    if g.latches.iter().any(|l| matches!(l.init, Init::Expr(_))) {
        return CarOutcome::Aborted("latch inits that are not constants".into());
    }

    // 0. Depth-0 base check: `Init & C & Bad`.
    let mut base = Unroll::new(g, solver(ctx), false);
    base.add_frame();
    let bad0 = base.bad(0);
    match base.sat.solve(&[bad0]) {
        Answer::Sat => {
            let trace = PreparedTrace::from_unroll(g, &base, 0);
            let witness = format_named_witness(g, &trace.init, &trace.inputs);
            return CarOutcome::Unsafe {
                depth: 0,
                trace,
                witness,
            };
        }
        Answer::Unknown => return CarOutcome::Aborted(ctx.stop_reason().into()),
        Answer::Unsat => {
            if g.latches.is_empty() {
                return CarOutcome::Safe(Vec::new());
            }
        }
    }

    // 1. Build 1-frame `bad_u` (C & Bad at frame 0), 1-frame `init_u` (Init & C at frame 0),
    //    and 2-frame `step_u` (`s@0 -> s'@1` with `C@0 & C@1`).
    let mut bad_u = Unroll::new(g, solver(ctx), true);
    bad_u.add_frame();
    let b0 = bad_u.bad(0);
    bad_u.sat.add_clause(&[b0]);

    let mut init_u = Unroll::new(g, solver(ctx), false);
    init_u.add_frame();

    let mut step_u = Unroll::new(g, solver(ctx), true);
    step_u.add_frame();
    step_u.add_frame();

    // `act_step[0]` forces `Bad@1` on frame 1 of `step_u` (so `O_0 = Bad`).
    let act0_step = step_u.new_var();
    let bad1 = step_u.bad(1);
    step_u.sat.add_clause(&[-act0_step, bad1]);

    // `frames[lvl]` holds cubes `c` that CANNOT reach `Bad` in `<= lvl` steps
    // (so `O_lvl = ∧_{j >= lvl} !c` over-approximates states reaching `Bad` in `<= lvl` steps).
    let mut act_step = vec![act0_step];
    let mut act_init = vec![init_u.new_var()];
    let mut frames: Vec<Vec<ic3::Cube>> = vec![Vec::new()];
    let mut r_states: Vec<RState> = Vec::new();
    let mut k = 0usize;

    loop {
        if ctx.should_stop() {
            return CarOutcome::Aborted(ctx.stop_reason().into());
        }
        if max_depth.is_some_and(|m| k >= m) {
            return CarOutcome::Aborted(format!("bound {k} reached"));
        }

        k += 1;
        act_step.push(step_u.new_var());
        act_init.push(init_u.new_var());
        frames.push(Vec::new());

        ctx.report(Progress {
            engine: "informal-bcar".into(),
            bound: Some(k as u32),
            note: format!("bcar frame {k}, |U|={}", r_states.len()),
            output: None,
        });

        // A. Attack existing persistent forward `U`-states from deepest/newest first!
        let existing_r = r_states.len();
        for rid in (0..existing_r).rev() {
            if ctx.should_stop() {
                return CarOutcome::Aborted(ctx.stop_reason().into());
            }
            if is_blocked_at(g, &frames, k, &r_states[rid].state) {
                continue;
            }
            if let Some(res) = block_forward_dfs(
                g,
                &mut bad_u,
                &mut step_u,
                &mut init_u,
                &act_step,
                &act_init,
                &mut frames,
                &mut r_states,
                rid,
                k,
                ctx,
            ) {
                return res;
            }
        }

        // B. Block every remaining initial state not yet excluded from `O_k`.
        loop {
            if ctx.should_stop() {
                return CarOutcome::Aborted(ctx.stop_reason().into());
            }
            let assume_init: Vec<i32> = act_init[k..=k].to_vec();
            match init_u.sat.solve(&assume_init) {
                Answer::Unsat => break,
                Answer::Unknown => return CarOutcome::Aborted(ctx.stop_reason().into()),
                Answer::Sat => {
                    let st: Vec<bool> = g
                        .latches
                        .iter()
                        .map(|l| init_u.sat.value(init_u.lit(0, l.lit)))
                        .collect();
                    let rid = r_states.len();
                    r_states.push(RState {
                        state: st,
                        parent: None,
                    });
                    if let Some(res) = block_forward_dfs(
                        g,
                        &mut bad_u,
                        &mut step_u,
                        &mut init_u,
                        &act_step,
                        &act_init,
                        &mut frames,
                        &mut r_states,
                        rid,
                        k,
                        ctx,
                    ) {
                        return res;
                    }
                }
            }
        }

        // C. Push safe cubes forward and check backward inductive fixpoint.
        for lvl in 1..k {
            if ctx.should_stop() {
                return CarOutcome::Aborted(ctx.stop_reason().into());
            }
            let mut keep = Vec::new();
            let cubes = std::mem::take(&mut frames[lvl]);
            for cube in cubes {
                if can_push_bwd(&mut step_u, &act_step, lvl, &cube) {
                    add_safe_cube(
                        &mut step_u,
                        &mut init_u,
                        &act_step,
                        &act_init,
                        lvl + 1,
                        &cube,
                    );
                    frames[lvl + 1].push(cube);
                } else {
                    keep.push(cube);
                }
            }
            frames[lvl] = keep;
            if frames[lvl].is_empty() {
                // Backward fixpoint: `Inv = ∨_{c ∈ safe_cubes} c` is a global inductive invariant!
                let mut safe_cubes = Vec::new();
                for f in &frames[(lvl + 1)..=k] {
                    safe_cubes.extend(f.iter().cloned());
                }
                return match dnf_to_blocked_cubes(g, &safe_cubes, ctx) {
                    Ok(lemmas) => CarOutcome::Safe(lemmas),
                    Err(r) => CarOutcome::Aborted(r),
                };
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn block_forward_dfs(
    g: &Aig,
    bad_u: &mut Unroll,
    step_u: &mut Unroll,
    init_u: &mut Unroll,
    act_step: &[i32],
    act_init: &[i32],
    frames: &mut [Vec<ic3::Cube>],
    r_states: &mut Vec<RState>,
    start_rid: usize,
    start_lvl: usize,
    ctx: &RunContext,
) -> Option<CarOutcome> {
    let mut stack = vec![(start_rid, start_lvl)];
    while let Some((rid, lvl)) = stack.pop() {
        if ctx.should_stop() {
            return Some(CarOutcome::Aborted(ctx.stop_reason().into()));
        }
        if is_blocked_at(g, frames, lvl, &r_states[rid].state) {
            continue;
        }
        let mut assume = frame_assumptions(act_step, lvl - 1);
        let state_lits: Vec<(Lit, i32)> = g
            .latches
            .iter()
            .zip(&r_states[rid].state)
            .map(|(l, &val)| {
                let al = if val { l.lit } else { neg(l.lit) };
                (al, step_u.lit(0, al))
            })
            .collect();
        for &(_, sl) in &state_lits {
            assume.push(sl);
        }

        match step_u.sat.solve(&assume) {
            Answer::Unknown => return Some(CarOutcome::Aborted(ctx.stop_reason().into())),
            Answer::Sat => {
                let succ: Vec<bool> = g
                    .latches
                    .iter()
                    .map(|l| step_u.sat.value(step_u.lit(1, l.lit)))
                    .collect();
                let inp0: Vec<bool> = g
                    .inputs
                    .iter()
                    .map(|&i| step_u.sat.value(step_u.lit(0, i)))
                    .collect();
                let new_rid = r_states.len();
                r_states.push(RState {
                    state: succ.clone(),
                    parent: Some((rid, inp0)),
                });
                if lvl == 1 {
                    let inp_bad: Vec<bool> = g
                        .inputs
                        .iter()
                        .map(|&i| step_u.sat.value(step_u.lit(1, i)))
                        .collect();
                    return Some(reconstruct_forward_cex(g, r_states, new_rid, inp_bad));
                }
                // Check if `succ` itself already satisfies `C & Bad` (early DFS bug hit!).
                let bad_ass: Vec<i32> = g
                    .latches
                    .iter()
                    .zip(&succ)
                    .map(|(l, &v)| {
                        let al = if v { l.lit } else { neg(l.lit) };
                        bad_u.lit(0, al)
                    })
                    .collect();
                if matches!(bad_u.sat.solve(&bad_ass), Answer::Sat) {
                    let inp_bad: Vec<bool> = g
                        .inputs
                        .iter()
                        .map(|&i| bad_u.sat.value(bad_u.lit(0, i)))
                        .collect();
                    return Some(reconstruct_forward_cex(g, r_states, new_rid, inp_bad));
                }
                stack.push((rid, lvl));
                stack.push((new_rid, lvl - 1));
            }
            Answer::Unsat => {
                let mut cube: ic3::Cube = state_lits
                    .iter()
                    .filter(|&&(_, sl)| step_u.sat.failed(sl))
                    .map(|&(al, _)| al)
                    .collect();
                ensure_disjoint_from_bad(g, bad_u, &mut cube, &r_states[rid].state);
                generalize_safe_cube(bad_u, step_u, act_step, lvl - 1, &mut cube, ctx);
                cube.sort_unstable();
                cube.dedup();
                add_safe_cube(step_u, init_u, act_step, act_init, lvl, &cube);
                frames[lvl].push(cube);
            }
        }
    }
    None
}

fn frame_assumptions(act_step: &[i32], lvl: usize) -> Vec<i32> {
    if lvl == 0 {
        vec![act_step[0]]
    } else {
        act_step[lvl..].to_vec()
    }
}

fn is_blocked_at(g: &Aig, frames: &[Vec<ic3::Cube>], lvl: usize, state: &[bool]) -> bool {
    let mut latch_idx = std::collections::HashMap::with_capacity(g.latches.len());
    for (i, l) in g.latches.iter().enumerate() {
        latch_idx.insert(var(l.lit), i);
    }
    for f in &frames[lvl..] {
        for cube in f {
            if cube.iter().all(|&al| {
                let i = latch_idx[&var(al)];
                state[i] ^ is_neg(al)
            }) {
                return true;
            }
        }
    }
    false
}

fn ensure_disjoint_from_bad(g: &Aig, bad_u: &mut Unroll, cube: &mut ic3::Cube, state: &[bool]) {
    loop {
        let assume: Vec<i32> = cube.iter().map(|&al| bad_u.lit(0, al)).collect();
        if !matches!(bad_u.sat.solve(&assume), Answer::Sat) {
            return;
        }
        let mut added = false;
        for (l, &val) in g.latches.iter().zip(state) {
            let bv = bad_u.sat.value(bad_u.lit(0, l.lit));
            if bv != val {
                let al = if val { l.lit } else { neg(l.lit) };
                if !cube.contains(&al) {
                    cube.push(al);
                    added = true;
                    break;
                }
            }
        }
        if !added {
            return;
        }
    }
}

fn generalize_safe_cube(
    bad_u: &mut Unroll,
    step_u: &mut Unroll,
    act_step: &[i32],
    prev_lvl: usize,
    cube: &mut ic3::Cube,
    ctx: &RunContext,
) {
    let mut i = 0;
    while i < cube.len() {
        if ctx.should_stop() {
            return;
        }
        let saved = cube.remove(i);
        let bad_ass: Vec<i32> = cube.iter().map(|&al| bad_u.lit(0, al)).collect();
        let bad_ok = matches!(bad_u.sat.solve(&bad_ass), Answer::Unsat);
        let step_ok = if bad_ok {
            let mut step_ass = frame_assumptions(act_step, prev_lvl);
            for &al in cube.iter() {
                step_ass.push(step_u.lit(0, al));
            }
            matches!(step_u.sat.solve(&step_ass), Answer::Unsat)
        } else {
            false
        };
        if !step_ok {
            cube.insert(i, saved);
            i += 1;
        }
    }
}

fn can_push_bwd(step_u: &mut Unroll, act_step: &[i32], lvl: usize, cube: &ic3::Cube) -> bool {
    let mut ass = frame_assumptions(act_step, lvl);
    for &al in cube {
        ass.push(step_u.lit(0, al));
    }
    matches!(step_u.sat.solve(&ass), Answer::Unsat)
}

fn add_safe_cube(
    step_u: &mut Unroll,
    init_u: &mut Unroll,
    act_step: &[i32],
    act_init: &[i32],
    lvl: usize,
    cube: &ic3::Cube,
) {
    // Exclude `cube` from frame 1 of `step_u` (`O_lvl` = states NOT in any safe cube).
    let mut cs = Vec::with_capacity(cube.len() + 1);
    cs.push(-act_step[lvl]);
    for &al in cube {
        cs.push(-step_u.lit(1, al));
    }
    step_u.sat.add_clause(&cs);

    // Exclude `cube` from frame 0 of `init_u`.
    let mut ci = Vec::with_capacity(cube.len() + 1);
    ci.push(-act_init[lvl]);
    for &al in cube {
        ci.push(-init_u.lit(0, al));
    }
    init_u.sat.add_clause(&ci);
}

fn reconstruct_forward_cex(
    g: &Aig,
    r_states: &[RState],
    end_rid: usize,
    end_inp: Vec<bool>,
) -> CarOutcome {
    let mut rev_inputs = vec![end_inp];
    let mut cur = end_rid;
    while let Some((p, ref inp)) = r_states[cur].parent {
        rev_inputs.push(inp.clone());
        cur = p;
    }
    let init = r_states[cur].state.clone();
    rev_inputs.reverse();
    let depth = rev_inputs.len().saturating_sub(1);
    let witness = format_named_witness(g, &init, &rev_inputs);
    CarOutcome::Unsafe {
        depth,
        trace: PreparedTrace {
            init,
            inputs: rev_inputs,
        },
        witness,
    }
}

/// Convert a backward-CAR DNF invariant `Inv = ∨_{c ∈ safe_cubes} c` into
/// minimal CNF blocked cubes (`∧ !b == Inv`) for `certify_lemmas`.
fn dnf_to_blocked_cubes(
    g: &Aig,
    safe_cubes: &[ic3::Cube],
    ctx: &RunContext,
) -> Result<Vec<ic3::Cube>, String> {
    if safe_cubes.iter().any(Vec::is_empty) {
        return Ok(Vec::new()); // `TRUE` is a disjunct -> invariant is `TRUE`
    }
    if safe_cubes.is_empty() {
        return Ok(vec![Vec::new()]); // empty disjunction -> `FALSE`
    }
    let n = g.latches.len();
    let mut latch_idx = std::collections::HashMap::with_capacity(n);
    for (i, l) in g.latches.iter().enumerate() {
        latch_idx.insert(var(l.lit), i);
    }

    // `enum_sat` models `!Inv = ∧_{c ∈ safe_cubes} !c`.
    let mut enum_sat = solver(ctx);
    enum_sat.reserve(n as i32);
    for c in safe_cubes {
        let cl: Vec<i32> = c
            .iter()
            .map(|&al| {
                let v = latch_idx[&var(al)] as i32 + 1;
                if is_neg(al) {
                    v
                } else {
                    -v
                }
            })
            .collect();
        enum_sat.add_clause(&cl);
    }

    // `gen_sat` models `Inv = ∨_{c ∈ safe_cubes} c` using selector variables `sel[j]`.
    let mut gen_sat = solver(ctx);
    gen_sat.reserve((n + safe_cubes.len()) as i32);
    let mut sel_lits = Vec::with_capacity(safe_cubes.len());
    for (j, c) in safe_cubes.iter().enumerate() {
        let sel = (n + 1 + j) as i32;
        sel_lits.push(sel);
        for &al in c {
            let v = latch_idx[&var(al)] as i32 + 1;
            let lit = if is_neg(al) { -v } else { v };
            gen_sat.add_clause(&[-sel, lit]);
        }
    }
    gen_sat.add_clause(&sel_lits);

    extract_cubes_from_pair(g, &mut *enum_sat, &mut *gen_sat, ctx)
}

fn extract_cubes_from_pair(
    g: &Aig,
    enum_sat: &mut dyn Sat,
    gen_sat: &mut dyn Sat,
    ctx: &RunContext,
) -> Result<Vec<ic3::Cube>, String> {
    let n = g.latches.len();
    let mut lemmas = Vec::new();
    loop {
        if ctx.should_stop() {
            return Err(ctx.stop_reason().into());
        }
        if lemmas.len() >= MAX_CERT_CUBES {
            return Err("bcar invariant cube limit reached".into());
        }
        match enum_sat.solve(&[]) {
            Answer::Unsat => return Ok(lemmas),
            Answer::Unknown => return Err(ctx.stop_reason().into()),
            Answer::Sat => {
                let mut lits: Vec<(usize, bool)> = (0..n)
                    .map(|i| (i, enum_sat.value((i + 1) as i32)))
                    .collect();
                let assume: Vec<i32> = lits
                    .iter()
                    .map(|&(i, v)| if v { (i + 1) as i32 } else { -((i + 1) as i32) })
                    .collect();
                if matches!(gen_sat.solve(&assume), Answer::Unsat) {
                    lits.retain(|&(i, v)| {
                        let sl = if v { (i + 1) as i32 } else { -((i + 1) as i32) };
                        gen_sat.failed(sl)
                    });
                }
                let mut idx = 0;
                while idx < lits.len() {
                    if ctx.should_stop() {
                        return Err(ctx.stop_reason().into());
                    }
                    let trial: Vec<i32> = lits
                        .iter()
                        .enumerate()
                        .filter(|&(j, _)| j != idx)
                        .map(|(_, &(i, v))| if v { (i + 1) as i32 } else { -((i + 1) as i32) })
                        .collect();
                    if matches!(gen_sat.solve(&trial), Answer::Unsat) {
                        lits.remove(idx);
                    } else {
                        idx += 1;
                    }
                }
                let block: Vec<i32> = lits
                    .iter()
                    .map(|&(i, v)| if v { -((i + 1) as i32) } else { (i + 1) as i32 })
                    .collect();
                enum_sat.add_clause(&block);

                let mut cube: ic3::Cube = lits
                    .into_iter()
                    .map(|(i, v)| {
                        let l = g.latches[i].lit;
                        if v {
                            l
                        } else {
                            neg(l)
                        }
                    })
                    .collect();
                cube.sort_unstable();
                lemmas.push(cube);
            }
        }
    }
}
