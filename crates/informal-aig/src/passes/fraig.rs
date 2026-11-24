//! Simulation-guided SAT sweeping (FRAIG — Mishchenko et al. DAC'05).
//!
//! Partitions AIG nodes into candidate equivalence classes using 256-bit random
//! parallel simulation, verifies candidate pairs in topological order with an
//! incremental SAT solver, refines classes immediately on every SAT
//! counterexample, and merges proven equivalent nodes through structural hashing.

use crate::passes::rebuild_with_subst;
use crate::preprocess::{InputMap, LatchMap};
use crate::{is_neg, neg, var, Aig, Lit, Node, FALSE};
use informal_sat::{Answer, Sat};
use std::collections::HashMap;

const SIM_WORDS: usize = 4; // 256 random parallel simulation vectors

/// Run simulation-guided SAT sweeping (FRAIG) on `g`, performing at most
/// `max_checks` incremental SAT equivalence checks.
/// Returns `(fraiged_aig, latch_map, input_map, merged_count)`.
pub fn fraig(g: &Aig, max_checks: usize) -> (Aig, LatchMap, InputMap, usize) {
    let n = g.nodes.len();
    if n <= 2 || max_checks == 0 {
        let (s, _, lm, im) = crate::preprocess::simplify_maps(g);
        return (s, lm, im, 0);
    }

    // 1. 256-bit parallel random simulation over inputs + latches.
    let mut sigs: Vec<[u64; SIM_WORDS]> = vec![[0; SIM_WORDS]; n];
    let mut rng = 0x5EED_CAFE_BABE_1337u64;
    let mut next_u64 = || -> u64 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };

    for (v, node) in g.nodes.iter().enumerate() {
        sigs[v] = match *node {
            Node::Const => [0; SIM_WORDS],
            Node::Input | Node::Latch => [next_u64(), next_u64(), next_u64(), next_u64()],
            Node::And(a, b) => {
                let sa = sigs[var(a) as usize];
                let sb = sigs[var(b) as usize];
                let na = if is_neg(a) { !0u64 } else { 0u64 };
                let nb = if is_neg(b) { !0u64 } else { 0u64 };
                let mut out = [0u64; SIM_WORDS];
                for w in 0..SIM_WORDS {
                    out[w] = (sa[w] ^ na) & (sb[w] ^ nb);
                }
                out
            }
        };
    }

    // 2. Group nodes by canonicalized signature `min(sig, !sig)` so complemented
    //    equivalences (`u == !v`) land in the same bucket.
    let canon = |s: [u64; SIM_WORDS]| -> ([u64; SIM_WORDS], bool) {
        if (s[0] & 1) != 0 {
            ([!s[0], !s[1], !s[2], !s[3]], true)
        } else {
            (s, false)
        }
    };

    let mut buckets: HashMap<[u64; SIM_WORDS], Vec<(usize, bool)>> = HashMap::new();
    for (v, &s) in sigs.iter().enumerate() {
        let (key, inv) = canon(s);
        buckets.entry(key).or_default().push((v, inv));
    }

    // Only build the SAT solver if at least one non-trivial equivalence bucket exists.
    let has_candidates = buckets.values().any(|b| b.len() > 1);
    if !has_candidates {
        let (s, _, lm, im) = crate::preprocess::simplify_maps(g);
        return (s, lm, im, 0);
    }

    // 3. Encode combinational AIG into one incremental SAT solver.
    let mut sat = informal_sat::best();
    let tru = 1i32;
    sat.reserve(n as i32 + 2);
    sat.add_clause(&[tru]);
    let mut sat_var = vec![0i32; n];
    let mut nv = 2i32;
    for (v, node) in g.nodes.iter().enumerate() {
        sat_var[v] = match *node {
            Node::Const => -tru,
            Node::Input | Node::Latch => {
                let x = nv;
                nv += 1;
                x
            }
            Node::And(a, b) => {
                let out = nv;
                nv += 1;
                let la = sat_var[var(a) as usize] * if is_neg(a) { -1 } else { 1 };
                let lb = sat_var[var(b) as usize] * if is_neg(b) { -1 } else { 1 };
                sat.add_clause(&[-out, la]);
                sat.add_clause(&[-out, lb]);
                sat.add_clause(&[out, -la, -lb]);
                out
            }
        };
    }

    // Extra CEX simulation bits appended dynamically to split false candidate pairs!
    let mut cex_bits: Vec<u64> = vec![0u64; n];
    let mut cex_count: u32 = 0;

    let mut subst: Vec<Lit> = (0..n as u32).map(|v| 2 * v).collect();
    let mut checks = 0usize;
    let mut merged = 0usize;

    // Sort buckets by earliest representative for deterministic topological merging.
    let mut class_list: Vec<Vec<(usize, bool)>> =
        buckets.into_values().filter(|b| b.len() > 1).collect();
    class_list.sort_by_key(|b| b[0].0);

    for members in class_list {
        if checks >= max_checks {
            break;
        }
        // Maintain confirmed representatives within this bucket.
        let mut reps: Vec<(usize, bool)> = Vec::new();
        for (v, inv_v) in members {
            if !matches!(g.nodes[v], Node::And(..)) {
                reps.push((v, inv_v));
                continue;
            }
            let mut matched = None;
            for &(r, inv_r) in &reps {
                if checks >= max_checks {
                    break;
                }
                let flip = inv_v ^ inv_r;
                // Quick reject via accumulated CEX simulation bits!
                if cex_count > 0 {
                    let mask = if cex_count >= 64 {
                        !0u64
                    } else {
                        (1u64 << cex_count) - 1
                    };
                    let want = if flip {
                        (!cex_bits[r]) & mask
                    } else {
                        cex_bits[r] & mask
                    };
                    if (cex_bits[v] & mask) != want {
                        continue;
                    }
                }

                // Check `v != (r ^ flip)` via 2 incremental SAT queries.
                checks += 1;
                let lv = sat_var[v];
                let lr = if flip { -sat_var[r] } else { sat_var[r] };
                let eq = match sat.solve(&[lv, -lr]) {
                    Answer::Sat => {
                        record_cex(g, &*sat, &sat_var, &mut cex_bits, &mut cex_count);
                        false
                    }
                    Answer::Unsat => match sat.solve(&[-lv, lr]) {
                        Answer::Sat => {
                            record_cex(g, &*sat, &sat_var, &mut cex_bits, &mut cex_count);
                            false
                        }
                        Answer::Unsat => true,
                        Answer::Unknown => false,
                    },
                    Answer::Unknown => false,
                };
                if eq {
                    // Add learned equivalence clauses to accelerate subsequent SAT checks.
                    sat.add_clause(&[-lv, lr]);
                    sat.add_clause(&[lv, -lr]);
                    let rep_lit = if r == 0 {
                        if flip {
                            crate::TRUE
                        } else {
                            FALSE
                        }
                    } else {
                        let base = subst[r];
                        if flip {
                            neg(base)
                        } else {
                            base
                        }
                    };
                    matched = Some(rep_lit);
                    break;
                }
            }
            if let Some(rep_lit) = matched {
                subst[v] = rep_lit;
                merged += 1;
            } else {
                reps.push((v, inv_v));
            }
        }
    }

    if merged == 0 {
        let (s, _, lm, im) = crate::preprocess::simplify_maps(g);
        return (s, lm, im, 0);
    }
    let (out, lm, im) = rebuild_with_subst(g, &subst);
    (out, lm, im, merged)
}

fn record_cex(g: &Aig, sat: &dyn Sat, sat_var: &[i32], cex_bits: &mut [u64], cex_count: &mut u32) {
    if *cex_count >= 64 {
        return;
    }
    let bit = 1u64 << *cex_count;
    *cex_count += 1;
    let mut vals = vec![false; g.nodes.len()];
    for (v, node) in g.nodes.iter().enumerate() {
        vals[v] = match *node {
            Node::Const => false,
            Node::Input | Node::Latch => sat.value(sat_var[v]),
            Node::And(a, b) => {
                (vals[var(a) as usize] ^ is_neg(a)) && (vals[var(b) as usize] ^ is_neg(b))
            }
        };
        if vals[v] {
            cex_bits[v] |= bit;
        }
    }
}
