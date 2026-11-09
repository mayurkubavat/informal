//! The transition relation as a CNF template with internal variables
//! eliminated (DAG-CNF variable elimination, as rIC3 does).
//!
//! The AIG's AND nodes are Tseitin-encoded once into template clauses.
//! Variables the engines must see stay frozen: constant, inputs, latches,
//! next-state functions, bad and constraint outputs, and non-constant
//! latch inits. Every other variable is eliminated by resolution when that
//! does not grow the clause count (bounded variable elimination). Frames
//! are then instances of the template with renamed variables.
//!
//! TODO(D7, Gemini P7): bound elimination blowup (skip variables with more
//! than 8 positive or negative occurrences, never grow literal count);
//! one CNF stage once needed 10.9 GB.

use informal_aig::{is_neg, var, Aig, Init, Node};
use std::sync::{Arc, Mutex, Weak};

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
pub fn tlit(l: informal_aig::Lit) -> TLit {
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
    build_until(g, &|| false)
}

/// [`build`], ending variable elimination early once `stop` says so: the
/// template is then still exact, only larger (elimination is optional).
/// Elimination is the slow part (37 s on sosylab Problem07).
pub fn build_until(g: &Aig, stop: &dyn Fn() -> bool) -> Template {
    if std::env::var_os("INFORMAL_CNF_PAIRWISE").is_some() {
        build_pairwise(g)
    } else {
        build_dag(g, stop)
    }
}

/// Templates in use, by the address of the AIG they were built for (see
/// [`share`]). Weak: a template goes when its last user drops it.
type SharedEntry = (usize, usize, usize, Weak<Template>);
static SHARED: Mutex<Vec<SharedEntry>> = Mutex::new(Vec::new());

fn aig_key(g: &Aig) -> (usize, usize, usize) {
    (g as *const Aig as usize, g.nodes.len(), g.latches.len())
}

/// Offer `t`, built for `g`, to every later [`template`] call on the same
/// AIG (same address) while `t` is alive: the portfolio's engines share one
/// prepared model, so its template is built once instead of once per
/// engine, IC3 worker and k-induction unrolling.
pub fn share(g: &Aig, t: &Arc<Template>) {
    let (a, n, l) = aig_key(g);
    let mut s = SHARED.lock().unwrap_or_else(|p| p.into_inner());
    s.retain(|e| e.3.strong_count() > 0 && e.0 != a);
    s.push((a, n, l, Arc::downgrade(t)));
}

