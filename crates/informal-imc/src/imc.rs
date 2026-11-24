//! McMillan's Interpolation-Based Model Checking (CAV 2003) over CaDiCaL's
//! proof-tracing Craig interpolator.
//!
//! At unrolling depth `k >= 1`, partitions the bounded reachability query into:
//! - **`A`** = `R(s_0) & C(s_0, i_0) & (s_1 <=> next(s_0, i_0))`
//! - **`B`** = `∨_{t=1..=k} (C(s_1, i_1) & ... & C(s_t, i_t) & Bad(s_t, i_t))`
//!
//! Because `A` and `B` share **only** the frame-1 latch variables `s_1[0..n]`,
//! every McMillan interpolant `I(s_1)` is an over-approximation of the 1-step
//! image of `R` that cannot reach `Bad` within `k-1` legal steps. When
//! `I(s) & C(s, i) => R(s)` holds, `R` is a global inductive invariant and is
//! converted into minimal CNF blocked cubes for independent certification via
//! [`informal_bmc::certify_lemmas`].

use informal_aig::{is_neg, var, Aig, Init, Lit, Node};
use informal_bmc::cnf::{self, Template};
use informal_bmc::{ic3, seed_from_hub, solver, Prepared, PreparedTrace};
use informal_engine::{Progress, RunContext};
use informal_sat::{Answer, InterpAig, Interpolator, Sat};
use std::collections::HashMap;
use std::time::Duration;

/// Maximum CNF blocked cubes extracted from an interpolant fixpoint invariant.
pub const MAX_CERT_CUBES: usize = 4_096;
/// Maximum inner interpolation iterations per unrolling depth `k` before widening `k`.
const MAX_INNER_ITERS: usize = 64;

/// Outcome of McMillan IMC on a prepared AIG.
pub enum ImcOutcome {
    Safe(Vec<ic3::Cube>),
    Unsafe {
        depth: usize,
        trace: PreparedTrace,
        witness: String,
    },
    Aborted(String),
}

/// Compact hash-consed state-predicate AIG over latches `0..n_latches`.
///
/// Encoding: `0 = FALSE`, `1 = TRUE`, `2*(1 + li)` = positive latch `li`,
/// `2*(1 + n_latches + gi)` = AND gate `ands[gi]`.
struct StateAig {
    n_latches: usize,
    ands: Vec<(u32, u32)>,
    hash: HashMap<(u32, u32), u32>,
}

impl StateAig {
    fn new(n_latches: usize) -> Self {
        Self {
            n_latches,
            ands: Vec::new(),
            hash: HashMap::new(),
        }
    }

    #[inline]
    fn latch_lit(&self, li: usize) -> u32 {
        2 * (1 + li as u32)
    }

    fn mk_and(&mut self, mut a: u32, mut b: u32) -> u32 {
        if a == 0 || b == 0 {
            return 0;
        }
        if a == 1 {
            return b;
        }
        if b == 1 || a == b {
            return a;
        }
        if a == (b ^ 1) {
            return 0;
        }
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        if let Some(&l) = self.hash.get(&(a, b)) {
            return l;
        }
        let gi = self.ands.len() as u32;
        self.ands.push((a, b));
        let lit = 2 * (1 + self.n_latches as u32 + gi);
        self.hash.insert((a, b), lit);
        lit
    }

    #[inline]
    fn mk_or(&mut self, a: u32, b: u32) -> u32 {
        self.mk_and(a ^ 1, b ^ 1) ^ 1
    }

    fn encode_init(&mut self, g: &Aig) -> u32 {
        let mut acc = 1u32;
        for (li, l) in g.latches.iter().enumerate() {
            let sl = self.latch_lit(li);
            match l.init {
                Init::Zero => acc = self.mk_and(acc, sl ^ 1),
                Init::One => acc = self.mk_and(acc, sl),
                Init::Free | Init::Expr(_) => {}
            }
        }
        acc
    }

    fn import_interp(&mut self, itp: &InterpAig, var_to_latch: &HashMap<i32, usize>) -> u32 {
        let n_shared = itp.shared_vars.len();
        let mut mapped = Vec::with_capacity(1 + n_shared + itp.ands.len());
        mapped.push(0u32); // index 0 -> FALSE
        for &sv in &itp.shared_vars {
            let li = var_to_latch[&sv];
            mapped.push(self.latch_lit(li));
        }
        for &(l, r) in &itp.ands {
            let ml = mapped[(l >> 1) as usize] ^ (l & 1);
            let mr = mapped[(r >> 1) as usize] ^ (r & 1);
            mapped.push(self.mk_and(ml, mr));
        }
        mapped[(itp.root >> 1) as usize] ^ (itp.root & 1)
    }

