//! Independent fresh-solver certification for `informal-ic3sa` (D8 soundness).
//!
//! - **Safe:** verifies the discovered word-level invariant `Inv(s, p)` in a
//!   brand-new Bitwuzla instance with three independent queries:
//!   1. Initiation:  `Init(s) & C(s, i) & !Inv(s, p)` is UNSAT
//!   2. Consecution: `Inv(s, p) & C(s, i) & T(s, i, s') & (p' == p) & C(s', i') & !Inv(s', p')` is UNSAT
//!   3. Safety:      `Inv(s, p) & C(s, i) & Bad_P(s, p, i)` is UNSAT
//! - **Unsafe:** verifies a concrete trace of length `depth` on the original
//!   un-augmented model in a fresh Bitwuzla instance and extracts an
//!   `informalw 1` named witness for bit-vector inputs and states.

#[cfg(informal_bitwuzla)]
use crate::ffi::{build_sorts, unroll_step, Bz, T};
#[cfg(informal_bitwuzla)]
use crate::ic3sa::wts::{fresh_sorts, Cube, FrameEnv, Wts};
#[cfg(informal_bitwuzla)]
use informal_ir::{Model, Op, Sort};
#[cfg(informal_bitwuzla)]
use std::collections::HashMap;
#[cfg(informal_bitwuzla)]
use std::sync::atomic::AtomicBool;

/// Independently verify that `clauses` (each a blocked [`Cube`]) form a valid
/// inductive invariant proving `!Bad` on `(m, wts)`.
#[cfg(informal_bitwuzla)]
pub(crate) fn certify_invariant(
    m: &Model,
    wts: &Wts,
    clauses: &[Cube],
    left_ms: i64,
    cancel: &AtomicBool,
) -> Result<(), String> {
    // 1. Initiation: Init(s) & C(s, i) & !Inv(s, p) must be UNSAT.
    {
        let bz = Bz::new(left_ms, cancel);
        let sorts = fresh_sorts(m, &bz).map_err(|e| format!("{e:#}"))?;
        let mut f0 = FrameEnv::encode(m, wts, &bz, &sorts, None, None, "ci0")
            .map_err(|e| format!("{e:#}"))?;
        f0.assert_init(m, wts, &bz).map_err(|e| format!("{e:#}"))?;
        f0.assert_constraints(m, &bz)
            .map_err(|e| format!("{e:#}"))?;
        for c in clauses {
            let cl = f0.encode_clause(m, &bz, c).map_err(|e| format!("{e:#}"))?;
            let ncl = bz.op("not", &[cl], 0, 0).map_err(|e| format!("{e:#}"))?;
            match bz.solve(&[ncl]).map_err(|e| format!("{e:#}"))? {
                20 => {}
                10 => return Err("initiation check failed".into()),
                _ => return Err("initiation check stopped".into()),
            }
        }
    }

    // 2. Safety: Inv(s, p) & C(s, i) & Bad_P(s, p, i) must be UNSAT.
    {
        let bz = Bz::new(left_ms, cancel);
        let sorts = fresh_sorts(m, &bz).map_err(|e| format!("{e:#}"))?;
        let mut f0 = FrameEnv::encode(m, wts, &bz, &sorts, None, None, "cs0")
            .map_err(|e| format!("{e:#}"))?;
        f0.assert_constraints(m, &bz)
            .map_err(|e| format!("{e:#}"))?;
        for c in clauses {
            let cl = f0.encode_clause(m, &bz, c).map_err(|e| format!("{e:#}"))?;
            bz.assert(cl).map_err(|e| format!("{e:#}"))?;
        }
        match bz.solve(&[f0.bad_p]).map_err(|e| format!("{e:#}"))? {
            20 => {}
            10 => return Err("safety check failed".into()),
            _ => return Err("safety check stopped".into()),
        }
    }

    // 3. Consecution: Inv(s, p) & C(s, i) & T & (p' == p) & C(s', i') & !Inv(s', p') is UNSAT.
    if !wts.bv_states.is_empty() || !wts.arr_states.is_empty() {
        let bz = Bz::new(left_ms, cancel);
        let sorts = fresh_sorts(m, &bz).map_err(|e| format!("{e:#}"))?;
        let mut f0 = FrameEnv::encode(m, wts, &bz, &sorts, None, None, "cc0")
            .map_err(|e| format!("{e:#}"))?;
        f0.assert_constraints(m, &bz)
            .map_err(|e| format!("{e:#}"))?;
        let next_map = f0
            .next_state_terms(m, wts, &bz, "cc1")
            .map_err(|e| format!("{e:#}"))?;
        let mut f1 = FrameEnv::encode(
            m,
            wts,
            &bz,
            &sorts,
            Some(&next_map),
            Some(&f0.prophecies),
            "cc1",
        )
        .map_err(|e| format!("{e:#}"))?;
        f1.assert_constraints(m, &bz)
            .map_err(|e| format!("{e:#}"))?;
        for c in clauses {
            let cl0 = f0.encode_clause(m, &bz, c).map_err(|e| format!("{e:#}"))?;
            bz.assert(cl0).map_err(|e| format!("{e:#}"))?;
        }
        for c in clauses {
            let cl1 = f1.encode_clause(m, &bz, c).map_err(|e| format!("{e:#}"))?;
            let ncl1 = bz.op("not", &[cl1], 0, 0).map_err(|e| format!("{e:#}"))?;
            match bz.solve(&[ncl1]).map_err(|e| format!("{e:#}"))? {
                20 => {}
                10 => return Err("consecution check failed".into()),
                _ => return Err("consecution check stopped".into()),
            }
        }
    }
    Ok(())
}

