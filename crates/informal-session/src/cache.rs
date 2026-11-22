//! Content-addressed proof cache (`set_prove_cache`, `set_prove_cache_path`,
//! `set_prove_cache_mode`).
//!
//! Keys each target by the FIPS 180-4 SHA-256 digest of `(kind, canonical_btor2)`.
//! Because `canonical_btor2` includes every sort, input, state, reset, clock
//! wave, cut point, assumption constraint, fairness constraint, and target
//! cone node in the isolated per-property Btor2 model, any change to the RTL
//! cone, assumptions, reset, black boxes, cut points, or property expression
//! changes the digest automatically. Only definitive safety/liveness proofs
//! (`Proven` for assertions, `Unreachable` for covers) are cached so counterexample
//! and cover VCD traces are never served stale.

use crate::session::{Kind, Status};
use crate::stats::ProofStats;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static CACHE_TMP_SEQ: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMode {
    #[default]
    ReadWrite,
    ReadOnly,
    WriteOnly,
}

impl CacheMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "read_write" | "rw" | "readwrite" => Some(Self::ReadWrite),
            "read_only" | "ro" | "readonly" => Some(Self::ReadOnly),
            "write_only" | "wo" | "writeonly" => Some(Self::WriteOnly),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadWrite => "read_write",
            Self::ReadOnly => "read_only",
            Self::WriteOnly => "write_only",
        }
    }

    pub fn can_read(self) -> bool {
        matches!(self, Self::ReadWrite | Self::ReadOnly)
    }

    pub fn can_write(self) -> bool {
        matches!(self, Self::ReadWrite | Self::WriteOnly)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub version: u32,
    pub kind: String,
    pub status: String,
    pub engine: String,
    pub orig_wall_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invariant_clauses: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

/// Computes the cone-of-influence content-addressed SHA-256 hex key for `(kind, btor_model)`.
///
/// Dead nodes outside the target + constraints + fairness cone are pruned and
/// surviving Btor2 IDs are renumbered `1..=K` in topological file order so
/// adding or removing unrelated properties in the same task never perturbs a
/// target's cache key.
pub fn cache_key(kind: Kind, btor_model: &str) -> String {
    use informal_ir::{Entry, Op, Sort};
    use std::collections::{HashMap, HashSet};

    let (engine_part, _) = informal_svlang::split_trace(btor_model);
    let Ok(m) = informal_ir::parse(engine_part) else {
        return sha256_hex(
            format!(
                "informal-prove-cache-raw-v1\n{}\n{engine_part}",
                kind.label()
            )
            .as_bytes(),
        );
    };

    let mut state_updates: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut todo: Vec<u32> = Vec::new();
    for l in m.lines() {
        match l.op {
            Op::Init | Op::Next => {
                if let Some(&st) = l.args.first() {
                    state_updates
                        .entry(st.unsigned_abs() as u32)
                        .or_default()
                        .push(l.id);
                }
            }
            Op::Bad | Op::Justice | Op::Constraint | Op::Fair => {
                todo.push(l.id);
            }
            _ => {}
        }
    }

    let mut reached: HashSet<u32> = HashSet::new();
    let mut sort_todo: Vec<u32> = Vec::new();
    while let Some(id) = todo.pop() {
        if !reached.insert(id) {
            continue;
        }
        let Some(l) = m.line(id) else { continue };
        if let Some(sid) = l.sort {
            sort_todo.push(sid);
        }
        for &a in &l.args {
            let aid = a.unsigned_abs() as u32;
            if !reached.contains(&aid) {
                todo.push(aid);
            }
        }
        if l.op == Op::State {
            if let Some(upds) = state_updates.get(&id) {
                for &uid in upds {
                    if !reached.contains(&uid) {
                        todo.push(uid);
                    }
                }
            }
        }
    }
    while let Some(sid) = sort_todo.pop() {
        if !reached.insert(sid) {
            continue;
        }
        if let Some(Sort::Array { index, element }) = m.sort(sid) {
            sort_todo.push(index);
            sort_todo.push(element);
        }
    }

    let mut remap: HashMap<u32, u32> = HashMap::with_capacity(reached.len());
    let mut canonical = String::with_capacity(reached.len() * 24 + 48);
    canonical.push_str("informal-prove-cache-v1\n");
    canonical.push_str(kind.label());
    canonical.push('\n');

    let map_signed = |a: i64, remap: &HashMap<u32, u32>| -> i64 {
        let m = remap.get(&(a.unsigned_abs() as u32)).copied().unwrap_or(0) as i64;
        if a < 0 {
            -m
        } else {
            m
        }
    };

    for entry in m.entries() {
        match entry {
            Entry::Sort { id, sort, .. } if reached.contains(id) => {
                let nid = (remap.len() as u32) + 1;
                remap.insert(*id, nid);
                match sort {
                    Sort::BitVec(w) => {
                        let _ = writeln!(canonical, "{nid} sort bitvec {w}");
                    }
                    Sort::Array { index, element } => {
                        let ix = remap.get(index).copied().unwrap_or(0);
                        let el = remap.get(element).copied().unwrap_or(0);
                        let _ = writeln!(canonical, "{nid} sort array {ix} {el}");
                    }
                }
            }
            Entry::Line(l) if reached.contains(&l.id) => {
                let nid = (remap.len() as u32) + 1;
                remap.insert(l.id, nid);
                let _ = write!(canonical, "{nid} {}", l.op.name());
                if let Some(sid) = l.sort {
                    let _ = write!(canonical, " {}", remap.get(&sid).copied().unwrap_or(0));
                }
                if l.op == Op::Justice {
                    let _ = write!(canonical, " {}", l.args.len());
                }
                for &a in &l.args {
                    let _ = write!(canonical, " {}", map_signed(a, &remap));
                }
                for n in &l.nums {
                    let _ = write!(canonical, " {n}");
                }
                if matches!(l.op, Op::Input | Op::State) {
                    if let Some(sym) = &l.symbol {
                        let _ = write!(canonical, " {sym}");
                    }
                }
                canonical.push('\n');
            }
            _ => {}
        }
    }
    sha256_hex(canonical.as_bytes())
}

pub fn lookup(dir: &Path, key: &str, kind: Kind) -> Option<(Status, ProofStats)> {
    let path = entry_path(dir, key);
    let raw = std::fs::read_to_string(path).ok()?;
    let entry: CacheEntry = serde_json::from_str(&raw).ok()?;
    if entry.version != 1 || entry.kind != kind.label() {
        return None;
    }
    let cached_engine = format!("cache({})", entry.engine);
    let status = match (entry.status.as_str(), kind) {
        ("proven", Kind::Assert) => Status::Proven {
            engine: cached_engine.clone(),
            ms: 0,
        },
        ("unreachable", Kind::Cover) => Status::Unreachable {
            engine: cached_engine.clone(),
            ms: 0,
        },
        _ => return None,
    };
    let depth_kind = match entry.depth_kind.as_deref() {
        Some("k") => Some("k"),
        Some("frames") => Some("frames"),
        Some("trace") => Some("trace"),
        Some("bound") => Some("bound"),
        _ => None,
    };
    let stats = ProofStats {
        engine: Some(cached_engine),
        wall_ms: 0,
        cpu_ms: Some(0),
        peak_rss_kb: None,
        depth: entry.depth,
        depth_kind,
        max_bound: entry.depth,
        invariant_clauses: entry.invariant_clauses,
        evidence: entry.evidence,
        certificate: None,
    };
    Some((status, stats))
}

pub fn store(dir: &Path, key: &str, kind: Kind, status: &Status, stats: Option<&ProofStats>) {
    let (status_word, engine, ms) = match (status, kind) {
        (Status::Proven { engine, ms }, Kind::Assert) => ("proven", engine.as_str(), *ms),
        (Status::Unreachable { engine, ms }, Kind::Cover) => ("unreachable", engine.as_str(), *ms),
        _ => return,
    };
    if engine.starts_with("cache(") || engine == "elaboration" {
        return;
    }
    let entry = CacheEntry {
        version: 1,
        kind: kind.label().to_string(),
        status: status_word.to_string(),
        engine: engine.to_string(),
        orig_wall_ms: ms as u64,
        depth: stats.and_then(|s| s.depth),
        depth_kind: stats.and_then(|s| s.depth_kind.map(String::from)),
        invariant_clauses: stats.and_then(|s| s.invariant_clauses),
        evidence: stats.and_then(|s| s.evidence.clone()),
    };
    let Ok(json) = serde_json::to_string_pretty(&entry) else {
        return;
    };
    let dst = entry_path(dir, key);
    if let Some(parent) = dst.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let seq = CACHE_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dst.with_extension(format!("tmp.{}.{seq}", std::process::id()));
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &dst);
    }
}

fn entry_path(dir: &Path, key: &str) -> PathBuf {
    let prefix = key.get(0..2).unwrap_or("00");
    dir.join(prefix).join(format!("{key}.json"))
}

/// Pure-Rust FIPS 180-4 SHA-256 digest formatted as 64 lowercase hex digits.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64) * 8;
    let rem = (data.len() + 9) % 64;
    let pad_zeros = if rem == 0 { 0 } else { 64 - rem };
    let mut msg = Vec::with_capacity(data.len() + 9 + pad_zeros);
    msg.extend_from_slice(data);
    msg.push(0x80);
    msg.resize(data.len() + 1 + pad_zeros, 0);
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, &word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = String::with_capacity(64);
    for word in h {
        let _ = write!(out, "{word:08x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_nist_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
