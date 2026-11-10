//! Two-literal invariant mining (hypothesis H1 in docs/ic3-research.md).
//!
//! On the CPU-style models IC3 struggles with, most of its lemmas are
//! two-latch relations ("never both on", "a implies b") that only hold
//! together, and IC3 finds them one counterexample at a time, again in
//! every frame. Here they are found up front instead: simulate from the
//! initial states, keep every two-literal clause over the most relevant
//! latches that no simulated state violates, and let Houdini keep the
//! largest subset that is inductive as a whole. The result holds in every
//! reachable state, so IC3 can assume it in every frame.

use crate::ic3::Cube;
use informal_aig::{neg, var, Aig, Init, Lit, Node};
use std::collections::{HashMap, VecDeque};

/// Latches closest to the bad outputs (breadth-first through the latch
/// dependency graph), at most `k`.
fn relevant_latches(g: &Aig, k: usize) -> Vec<usize> {
    let index: HashMap<u32, usize> = g
        .latches
        .iter()
        .enumerate()
        .map(|(i, l)| (var(l.lit), i))
        .collect();
    // Latches in the combinational cone of a literal.
    let cone_latches = |roots: &[Lit], seen: &mut Vec<bool>| -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack: Vec<u32> = roots.iter().map(|&l| var(l)).collect();
        while let Some(v) = stack.pop() {
            if std::mem::replace(&mut seen[v as usize], true) {
                continue;
            }
            match g.nodes[v as usize] {
                Node::And(a, b) => stack.extend([var(a), var(b)]),
                Node::Latch => out.push(index[&v]),
                _ => {}
            }
        }
        out
    };
    let mut seen = vec![false; g.nodes.len()];
    let mut taken = vec![false; g.latches.len()];
    let mut order = Vec::new();
    let mut queue: VecDeque<usize> = cone_latches(&g.bads, &mut seen).into();
    while let Some(i) = queue.pop_front() {
        if std::mem::replace(&mut taken[i], true) {
            continue;
        }
        order.push(i);
        if order.len() >= k {
            break;
        }
        let next = g.latches[i].next;
        for j in cone_latches(&[next], &mut seen) {
            queue.push_back(j);
        }
    }
    order
}

/// Candidate cubes (each blocked: the clause is the cube's negation) of
/// two latch literals that no simulated reachable state contains, best
/// supported first, at most `cap`.
pub fn candidates(g: &Aig, k: usize, cap: usize, sat_budget: std::time::Duration) -> Vec<Cube> {
    if g.latches.iter().any(|l| matches!(l.init, Init::Expr(_))) {
        return Vec::new();
    }
    let lat = relevant_latches(g, k);
    // Samples: random simulation from init; when the constraints kill
    // the random runs (input protocols), traces from a SAT solver instead.
    let mut words: Vec<Vec<u64>> = vec![Vec::new(); lat.len()];
    let mut live: Vec<u64> = Vec::new();
    let (sig0, alive0) = simulate_constrained(g, 32, 0x9e37_79b9_7f4a_7c15);
    let sim_ok = alive0.last().is_some_and(|a| a.count_ones() >= 16);
    if sim_ok {
        for seed in 0..4u64 {
            let (sig, alive) = if seed == 0 {
                (sig0.clone(), alive0.clone())
            } else {
                simulate_constrained(g, 32, 0x9e37_79b9_7f4a_7c15 ^ seed)
            };
            for (w, &i) in words.iter_mut().zip(&lat) {
                w.extend(sig[i].iter().zip(&alive).map(|(&x, &a)| x & a));
            }
            live.extend(alive);
        }
    } else {
        let (w, l) = sat_traces(g, &lat, sat_budget);
        words = w;
        live = l;
    }
    let n: u32 = live.iter().map(|w| w.count_ones()).sum();
    if n == 0 {
        return Vec::new();
    }
    let ones: Vec<u32> = words
        .iter()
        .map(|w| w.iter().map(|x| x.count_ones()).sum())
        .collect();
    // Constant latches are scorr's and simplify's business.
    let active: Vec<usize> = (0..lat.len())
        .filter(|&a| ones[a] > 0 && ones[a] < n)
        .collect();
    if std::env::var_os("INFORMAL_HOUDINI_STATS").is_some() {
        eprintln!(
            "mine: {} samples, {} of {} latches active",
            n,
            active.len(),
            lat.len()
        );
    }
    let mut cands: Vec<(u32, Cube)> = Vec::new();
    for (x, &a) in active.iter().enumerate() {
        for &b in &active[x + 1..] {
            let (wa, wb) = (&words[a], &words[b]);
            // Which of the four polarity pairs never occur together.
            let (mut pp, mut pn, mut np, mut nn) = (0u64, 0u64, 0u64, 0u64);
            for ((&p, &q), &m) in wa.iter().zip(wb).zip(&live) {
                pp |= p & q;
                pn |= p & !q & m;
                np |= !p & q & m;
                nn |= !p & !q & m;
            }
            let (la, lb) = (g.latches[lat[a]].lit, g.latches[lat[b]].lit);
            let (oa, ob) = (ones[a], ones[b]);
            let (za, zb) = (n - oa, n - ob);
            // Support: how often the clause was satisfied by a near miss
            // (each side's literal true on its own).
            if pp == 0 {
                cands.push((oa.min(ob), vec![la, lb]));
            }
            if pn == 0 {
                cands.push((oa.min(zb), vec![la, neg(lb)]));
            }
            if np == 0 {
                cands.push((za.min(ob), vec![neg(la), lb]));
            }
            if nn == 0 {
                cands.push((za.min(zb), vec![neg(la), neg(lb)]));
            }
        }
    }
    cands.sort_by_key(|x| std::cmp::Reverse(x.0));
    cands.truncate(cap);
    cands
        .into_iter()
        .map(|(_, mut c)| {
            c.sort_unstable();
            c
        })
        .collect()
}

