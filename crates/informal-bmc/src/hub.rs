//! Canonical translation and zero-trust validation bridge between bit-level
//! [`Prepared`] models and [`informal_engine::LemmaHub`].
//!
//! Every bit-level engine (IC3, IC3-MT, k-induction, CAR, GLA, IC3-INN, IMC,
//! BDD) operates on a simplified, optionally clock-folded and `scorr`-merged
//! [`Prepared`] AIG. This module translates blocked cubes on `Prepared` to and
//! from [`CanonicalClause`]s over original loaded-model latch indices without
//! reloading the model from disk, and validates every imported standing batch
//! via bounded Houdini before adoption.

use crate::prep::{solver, Prepared};
use crate::{certify, ic3, invariant};
use informal_aig::{is_neg, neg, var, Aig, Lit, FALSE, TRUE};
use informal_engine::{CanonicalClause, LemmaHub, LemmaScope, RunContext, SharedLemma};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Maximum literal width of frame lemmas published live to the hub.
const MAX_FRAME_CUBE_LEN: usize = 16;
/// Minimum frame level required before publishing a bounded frame lemma.
const MIN_FRAME_LEVEL: u32 = 2;

/// Bidirectional map between a [`Prepared`] AIG's latch literals and original
/// loaded-model latch indices (`CanonicalClause`).
#[derive(Debug, Clone)]
pub struct LatchCodec {
    /// Valid latch variable indices in `Prepared`.
    p_latch_vars: HashSet<u32>,
    /// Reverse map: prepared latch variable -> `(orig_latch_idx, orig_negated)`.
    /// Meaning `orig_latch[orig_idx] == (var ^ orig_negated)` in `Prepared`.
    rev: HashMap<u32, (u32, bool)>,
    /// Forward map cloned from `Prepared::latch_map`.
    latch_map: Vec<Option<Lit>>,
}

impl LatchCodec {
    /// Build a codec directly from `p` (O(|orig.latches| + |p.latches|), no disk I/O).
    pub fn from_prepared(p: &Prepared) -> Self {
        let p_latch_vars: HashSet<u32> = p.latches.iter().map(|l| var(l.lit)).collect();
        let mut rev = HashMap::with_capacity(p_latch_vars.len());
        for (orig_idx, slot) in p.latch_map.iter().enumerate() {
            if let Some(m) = *slot {
                if m != FALSE && m != TRUE && p_latch_vars.contains(&var(m)) {
                    rev.entry(var(m)).or_insert((orig_idx as u32, is_neg(m)));
                }
            }
        }
        Self {
            p_latch_vars,
            rev,
            latch_map: p.latch_map.clone(),
        }
    }

    /// Encode a blocked `cube` on `Prepared` (meaning invariant clause `∨ !l`)
    /// into a canonical clause over original loaded-model latch indices.
    ///
    /// Returns `None` if any literal is not a mapped original latch (e.g., a
    /// folded synthetic `ok` latch or an IC3-INN shadow latch), or if the
    /// resulting clause is empty/tautological.
    pub fn encode_cube(&self, cube: &[Lit]) -> Option<CanonicalClause> {
        if cube.is_empty() {
            return None;
        }
        let mut clause = Vec::with_capacity(cube.len());
        for &l in cube {
            let &(orig_idx, map_neg) = self.rev.get(&var(l))?;
            // Positive prepared var equals `(orig_idx, map_neg)`.
            // Literal `l` equals `(orig_idx, map_neg ^ is_neg(l))`.
            // Clause literal `!l` inverts once more.
            let clause_neg = !(map_neg ^ is_neg(l));
            clause.push((orig_idx, clause_neg));
        }
        clause.sort_unstable();
        clause.dedup();
        if clause.is_empty() || clause.windows(2).any(|w| w[0].0 == w[1].0) {
            return None;
        }
        Some(clause)
    }

