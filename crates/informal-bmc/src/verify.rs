//! Shared independent verdict certification, invariant store, and mining helpers.
//!
//! Any bit-level engine that solves on [`Prepared`] can pass its inductive
//! invariant cubes (`certify_lemmas`), k-induction proof (`kind_certified`), or
//! counterexample trace (`unsafe_certified` / `unsafe_certified_at`) through
//! these helpers to verify on the original loaded model and attach a
//! [`Certificate`].

use crate::prep::{solver, unknown, Checked, Prepared};
use crate::unroll::Unroll;
use crate::{certify, ic3, invariant, mine, store};
use informal_aig::Aig;
use informal_engine::{Certificate, CheckRequest, RunContext, Verdict};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Experiment H1: mine two-literal invariants by simulation and prove them
/// with Houdini (up to 10% of the time left, at most 30 s).
pub fn mine_invariant(g: &Aig, ctx: &RunContext, name: &str) -> Vec<ic3::Cube> {
    let t = Instant::now();
    let k = std::env::var("INFORMAL_MINE_K")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(1500);
    let cap = std::env::var("INFORMAL_MINE_CAP")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(50_000);
    let cands = mine::candidates(g, k, cap, Duration::from_secs(10));
    let nc = cands.len();
    let mut h = ctx.clone();
    let rest = ctx.deadline.saturating_duration_since(Instant::now());
    let cap_s: f64 = std::env::var("INFORMAL_MINE_TL")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(30.0);
    h.deadline = Instant::now() + (rest / 10).min(Duration::from_secs_f64(cap_s));
    if std::env::var_os("INFORMAL_MINE_TL").is_some() {
        h.deadline = Instant::now() + Duration::from_secs_f64(cap_s);
    }
    let inv = invariant::houdini(g, cands, || solver(&h), &h).unwrap_or_default();
    if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
        eprintln!(
            "{name}: mined {} candidates, {} proved invariant, {:.2} s",
            nc,
            inv.len(),
            t.elapsed().as_secs_f64()
        );
    }
    inv
}

/// Mine simulation invariants and assume them in `ic3`.
pub fn mine_into(g: &Aig, ic3: &mut ic3::Ic3, ctx: &RunContext, name: &str) {
    ic3.assume_invariant(mine_invariant(g, ctx, name));
}

/// Certify a set of blocked state cubes `lemmas` on `p` against the original
/// loaded model at `req.model`. Reusable across IC3, IMC, CAR, and BDD engines.
pub fn certify_lemmas(
    req: &CheckRequest,
    p: &Prepared,
    lemmas: &[ic3::Cube],
    ctx: &RunContext,
) -> Checked {
    if std::env::var_os("INFORMAL_NO_CERT").is_some() {
        return Verdict::Safe.into();
    }
    let t = Instant::now();
    let mut cert = None;
    let r = (|| {
        let orig = informal_aig::load(&req.model)
            .map_err(|e| format!("cannot reload the model: {e:#}"))?;
        if let Some(f) = &p.folded {
            // A folded model's invariant is a 2-step one on the loaded
            // model, on the clock's initial phase (not stored: the store
            // holds invariants of every reachable state).
            let mut inv = certify::lift_folded(&orig, &p.latch_map, p, lemmas, f.ok)?;
            inv.extend(certify::equalities(&p.merged));
            certify::check_folded(&orig, &inv, &|| solver(ctx))?;
            cert = Some(Certificate::TwoStep {
                clauses: without_ok(inv),
            });
            return Ok(());
        }
        let mut inv = certify::lift(&orig, &p.latch_map, p, lemmas)?;
        inv.extend(certify::equalities(&p.merged));
        certify::check(&orig, &inv, &|| solver(ctx))?;
        if store::dir().is_some() {
            store_invariant(&orig, p.design_hash, &inv);
        }
        cert = Some(Certificate::Invariant { clauses: inv });
        Ok(())
    })();
    Checked::new(report_certificate(req, r, t, ctx), cert)
}

