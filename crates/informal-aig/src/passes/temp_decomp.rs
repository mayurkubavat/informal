//! Temporal decomposition / transient reset-chain analysis
//! (Case–Mishchenko–Brayton FMCAD'09).
//!
//! Runs ternary forward simulation from initial latch values (`Init::Zero` /
//! `Init::One` vs `None` for free latches and all primary inputs) to identify
//! deterministic startup transients (reset synchronizers, power-on delay shift
//! registers) and latches that permanently settle to a constant after `T` steps.

use crate::preprocess::{self as pre, InputMap, LatchMap};
use crate::{is_neg, var, Aig, Init, Lit, Node};

/// Maximum transient depth to simulate before stopping.
const MAX_TRANSIENT_STEPS: usize = 64;

/// Summary of ternary transient simulation on an AIG.
#[derive(Debug, Clone)]
pub struct TransientInfo {
    /// Number of steps before the ternary state reaches a fixpoint (`0` if
    /// already steady at frame 0).
    pub transient_depth: usize,
    /// Steady-state constant value of each latch at/after `transient_depth`
    /// (`None` if input-dependent or oscillating).
    pub steady_const: Vec<Option<bool>>,
    /// Whether any latch is already constant from frame 0 onward.
    pub constant_from_zero: Vec<Option<bool>>,
}

type T = Option<bool>;

#[inline]
fn t_and(a: T, b: T) -> T {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

#[inline]
fn lit_t(vals: &[T], l: Lit) -> T {
    vals[var(l) as usize].map(|b| b ^ is_neg(l))
}

/// Analyze deterministic initialization transients via ternary simulation.
pub fn analyze_transients(g: &Aig) -> TransientInfo {
    let n_latches = g.latches.len();
    let mut state: Vec<T> = g
        .latches
        .iter()
        .map(|l| match l.init {
            Init::Zero => Some(false),
            Init::One => Some(true),
            Init::Free | Init::Expr(_) => None,
        })
        .collect();
    let mut ever_changed = vec![false; n_latches];
    let init_state = state.clone();

    let mut latch_idx = vec![None; g.nodes.len()];
    for (li, l) in g.latches.iter().enumerate() {
        latch_idx[var(l.lit) as usize] = Some(li);
    }

    let mut depth = 0usize;
    let mut vals = vec![None; g.nodes.len()];

    for step in 0..MAX_TRANSIENT_STEPS {
        for (v, node) in g.nodes.iter().enumerate() {
            vals[v] = match *node {
                Node::Const => Some(false),
                Node::Input => None,
                Node::Latch => state[latch_idx[v].expect("latch idx")],
                Node::And(a, b) => t_and(lit_t(&vals, a), lit_t(&vals, b)),
            };
        }
        let next_state: Vec<T> = g.latches.iter().map(|l| lit_t(&vals, l.next)).collect();
        for li in 0..n_latches {
            if next_state[li] != init_state[li] {
                ever_changed[li] = true;
            }
        }
        if next_state == state {
            depth = step;
            break;
        }
        state = next_state;
        depth = step + 1;
    }

    let constant_from_zero = init_state
        .iter()
        .zip(&ever_changed)
        .map(|(&iv, &chg)| if !chg { iv } else { None })
        .collect();

    TransientInfo {
        transient_depth: depth,
        steady_const: state,
        constant_from_zero,
    }
}

/// If ternary simulation revealed latches that remain constant from frame 0
/// onward (without modifying semantics on transient frames `0..T`), fold them
/// through `preprocess::simplify_maps`.
pub fn simplify_transient_constants(
    g: &Aig,
    info: &TransientInfo,
) -> Option<(Aig, LatchMap, InputMap)> {
    if !info.constant_from_zero.iter().any(Option::is_some) {
        return None;
    }
    let (s, _, lm, im) = pre::simplify_maps(g);
    if s.latches.len() < g.latches.len() || s.num_ands() < g.num_ands() {
        Some((s, lm, im))
    } else {
        None
    }
}