    /// Decode a canonical clause over original latch indices back into a
    /// blocked `ic3::Cube` on `Prepared`.
    ///
    /// Returns `None` if any referenced latch was dropped outside this cone,
    /// the clause is tautologically satisfied in `Prepared`, or the resulting
    /// cube would be empty (contradictory).
    pub fn decode_clause(&self, clause: &CanonicalClause) -> Option<ic3::Cube> {
        let mut cube = Vec::with_capacity(clause.len());
        for &(i, negated) in clause {
            let l = self.latch_map.get(i as usize).copied().flatten()?;
            let l = if negated { neg(l) } else { l };
            if l == TRUE {
                return None; // Clause already holds identically here.
            }
            if l == FALSE {
                continue; // Falsified literal drops out of the disjunction.
            }
            if !self.p_latch_vars.contains(&var(l)) {
                return None;
            }
            cube.push(neg(l));
        }
        cube.sort_unstable();
        cube.dedup();
        if cube.is_empty() {
            return None;
        }
        // Contradictory literals `l` and `!l` in the cube mean a tautological clause.
        for w in cube.windows(2) {
            if var(w[0]) == var(w[1]) {
                return None;
            }
        }
        Some(cube)
    }
}

/// Per-engine-run handle connecting an engine to a shared [`LemmaHub`].
pub struct HubBridge {
    hub: Arc<LemmaHub>,
    design_hash: u64,
    property_hash: u64,
    folded: Option<(usize, bool)>,
    source: String,
    publisher_id: u64,
    codec: LatchCodec,
    standing_cursor: usize,
    frame_cursor: usize,
    seen_standing: HashSet<ic3::Cube>,
    publish_frames: bool,
    import_frames: bool,
}

impl HubBridge {
    /// Create a bridge for `p` if `ctx.lemma_hub` is attached.
    pub fn new(p: &Prepared, ctx: &RunContext, source: impl Into<String>) -> Option<Self> {
        let hub = ctx.lemma_hub.as_ref()?.clone();
        let publisher_id = hub.next_publisher_id();
        let frames_on = LemmaHub::frames_enabled();
        Some(Self {
            hub,
            design_hash: p.design_hash,
            property_hash: p.property_hash,
            folded: p.folded.map(|f| (f.clock, f.phase)),
            source: source.into(),
            publisher_id,
            codec: LatchCodec::from_prepared(p),
            standing_cursor: 0,
            frame_cursor: 0,
            seen_standing: HashSet::new(),
            publish_frames: frames_on,
            import_frames: frames_on,
        })
    }

    /// Enable or disable live bounded-frame publishing (standing invariants are
    /// always published).
    pub fn with_frame_publish(mut self, enabled: bool) -> Self {
        self.publish_frames = enabled;
        self
    }

    /// Enable or disable live bounded-frame importing (standing invariants are
    /// always imported).
    pub fn with_frame_import(mut self, enabled: bool) -> Self {
        self.import_frames = enabled;
        self
    }

    /// True when this bridge imports bounded frame candidates from the hub.
    #[inline]
    pub fn import_frames_enabled(&self) -> bool {
        self.import_frames
    }

    /// Current standing-lemma cursor position.
    pub fn standing_cursor(&self) -> usize {
        self.standing_cursor
    }

    /// Start polling standing lemmas from `cursor` (avoids re-validating
    /// startup entries already seeded for a multi-worker fleet).
    pub fn with_standing_cursor(mut self, cursor: usize) -> Self {
        self.standing_cursor = cursor;
        self
    }

    /// This bridge's publisher identifier.
    pub fn publisher_id(&self) -> u64 {
        self.publisher_id
    }

    /// Share one `publisher_id` across sibling workers of a single multi-worker
    /// engine (e.g. `Ic3Mt`) so workers do not re-import each other's lemmas
    /// through `LemmaHub` when they already share an internal `LemmaPool`.
    pub fn with_publisher_id(mut self, publisher_id: u64) -> Self {
        self.publisher_id = publisher_id;
        self
    }

    /// Standing scope matching this model's fold state.
    pub fn standing_scope(&self) -> LemmaScope {
        match self.folded {
            None => LemmaScope::Standing,
            Some((clock, phase)) => LemmaScope::FoldedStanding { clock, phase },
        }
    }

    /// Lock-free check for unseen standing invariants in the hub.
    #[inline]
    pub fn has_new_standing(&self) -> bool {
        self.standing_cursor < self.hub.standing_seq()
    }

    /// Lock-free check for unseen bounded frame lemmas in the hub.
    #[inline]
    pub fn has_new_frames(&self) -> bool {
        self.frame_cursor < self.hub.frame_seq()
    }