/// Like [`crate::scorr::simulate`] from the initial states, but inputs
/// are redrawn (up to 32 times per step) for the runs whose constraints
/// fail, so that constrained models keep live runs. Per latch and step
/// its value word, and per step the mask of runs still alive.
fn simulate_constrained(g: &Aig, steps: usize, seed: u64) -> (Vec<Vec<u64>>, Vec<u64>) {
    let mut rng = seed | 1;
    let mut rnd = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let n = g.nodes.len();
    let w = |vals: &[u64], l: Lit| -> u64 {
        let x = vals[var(l) as usize];
        if informal_aig::is_neg(l) {
            !x
        } else {
            x
        }
    };
    let mut state: Vec<u64> = g
        .latches
        .iter()
        .map(|l| match l.init {
            Init::One => !0,
            Init::Zero => 0,
            _ => rnd(),
        })
        .collect();
    let inputs: Vec<usize> = (0..n)
        .filter(|&v| matches!(g.nodes[v], Node::Input))
        .collect();
    let mut vals = vec![0u64; n];
    for &v in &inputs {
        vals[v] = rnd();
    }
    let mut sig = vec![Vec::with_capacity(steps); g.latches.len()];
    let mut live = Vec::with_capacity(steps);
    let mut alive = !0u64;
    for _ in 0..steps {
        let mut ok = 0u64;
        for attempt in 0..32 {
            let mut li = 0;
            for v in 0..n {
                vals[v] = match g.nodes[v] {
                    Node::Const => 0,
                    Node::Input => {
                        if attempt == 0 {
                            rnd()
                        } else {
                            // Redraw only where the constraints failed.
                            let r = rnd();
                            (vals[v] & ok) | (r & !ok)
                        }
                    }
                    Node::Latch => {
                        li += 1;
                        state[li - 1]
                    }
                    Node::And(a, b) => w(&vals, a) & w(&vals, b),
                };
            }
            let mut c = !0u64;
            for &k in &g.constraints {
                c &= w(&vals, k);
            }
            ok |= c;
            if ok & alive == alive {
                break;
            }
            // Keep the inputs of runs that are fine: recompute with them.
            let _ = attempt;
        }
        alive &= ok;
        live.push(alive);
        for (i, l) in g.latches.iter().enumerate() {
            sig[i].push(state[i]);
            state[i] = w(&vals, l.next);
        }
    }
    (sig, live)
}

/// Reachable-state samples from SAT: up to 64 traces of 16 frames from the
/// initial states under the constraints, each steered by random
/// assumptions on a third of the inputs (dropped when they make it UNSAT).
/// Per latch of `lat`, bit (trace * 16 + frame) of its words, and the mask
/// of filled samples.
fn sat_traces(g: &Aig, lat: &[usize], budget: std::time::Duration) -> (Vec<Vec<u64>>, Vec<u64>) {
    const D: usize = 16;
    const T: usize = 64;
    let t0 = std::time::Instant::now();
    let mut words = vec![vec![0u64; T * D / 64]; lat.len()];
    let mut live = vec![0u64; T * D / 64];
    let mut u = crate::unroll::Unroll::new(g, informal_sat::best(), false);
    for _ in 0..D {
        u.add_frame();
    }
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    for tr in 0..T {
        if t0.elapsed() > budget {
            break;
        }
        let mut assume = Vec::new();
        for k in 0..D {
            for &i in &g.inputs {
                let r = rnd();
                if r % 16 == 0 {
                    let x = u.lit(k, i);
                    assume.push(if r & 8 != 0 { x } else { -x });
                }
            }
        }
        let mut ok = false;
        for _ in 0..16 {
            match u.sat.solve(&assume) {
                informal_sat::Answer::Sat => {
                    ok = true;
                    break;
                }
                informal_sat::Answer::Unsat => {
                    if std::env::var_os("INFORMAL_HOUDINI_STATS").is_some() && tr == 0 {
                        eprintln!("mine: trace UNSAT with {} assumptions", assume.len());
                    }
                    // Keep the assumptions the conflict did not need.
                    assume.retain(|&a| !u.sat.failed(a));
                }
                informal_sat::Answer::Unknown => break,
            }
        }
        if !ok {
            continue;
        }
        for k in 0..D {
            let bit = tr * D + k;
            live[bit / 64] |= 1 << (bit % 64);
            for (w, &i) in words.iter_mut().zip(lat) {
                if u.sat.value(u.lit(k, g.latches[i].lit)) {
                    w[bit / 64] |= 1 << (bit % 64);
                }
            }
        }
    }
    if std::env::var_os("INFORMAL_HOUDINI_STATS").is_some() {
        let n: u32 = live.iter().map(|w| w.count_ones()).sum();
        eprintln!(
            "mine: {n} SAT samples in {:.2} s",
            t0.elapsed().as_secs_f64()
        );
    }
    (words, live)
}
