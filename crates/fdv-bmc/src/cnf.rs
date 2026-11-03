//! The transition relation as a CNF template with internal variables
//! eliminated (DAG-CNF variable elimination, as rIC3 does).
//!
//! The AIG's AND nodes are Tseitin-encoded once into template clauses.
//! Variables the engines must see stay frozen: constant, inputs, latches,
//! next-state functions, bad and constraint outputs, and non-constant
//! latch inits. Every other variable is eliminated by resolution when that
//! does not grow the clause count (bounded variable elimination). Frames
//! are then instances of the template with renamed variables.

use fdv_aig::{is_neg, var, Aig, Init, Node};

/// Template literal: `+v` / `-v` over template variables 1.. (1 is the
/// AIG's constant-false variable 0, shifted by one).
pub type TLit = i32;

pub struct Template {
    pub clauses: Vec<Vec<TLit>>,
    /// Template variables still present (frozen or not eliminated).
    pub live: Vec<bool>,
    pub eliminated: usize,
}

/// Template literal of AIG literal `l`.
pub fn tlit(l: fdv_aig::Lit) -> TLit {
    let v = var(l) as i32 + 1;
    if is_neg(l) {
        -v
    } else {
        v
    }
}

const MAX_CLAUSE: usize = 24;
const MAX_OCC: usize = 16;

pub fn build(g: &Aig) -> Template {
    let n = g.nodes.len() + 1;
    let mut frozen = vec![false; n];
    frozen[1] = true;
    for (v, node) in g.nodes.iter().enumerate() {
        if matches!(node, Node::Input | Node::Latch) {
            frozen[v + 1] = true;
        }
    }
    for l in &g.latches {
        frozen[var(l.next) as usize + 1] = true;
        if let Init::Expr(e) = l.init {
            frozen[var(e) as usize + 1] = true;
        }
    }
    for &b in g.bads.iter().chain(&g.constraints) {
        frozen[var(b) as usize + 1] = true;
    }

    let mut clauses: Vec<Option<Vec<TLit>>> = Vec::new();
    for (v, node) in g.nodes.iter().enumerate() {
        if let Node::And(a, b) = *node {
            let (x, a, b) = (v as i32 + 1, tlit(a), tlit(b));
            clauses.push(Some(vec![-x, a]));
            clauses.push(Some(vec![-x, b]));
            clauses.push(Some(vec![x, -a, -b]));
        }
    }
    // Occurrence lists by literal.
    let idx = |l: TLit| -> usize { 2 * l.unsigned_abs() as usize + usize::from(l < 0) };
    let mut occ: Vec<Vec<usize>> = vec![Vec::new(); 2 * n + 2];
    for (ci, c) in clauses.iter().enumerate() {
        for &l in c.as_ref().unwrap() {
            occ[idx(l)].push(ci);
        }
    }
    let mut live = vec![true; n];
    let mut eliminated = 0;
    // Eliminate from the outputs toward the inputs (reverse topological).
    for v in (1..n).rev() {
        if frozen[v] || !live[v] {
            continue;
        }
        let pos: Vec<usize> = occ[idx(v as i32)]
            .iter()
            .copied()
            .filter(|&c| clauses[c].is_some())
            .collect();
        let negs: Vec<usize> = occ[idx(-(v as i32))]
            .iter()
            .copied()
            .filter(|&c| clauses[c].is_some())
            .collect();
        if pos.len() > MAX_OCC || negs.len() > MAX_OCC {
            continue;
        }
        let mut resolvents = Vec::new();
        let mut ok = true;
        'outer: for &p in &pos {
            for &q in &negs {
                if let Some(r) = resolve(
                    clauses[p].as_ref().unwrap(),
                    clauses[q].as_ref().unwrap(),
                    v as i32,
                ) {
                    if r.len() > MAX_CLAUSE {
                        ok = false;
                        break 'outer;
                    }
                    resolvents.push(r);
                    if resolvents.len() > pos.len() + negs.len() {
                        ok = false;
                        break 'outer;
                    }
                }
            }
        }
        if !ok {
            continue;
        }
        for &c in pos.iter().chain(&negs) {
            clauses[c] = None;
        }
        for r in resolvents {
            let ci = clauses.len();
            for &l in &r {
                occ[idx(l)].push(ci);
            }
            clauses.push(Some(r));
        }
        live[v] = false;
        eliminated += 1;
    }
    Template {
        clauses: clauses.into_iter().flatten().collect(),
        live,
        eliminated,
    }
}

/// Resolvent of `a` (with `v`) and `b` (with `-v`), or `None` if tautological.
fn resolve(a: &[TLit], b: &[TLit], v: TLit) -> Option<Vec<TLit>> {
    let mut r: Vec<TLit> = a.iter().copied().filter(|&l| l != v).collect();
    for &l in b {
        if l == -v {
            continue;
        }
        if r.contains(&-l) {
            return None;
        }
        if !r.contains(&l) {
            r.push(l);
        }
    }
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eliminates_internal_and_keeps_meaning() {
        // bad = (a & b) & c: the inner AND is internal and goes away.
        let mut g = Aig::new();
        let (a, b, c) = (g.input(), g.input(), g.input());
        let ab = g.and(a, b);
        let bad = g.and(ab, c);
        g.bads.push(bad);
        let t = build(&g);
        assert_eq!(t.eliminated, 1);
        assert!(!t.live[var(ab) as usize + 1]);
        // Check equivalence by enumeration: for every input assignment the
        // clauses force bad == a & b & c.
        for m in 0..8 {
            let val = |l: TLit| -> Option<bool> {
                let v = l.unsigned_abs() as i32 - 1;
                let x = match v {
                    v if v == var(a) as i32 => Some(m & 1 == 1),
                    v if v == var(b) as i32 => Some(m & 2 == 2),
                    v if v == var(c) as i32 => Some(m & 4 == 4),
                    _ => None,
                };
                x.map(|x| x ^ (l < 0))
            };
            for bv in [false, true] {
                let sat = t.clauses.iter().all(|cl| {
                    cl.iter().any(|&l| {
                        if l.unsigned_abs() as u32 == var(bad) + 1 {
                            bv ^ (l < 0)
                        } else {
                            val(l).unwrap_or(false)
                        }
                    })
                });
                assert_eq!(sat, bv == (m == 7), "m={m} bad={bv}");
            }
        }
    }
}
