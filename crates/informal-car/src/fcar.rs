//! Forward Complementary Approximate Reachability (`informal-car`).
//!
//! Maintains a persistent under-approximate `U`-sequence of concrete states
//! guaranteed to reach `Bad` alongside monotone over-approximate `O`-frames
//! (`O_0 = Init`, `O_1 ⊆ ... ⊆ O_k`). At each frame `k`, CAR attacks the
//! deepest states in `U` first (DFS-like bug finding without discarding
//! obligations across frames), generalizes UNSAT-core blocked cubes via
//! literal dropping, and returns an exact CNF inductive invariant upon
//! frame convergence for independent certification via `certify_lemmas`.

use informal_aig::{is_neg, neg, var, Aig, Init, Lit};
use informal_bmc::unroll::Unroll;
use informal_bmc::{ic3, solver, PreparedTrace};
use informal_engine::{Progress, RunContext};
use informal_sat::Answer;

/// Outcome of a CAR run on a prepared AIG.
pub enum CarOutcome {
    Safe(Vec<ic3::Cube>),
    Unsafe {
        depth: usize,
        trace: PreparedTrace,
        witness: String,
    },
    Aborted(String),
}

/// Concrete state known to reach `Bad` along a `C`-legal path.
struct UState {
    state: Vec<bool>,
    /// Next `UState` toward `Bad`, or `None` if `state` itself is bad under `inputs`.
    next: Option<usize>,
    inputs: Vec<bool>,
}

