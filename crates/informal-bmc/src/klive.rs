//! Claessen–Sörensson `k`-Liveness & Stabilizing Streak engine (`informal-klive`).
//!
//! Biere–Schuppan liveness-to-safety (`informal_aig::l2s`) duplicates every
//! design latch into a shadow latch, forcing IC3 to unroll equality over wide
//! timers/dividers. `informal-klive` avoids shadow state copies completely:
//!
//! 1. **Certified One-Shot / Persistence Streak (`FG h`):** Single-shot SVA
//!    liveness monitors (`_sva_watch` / `_sva_picked`) and stabilizing Buchi
//!    conditions can never transition `1 -> ... -> 0 -> ... -> 1`. We check
//!    this 2-latch trap property (`dropped && h`) with IC3 and certify its
//!    inductive invariant via `certify::check`. Once certified, any infinite
//!    fair trace must satisfy `h` on *every* step from its first hit onwards,
//!    so proving that no `(K + 1)`-step constrained trajectory keeps `h` high
//!    (verified by an independent Tseitin SAT query) proves `Safe` without
//!    adding counter latches.
//! 2. **General Claessen–Sörensson `k`-Liveness (FMCAD 2012):** For general
//!    multi-condition or recurring Buchi properties (`1 -> 0 -> 1`), reduces
//!    persistence to pure safety via a `ceil(log2(k + 1))`-bit saturating
//!    counter (`bad = (cnt == k) && hit`) with round-robin fairness reduction,
//!    verifying every `Safe` invariant via `certify::check` and every `Unsafe`
//!    lasso via `l2s::check_lasso` + `certify::replay_unsafe`.

use crate::unroll::Unroll;
use crate::{certify, ic3, report_certificate, result, solver, unknown, Checked, FORMATS};
use informal_aig::{lit_val, neg, var, Aig, Init, Lit, FALSE, TRUE};
use informal_engine::{
    Capabilities, Certificate, CheckRequest, Engine, EngineResult, Progress, RunContext, Verdict,
};
use informal_sat::Answer;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub struct KLive;

fn or(g: &mut Aig, a: Lit, b: Lit) -> Lit {
    neg(g.and(neg(a), neg(b)))
}

/// Build the `k`-liveness safety AIG for bound `k` from `raw_g`.
/// Keeps `raw_g.bads` first, then appends one counter-overflow `bad` per
/// justice property in `raw_g.justice`. Adds zero new primary inputs.
fn build_klive_aig(raw_g: &Aig, k: u32) -> Aig {
    let mut g = raw_g.clone();
    let justice = std::mem::take(&mut g.justice);
    let fairness = std::mem::take(&mut g.fairness);

    for conds in &justice {
        let all: Vec<Lit> = conds.iter().chain(&fairness).copied().collect();
        let hit = match all.as_slice() {
            [] => TRUE,
            [single] => *single,
            many => {
                let seen: Vec<Lit> = (0..many.len()).map(|_| g.latch(Init::Zero)).collect();
                let now: Vec<Lit> = seen
                    .iter()
                    .zip(many)
                    .map(|(&s, &c)| or(&mut g, s, c))
                    .collect();
                let mut all_now = TRUE;
                for &n in &now {
                    all_now = g.and(all_now, n);
                }
                let keep_round = neg(all_now);
                for (&s, &n) in seen.iter().zip(&now) {
                    let nxt = g.and(keep_round, n);
                    g.set_next(s, nxt);
                }
                all_now
            }
        };

        let cnt_bad = if k == 0 {
            hit
        } else {
            let w = (u32::BITS - k.leading_zeros()) as usize;
            let cnt: Vec<Lit> = (0..w).map(|_| g.latch(Init::Zero)).collect();
            let mut at_k = TRUE;
            for (b, &cb) in cnt.iter().enumerate() {
                let bit_lit = if (k >> b) & 1 == 1 { cb } else { neg(cb) };
                at_k = g.and(at_k, bit_lit);
            }
            let overflow = g.and(at_k, hit);
            let inc = g.and(hit, neg(at_k));
            let mut carry = inc;
            for &cb in &cnt {
                let nxt = g.xor(cb, carry);
                carry = g.and(cb, carry);
                g.set_next(cb, nxt);
            }
            overflow
        };
        g.bads.push(cnt_bad);
    }
    g
}