    /// Encode the cone of `roots` into CNF clauses via `emit_clause`, returning
    /// the SAT literal for each root.
    fn encode_roots(
        &self,
        roots: &[u32],
        latch_vars: &[i32],
        tru: i32,
        next_var: &mut i32,
        mut emit_clause: impl FnMut(&[i32]),
    ) -> Vec<i32> {
        let mut needed = vec![false; self.ands.len()];
        let mut stack: Vec<usize> = Vec::new();
        for &r in roots {
            let idx = (r >> 1) as usize;
            if idx > self.n_latches {
                let gi = idx - 1 - self.n_latches;
                if !needed[gi] {
                    needed[gi] = true;
                    stack.push(gi);
                }
            }
        }
        while let Some(gi) = stack.pop() {
            let (l, r) = self.ands[gi];
            for al in [l, r] {
                let idx = (al >> 1) as usize;
                if idx > self.n_latches {
                    let cj = idx - 1 - self.n_latches;
                    if !needed[cj] {
                        needed[cj] = true;
                        stack.push(cj);
                    }
                }
            }
        }

        let mut gate_var = vec![0i32; self.ands.len()];
        let map_lit = |al: u32, gate_var: &[i32]| -> i32 {
            let idx = (al >> 1) as usize;
            let neg = (al & 1) != 0;
            let pos = if idx == 0 {
                -tru
            } else if idx <= self.n_latches {
                latch_vars[idx - 1]
            } else {
                gate_var[idx - 1 - self.n_latches]
            };
            if neg {
                -pos
            } else {
                pos
            }
        };

        for gi in 0..self.ands.len() {
            if !needed[gi] {
                continue;
            }
            let out = *next_var;
            *next_var += 1;
            gate_var[gi] = out;
            let (l, r) = self.ands[gi];
            let sl = map_lit(l, &gate_var);
            let sr = map_lit(r, &gate_var);
            emit_clause(&[-out, sl]);
            emit_clause(&[-out, sr]);
            emit_clause(&[out, -sl, -sr]);
        }

        roots.iter().map(|&r| map_lit(r, &gate_var)).collect()
    }
}

/// Encode one AIG frame into CNF clauses via `emit_clause`.
fn encode_frame(
    g: &Aig,
    tpl: &Template,
    latch_vars: &[i32],
    tru: i32,
    next_var: &mut i32,
    mut emit_clause: impl FnMut(&[i32]),
) -> (Vec<i32>, Vec<i32>) {
    let mut f = vec![0i32; g.nodes.len()];
    let mut input_vars = Vec::with_capacity(g.inputs.len());
    let mut li = 0usize;
    for (v, n) in g.nodes.iter().enumerate() {
        f[v] = match *n {
            Node::Const => -tru,
            Node::Input => {
                let x = *next_var;
                *next_var += 1;
                input_vars.push(x);
                x
            }
            Node::Latch => {
                let x = latch_vars[li];
                li += 1;
                x
            }
            Node::And(..) if tpl.live[v + 1] => {
                let x = *next_var;
                *next_var += 1;
                x
            }
            Node::And(..) => 0,
        };
    }
    let mut buf = Vec::new();
    for c in &tpl.clauses {
        buf.clear();
        buf.extend(c.iter().map(|&t| {
            let x = f[t.unsigned_abs() as usize - 1];
            if t < 0 {
                -x
            } else {
                x
            }
        }));
        emit_clause(&buf);
    }
    (f, input_vars)
}

#[inline]
fn flit(frame: &[i32], l: Lit) -> i32 {
    let x = frame[var(l) as usize];
    if is_neg(l) {
        -x
    } else {
        x
    }
}

fn encode_and_all(
    lits: &[i32],
    tru: i32,
    next_var: &mut i32,
    mut emit_clause: impl FnMut(&[i32]),
) -> i32 {
    if lits.is_empty() {
        return tru;
    }
    if lits.len() == 1 {
        return lits[0];
    }
    let out = *next_var;
    *next_var += 1;
    let mut big = Vec::with_capacity(lits.len() + 1);
    big.push(out);
    for &l in lits {
        emit_clause(&[-out, l]);
        big.push(-l);
    }
    emit_clause(&big);
    out
}

