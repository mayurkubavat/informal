//! Cross-engine cooperative proof sharing hub (`Phase 2 — LemmaHub`).
//!
//! Enables concurrent engines inside a portfolio run and sequential properties
//! sharing a design cone to exchange inductive lemmas in a canonical,
//! model-independent latch-index representation.
//!
//! # Soundness contract
//!
//! Every entry is tagged with its `design_hash`, `property_hash`, and
//! [`LemmaScope`]. Importing engines **never** trust clauses blindly: standing
//! batches are re-validated via Houdini on the receiver's prepared model, frame
//! lemmas are re-checked for relative inductiveness, and every final `Safe`
//! verdict still passes independent certification on the un-preprocessed model.

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::RwLock;

/// Default maximum standing clauses retained per hub.
pub const DEFAULT_MAX_STANDING: usize = 16_384;
/// Default maximum bounded frame clauses retained per hub.
pub const DEFAULT_MAX_FRAME: usize = 16_384;
/// Maximum literal width accepted into the hub (avoids flooding with wide cubes).
pub const MAX_SHARED_CLAUSE_LEN: usize = 24;

/// A clause over the loaded model's original latch indices: `(latch_idx, negated)`.
///
/// Represents the disjunction `∨ (if negated { !L_i } else { L_i })`, kept
/// sorted and deduplicated in canonical form.
pub type CanonicalClause = Vec<(u32, bool)>;

/// Validity scope of a shared lemma.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LemmaScope {
    /// Holds in every reachable state of the original unfolded model.
    /// Safe to validate on any engine (unfolded or folded) and across sibling
    /// properties sharing `design_hash`.
    Standing,
    /// Holds in every reachable state of the clock-folded model with matching
    /// `(clock, phase)`. Safe to validate on folded engines with the same
    /// `(clock, phase)` across sibling properties sharing `design_hash`.
    FoldedStanding { clock: usize, phase: bool },
    /// Excludes states reachable within `level` steps from `Init` on the exact
    /// property `(design_hash, property_hash, folded)`.
    Frame {
        level: u32,
        folded: Option<(usize, bool)>,
    },
}

/// One lemma entry stored in a [`LemmaHub`].
#[derive(Debug, Clone)]
pub struct SharedLemma {
    pub design_hash: u64,
    pub property_hash: u64,
    pub scope: LemmaScope,
    pub clause: CanonicalClause,
    pub source: String,
    pub publisher_id: u64,
}

/// Telemetry counters recorded by a [`LemmaHub`].
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct HubStats {
    pub published_standing: u64,
    pub published_frame: u64,
    pub deduplicated: u64,
    pub dropped_cap: u64,
    pub imported_candidates: u64,
    pub accepted: u64,
    pub rejected: u64,
}

type StandingKey = (u64, LemmaScope, CanonicalClause);
type FrameKey = (u64, u64, Option<(usize, bool)>, CanonicalClause);

#[derive(Default)]
struct HubInner {
    standing: Vec<SharedLemma>,
    frames: Vec<SharedLemma>,
    standing_seen: HashSet<StandingKey>,
    frame_max_level: HashMap<FrameKey, u32>,
}

/// Thread-safe, low-contention cooperative lemma exchange hub.
pub struct LemmaHub {
    inner: RwLock<HubInner>,
    standing_seq: AtomicUsize,
    frame_seq: AtomicUsize,
    next_pub_id: AtomicU64,
    max_standing: usize,
    max_frame: usize,
    published_standing: AtomicU64,
    published_frame: AtomicU64,
    deduplicated: AtomicU64,
    dropped_cap: AtomicU64,
    imported_candidates: AtomicU64,
    accepted: AtomicU64,
    rejected: AtomicU64,
}

impl std::fmt::Debug for LemmaHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LemmaHub")
            .field("standing", &self.standing_seq.load(Ordering::Relaxed))
            .field("frames", &self.frame_seq.load(Ordering::Relaxed))
            .field("stats", &self.stats())
            .finish()
    }
}

impl Default for LemmaHub {
    fn default() -> Self {
        Self::new()
    }
}