fn format_named_witness(g: &Aig, tr: &certify::Trace, d: usize) -> String {
    let bit = |b: bool| if b { '1' } else { '0' };
    let mut s = format!("informalw 1\ndepth {d}\ninit");
    for (idx, l) in g.latches.iter().enumerate() {
        if matches!(l.init, Init::Zero | Init::One) {
            continue;
        }
        if let Some(name) = g.names.get(&var(l.lit)) {
            let b = tr.init.get(idx).copied().unwrap_or(false);
            s += &format!(" {name}={}", bit(b));
        }
    }
    for f in 0..=d {
        s += &format!("\n@{f}");
        let row = tr.inputs.get(f).map(Vec::as_slice).unwrap_or(&[]);
        for (idx, &inp) in g.inputs.iter().enumerate() {
            if let Some(name) = g.names.get(&var(inp)) {
                let b = row.get(idx).copied().unwrap_or(false);
                s += &format!(" {name}={}", bit(b));
            }
        }
    }
    s.push('\n');
    s
}

/// Inspect a counterexample trace `(klive_init, inputs[0..=d])` on `klive_g`:
/// returns `Some((verdict, cert, witness_str))` if it either violates a raw
/// safety `bad` on `raw_g` or contains a valid liveness lasso on `raw_g`.
fn try_extract_counterexample(
    req: &CheckRequest,
    raw_g: &Aig,
    klive_init: &[bool],
    inputs: &[Vec<bool>],
) -> Option<(Verdict, Option<Certificate>, Option<String>)> {
    let raw_latches = raw_g.latches.len();
    let raw_init: Vec<bool> = klive_init[..raw_latches].to_vec();
    let mut state = raw_init.clone();
    let mut states_by_step: HashMap<Vec<bool>, Vec<usize>> = HashMap::new();
    let mut safety_hit_depth: Option<usize> = None;

    for (t, step_in) in inputs.iter().enumerate() {
        states_by_step.entry(state.clone()).or_default().push(t);
        let vals = informal_aig::l2s::eval(raw_g, &state, step_in);
        if raw_g.constraints.iter().any(|&c| !lit_val(&vals, c)) {
            return None;
        }
        if safety_hit_depth.is_none() && raw_g.bads.iter().any(|&b| lit_val(&vals, b)) {
            safety_hit_depth = Some(t);
            break;
        }
        state = raw_g
            .latches
            .iter()
            .map(|l| lit_val(&vals, l.next))
            .collect();
    }

    let orig = informal_aig::load(&req.model).ok()?;

    if let Some(d) = safety_hit_depth {
        let mut orig_init = raw_init;
        orig_init.resize(orig.latches.len(), false);
        let orig_inputs: Vec<Vec<bool>> = inputs[..=d]
            .iter()
            .map(|row| {
                let mut r = row.clone();
                r.resize(orig.inputs.len(), false);
                r
            })
            .collect();
        let tr = certify::Trace {
            init: orig_init,
            inputs: orig_inputs,
        };
        if std::env::var_os("INFORMAL_NO_CERT").is_none()
            && certify::replay_unsafe(&orig, d, &tr).is_err()
        {
            return None;
        }
        let w = format_named_witness(&orig, &tr, d);
        let cert = Certificate::Trace {
            init: informal_engine::certificate::bits(tr.init),
            inputs: tr
                .inputs
                .into_iter()
                .map(informal_engine::certificate::bits)
                .collect(),
        };
        return Some((
            Verdict::Unsafe {
                depth: Some(d as u32),
            },
            Some(cert),
            Some(w),
        ));
    }

    states_by_step.entry(state).or_default().push(inputs.len());

    for steps in states_by_step.values() {
        if steps.len() < 2 {
            continue;
        }
        for (idx_a, &start) in steps.iter().enumerate() {
            for &end in &steps[idx_a + 1..] {
                if end > inputs.len() || start >= end {
                    continue;
                }
                for prop_idx in 0..raw_g.justice.len() {
                    if informal_aig::l2s::check_lasso(
                        raw_g,
                        prop_idx,
                        &raw_init,
                        &inputs[..end],
                        start,
                    )
                    .is_ok()
                    {
                        let mut orig_init = raw_init.clone();
                        orig_init.resize(orig.latches.len(), false);
                        let mut orig_inputs: Vec<Vec<bool>> = Vec::with_capacity(end + 1);
                        for (step, row) in inputs[..end].iter().enumerate() {
                            let mut r = row.clone();
                            if orig.inputs.len() > raw_g.inputs.len() {
                                r.push(step == start);
                                r.resize(orig.inputs.len(), false);
                            }
                            orig_inputs.push(r);
                        }
                        let mut close_in = inputs[start].clone();
                        if orig.inputs.len() > raw_g.inputs.len() {
                            close_in.push(false);
                            close_in.resize(orig.inputs.len(), false);
                        }
                        orig_inputs.push(close_in);

                        let tr = certify::Trace {
                            init: orig_init,
                            inputs: orig_inputs,
                        };
                        if std::env::var_os("INFORMAL_NO_CERT").is_none()
                            && certify::replay_unsafe(&orig, end, &tr).is_err()
                        {
                            continue;
                        }
                        let w = format_named_witness(&orig, &tr, end);
                        let cert = Certificate::Trace {
                            init: informal_engine::certificate::bits(tr.init),
                            inputs: tr
                                .inputs
                                .into_iter()
                                .map(informal_engine::certificate::bits)
                                .collect(),
                        };
                        return Some((
                            Verdict::Unsafe {
                                depth: Some(end as u32),
                            },
                            Some(cert),
                            Some(w),
                        ));
                    }
                }
            }
        }
    }
    None
}

