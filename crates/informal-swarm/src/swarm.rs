//! Bit-parallel 64-lane AIG simulation + novelty-guided frontier seeding +
//! short-horizon SAT bridging (`informal-swarm`).
//!
//! Combines:
//! 1. **64-lane bit-parallel simulation** with stratified input bias densities
//!    (sparse low-toggle lanes through dense high-1 lanes) so reset/enable
//!    signals do not starve random exploration.
//! 2. **Coverage/novelty frontier pool** retaining concrete reachable states
//!    (with their exact input prefix from `Init`) that flip unseen latch bits
//!    or visit rare state hashes.
//! 3. **Short-horizon SAT bridge** (`0..=BRIDGE_DEPTH` steps) fired from `Init`
//!    and novel frontier states to crack narrow multi-bit triggers (`key ==
//!    0xBEEF`) once simulation walks past deep sequential warmups.

use informal_aig::{is_neg, var, Aig, Init, Lit, Node};
use informal_bmc::unroll::Unroll;
use informal_bmc::{solver, PreparedTrace};
use informal_engine::{Progress, RunContext};
use informal_sat::Answer;
use std::collections::HashSet;

const BRIDGE_DEPTH: usize = 4;
const MAX_FRONTIER: usize = 64;
const STEPS_PER_WAVE: usize = 64;

/// Outcome of a swarm bug-hunting run.
pub enum SwarmOutcome {
    Unsafe {
        depth: usize,
        trace: PreparedTrace,
        witness: String,
    },
    Aborted(String),
}

/// A concrete reachable state along with its exact `C`-legal input prefix
/// from `Init`.
#[derive(Clone)]
struct Seed {
    init: Vec<bool>,
    state: Vec<bool>,
    prefix_inputs: Vec<Vec<bool>>,
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// Generate a 64-lane word with stratified bit densities across lanes:
    /// - lanes  0..8 : sparse (~1/16 ones)
    /// - lanes  8..24: low    (~1/4  ones)
    /// - lanes 24..48: balanced (~1/2 ones)
    /// - lanes 48..56: high   (~3/4  ones)
    /// - lanes 56..64: dense  (~15/16 ones)
    fn stratified_word(&mut self) -> u64 {
        let a = self.next_u64();
        let b = self.next_u64();
        let c = self.next_u64();
        let d = self.next_u64();
        let sparse = (a & b & c & d) & 0x0000_0000_0000_00FF;
        let low = (a & b) & 0x0000_0000_00FF_FF00;
        let mid = a & 0x0000_FFFF_FF00_0000;
        let high = (a | b) & 0x00FF_0000_0000_0000;
        let dense = (a | b | c | d) & 0xFF00_0000_0000_0000;
        sparse | low | mid | high | dense
    }
}