    /// Atomically publish a batch of Houdini-proved standing invariant cubes.
    pub fn publish_standing(&self, cubes: &[ic3::Cube]) -> usize {
        if cubes.is_empty() {
            return 0;
        }
        let scope = self.standing_scope();
        let entries: Vec<SharedLemma> = cubes
            .iter()
            .filter_map(|c| {
                let clause = self.codec.encode_cube(c)?;
                Some(SharedLemma {
                    design_hash: self.design_hash,
                    property_hash: self.property_hash,
                    scope,
                    clause,
                    source: self.source.clone(),
                    publisher_id: self.publisher_id,
                })
            })
            .collect();
        self.hub.publish_batch(entries)
    }

    /// Publish one compact bounded frame cube (`level >= 2`, `len <= 16`).
    pub fn publish_frame(&self, cube: &ic3::Cube, level: u32) -> bool {
        if !self.publish_frames
            || level < MIN_FRAME_LEVEL
            || cube.is_empty()
            || cube.len() > MAX_FRAME_CUBE_LEN
        {
            return false;
        }
        let Some(clause) = self.codec.encode_cube(cube) else {
            return false;
        };
        self.hub.publish(SharedLemma {
            design_hash: self.design_hash,
            property_hash: self.property_hash,
            scope: LemmaScope::Frame {
                level,
                folded: self.folded,
            },
            clause,
            source: self.source.clone(),
            publisher_id: self.publisher_id,
        })
    }

    /// Poll newly published compatible standing invariants and re-validate them
    /// alongside `existing` via bounded Houdini on `g`.
    ///
    /// Mutually inductive batches survive together; any false or non-inductive
    /// candidate is guaranteed rejected and counted in [`LemmaHub::stats`].
    pub fn poll_standing_verified(
        &mut self,
        g: &Aig,
        existing: &[ic3::Cube],
        ctx: &RunContext,
        max_budget: Duration,
    ) -> Vec<ic3::Cube> {
        if !self.has_new_standing() || ctx.should_stop() {
            return Vec::new();
        }
        let raw = self.hub.poll_standing(
            &mut self.standing_cursor,
            self.design_hash,
            self.folded,
            self.publisher_id,
        );
        if raw.is_empty() {
            return Vec::new();
        }
        let existing_set: HashSet<&ic3::Cube> = existing.iter().collect();
        let mut cands: Vec<ic3::Cube> = Vec::new();
        let mut cand_seen: HashSet<ic3::Cube> = HashSet::new();
        for entry in &raw {
            if let Some(cube) = self.codec.decode_clause(&entry.clause) {
                if !existing_set.contains(&cube)
                    && !self.seen_standing.contains(&cube)
                    && cand_seen.insert(cube.clone())
                {
                    cands.push(cube);
                }
            }
        }
        if cands.is_empty() {
            return Vec::new();
        }
        let n_cands = cands.len() as u64;
        let now = Instant::now();
        let rest = ctx.deadline.saturating_duration_since(now);
        let budget = max_budget.min((rest / 10).max(Duration::from_millis(5)).min(rest));
        let mut h = ctx.clone();
        h.deadline = now + budget;

        let mut all_cands = Vec::with_capacity(existing.len() + cands.len());
        all_cands.extend(existing.iter().cloned());
        all_cands.extend(cands.iter().cloned());

        let Some(inv) = invariant::houdini(g, all_cands, || solver(&h), &h) else {
            // Timed out before Houdini converged: count candidates tried, none accepted/rejected.
            self.hub.record_import(n_cands, 0, 0);
            return Vec::new();
        };
        let kept: HashSet<ic3::Cube> = inv.into_iter().collect();
        let mut accepted = Vec::new();
        for c in cands {
            if kept.contains(&c) {
                self.seen_standing.insert(c.clone());
                accepted.push(c);
            }
        }
        let n_acc = accepted.len() as u64;
        let n_rej = n_cands.saturating_sub(n_acc);
        self.hub.record_import(n_cands, n_acc, n_rej);
        accepted
    }

    /// Poll newly published bounded frame candidates for this exact property.
    /// The caller MUST validate initiation and relative induction before adopting.
    pub fn poll_frame_candidates(&mut self) -> Vec<(ic3::Cube, u32)> {
        if !self.has_new_frames() {
            return Vec::new();
        }
        let raw = self.hub.poll_frames(
            &mut self.frame_cursor,
            self.design_hash,
            self.property_hash,
            self.folded,
            self.publisher_id,
        );
        raw.into_iter()
            .filter_map(|(cl, lvl)| Some((self.codec.decode_clause(&cl)?, lvl)))
            .collect()
    }

