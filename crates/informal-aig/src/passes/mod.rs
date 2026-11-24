//! Composable AIG simplification and abstraction passes.
//!
//! Every pass preserves model semantics and composes [`LatchMap`] / [`InputMap`]
//! so downstream certification (`certify::lift` / `PreparedTrace::lift`) remains
//! sound across any pipeline sequence:
//!
//! - [`rewrite`]: DAG-aware 4-input cut rewriting (truth-table decomposition).
//! - [`fraig`]: simulation-guided SAT sweeping (FRAIG) with CEX signature refinement.
//! - [`temp_decomp`]: transient reset / initialization shift-register analysis.
//! - [`phase_abs`]: periodic clock / phase-abstraction detection and folding.

pub mod fraig;
pub mod phase_abs;
pub mod rewrite;
pub mod temp_decomp;

use crate::preprocess::{self as pre, InputMap, LatchMap};
use crate::{Aig, Lit};

/// Configuration selecting which passes run in [`run_pipeline`].
#[derive(Debug, Clone)]
pub struct PassConfig {
    pub rewrite: bool,
    pub fraig: bool,
    pub temp_decomp: bool,
    pub phase_abs: bool,
    /// Maximum SAT equivalence checks during FRAIG.
    pub max_fraig_checks: usize,
}

impl Default for PassConfig {
    fn default() -> Self {
        Self {
            rewrite: true,
            fraig: true,
            temp_decomp: true,
            phase_abs: false,
            max_fraig_checks: 2_000,
        }
    }
}

/// Summary metrics reported by [`run_pipeline`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PassReport {
    pub ands_before: usize,
    pub ands_after: usize,
    pub latches_before: usize,
    pub latches_after: usize,
    pub rewritten_cuts: usize,
    pub fraig_merged: usize,
    pub transient_depth: usize,
    pub phase_period: Option<usize>,
    pub phase_folded: bool,
}

/// Rebuild `g` applying a combinational literal substitution `subst[var]`
/// (for AND nodes), preserving latches and inputs verbatim, then run
/// `simplify_maps` to drop dead nodes and produce exact back-maps.
pub(crate) fn rebuild_with_subst(g: &Aig, subst: &[Lit]) -> (Aig, LatchMap, InputMap) {
    let mut out = Aig::new();
    let mut map = vec![0u32; g.nodes.len()];
    for (v, n) in g.nodes.iter().enumerate() {
        let mapped = match *n {
            crate::Node::Const => crate::FALSE,
            crate::Node::Input => {
                let l = out.input();
                if let Some(name) = g.names.get(&(v as u32)) {
                    out.names.insert(crate::var(l), name.clone());
                }
                l
            }
            crate::Node::Latch => {
                let li = g
                    .latches
                    .iter()
                    .position(|x| crate::var(x.lit) == v as u32)
                    .expect("latch index");
                let init = match g.latches[li].init {
                    crate::Init::Expr(_) => crate::Init::Free, // patched after loop
                    other => other,
                };
                let l = out.latch(init);
                if let Some(name) = g.names.get(&(v as u32)) {
                    out.names.insert(crate::var(l), name.clone());
                }
                l
            }
            crate::Node::And(a, b) => {
                let s = subst[v];
                if crate::var(s) != v as u32 {
                    let base = map[crate::var(s) as usize];
                    if crate::is_neg(s) {
                        crate::neg(base)
                    } else {
                        base
                    }
                } else {
                    let ma = map[crate::var(a) as usize] ^ (a & 1);
                    let mb = map[crate::var(b) as usize] ^ (b & 1);
                    out.and(ma, mb)
                }
            }
        };
        map[v] = mapped;
    }
    let xl = |l: Lit| map[crate::var(l) as usize] ^ (l & 1);
    for (i, l) in g.latches.iter().enumerate() {
        let out_lit = out.latches[i].lit;
        out.set_next(out_lit, xl(l.next));
        if let crate::Init::Expr(e) = l.init {
            out.latches[i].init = crate::Init::Expr(xl(e));
        }
    }
    out.bads = g.bads.iter().map(|&b| xl(b)).collect();
    out.constraints = g.constraints.iter().map(|&c| xl(c)).collect();
    out.fairness = g.fairness.iter().map(|&f| xl(f)).collect();
    out.justice = g
        .justice
        .iter()
        .map(|j| j.iter().map(|&l| xl(l)).collect())
        .collect();
    let (simp, _, lmap, imap) = pre::simplify_maps(&out);
    (simp, lmap, imap)
}