/// Run bit-parallel simulation-guided swarm search on prepared AIG `g`.
pub fn run_swarm(g: &Aig, max_depth: Option<usize>, ctx: &RunContext) -> SwarmOutcome {
    if g.latches.iter().any(|l| matches!(l.init, Init::Expr(_))) {
        return SwarmOutcome::Aborted("latch inits that are not constants".into());
    }

    // 1. Immediate short-horizon SAT bridge from `Init` (`0..=BRIDGE_DEPTH`):
    //    cracks combinational & shallow multi-bit trigger bugs instantly.
    let init_bridge_max = max_depth.map_or(BRIDGE_DEPTH, |m| m.min(BRIDGE_DEPTH));
    match sat_bridge_from_init(g, init_bridge_max, ctx) {
        BridgeRes::Hit(trace) => {
            let depth = trace.inputs.len().saturating_sub(1);
            let witness = format_named_witness(g, &trace.init, &trace.inputs);
            return SwarmOutcome::Unsafe {
                depth,
                trace,
                witness,
            };
        }
        BridgeRes::Stopped => return SwarmOutcome::Aborted(ctx.stop_reason().into()),
        BridgeRes::Miss => {}
    }

    if g.latches.is_empty() || max_depth.is_some_and(|m| m <= init_bridge_max) {
        return SwarmOutcome::Aborted(max_depth.map_or_else(
            || "combinational search exhausted".into(),
            |m| format!("bound {m} reached"),
        ));
    }

    // 2. Build initial seed pool (concrete `Init` states satisfying frame-0 constraints).
    let Some(base_init) = sample_valid_init(g, ctx) else {
        return SwarmOutcome::Aborted(ctx.stop_reason().into());
    };
    let mut frontier: Vec<Seed> = vec![Seed {
        init: base_init.clone(),
        state: base_init,
        prefix_inputs: Vec::new(),
    }];
    let mut visited_hashes: HashSet<u64> = HashSet::with_capacity(4096);
    visited_hashes.insert(hash_state(&frontier[0].state));

    let n_latches = g.latches.len();
    let mut seen_zero = vec![false; n_latches];
    let mut seen_one = vec![false; n_latches];
    record_toggles(&frontier[0].state, &mut seen_zero, &mut seen_one);

    let mut latch_idx = vec![None; g.nodes.len()];
    for (li, l) in g.latches.iter().enumerate() {
        latch_idx[var(l.lit) as usize] = Some(li);
    }
    let mut input_idx = vec![None; g.nodes.len()];
    for (mi, &i) in g.inputs.iter().enumerate() {
        input_idx[var(i) as usize] = Some(mi);
    }

    let mut rng = Rng::new(0xA1B2_C3D4_E5F6_0719);
    let mut wave = 0usize;
    let mut max_depth_seen = init_bridge_max;

    loop {
        if ctx.should_stop() {
            return SwarmOutcome::Aborted(ctx.stop_reason().into());
        }
        wave += 1;

        // Pick a seed from the frontier (alternating between deepest novel state and random).
        let seed_idx = if wave % 2 == 1 {
            frontier.len() - 1
        } else {
            (rng.next_u64() as usize) % frontier.len()
        };
        let seed = frontier[seed_idx].clone();
        let base_depth = seed.prefix_inputs.len();
        let steps_left = match max_depth {
            Some(m) if base_depth >= m => {
                // Frontier seed already at max_depth; retry from depth 0 or stop.
                if wave > 16 {
                    return SwarmOutcome::Aborted(format!("bound {m} reached"));
                }
                continue;
            }
            Some(m) => (m - base_depth).min(STEPS_PER_WAVE),
            None => STEPS_PER_WAVE,
        };

        // Initialize all 64 lanes to `seed.state`.
        let mut lane_state: Vec<u64> = seed
            .state
            .iter()
            .map(|&b| if b { !0u64 } else { 0u64 })
            .collect();
        let mut alive: u64 = !0u64;
        let mut wave_inputs: Vec<Vec<u64>> = Vec::with_capacity(steps_left + 1);
        let mut vals = vec![0u64; g.nodes.len()];

        for step in 0..=steps_left {
            if ctx.should_stop() {
                return SwarmOutcome::Aborted(ctx.stop_reason().into());
            }
            let in_words: Vec<u64> = (0..g.inputs.len()).map(|_| rng.stratified_word()).collect();

            for (v, node) in g.nodes.iter().enumerate() {
                vals[v] = match *node {
                    Node::Const => 0u64,
                    Node::Input => in_words[input_idx[v].expect("input idx")],
                    Node::Latch => lane_state[latch_idx[v].expect("latch idx")],
                    Node::And(a, b) => eval_word(&vals, a) & eval_word(&vals, b),
                };
            }

            for &c in &g.constraints {
                alive &= eval_word(&vals, c);
            }
            wave_inputs.push(in_words);
            if alive == 0 {
                break;
            }

            let mut bad_word = 0u64;
            for &b in &g.bads {
                bad_word |= eval_word(&vals, b);
            }
            let hit = bad_word & alive;
            if hit != 0 {
                let lane = hit.trailing_zeros() as usize;
                let mut full_inputs = seed.prefix_inputs.clone();
                for w_in in &wave_inputs {
                    full_inputs.push(w_in.iter().map(|&w| ((w >> lane) & 1) != 0).collect());
                }
                let depth = full_inputs.len().saturating_sub(1);
                let witness = format_named_witness(g, &seed.init, &full_inputs);
                return SwarmOutcome::Unsafe {
                    depth,
                    trace: PreparedTrace {
                        init: seed.init,
                        inputs: full_inputs,
                    },
                    witness,
                };
            }

            if step == steps_left {
                break;
            }

            // Advance 64 lanes to next state.
            let next_state: Vec<u64> = g.latches.iter().map(|l| eval_word(&vals, l.next)).collect();
            lane_state = next_state;
            let cur_depth = base_depth + step + 1;
            max_depth_seen = max_depth_seen.max(cur_depth);

            // Sample novel frontier state every `BRIDGE_DEPTH` steps (zero blind spots!).
            let is_checkpoint = step == steps_left - 1 || (step + 1) % BRIDGE_DEPTH == 0;
            if is_checkpoint {
                let lane = alive.trailing_zeros() as usize;
                let st: Vec<bool> = lane_state.iter().map(|&w| ((w >> lane) & 1) != 0).collect();
                let h = hash_state(&st);
                let new_toggle = record_toggles(&st, &mut seen_zero, &mut seen_one);
                let is_new = visited_hashes.insert(h);
                if new_toggle || is_new {
                    let mut prefix = seed.prefix_inputs.clone();
                    for w_in in &wave_inputs {
                        prefix.push(w_in.iter().map(|&w| ((w >> lane) & 1) != 0).collect());
                    }
                    // Fire targeted short-horizon SAT bridge from this novel frontier state!
                    let b_max = max_depth.map_or(BRIDGE_DEPTH, |m| {
                        m.saturating_sub(prefix.len()).min(BRIDGE_DEPTH)
                    });
                    match sat_bridge_from_state(g, &st, b_max, ctx) {
                        BridgeRes::Hit(suffix) => {
                            let mut full_inputs = prefix;
                            full_inputs.extend(suffix.inputs);
                            let depth = full_inputs.len().saturating_sub(1);
                            let witness = format_named_witness(g, &seed.init, &full_inputs);
                            return SwarmOutcome::Unsafe {
                                depth,
                                trace: PreparedTrace {
                                    init: seed.init,
                                    inputs: full_inputs,
                                },
                                witness,
                            };
                        }
                        BridgeRes::Stopped => {
                            return SwarmOutcome::Aborted(ctx.stop_reason().into());
                        }
                        BridgeRes::Miss => {}
                    }
                    let new_seed = Seed {
                        init: seed.init.clone(),
                        state: st,
                        prefix_inputs: prefix,
                    };
                    if frontier.len() < MAX_FRONTIER {
                        frontier.push(new_seed);
                    } else {
                        let evict = 1 + ((rng.next_u64() as usize) % (MAX_FRONTIER - 1));
                        frontier[evict] = new_seed;
                    }
                }
            }
        }

        if wave.is_multiple_of(8) {
            ctx.report(Progress {
                engine: "informal-swarm".into(),
                bound: Some(max_depth_seen as u32),
                note: format!("swarm wave {wave}, frontier {}", frontier.len()),
                output: None,
            });
        }
        if max_depth.is_some_and(|m| max_depth_seen >= m && wave >= 32) {
            return SwarmOutcome::Aborted(format!("bound {} reached", max_depth.unwrap()));
        }
    }
}