/// Prove that `raw_g.bads` (if any, e.g. `s_until` safety side) are unreachable,
/// returning `Some(Ok(()))` when certified safe, `Some(Err(res))` on counterexample,
/// or `None` if undecided within `budget`.
fn prove_raw_safety_bads(
    req: &CheckRequest,
    raw_g: &Aig,
    ctx: &RunContext,
    t0: Instant,
) -> Option<Result<(), EngineResult>> {
    if raw_g.bads.is_empty() {
        return Some(Ok(()));
    }
    let mut g = raw_g.clone();
    g.justice.clear();
    g.fairness.clear();
    let (simp, _, map, _) = informal_aig::preprocess::simplify_maps(&g);
    if simp.bads.iter().all(|&b| b == FALSE) {
        let ok = std::env::var_os("INFORMAL_NO_CERT").is_some()
            || certify::lift(&g, &map, &simp, &[])
                .and_then(|inv| certify::check(&g, &inv, &|| solver(ctx)))
                .is_ok();
        return ok.then_some(Ok(()));
    }
    let mut sctx = ctx.clone();
    let left = ctx.deadline.saturating_duration_since(Instant::now());
    sctx.deadline = Instant::now() + (left / 4).max(Duration::from_millis(200));
    let c_make = sctx.clone();
    let cfg = ic3::Config {
        houdini_every: 4,
        ..ic3::Config::default()
    };
    let mut ic3 = ic3::Ic3::with_config(&simp, Box::new(move || solver(&c_make)), cfg)?;
    match ic3.check(&sctx) {
        Verdict::Safe => {
            if std::env::var_os("INFORMAL_NO_CERT").is_none() {
                let lemmas = ic3.invariant()?;
                let inv = certify::lift(&g, &map, &simp, &lemmas).ok()?;
                certify::check(&g, &inv, &|| solver(ctx)).ok()?;
            }
            Some(Ok(()))
        }
        Verdict::Unsafe { depth: Some(d) } if d <= 64 => {
            let tr =
                certify::find_unsafe(&g, d as usize, &|| solver(&sctx), &|| sctx.should_stop())
                    .ok()?;
            let (verdict, cert, witness) =
                try_extract_counterexample(req, raw_g, &tr.init, &tr.inputs)?;
            Some(Err(EngineResult {
                engine: "informal-klive".into(),
                verdict,
                wall_ms: t0.elapsed().as_millis(),
                witness,
                certificate: cert,
            }))
        }
        _ => None,
    }
}

/// Plain independent Tseitin check (without `cnf::template` or preprocessing)
/// that no `(depth + 1)`-step constrained trajectory of `raw_g` (from any
/// state) satisfies `h` at every step `0..=depth`.
fn verify_no_streak(raw_g: &Aig, h: Lit, depth: usize, ctx: &RunContext) -> Result<(), String> {
    use informal_aig::{is_neg, Node};
    let mut sat = solver(ctx);
    sat.reserve(1);
    let tru: informal_sat::Lit = 1;
    sat.add_clause(&[tru]);
    let mut next_v: informal_sat::Lit = 2;
    let mut prev_frame: Vec<informal_sat::Lit> = Vec::new();
    let map_lit = |frame: &[informal_sat::Lit], l: Lit| -> informal_sat::Lit {
        let v = frame[var(l) as usize];
        if is_neg(l) {
            -v
        } else {
            v
        }
    };
    for f in 0..=depth {
        if ctx.should_stop() {
            return Err("streak certificate check stopped".into());
        }
        sat.reserve(raw_g.nodes.len() as i32);
        let mut cur = vec![0; raw_g.nodes.len()];
        let mut li = 0usize;
        for (vi, node) in raw_g.nodes.iter().enumerate() {
            cur[vi] = match *node {
                Node::Const => -tru,
                Node::Input => {
                    let v = next_v;
                    next_v += 1;
                    v
                }
                Node::Latch => {
                    let latch = &raw_g.latches[li];
                    li += 1;
                    if f == 0 {
                        let v = next_v;
                        next_v += 1;
                        v
                    } else {
                        map_lit(&prev_frame, latch.next)
                    }
                }
                Node::And(a, b) => {
                    let v = next_v;
                    next_v += 1;
                    let (sa, sb) = (map_lit(&cur, a), map_lit(&cur, b));
                    sat.add_clause(&[-v, sa]);
                    sat.add_clause(&[-v, sb]);
                    sat.add_clause(&[v, -sa, -sb]);
                    v
                }
            };
        }
        for &c in &raw_g.constraints {
            sat.add_clause(&[map_lit(&cur, c)]);
        }
        sat.add_clause(&[map_lit(&cur, h)]);
        prev_frame = cur;
    }
    match sat.solve(&[]) {
        Answer::Unsat => Ok(()),
        Answer::Unknown => Err("streak certificate check stopped".into()),
        Answer::Sat => Err("streak certificate failed: trajectory exists".into()),
    }
}

