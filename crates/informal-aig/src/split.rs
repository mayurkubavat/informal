//! Splitting a property that is a conjunction into its parts.
//!
//! A model's bad output is often a wide OR: `bad = b1 | ... | bk`, so the
//! property `!bad` is the conjunction of the `!b_i`. Each `b_i` on the same
//! design is a property of its own, usually with a smaller cone, and an
//! invariant proved for one holds in every reachable state of the design,
//! so the parts can be proved separately and help one another (the
//! invariant store's sibling properties, made from one property). The
//! conjunction of the parts' invariants proves the original.
//!
//! In an AIG an OR is a negated AND, so the terms are found by opening
//! negated AND nodes from the bad literal down.

use crate::{is_neg, neg, var, Aig, Lit, Node, FALSE, TRUE};

/// The OR terms of the model's bad outputs: `bad_1 | bad_2 | ...` is
/// equivalent to the OR of the result. Sorted, without duplicates and
/// without constant-false terms; `[TRUE]` when some term is constant true.
/// Opening stops once there would be more than `max` terms (the terms
/// left unopened are still correct, only wider).
pub fn terms(g: &Aig, max: usize) -> Vec<Lit> {
    let mut work: std::collections::VecDeque<Lit> = g.bads.iter().copied().collect();
    let mut seen: std::collections::HashSet<Lit> = work.iter().copied().collect();
    let mut out = Vec::new();
    while let Some(l) = work.pop_front() {
        if l == TRUE {
            return vec![TRUE];
        }
        if l == FALSE {
            continue;
        }
        // !(a & b) = !a | !b
        if let (true, Node::And(a, b)) = (is_neg(l), g.nodes[var(l) as usize]) {
            if out.len() + work.len() + 2 <= max {
                for t in [neg(a), neg(b)] {
                    if seen.insert(t) {
                        work.push_back(t);
                    }
                }
                continue;
            }
        }
        out.push(l);
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// `g` with `bad` as its only bad output. Inputs, latches and their order
/// are unchanged, so a latch index means the same latch in every model
/// made this way from one `g`.
pub fn with_bad(g: &Aig, bad: Lit) -> Aig {
    let mut h = g.clone();
    h.bads = vec![bad];
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lit_val, Init};

    /// Three inputs and a latch; bad = a | (b & l) | !c.
    fn model() -> (Aig, [Lit; 4]) {
        let mut g = Aig::new();
        let (a, b, c) = (g.input(), g.input(), g.input());
        let l = g.latch(Init::Zero);
        g.set_next(l, a);
        let bl = g.and(b, l);
        let o1 = g.or(a, bl);
        let bad = g.or(o1, neg(c));
        g.bads.push(bad);
        (g, [a, b, c, l])
    }

    #[test]
    fn an_or_tree_opens_into_its_leaves() {
        let (g, [a, b, c, l]) = model();
        let t = terms(&g, 64);
        assert_eq!(t.len(), 3, "{t:?}");
        assert!(t.contains(&a) && t.contains(&neg(c)));
        // The third term is the AND node b & l, kept whole.
        let and = *t.iter().find(|&&x| x != a && x != neg(c)).unwrap();
        assert_eq!(g.nodes[var(and) as usize], Node::And(b.min(l), b.max(l)));
    }

    #[test]
    fn the_terms_or_to_the_bad_output() {
        let (g, leaves) = model();
        let t = terms(&g, 64);
        for m in 0..16u32 {
            let vals = g.eval(|x| {
                let k = leaves.iter().position(|&y| y == x).unwrap();
                m >> k & 1 == 1
            });
            let any = t.iter().any(|&x| lit_val(&vals, x));
            assert_eq!(any, lit_val(&vals, g.bads[0]), "assignment {m:04b}");
        }
    }

    #[test]
    fn an_and_is_one_term_and_the_cap_stops_opening() {
        let mut g = Aig::new();
        let (a, b) = (g.input(), g.input());
        let bad = g.and(a, b);
        g.bads.push(bad);
        assert_eq!(terms(&g, 64), vec![bad]);

        let (g, _) = model();
        // With room for one term only, nothing is opened.
        assert_eq!(terms(&g, 1), g.bads);
        assert_eq!(terms(&g, 2).len(), 2);
    }

    #[test]
    fn several_bad_outputs_are_terms_and_constants_are_handled() {
        let mut g = Aig::new();
        let (a, b) = (g.input(), g.input());
        g.bads = vec![a, b, FALSE, a];
        assert_eq!(terms(&g, 64), vec![a, b]);
        g.bads.push(TRUE);
        assert_eq!(terms(&g, 64), vec![TRUE]);
        assert_eq!(with_bad(&g, b).bads, vec![b]);
    }
}
