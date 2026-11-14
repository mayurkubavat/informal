//! An invariant store shared by the properties of one design (H15 in
//! docs/ic3-research.md).
//!
//! Many models are one design with different properties: every
//! cv32e40x-pNNN file of HWMCC'24 is the same AIG with another bad output,
//! and an FPV run has hundreds of asserts over one design. An invariant
//! certified for one property holds in every reachable state of the
//! design, so a sibling may assume the part of it that is inductive on
//! its own cone.
//!
//! With `INFORMAL_INV_STORE=<dir>` set, every certified invariant is
//! added to `<dir>/<design hash>.inv` as clauses over the latch indices
//! of the model as loaded, and IC3 imports what is there before it
//! starts. Nothing read from the store is trusted: the importer keeps
//! only what Houdini proves on the new model, and the final certificate
//! checks the whole invariant again.

use informal_aig::{Aig, Init, Node};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Mutex;

/// A clause over the loaded model's latches: (latch index, negated).
pub type IdxClause = Vec<(u32, bool)>;

/// At most this many clauses are kept per design.
const MAX_CLAUSES: usize = 200_000;

static WRITE: Mutex<()> = Mutex::new(());

/// The store directory, when the store is on.
pub fn dir() -> Option<PathBuf> {
    std::env::var_os("INFORMAL_INV_STORE")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
}

/// A hash of the design: everything in `g` except its bad outputs (and
/// names). Two properties of one design get the same value.
pub fn design_hash(g: &Aig) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut add = |x: u64| {
        h ^= x;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        h ^= h >> 29;
    };
    add(g.nodes.len() as u64);
    add(g.inputs.len() as u64);
    add(g.latches.len() as u64);
    for n in &g.nodes {
        match *n {
            Node::Const => add(1),
            Node::Input => add(2),
            Node::Latch => add(3),
            Node::And(a, b) => {
                add(4);
                add(a as u64);
                add(b as u64);
            }
        }
    }
    for l in &g.latches {
        add(l.lit as u64);
        add(l.next as u64);
        match l.init {
            Init::Zero => add(10),
            Init::One => add(11),
            Init::Free => add(12),
            Init::Expr(e) => {
                add(13);
                add(e as u64);
            }
        }
    }
    add(g.constraints.len() as u64);
    for &c in &g.constraints {
        add(c as u64);
    }
    h
}

fn path(dir: &std::path::Path, hash: u64) -> PathBuf {
    dir.join(format!("{hash:016x}.inv"))
}

fn parse(text: &str) -> BTreeSet<IdxClause> {
    text.lines()
        .filter_map(|line| {
            let c: Option<IdxClause> = line
                .split_whitespace()
                .map(|t| {
                    let x: i64 = t.parse().ok()?;
                    (x != 0).then(|| ((x.unsigned_abs() - 1) as u32, x < 0))
                })
                .collect();
            c.filter(|c| !c.is_empty())
        })
        .collect()
}

/// The clauses stored for the design (empty when there are none).
pub fn load(hash: u64) -> Vec<IdxClause> {
    let Some(dir) = dir() else {
        return Vec::new();
    };
    std::fs::read_to_string(path(&dir, hash))
        .map(|t| parse(&t).into_iter().collect())
        .unwrap_or_default()
}

/// Add `clauses` to the design's file (a union with what is there),
/// written to a temporary file and renamed so a reader never sees half
/// of it. Returns how many clauses the file holds afterwards.
pub fn save(hash: u64, clauses: &[IdxClause]) -> std::io::Result<usize> {
    let Some(dir) = dir() else {
        return Ok(0);
    };
    // INFORMAL_INV_STORE_READONLY: import only (for measurements, so that
    // one run's proofs do not help the next).
    if std::env::var_os("INFORMAL_INV_STORE_READONLY").is_some() {
        return Ok(0);
    }
    let _guard = WRITE.lock().unwrap_or_else(|p| p.into_inner());
    std::fs::create_dir_all(&dir)?;
    let file = path(&dir, hash);
    let mut all = std::fs::read_to_string(&file)
        .map(|t| parse(&t))
        .unwrap_or_default();
    for c in clauses {
        if all.len() >= MAX_CLAUSES {
            break;
        }
        let mut c = c.clone();
        c.sort_unstable();
        c.dedup();
        if !c.is_empty() {
            all.insert(c);
        }
    }
    let mut text = String::new();
    for c in &all {
        for (k, &(i, n)) in c.iter().enumerate() {
            if k > 0 {
                text.push(' ');
            }
            let x = i as i64 + 1;
            text.push_str(&(if n { -x } else { x }).to_string());
        }
        text.push('\n');
    }
    let tmp = file.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &file)?;
    Ok(all.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_aig::neg;

    #[test]
    fn hash_ignores_the_property_and_sees_the_design() {
        let mut g = Aig::new();
        let a = g.latch(Init::Zero);
        let b = g.latch(Init::Zero);
        g.set_next(a, neg(a));
        g.set_next(b, a);
        let mut p1 = g.clone();
        p1.bads.push(a);
        let mut p2 = g.clone();
        p2.bads.push(b);
        assert_eq!(design_hash(&p1), design_hash(&p2));
        let mut other = g.clone();
        other.set_next(b, neg(a));
        other.bads.push(a);
        assert_ne!(design_hash(&p1), design_hash(&other));
    }

    #[test]
    fn clauses_round_trip_through_the_text_format() {
        let text = "1 -3\n-2\n\n7 8 -9\n";
        let got: Vec<IdxClause> = parse(text).into_iter().collect();
        assert!(got.contains(&vec![(0, false), (2, true)]));
        assert!(got.contains(&vec![(1, true)]));
        assert!(got.contains(&vec![(6, false), (7, false), (8, true)]));
        assert_eq!(got.len(), 3);
    }
}