/// Try to prove a single-condition justice obligation `h` by:
/// 1. Certifying via IC3 + `certify::check` that `h` never re-enters `1` after
///    dropping `1 -> 0` (`dropped && h` unreachable), reducing `GF h` to `FG h`.
/// 2. Showing via incremental unrolling from `free_init = true` (independently
///    re-checked by [`verify_no_streak`]) that `h` cannot hold for `K + 1`
///    consecutive constrained steps from any state.
fn try_prove_contiguous_streak(raw_g: &Aig, h: Lit, ctx: &RunContext) -> bool {
    let mut g_re = raw_g.clone();
    g_re.bads.clear();
    g_re.justice.clear();
    g_re.fairness.clear();
    let ever_h = g_re.latch(Init::Zero);
    let dropped = g_re.latch(Init::Zero);
    let next_ever = or(&mut g_re, ever_h, h);
    let fell = g_re.and(ever_h, neg(h));
    let next_dropped = or(&mut g_re, dropped, fell);
    g_re.set_next(ever_h, next_ever);
    g_re.set_next(dropped, next_dropped);
    let reenter = g_re.and(dropped, h);
    g_re.bads.push(reenter);

    let (simp, _, map, _) = informal_aig::preprocess::simplify_maps(&g_re);
    let mut raw_inv: Vec<certify::Clause> = Vec::new();
    if simp.bads.iter().all(|&b| b == FALSE) {
        if std::env::var_os("INFORMAL_NO_CERT").is_none() {
            let Ok(inv) = certify::lift(&g_re, &map, &simp, &[]) else {
                return false;
            };
            if certify::check(&g_re, &inv, &|| solver(ctx)).is_err() {
                return false;
            }
        }
    } else {
        let mut rctx = ctx.clone();
        let left = ctx.deadline.saturating_duration_since(Instant::now());
        rctx.deadline = Instant::now() + (left / 6).min(Duration::from_millis(600));

        // Seed IC3 with 1- and 2-literal inductive invariants (Houdini) over
        // the monitor latches at the tail of `simp.latches` (`picked`, `watch`,
        // `ever_h`, `dropped`), avoiding backward unrolling of wide RTL timers.
        let mut seed_cubes: Vec<ic3::Cube> = Vec::new();
        let cand_latches: Vec<(Lit, bool)> = simp
            .latches
            .iter()
            .rev()
            .filter_map(|l| match l.init {
                Init::Zero => Some((l.lit, false)),
                Init::One => Some((l.lit, true)),
                _ => None,
            })
            .take(16)
            .collect();
        let mut cands: Vec<ic3::Cube> = Vec::new();
        for &(lit, init_one) in &cand_latches {
            // Cube literal that is TRUE when the latch differs from its init:
            cands.push(vec![if init_one { neg(lit) } else { lit }]);
        }
        for i in 0..cand_latches.len() {
            for j in (i + 1)..cand_latches.len() {
                let (li, ii) = cand_latches[i];
                let (lj, ij) = cand_latches[j];
                for &a in &[li, neg(li)] {
                    let a_at_init = if a == li { ii } else { !ii };
                    for &b in &[lj, neg(lj)] {
                        let b_at_init = if b == lj { ij } else { !ij };
                        if !(a_at_init && b_at_init) {
                            let mut cube = if a < b { vec![a, b] } else { vec![b, a] };
                            cube.dedup();
                            cands.push(cube);
                        }
                    }
                }
            }
        }
        if !cands.is_empty() {
            if let Some(core) = crate::invariant::houdini(&simp, cands, || solver(&rctx), &rctx) {
                seed_cubes = core;
            }
        }

        let c_make = rctx.clone();
        let cfg = ic3::Config {
            houdini_every: 4,
            ..ic3::Config::default()
        };
        let Some(mut ic3) = ic3::Ic3::with_config(&simp, Box::new(move || solver(&c_make)), cfg)
        else {
            return false;
        };
        if !seed_cubes.is_empty() {
            ic3.assume_invariant(seed_cubes);
        }
        if ic3.check(&rctx) != Verdict::Safe {
            return false;
        }
        let Some(lemmas) = ic3.invariant() else {
            return false;
        };
        let Ok(inv) = certify::lift(&g_re, &map, &simp, &lemmas) else {
            return false;
        };
        if std::env::var_os("INFORMAL_NO_CERT").is_none()
            && certify::check(&g_re, &inv, &|| solver(ctx)).is_err()
        {
            return false;
        }
        // Keep inductive clauses that mention only `raw_g` latches? No: an
        // inductive invariant of `g_re` may mutually depend on `ever_h` /
        // `dropped`, so only keep clauses if we certify them on `raw_g` itself.
        let raw_vars: std::collections::HashSet<u32> =
            raw_g.latches.iter().map(|l| var(l.lit)).collect();
        let cand: Vec<certify::Clause> = inv
            .into_iter()
            .filter(|c| c.iter().all(|&l| raw_vars.contains(&var(l))))
            .collect();
        if !cand.is_empty() {
            let mut rg_only = raw_g.clone();
            rg_only.bads.clear();
            rg_only.justice.clear();
            rg_only.fairness.clear();
            if certify::check(&rg_only, &cand, &|| solver(ctx)).is_ok() {
                raw_inv = cand;
            }
        }
    }

    // Put `h` in `streak_g.constraints` so `cnf::template(&streak_g)` keeps
    // `h`'s full cone of influence live and `add_frame()` asserts `h` at each step!
    let mut streak_g = raw_g.clone();
    streak_g.bads.clear();
    streak_g.justice.clear();
    streak_g.fairness.clear();
    streak_g.constraints.push(h);

    let mut sctx = ctx.clone();
    let left = ctx.deadline.saturating_duration_since(Instant::now());
    sctx.deadline = Instant::now() + (left / 2).max(Duration::from_millis(500));
    let mut u = Unroll::new(&streak_g, solver(&sctx), true);
    let mut next_check: usize = 0;
    let max_streak: usize = 16_384;
    for f in 0..=max_streak {
        if sctx.should_stop() {
            return false;
        }
        u.add_frame();
        if f == 0 {
            for cl in &raw_inv {
                let sat_cl: Vec<informal_sat::Lit> = cl.iter().map(|&l| u.lit(0, l)).collect();
                u.sat.add_clause(&sat_cl);
            }
        }
        if f == next_check || f == max_streak {
            ctx.report(Progress {
                engine: "informal-klive".into(),
                bound: Some(f as u32),
                note: format!("streak bound {f}"),
                output: None,
            });
            match u.sat.solve(&[]) {
                Answer::Unsat => {
                    if std::env::var_os("INFORMAL_NO_CERT").is_none()
                        && verify_no_streak_with_inv(raw_g, &raw_inv, h, f, ctx).is_err()
                    {
                        return false;
                    }
                    return true;
                }
                Answer::Unknown => return false,
                Answer::Sat => {}
            }
            next_check = if next_check < 15 {
                next_check * 2 + 3
            } else {
                (next_check * 2 + 1).min(max_streak)
            };
        }
    }
    false
}