/// Independently verify a concrete counterexample at exact depth `depth` on
/// the original un-augmented Btor2 model.
/// Returns `Ok(Some(witness))` if SAT, `Ok(None)` if UNSAT (spurious), or `Err` if stopped/error.
#[cfg(informal_bitwuzla)]
pub(crate) fn certify_cex(
    m: &Model,
    depth: usize,
    left_ms: i64,
    cancel: &AtomicBool,
) -> Result<Option<Option<String>>, String> {
    let bz = Bz::new_ic3(left_ms, cancel);
    let sorts = build_sorts(m, &bz).map_err(|e| format!("{e:#}"))?;
    let next: HashMap<u32, i64> = m
        .with_op(Op::Next)
        .map(|l| (l.args[0] as u32, l.args[1]))
        .collect();
    let init: HashMap<u32, i64> = m
        .with_op(Op::Init)
        .map(|l| (l.args[0] as u32, l.args[1]))
        .collect();

    let mut frames: Vec<HashMap<u32, T>> = Vec::with_capacity(depth + 1);
    let mut prev: HashMap<u32, T> = HashMap::new();
    let mut last_bad = 0;
    for k in 0..=depth {
        let (f, bad) = unroll_step(m, &bz, &sorts, &init, &next, &prev, k, true)
            .map_err(|e| format!("{e:#}"))?;
        last_bad = bad;
        prev = f.clone();
        frames.push(f);
    }
    match bz.solve(&[last_bad]).map_err(|e| format!("{e:#}"))? {
        10 => {
            let witness = extract_named_witness(m, &bz, &frames, depth).ok();
            Ok(Some(witness))
        }
        20 => Ok(None),
        _ => Err("cex check stopped".into()),
    }
}

#[cfg(informal_bitwuzla)]
fn extract_named_witness(
    m: &Model,
    bz: &Bz,
    frames: &[HashMap<u32, T>],
    depth: usize,
) -> anyhow::Result<String> {
    let mut s = format!("informalw 1\ndepth {depth}\ninit");
    let f0 = &frames[0];
    for l in m.with_op(Op::State) {
        let Some(name) = l.symbol.as_deref() else {
            continue;
        };
        let Some(Sort::BitVec(w)) = l.sort.and_then(|sid| m.sort(sid)) else {
            continue;
        };
        let bits = bz.get_value_bits(f0[&l.id])?;
        append_signal_bits(&mut s, name, w, &bits);
    }
    for (k, fk) in frames.iter().enumerate() {
        s += &format!("\n@{k}");
        for l in m.with_op(Op::Input) {
            let Some(name) = l.symbol.as_deref() else {
                continue;
            };
            let Some(Sort::BitVec(w)) = l.sort.and_then(|sid| m.sort(sid)) else {
                continue;
            };
            let bits = bz.get_value_bits(fk[&l.id])?;
            append_signal_bits(&mut s, name, w, &bits);
        }
    }
    s.push('\n');
    Ok(s)
}

#[cfg(informal_bitwuzla)]
fn append_signal_bits(out: &mut String, name: &str, width: u32, msb_bits: &str) {
    if width == 1 {
        if let Some(ch) = msb_bits.chars().last() {
            out.push_str(&format!(" {name}={ch}"));
        }
        return;
    }
    for (i, ch) in msb_bits.chars().rev().enumerate() {
        out.push_str(&format!(" {name}[{i}]={ch}"));
    }
}
