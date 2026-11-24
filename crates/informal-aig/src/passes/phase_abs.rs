//! Phase abstraction pass: autonomous clock/phase period detection and
//! period-2 clock folding (`preprocess::fold_clock`).

use crate::preprocess::{self as pre, InputMap, LatchMap};
use crate::{is_neg, var, Aig, Init, Lit, Node};

/// Maximum autonomous period searched by [`detect_phase_period`].
const MAX_PERIOD: usize = 8;

/// Detect the smallest period `p >= 2` of an autonomous deterministic latch
/// (such as a free-running clock `clk' = !clk` or ring-counter phase generator)
/// whose next-state cone depends only on constant-initialized latches (no inputs).
pub fn detect_phase_period(g: &Aig) -> Option<usize> {
    if g.latches.is_empty() {
        return None;
    }
    let n = g.nodes.len();
    let mut latch_idx = vec![None; n];
    for (li, l) in g.latches.iter().enumerate() {
        latch_idx[var(l.lit) as usize] = Some(li);
    }

    // Mark nodes that are autonomous (depend on NO inputs and NO free/expr latches).
    let mut auto_latch: Vec<bool> = g
        .latches
        .iter()
        .map(|l| matches!(l.init, Init::Zero | Init::One))
        .collect();
    let mut auto_node = vec![false; n];
    loop {
        for (v, node) in g.nodes.iter().enumerate() {
            auto_node[v] = match *node {
                Node::Const => true,
                Node::Input => false,
                Node::Latch => auto_latch[latch_idx[v].expect("latch idx")],
                Node::And(a, b) => auto_node[var(a) as usize] && auto_node[var(b) as usize],
            };
        }
        let mut changed = false;
        for (li, l) in g.latches.iter().enumerate() {
            if auto_latch[li] && !auto_node[var(l.next) as usize] {
                auto_latch[li] = false;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    if !auto_latch.iter().any(|&b| b) {
        return None;
    }

    // Simulate autonomous latches for `2 * MAX_PERIOD` steps to detect their period.
    let mut state: Vec<bool> = g
        .latches
        .iter()
        .map(|l| matches!(l.init, Init::One))
        .collect();
    let mut history: Vec<Vec<bool>> = Vec::with_capacity(2 * MAX_PERIOD + 1);
    let mut vals = vec![false; n];

    for _ in 0..=(2 * MAX_PERIOD) {
        let proj: Vec<bool> = state
            .iter()
            .zip(&auto_latch)
            .filter_map(|(&s, &a)| a.then_some(s))
            .collect();
        if let Some(prev) = history.iter().position(|h| h == &proj) {
            let period = history.len() - prev;
            if (2..=MAX_PERIOD).contains(&period) {
                return Some(period);
            }
            return None;
        }
        history.push(proj);

        for (v, node) in g.nodes.iter().enumerate() {
            vals[v] = match *node {
                Node::Const | Node::Input => false,
                Node::Latch => state[latch_idx[v].expect("latch idx")],
                Node::And(a, b) => eval_lit(&vals, a) && eval_lit(&vals, b),
            };
        }
        for (li, l) in g.latches.iter().enumerate() {
            if auto_latch[li] {
                state[li] = eval_lit(&vals, l.next);
            }
        }
    }
    None
}

#[inline]
fn eval_lit(vals: &[bool], l: Lit) -> bool {
    vals[var(l) as usize] ^ is_neg(l)
}

/// Apply period-2 clock folding (`preprocess::fold_clock`) followed by
/// `simplify_maps`, composing the resulting [`LatchMap`] and [`InputMap`].
pub fn fold_phase(g: &Aig) -> Option<(Aig, LatchMap, InputMap)> {
    let (folded, _, fl, fi) = pre::fold_clock(g)?;
    let (simp, _, sl, si) = pre::simplify_maps(&folded);
    Some((
        simp,
        pre::compose(&fl, &folded, &sl),
        pre::compose_inputs(&fi, &folded, &si),
    ))
}