fn encode_or_all(
    lits: &[i32],
    tru: i32,
    next_var: &mut i32,
    mut emit_clause: impl FnMut(&[i32]),
) -> i32 {
    if lits.is_empty() {
        return -tru;
    }
    if lits.len() == 1 {
        return lits[0];
    }
    let out = *next_var;
    *next_var += 1;
    let mut big = Vec::with_capacity(lits.len() + 1);
    big.push(-out);
    for &l in lits {
        emit_clause(&[out, -l]);
        big.push(l);
    }
    emit_clause(&big);
    out
}

/// Run McMillan's IMC algorithm on prepared AIG `g`.
pub fn run_imc(g: &Prepared, max_depth: Option<usize>, ctx: &RunContext) -> ImcOutcome {
    if !Interpolator::available() {
        return ImcOutcome::Aborted("CaDiCaL proof tracer unavailable".into());
    }
    if g.latches.iter().any(|l| matches!(l.init, Init::Expr(_))) {
        return ImcOutcome::Aborted("latch inits that are not constants".into());
    }
    let tpl = cnf::template(g);

    // Depth-0 base check: `Init(s_0) & C(s_0, i_0) & Bad(s_0, i_0)`.
    match check_depth_zero(g, &tpl, ctx) {
        DepthZero::Unsafe(trace, witness) => {
            return ImcOutcome::Unsafe {
                depth: 0,
                trace,
                witness,
            };
        }
        DepthZero::Safe => {
            if g.latches.is_empty() {
                return ImcOutcome::Safe(Vec::new());
            }
        }
        DepthZero::Aborted(r) => return ImcOutcome::Aborted(r),
    }

    let (mut hub, mut standing) = seed_from_hub(g, ctx, "informal-imc");
    let latch_idx: HashMap<u32, usize> = g
        .latches
        .iter()
        .enumerate()
        .map(|(i, l)| (var(l.lit), i))
        .collect();
    let mut inv_idx = cubes_to_latch_idx(&standing, &latch_idx);

    let mut k = 1usize;
    loop {
        if ctx.should_stop() {
            return ImcOutcome::Aborted(ctx.stop_reason().into());
        }
        if max_depth.is_some_and(|m| k > m) {
            return ImcOutcome::Aborted(format!("bound {} reached", k - 1));
        }
        if let Some(h) = &mut hub {
            if h.has_new_standing() {
                let added = h.poll_standing_verified(g, &standing, ctx, Duration::from_millis(50));
                if !added.is_empty() {
                    inv_idx.extend(cubes_to_latch_idx(&added, &latch_idx));
                    standing.extend(added);
                }
            }
        }
        ctx.report(Progress {
            engine: "informal-imc".into(),
            bound: Some(k as u32),
            note: format!("imc k={k}"),
            output: None,
        });

        let mut saig = StateAig::new(g.latches.len());
        let mut r_root = saig.encode_init(g);

        for iter in 0..MAX_INNER_ITERS {
            if ctx.should_stop() {
                return ImcOutcome::Aborted(ctx.stop_reason().into());
            }
            match solve_partition(g, &tpl, &saig, r_root, &inv_idx, k, iter == 0, ctx) {
                PartResult::RealCex {
                    depth,
                    trace,
                    witness,
                } => {
                    return ImcOutcome::Unsafe {
                        depth,
                        trace,
                        witness,
                    };
                }
                PartResult::Spurious => break, // widen k
                PartResult::Interpolant(itp, s1_to_latch) => {
                    let i_root = saig.import_interp(&itp, &s1_to_latch);
                    if is_contained(g, &tpl, &saig, i_root, r_root, &inv_idx, ctx) {
                        // Fixpoint reached: extract minimal CNF blocked cubes for `R & standing`.
                        return match extract_blocked_cubes(g, &saig, r_root, ctx) {
                            Ok(mut lemmas) => {
                                lemmas.extend(standing.iter().cloned());
                                ImcOutcome::Safe(lemmas)
                            }
                            Err(reason) => ImcOutcome::Aborted(reason),
                        };
                    }
                    r_root = saig.mk_or(r_root, i_root);
                }
                PartResult::Aborted(r) => return ImcOutcome::Aborted(r),
            }
        }
        k += 1;
    }
}

fn cubes_to_latch_idx(
    cubes: &[ic3::Cube],
    latch_idx: &HashMap<u32, usize>,
) -> Vec<Vec<(usize, bool)>> {
    cubes
        .iter()
        .filter_map(|c| {
            c.iter()
                .map(|&l| Some((*latch_idx.get(&var(l))?, is_neg(l))))
                .collect()
        })
        .collect()
}

