//! Independent check of a SAFE verdict: an inductive invariant, carried
//! back to the model as loaded (before simplification and latch merging),
//! checked with three fresh SAT queries.
//!
//! - **Init:** every initial state (that meets the constraints) satisfies
//!   the invariant.
//! - **Consecution:** from a state that satisfies the invariant and the
//!   constraints, every step into a state that meets the constraints
//!   satisfies it again.
//! - **Safety:** no state that satisfies the invariant and the
//!   constraints is bad.
//!
//! The queries use their own plain Tseitin encoding of the loaded AIG, not
//! the engines' CNF template, variable elimination or preprocessing, so a
//! bug there (the old `scorr` and lemma-import bugs were of that kind)
//! makes the check fail instead of passing silently. What is still
//! trusted: the AIGER/Btor2 reader and bit-blaster, this file, and the
//! SAT solver's UNSAT answers.

use informal_aig::preprocess::LatchMap;
use informal_aig::{is_neg, var, Aig, Init, Lit, Node, FALSE, TRUE};
use informal_sat::{Answer, Sat};
use std::collections::HashMap;

/// A clause over AIG literals of the loaded model's latches.
pub type Clause = Vec<Lit>;

/// The invariant over the loaded model `orig`: IC3's `lemmas` (cubes over
/// `prepared`'s latch literals, each blocked: the clause is the cube's
/// negation), plus what preprocessing assumed: latches it found constant
/// and latches it merged (`map`, see [`LatchMap`]).
pub fn lift(
    orig: &Aig,
    map: &LatchMap,
    prepared: &Aig,
    lemmas: &[Vec<Lit>],
) -> Result<Vec<Clause>, String> {
    if map.len() != orig.latches.len() {
        return Err(format!(
            "latch map has {} entries for {} latches",
            map.len(),
            orig.latches.len()
        ));
    }
    let prepared_latch: std::collections::HashSet<u32> =
        prepared.latches.iter().map(|l| var(l.lit)).collect();
    // Prepared latch variable -> the loaded model's literal equal to it.
    let mut rep: HashMap<u32, Lit> = HashMap::new();
    let mut inv = Vec::new();
    for (l, m) in orig.latches.iter().zip(map) {
        let Some(m) = *m else { continue };
        let x = l.lit;
        if m == FALSE {
            inv.push(vec![x ^ 1]);
        } else if m == TRUE {
            inv.push(vec![x]);
        } else {
            if !prepared_latch.contains(&var(m)) {
                return Err(format!("latch map points at non-latch literal {m}"));
            }
            // x == m, so the prepared latch's positive literal equals x ^ neg(m).
            let x = x ^ (m & 1);
            match rep.get(&var(m)) {
                None => {
                    rep.insert(var(m), x);
                }
                Some(&y) => {
                    inv.push(vec![x ^ 1, y]);
                    inv.push(vec![x, y ^ 1]);
                }
            }
        }
    }
    for cube in lemmas {
        let mut c = Vec::with_capacity(cube.len());
        for &l in cube {
            let Some(&y) = rep.get(&var(l)) else {
                return Err(format!(
                    "lemma literal {l} is not a latch of the loaded model"
                ));
            };
            c.push((y ^ (l & 1)) ^ 1);
        }
        inv.push(c);
    }
    Ok(inv)
}

/// SAT literal of every AIG variable in one time frame.
struct Frame(Vec<i32>);

impl Frame {
    fn lit(&self, l: Lit) -> i32 {
        let x = self.0[var(l) as usize];
        if is_neg(l) {
            -x
        } else {
            x
        }
    }
}

struct Enc {
    sat: Box<dyn Sat>,
    next: i32,
}

