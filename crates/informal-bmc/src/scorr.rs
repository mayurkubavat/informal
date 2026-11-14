//! Latch correspondence (van Eijk 1998; ABC's `scorr` restricted to
//! latches): find latches that are equal, opposite, or constant in every
//! reachable state, and merge each onto one representative.
//!
//! Candidates come from random simulation from the initial state. They
//! are then refined by 1-induction: assuming every candidate equality in
//! frame 0, a SAT model that breaks one in frame 1 splits the classes by
//! their frame-1 values, until no model exists. The surviving equalities
//! hold initially (all inits are constants) and are inductive together, so
//! they are invariants and merging keeps every verdict and counterexample
//! depth. The step assumes the constraints in frame 0 only: an equality
//! that needs them in frame 1 too may fail exactly where merging makes a
//! constraint hold that the original breaks. Uninitialized latches are simulated but never merged; runs that
//! break a constraint stop counting. Models with Btor2 expression inits
//! are left alone.

use crate::unroll::Unroll;
use informal_aig::preprocess::{InputMap, LatchMap};
use informal_aig::{is_neg, var, Aig, Init, Lit, Node, FALSE};
use informal_engine::RunContext;
use informal_sat::{Answer, Sat};
use std::collections::HashMap;

/// Simulation depth and rounds of 64 parallel runs.
const SIM_STEPS: usize = 128;
const SIM_ROUNDS: usize = 8;

pub struct Stats {
    pub merged: usize,
    pub refinements: usize,
    /// The proved equalities, over the input model's literals: a merged
    /// latch and what it equals (its representative, possibly negated,
    /// or a constant). A certificate needs all of them, also the ones
    /// whose latches fall out of the cone once they are merged.
    pub equalities: Vec<(Lit, Lit)>,
}

/// One member of a class: AIG literal of the latch (or [`FALSE`] for the
/// constant) and whether it is the class representative's complement.
#[derive(Clone, Copy)]
struct Member {
    lit: Lit,
    phase: bool,
}

/// Simulate 64 runs for `steps` steps with random inputs, from the
/// initial states (`start` = None; uninitialized latches random) or from
/// one given state. Per latch and step its value word, and per step the
/// mask of runs that have kept every constraint so far.
pub(crate) fn simulate(
    g: &Aig,
    start: Option<&[bool]>,
    steps: usize,
    seed: u64,
) -> (Vec<Vec<u64>>, Vec<u64>) {
    let mut rng = seed | 1;
    let mut rnd = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let n = g.nodes.len();
    let mut vals = vec![0u64; n];
    let mut sig = vec![Vec::with_capacity(steps); g.latches.len()];
    let mut state: Vec<u64> = match start {
        Some(st) => st.iter().map(|&b| if b { !0 } else { 0 }).collect(),
        None => g
            .latches
            .iter()
            .map(|l| match l.init {
                Init::One => !0,
                Init::Zero => 0,
                _ => rnd(),
            })
            .collect(),
    };
    let w = |vals: &[u64], l: Lit| -> u64 {
        let x = vals[var(l) as usize];
        if is_neg(l) {
            !x
        } else {
            x
        }
    };
    let mut alive = !0u64;
    let mut live = Vec::with_capacity(steps);
    for _ in 0..steps {
        let mut li = 0;
        for v in 0..n {
            vals[v] = match g.nodes[v] {
                Node::Const => 0,
                Node::Input => rnd(),
                Node::Latch => {
                    li += 1;
                    state[li - 1]
                }
                Node::And(a, b) => w(&vals, a) & w(&vals, b),
            };
        }
        // Runs that break a constraint are dead from here on.
        for &c in &g.constraints {
            alive &= w(&vals, c);
        }
        live.push(alive);
        for (i, l) in g.latches.iter().enumerate() {
            sig[i].push(state[i]);
            state[i] = w(&vals, l.next);
        }
    }
    (sig, live)
}

/// A member's signature: its latch's words, complemented by its phase,
/// on live runs only (the constant is all zero).
fn signature(m: &Member, row: &HashMap<Lit, usize>, sigs: &[Vec<u64>], live: &[u64]) -> Vec<u64> {
    let flip = if m.phase { !0u64 } else { 0 };
    match row.get(&m.lit) {
        None => vec![0; live.len()],
        Some(&i) => sigs[i]
            .iter()
            .zip(live)
            .map(|(&x, &a)| (x ^ flip) & a)
            .collect(),
    }
}

