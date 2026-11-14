//! Delta debugging for bit-level models: shrink an AIG while a test stays
//! true (idea 5 in `research/innovations-2026-11-14.md`).
//!
//! The test is any predicate on a model, typically "engine A says safe and
//! engine B says unsafe" or "the certificate is withdrawn". The reducer
//! never applies informal's own preprocessing, since the bug being chased
//! may be in it: it only replaces nodes of the original model and drops
//! what no bad state, constraint or kept latch depends on.
//!
//! Every edit is a substitution on a node of the *original* model:
//!
//! - a latch or an AND gate becomes a free input (cuts its fan-in),
//! - a latch or an input becomes a constant,
//!
//! plus choosing one bad state and dropping constraints. Substitutions
//! accumulate, and candidates are tried in chunks that halve when a chunk
//! fails (the one-pass ddmin variant C-Reduce uses), so a model with many
//! irrelevant nodes shrinks in a few tests per halving.

use crate::{is_neg, var, Aig, Init, Lit, Node, FALSE};

/// What a node of the original model becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sub {
    Keep,
    Input,
    Const(bool),
}

/// The edits applied to an original model so far.
#[derive(Debug, Clone)]
pub struct Edits {
    pub sub: Vec<Sub>,
    /// Indices into the original `bads` that are kept.
    pub bads: Vec<usize>,
    /// Indices into the original `constraints` that are kept.
    pub constraints: Vec<usize>,
}

impl Edits {
    pub fn none(g: &Aig) -> Edits {
        Edits {
            sub: vec![Sub::Keep; g.nodes.len()],
            bads: (0..g.bads.len()).collect(),
            constraints: (0..g.constraints.len()).collect(),
        }
    }
}

/// Which original variables the edited model still depends on.
pub fn live(g: &Aig, e: &Edits) -> Vec<bool> {
    let mut mark = vec![false; g.nodes.len()];
    let mut stack: Vec<u32> = Vec::new();
    for &b in &e.bads {
        stack.push(var(g.bads[b]));
    }
    for &c in &e.constraints {
        stack.push(var(g.constraints[c]));
    }
    let latch_of: std::collections::HashMap<u32, usize> = g
        .latches
        .iter()
        .enumerate()
        .map(|(i, l)| (var(l.lit), i))
        .collect();
    while let Some(v) = stack.pop() {
        if mark[v as usize] {
            continue;
        }
        mark[v as usize] = true;
        if e.sub[v as usize] != Sub::Keep {
            continue;
        }
        match g.nodes[v as usize] {
            Node::And(a, b) => {
                stack.push(var(a));
                stack.push(var(b));
            }
            Node::Latch => {
                let l = &g.latches[latch_of[&v]];
                stack.push(var(l.next));
                if let Init::Expr(x) = l.init {
                    stack.push(var(x));
                }
            }
            Node::Const | Node::Input => {}
        }
    }
    mark
}

/// The edited model: only live nodes, substitutions applied, names kept.
pub fn apply(g: &Aig, e: &Edits) -> Aig {
    let mark = live(g, e);
    let mut out = Aig::new();
    // New literal for each original variable (positive phase).
    let mut map: Vec<Lit> = vec![FALSE; g.nodes.len()];
    let mut new_latches: Vec<(usize, Lit)> = Vec::new();
    let latch_index: std::collections::HashMap<u32, usize> = g
        .latches
        .iter()
        .enumerate()
        .map(|(i, l)| (var(l.lit), i))
        .collect();
    let name = |out: &mut Aig, v: u32, l: Lit| {
        if let Some(n) = g.names.get(&v) {
            out.names.insert(var(l), n.clone());
        }
    };
    for (i, n) in g.nodes.iter().enumerate() {
        if !mark[i] {
            continue;
        }
        let v = i as u32;
        map[i] = match (e.sub[i], *n) {
            (_, Node::Const) => FALSE,
            (Sub::Const(b), _) => b as Lit,
            (Sub::Input, _) | (Sub::Keep, Node::Input) => {
                let l = out.input();
                name(&mut out, v, l);
                l
            }
            (Sub::Keep, Node::Latch) => {
                let li = latch_index[&v];
                // Init::Expr is fixed up below, once its cone is mapped.
                let init = match g.latches[li].init {
                    Init::Expr(_) => Init::Free,
                    x => x,
                };
                let l = out.latch(init);
                name(&mut out, v, l);
                new_latches.push((li, l));
                l
            }
            (Sub::Keep, Node::And(a, b)) => {
                let (a, b) = (lit(&map, a), lit(&map, b));
                out.and(a, b)
            }
        };
    }
    for (li, l) in new_latches {
        let old = &g.latches[li];
        let next = lit(&map, old.next);
        out.set_next(l, next);
        if let Init::Expr(x) = old.init {
            let x = lit(&map, x);
            if let Some(nl) = out.latches.iter_mut().find(|y| y.lit == l) {
                nl.init = Init::Expr(x);
            }
        }
    }
    out.bads = e.bads.iter().map(|&b| lit(&map, g.bads[b])).collect();
    out.constraints = e
        .constraints
        .iter()
        .map(|&c| lit(&map, g.constraints[c]))
        .collect();
    out
}

