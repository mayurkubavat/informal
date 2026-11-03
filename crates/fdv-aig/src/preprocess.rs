//! Model simplification before any engine runs, in the order the round-2
//! research recommends porting from ABC / rIC3: cone of influence and
//! constant propagation first (this module), structural hashing on the way
//! (every rebuilt AND goes through [`Aig::and`]); DAG-CNF elimination,
//! fraiging and rewriting come later.

use crate::{is_neg, var, Aig, Init, Lit, Node, FALSE, TRUE};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub latches_before: usize,
    pub latches_after: usize,
    pub ands_before: usize,
    pub ands_after: usize,
    pub inputs_before: usize,
    pub inputs_after: usize,
    pub constant_latches: usize,
}

/// Ternary value: Some(bool) known, None unknown.
type T = Option<bool>;

fn t_and(a: T, b: T) -> T {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

/// Latches that keep their initial value forever: assume every latch with
/// a constant init is stuck there, simulate one step with inputs unknown,
/// drop the ones whose next value differs, and repeat to a fixpoint.
fn constant_latches(g: &Aig) -> Vec<Option<bool>> {
    let mut stuck: Vec<Option<bool>> = g
        .latches
        .iter()
        .map(|l| match l.init {
            Init::Zero => Some(false),
            Init::One => Some(true),
            _ => None,
        })
        .collect();
    let index: std::collections::HashMap<u32, usize> = g
        .latches
        .iter()
        .enumerate()
        .map(|(i, l)| (var(l.lit), i))
        .collect();
    loop {
        let mut vals: Vec<T> = vec![None; g.nodes.len()];
        for (v, n) in g.nodes.iter().enumerate() {
            vals[v] = match *n {
                Node::Const => Some(false),
                Node::Input => None,
                Node::Latch => stuck[index[&(v as u32)]],
                Node::And(a, b) => t_and(lit_t(&vals, a), lit_t(&vals, b)),
            };
        }
        let mut changed = false;
        for (i, l) in g.latches.iter().enumerate() {
            if let Some(c) = stuck[i] {
                if lit_t(&vals, l.next) != Some(c) {
                    stuck[i] = None;
                    changed = true;
                }
            }
        }
        if !changed {
            return stuck;
        }
    }
}

fn lit_t(vals: &[T], l: Lit) -> T {
    vals[var(l) as usize].map(|b| b ^ is_neg(l))
}

/// Cone of influence of the bad and constraint outputs, constant latches
/// replaced by their value, and the rest rebuilt with structural hashing.
pub fn simplify(g: &Aig) -> (Aig, Stats) {
    let stuck = constant_latches(g);
    let mut stats = Stats {
        latches_before: g.latches.len(),
        ands_before: g.num_ands(),
        inputs_before: g.inputs.len(),
        constant_latches: stuck.iter().filter(|s| s.is_some()).count(),
        ..Default::default()
    };
    let latch_of: std::collections::HashMap<u32, usize> = g
        .latches
        .iter()
        .enumerate()
        .map(|(i, l)| (var(l.lit), i))
        .collect();

    // Cone of influence over the original graph.
    let mut need = vec![false; g.nodes.len()];
    let mut stack: Vec<u32> = g
        .bads
        .iter()
        .chain(&g.constraints)
        .map(|&l| var(l))
        .collect();
    while let Some(v) = stack.pop() {
        if std::mem::replace(&mut need[v as usize], true) {
            continue;
        }
        match g.nodes[v as usize] {
            Node::And(a, b) => stack.extend([var(a), var(b)]),
            Node::Latch => {
                let i = latch_of[&v];
                if stuck[i].is_none() {
                    let l = &g.latches[i];
                    stack.push(var(l.next));
                    if let Init::Expr(e) = l.init {
                        stack.push(var(e));
                    }
                }
            }
            _ => {}
        }
    }

    // Rebuild in node order; latch next functions are attached at the end.
    let mut out = Aig::new();
    let mut map = vec![FALSE; g.nodes.len()];
    let mut new_latches = Vec::new();
    for (v, n) in g.nodes.iter().enumerate() {
        if !need[v] {
            continue;
        }
        map[v] = match *n {
            Node::Const => FALSE,
            Node::Input => {
                let l = out.input();
                if let Some(name) = g.names.get(&(v as u32)) {
                    out.names.insert(var(l), name.clone());
                }
                l
            }
            Node::Latch => {
                let i = latch_of[&(v as u32)];
                match stuck[i] {
                    Some(c) => {
                        if c {
                            TRUE
                        } else {
                            FALSE
                        }
                    }
                    None => {
                        // Expr inits are remapped below, once their cone exists.
                        let init = match g.latches[i].init {
                            Init::Expr(_) => Init::Free,
                            other => other,
                        };
                        let l = out.latch(init);
                        if let Some(name) = g.names.get(&(v as u32)) {
                            out.names.insert(var(l), name.clone());
                        }
                        new_latches.push((i, l));
                        l
                    }
                }
            }
            Node::And(a, b) => {
                let (x, y) = (
                    map[var(a) as usize] ^ (a & 1),
                    map[var(b) as usize] ^ (b & 1),
                );
                out.and(x, y)
            }
        };
    }
    let m = |map: &[Lit], l: Lit| map[var(l) as usize] ^ (l & 1);
    for (i, l) in new_latches {
        let old = &g.latches[i];
        out.set_next(l, m(&map, old.next));
        if let Init::Expr(e) = old.init {
            let e = m(&map, e);
            let init = match e {
                FALSE => Init::Zero,
                TRUE => Init::One,
                _ => Init::Expr(e),
            };
            out.latches.iter_mut().find(|q| q.lit == l).unwrap().init = init;
        }
    }
    out.bads = g.bads.iter().map(|&b| m(&map, b)).collect();
    // Constraints that became true carry no information.
    out.constraints = g
        .constraints
        .iter()
        .map(|&c| m(&map, c))
        .filter(|&c| c != TRUE)
        .collect();
    stats.latches_after = out.latches.len();
    stats.ands_after = out.num_ands();
    stats.inputs_after = out.inputs.len();
    (out, stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neg;

    #[test]
    fn drops_cone_and_constant_latches() {
        let mut g = Aig::new();
        let a = g.input();
        let unused = g.input();
        // `stuck` starts at 0 and stays 0: next = stuck & a.
        let stuck = g.latch(Init::Zero);
        let n = g.and(stuck, a);
        g.set_next(stuck, n);
        // `t` toggles.
        let t = g.latch(Init::Zero);
        g.set_next(t, neg(t));
        // Dead logic.
        let _ = g.and(unused, a);
        let bad = g.or(stuck, t);
        g.bads.push(bad);
        let (s, st) = simplify(&g);
        assert_eq!(st.constant_latches, 1);
        assert_eq!(s.latches.len(), 1);
        assert_eq!(s.inputs.len(), 0, "{}", s.summary());
        assert_eq!(s.num_ands(), 0, "bad is just the toggle latch");
        assert_eq!(s.bads[0], s.latches[0].lit);
    }
}