fn verify_no_streak_with_inv(
    raw_g: &Aig,
    raw_inv: &[certify::Clause],
    h: Lit,
    depth: usize,
    ctx: &RunContext,
) -> Result<(), String> {
    if raw_inv.is_empty() {
        return verify_no_streak(raw_g, h, depth, ctx);
    }
    use informal_aig::{is_neg, Node};
    // Re-verify `raw_inv` on `raw_g` before trusting it at frame 0.
    let mut rg_only = raw_g.clone();
    rg_only.bads.clear();
    rg_only.justice.clear();
    rg_only.fairness.clear();
    certify::check(&rg_only, raw_inv, &|| solver(ctx))?;

    let mut sat = solver(ctx);
    sat.reserve(1);
    let tru: informal_sat::Lit = 1;
    sat.add_clause(&[tru]);
    let mut next_v: informal_sat::Lit = 2;
    let mut prev_frame: Vec<informal_sat::Lit> = Vec::new();
    let map_lit = |frame: &[informal_sat::Lit], l: Lit| -> informal_sat::Lit {
        let v = frame[var(l) as usize];
        if is_neg(l) {
            -v
        } else {
            v
        }
    };
    for f in 0..=depth {
        if ctx.should_stop() {
            return Err("streak certificate check stopped".into());
        }
        sat.reserve(raw_g.nodes.len() as i32);
        let mut cur = vec![0; raw_g.nodes.len()];
        let mut li = 0usize;
        for (vi, node) in raw_g.nodes.iter().enumerate() {
            cur[vi] = match *node {
                Node::Const => -tru,
                Node::Input => {
                    let v = next_v;
                    next_v += 1;
                    v
                }
                Node::Latch => {
                    let latch = &raw_g.latches[li];
                    li += 1;
                    if f == 0 {
                        let v = next_v;
                        next_v += 1;
                        v
                    } else {
                        map_lit(&prev_frame, latch.next)
                    }
                }
                Node::And(a, b) => {
                    let v = next_v;
                    next_v += 1;
                    let (sa, sb) = (map_lit(&cur, a), map_lit(&cur, b));
                    sat.add_clause(&[-v, sa]);
                    sat.add_clause(&[-v, sb]);
                    sat.add_clause(&[v, -sa, -sb]);
                    v
                }
            };
        }
        if f == 0 {
            for cl in raw_inv {
                let sat_cl: Vec<informal_sat::Lit> = cl.iter().map(|&l| map_lit(&cur, l)).collect();
                sat.add_clause(&sat_cl);
            }
        }
        for &c in &raw_g.constraints {
            sat.add_clause(&[map_lit(&cur, c)]);
        }
        sat.add_clause(&[map_lit(&cur, h)]);
        prev_frame = cur;
    }
    match sat.solve(&[]) {
        Answer::Unsat => Ok(()),
        Answer::Unknown => Err("streak certificate check stopped".into()),
        Answer::Sat => Err("streak certificate failed: trajectory exists".into()),
    }
}