fn push_inv_clauses(inv_idx: &[Vec<(usize, bool)>], svars: &[i32], clauses: &mut Vec<Vec<i32>>) {
    for cube in inv_idx {
        let cl: Vec<i32> = cube
            .iter()
            .map(|&(li, neg)| if neg { svars[li] } else { -svars[li] })
            .collect();
        clauses.push(cl);
    }
}

enum DepthZero {
    Safe,
    Unsafe(PreparedTrace, String),
    Aborted(String),
}

fn check_depth_zero(g: &Aig, tpl: &Template, ctx: &RunContext) -> DepthZero {
    let mut sat = solver(ctx);
    let mut next_var = 2i32;
    let tru = 1i32;
    sat.reserve(1);
    sat.add_clause(&[tru]);
    let latch_vars: Vec<i32> = g
        .latches
        .iter()
        .map(|l| match l.init {
            Init::Zero => -tru,
            Init::One => tru,
            Init::Free | Init::Expr(_) => {
                let x = next_var;
                next_var += 1;
                x
            }
        })
        .collect();
    let mut clauses: Vec<Vec<i32>> = Vec::new();
    let (f0, in0) = encode_frame(g, tpl, &latch_vars, tru, &mut next_var, |c| {
        clauses.push(c.to_vec());
    });
    for &c in &g.constraints {
        clauses.push(vec![flit(&f0, c)]);
    }
    let bad_lits: Vec<i32> = g.bads.iter().map(|&b| flit(&f0, b)).collect();
    clauses.push(bad_lits);
    sat.reserve(next_var);
    for c in &clauses {
        sat.add_clause(c);
    }
    match sat.solve(&[]) {
        Answer::Unsat => DepthZero::Safe,
        Answer::Sat => {
            let init: Vec<bool> = latch_vars.iter().map(|&v| sat.value(v)).collect();
            let inputs = vec![in0.iter().map(|&v| sat.value(v)).collect()];
            let witness = format_named_witness(g, &init, &inputs);
            DepthZero::Unsafe(PreparedTrace { init, inputs }, witness)
        }
        Answer::Unknown => DepthZero::Aborted(ctx.stop_reason().into()),
    }
}

enum PartResult {
    RealCex {
        depth: usize,
        trace: PreparedTrace,
        witness: String,
    },
    Spurious,
    Interpolant(InterpAig, HashMap<i32, usize>),
    Aborted(String),
}