/// The template of `g`: the shared one when [`share`] offered one for this
/// AIG, else a new build.
pub fn template(g: &Aig) -> Arc<Template> {
    let key = aig_key(g);
    let found = SHARED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .find(|e| (e.0, e.1, e.2) == key)
        .and_then(|e| e.3.upgrade());
    found.unwrap_or_else(|| Arc::new(build(g)))
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
fn build_dag(g: &Aig, stop: &dyn Fn() -> bool) -> Template {
    let n = g.nodes.len() + 1;
    let frozen = frozen_vars(g);
    let max_occ: usize = std::env::var("INFORMAL_BVE_OCC")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(2000);
    let mut clauses: Vec<Option<Vec<TLit>>> = Vec::new();
    // owned[v]: clauses whose highest variable is v (v's definition).
    let mut owned: Vec<Vec<u32>> = vec![Vec::new(); n];
    // uses[lit]: clauses containing lit whose owner is another variable.
    let idx = |l: TLit| -> usize { 2 * l.unsigned_abs() as usize + usize::from(l < 0) };
    let mut uses: Vec<Vec<u32>> = vec![Vec::new(); 2 * n + 2];
    let add = |c: Vec<TLit>,
               clauses: &mut Vec<Option<Vec<TLit>>>,
               owned: &mut Vec<Vec<u32>>,
               uses: &mut Vec<Vec<u32>>| {
        let ci = clauses.len() as u32;
        let o = c.iter().map(|l| l.unsigned_abs()).max().unwrap() as usize;
        owned[o].push(ci);
        for &l in &c {
            if l.unsigned_abs() as usize != o {
                uses[idx(l)].push(ci);
            }
        }
        clauses.push(Some(c));
    };
    // Fanout of every node (AND inputs, next states, inits, outputs).
    let mut fanout = vec![0u32; g.nodes.len()];
    for node in &g.nodes {
        if let Node::And(a, b) = *node {
            fanout[var(a) as usize] += 1;
            fanout[var(b) as usize] += 1;
        }
    }
    for l in &g.latches {
        fanout[var(l.next) as usize] += 1;
        if let Init::Expr(e) = l.init {
            fanout[var(e) as usize] += 1;
        }
    }
    for &b in g.bads.iter().chain(&g.constraints) {
        fanout[var(b) as usize] += 1;
    }
    let mut live = vec![true; n];
    let mut eliminated = 0;
    // Inner ANDs of a recognized gate that nothing else uses: no clauses.
    let mut absorbed = vec![false; g.nodes.len()];
    let compact = std::env::var_os("INFORMAL_CNF_NO_XOR").is_none();
    let mut implied: Vec<[Vec<TLit>; 2]> = Vec::new();
    for v in (0..g.nodes.len()).rev() {
        let Node::And(a, b) = g.nodes[v] else {
            continue;
        };
        if absorbed[v] {
            continue;
        }
        let x = v as i32 + 1;
        if compact {
            if let Some((c, t, e, inner)) = ite_of(g, a, b).filter(|(_, _, _, inner)| {
                inner.iter().all(|&iv| fanout[iv] == 1 && !frozen[iv + 1])
            }) {
                // x = !ite(c, t, e)  <=>  m = ite(c, t, e) with m = -x.
                let (c, t, e, m) = (tlit(c), tlit(t), tlit(e), -x);
                if t == -e {
                    // XOR: m = c ? t : !t  <=>  m = c XNOR t ... encoded directly.
                    for cl in [
                        vec![-c, -t, m],
                        vec![-c, t, -m],
                        vec![c, t, m],
                        vec![c, -t, -m],
                    ] {
                        add(cl, &mut clauses, &mut owned, &mut uses);
                    }
                } else {
                    for cl in [
                        vec![-c, -t, m],
                        vec![-c, t, -m],
                        vec![c, -e, m],
                        vec![c, e, -m],
                    ] {
                        add(cl, &mut clauses, &mut owned, &mut uses);
                    }
                    // Implied (resolve on c), but unit propagation needs
                    // them to set m when t = e: IC3 lost models without.
                    // Added after elimination so they do not block it.
                    implied.push([vec![-t, -e, m], vec![t, e, -m]]);
                }
                for iv in inner {
                    if fanout[iv] == 1 && !frozen[iv + 1] {
                        absorbed[iv] = true;
                        live[iv + 1] = false;
                        eliminated += 1;
                    }
                }
                continue;
            }
        }
        let (a, b) = (tlit(a), tlit(b));
        add(vec![-x, a], &mut clauses, &mut owned, &mut uses);
        add(vec![-x, b], &mut clauses, &mut owned, &mut uses);
        add(vec![x, -a, -b], &mut clauses, &mut owned, &mut uses);
    }
    let mut live_cls = clauses.len();
    for v in 1..n {
        if v % 1024 == 0 && stop() {
            break;
        }
        // Eliminations create and delete many clauses (113M created for
        // 1.4M kept on sosylab Problem101): once dead clauses outnumber
        // live ones 3 to 1, renumber the live ones and rebuild the lists,
        // so memory stays proportional to the live CNF.
        if clauses.len() > 3 * live_cls + (1 << 20) {
            let old = std::mem::take(&mut clauses);
            for o in owned.iter_mut() {
                o.clear();
            }
            for u in uses.iter_mut() {
                u.clear();
            }
            for c in old.into_iter().flatten() {
                add(c, &mut clauses, &mut owned, &mut uses);
            }
            for o in owned.iter_mut() {
                o.shrink_to_fit();
            }
            for u in uses.iter_mut() {
                u.shrink_to_fit();
            }
        }
        if !live[v] {
            continue;
        }
        if frozen[v] {
            continue;
        }
        let alive = |ids: &[u32], cl: &[Option<Vec<TLit>>]| -> Vec<usize> {
            ids.iter()
                .map(|&c| c as usize)
                .filter(|&c| cl[c].is_some())
                .collect()
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
            if clauses[c].take().is_some() {
                live_cls -= 1;
            }
        }
        live_cls += res.len();
        owned[v] = Vec::new();
        uses[idx(v as i32)] = Vec::new();
        uses[idx(-(v as i32))] = Vec::new();
        for r in res {
            add(r, &mut clauses, &mut owned, &mut uses);
        }
        live[v] = false;
        eliminated += 1;
    }
    if std::env::var_os("INFORMAL_CNF_STATS").is_some() {
        let hist_lits: usize = uses.iter().map(|u| u.len()).sum();
        eprintln!(
            "cnf: {} clauses created, {} use entries, {} live",
            clauses.len(),
            hist_lits,
            clauses.iter().filter(|c| c.is_some()).count()
        );
    }
    let mut out: Vec<Vec<TLit>> = clauses.into_iter().flatten().collect();
    for pair in implied {
        for cl in pair {
            if cl.iter().all(|l| live[l.unsigned_abs() as usize]) {
                out.push(cl);
            }
        }
    }
    Template {
        clauses: out,
        live,
        eliminated,
    }
}

/// `!ite(c, t, e)` pattern: an AND of two negated ANDs `(c & t)` and
/// `(!c & e)`. Returns (c, t, e) and the two inner AND variables.
fn ite_of(
    g: &Aig,
    a: informal_aig::Lit,
    b: informal_aig::Lit,
) -> Option<(
    informal_aig::Lit,
    informal_aig::Lit,
    informal_aig::Lit,
    [usize; 2],
)> {
    if !is_neg(a) || !is_neg(b) {
        return None;
    }
    let (Node::And(a0, a1), Node::And(b0, b1)) =
        (g.nodes[var(a) as usize], g.nodes[var(b) as usize])
    else {
        return None;
    };
    let inner = [var(a) as usize, var(b) as usize];
    let neg = informal_aig::neg;
    // Find c in one inner AND and !c in the other.
    for (c, t) in [(a0, a1), (a1, a0)] {
        for (nc, e) in [(b0, b1), (b1, b0)] {
            if nc == neg(c) {
                return Some((c, t, e, inner));
            }
        }
    }
    None
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
/// (INFORMAL_CNF_PAIRWISE).
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
        env("INFORMAL_BVE_OCC", MAX_OCC),
        env("INFORMAL_BVE_CLAUSE", MAX_CLAUSE),
        env("INFORMAL_BVE_PASSES", PASSES),
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
                        if l.unsigned_abs() == var(bad) + 1 {
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

    /// ITE and XOR gates built from three ANDs: the compact encoding
    /// must force `bad == ite(c, t, e)` (and the XOR case) for every input.
    #[test]
    fn ite_and_xor_encodings_keep_meaning() {
        for xor in [false, true] {
            let mut g = Aig::new();
            let (c, t, e0) = (g.input(), g.input(), g.input());
            let e = if xor { informal_aig::neg(t) } else { e0 };
            let ct = g.and(c, t);
            let ce = g.and(informal_aig::neg(c), e);
            let x = g.and(informal_aig::neg(ct), informal_aig::neg(ce));
            let bad = informal_aig::neg(x); // ite(c, t, e)
            g.bads.push(bad);
            let tp = build(&g);
            assert!(!tp.live[var(ct) as usize + 1] && !tp.live[var(ce) as usize + 1]);
            for m in 0..8u32 {
                let vals = [
                    (var(c), m & 1 == 1),
                    (var(t), m & 2 == 2),
                    (var(e0), m & 4 == 4),
                ];
                let val = |l: TLit| -> Option<bool> {
                    let v = l.unsigned_abs() - 1;
                    vals.iter()
                        .find(|(x, _)| *x == v)
                        .map(|(_, b)| *b ^ (l < 0))
                };
                let (cv, tv) = (m & 1 == 1, m & 2 == 2);
                let ev = if xor { !tv } else { m & 4 == 4 };
                let want = if cv { tv } else { ev };
                for xv in [false, true] {
                    let sat = tp.clauses.iter().all(|cl| {
                        cl.iter().any(|&l| {
                            if l.unsigned_abs() == var(x) + 1 {
                                xv ^ (l < 0)
                            } else {
                                val(l).unwrap_or(false)
                            }
                        })
                    });
                    // x = !ite: satisfiable exactly when xv == !want.
                    assert_eq!(sat, xv == !want, "xor={xor} m={m} x={xv}");
                }
            }
        }
    }

    #[test]
    fn a_shared_template_is_reused_until_dropped_and_a_stop_keeps_it_exact() {
        let mut g = Aig::new();
        let (a, b, c) = (g.input(), g.input(), g.input());
        let ab = g.and(a, b);
        let bad = g.and(ab, c);
        g.bads.push(bad);
        let t = Arc::new(build(&g));
        share(&g, &t);
        assert!(Arc::ptr_eq(&template(&g), &t));
        drop(t);
        // Gone with its last user: a new build.
        let fresh = template(&g);
        assert_eq!(fresh.eliminated, 1);
        // Stopped before elimination: nothing eliminated, same models.
        let s = build_until(&g, &|| true);
        assert!(s.eliminated <= fresh.eliminated);
        assert!(s.clauses.len() >= fresh.clauses.len());
    }
}