/// `v`, IC3's verdict, with a SAFE kept only when its invariant passes the
/// independent check on the model as loaded ([`certify`]); otherwise it
/// becomes unknown, loudly. `INFORMAL_NO_CERT` skips the check (for
/// measuring its cost only).
pub fn certified(
    req: &CheckRequest,
    p: &Prepared,
    ic3: &ic3::Ic3,
    v: Verdict,
    ctx: &RunContext,
) -> Checked {
    if v != Verdict::Safe || std::env::var_os("INFORMAL_NO_CERT").is_some() {
        return v.into();
    }
    let t = Instant::now();
    let Some(lemmas) = ic3.invariant() else {
        return Checked::new(
            report_certificate(
                req,
                Err("missing: IC3 said safe without an invariant".into()),
                t,
                ctx,
            ),
            None,
        );
    };
    certify_lemmas(req, p, &lemmas, ctx)
}

/// The clauses of a folded model's invariant with the fold's `ok` latch
/// ([`certify::OK`]) read as true, as the folded checks read it: a clause
/// with `ok` holds, and `!ok` drops out of the clause it is in.
pub(crate) fn without_ok(mut inv: Vec<certify::Clause>) -> Vec<certify::Clause> {
    inv.retain(|c| !c.contains(&certify::OK));
    for c in &mut inv {
        c.retain(|&l| l != certify::OK ^ 1);
    }
    inv
}

/// H15: add a certified invariant (clauses over the loaded model's latch
/// literals) to the design's store.
pub fn store_invariant(orig: &Aig, hash: u64, inv: &[certify::Clause]) {
    let index: HashMap<u32, u32> = orig
        .latches
        .iter()
        .enumerate()
        .map(|(i, l)| (informal_aig::var(l.lit), i as u32))
        .collect();
    let clauses: Vec<store::IdxClause> = inv
        .iter()
        .filter_map(|c| {
            c.iter()
                .map(|&l| Some((*index.get(&informal_aig::var(l))?, informal_aig::is_neg(l))))
                .collect()
        })
        .collect();
    match store::save(hash, &clauses) {
        Ok(n) if std::env::var_os("INFORMAL_IC3_STATS").is_some() => {
            eprintln!(
                "invariant store: {} clauses added, {n} held for this design",
                clauses.len()
            )
        }
        Ok(_) => {}
        Err(e) => eprintln!("informal: invariant store not written: {e}"),
    }
}

/// H15: the stored clauses of this design that Houdini proves inductive
/// on the prepared model (within a tenth of the time left, 30 s at
/// most), as cubes for [`ic3::Ic3::assume_invariant`]. Empty when the
/// store is off or has nothing.
pub fn stored_invariant(p: &Prepared, ctx: &RunContext, name: &str) -> Vec<ic3::Cube> {
    use informal_aig::{neg, FALSE, TRUE};
    if store::dir().is_none() {
        return Vec::new();
    }
    let t0 = Instant::now();
    let clauses = store::load(p.design_hash);
    if clauses.is_empty() {
        return Vec::new();
    }
    let is_latch: HashSet<u32> = p.latches.iter().map(|l| informal_aig::var(l.lit)).collect();
    let mut cands: Vec<ic3::Cube> = Vec::new();
    'clause: for c in &clauses {
        // The clause through the latch map; its negation is the cube.
        let mut cube = Vec::with_capacity(c.len());
        for &(i, n) in c {
            let Some(Some(l)) = p.latch_map.get(i as usize).copied() else {
                continue 'clause; // a latch outside this property's cone
            };
            let l = if n { neg(l) } else { l };
            if l == TRUE {
                continue 'clause; // holds anyway
            }
            if l == FALSE {
                continue;
            }
            if !is_latch.contains(&informal_aig::var(l)) {
                continue 'clause;
            }
            cube.push(neg(l));
        }
        cube.sort_unstable();
        cube.dedup();
        if !cube.is_empty() {
            cands.push(cube);
        }
    }
    cands.sort();
    cands.dedup();
    let n = cands.len();
    let mut h = ctx.clone();
    let rest = ctx.deadline.saturating_duration_since(t0);
    h.deadline = t0 + (rest / 10).min(Duration::from_secs(30));
    let inv = invariant::houdini(p, cands, || solver(&h), &h).unwrap_or_default();
    if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
        eprintln!(
            "{name}: invariant store: {} clauses stored, {n} apply here, {} proved inductive, {:.2} s",
            clauses.len(),
            inv.len(),
            t0.elapsed().as_secs_f64()
        );
    }
    inv
}