/// Run the configured pass pipeline on `g`, composing [`LatchMap`] and
/// [`InputMap`] across every enabled pass.
pub fn run_pipeline(g: &Aig, cfg: &PassConfig) -> (Aig, LatchMap, InputMap, PassReport) {
    let mut rep = PassReport {
        ands_before: g.num_ands(),
        latches_before: g.latches.len(),
        ..Default::default()
    };

    let (mut cur, _, mut lmap, mut imap) = pre::simplify_maps(g);

    if cfg.rewrite {
        let (next, lm, im, count) = rewrite::rewrite(&cur);
        lmap = pre::compose(&lmap, &cur, &lm);
        imap = pre::compose_inputs(&imap, &cur, &im);
        cur = next;
        rep.rewritten_cuts = count;
    }

    if cfg.fraig {
        let (next, lm, im, merged) = fraig::fraig(&cur, cfg.max_fraig_checks);
        lmap = pre::compose(&lmap, &cur, &lm);
        imap = pre::compose_inputs(&imap, &cur, &im);
        cur = next;
        rep.fraig_merged = merged;
    }

    if cfg.temp_decomp {
        let info = temp_decomp::analyze_transients(&cur);
        rep.transient_depth = info.transient_depth;
        if let Some((next, lm, im)) = temp_decomp::simplify_transient_constants(&cur, &info) {
            lmap = pre::compose(&lmap, &cur, &lm);
            imap = pre::compose_inputs(&imap, &cur, &im);
            cur = next;
        }
    }

    rep.phase_period = phase_abs::detect_phase_period(&cur);
    if cfg.phase_abs {
        if let Some((next, lm, im)) = phase_abs::fold_phase(&cur) {
            lmap = pre::compose(&lmap, &cur, &lm);
            imap = pre::compose_inputs(&imap, &cur, &im);
            cur = next;
            rep.phase_folded = true;
        }
    }

    rep.ands_after = cur.num_ands();
    rep.latches_after = cur.latches.len();
    (cur, lmap, imap, rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{neg, Init};
    use informal_sat::{Answer, Sat};

    /// Verify combinational equivalence of two AIGs with identical input/latch
    /// order via SAT miter over `bads` and latch `next` functions.
    fn assert_comb_equiv(a: &Aig, b: &Aig) {
        assert_eq!(a.inputs.len(), b.inputs.len());
        assert_eq!(a.latches.len(), b.latches.len());
        assert_eq!(a.bads.len(), b.bads.len());

        let mut sat = informal_sat::best();
        let tru = 1i32;
        sat.reserve(1);
        sat.add_clause(&[tru]);
        let mut nv = 2i32;

        let pi: Vec<i32> = (0..a.inputs.len())
            .map(|_| {
                let x = nv;
                nv += 1;
                x
            })
            .collect();
        let pl: Vec<i32> = (0..a.latches.len())
            .map(|_| {
                let x = nv;
                nv += 1;
                x
            })
            .collect();

        let encode = |g: &Aig, sat: &mut dyn Sat, nv: &mut i32| -> Vec<i32> {
            let mut m = vec![0i32; g.nodes.len()];
            let (mut ii, mut li) = (0, 0);
            for (v, n) in g.nodes.iter().enumerate() {
                m[v] = match *n {
                    crate::Node::Const => -tru,
                    crate::Node::Input => {
                        let x = pi[ii];
                        ii += 1;
                        x
                    }
                    crate::Node::Latch => {
                        let x = pl[li];
                        li += 1;
                        x
                    }
                    crate::Node::And(x, y) => {
                        let out = *nv;
                        *nv += 1;
                        let sx = m[crate::var(x) as usize] * if crate::is_neg(x) { -1 } else { 1 };
                        let sy = m[crate::var(y) as usize] * if crate::is_neg(y) { -1 } else { 1 };
                        sat.reserve(1);
                        sat.add_clause(&[-out, sx]);
                        sat.add_clause(&[-out, sy]);
                        sat.add_clause(&[out, -sx, -sy]);
                        out
                    }
                };
            }
            m
        };
        sat.reserve(nv);
        let ma = encode(a, &mut *sat, &mut nv);
        let mb = encode(b, &mut *sat, &mut nv);
        let lit =
            |m: &[i32], l: Lit| m[crate::var(l) as usize] * if crate::is_neg(l) { -1 } else { 1 };

        for (&ba, &bb) in a.bads.iter().zip(&b.bads) {
            let la = lit(&ma, ba);
            let lb = lit(&mb, bb);
            assert_eq!(sat.solve(&[la, -lb]), Answer::Unsat);
            assert_eq!(sat.solve(&[-la, lb]), Answer::Unsat);
        }
        for (la_l, lb_l) in a.latches.iter().zip(&b.latches) {
            let na = lit(&ma, la_l.next);
            let nb = lit(&mb, lb_l.next);
            assert_eq!(sat.solve(&[na, -nb]), Answer::Unsat);
            assert_eq!(sat.solve(&[-na, nb]), Answer::Unsat);
        }
    }

    #[test]
    fn rewrite_and_fraig_reduce_redundant_logic_and_preserve_equivalence() {
        // Build redundant circuit:
        //   f1 = (x & y) | (x & !y)  (semantically == x, takes 3 ANDs!)
        //   f2 = (x ^ z) ^ z         (semantically == x, takes 6 ANDs!)
        //   bad = f1 ^ f2            (semantically == FALSE!)
        let mut g = Aig::new();
        let x = g.input();
        let y = g.input();
        let z = g.input();
        let a1 = g.and(x, y);
        let a2 = g.and(x, neg(y));
        let f1 = g.or(a1, a2);
        let xz = g.xor(x, z);
        let f2 = g.xor(xz, z);
        let diff = g.xor(f1, f2);
        g.bads.push(diff);

        let before = g.num_ands();
        assert!(before >= 8);

        let (rw, _, _, rw_cuts) = rewrite::rewrite(&g);
        assert!(rw_cuts > 0 || rw.num_ands() < before);
        assert_eq!(rw.bads[0], crate::FALSE);

        // FRAIG directly on the un-rewritten circuit also collapses `f1 == f2` -> `diff == FALSE`!
        let (fr, _, _, merged) = fraig::fraig(&g, 500);
        assert!(merged > 0);
        assert_eq!(fr.bads[0], crate::FALSE);

        // Non-trivial MUX + adder equivalence check across full pipeline:
        let mut g2 = Aig::new();
        let s = g2.latch(Init::Zero);
        let i0 = g2.input();
        let i1 = g2.input();
        // Redundant double-MUX: ite(i0, ite(i0, s, i1), i1) == ite(i0, s, i1)
        let m1 = g2.ite(i0, s, i1);
        let m2 = g2.ite(i0, m1, i1);
        g2.set_next(s, m2);
        let b = g2.and(m2, neg(i1));
        g2.bads.push(b);

        let (opt, _, _, rep) = run_pipeline(&g2, &PassConfig::default());
        assert!(rep.ands_after <= rep.ands_before);
        assert_comb_equiv(&g2, &opt);
    }

    #[test]
    fn temp_decomp_and_phase_abs_detect_transients_and_clocks() {
        // 3-stage reset shift register: r0(1->0) -> r1(1->r0) -> r2(1->r1)
        let mut g = Aig::new();
        let r0 = g.latch(Init::One);
        let r1 = g.latch(Init::One);
        let r2 = g.latch(Init::One);
        let x = g.input();
        g.set_next(r0, crate::FALSE);
        g.set_next(r1, r0);
        g.set_next(r2, r1);
        let bad = g.and(r2, x);
        g.bads.push(bad);

        let info = temp_decomp::analyze_transients(&g);
        assert_eq!(info.transient_depth, 3);
        assert_eq!(info.steady_const[0], Some(false));
        assert_eq!(info.steady_const[1], Some(false));
        assert_eq!(info.steady_const[2], Some(false));

        // Period-2 free-running clock latch: clk(0 -> !clk)
        let mut gc = Aig::new();
        let clk = gc.latch(Init::Zero);
        let d = gc.latch(Init::Zero);
        let en = gc.input();
        gc.set_next(clk, neg(clk));
        let nxt_d = gc.ite(clk, en, d);
        gc.set_next(d, nxt_d);
        gc.bads.push(d);

        assert_eq!(phase_abs::detect_phase_period(&gc), Some(2));
        let (folded, _, _, rep) = run_pipeline(
            &gc,
            &PassConfig {
                phase_abs: true,
                ..Default::default()
            },
        );
        assert!(rep.phase_folded);
        assert!(folded.latches.len() <= gc.latches.len());
    }
}