impl Enc {
    fn new(mut sat: Box<dyn Sat>) -> Self {
        sat.reserve(1);
        sat.add_clause(&[1]); // variable 1 is true
        Enc { sat, next: 2 }
    }
    fn fresh(&mut self) -> i32 {
        self.sat.reserve(1);
        self.next += 1;
        self.next - 1
    }
    /// One frame of `g` over the nodes in `cone` (others stay unencoded);
    /// latch values from `latch` (by latch index) or free.
    fn frame(&mut self, g: &Aig, cone: &[bool], latch: Option<&[i32]>) -> Frame {
        let index: HashMap<u32, usize> = g
            .latches
            .iter()
            .enumerate()
            .map(|(i, l)| (var(l.lit), i))
            .collect();
        let mut f = Frame(vec![0; g.nodes.len()]);
        for (v, n) in g.nodes.iter().enumerate() {
            if !cone[v] {
                continue;
            }
            f.0[v] = match *n {
                Node::Const => -1,
                Node::Input => self.fresh(),
                Node::Latch => match latch {
                    Some(vals) => {
                        let x = vals[index[&(v as u32)]];
                        debug_assert!(x != 0, "next state outside the cone");
                        x
                    }
                    None => self.fresh(),
                },
                Node::And(a, b) => {
                    let (a, b) = (f.lit(a), f.lit(b));
                    let x = self.fresh();
                    self.sat.add_clause(&[-x, a]);
                    self.sat.add_clause(&[-x, b]);
                    self.sat.add_clause(&[x, -a, -b]);
                    x
                }
            };
        }
        f
    }
    fn assert_inv(&mut self, f: &Frame, inv: &[Clause]) {
        for c in inv {
            let c: Vec<i32> = c.iter().map(|&l| f.lit(l)).collect();
            self.sat.add_clause(&c);
        }
    }
    /// Some clause of `inv` is false in `f`; returns the selector to assume.
    fn violate_inv(&mut self, f: &Frame, inv: &[Clause]) -> i32 {
        let any = self.fresh();
        let mut or = vec![-any];
        for c in inv {
            let s = self.fresh();
            for &l in c {
                self.sat.add_clause(&[-s, -f.lit(l)]);
            }
            or.push(s);
        }
        self.sat.add_clause(&or);
        any
    }
    fn constraints(&mut self, g: &Aig, f: &Frame) {
        for &c in &g.constraints {
            let l = f.lit(c);
            self.sat.add_clause(&[l]);
        }
    }
    fn unsat(&mut self, assume: &[i32], what: &str) -> Result<(), String> {
        match self.sat.solve(assume) {
            Answer::Unsat => Ok(()),
            Answer::Sat => Err(format!("{what} fails")),
            Answer::Unknown => Err(format!("{what} check stopped")),
        }
    }
}

/// Nodes the queries can depend on: the cone of the bad and constraint
/// outputs and of the invariant's latches, through latch next states and
/// inits. Everything outside is unconstrained by the queries, so leaving
/// it out changes no answer (the big CPU models have many unrelated
/// properties' logic in one file).
fn cone(g: &Aig, inv: &[Clause]) -> Vec<bool> {
    let index: HashMap<u32, usize> = g
        .latches
        .iter()
        .enumerate()
        .map(|(i, l)| (var(l.lit), i))
        .collect();
    let mut need = vec![false; g.nodes.len()];
    let mut stack: Vec<u32> = g
        .bads
        .iter()
        .chain(&g.constraints)
        .chain(inv.iter().flatten())
        .map(|&l| var(l))
        .collect();
    while let Some(v) = stack.pop() {
        if std::mem::replace(&mut need[v as usize], true) {
            continue;
        }
        match g.nodes[v as usize] {
            Node::And(a, b) => stack.extend([var(a), var(b)]),
            Node::Latch => {
                let l = &g.latches[index[&v]];
                stack.push(var(l.next));
                if let Init::Expr(e) = l.init {
                    stack.push(var(e));
                }
            }
            _ => {}
        }
    }
    need
}