/// `v` with an UNSAFE kept only when [`certify::find_unsafe`] or trace replay
/// confirms a counterexample of that depth on the model as loaded; otherwise
/// unknown, loudly (a stop on the way also gives unknown). `INFORMAL_NO_CERT`
/// skips the check.
pub fn unsafe_certified(
    req: &CheckRequest,
    v: impl Into<Checked>,
    ctx: &RunContext,
    trace: Option<(&Prepared, PreparedTrace)>,
) -> Checked {
    unsafe_certified_at(req, v, ctx, trace, false)
}

/// [`unsafe_certified`]; with `folded`, the depth is in steps of a
/// clock-folded model, so the bad state is at twice that depth or one
/// more in the loaded model. The reported depth is the loaded model's.
pub fn unsafe_certified_at(
    req: &CheckRequest,
    v: impl Into<Checked>,
    ctx: &RunContext,
    trace: Option<(&Prepared, PreparedTrace)>,
    folded: bool,
) -> Checked {
    let v: Checked = v.into();
    let Verdict::Unsafe { depth } = v.verdict else {
        return v;
    };
    if std::env::var_os("INFORMAL_NO_CERT").is_some() {
        return v;
    }
    let t = Instant::now();
    let mut real = depth;
    let r = (|| {
        let d = depth.ok_or("missing: UNSAFE without a depth")? as usize;
        let orig = informal_aig::load(&req.model)
            .map_err(|e| format!("cannot reload the model: {e:#}"))?;
        if folded {
            let mut last = Err("unsafe fails: no depth tried".to_string());
            for d2 in [2 * d, 2 * d + 1] {
                last = certify::find_unsafe(&orig, d2, &|| solver(ctx), &|| ctx.should_stop());
                if last.is_ok() {
                    real = Some(d2 as u32);
                    break;
                }
            }
            return last;
        }
        // The engine's own trace, carried back to the loaded model, is
        // checked by simulation alone; without one (IC3), or if it does
        // not replay, a fresh search at the same depth decides.
        if let Some((p, pt)) = &trace {
            let t = pt.lift(p, &orig);
            match certify::replay_unsafe(&orig, d, &t) {
                Ok(()) => return Ok(t),
                Err(e) => eprintln!(
                    "informal: engine trace did not replay on the loaded model ({e}); searching: {}",
                    req.model.display()
                ),
            }
        }
        certify::find_unsafe(&orig, d, &|| solver(ctx), &|| ctx.should_stop())
    })();
    let (r, trace) = match r {
        Ok(t) => (Ok(()), Some(t)),
        Err(e) => (Err(e), None),
    };
    match report_certificate(req, r, t, ctx) {
        Verdict::Safe => Checked::new(
            Verdict::Unsafe { depth: real },
            trace.map(|t| Certificate::Trace {
                init: informal_engine::certificate::bits(t.init),
                inputs: t
                    .inputs
                    .into_iter()
                    .map(informal_engine::certificate::bits)
                    .collect(),
            }),
        ),
        other => other.into(),
    }
}

/// A counterexample on the prepared model: each latch's initial value and
/// each input's value per frame, by index.
pub struct PreparedTrace {
    pub init: Vec<bool>,
    pub inputs: Vec<Vec<bool>>,
}

impl PreparedTrace {
    pub fn from_unroll(g: &Aig, u: &Unroll, d: usize) -> Self {
        PreparedTrace {
            init: g
                .latches
                .iter()
                .map(|l| u.sat.value(u.lit(0, l.lit)))
                .collect(),
            inputs: (0..=d)
                .map(|f| g.inputs.iter().map(|&i| u.sat.value(u.lit(f, i))).collect())
                .collect(),
        }
    }