    /// Record whether one checked frame candidate was accepted or rejected.
    pub fn record_frame_result(&self, accepted: bool) {
        self.hub
            .record_import(1, u64::from(accepted), u64::from(!accepted));
    }

    /// Access the underlying latch codec (e.g., for custom translation in GLA).
    pub fn codec(&self) -> &LatchCodec {
        &self.codec
    }
}

/// Startup convenience: create a [`HubBridge`] and import all currently
/// available verified standing invariants from `ctx.lemma_hub`.
pub fn seed_from_hub(
    p: &Prepared,
    ctx: &RunContext,
    source: impl Into<String>,
) -> (Option<HubBridge>, Vec<ic3::Cube>) {
    let Some(mut bridge) = HubBridge::new(p, ctx, source) else {
        return (None, Vec::new());
    };
    let inv = bridge.poll_standing_verified(p, &[], ctx, Duration::from_secs(2));
    (Some(bridge), inv)
}

/// Publish an independently certified invariant (`certify::check` or
/// `certify::check_folded` already succeeded on `orig`) into `ctx.lemma_hub`.
pub fn publish_certified_to_hub(
    orig: &Aig,
    p: &Prepared,
    inv: &[certify::Clause],
    ctx: &RunContext,
) {
    let Some(hub) = &ctx.lemma_hub else { return };
    if inv.is_empty() {
        return;
    }
    let index: HashMap<u32, u32> = orig
        .latches
        .iter()
        .enumerate()
        .map(|(i, l)| (var(l.lit), i as u32))
        .collect();
    let scope = match p.folded {
        None => LemmaScope::Standing,
        Some(f) => LemmaScope::FoldedStanding {
            clock: f.clock,
            phase: f.phase,
        },
    };
    let pub_id = hub.next_publisher_id();
    let entries: Vec<SharedLemma> = inv
        .iter()
        .filter_map(|c| {
            let clause: CanonicalClause = c
                .iter()
                .map(|&l| Some((*index.get(&var(l))?, is_neg(l))))
                .collect::<Option<_>>()?;
            Some(SharedLemma {
                design_hash: p.design_hash,
                property_hash: p.property_hash,
                scope,
                clause,
                source: "certified".into(),
                publisher_id: pub_id,
            })
        })
        .collect();
    hub.publish_batch(entries);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prep::prepare_ctx_fold;
    use informal_engine::{CheckRequest, ModelFormat};

    #[test]
    fn codec_round_trips_positive_complemented_and_constant_latches() {
        let mut aig = Aig::new();
        let l0 = aig.latch(informal_aig::Init::Zero);
        let l1 = aig.latch(informal_aig::Init::Zero);
        let codec = LatchCodec {
            p_latch_vars: [var(l0), var(l1)].into_iter().collect(),
            // orig 0 -> l0 (pos), orig 1 -> !l1 (neg), orig 2 -> FALSE, orig 3 -> None
            rev: [(var(l0), (0, false)), (var(l1), (1, true))]
                .into_iter()
                .collect(),
            latch_map: vec![Some(l0), Some(neg(l1)), Some(FALSE), None],
        };

        // Blocked cube `[l0, l1]` means `!(l0 & l1)` = `!orig0 \/ orig1`.
        let cube = vec![l0, l1];
        let cl = codec.encode_cube(&cube).unwrap();
        assert_eq!(cl, vec![(0, true), (1, false)]);
        let back = codec.decode_clause(&cl).unwrap();
        assert_eq!(back, cube);

        // Clause mentioning constant FALSE literal (orig 2 positive) drops it cleanly.
        let with_false = vec![(0, true), (2, false)];
        assert_eq!(codec.decode_clause(&with_false).unwrap(), vec![l0]);

        // Clause mentioning constant TRUE literal (!orig 2) is tautological -> None.
        let with_true = vec![(0, true), (2, true)];
        assert!(codec.decode_clause(&with_true).is_none());

        // Clause mentioning dropped latch (orig 3) -> None.
        let with_dropped = vec![(0, true), (3, false)];
        assert!(codec.decode_clause(&with_dropped).is_none());
    }

    #[test]
    fn hub_bridge_validates_inductive_and_rejects_false_clauses() {
        if informal_sat::Cadical::signature().is_none() {
            return;
        }
        // 3-bit wrapping counter 0..=5, bad == 7.
        let btor = "\
1 sort bitvec 3
2 sort bitvec 1
3 zero 1
4 state 1 cnt
5 init 1 4 3
6 one 1
7 add 1 4 6
8 constd 1 5
9 eq 2 4 8
10 ite 1 9 3 7
11 next 1 4 10
12 constd 1 7
13 eq 2 4 12
14 bad 13
";
        let dir = std::env::temp_dir().join(format!("informal-hub-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cnt.btor2");
        std::fs::write(&path, btor).unwrap();
        let req = CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(10));
        let hub = Arc::new(LemmaHub::new());
        let ctx = RunContext::new(req.timeout).with_lemma_hub(hub.clone());
        let p = prepare_ctx_fold(&req, &ctx, false).unwrap();
        assert_eq!(p.latches.len(), 3);

        let bits: Vec<Lit> = p.latches.iter().map(|l| l.lit).collect();
        let val_cube = |v: u32| -> ic3::Cube {
            let mut c: Vec<Lit> = (0..3)
                .map(|i| {
                    if (v >> i) & 1 == 1 {
                        bits[i]
                    } else {
                        neg(bits[i])
                    }
                })
                .collect();
            c.sort_unstable();
            c
        };

        // Publisher A injects two TRUE mutually inductive cubes (not 6, not 7)
        // AND one FALSE cube (not 3, reachable at step 3!).
        let pub_a = HubBridge::new(&p, &ctx, "pub-a").unwrap();
        let added = pub_a.publish_standing(&[val_cube(6), val_cube(7), val_cube(3)]);
        assert_eq!(added, 3);

        // Receiver B validates via Houdini: MUST accept {6, 7} and REJECT {3}!
        let (bridge_b, accepted) = seed_from_hub(&p, &ctx, "recv-b");
        assert!(bridge_b.is_some());
        let mut got = accepted;
        got.sort();
        let mut want = vec![val_cube(6), val_cube(7)];
        want.sort();
        assert_eq!(got, want);
        let st = hub.stats();
        assert_eq!(st.published_standing, 3);
        assert_eq!(st.accepted, 2);
        assert_eq!(st.rejected, 1);
    }

    #[test]
    fn ic3_frame_import_opt_in_rejects_false_frame_clauses() {
        if informal_sat::Cadical::signature().is_none() {
            return;
        }
        // Input-enabled 4-bit counter hitting 5 at depth 5.
        let btor = "\
1 sort bitvec 4
2 sort bitvec 1
3 zero 1
4 one 1
5 state 1 cnt
6 init 1 5 3
7 input 2 en
8 add 1 5 4
9 ite 1 7 8 5
10 next 1 5 9
11 constd 1 5
12 eq 2 5 11
13 bad 12
";
        let dir = std::env::temp_dir().join(format!("informal-hub-frames-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cnt5.btor2");
        std::fs::write(&path, btor).unwrap();
        let req = CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(10));
        let hub = Arc::new(LemmaHub::new());
        let ctx = RunContext::new(req.timeout).with_lemma_hub(hub.clone());
        let p = prepare_ctx_fold(&req, &ctx, false).unwrap();

        // Inject two FALSE frame clauses at level 8: blocking Init (0) and step-1 state (1).
        let adv = hub.next_publisher_id();
        hub.publish_batch(vec![
            SharedLemma {
                design_hash: p.design_hash,
                property_hash: p.property_hash,
                scope: LemmaScope::Frame {
                    level: 8,
                    folded: None,
                },
                clause: vec![(0, false), (1, false), (2, false), (3, false)],
                source: "adv".into(),
                publisher_id: adv,
            },
            SharedLemma {
                design_hash: p.design_hash,
                property_hash: p.property_hash,
                scope: LemmaScope::Frame {
                    level: 8,
                    folded: None,
                },
                clause: vec![(0, true), (1, false), (2, false), (3, false)],
                source: "adv".into(),
                publisher_id: adv,
            },
        ]);

        // Attach bridge with frame import explicitly enabled and verify both false frames are rejected.
        let bridge = HubBridge::new(&p, &ctx, "ic3-test")
            .unwrap()
            .with_frame_import(true);
        let c = ctx.clone();
        let mut runner = ic3::Ic3::new(&p, Box::new(move || solver(&c))).unwrap();
        runner.attach_hub(bridge);
        assert_eq!(
            runner.check(&ctx),
            informal_engine::Verdict::Unsafe { depth: Some(5) }
        );
        assert_eq!(hub.stats().accepted, 0);
        assert_eq!(hub.stats().rejected, 2);
    }
}
