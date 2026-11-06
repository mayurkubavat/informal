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
const PASSES: usize = 1;

pub fn build(g: &Aig) -> Template {
    if std::env::var_os("FDV_CNF_PAIRWISE").is_some() {
        build_pairwise(g)
    } else {
        build_dag(g)
    }
}

/// Variables the engines must see: constant, inputs, latches, next-state
/// functions, non-constant inits, bads and constraints.
fn frozen_vars(g: &Aig) -> Vec<bool> {
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
    frozen
}

/// Gate-based elimination on the DAG-CNF (as rIC3/logicrs do): every
/// clause belongs to the gate of its highest variable, and a variable is
/// eliminated by resolving only its own definition clauses against the
/// clauses that use it (Een and Biere's gate rule: definition-definition
/// and use-use resolvents are redundant). Variables go inputs-to-outputs,
/// so a gate's definition is already rewritten over surviving variables
/// when it is eliminated; it stays a definition of that gate. Accepted
/// when the clause count does not grow; resolvents are subsumption-reduced.
fn build_dag(g: &Aig) -> Template {
    let n = g.nodes.len() + 1;
    let frozen = frozen_vars(g);
    let max_occ: usize = std::env::var("FDV_BVE_OCC")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(2000);
    let mut clauses: Vec<Option<Vec<TLit>>> = Vec::new();
    // owned[v]: clauses whose highest variable is v (v's definition).
    let mut owned: Vec<Vec<usize>> = vec![Vec::new(); n];
    // uses[lit]: clauses containing lit whose owner is another variable.
    let idx = |l: TLit| -> usize { 2 * l.unsigned_abs() as usize + usize::from(l < 0) };
    let mut uses: Vec<Vec<usize>> = vec![Vec::new(); 2 * n + 2];
    let add = |c: Vec<TLit>,
               clauses: &mut Vec<Option<Vec<TLit>>>,
               owned: &mut Vec<Vec<usize>>,
               uses: &mut Vec<Vec<usize>>| {
        let ci = clauses.len();
        let o = c.iter().map(|l| l.unsigned_abs()).max().unwrap() as usize;
        owned[o].push(ci);
        for &l in &c {
            if l.unsigned_abs() as usize != o {
                uses[idx(l)].push(ci);
            }
        }
        clauses.push(Some(c));
    };
    for (v, node) in g.nodes.iter().enumerate() {
        if let Node::And(a, b) = *node {
            let (x, a, b) = (v as i32 + 1, tlit(a), tlit(b));
            add(vec![-x, a], &mut clauses, &mut owned, &mut uses);
            add(vec![-x, b], &mut clauses, &mut owned, &mut uses);
            add(vec![x, -a, -b], &mut clauses, &mut owned, &mut uses);
        }
    }
    let mut live = vec![true; n];
    let mut eliminated = 0;
    for v in 1..n {
        if frozen[v] {
            continue;
        }
        let alive = |ids: &[usize], cl: &[Option<Vec<TLit>>]| -> Vec<usize> {
            ids.iter().copied().filter(|&c| cl[c].is_some()).collect()
        };
        let def = alive(&owned[v], &clauses);
        let up = alive(&uses[idx(v as i32)], &clauses);
        let un = alive(&uses[idx(-(v as i32))], &clauses);
        let cost = def.len() + up.len() + un.len();
        if def.is_empty() || cost > max_occ {
            continue;
        }
        let (dp, dn): (Vec<usize>, Vec<usize>) = def
            .iter()
            .partition(|&&c| clauses[c].as_ref().unwrap().contains(&(v as i32)));
        let mut res: Vec<Vec<TLit>> = Vec::new();
        let mut ok = true;
        // `resolve(a, b, v)` wants `v` in `a` and `-v` in `b`.
        'outer: for (ps, ns) in [(&dp, &un), (&up, &dn)] {
            for &p in ps.iter() {
                for &q in ns.iter() {
                    if let Some(r) = resolve(
                        clauses[p].as_ref().unwrap(),
                        clauses[q].as_ref().unwrap(),
                        v as i32,
                    ) {
                        res.push(r);
                        if res.len() > cost {
                            ok = false;
                            break 'outer;
                        }
                    }
                }
            }
        }
        if !ok {
            continue;
        }
        let res = subsume(res);
        for &c in def.iter().chain(&up).chain(&un) {
            clauses[c] = None;
        }
        for r in res {
            add(r, &mut clauses, &mut owned, &mut uses);
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

/// Drop duplicate and subsumed clauses (small sets: quadratic).
fn subsume(mut cs: Vec<Vec<TLit>>) -> Vec<Vec<TLit>> {
    for c in &mut cs {
        c.sort_unstable();
    }
    cs.sort_by_key(|c| c.len());
    let mut out: Vec<Vec<TLit>> = Vec::with_capacity(cs.len());
    for c in cs {
        let sub = out
            .iter()
            .any(|o| o.iter().all(|l| c.binary_search(l).is_ok()));
        if !sub {
            out.push(c);
        }
    }
    out
}

/// The original elimination: full pairwise resolution in reverse order
/// (FDV_CNF_PAIRWISE).
fn build_pairwise(g: &Aig) -> Template {
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
    let env = |k: &str, d: usize| -> usize {
        std::env::var(k)
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(d)
    };
    let (max_occ, max_clause, passes) = (
        env("FDV_BVE_OCC", MAX_OCC),
        env("FDV_BVE_CLAUSE", MAX_CLAUSE),
        env("FDV_BVE_PASSES", PASSES),
    );
    // Eliminate from the outputs toward the inputs (reverse topological),
    // repeating while a pass still eliminates something: an elimination
    // can bring a neighbour under the limits.
    for _pass in 0..passes {
        let before = eliminated;
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
            if pos.len() > max_occ || negs.len() > max_occ {
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
                        if r.len() > max_clause {
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
        if eliminated == before {
            break;
        }
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