#[inline]
fn eval_word(vals: &[u64], l: Lit) -> u64 {
    let w = vals[var(l) as usize];
    if is_neg(l) {
        !w
    } else {
        w
    }
}

fn hash_state(state: &[bool]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in state {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn record_toggles(state: &[bool], seen_zero: &mut [bool], seen_one: &mut [bool]) -> bool {
    let mut novel = false;
    for (i, &b) in state.iter().enumerate() {
        if b {
            if !seen_one[i] {
                seen_one[i] = true;
                novel = true;
            }
        } else if !seen_zero[i] {
            seen_zero[i] = true;
            novel = true;
        }
    }
    novel
}

fn sample_valid_init(g: &Aig, ctx: &RunContext) -> Option<Vec<bool>> {
    let mut u = Unroll::new(g, solver(ctx), false);
    u.add_frame();
    match u.sat.solve(&[]) {
        Answer::Sat => Some(
            g.latches
                .iter()
                .map(|l| u.sat.value(u.lit(0, l.lit)))
                .collect(),
        ),
        _ => None,
    }
}

enum BridgeRes {
    Hit(PreparedTrace),
    Miss,
    Stopped,
}

fn sat_bridge_from_init(g: &Aig, max_steps: usize, ctx: &RunContext) -> BridgeRes {
    let mut u = Unroll::new(g, solver(ctx), false);
    for d in 0..=max_steps {
        if ctx.should_stop() {
            return BridgeRes::Stopped;
        }
        u.add_frame();
        let bad = u.bad(d);
        match u.sat.solve(&[bad]) {
            Answer::Sat => return BridgeRes::Hit(PreparedTrace::from_unroll(g, &u, d)),
            Answer::Unsat => u.sat.add_clause(&[-bad]),
            Answer::Unknown => return BridgeRes::Stopped,
        }
    }
    BridgeRes::Miss
}

fn sat_bridge_from_state(g: &Aig, state: &[bool], max_steps: usize, ctx: &RunContext) -> BridgeRes {
    let mut seeded = g.clone();
    for (l, &b) in seeded.latches.iter_mut().zip(state) {
        l.init = if b { Init::One } else { Init::Zero };
    }
    sat_bridge_from_init(&seeded, max_steps, ctx)
}

fn format_named_witness(g: &Aig, init: &[bool], inputs: &[Vec<bool>]) -> String {
    let bit = |b: bool| if b { '1' } else { '0' };
    let d = inputs.len().saturating_sub(1);
    let mut s = format!("informalw 1\ndepth {d}\ninit");
    for (l, &val) in g.latches.iter().zip(init) {
        if matches!(l.init, Init::Zero | Init::One) {
            continue;
        }
        if let Some(name) = g.names.get(&var(l.lit)) {
            s += &format!(" {name}={}", bit(val));
        }
    }
    for (f, frame_in) in inputs.iter().enumerate() {
        s += &format!("\n@{f}");
        for (&i, &val) in g.inputs.iter().zip(frame_in) {
            if let Some(name) = g.names.get(&var(i)) {
                s += &format!(" {name}={}", bit(val));
            }
        }
    }
    s.push('\n');
    s
}