#[allow(clippy::too_many_arguments)]
fn solve_partition(
    g: &Aig,
    tpl: &Template,
    saig: &StateAig,
    r_root: u32,
    inv_idx: &[Vec<(usize, bool)>],
    k: usize,
    is_exact_init: bool,
    ctx: &RunContext,
) -> PartResult {
    let mut itp = match Interpolator::new() {
        Ok(i) => i,
        Err(e) => return PartResult::Aborted(e),
    };
    let c = ctx.clone();
    itp.set_terminate(Box::new(move || c.should_stop()));

    let mut next_var = 1i32;
    let fresh = |nv: &mut i32| -> i32 {
        let x = *nv;
        *nv += 1;
        x
    };

    // Shared interface variables `s1[0..n]` (the ONLY variables shared between A and B).
    let n = g.latches.len();
    let s1_vars: Vec<i32> = (0..n).map(|_| fresh(&mut next_var)).collect();
    let s1_to_latch: HashMap<i32, usize> =
        s1_vars.iter().enumerate().map(|(li, &v)| (v, li)).collect();

    // --- Partition A: R(s_0) & C(s_0, i_0) & (s_1 <=> next(s_0, i_0)) ---
    let mut a_clauses: Vec<Vec<i32>> = Vec::new();
    let tru_a = fresh(&mut next_var);
    a_clauses.push(vec![tru_a]);
    let s0_vars: Vec<i32> = (0..n).map(|_| fresh(&mut next_var)).collect();
    let (f0, in0) = encode_frame(g, tpl, &s0_vars, tru_a, &mut next_var, |cl| {
        a_clauses.push(cl.to_vec());
    });
    let r_lit = saig.encode_roots(&[r_root], &s0_vars, tru_a, &mut next_var, |cl| {
        a_clauses.push(cl.to_vec());
    })[0];
    a_clauses.push(vec![r_lit]);
    push_inv_clauses(inv_idx, &s0_vars, &mut a_clauses);
    for &con in &g.constraints {
        a_clauses.push(vec![flit(&f0, con)]);
    }
    for (li, l) in g.latches.iter().enumerate() {
        let nxt = flit(&f0, l.next);
        let s1 = s1_vars[li];
        a_clauses.push(vec![-s1, nxt]);
        a_clauses.push(vec![s1, -nxt]);
    }

    // --- Partition B: frames 1..=k with `alive_t` prefix constraints & bad disjunction ---
    let mut b_clauses: Vec<Vec<i32>> = Vec::new();
    let tru_b = fresh(&mut next_var);
    b_clauses.push(vec![tru_b]);
    push_inv_clauses(inv_idx, &s1_vars, &mut b_clauses);
    let mut prev_latches = s1_vars.clone();
    let mut frame_inputs: Vec<Vec<i32>> = Vec::with_capacity(k);
    let mut hit_lits: Vec<i32> = Vec::with_capacity(k);
    let mut prev_alive = tru_b;

    for _t in 1..=k {
        let (ft, in_t) = encode_frame(g, tpl, &prev_latches, tru_b, &mut next_var, |cl| {
            b_clauses.push(cl.to_vec());
        });
        let c_lits: Vec<i32> = g.constraints.iter().map(|&con| flit(&ft, con)).collect();
        let c_all = encode_and_all(&c_lits, tru_b, &mut next_var, |cl| {
            b_clauses.push(cl.to_vec());
        });
        let alive = encode_and_all(&[prev_alive, c_all], tru_b, &mut next_var, |cl| {
            b_clauses.push(cl.to_vec());
        });
        let bad_lits: Vec<i32> = g.bads.iter().map(|&b| flit(&ft, b)).collect();
        let bad_any = encode_or_all(&bad_lits, tru_b, &mut next_var, |cl| {
            b_clauses.push(cl.to_vec());
        });
        let hit = encode_and_all(&[alive, bad_any], tru_b, &mut next_var, |cl| {
            b_clauses.push(cl.to_vec());
        });
        hit_lits.push(hit);
        frame_inputs.push(in_t);
        prev_alive = alive;
        prev_latches = g.latches.iter().map(|l| flit(&ft, l.next)).collect();
    }
    b_clauses.push(hit_lits.clone());

    itp.reserve(next_var);
    for cl in &a_clauses {
        itp.add_a(cl);
    }
    for cl in &b_clauses {
        itp.add_b(cl);
    }

    match itp.solve() {
        Answer::Sat => {
            if !is_exact_init {
                return PartResult::Spurious;
            }
            let d = hit_lits
                .iter()
                .position(|&h| itp.value(h))
                .map_or(k, |idx| idx + 1);
            let init: Vec<bool> = s0_vars.iter().map(|&v| itp.value(v)).collect();
            let mut inputs = Vec::with_capacity(d + 1);
            inputs.push(in0.iter().map(|&v| itp.value(v)).collect());
            for in_t in frame_inputs.iter().take(d) {
                inputs.push(in_t.iter().map(|&v| itp.value(v)).collect());
            }
            let witness = format_named_witness(g, &init, &inputs);
            PartResult::RealCex {
                depth: d,
                trace: PreparedTrace { init, inputs },
                witness,
            }
        }
        Answer::Unsat => match itp.extract_interpolant() {
            Ok(aig) => PartResult::Interpolant(aig, s1_to_latch),
            Err(e) => PartResult::Aborted(e),
        },
        Answer::Unknown => PartResult::Aborted(ctx.stop_reason().into()),
    }
}

/// Checks whether `I(s) & standing(s) & C(s, i) => R(s)` (i.e. `I & standing & C & !R` is UNSAT).
fn is_contained(
    g: &Aig,
    tpl: &Template,
    saig: &StateAig,
    i_root: u32,
    r_root: u32,
    inv_idx: &[Vec<(usize, bool)>],
    ctx: &RunContext,
) -> bool {
    if i_root == 0 || r_root == 1 || i_root == r_root {
        return true;
    }
    let mut sat = solver(ctx);
    let mut next_var = 2i32;
    let tru = 1i32;
    let mut clauses = vec![vec![tru]];
    let s_vars: Vec<i32> = (0..g.latches.len())
        .map(|_| {
            let x = next_var;
            next_var += 1;
            x
        })
        .collect();
    push_inv_clauses(inv_idx, &s_vars, &mut clauses);
    if !g.constraints.is_empty() {
        let (f, _) = encode_frame(g, tpl, &s_vars, tru, &mut next_var, |c| {
            clauses.push(c.to_vec());
        });
        for &con in &g.constraints {
            clauses.push(vec![flit(&f, con)]);
        }
    }
    let roots = saig.encode_roots(&[i_root, r_root], &s_vars, tru, &mut next_var, |c| {
        clauses.push(c.to_vec());
    });
    clauses.push(vec![roots[0]]);
    clauses.push(vec![-roots[1]]);
    sat.reserve(next_var);
    for c in &clauses {
        sat.add_clause(c);
    }
    matches!(sat.solve(&[]), Answer::Unsat)
}