impl Engine for KLive {
    fn name(&self) -> &str {
        "informal-klive"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: FORMATS.to_vec(),
            proves: true,
            finds_bugs: true,
            liveness: true,
            ..Default::default()
        }
    }

    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let raw_g = match informal_aig::load_raw(&req.model) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(format!("{e:#}")), t0)),
        };
        if raw_g.justice.is_empty() {
            return Ok(result(
                self.name(),
                unknown("model has no liveness property"),
                t0,
            ));
        }
        if raw_g
            .latches
            .iter()
            .any(|l| matches!(l.init, Init::Expr(_)))
        {
            return Ok(result(
                self.name(),
                unknown("latch inits that are not constants"),
                t0,
            ));
        }

        // Fast shallow check at `k = 0` first (catches immediate safety bugs and
        // short lassos in < 2 ms before attempting safety/streak proofs).
        {
            let g0 = build_klive_aig(&raw_g, 0);
            let mut bctx = ctx.clone();
            let left = ctx.deadline.saturating_duration_since(Instant::now());
            bctx.deadline = Instant::now() + (left / 10).min(Duration::from_millis(100));
            let mut u = Unroll::new(&g0, solver(&bctx), false);
            let mut bads = Vec::new();
            for f in 0..=16 {
                if bctx.should_stop() {
                    break;
                }
                u.add_frame();
                bads.push(u.bad(f));
            }
            if !bads.is_empty() {
                let assume = u.new_var();
                let mut c = vec![-assume];
                c.extend(&bads);
                u.sat.add_clause(&c);
                if u.sat.solve(&[assume]) == Answer::Sat {
                    if let Some(d) = (0..bads.len()).find(|&f| u.sat.value(bads[f])) {
                        let init_vals: Vec<bool> = g0
                            .latches
                            .iter()
                            .map(|l| u.sat.value(u.lit(0, l.lit)))
                            .collect();
                        let in_vals: Vec<Vec<bool>> = (0..=d)
                            .map(|f| {
                                g0.inputs
                                    .iter()
                                    .map(|&i| u.sat.value(u.lit(f, i)))
                                    .collect()
                            })
                            .collect();
                        if let Some((verdict, cert, witness)) =
                            try_extract_counterexample(req, &raw_g, &init_vals, &in_vals)
                        {
                            return Ok(EngineResult {
                                engine: self.name().into(),
                                verdict,
                                wall_ms: t0.elapsed().as_millis(),
                                witness,
                                certificate: cert,
                            });
                        }
                    }
                }
            }
        }

        // Prong 1: Certified One-Shot Contiguous Streak (`FG h`) when fairness is
        // empty and every justice property is a single literal.
        if raw_g.fairness.is_empty() && raw_g.justice.iter().all(|j| j.len() == 1) {
            match prove_raw_safety_bads(req, &raw_g, ctx, t0) {
                Some(Err(cex)) => return Ok(cex),
                Some(Ok(())) => {
                    let all_streaks_safe = raw_g
                        .justice
                        .iter()
                        .all(|j| try_prove_contiguous_streak(&raw_g, j[0], ctx));
                    if all_streaks_safe {
                        return Ok(result(self.name(), Checked::new(Verdict::Safe, None), t0));
                    }
                }
                None => {}
            }
        }

        // Prong 2: General Claessen–Sörensson `k`-Liveness over IC3.
        let mut k: u32 = 0;
        let mut round: u32 = 0;
        loop {
            if ctx.should_stop() {
                return Ok(result(self.name(), unknown(ctx.stop_reason()), t0));
            }
            ctx.report(Progress {
                engine: self.name().into(),
                bound: Some(k),
                note: format!("k-liveness k={k}"),
                output: None,
            });

            let klive_g = build_klive_aig(&raw_g, k);

            if k <= 63 {
                let bmc_max = (k as usize + 6).min(72);
                let mut bmc_ctx = ctx.clone();
                let left = ctx.deadline.saturating_duration_since(Instant::now());
                bmc_ctx.deadline = Instant::now() + (left / 10).min(Duration::from_millis(120));
                let mut u = Unroll::new(&klive_g, solver(&bmc_ctx), false);
                let mut hit_at = None;
                let mut bads = Vec::with_capacity(bmc_max + 1);
                for f in 0..=bmc_max {
                    if bmc_ctx.should_stop() {
                        break;
                    }
                    u.add_frame();
                    bads.push(u.bad(f));
                }
                if !bads.is_empty() {
                    let assume = if let [b] = bads[..] {
                        b
                    } else {
                        let a = u.new_var();
                        let mut c = vec![-a];
                        c.extend(&bads);
                        u.sat.add_clause(&c);
                        a
                    };
                    if u.sat.solve(&[assume]) == Answer::Sat {
                        hit_at = (0..bads.len())
                            .find(|&f| u.sat.value(bads[f]))
                            .or(Some(bads.len() - 1));
                    }
                }
                if let Some(d) = hit_at {
                    let init_vals: Vec<bool> = klive_g
                        .latches
                        .iter()
                        .map(|l| u.sat.value(u.lit(0, l.lit)))
                        .collect();
                    let in_vals: Vec<Vec<bool>> = (0..=d)
                        .map(|f| {
                            klive_g
                                .inputs
                                .iter()
                                .map(|&i| u.sat.value(u.lit(f, i)))
                                .collect()
                        })
                        .collect();
                    if let Some((verdict, cert, witness)) =
                        try_extract_counterexample(req, &raw_g, &init_vals, &in_vals)
                    {
                        return Ok(EngineResult {
                            engine: self.name().into(),
                            verdict,
                            wall_ms: t0.elapsed().as_millis(),
                            witness,
                            certificate: cert,
                        });
                    }
                    k = k.saturating_mul(8).saturating_add(7);
                    round += 1;
                    continue;
                }
            }

            let (simp, _, map, _) = informal_aig::preprocess::simplify_maps(&klive_g);
            if simp.bads.iter().all(|&b| b == FALSE) {
                let t_cert = Instant::now();
                let r = if std::env::var_os("INFORMAL_NO_CERT").is_some() {
                    Ok(())
                } else {
                    certify::lift(&klive_g, &map, &simp, &[])
                        .and_then(|inv| certify::check(&klive_g, &inv, &|| solver(ctx)))
                };
                let v = report_certificate(req, r, t_cert, ctx);
                return Ok(result(self.name(), Checked::new(v, None), t0));
            }

            let now = Instant::now();
            let mut slice = ctx.clone();
            if round < 8 {
                let cap = Duration::from_millis(300u64.saturating_mul(1u64 << round.min(5)));
                slice.deadline = ctx.deadline.min(now + cap);
            }
            let c_make = slice.clone();
            let cfg = ic3::Config {
                houdini_every: 4,
                ..ic3::Config::default()
            };
            let Some(mut ic3) =
                ic3::Ic3::with_config(&simp, Box::new(move || solver(&c_make)), cfg)
            else {
                return Ok(result(
                    self.name(),
                    unknown("latch inits that are not constants"),
                    t0,
                ));
            };

            let v = ic3.check(&slice);
            match v {
                Verdict::Safe => {
                    let t_cert = Instant::now();
                    let r = if std::env::var_os("INFORMAL_NO_CERT").is_some() {
                        Ok(())
                    } else {
                        (|| {
                            let lemmas = ic3
                                .invariant()
                                .ok_or("missing: IC3 said safe without an invariant")?;
                            let inv = certify::lift(&klive_g, &map, &simp, &lemmas)?;
                            certify::check(&klive_g, &inv, &|| solver(ctx))
                        })()
                    };
                    let checked_v = report_certificate(req, r, t_cert, ctx);
                    return Ok(result(self.name(), Checked::new(checked_v, None), t0));
                }
                Verdict::Unsafe { depth: Some(d) } if d <= 64 => {
                    let mut fctx = ctx.clone();
                    let fleft = ctx.deadline.saturating_duration_since(Instant::now());
                    fctx.deadline = Instant::now() + (fleft / 8).min(Duration::from_millis(200));
                    if let Ok(tr) =
                        certify::find_unsafe(&klive_g, d as usize, &|| solver(&fctx), &|| {
                            fctx.should_stop()
                        })
                    {
                        if let Some((verdict, cert, witness)) =
                            try_extract_counterexample(req, &raw_g, &tr.init, &tr.inputs)
                        {
                            return Ok(EngineResult {
                                engine: self.name().into(),
                                verdict,
                                wall_ms: t0.elapsed().as_millis(),
                                witness,
                                certificate: cert,
                            });
                        }
                    }
                }
                _ => {
                    if ctx.should_stop() {
                        return Ok(result(self.name(), unknown(ctx.stop_reason()), t0));
                    }
                }
            }

            if k >= (1 << 24) {
                return Ok(result(
                    self.name(),
                    unknown("k-liveness bound limit reached"),
                    t0,
                ));
            }
            k = k.saturating_mul(8).saturating_add(7);
            round += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_engine::ModelFormat;

    fn run_btor2(tag: &str, text: &str) -> EngineResult {
        let dir = std::env::temp_dir().join(format!("informal-klive-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.btor2");
        std::fs::write(&path, text).unwrap();
        let req = CheckRequest {
            model: path,
            format: ModelFormat::Btor2,
            timeout: Duration::from_secs(30),
        };
        KLive.check(&req, &RunContext::new(req.timeout)).unwrap()
    }

    #[test]
    fn klive_proves_bounded_streak_and_extracts_lassos() {
        if informal_sat::Cadical::signature().is_none() {
            return;
        }
        // 1. Repo `liveness.btor2`: Unsafe with verified trace + witness.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/models");
        let liv = std::fs::read_to_string(root.join("liveness.btor2")).unwrap();
        let r_bad = run_btor2("repo-liv", &liv);
        assert!(matches!(r_bad.verdict, Verdict::Unsafe { .. }), "{r_bad:?}");
        assert!(r_bad.certificate.is_some());
        assert!(r_bad.witness.is_some());

        // 2. 10-bit free-running counter (`0..1023`): `justice` asks for `cnt < 400`
        //    forever (single-shot watch that stays 1 only while `cnt < 400`).
        let safe_btor = "\
1 sort bitvec 10
2 sort bitvec 1
3 zero 1
4 one 1
5 state 1 cnt
6 init 1 5 3
7 add 1 5 4
8 next 1 5 7
9 constd 1 400
10 ult 2 5 9
11 zero 2
12 input 2 pick
13 state 2 picked
14 init 2 13 11
15 state 2 watch
16 init 2 15 11
17 not 2 13
18 and 2 12 10
19 and 2 18 17
20 or 2 13 19
21 next 2 13 20
22 or 2 15 19
23 and 2 22 10
24 next 2 15 23
25 justice 1 15 j_pwm
";
        let r_safe = run_btor2("pwm-safe", safe_btor);
        assert_eq!(r_safe.verdict, Verdict::Safe);

        // 3. Combined safety (`bad`) + liveness (`justice`): safety fails at step 1!
        let both_btor = format!("{safe_btor}26 eq 2 5 4\n27 bad 26 b_step1\n");
        let r_both = run_btor2("both-unsafe", &both_btor);
        assert_eq!(r_both.verdict, Verdict::Unsafe { depth: Some(1) });

        // 4. Recurring non-contiguous Buchi + fairness (`fair` requires `!en`
        //    infinitely often, while `justice` requires `en && st` infinitely often,
        //    but `st` clears whenever `!en` holds -> `Safe` via Prong 2 `k`-Liveness!).
        let recurring_safe = "\
1 sort bitvec 1
2 zero 1
3 one 1
4 input 1 en
5 state 1 st
6 init 1 5 3
7 and 1 5 4
8 next 1 5 7
9 not 1 4
10 fair 9 f_drop
11 justice 1 7 j_both
";
        let r_rec = run_btor2("recurring-safe", recurring_safe);
        assert_eq!(r_rec.verdict, Verdict::Safe);
    }
}