/// Run Forward CAR on prepared AIG `g`.
pub fn run_fcar(g: &Aig, max_depth: Option<usize>, ctx: &RunContext) -> CarOutcome {
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

    // 1. Build shared 1-frame `init_sat`, 1-frame `bad_sat`, and 2-frame `step_sat`.
    let mut init_u = Unroll::new(g, solver(ctx), false);
    init_u.add_frame();

    let mut bad_u = Unroll::new(g, solver(ctx), true);
    bad_u.add_frame();
    let bad_lit = bad_u.bad(0);
    bad_u.sat.add_clause(&[bad_lit]);

    let mut step_u = Unroll::new(g, solver(ctx), true);
    step_u.add_frame();
    step_u.add_frame();

    // `act_step[0]` activates `Init` on frame 0 of `step_u`.
    let act0_step = step_u.new_var();
    for l in &g.latches {
        let sl = step_u.lit(0, l.lit);
        match l.init {
            Init::Zero => step_u.sat.add_clause(&[-act0_step, -sl]),
            Init::One => step_u.sat.add_clause(&[-act0_step, sl]),
            Init::Free | Init::Expr(_) => {}
        }
    }

    let mut act_step = vec![act0_step];
    let mut act_bad = vec![bad_u.new_var()];
    // `frames[lvl]` holds cubes whose highest confirmed blocked level is `lvl`.
    let mut frames: Vec<Vec<ic3::Cube>> = vec![Vec::new()];
    let mut u_states: Vec<UState> = Vec::new();
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
        act_bad.push(bad_u.new_var());
        frames.push(Vec::new());

        ctx.report(Progress {
            engine: "informal-car".into(),
            bound: Some(k as u32),
            note: format!("fcar frame {k}, |U|={}", u_states.len()),
            output: None,
        });

        // A. Attack existing persistent `U`-states from deepest/newest first!
        let existing_u = u_states.len();
        for uid in (0..existing_u).rev() {
            if ctx.should_stop() {
                return CarOutcome::Aborted(ctx.stop_reason().into());
            }
            if is_blocked_at(g, &frames, k, &u_states[uid].state) {
                continue;
            }
            if let Some(res) = block_dfs(
                g,
                &mut init_u,
                &mut step_u,
                &mut bad_u,
                &act_step,
                &act_bad,
                &mut frames,
                &mut u_states,
                uid,
                k,
                ctx,
            ) {
                return res;
            }
        }

        // B. Block any remaining `O_k & C & Bad` counterexamples-to-induction.
        loop {
            if ctx.should_stop() {
                return CarOutcome::Aborted(ctx.stop_reason().into());
            }
            let assume_bad: Vec<i32> = act_bad[k..=k].to_vec();
            match bad_u.sat.solve(&assume_bad) {
                Answer::Unsat => break,
                Answer::Unknown => return CarOutcome::Aborted(ctx.stop_reason().into()),
                Answer::Sat => {
                    let st: Vec<bool> = g
                        .latches
                        .iter()
                        .map(|l| bad_u.sat.value(bad_u.lit(0, l.lit)))
                        .collect();
                    let inp: Vec<bool> = g
                        .inputs
                        .iter()
                        .map(|&i| bad_u.sat.value(bad_u.lit(0, i)))
                        .collect();
                    let uid = u_states.len();
                    u_states.push(UState {
                        state: st,
                        next: None,
                        inputs: inp,
                    });
                    if let Some(res) = block_dfs(
                        g,
                        &mut init_u,
                        &mut step_u,
                        &mut bad_u,
                        &act_step,
                        &act_bad,
                        &mut frames,
                        &mut u_states,
                        uid,
                        k,
                        ctx,
                    ) {
                        return res;
                    }
                }
            }
        }

        // C. Push blocked cubes forward and check exact inductive fixpoint (`frames[lvl].is_empty()`).
        for lvl in 1..k {
            if ctx.should_stop() {
                return CarOutcome::Aborted(ctx.stop_reason().into());
            }
            let mut keep = Vec::new();
            let cubes = std::mem::take(&mut frames[lvl]);
            for cube in cubes {
                if can_push(&mut step_u, &act_step, lvl, &cube) {
                    add_cube_to_solvers(
                        &mut step_u,
                        &mut bad_u,
                        &act_step,
                        &act_bad,
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
                let mut inv = Vec::new();
                for f in &frames[(lvl + 1)..=k] {
                    inv.extend(f.iter().cloned());
                }
                return CarOutcome::Safe(inv);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn block_dfs(
    g: &Aig,
    init_u: &mut Unroll,
    step_u: &mut Unroll,
    bad_u: &mut Unroll,
    act_step: &[i32],
    act_bad: &[i32],
    frames: &mut [Vec<ic3::Cube>],
    u_states: &mut Vec<UState>,
    start_uid: usize,
    start_lvl: usize,
    ctx: &RunContext,
) -> Option<CarOutcome> {
    let mut stack = vec![(start_uid, start_lvl)];
    while let Some((uid, lvl)) = stack.pop() {
        if ctx.should_stop() {
            return Some(CarOutcome::Aborted(ctx.stop_reason().into()));
        }
        if is_blocked_at(g, frames, lvl, &u_states[uid].state) {
            continue;
        }
        let mut assume = frame_assumptions(act_step, lvl - 1);
        let state_lits: Vec<(Lit, i32)> = g
            .latches
            .iter()
            .zip(&u_states[uid].state)
            .map(|(l, &val)| {
                let al = if val { l.lit } else { neg(l.lit) };
                (al, step_u.lit(1, al))
            })
            .collect();
        for &(_, sl) in &state_lits {
            assume.push(sl);
        }

        match step_u.sat.solve(&assume) {
            Answer::Unknown => return Some(CarOutcome::Aborted(ctx.stop_reason().into())),
            Answer::Sat => {
                let pred: Vec<bool> = g
                    .latches
                    .iter()
                    .map(|l| step_u.sat.value(step_u.lit(0, l.lit)))
                    .collect();
                let inp0: Vec<bool> = g
                    .inputs
                    .iter()
                    .map(|&i| step_u.sat.value(step_u.lit(0, i)))
                    .collect();
                let is_init = lvl == 1
                    || g.latches.iter().zip(&pred).all(|(l, &v)| match l.init {
                        Init::Zero => !v,
                        Init::One => v,
                        Init::Free | Init::Expr(_) => true,
                    });
                let new_uid = u_states.len();
                u_states.push(UState {
                    state: pred,
                    next: Some(uid),
                    inputs: inp0,
                });
                if is_init {
                    return Some(reconstruct_cex(g, u_states, new_uid));
                }
                stack.push((uid, lvl));
                stack.push((new_uid, lvl - 1));
            }
            Answer::Unsat => {
                // UNSAT core over frame-1 state literals.
                let mut cube: ic3::Cube = state_lits
                    .iter()
                    .filter(|&&(_, sl)| step_u.sat.failed(sl))
                    .map(|&(al, _)| al)
                    .collect();
                // Ensure `Init & C & cube` is UNSAT so monotone `O_0 ⊆ O_lvl` holds.
                ensure_disjoint_from_init(g, init_u, &mut cube, &u_states[uid].state);
                // Greedy MIC literal dropping.
                generalize_cube(init_u, step_u, act_step, lvl - 1, &mut cube, ctx);
                cube.sort_unstable();
                cube.dedup();
                add_cube_to_solvers(step_u, bad_u, act_step, act_bad, lvl, &cube);
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

fn ensure_disjoint_from_init(g: &Aig, init_u: &mut Unroll, cube: &mut ic3::Cube, state: &[bool]) {
    loop {
        let assume: Vec<i32> = cube.iter().map(|&al| init_u.lit(0, al)).collect();
        if !matches!(init_u.sat.solve(&assume), Answer::Sat) {
            return;
        }
        // Pick a latch literal of `state` falsified by the Init model.
        let mut added = false;
        for (l, &val) in g.latches.iter().zip(state) {
            let iv = init_u.sat.value(init_u.lit(0, l.lit));
            if iv != val {
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

fn generalize_cube(
    init_u: &mut Unroll,
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
        let init_ass: Vec<i32> = cube.iter().map(|&al| init_u.lit(0, al)).collect();
        let init_ok = matches!(init_u.sat.solve(&init_ass), Answer::Unsat);
        let step_ok = if init_ok {
            let mut step_ass = frame_assumptions(act_step, prev_lvl);
            for &al in cube.iter() {
                step_ass.push(step_u.lit(1, al));
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

fn can_push(step_u: &mut Unroll, act_step: &[i32], lvl: usize, cube: &ic3::Cube) -> bool {
    let mut ass = frame_assumptions(act_step, lvl);
    for &al in cube {
        ass.push(step_u.lit(1, al));
    }
    matches!(step_u.sat.solve(&ass), Answer::Unsat)
}

fn add_cube_to_solvers(
    step_u: &mut Unroll,
    bad_u: &mut Unroll,
    act_step: &[i32],
    act_bad: &[i32],
    lvl: usize,
    cube: &ic3::Cube,
) {
    let mut cs = Vec::with_capacity(cube.len() + 1);
    cs.push(-act_step[lvl]);
    for &al in cube {
        cs.push(-step_u.lit(0, al));
    }
    step_u.sat.add_clause(&cs);

    let mut cb = Vec::with_capacity(cube.len() + 1);
    cb.push(-act_bad[lvl]);
    for &al in cube {
        cb.push(-bad_u.lit(0, al));
    }
    bad_u.sat.add_clause(&cb);
}

fn reconstruct_cex(g: &Aig, u_states: &[UState], start_uid: usize) -> CarOutcome {
    let init = u_states[start_uid].state.clone();
    let mut inputs = Vec::new();
    let mut cur = Some(start_uid);
    while let Some(uid) = cur {
        inputs.push(u_states[uid].inputs.clone());
        cur = u_states[uid].next;
    }
    let depth = inputs.len().saturating_sub(1);
    let witness = format_named_witness(g, &init, &inputs);
    CarOutcome::Unsafe {
        depth,
        trace: PreparedTrace { init, inputs },
        witness,
    }
}

pub(crate) fn format_named_witness(g: &Aig, init: &[bool], inputs: &[Vec<bool>]) -> String {
    let bit = |b: bool| if b { '1' } else { '0' };
    let d = inputs.len().saturating_sub(1);
    let mut s = format!("informalw 1\ndepth {d}\ninit");
    for (l, &val) in g.latches.iter().zip(init) {
        if matches!(l.init, Init::Zero | Init::One) {
            continue;
        }
        if let Some(name) = g.names.get(&var(l.lit)) {
            s += &format!(" {name}={}", bit(val));
        }
    }
    for (f, frame_in) in inputs.iter().enumerate() {
        s += &format!("\n@{f}");
        for (&i, &val) in g.inputs.iter().zip(frame_in) {
            if let Some(name) = g.names.get(&var(i)) {
                s += &format!(" {name}={}", bit(val));
            }
        }
    }
    s.push('\n');
    s
}