impl LemmaHub {
    /// Create a new hub with default capacity caps.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_MAX_STANDING, DEFAULT_MAX_FRAME)
    }

    /// Create a new hub with explicit standing and frame capacity caps.
    pub fn with_capacity(max_standing: usize, max_frame: usize) -> Self {
        Self {
            inner: RwLock::new(HubInner::default()),
            standing_seq: AtomicUsize::new(0),
            frame_seq: AtomicUsize::new(0),
            next_pub_id: AtomicU64::new(1),
            max_standing,
            max_frame,
            published_standing: AtomicU64::new(0),
            published_frame: AtomicU64::new(0),
            deduplicated: AtomicU64::new(0),
            dropped_cap: AtomicU64::new(0),
            imported_candidates: AtomicU64::new(0),
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        }
    }

    /// True unless `INFORMAL_LEMMA_HUB=0` disables cooperative sharing.
    pub fn is_enabled() -> bool {
        std::env::var("INFORMAL_LEMMA_HUB").map_or(true, |v| v != "0")
    }

    /// True only when `INFORMAL_LEMMA_HUB_FRAMES=1` opts into bounded
    /// frame-lemma exchange across engines.
    ///
    /// Standing invariants (`Standing` / `FoldedStanding`) are always shared
    /// when [`Self::is_enabled`] is true; bounded frame sharing defaults to off
    /// because re-checking unsolicited cross-engine frame cubes can slow IC3
    /// convergence on large (>1k-latch) models.
    pub fn frames_enabled() -> bool {
        std::env::var("INFORMAL_LEMMA_HUB_FRAMES").is_ok_and(|v| !v.is_empty() && v != "0")
    }

    /// Allocate a unique per-engine-run publisher identifier so an engine skips
    /// only its own entries while sibling properties/workers still receive them.
    pub fn next_publisher_id(&self) -> u64 {
        self.next_pub_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Monotonic count of standing lemmas currently in the hub (lock-free).
    #[inline]
    pub fn standing_seq(&self) -> usize {
        self.standing_seq.load(Ordering::Acquire)
    }

    /// Monotonic count of frame lemmas currently in the hub (lock-free).
    #[inline]
    pub fn frame_seq(&self) -> usize {
        self.frame_seq.load(Ordering::Acquire)
    }

    /// Total stored entries (standing + frame).
    pub fn len(&self) -> usize {
        self.standing_seq() + self.frame_seq()
    }

    /// True when no lemmas have been published yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Publish a single lemma; returns true if newly inserted.
    pub fn publish(&self, lemma: SharedLemma) -> bool {
        self.publish_batch(std::iter::once(lemma)) > 0
    }

    /// Publish a batch of lemmas atomically under one write lock so readers
    /// observe mutually inductive standing sets together. Returns the number
    /// of newly inserted entries.
    pub fn publish_batch(&self, lemmas: impl IntoIterator<Item = SharedLemma>) -> usize {
        let mut norm: Vec<SharedLemma> = Vec::new();
        for mut l in lemmas {
            if canonicalize(&mut l.clause) {
                norm.push(l);
            }
        }
        if norm.is_empty() {
            return 0;
        }
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        let mut added = 0usize;
        let mut added_standing = 0u64;
        let mut added_frame = 0u64;
        let mut deduped = 0u64;
        let mut capped = 0u64;

        for lemma in norm {
            match lemma.scope {
                LemmaScope::Standing | LemmaScope::FoldedStanding { .. } => {
                    if inner.standing.len() >= self.max_standing {
                        capped += 1;
                        continue;
                    }
                    let key = (lemma.design_hash, lemma.scope, lemma.clause.clone());
                    if !inner.standing_seen.insert(key) {
                        deduped += 1;
                        continue;
                    }
                    inner.standing.push(lemma);
                    added += 1;
                    added_standing += 1;
                }
                LemmaScope::Frame { level, folded } => {
                    if inner.frames.len() >= self.max_frame {
                        capped += 1;
                        continue;
                    }
                    let key = (
                        lemma.design_hash,
                        lemma.property_hash,
                        folded,
                        lemma.clause.clone(),
                    );
                    if inner
                        .frame_max_level
                        .get(&key)
                        .is_some_and(|&prev| prev >= level)
                    {
                        deduped += 1;
                        continue;
                    }
                    inner.frame_max_level.insert(key, level);
                    inner.frames.push(lemma);
                    added += 1;
                    added_frame += 1;
                }
            }
        }
        let new_s_len = inner.standing.len();
        let new_f_len = inner.frames.len();
        drop(inner);

        if added_standing > 0 {
            self.standing_seq.store(new_s_len, Ordering::Release);
            self.published_standing
                .fetch_add(added_standing, Ordering::Relaxed);
        }
        if added_frame > 0 {
            self.frame_seq.store(new_f_len, Ordering::Release);
            self.published_frame
                .fetch_add(added_frame, Ordering::Relaxed);
        }
        if deduped > 0 {
            self.deduplicated.fetch_add(deduped, Ordering::Relaxed);
        }
        if capped > 0 {
            self.dropped_cap.fetch_add(capped, Ordering::Relaxed);
        }
        added
    }

    /// Incrementally poll compatible standing invariants (`Standing`, plus
    /// `FoldedStanding` matching `folded`) published since `*cursor`, skipping
    /// entries from `exclude_publisher`.
    ///
    /// Lock-free when no new standing entries exist.
    pub fn poll_standing(
        &self,
        cursor: &mut usize,
        design_hash: u64,
        folded: Option<(usize, bool)>,
        exclude_publisher: u64,
    ) -> Vec<SharedLemma> {
        let cur_seq = self.standing_seq.load(Ordering::Acquire);
        if *cursor >= cur_seq {
            return Vec::new();
        }
        let inner = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let slice = &inner.standing[*cursor..];
        let mut out = Vec::new();
        for entry in slice {
            if entry.design_hash != design_hash || entry.publisher_id == exclude_publisher {
                continue;
            }
            let compat = match (entry.scope, folded) {
                (LemmaScope::Standing, _) => true,
                (LemmaScope::FoldedStanding { clock, phase }, Some((c, p))) => {
                    clock == c && phase == p
                }
                _ => false,
            };
            if compat {
                out.push(entry.clone());
            }
        }
        *cursor = inner.standing.len();
        out
    }

    /// Incrementally poll compatible bounded frame lemmas for one property
    /// `(design_hash, property_hash, folded)` published since `*cursor`,
    /// skipping entries from `exclude_publisher`.
    ///
    /// Lock-free when no new frame entries exist.
    pub fn poll_frames(
        &self,
        cursor: &mut usize,
        design_hash: u64,
        property_hash: u64,
        folded: Option<(usize, bool)>,
        exclude_publisher: u64,
    ) -> Vec<(CanonicalClause, u32)> {
        let cur_seq = self.frame_seq.load(Ordering::Acquire);
        if *cursor >= cur_seq {
            return Vec::new();
        }
        let inner = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let slice = &inner.frames[*cursor..];
        let mut out = Vec::new();
        for entry in slice {
            if entry.design_hash != design_hash
                || entry.property_hash != property_hash
                || entry.publisher_id == exclude_publisher
            {
                continue;
            }
            if let LemmaScope::Frame {
                level,
                folded: f_scope,
            } = entry.scope
            {
                if f_scope == folded {
                    out.push((entry.clause.clone(), level));
                }
            }
        }
        *cursor = inner.frames.len();
        out
    }

    /// Record receiver-side validation results (lock-free).
    pub fn record_import(&self, candidates: u64, accepted: u64, rejected: u64) {
        if candidates > 0 {
            self.imported_candidates
                .fetch_add(candidates, Ordering::Relaxed);
        }
        if accepted > 0 {
            self.accepted.fetch_add(accepted, Ordering::Relaxed);
        }
        if rejected > 0 {
            self.rejected.fetch_add(rejected, Ordering::Relaxed);
        }
    }

    /// Snapshot telemetry counters.
    pub fn stats(&self) -> HubStats {
        HubStats {
            published_standing: self.published_standing.load(Ordering::Relaxed),
            published_frame: self.published_frame.load(Ordering::Relaxed),
            deduplicated: self.deduplicated.load(Ordering::Relaxed),
            dropped_cap: self.dropped_cap.load(Ordering::Relaxed),
            imported_candidates: self.imported_candidates.load(Ordering::Relaxed),
            accepted: self.accepted.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
        }
    }
}

