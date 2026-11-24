//! Shared lasso extraction, verification, witness formatting, and raw-safety
//! pre-checking used by both [`crate::klive`] and [`crate::rlive`].

use informal_aig::{lit_val, var, Aig, Init, FALSE};
use informal_bmc::{certify, ic3, solver};
use informal_engine::{Certificate, CheckRequest, EngineResult, RunContext, Verdict};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub(crate) fn format_named_witness(g: &Aig, tr: &certify::Trace, d: usize) -> String {
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

/// Inspect a candidate trace `(ext_init, inputs[0..=d])` over an augmented AIG
/// whose first `raw_g.latches.len()` latches match `raw_g`: returns
/// `Some((verdict, cert, witness))` if it either hits a raw safety `bad` on
/// `raw_g` or contains a verified liveness lasso on `raw_g`.
pub(crate) fn try_extract_counterexample(
    req: &CheckRequest,
    raw_g: &Aig,
    ext_init: &[bool],
    inputs: &[Vec<bool>],
) -> Option<(Verdict, Option<Certificate>, Option<String>)> {
    let raw_latches = raw_g.latches.len();
    if ext_init.len() < raw_latches {
        return None;
    }
    let raw_init: Vec<bool> = ext_init[..raw_latches].to_vec();
    let mut state = raw_init.clone();
    let mut states_by_step: HashMap<Vec<bool>, Vec<usize>> = HashMap::new();
    let mut safety_hit_depth: Option<usize> = None;

    for (t, step_in) in inputs.iter().enumerate() {
        states_by_step.entry(state.clone()).or_default().push(t);
        let raw_in = &step_in[..raw_g.inputs.len().min(step_in.len())];
        let vals = informal_aig::l2s::eval(raw_g, &state, raw_in);
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
                let mut r = row[..raw_g.inputs.len().min(row.len())].to_vec();
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
    let raw_inputs: Vec<Vec<bool>> = inputs
        .iter()
        .map(|row| row[..raw_g.inputs.len().min(row.len())].to_vec())
        .collect();

    for steps in states_by_step.values() {
        if steps.len() < 2 {
            continue;
        }
        for (idx_a, &start) in steps.iter().enumerate() {
            for &end in &steps[idx_a + 1..] {
                if end > raw_inputs.len() || start >= end {
                    continue;
                }
                for prop_idx in 0..raw_g.justice.len() {
                    if let Some(found) =
                        verify_lasso(raw_g, &orig, prop_idx, &raw_init, &raw_inputs[..end], start)
                    {
                        return Some(found);
                    }
                }
            }
        }
    }
    None
}

/// Confirm a candidate lasso `(raw_init, inputs[0..end], loop_start)` against
/// `l2s::check_lasso` and replay it on `orig` (`l2s(raw_g)`).
pub(crate) fn verify_lasso(
    raw_g: &Aig,
    orig: &Aig,
    prop_idx: usize,
    raw_init: &[bool],
    raw_inputs: &[Vec<bool>],
    start: usize,
) -> Option<(Verdict, Option<Certificate>, Option<String>)> {
    let end = raw_inputs.len();
    if start >= end {
        return None;
    }
    informal_aig::l2s::check_lasso(raw_g, prop_idx, raw_init, raw_inputs, start).ok()?;

    let mut orig_init = raw_init.to_vec();
    orig_init.resize(orig.latches.len(), false);
    let mut orig_inputs: Vec<Vec<bool>> = Vec::with_capacity(end + 1);
    for (step, row) in raw_inputs.iter().enumerate() {
        let mut r = row.clone();
        if orig.inputs.len() > raw_g.inputs.len() {
            r.push(step == start);
            r.resize(orig.inputs.len(), false);
        }
        orig_inputs.push(r);
    }
    let mut close_in = raw_inputs[start].clone();
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
        && certify::replay_unsafe(orig, end, &tr).is_err()
    {
        return None;
    }
    let w = format_named_witness(orig, &tr, end);
    let cert = Certificate::Trace {
        init: informal_engine::certificate::bits(tr.init),
        inputs: tr
            .inputs
            .into_iter()
            .map(informal_engine::certificate::bits)
            .collect(),
    };
    Some((
        Verdict::Unsafe {
            depth: Some(end as u32),
        },
        Some(cert),
        Some(w),
    ))
}

/// Prove that `raw_g.bads` (if any, e.g. `s_until` safety side) are unreachable,
/// returning `Some(Ok(()))` when certified safe, `Some(Err(res))` on counterexample,
/// or `None` if undecided within budget.
pub(crate) fn prove_raw_safety_bads(
    req: &CheckRequest,
    raw_g: &Aig,
    ctx: &RunContext,
    t0: Instant,
    engine_name: &str,
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
                engine: engine_name.into(),
                verdict,
                wall_ms: t0.elapsed().as_millis(),
                witness,
                certificate: cert,
            }))
        }
        _ => None,
    }
}