fn lit(map: &[Lit], l: Lit) -> Lit {
    map[var(l) as usize] ^ is_neg(l) as Lit
}

/// One substitution pass: label, which nodes it applies to, and what they
/// become.
type Pass = (&'static str, fn(&Node) -> bool, Sub);

/// Model size used to report progress: (inputs, latches, ANDs).
pub fn size(g: &Aig) -> (usize, usize, usize) {
    (g.inputs.len(), g.latches.len(), g.num_ands())
}

/// Shrink `g` while `test` holds. `test` must hold on `g` itself (the
/// caller checks that first). `log` gets one line per accepted step.
/// Returns the reduced model and the number of tests run.
pub fn reduce(
    g: &Aig,
    mut test: impl FnMut(&Aig) -> bool,
    mut log: impl FnMut(&str),
) -> (Aig, usize) {
    let mut e = Edits::none(g);
    let mut tests = 0usize;
    let mut try_edit = |cand: &Edits, tests: &mut usize| -> bool {
        *tests += 1;
        test(&apply(g, cand))
    };

    // One bad state is enough to show a disagreement.
    if e.bads.len() > 1 {
        for b in 0..g.bads.len() {
            let mut c = e.clone();
            c.bads = vec![b];
            if try_edit(&c, &mut tests) {
                e = c;
                log(&format!("kept bad {b} only"));
                break;
            }
        }
    }

    loop {
        let before = e.sub.clone();
        let before_cons = e.constraints.len();

        // Drop constraints, chunk by chunk.
        let cons = e.constraints.clone();
        let dropped = ddmin(&cons, |chunk| {
            let mut c = e.clone();
            c.constraints.retain(|x| !chunk.contains(x));
            if try_edit(&c, &mut tests) {
                e = c;
                true
            } else {
                false
            }
        });
        if dropped > 0 {
            log(&format!("dropped {dropped} constraints"));
        }

        // Each substitution kind, on the nodes it applies to that are
        // still live. ANDs go from the outputs down, so one cut near a bad
        // removes a whole cone.
        let passes: [Pass; 5] = [
            (
                "latches -> inputs",
                |n| matches!(n, Node::Latch),
                Sub::Input,
            ),
            ("ANDs -> inputs", |n| matches!(n, Node::And(..)), Sub::Input),
            (
                "inputs -> 0",
                |n| matches!(n, Node::Input),
                Sub::Const(false),
            ),
            (
                "latches -> 0",
                |n| matches!(n, Node::Latch),
                Sub::Const(false),
            ),
            (
                "latches -> 1",
                |n| matches!(n, Node::Latch),
                Sub::Const(true),
            ),
        ];
        for (label, applies, to) in passes {
            let mark = live(g, &e);
            let mut cands: Vec<usize> = (0..g.nodes.len())
                .filter(|&i| mark[i] && e.sub[i] == Sub::Keep && applies(&g.nodes[i]))
                .collect();
            cands.reverse();
            let n = ddmin(&cands, |chunk| {
                let mut c = e.clone();
                for &i in chunk {
                    c.sub[i] = to;
                }
                if try_edit(&c, &mut tests) {
                    e = c;
                    true
                } else {
                    false
                }
            });
            if n > 0 {
                let (i, l, a) = size(&apply(g, &e));
                log(&format!(
                    "{label}: {n} accepted; now {i} inputs, {l} latches, {a} ANDs ({tests} tests)"
                ));
            }
        }

        if e.sub == before && e.constraints.len() == before_cons {
            break;
        }
    }
    (apply(g, &e), tests)
}

/// Try applying chunks of `items`, halving the chunk size after a full
/// pass, down to single items. `accept` applies a chunk and says whether
/// it was kept. Returns how many items were accepted.
fn ddmin<T: Copy + PartialEq>(items: &[T], mut accept: impl FnMut(&[T]) -> bool) -> usize {
    let mut left: Vec<T> = items.to_vec();
    let mut done = 0;
    let mut size = left.len().max(1);
    while size >= 1 && !left.is_empty() {
        let mut i = 0;
        while i < left.len() {
            let end = (i + size).min(left.len());
            let chunk: Vec<T> = left[i..end].to_vec();
            if accept(&chunk) {
                done += chunk.len();
                left.drain(i..end);
            } else {
                i = end;
            }
        }
        if size == 1 {
            break;
        }
        size = (size / 2).max(1);
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lit_val, neg};

    /// Does `g` reach a bad state within `k` steps, by brute force over
    /// inputs (tiny models only)?
    fn reaches_bad(g: &Aig, k: usize) -> bool {
        let ni = g.inputs.len();
        let mut states: Vec<Vec<bool>> =
            vec![g.latches.iter().map(|l| l.init == Init::One).collect()];
        for _ in 0..=k {
            let mut next = Vec::new();
            for s in &states {
                for m in 0..(1u32 << ni) {
                    let v = g.eval(|l| {
                        if let Some(i) = g.inputs.iter().position(|&x| x == l) {
                            m >> i & 1 == 1
                        } else {
                            let j = g.latches.iter().position(|x| x.lit == l).unwrap();
                            s[j]
                        }
                    });
                    if g.constraints.iter().any(|&c| !lit_val(&v, c)) {
                        continue;
                    }
                    if g.bads.iter().any(|&b| lit_val(&v, b)) {
                        return true;
                    }
                    let n: Vec<bool> = g.latches.iter().map(|l| lit_val(&v, l.next)).collect();
                    if !next.contains(&n) {
                        next.push(n);
                    }
                }
            }
            states = next;
        }
        false
    }

    /// A 3-bit counter that reaches 7 (the bug), next to an unrelated
    /// 4-latch shift register and a constraint that does not matter.
    fn model() -> Aig {
        let mut g = Aig::new();
        let en = g.input();
        let junk_in = g.input();
        let c: Vec<Lit> = (0..3).map(|_| g.latch(Init::Zero)).collect();
        let s: Vec<Lit> = (0..4).map(|_| g.latch(Init::Zero)).collect();
        // counter += en
        let mut carry = en;
        for &b in &c {
            let n = g.xor(b, carry);
            carry = g.and(b, carry);
            g.set_next(b, n);
        }
        g.set_next(s[0], junk_in);
        for i in 1..4 {
            g.set_next(s[i], s[i - 1]);
        }
        let all = g.and_all(&c);
        g.bads.push(all);
        let sr = g.and_all(&s);
        g.bads.push(sr); // a second, also reachable bad
        let nj = neg(junk_in);
        let ok = g.or(nj, junk_in);
        g.constraints.push(ok);
        g
    }

    #[test]
    fn apply_with_no_edits_keeps_behaviour() {
        let g = model();
        let h = apply(&g, &Edits::none(&g));
        // Same inputs and latches; dead gates (the counter's last carry)
        // are dropped.
        let ((gi, gl, ga), (hi, hl, ha)) = (size(&g), size(&h));
        assert_eq!((gi, gl), (hi, hl));
        assert!(ha <= ga);
        assert!(reaches_bad(&h, 8) && !reaches_bad(&h, 1));
    }

    #[test]
    fn reduces_to_a_small_model_that_still_fails() {
        let g = model();
        // "Interesting": a bad is reachable within 8 steps but not in 1
        // (so the reducer cannot make the bug trivial by a constant).
        let test = |h: &Aig| reaches_bad(h, 8) && !reaches_bad(h, 1);
        assert!(test(&g));
        let (h, tests) = reduce(&g, test, |_| {});
        assert!(test(&h));
        let (_, l, _) = size(&h);
        assert!(l < g.latches.len(), "latches {l}");
        assert_eq!(h.bads.len(), 1);
        assert!(h.constraints.is_empty());
        assert!(tests > 0);
    }

    #[test]
    fn ddmin_finds_single_needed_item() {
        // Accept a removal only if item 5 stays.
        let items: Vec<u32> = (0..10).collect();
        let mut kept = items.clone();
        let n = ddmin(&items, |chunk| {
            if chunk.contains(&5) {
                false
            } else {
                kept.retain(|x| !chunk.contains(x));
                true
            }
        });
        assert_eq!(n, 9);
        assert_eq!(kept, vec![5]);
    }
}
