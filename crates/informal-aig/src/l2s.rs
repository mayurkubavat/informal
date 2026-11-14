//! Liveness to safety (Biere, Artho, Schuppan: "Liveness checking as safety
//! checking", FMICS 2002).
//!
//! A justice property is a set of conditions; an infinite trace on which
//! each of them (and each fairness condition of the model) is true
//! infinitely often is a counterexample. In a finite model such a trace
//! exists exactly when a lasso does: a state that the model comes back to,
//! with every condition true somewhere on the way round. [`to_safety`]
//! turns that into a `bad`:
//!
//! - a free input [`SAVE`] picks the step whose state is the start of the
//!   loop, a latch [`SAVED`] remembers that it was picked, and a shadow
//!   latch per latch of the model keeps that state;
//! - a latch per condition remembers that the condition was true at the
//!   picked step or after it;
//! - the bad is: a state was saved at an earlier step, the latches are
//!   equal to their shadows now, and every condition was seen.
//!
//! Constraints stay constraints: they hold at every step of the stem and
//! the loop, so they hold for ever on the trace that goes round the loop.
//! The result is an ordinary safety model, so every engine, preprocessing
//! pass and certificate works on it unchanged. A SAFE verdict means no
//! lasso exists, which for a finite model means the liveness property
//! holds; an UNSAFE one is a lasso, read back with [`lasso`] and checked on
//! the model as it was with [`check_lasso`].

use crate::{is_neg, neg, var, Aig, Init, Lit, Node};

/// Name of the input that picks the step where the loop starts.
pub const SAVE: &str = "informal_l2s_save";
/// Name of the latch that is set from the step after that one.
pub const SAVED: &str = "informal_l2s_saved";
/// Prefix of the names of the shadow latches and of the per-condition
/// latches. Helpers named `informal_*` are kept out of the design's scopes
/// in traces.
pub const HELPER: &str = "informal_l2s_";

fn or(g: &mut Aig, a: Lit, b: Lit) -> Lit {
    neg(g.and(neg(a), neg(b)))
}

fn mux(g: &mut Aig, c: Lit, t: Lit, e: Lit) -> Lit {
    let (x, y) = (g.and(c, t), g.and(neg(c), e));
    or(g, x, y)
}

fn same(g: &mut Aig, a: Lit, b: Lit) -> Lit {
    let (x, y) = (g.and(a, b), g.and(neg(a), neg(b)));
    or(g, x, y)
}

/// Replace every justice property of `g` (with its fairness conditions) by
/// a `bad`, appended after the bads the model already has, in order. A
/// model with no justice property is left as it is, apart from fairness
/// conditions, which only constrain liveness and are dropped.
pub fn to_safety(g: &mut Aig) {
    let justice = std::mem::take(&mut g.justice);
    let fairness = std::mem::take(&mut g.fairness);
    if justice.is_empty() {
        return;
    }
    let originals: Vec<Lit> = g.latches.iter().map(|l| l.lit).collect();
    let save = g.input();
    g.names.insert(var(save), SAVE.into());
    let saved = g.latch(Init::Zero);
    g.names.insert(var(saved), SAVED.into());
    // The step whose state is kept: the first one picked.
    let first = g.and(save, neg(saved));
    let in_loop = or(g, saved, save);
    g.set_next(saved, in_loop);
    let mut closed = saved;
    for &l in &originals {
        let shadow = g.latch(Init::Zero);
        let name = match g.names.get(&var(l)) {
            Some(n) => format!("{HELPER}at_{}", n.replace('.', "_")),
            None => format!("{HELPER}at_{}", var(l)),
        };
        g.names.insert(var(shadow), name);
        let keep = mux(g, first, l, shadow);
        g.set_next(shadow, keep);
        let eq = same(g, l, shadow);
        closed = g.and(closed, eq);
    }
    let mut seen_latches = 0;
    for conditions in justice {
        let mut bad = closed;
        for c in conditions.iter().chain(&fairness) {
            let seen = g.latch(Init::Zero);
            g.names
                .insert(var(seen), format!("{HELPER}seen_{seen_latches}"));
            seen_latches += 1;
            let now = or(g, seen, *c);
            let next = g.and(in_loop, now);
            g.set_next(seen, next);
            bad = g.and(bad, seen);
        }
        g.bads.push(bad);
    }
}

/// True for a latch or input that [`to_safety`] added.
pub fn is_helper(name: &str) -> bool {
    name.starts_with(HELPER)
}

/// The lasso of a counterexample on a model made by [`to_safety`]: the step
/// the loop starts at, given for each step whether [`SAVE`] was 1 in it.
/// The loop closes at the step the bad is true in, which has the state of
/// the start step again. `None` when no step was picked.
pub fn lasso(save_by_step: &[bool]) -> Option<usize> {
    save_by_step.iter().position(|&s| s)
}