/// Split classes by a per-member key (relative to the member's phase);
/// singletons drop out, and each new class is re-based on its first
/// member.
fn split<K: Eq + std::hash::Hash>(
    classes: Vec<Vec<Member>>,
    key: impl Fn(&Member) -> K,
) -> Vec<Vec<Member>> {
    let mut next = Vec::new();
    for c in classes {
        let mut groups: Vec<(K, Vec<Member>)> = Vec::new();
        let mut index: HashMap<K, usize> = HashMap::new();
        for m in c {
            let k = key(&m);
            match index.get(&k) {
                Some(&gi) => groups[gi].1.push(m),
                None => {
                    // Keys are cloned only through the map position.
                    index.insert(key(&m), groups.len());
                    groups.push((k, vec![m]));
                }
            }
        }
        for (_, mut g) in groups {
            if g.len() > 1 {
                let p0 = g[0].phase;
                for m in g.iter_mut() {
                    m.phase ^= p0;
                }
                next.push(g);
            }
        }
    }
    next
}

/// Latch literal -> index in `g.latches`.
fn latch_rows(g: &Aig) -> HashMap<Lit, usize> {
    g.latches
        .iter()
        .enumerate()
        .map(|(i, l)| (l.lit, i))
        .collect()
}

/// Classes of candidates; the first member of each is the representative
/// (the constant when the class has one, else the lowest latch).
fn candidates(g: &Aig) -> Vec<Vec<Member>> {
    let mut sigs: Vec<Vec<u64>> = vec![Vec::new(); g.latches.len()];
    let mut live = Vec::new();
    for r in 0..SIM_ROUNDS {
        let (sim, l) = simulate(g, None, SIM_STEPS, 0x9e37_79b9_7f4a_7c15 ^ r as u64);
        for (i, s) in sim.into_iter().enumerate() {
            sigs[i].extend(s);
        }
        live.extend(l);
    }
    // One class of every initialized latch and the constant; a member's
    // phase is its init value, so its normalized signature is
    // (value ^ init) on live runs.
    let mut all = vec![Member {
        lit: FALSE,
        phase: false,
    }];
    for l in &g.latches {
        // Uninitialized latches take part in simulation but never merge.
        if matches!(l.init, Init::Zero | Init::One) {
            all.push(Member {
                lit: l.lit,
                phase: l.init == Init::One,
            });
        }
    }
    let row = latch_rows(g);
    let mut classes = split(vec![all], |m| signature(m, &row, &sigs, &live));
    classes.sort_by_key(|c| c[0].lit);
    classes
}

/// One step of `g` from 64 states at once (`state`: a word per latch)
/// with random inputs: the next-state word of every latch, and the mask of
/// runs whose state and inputs meet the constraints.
fn step_words(g: &Aig, state: &[u64], rnd: &mut impl FnMut() -> u64) -> (Vec<u64>, u64) {
    let mut vals = vec![0u64; g.nodes.len()];
    let w = |vals: &[u64], l: Lit| -> u64 {
        let x = vals[var(l) as usize];
        if is_neg(l) {
            !x
        } else {
            x
        }
    };
    let mut li = 0;
    for v in 0..g.nodes.len() {
        vals[v] = match g.nodes[v] {
            Node::Const => 0,
            Node::Input => rnd(),
            Node::Latch => {
                li += 1;
                state[li - 1]
            }
            Node::And(a, b) => w(&vals, a) & w(&vals, b),
        };
    }
    let mut live = !0u64;
    for &c in &g.constraints {
        live &= w(&vals, c);
    }
    (g.latches.iter().map(|l| w(&vals, l.next)).collect(), live)
}

/// Refutation without SAT: 64 random states in which every candidate
/// equality holds (each class gets one random word, its members follow
/// by phase; other latches are random), one step with random inputs, and
/// the classes split by the next-state values on the runs that meet the
/// constraints. Each such run is exactly a counterexample the induction
/// query could return. Repeats until `quiet` rounds in a row split
/// nothing. On the cv32e40x models the SAT loop alone peeled about one
/// member per query, at up to 160 ms a query, and never finished.
fn random_refine(
    g: &Aig,
    mut classes: Vec<Vec<Member>>,
    row: &HashMap<Lit, usize>,
    seed: u64,
    quiet: usize,
    ctx: &RunContext,
) -> Vec<Vec<Member>> {
    let mut rng = seed | 1;
    let mut rnd = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let count = |cs: &[Vec<Member>]| cs.iter().map(Vec::len).sum::<usize>();
    let mut idle = 0;
    let mut state = vec![0u64; g.latches.len()];
    while idle < quiet && !classes.is_empty() && !ctx.should_stop() {
        for x in state.iter_mut() {
            *x = rnd();
        }
        for c in &classes {
            let r = if c[0].lit == FALSE { 0 } else { rnd() };
            for m in c {
                if let Some(&i) = row.get(&m.lit) {
                    state[i] = if m.phase { !r } else { r };
                }
            }
        }
        let (next, live) = step_words(g, &state, &mut rnd);
        let before = (classes.len(), count(&classes));
        classes = split(classes, |m| match row.get(&m.lit) {
            None => 0,
            Some(&i) => (if m.phase { !next[i] } else { next[i] }) & live,
        });
        if (classes.len(), count(&classes)) == before {
            idle += 1;
        } else {
            idle = 0;
        }
    }
    classes
}