    /// The same trace on the loaded model, through `p`'s maps; inputs and
    /// latches preprocessing dropped get 0.
    pub fn lift(&self, p: &Prepared, orig: &Aig) -> certify::Trace {
        use informal_aig::{is_neg, var, FALSE, TRUE};
        let lat: HashMap<u32, usize> = p
            .latches
            .iter()
            .enumerate()
            .map(|(i, l)| (var(l.lit), i))
            .collect();
        let inp: HashMap<u32, usize> = p
            .inputs
            .iter()
            .enumerate()
            .map(|(i, &l)| (var(l), i))
            .collect();
        let init = p
            .latch_map
            .iter()
            .map(|m| match *m {
                None | Some(FALSE) => false,
                Some(TRUE) => true,
                Some(l) => lat.get(&var(l)).is_some_and(|&i| self.init[i] ^ is_neg(l)),
            })
            .collect();
        let inputs = self
            .inputs
            .iter()
            .map(|frame| {
                p.input_map
                    .iter()
                    .map(|m| match *m {
                        Some(l) => inp.get(&var(l)).is_some_and(|&i| frame[i] ^ is_neg(l)),
                        None => false,
                    })
                    .collect()
            })
            .collect();
        let _ = orig;
        certify::Trace { init, inputs }
    }
}

/// [`certified`] for k-induction: `v` with the depth it was proved at,
/// `strengthening` the invariant cubes it blocked in every frame.
pub fn kind_certified(
    req: &CheckRequest,
    p: &Prepared,
    v: (Verdict, Option<usize>),
    strengthening: &[ic3::Cube],
    ctx: &RunContext,
) -> Checked {
    let (v, k) = v;
    if v != Verdict::Safe || std::env::var_os("INFORMAL_NO_CERT").is_some() {
        return v.into();
    }
    let t = Instant::now();
    let mut cert = None;
    let r = (|| {
        let k = k.ok_or("missing: k-induction said safe without a depth")?;
        let orig = informal_aig::load(&req.model)
            .map_err(|e| format!("cannot reload the model: {e:#}"))?;
        if let Some(f) = &p.folded {
            // A proof on the clock-folded model: checked as one over pairs
            // of loaded steps.
            let k = k.max(1);
            let mut inv = certify::lift_folded(&orig, &p.latch_map, p, strengthening, f.ok)?;
            inv.extend(certify::equalities(&p.merged));
            let differ =
                certify::check_kind_folded(&orig, &inv, k, &|| solver(ctx), &|| ctx.should_stop())?;
            cert = Some(Certificate::TwoStepKInduction {
                k: k as u32,
                clauses: without_ok(inv),
                simple_path: differ > 0,
            });
            return Ok(());
        }
        let (mut inv, _) = certify::lift_reps(&orig, &p.latch_map, p, strengthening)?;
        inv.extend(certify::equalities(&p.merged));
        // Depth k here is the number of good frames before the bad one.
        let k = k.max(1);
        let differ = certify::check_kind_paths(&orig, &inv, true, k, &|| solver(ctx), &|| {
            ctx.should_stop()
        })?;
        cert = Some(Certificate::KInduction {
            k: k as u32,
            clauses: inv,
            simple_path: differ > 0,
        });
        Ok(())
    })();
    Checked::new(report_certificate(req, r, t, ctx), cert)
}

/// An UNSAFE from k-induction, certified on the loaded model: by replaying
/// the engine's trace, or (on a clock-folded model, whose trace has two
/// loaded steps per frame) by a fresh search at the loaded depth.
pub fn kind_unsafe(
    req: &CheckRequest,
    p: &Prepared,
    v: Checked,
    ctx: &RunContext,
    tr: Option<PreparedTrace>,
) -> Checked {
    if p.folded.is_some() {
        unsafe_certified_at(req, v, ctx, None, true)
    } else {
        unsafe_certified(req, v, ctx, tr.map(|t| (p, t)))
    }
}

/// Turn a certificate check's outcome into the verdict to report.
pub fn report_certificate(
    req: &CheckRequest,
    r: Result<(), String>,
    t: Instant,
    ctx: &RunContext,
) -> Verdict {
    if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
        eprintln!(
            "certificate: {:?} in {:.3} s {}",
            r,
            t.elapsed().as_secs_f64(),
            req.model.display()
        );
    }
    match r {
        Ok(()) => Verdict::Safe,
        Err(e) if e.ends_with("check stopped") => unknown(ctx.stop_reason()),
        Err(e) => {
            eprintln!(
                "informal: verdict withdrawn, certificate {e}: {}",
                req.model.display()
            );
            unknown(format!("certificate {e}"))
        }
    }
}