/// The values of every variable of `g` in one step, from its latch and
/// input values (by position in [`Aig::latches`] and [`Aig::inputs`]).
pub fn eval(g: &Aig, latches: &[bool], inputs: &[bool]) -> Vec<bool> {
    let mut v = vec![false; g.nodes.len()];
    for (l, &b) in g.latches.iter().zip(latches) {
        v[var(l.lit) as usize] = b;
    }
    for (&i, &b) in g.inputs.iter().zip(inputs) {
        v[var(i) as usize] = b;
    }
    let lit = |v: &[bool], l: Lit| v[var(l) as usize] ^ is_neg(l);
    for i in 0..g.nodes.len() {
        if let Node::And(a, b) = g.nodes[i] {
            v[i] = lit(&v, a) && lit(&v, b);
        }
    }
    v
}

/// Check a lasso on `g`, the model before [`to_safety`]: from the latch
/// values `init`, with the inputs `inputs[t]` in step `t`, the model keeps
/// its constraints, is at step `inputs.len()` in the state it had at step
/// `start`, and every condition of justice property `property`, and every
/// fairness condition, is true in some step from `start` on. `Err` says
/// which of these fails.
pub fn check_lasso(
    g: &Aig,
    property: usize,
    init: &[bool],
    inputs: &[Vec<bool>],
    start: usize,
) -> Result<(), String> {
    let lit = |v: &[bool], l: Lit| v[var(l) as usize] ^ is_neg(l);
    let conditions: Vec<Lit> = g
        .justice
        .get(property)
        .ok_or("no such justice property")?
        .iter()
        .chain(&g.fairness)
        .copied()
        .collect();
    if start >= inputs.len() {
        return Err("the loop starts at or after the step it closes in".into());
    }
    let mut state = init.to_vec();
    let mut at_start = Vec::new();
    let mut hit = vec![false; conditions.len()];
    for (t, step) in inputs.iter().enumerate() {
        if t == start {
            at_start = state.clone();
        }
        let v = eval(g, &state, step);
        if let Some(c) = g.constraints.iter().find(|&&c| !lit(&v, c)) {
            return Err(format!("constraint {c} does not hold in step {t}"));
        }
        if t >= start {
            for (h, &c) in hit.iter_mut().zip(&conditions) {
                *h |= lit(&v, c);
            }
        }
        state = g.latches.iter().map(|l| lit(&v, l.next)).collect();
    }
    if state != at_start {
        return Err(format!(
            "the state after step {} is not the state of step {start}",
            inputs.len() - 1
        ));
    }
    match hit.iter().position(|h| !h) {
        Some(i) => Err(format!(
            "condition {} is never true in the loop",
            conditions[i]
        )),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet, VecDeque};

    /// A two-bit counter that goes up by one when its input is 1, and
    /// stops at `stop` when given.
    fn counter(stop: Option<u8>) -> (Aig, [Lit; 2]) {
        let mut g = Aig::new();
        let up = g.input();
        let (b0, b1) = (g.latch(Init::Zero), g.latch(Init::Zero));
        let at = |g: &mut Aig, n: u8| {
            let (x, y) = (
                if n & 1 == 1 { b0 } else { neg(b0) },
                if n & 2 == 2 { b1 } else { neg(b1) },
            );
            g.and(x, y)
        };
        let stopped = stop.map_or(crate::FALSE, |n| at(&mut g, n));
        let step = g.and(up, neg(stopped));
        let n0 = mux(&mut g, step, neg(b0), b0);
        let carry = g.and(step, b0);
        let n1 = mux(&mut g, carry, neg(b1), b1);
        g.set_next(b0, n0);
        g.set_next(b1, n1);
        (g, [b0, b1])
    }

    /// Breadth-first search of the reachable states of a safety model:
    /// the inputs of a shortest trace to a bad state (one vector per step,
    /// the last one for the step the bad is true in), or `None` when no bad
    /// state can be reached.
    fn reach_bad(g: &Aig) -> Option<Vec<Vec<bool>>> {
        let lit = |v: &[bool], l: Lit| v[var(l) as usize] ^ is_neg(l);
        let init: Vec<bool> = g
            .latches
            .iter()
            .map(|l| match l.init {
                Init::Zero => false,
                Init::One => true,
                _ => panic!("test models have constant initial values"),
            })
            .collect();
        let n_in = g.inputs.len();
        assert!(n_in <= 8);
        // For each state reached: the state before it and the inputs that led here.
        type Step = Option<(Vec<bool>, Vec<bool>)>;
        let mut from: HashMap<Vec<bool>, Step> = HashMap::new();
        from.insert(init.clone(), None);
        let mut queue = VecDeque::from([init]);
        let mut seen = HashSet::new();
        while let Some(s) = queue.pop_front() {
            if !seen.insert(s.clone()) {
                continue;
            }
            for bits in 0..(1u32 << n_in) {
                let inputs: Vec<bool> = (0..n_in).map(|i| bits >> i & 1 == 1).collect();
                let v = eval(g, &s, &inputs);
                if g.constraints.iter().any(|&c| !lit(&v, c)) {
                    continue;
                }
                if g.bads.iter().any(|&b| lit(&v, b)) {
                    let mut trace = vec![inputs];
                    let mut cur = s.clone();
                    while let Some(Some((prev, how))) = from.get(&cur) {
                        trace.push(how.clone());
                        cur = prev.clone();
                    }
                    trace.reverse();
                    return Some(trace);
                }
                let next: Vec<bool> = g.latches.iter().map(|l| lit(&v, l.next)).collect();
                if !from.contains_key(&next) {
                    from.insert(next.clone(), Some((s.clone(), inputs)));
                    queue.push_back(next);
                }
            }
        }
        None
    }

    /// The lasso a trace of the transformed model stands for, checked on
    /// the model as it was: (loop start, steps before the loop closes).
    fn replay(original: &Aig, safe: &Aig, trace: &[Vec<bool>]) -> (usize, usize) {
        let save_at = safe
            .inputs
            .iter()
            .position(|i| safe.names.get(&var(*i)).is_some_and(|n| n == SAVE))
            .expect("the save input");
        let picks: Vec<bool> = trace.iter().map(|step| step[save_at]).collect();
        let start = lasso(&picks).expect("a counterexample picks a step");
        // The model's own inputs come first, in the same order; the last
        // step only shows the bad, so the loop closes before it.
        let n = original.inputs.len();
        let steps: Vec<Vec<bool>> = trace[..trace.len() - 1]
            .iter()
            .map(|s| s[..n].to_vec())
            .collect();
        let init = vec![false; original.latches.len()];
        check_lasso(original, 0, &init, &steps, start).unwrap();
        (start, steps.len())
    }

    #[test]
    fn a_reachable_loop_through_the_condition_is_a_bad() {
        // The counter can go round for ever, passing 3 each time.
        let (mut g, [b0, b1]) = counter(None);
        let three = g.and(b0, b1);
        g.justice.push(vec![three]);
        let original = g.clone();
        to_safety(&mut g);
        assert!(g.justice.is_empty() && g.bads.len() == 1);
        let trace = reach_bad(&g).expect("a lasso");
        let (start, close) = replay(&original, &g, &trace);
        assert!(start < close);
        // A loop that stays at 3 is the shortest: three steps up, one round.
        assert_eq!((start, close), (3, 4));
    }

    #[test]
    fn no_loop_through_the_condition_is_safe() {
        // The counter stops at 2, so 3 is never reached, let alone again
        // and again.
        let (mut g, [b0, b1]) = counter(Some(2));
        let three = g.and(b0, b1);
        g.justice.push(vec![three]);
        to_safety(&mut g);
        assert!(reach_bad(&g).is_none());
    }

    #[test]
    fn every_condition_and_fairness_must_be_in_the_loop() {
        // 0 again and again, and 2 again and again: only by going round.
        let (mut g, [b0, b1]) = counter(None);
        let zero = g.and(neg(b0), neg(b1));
        let two = g.and(neg(b0), b1);
        g.justice.push(vec![zero]);
        g.fairness.push(two);
        let original = g.clone();
        to_safety(&mut g);
        let trace = reach_bad(&g).expect("a lasso");
        let (start, close) = replay(&original, &g, &trace);
        assert_eq!(close - start, 4, "the loop is a whole round");

        // With the counter stopping at 1, 2 is never seen: fairness rules
        // every loop out.
        let (mut g, [b0, b1]) = counter(Some(1));
        let zero = g.and(neg(b0), neg(b1));
        let two = g.and(neg(b0), b1);
        g.justice.push(vec![zero]);
        g.fairness.push(two);
        to_safety(&mut g);
        assert!(reach_bad(&g).is_none());
    }

    #[test]
    fn constraints_hold_round_the_loop() {
        // The counter may not pass through 3, so a loop needs it to stay
        // put: staying at 1 for ever shows 1 again and again.
        let (mut g, [b0, b1]) = counter(None);
        let three = g.and(b0, b1);
        let one = g.and(b0, neg(b1));
        g.constraints.push(neg(three));
        g.justice.push(vec![one]);
        let original = g.clone();
        to_safety(&mut g);
        let trace = reach_bad(&g).expect("a lasso");
        let (start, close) = replay(&original, &g, &trace);
        assert_eq!((start, close), (1, 2));
        // But 0 and 2 again and again would need the way round through 3.
        let (mut g, [b0, b1]) = counter(None);
        let three = g.and(b0, b1);
        let zero = g.and(neg(b0), neg(b1));
        let two = g.and(neg(b0), b1);
        g.constraints.push(neg(three));
        g.justice.push(vec![zero, two]);
        to_safety(&mut g);
        assert!(reach_bad(&g).is_none());
    }

    #[test]
    fn a_model_without_justice_is_left_alone() {
        let (mut g, [b0, _]) = counter(None);
        g.bads.push(b0);
        g.fairness.push(b0);
        let before = (g.inputs.len(), g.latches.len(), g.bads.clone());
        to_safety(&mut g);
        assert_eq!((g.inputs.len(), g.latches.len(), g.bads.clone()), before);
        assert!(g.fairness.is_empty());
    }
}