/// Refine classes until their equalities are inductive together. `None`
/// when time runs out.
fn refine(
    g: &Aig,
    mut classes: Vec<Vec<Member>>,
    sat: Box<dyn Sat>,
    ctx: &RunContext,
    stats: &mut Stats,
) -> Option<Vec<Vec<Member>>> {
    let row = latch_rows(g);
    let mut u = Unroll::new(g, sat, true);
    u.add_frame();
    // No constraints in frame 1: the equalities must follow from a valid
    // frame 0 alone. Merging changes how constraints evaluate, so an
    // equality that only holds when frame 1 also meets them is not safe to
    // merge (that gave false counterexamples on x-epic a16).
    u.add_frame_with(false);
    let t0 = std::time::Instant::now();
    let mut enc: HashMap<(Lit, Lit, bool), (i32, i32)> = HashMap::new();
    let random = std::env::var_os("INFORMAL_SCORR_NO_RANDOM").is_none();
    loop {
        if random {
            let before = classes.iter().map(|c| c.len() - 1).sum::<usize>();
            classes = random_refine(
                g,
                classes,
                &row,
                0x2545_f491 ^ stats.refinements as u64,
                16,
                ctx,
            );
            if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
                let after = classes.iter().map(|c| c.len() - 1).sum::<usize>();
                if after != before {
                    eprintln!(
                        "scorr: random steps cut {before} members to {after} ({:.2} s)",
                        t0.elapsed().as_secs_f64()
                    );
                }
            }
        }
        if ctx.should_stop() {
            if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
                eprintln!(
                    "scorr: out of time after {} refinements ({:.1} s), {} members left",
                    stats.refinements,
                    t0.elapsed().as_secs_f64(),
                    classes.iter().map(|c| c.len() - 1).sum::<usize>()
                );
            }
            return None;
        }
        if classes.is_empty() {
            return Some(classes);
        }
        // Frame 0: assume every equality (under activation `a`); frame 1:
        // some equality fails.
        // Each (member, representative, phase) pair is encoded once: `s`
        // turns the frame-0 equality on, `d` means it breaks in frame 1.
        let a = u.new_var();
        let mut any = vec![-a];
        let mut assume = vec![a];
        for c in &classes {
            let r = c[0];
            for m in &c[1..] {
                let (s, d) = *enc.entry((m.lit, r.lit, m.phase)).or_insert_with(|| {
                    let ph = |x: i32| if m.phase { -x } else { x };
                    let (x0, r0) = (u.lit(0, m.lit), ph(u.lit(0, r.lit)));
                    let s = u.new_var();
                    u.sat.add_clause(&[-s, -x0, r0]);
                    u.sat.add_clause(&[-s, x0, -r0]);
                    let (x1, r1) = (u.lit(1, m.lit), ph(u.lit(1, r.lit)));
                    let d = u.new_var();
                    u.sat.add_clause(&[-d, x1, r1]);
                    u.sat.add_clause(&[-d, -x1, -r1]);
                    (s, d)
                });
                assume.push(s);
                any.push(d);
            }
        }
        u.sat.add_clause(&any);
        let r = u.sat.solve(&assume);
        match r {
            Answer::Unsat => {
                u.sat.add_clause(&[-a]);
                return Some(classes);
            }
            Answer::Unknown => {
                if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
                    eprintln!(
                        "scorr: SAT query stopped after {} refinements ({:.1} s), {} members left",
                        stats.refinements,
                        t0.elapsed().as_secs_f64(),
                        classes.iter().map(|c| c.len() - 1).sum::<usize>()
                    );
                }
                return None;
            }
            Answer::Sat => {}
        }
        stats.refinements += 1;
        // Split by the frame-1 values, then by 64 random runs of 8 steps
        // from the frame-1 state: one SAT model refines many classes.
        let v1: Vec<bool> = g
            .latches
            .iter()
            .map(|l| u.sat.value(u.lit(1, l.lit)))
            .collect();
        let one = |m: &Member| match row.get(&m.lit) {
            None => false,
            Some(&i) => v1[i] ^ m.phase,
        };
        classes = split(classes, one);
        let (sigs, live) = simulate(g, Some(&v1), 8, 0x5851_f42d ^ stats.refinements as u64);
        let next = split(classes, |m| signature(m, &row, &sigs, &live));
        u.sat.add_clause(&[-a]);
        classes = next;
    }
}