/// Check that `inv` is an inductive invariant of `g` that excludes every
/// bad state. Each query gets a fresh solver from `new_sat`.
pub fn check(g: &Aig, inv: &[Clause], new_sat: &dyn Fn() -> Box<dyn Sat>) -> Result<(), String> {
    let cone = cone(g, inv);
    // Init.
    let mut e = Enc::new(new_sat());
    let f0 = e.frame(g, &cone, None);
    for l in g.latches.iter().filter(|l| cone[var(l.lit) as usize]) {
        let x = f0.lit(l.lit);
        match l.init {
            Init::Zero => e.sat.add_clause(&[-x]),
            Init::One => e.sat.add_clause(&[x]),
            Init::Expr(i) => {
                let y = f0.lit(i);
                e.sat.add_clause(&[-x, y]);
                e.sat.add_clause(&[x, -y]);
            }
            Init::Free => {}
        }
    }
    e.constraints(g, &f0);
    let s = e.violate_inv(&f0, inv);
    e.unsat(&[s], "init")?;

    // Consecution.
    let mut e = Enc::new(new_sat());
    let f0 = e.frame(g, &cone, None);
    e.assert_inv(&f0, inv);
    e.constraints(g, &f0);
    let next: Vec<i32> = g
        .latches
        .iter()
        .map(|l| {
            if cone[var(l.lit) as usize] {
                f0.lit(l.next)
            } else {
                0
            }
        })
        .collect();
    let f1 = e.frame(g, &cone, Some(&next));
    e.constraints(g, &f1);
    let s = e.violate_inv(&f1, inv);
    e.unsat(&[s], "consecution")?;

    // Safety.
    let mut e = Enc::new(new_sat());
    let f0 = e.frame(g, &cone, None);
    e.assert_inv(&f0, inv);
    e.constraints(g, &f0);
    let bads: Vec<i32> = g.bads.iter().map(|&b| f0.lit(b)).collect();
    let any = e.fresh();
    let mut or = vec![-any];
    or.extend(&bads);
    e.sat.add_clause(&or);
    e.unsat(&[any], "safety")
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_aig::neg;

    fn sat() -> Box<dyn Sat> {
        informal_sat::best()
    }

    /// Two latches that always toggle together from 0; bad = they differ.
    fn twins() -> Aig {
        let mut g = Aig::new();
        let a = g.latch(Init::Zero);
        let b = g.latch(Init::Zero);
        g.set_next(a, neg(a));
        g.set_next(b, neg(b));
        let bad = g.xor(a, b);
        g.bads.push(bad);
        g
    }

    #[test]
    fn accepts_an_inductive_invariant_and_rejects_wrong_ones() {
        let g = twins();
        let (a, b) = (g.latches[0].lit, g.latches[1].lit);
        // a == b.
        let good = vec![vec![neg(a), b], vec![a, neg(b)]];
        assert_eq!(check(&g, &good, &sat), Ok(()));
        // Not inductive: a is false holds initially but not after a step.
        assert_eq!(
            check(&g, &[vec![neg(a)], vec![neg(b)]], &sat),
            Err("consecution fails".into())
        );
        // Does not hold initially.
        assert_eq!(check(&g, &[vec![a]], &sat), Err("init fails".into()));
        // Inductive (true) but too weak to exclude the bad states.
        assert_eq!(check(&g, &[], &sat), Err("safety fails".into()));
    }

    #[test]
    fn lifts_merged_and_constant_latches_back() {
        let g = twins();
        // Preprocessing merged b into a, and IC3 needed no lemma: the
        // invariant is the merge itself.
        let mut p = Aig::new();
        let pa = p.latch(Init::Zero);
        p.set_next(pa, neg(pa));
        let map = vec![Some(pa), Some(pa)];
        let inv = lift(&g, &map, &p, &[]).unwrap();
        assert_eq!(check(&g, &inv, &sat), Ok(()));
        // A wrong merge (b == !a) is caught on the loaded model.
        let wrong = vec![Some(pa), Some(neg(pa))];
        let inv = lift(&g, &wrong, &p, &[]).unwrap();
        assert_eq!(check(&g, &inv, &sat), Err("init fails".into()));
        // A latch claimed constant that is not.
        let inv = lift(&g, &vec![Some(FALSE), None], &p, &[]).unwrap();
        assert_eq!(check(&g, &inv, &sat), Err("consecution fails".into()));
        // Lemma cubes are blocked: cube {pa} means clause !a.
        let inv = lift(&g, &vec![Some(pa), None], &p, &[vec![pa]]).unwrap();
        assert_eq!(inv, vec![vec![neg(g.latches[0].lit)]]);
    }
}
