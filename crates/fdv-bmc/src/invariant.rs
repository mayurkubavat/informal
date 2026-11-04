//! Turning candidate lemmas into a proved invariant (Houdini).
//!
//! Candidates are cubes over latch literals; the clause `!cube` is the
//! lemma. The largest subset that holds initially and is inductive on its
//! own (the conjunction of its clauses at frame 0 implies each clause at
//! frame 1) is found by dropping every clause a counterexample violates,
//! until none is left to drop. What remains holds in every reachable state,
//! so any engine may assume it in any frame.

use crate::ic3::Cube;
use crate::unroll::Unroll;
use fdv_aig::Aig;
use fdv_engine::RunContext;
use fdv_sat::{Answer, Sat};

/// The inductive subset of `cands`, or `None` when time runs out.
pub fn houdini(
    g: &Aig,
    cands: Vec<Cube>,
    make: impl Fn() -> Box<dyn Sat>,
    ctx: &RunContext,
) -> Option<Vec<Cube>> {
    let mut cands = cands;
    cands.sort();
    cands.dedup();
    // Initiation: no initial state lies in the cube.
    let mut init = Unroll::new(g, make(), false);
    init.add_frame();
    let mut keep = Vec::with_capacity(cands.len());
    for c in cands {
        if ctx.should_stop() {
            return None;
        }
        let a: Vec<i32> = c.iter().map(|&l| init.lit(0, l)).collect();
        match init.sat.solve(&a) {
            Answer::Unsat => keep.push(c),
            Answer::Sat => {}
            Answer::Unknown => return None,
        }
    }
    let cands = keep;
    if cands.is_empty() {
        return Some(cands);
    }
    // Consecution: selector s_i turns clause i on in frame 0; h_i holds
    // when cube i is true in frame 1 (the clause is violated there).
    let mut u = Unroll::new(g, make(), true);
    u.add_frame();
    u.add_frame();
    let mut sel = Vec::with_capacity(cands.len());
    let mut hit = Vec::with_capacity(cands.len());
    for c in &cands {
        let s = u.new_var();
        let mut cl: Vec<i32> = c.iter().map(|&l| -u.lit(0, l)).collect();
        cl.push(-s);
        u.sat.add_clause(&cl);
        let h = u.new_var();
        for &l in c {
            let x = u.lit(1, l);
            u.sat.add_clause(&[-h, x]);
        }
        sel.push(s);
        hit.push(h);
    }
    let mut alive = vec![true; cands.len()];
    loop {
        if ctx.should_stop() {
            return None;
        }
        // "Some live clause fails at frame 1", under a fresh activation.
        let act = u.new_var();
        let mut any = vec![-act];
        any.extend((0..cands.len()).filter(|&i| alive[i]).map(|i| hit[i]));
        u.sat.add_clause(&any);
        let mut assume: Vec<i32> = (0..cands.len())
            .filter(|&i| alive[i])
            .map(|i| sel[i])
            .collect();
        assume.push(act);
        let r = u.sat.solve(&assume);
        let dropped: Vec<usize> = match r {
            Answer::Unsat => {
                u.sat.add_clause(&[-act]);
                break;
            }
            Answer::Unknown => return None,
            Answer::Sat => (0..cands.len())
                .filter(|&i| alive[i] && cands[i].iter().all(|&l| u.sat.value(u.lit(1, l))))
                .collect(),
        };
        u.sat.add_clause(&[-act]);
        debug_assert!(!dropped.is_empty());
        if dropped.is_empty() {
            return None;
        }
        for i in dropped {
            alive[i] = false;
        }
    }
    Some(
        cands
            .into_iter()
            .zip(alive)
            .filter_map(|(c, a)| a.then_some(c))
            .collect(),
    )
}