/// Convert exact state predicate `R(s)` into minimal blocked cubes over
/// `g.latches` whose conjunction of negations equals `R(s)`.
fn extract_blocked_cubes(
    g: &Aig,
    saig: &StateAig,
    r_root: u32,
    ctx: &RunContext,
) -> Result<Vec<ic3::Cube>, String> {
    if r_root == 1 {
        return Ok(Vec::new());
    }
    if r_root == 0 {
        return Ok(vec![Vec::new()]);
    }
    let n = g.latches.len();
    let build_solver = |assert_r: bool| -> (Box<dyn Sat>, Vec<i32>) {
        let mut sat = solver(ctx);
        let mut next_var = 2i32;
        let tru = 1i32;
        let mut clauses = vec![vec![tru]];
        let s_vars: Vec<i32> = (0..n)
            .map(|_| {
                let x = next_var;
                next_var += 1;
                x
            })
            .collect();
        let r_lit = saig.encode_roots(&[r_root], &s_vars, tru, &mut next_var, |c| {
            clauses.push(c.to_vec());
        })[0];
        clauses.push(vec![if assert_r { r_lit } else { -r_lit }]);
        sat.reserve(next_var);
        for c in &clauses {
            sat.add_clause(c);
        }
        (sat, s_vars)
    };

    let (mut enum_sat, enum_s) = build_solver(false);
    let (mut gen_sat, gen_s) = build_solver(true);
    let mut lemmas: Vec<ic3::Cube> = Vec::new();

    loop {
        if ctx.should_stop() {
            return Err(ctx.stop_reason().into());
        }
        if lemmas.len() >= MAX_CERT_CUBES {
            return Err("imc invariant cube limit reached".into());
        }
        match enum_sat.solve(&[]) {
            Answer::Unsat => return Ok(lemmas),
            Answer::Unknown => return Err(ctx.stop_reason().into()),
            Answer::Sat => {
                // Full state cube in `!R`, generalized via UNSAT core on `R & cube`.
                let mut model_lits: Vec<(usize, bool)> =
                    (0..n).map(|li| (li, enum_sat.value(enum_s[li]))).collect();
                let assume: Vec<i32> = model_lits
                    .iter()
                    .map(|&(li, val)| if val { gen_s[li] } else { -gen_s[li] })
                    .collect();
                if matches!(gen_sat.solve(&assume), Answer::Unsat) {
                    model_lits.retain(|&(li, val)| {
                        let sl = if val { gen_s[li] } else { -gen_s[li] };
                        gen_sat.failed(sl)
                    });
                }
                // Greedy literal dropping for compact prime blocked cubes.
                let mut i = 0;
                while i < model_lits.len() {
                    if ctx.should_stop() {
                        return Err(ctx.stop_reason().into());
                    }
                    let trial: Vec<i32> = model_lits
                        .iter()
                        .enumerate()
                        .filter(|&(j, _)| j != i)
                        .map(|(_, &(li, val))| if val { gen_s[li] } else { -gen_s[li] })
                        .collect();
                    if matches!(gen_sat.solve(&trial), Answer::Unsat) {
                        model_lits.remove(i);
                    } else {
                        i += 1;
                    }
                }

                let block_clause: Vec<i32> = model_lits
                    .iter()
                    .map(|&(li, val)| if val { -enum_s[li] } else { enum_s[li] })
                    .collect();
                enum_sat.add_clause(&block_clause);

                let mut cube: ic3::Cube = model_lits
                    .into_iter()
                    .map(|(li, val)| {
                        let l = g.latches[li].lit;
                        if val {
                            l
                        } else {
                            informal_aig::neg(l)
                        }
                    })
                    .collect();
                cube.sort_unstable();
                lemmas.push(cube);
            }
        }
    }
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