/// `g` with proved-equivalent latches merged, and the map from `g`'s latches
/// to the result's literals, or `None` when nothing merges (or the model is
/// out of scope, or time runs out).
pub fn latch_correspondence(
    g: &Aig,
    sat: Box<dyn Sat>,
    ctx: &RunContext,
) -> Option<(Aig, Stats, LatchMap, InputMap)> {
    if g.latches.iter().any(|l| matches!(l.init, Init::Expr(_))) {
        return None;
    }
    let mut stats = Stats {
        merged: 0,
        refinements: 0,
        equalities: Vec::new(),
    };
    let classes = candidates(g);
    let dbg = std::env::var_os("INFORMAL_IC3_STATS").is_some();
    if dbg {
        eprintln!(
            "scorr: {} candidate classes, {} members",
            classes.len(),
            classes.iter().map(|c| c.len() - 1).sum::<usize>()
        );
    }
    if classes.is_empty() {
        return None;
    }
    let classes = refine(g, classes, sat, ctx, &mut stats)?;
    // Member latch variable -> (representative literal, phase).
    let mut subst: HashMap<u32, (Lit, bool)> = HashMap::new();
    for c in &classes {
        for m in &c[1..] {
            subst.insert(var(m.lit), (c[0].lit, m.phase));
            stats
                .equalities
                .push((m.lit, c[0].lit ^ Lit::from(m.phase)));
        }
    }
    stats.merged = subst.len();
    if std::env::var_os("INFORMAL_SCORR_CHECK").is_some() {
        bmc_check(g, &classes, 8);
    }
    if subst.is_empty() {
        return None;
    }
    let (h, map, imap) = substitute(g, &subst);
    Some((h, stats, map, imap))
}

/// Rebuild `g` with each latch in `subst` replaced by its representative
/// (an earlier node), then simplify (the merged latches fall out of the
/// cone).
fn substitute(g: &Aig, subst: &HashMap<u32, (Lit, bool)>) -> (Aig, LatchMap, InputMap) {
    let mut out = Aig::new();
    let mut map = vec![FALSE; g.nodes.len()];
    let m = |map: &[Lit], l: Lit| map[var(l) as usize] ^ (l & 1);
    let mut li = 0;
    let mut kept = Vec::new();
    for (v, n) in g.nodes.iter().enumerate() {
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
                let latch = &g.latches[li];
                li += 1;
                match subst.get(&(v as u32)) {
                    Some(&(rep, phase)) => m(&map, rep) ^ phase as Lit,
                    None => {
                        let l = out.latch(latch.init);
                        if let Some(name) = g.names.get(&(v as u32)) {
                            out.names.insert(var(l), name.clone());
                        }
                        kept.push((l, latch.next));
                        l
                    }
                }
            }
            Node::And(a, b) => {
                let (x, y) = (m(&map, a), m(&map, b));
                out.and(x, y)
            }
        };
    }
    for (l, next) in kept {
        let n = m(&map, next);
        out.set_next(l, n);
    }
    out.bads = g.bads.iter().map(|&b| m(&map, b)).collect();
    out.constraints = g.constraints.iter().map(|&c| m(&map, c)).collect();
    let to_out: LatchMap = g.latches.iter().map(|l| Some(m(&map, l.lit))).collect();
    let in_out: InputMap = g.inputs.iter().map(|&i| Some(m(&map, i))).collect();
    let (h, _, to_h, in_h) = informal_aig::preprocess::simplify_maps(&out);
    let lmap = informal_aig::preprocess::compose(&to_out, &out, &to_h);
    let imap = informal_aig::preprocess::compose_inputs(&in_out, &out, &in_h);
    (h, lmap, imap)
}

/// Debug (INFORMAL_SCORR_CHECK): BMC the equalities on `g` itself, from the
/// initial states, up to `depth`; prints the first one that fails.
fn bmc_check(g: &Aig, classes: &[Vec<Member>], depth: usize) {
    let mut u = Unroll::new(g, informal_sat::best(), false);
    for k in 0..=depth {
        u.add_frame();
        for c in classes {
            let r = c[0];
            for m in &c[1..] {
                let x = u.lit(k, m.lit);
                let y = u.lit(k, r.lit);
                let y = if m.phase { -y } else { y };
                // Is x != y possible at frame k?
                let d = u.new_var();
                u.sat.add_clause(&[-d, x, y]);
                u.sat.add_clause(&[-d, -x, -y]);
                if u.sat.solve(&[d]) == Answer::Sat {
                    eprintln!(
                        "scorr check: latch {} != {}latch {} at depth {k}",
                        m.lit,
                        if m.phase { "!" } else { "" },
                        r.lit
                    );
                    return;
                }
                u.sat.add_clause(&[-d]);
            }
        }
    }
    eprintln!("scorr check: all equalities hold to depth {depth}");
}