/// Sort, deduplicate, and validate a canonical clause. Returns `false` if empty,
/// wider than [`MAX_SHARED_CLAUSE_LEN`], or tautological (`L_i \/ !L_i`).
fn canonicalize(clause: &mut CanonicalClause) -> bool {
    if clause.is_empty() || clause.len() > MAX_SHARED_CLAUSE_LEN {
        return false;
    }
    clause.sort_unstable();
    clause.dedup();
    for w in clause.windows(2) {
        if w[0].0 == w[1].0 {
            // Contains both `(idx, false)` and `(idx, true)` -> tautology.
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(
        design_hash: u64,
        property_hash: u64,
        scope: LemmaScope,
        clause: CanonicalClause,
        pub_id: u64,
    ) -> SharedLemma {
        SharedLemma {
            design_hash,
            property_hash,
            scope,
            clause,
            source: "test".into(),
            publisher_id: pub_id,
        }
    }

    #[test]
    fn scope_isolation_and_dedup() {
        let hub = LemmaHub::new();
        let p1 = hub.next_publisher_id();
        let p2 = hub.next_publisher_id();
        let p3 = hub.next_publisher_id();

        // Publish standing, folded-standing, tautology, duplicate, and frame lemmas.
        let added = hub.publish_batch(vec![
            mk(
                100,
                1,
                LemmaScope::Standing,
                vec![(2, true), (0, false)],
                p1,
            ),
            mk(
                100,
                2,
                LemmaScope::Standing,
                vec![(0, false), (2, true)],
                p2,
            ), // dup
            mk(
                100,
                1,
                LemmaScope::Standing,
                vec![(1, false), (1, true)],
                p1,
            ), // tautology
            mk(
                100,
                1,
                LemmaScope::FoldedStanding {
                    clock: 3,
                    phase: false,
                },
                vec![(4, true)],
                p1,
            ),
            mk(
                200, // different design!
                1,
                LemmaScope::Standing,
                vec![(0, true)],
                p1,
            ),
            mk(
                100,
                1,
                LemmaScope::Frame {
                    level: 2,
                    folded: None,
                },
                vec![(5, false)],
                p1,
            ),
            mk(
                100,
                1,
                LemmaScope::Frame {
                    level: 4,
                    folded: None,
                },
                vec![(5, false)],
                p1,
            ),
        ]);
        assert_eq!(added, 5);
        assert_eq!(hub.stats().deduplicated, 1);

        // Unfolded reader on design 100 sees ONLY Standing (never FoldedStanding or design 200).
        let mut c_unfolded = 0;
        let got = hub.poll_standing(&mut c_unfolded, 100, None, p3);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].clause, vec![(0, false), (2, true)]);
        // Second poll with no new entries is empty.
        assert!(hub.poll_standing(&mut c_unfolded, 100, None, p3).is_empty());

        // Folded reader with matching (3, false) sees BOTH Standing and FoldedStanding.
        let mut c_folded = 0;
        let got_f = hub.poll_standing(&mut c_folded, 100, Some((3, false)), p3);
        assert_eq!(got_f.len(), 2);

        // Folded reader with mismatched clock (3, true) sees ONLY Standing.
        let mut c_mismatch = 0;
        let got_m = hub.poll_standing(&mut c_mismatch, 100, Some((3, true)), p3);
        assert_eq!(got_m.len(), 1);

        // Publisher p1 skips its own standing entries.
        let mut c_self = 0;
        assert!(hub
            .poll_standing(&mut c_self, 100, Some((3, false)), p1)
            .is_empty());

        // Frame polling is scoped to property_hash == 1 (property 2 sees nothing).
        let mut cf1 = 0;
        assert_eq!(hub.poll_frames(&mut cf1, 100, 1, None, p3).len(), 2);
        let mut cf2 = 0;
        assert!(hub.poll_frames(&mut cf2, 100, 2, None, p3).is_empty());
    }

    #[test]
    fn capacity_cap_enforced() {
        let hub = LemmaHub::with_capacity(2, 1);
        let p = hub.next_publisher_id();
        let added = hub.publish_batch(vec![
            mk(1, 1, LemmaScope::Standing, vec![(0, false)], p),
            mk(1, 1, LemmaScope::Standing, vec![(1, false)], p),
            mk(1, 1, LemmaScope::Standing, vec![(2, false)], p),
            mk(
                1,
                1,
                LemmaScope::Frame {
                    level: 2,
                    folded: None,
                },
                vec![(3, false)],
                p,
            ),
            mk(
                1,
                1,
                LemmaScope::Frame {
                    level: 2,
                    folded: None,
                },
                vec![(4, false)],
                p,
            ),
        ]);
        assert_eq!(added, 3);
        assert_eq!(hub.stats().dropped_cap, 2);
    }
}
