//! Phase 3 learned engine selector (`Btor2-Select` / AIGER+Btor2 Ridge ranker).
//!
//! Zero-dependency, deterministic linear utility model over 10 scale-invariant
//! structural features extracted from [`informal_ir::Features`]. Trained offline
//! from `informal bench --each` JSONL records via `scripts/train_selector.py`
//! (L2-regularized Ridge regression, 5-fold cross-validated against the Virtual
//! Best Solver).
//!
//! Runtime controls:
//! - `INFORMAL_SELECTOR=rules`: bypass learned scores and use capability rules.
//! - `INFORMAL_SELECTOR_MODEL=<path>`: load a custom JSON model produced by
//!   `scripts/train_selector.py`.
//! - `INFORMAL_PORTFOLIO_SLOTS=<n>`: override the host/size-aware slot budget.

use crate::portfolio::{self, ScheduleConfig, ScheduledEngine};
use crate::{Effort, Engine, EngineDomain, EngineKind};
use informal_ir::Features;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Complete staged-wave execution plan: ordered Wave-0/Wave-1 engine schedule
/// plus the host- and model-size-aware concurrency cap.
#[derive(Clone)]
pub struct PortfolioPlan {
    pub schedule: Vec<ScheduledEngine>,
    pub config: ScheduleConfig,
}

/// Wave-1 delay derived from total timeout: short enough to rescue stalled
/// 10 s checks, long enough to keep fast (<1.5 s) models completely free of
/// secondary engine startup overhead.
pub(crate) fn wave1_delay(timeout: Duration) -> Duration {
    (timeout / 6).clamp(Duration::from_millis(750), Duration::from_millis(2_500))
}

/// Deterministic fallback priority rank ensuring prover + bug-finder diversity
/// when the learned selector is disabled or on liveness/array/combinational cones.
pub(crate) fn base_priority(e: &Arc<dyn Engine>, f: &Features) -> u8 {
    let c = e.capabilities();
    if f.justice > 0 {
        return match c.kind {
            EngineKind::Liveness => 0,
            EngineKind::Ic3 if c.multi_worker => 1,
            EngineKind::Bmc if !c.deep_step => 2,
            _ => 5,
        };
    }
    if f.array_states > 0 || f.has_arrays {
        return match (c.domain, c.kind) {
            (EngineDomain::WordLevel, EngineKind::Ic3) => 0,
            (EngineDomain::WordLevel, EngineKind::Bmc) => 1,
            (EngineDomain::WordLevel, EngineKind::KInduction) => 2,
            _ => 5,
        };
    }
    match (c.domain, c.kind, c.multi_worker, c.deep_step) {
        (EngineDomain::BitLevel, EngineKind::Ic3, true, _) => 0,
        (EngineDomain::BitLevel, EngineKind::Bmc, _, false) if c.min_effort <= Effort::Low => 1,
        (EngineDomain::BitLevel, EngineKind::Bdd, _, _) => 2,
        (EngineDomain::BitLevel, EngineKind::Hybrid, _, _) => 3,
        (EngineDomain::BitLevel, EngineKind::KInduction, _, _) => 4,
        (EngineDomain::BitLevel, EngineKind::Swarm, _, _) => 5,
        (EngineDomain::BitLevel, EngineKind::Bmc, _, false) => 6,
        (EngineDomain::BitLevel, EngineKind::Car, _, _) => 7,
        (EngineDomain::BitLevel, EngineKind::Interpolation, _, _) => 8,
        (EngineDomain::BitLevel, EngineKind::Bmc, _, true) => 9,
        (EngineDomain::BitLevel, EngineKind::Ic3, false, _) => 10,
        (EngineDomain::WordLevel, _, _, _) => 11,
        _ => 12,
    }
}

/// Assemble a [`PortfolioPlan`] using the learned Ridge selector for sequential
/// safety models (called by `selector::plan_for_features`).
pub(crate) fn plan_sequential_safety(
    engines: &[Arc<dyn Engine>],
    f: &Features,
    effort: Effort,
    timeout: Duration,
    cap: usize,
) -> PortfolioPlan {
    let config = ScheduleConfig { max_slots: cap };
    let base: Vec<_> = engines
        .iter()
        .filter(|e| e.capabilities().kind != EngineKind::Liveness)
        .cloned()
        .collect();
    let keep = |pool: Vec<Arc<dyn Engine>>, pred: &dyn Fn(&Arc<dyn Engine>) -> bool| {
        let next: Vec<_> = pool.iter().filter(|e| pred(e)).cloned().collect();
        if next.is_empty() {
            pool
        } else {
            next
        }
    };
    let capped = keep(base, &|e| {
        e.capabilities()
            .max_state_bits
            .is_none_or(|max| f.state_bits <= max)
    });
    let max_eff = if effort == Effort::Standard {
        Effort::High
    } else {
        effort
    };
    let eff_ok = keep(capped, &|e| e.capabilities().min_effort <= max_eff);
    let arith_heavy = f.arith_ops >= 8 && f.max_width >= 16;
    let tiny_state = f.state_bits <= 16;
    let pool = keep(eff_ok, &|e| {
        !(e.capabilities().deep_step && (tiny_state || arith_heavy))
    });

    let (wave0, wave1) = default_model().rank_and_partition(&pool, f, cap);
    let mut schedule: Vec<ScheduledEngine> =
        wave0.into_iter().map(ScheduledEngine::immediate).collect();
    if portfolio::staged_enabled() {
        let delay = wave1_delay(timeout);
        for e in wave1 {
            schedule.push(ScheduledEngine::delayed(e, delay));
        }
    }
    PortfolioPlan { schedule, config }
}

pub const NUM_FEATURES: usize = 10;

pub type EngineList = Vec<Arc<dyn Engine>>;

/// Serialized Ridge utility ranker (compatible with `scripts/train_selector.py`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectorModel {
    pub version: u32,
    pub means: [f64; NUM_FEATURES],
    pub stds: [f64; NUM_FEATURES],
    pub redundancy_penalty: f64,
    pub weights: HashMap<String, [f64; NUM_FEATURES]>,
}

/// Whether the learned selector is active (`INFORMAL_SELECTOR=rules` disables it).
pub fn enabled() -> bool {
    std::env::var("INFORMAL_SELECTOR").map_or(true, |v| !v.trim().eq_ignore_ascii_case("rules"))
}

/// Extract the 10 scale-invariant structural features used by [`SelectorModel`].
/// Matches `scripts/train_selector.py::raw_features` verbatim.
pub fn extract_features(f: &Features) -> [f64; NUM_FEATURES] {
    let states = f.states as f64;
    let state_bits = if f.state_bits > 0 {
        f.state_bits as f64
    } else {
        states
    };
    let nodes = f.nodes as f64;
    let inputs = f.inputs as f64;
    let constraints = f.constraints as f64;
    let arith_ops = f.arith_ops as f64;
    let max_width = f.max_width.max(1) as f64;
    [
        1.0,
        states.ln_1p(),
        state_bits.ln_1p(),
        nodes.ln_1p(),
        inputs.ln_1p(),
        constraints.ln_1p(),
        (nodes / states.max(1.0)).ln_1p(),
        (inputs / states.max(1.0)).ln_1p(),
        arith_ops / nodes.max(1.0),
        max_width.ln_1p(),
    ]
}

/// Size- and effort-aware concurrent thread-slot ceiling.
///
/// Large chip-level cones (`>= 250k` nodes or `>= 20k` state bits) hold
/// 100–300 MB per solver and saturate memory bandwidth above ~6 threads;
/// small/medium cones stay cache-resident and benefit from wider parallelism.
/// Explicit `INFORMAL_PORTFOLIO_SLOTS=<n>` overrides this scaling.
pub fn size_aware_slot_cap(base_cap: usize, f: &Features, effort: Effort) -> usize {
    if base_cap == 0 {
        return 0;
    }
    if std::env::var_os("INFORMAL_PORTFOLIO_SLOTS").is_some() {
        return base_cap;
    }
    let effort_bonus = match effort {
        Effort::Low | Effort::Standard => 0,
        Effort::High => 2,
        Effort::Max => 4,
    };
    let size_cap = if f.nodes >= 250_000 || f.state_bits >= 20_000 {
        6 + effort_bonus
    } else if f.nodes >= 50_000 || f.state_bits >= 2_000 {
        8 + effort_bonus
    } else {
        10 + effort_bonus
    };
    base_cap.min(size_cap)
}

/// Capability-derived diversity bucket used to penalize redundant siblings
/// without suppressing complementary paradigms (bit vs word domain, step-1 vs
/// jump-step BMC, or CEGAR gate-level abstraction vs k-induction+IC3).
fn diversity_bucket(c: &crate::Capabilities) -> (EngineDomain, EngineKind, u8) {
    let sub = match c.kind {
        EngineKind::Bmc => u8::from(c.min_effort > Effort::Low),
        EngineKind::Hybrid => u8::from(c.min_effort > Effort::Standard),
        _ => 0,
    };
    (c.domain, c.kind, sub)
}

/// Half-budget opportunity-cost penalty for multi-worker engines whose slot
/// footprint would exceed half of Wave 0's thread budget.
fn slot_pressure(slots: usize, wave0_slot_cap: usize) -> f64 {
    if wave0_slot_cap > 0 && slots > 1 && slots * 2 > wave0_slot_cap {
        0.08 * (slots - 1) as f64 / wave0_slot_cap as f64
    } else {
        0.0
    }
}

impl SelectorModel {
    /// Parse a JSON model file produced by `scripts/train_selector.py`.
    pub fn from_json_str(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw)
    }

    /// Standardize a raw feature vector using the model's training statistics.
    pub fn standardize(&self, raw: &[f64; NUM_FEATURES]) -> [f64; NUM_FEATURES] {
        let mut out = [0.0; NUM_FEATURES];
        out[0] = 1.0;
        for i in 1..NUM_FEATURES {
            let s = if self.stds[i].abs() > 1e-9 {
                self.stds[i]
            } else {
                1.0
            };
            out[i] = (raw[i] - self.means[i]) / s;
        }
        out
    }

    /// Predict utility score for `engine` on standardized features `x`.
    /// Engines absent from the trained table (e.g., word-level SMT or external
    /// solvers) receive a capability-derived prior so they remain competitive.
    pub fn score(&self, engine: &Arc<dyn Engine>, x: &[f64; NUM_FEATURES], f: &Features) -> f64 {
        if let Some(w) = self.weights.get(engine.name()) {
            return w.iter().zip(x.iter()).map(|(a, b)| a * b).sum();
        }
        let c = engine.capabilities();
        let arith_heavy = f.arith_ops >= 8 && f.max_width >= 16;
        match (c.domain, c.kind) {
            (EngineDomain::WordLevel, EngineKind::Ic3) if arith_heavy || f.has_arrays => 0.34,
            (EngineDomain::WordLevel, EngineKind::Bmc) if arith_heavy || f.has_arrays => 0.28,
            (EngineDomain::WordLevel, EngineKind::KInduction) if arith_heavy || f.has_arrays => {
                0.26
            }
            (EngineDomain::WordLevel, _) => 0.14,
            (_, EngineKind::Ic3) if c.proves => 0.23,
            (_, EngineKind::KInduction | EngineKind::Hybrid | EngineKind::Interpolation) => 0.20,
            (_, EngineKind::Bmc) if !c.deep_step => 0.18,
            (_, EngineKind::Bdd | EngineKind::Car | EngineKind::Swarm) => 0.16,
            _ if c.proves => 0.19,
            _ => 0.12,
        }
    }

    /// Complementarity-aware greedy partitioning into `(wave0, wave1)` under a
    /// thread-slot budget `wave0_slot_cap` (`0` = all candidates in Wave 0).
    ///
    /// Guarantees Wave 0 includes at least one prover and up to two distinct-bucket
    /// dedicated bug-finders (when `wave0_slot_cap == 0 || wave0_slot_cap >= 6`)
    /// before filling remaining slots using predicted utility minus slot-pressure
    /// and `redundancy_penalty` per already-picked engine in the same
    /// capability diversity bucket.
    pub fn rank_and_partition(
        &self,
        candidates: &[Arc<dyn Engine>],
        f: &Features,
        wave0_slot_cap: usize,
    ) -> (EngineList, EngineList) {
        if candidates.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let x = self.standardize(&extract_features(f));
        let scores: Vec<f64> = candidates.iter().map(|e| self.score(e, &x, f)).collect();
        let buckets: Vec<(EngineDomain, EngineKind, u8)> = candidates
            .iter()
            .map(|e| diversity_bucket(&e.capabilities()))
            .collect();

        let mut picked_idx: Vec<usize> = Vec::with_capacity(candidates.len());
        let mut picked_set: HashSet<usize> = HashSet::with_capacity(candidates.len());
        let mut used_slots = 0usize;
        let mut bucket_counts: HashMap<(EngineDomain, EngineKind, u8), usize> = HashMap::new();

        let fits = |idx: usize, used: usize| -> bool {
            let need = candidates[idx].capabilities().slots();
            used == 0 || wave0_slot_cap == 0 || used + need <= wave0_slot_cap
        };

        let eff =
            |idx: usize, counts: &HashMap<(EngineDomain, EngineKind, u8), usize>, pen: f64| {
                let need = candidates[idx].capabilities().slots();
                let dup = *counts.get(&buckets[idx]).unwrap_or(&0) as f64;
                scores[idx] - slot_pressure(need, wave0_slot_cap) - pen * dup
            };

        let pick =
            |idx: usize,
             picked_idx: &mut Vec<usize>,
             picked_set: &mut HashSet<usize>,
             used_slots: &mut usize,
             counts: &mut HashMap<(EngineDomain, EngineKind, u8), usize>| {
                picked_idx.push(idx);
                picked_set.insert(idx);
                *used_slots += candidates[idx].capabilities().slots();
                *counts.entry(buckets[idx]).or_insert(0) += 1;
            };

        // 1. Best prover that fits.
        if let Some(best_p) = (0..candidates.len())
            .filter(|&i| candidates[i].capabilities().proves && fits(i, used_slots))
            .max_by(|&a, &b| {
                eff(a, &bucket_counts, 0.0)
                    .total_cmp(&eff(b, &bucket_counts, 0.0))
                    .then_with(|| b.cmp(&a))
            })
        {
            pick(
                best_p,
                &mut picked_idx,
                &mut picked_set,
                &mut used_slots,
                &mut bucket_counts,
            );
        }

        // 2a. Best dedicated bug-finder (BMC/Swarm) that fits.
        if let Some(best_b) = (0..candidates.len())
            .filter(|&i| {
                let c = candidates[i].capabilities();
                !picked_set.contains(&i) && c.finds_bugs && !c.proves && fits(i, used_slots)
            })
            .max_by(|&a, &b| {
                eff(a, &bucket_counts, self.redundancy_penalty)
                    .total_cmp(&eff(b, &bucket_counts, self.redundancy_penalty))
                    .then_with(|| b.cmp(&a))
            })
        {
            pick(
                best_b,
                &mut picked_idx,
                &mut picked_set,
                &mut used_slots,
                &mut bucket_counts,
            );
        }

        // 2b. Second dedicated bug-finder from a distinct diversity bucket
        // (e.g., jump-step BMC alongside step-1 BMC, or bit-level BMC alongside
        // word-level BMC) when Wave 0 has >= 6 slots.
        if wave0_slot_cap == 0 || wave0_slot_cap >= 6 {
            if let Some(best_b2) = (0..candidates.len())
                .filter(|&i| {
                    let c = candidates[i].capabilities();
                    !picked_set.contains(&i)
                        && c.finds_bugs
                        && !c.proves
                        && *bucket_counts.get(&buckets[i]).unwrap_or(&0) == 0
                        && fits(i, used_slots)
                })
                .max_by(|&a, &b| {
                    eff(a, &bucket_counts, self.redundancy_penalty)
                        .total_cmp(&eff(b, &bucket_counts, self.redundancy_penalty))
                        .then_with(|| b.cmp(&a))
                })
            {
                pick(
                    best_b2,
                    &mut picked_idx,
                    &mut picked_set,
                    &mut used_slots,
                    &mut bucket_counts,
                );
            }
        }

        // 3. Greedily fill remaining Wave-0 slots with diversity penalty.
        loop {
            let next = (0..candidates.len())
                .filter(|&i| !picked_set.contains(&i) && fits(i, used_slots))
                .max_by(|&a, &b| {
                    eff(a, &bucket_counts, self.redundancy_penalty)
                        .total_cmp(&eff(b, &bucket_counts, self.redundancy_penalty))
                        .then_with(|| b.cmp(&a))
                });
            match next {
                Some(i) => pick(
                    i,
                    &mut picked_idx,
                    &mut picked_set,
                    &mut used_slots,
                    &mut bucket_counts,
                ),
                None => break,
            }
        }

        let wave0: Vec<Arc<dyn Engine>> =
            picked_idx.iter().map(|&i| candidates[i].clone()).collect();

        // Wave 1: remaining eligible engines ranked by utility with diversity.
        let mut rem_idx: Vec<usize> = (0..candidates.len())
            .filter(|i| !picked_set.contains(i))
            .collect();
        rem_idx.sort_by(|&a, &b| {
            let pa = eff(a, &bucket_counts, 0.5 * self.redundancy_penalty);
            let pb = eff(b, &bucket_counts, 0.5 * self.redundancy_penalty);
            pb.total_cmp(&pa).then_with(|| a.cmp(&b))
        });
        let wave1: Vec<Arc<dyn Engine>> =
            rem_idx.into_iter().map(|i| candidates[i].clone()).collect();
        (wave0, wave1)
    }
}

/// Process-wide selector model: loads `INFORMAL_SELECTOR_MODEL` when set and
/// valid, otherwise uses the built-in HWMCC'24 Ridge model (`lam = 50.0`).
pub fn default_model() -> &'static SelectorModel {
    static MODEL: OnceLock<SelectorModel> = OnceLock::new();
    MODEL.get_or_init(|| {
        if let Ok(path) = std::env::var("INFORMAL_SELECTOR_MODEL") {
            if let Ok(raw) = std::fs::read_to_string(&path) {
                match SelectorModel::from_json_str(&raw) {
                    Ok(m) => return m,
                    Err(e) => eprintln!("informal: ignoring invalid selector model {path}: {e}"),
                }
            }
        }
        builtin_model()
    })
}

/// Built-in default Ridge model (`lam = 50.0`, trained on 219 models: 179
/// HWMCC'24 AIGER + 40 word-level Btor2 BV/array, 18 safety engines; 5-fold CV
/// `119/219`, OOF PAR-2 `2149.8 s` vs `2131.3 s` VBS, Btor2 `33/40` `+0.2 s`).
pub fn builtin_model() -> SelectorModel {
    #[rustfmt::skip]
    let entries: &[(&str, [f64; NUM_FEATURES])] = &[
        ("informal-bcar",        [0.24159, -0.02437, -0.03241, -0.05494, -0.06946,  0.01272, -0.06058,  0.04238, -0.06868,  0.01414]),
        ("informal-bdd",         [0.21685, -0.02646, -0.03977, -0.04930, -0.05593, -0.00269, -0.04683, -0.03419, -0.09811,  0.02802]),
        ("informal-bmc",         [0.13362, -0.01746, -0.00815, -0.02281, -0.01003, -0.01824, -0.00695,  0.01472, -0.00181,  0.02196]),
        ("informal-bmc-10",      [0.12647, -0.01724, -0.00763, -0.02344, -0.01878, -0.01242, -0.00794,  0.00705,  0.00424,  0.02134]),
        ("informal-bmc-65",      [0.09931, -0.01965, -0.01143, -0.02806, -0.03194,  0.00359, -0.01269, -0.01199,  0.00111,  0.01858]),
        ("informal-car",         [0.23312, -0.02749, -0.02699, -0.05871, -0.08170, -0.01930, -0.05363,  0.03824, -0.07853,  0.03653]),
        ("informal-gla",         [0.27798, -0.03120, -0.03757, -0.06270, -0.07299,  0.02621, -0.05768,  0.03408, -0.09123,  0.01908]),
        ("informal-ic3",         [0.29972, -0.03004, -0.03333, -0.07438, -0.06027,  0.01048, -0.08188,  0.00753, -0.09052,  0.02356]),
        ("informal-ic3-cadical", [0.27983, -0.02824, -0.03385, -0.06430, -0.07162,  0.02066, -0.06642,  0.03602, -0.09213,  0.02095]),
        ("informal-ic3-inn",     [0.23121, -0.02363, -0.02604, -0.05418, -0.06647,  0.00913, -0.05713,  0.04481, -0.08213,  0.03017]),
        ("informal-ic3-mt",      [0.29702, -0.02656, -0.02888, -0.06665, -0.06245,  0.01853, -0.07247,  0.03654, -0.08948,  0.01510]),
        ("informal-ic3sa",       [0.53777, -0.01009, -0.04868, -0.01348,  0.00054,  0.00132, -0.00442,  0.00358, -0.08370, -0.03166]),
        ("informal-imc",         [0.24301, -0.02308, -0.02363, -0.05162, -0.04959,  0.00021, -0.05316,  0.04739, -0.06623,  0.03775]),
        ("informal-kind",        [0.28777, -0.03918, -0.04176, -0.07368, -0.07602,  0.00095, -0.06021,  0.01825, -0.07457,  0.02100]),
        ("informal-kind-ic3",    [0.30975, -0.03580, -0.04205, -0.07338, -0.06831,  0.01615, -0.06919,  0.02944, -0.08261,  0.01612]),
        ("informal-swarm",       [0.09221, -0.01746, -0.00655, -0.02558, -0.03594,  0.00879, -0.00792,  0.01332,  0.00074,  0.02742]),
        ("informal-wbmc",        [0.26132, -0.01819, -0.01978, -0.01443, -0.00944,  0.00429,  0.01195,  0.00038, -0.01303, -0.01389]),
        ("informal-wkind",       [0.63702, -0.01422, -0.00263, -0.01898, -0.01172,  0.00086, -0.00590, -0.01756, -0.04952, -0.03395]),
    ];
    SelectorModel {
        version: 1,
        means: [
            0.0, 7.30511, 7.64278, 10.08583, 6.33024, 1.44464, 2.93135, 0.67389, 0.01542, 1.13242,
        ],
        stds: [
            1.0, 3.75020, 3.34194, 3.71277, 3.05094, 1.59132, 1.66510, 0.95766, 0.05222, 1.06112,
        ],
        redundancy_penalty: 0.28,
        weights: entries.iter().map(|&(k, w)| (k.to_string(), w)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Capabilities, CheckRequest, EngineResult, ModelFormat, RunContext, Verdict};

    struct Stub {
        name: &'static str,
        caps: Capabilities,
    }

    impl Engine for Stub {
        fn name(&self) -> &str {
            self.name
        }
        fn capabilities(&self) -> Capabilities {
            self.caps.clone()
        }
        fn check(&self, _req: &CheckRequest, _ctx: &RunContext) -> anyhow::Result<EngineResult> {
            Ok(EngineResult {
                engine: self.name.into(),
                verdict: Verdict::Unknown {
                    reason: "stub".into(),
                },
                wall_ms: 0,
                witness: None,
                certificate: None,
            })
        }
    }

    fn stub(name: &'static str, kind: EngineKind, proves: bool, slots: u8) -> Arc<dyn Engine> {
        Arc::new(Stub {
            name,
            caps: Capabilities {
                formats: vec![ModelFormat::Aiger, ModelFormat::Btor2],
                proves,
                finds_bugs: true,
                kind,
                worker_slots: slots,
                multi_worker: slots > 1,
                ..Default::default()
            },
        })
    }

    #[test]
    fn builtin_model_round_trips_json_and_preserves_complementarity() {
        let m = builtin_model();
        let json = serde_json::to_string(&m).unwrap();
        let m2 = SelectorModel::from_json_str(&json).unwrap();
        assert_eq!(m2.version, 1);
        assert_eq!(m2.weights.len(), 18);

        let stub_with = |name: &'static str,
                         domain: EngineDomain,
                         kind: EngineKind,
                         proves: bool,
                         min_effort: Effort,
                         slots: u8|
         -> Arc<dyn Engine> {
            Arc::new(Stub {
                name,
                caps: Capabilities {
                    formats: vec![ModelFormat::Aiger, ModelFormat::Btor2],
                    proves,
                    finds_bugs: true,
                    domain,
                    kind,
                    min_effort,
                    worker_slots: slots,
                    multi_worker: slots > 1,
                    ..Default::default()
                },
            })
        };

        let pool = vec![
            stub_with(
                "informal-ic3-mt",
                EngineDomain::BitLevel,
                EngineKind::Ic3,
                true,
                Effort::Standard,
                4,
            ),
            stub_with(
                "informal-ic3",
                EngineDomain::BitLevel,
                EngineKind::Ic3,
                true,
                Effort::Low,
                1,
            ),
            stub_with(
                "informal-kind-ic3",
                EngineDomain::BitLevel,
                EngineKind::Hybrid,
                true,
                Effort::High,
                1,
            ),
            stub_with(
                "informal-gla",
                EngineDomain::BitLevel,
                EngineKind::Hybrid,
                true,
                Effort::Standard,
                1,
            ),
            stub_with(
                "informal-kind",
                EngineDomain::BitLevel,
                EngineKind::KInduction,
                true,
                Effort::Low,
                1,
            ),
            stub_with(
                "informal-bcar",
                EngineDomain::BitLevel,
                EngineKind::Car,
                true,
                Effort::Standard,
                1,
            ),
            stub_with(
                "informal-imc",
                EngineDomain::BitLevel,
                EngineKind::Interpolation,
                true,
                Effort::Standard,
                1,
            ),
            stub_with(
                "informal-bmc",
                EngineDomain::BitLevel,
                EngineKind::Bmc,
                false,
                Effort::Low,
                1,
            ),
            stub_with(
                "informal-bmc-10",
                EngineDomain::BitLevel,
                EngineKind::Bmc,
                false,
                Effort::Standard,
                1,
            ),
            stub_with(
                "informal-swarm",
                EngineDomain::BitLevel,
                EngineKind::Swarm,
                false,
                Effort::Standard,
                1,
            ),
        ];

        // Huge RISC-V-like cone: cap=6 must include a prover, BOTH step-1 and
        // jump-step BMC, and avoid letting 4-slot ic3-mt starve Wave 0.
        let huge = Features {
            nodes: 1_109_978,
            inputs: 14_000,
            states: 189_026,
            state_bits: 189_026,
            bad: 1,
            constraints: 18,
            max_width: 1,
            ..Default::default()
        };
        let cap = size_aware_slot_cap(12, &huge, Effort::Standard);
        assert_eq!(cap, 6);
        let (w0, w1) = m2.rank_and_partition(&pool, &huge, cap);
        let slots: usize = w0.iter().map(|e| e.capabilities().slots()).sum();
        assert!(slots <= cap, "Wave 0 used {slots} > cap {cap}");
        assert!(w0.iter().any(|e| e.capabilities().proves));
        assert!(w0.iter().any(|e| e.name() == "informal-bmc"));
        assert!(w0.iter().any(|e| e.name() == "informal-bmc-10"));
        assert!(w0.iter().any(|e| e.name() == "informal-gla"));
        assert!(!w1.is_empty(), "overflow engines must queue in Wave 1");

        // Wide BV Btor2 cone: word-level and bit-level engines coexist without
        // suppressing each other across domains.
        let mut bv_pool = pool;
        bv_pool.push(stub_with(
            "informal-wkind",
            EngineDomain::WordLevel,
            EngineKind::KInduction,
            true,
            Effort::Standard,
            1,
        ));
        bv_pool.push(stub_with(
            "informal-ic3sa",
            EngineDomain::WordLevel,
            EngineKind::Ic3,
            true,
            Effort::Standard,
            1,
        ));
        bv_pool.push(stub_with(
            "informal-wbmc",
            EngineDomain::WordLevel,
            EngineKind::Bmc,
            false,
            Effort::Low,
            1,
        ));
        let wide_bv = Features {
            nodes: 34,
            inputs: 3,
            states: 5,
            state_bits: 320,
            bad: 1,
            arith_ops: 8,
            max_width: 64,
            ..Default::default()
        };
        let (bv_w0, _) = m2.rank_and_partition(&bv_pool, &wide_bv, 10);
        assert!(bv_w0
            .iter()
            .any(|e| e.capabilities().domain == EngineDomain::WordLevel));
        assert!(bv_w0.iter().any(|e| e.name() == "informal-bmc-10"));
        let _ = stub;
    }

    #[test]
    fn size_aware_cap_scales_with_model_size_and_effort() {
        let small = Features {
            nodes: 3_000,
            states: 100,
            state_bits: 100,
            ..Default::default()
        };
        let huge = Features {
            nodes: 900_000,
            states: 180_000,
            state_bits: 180_000,
            ..Default::default()
        };
        assert_eq!(size_aware_slot_cap(12, &small, Effort::Standard), 10);
        assert_eq!(size_aware_slot_cap(12, &huge, Effort::Standard), 6);
        assert_eq!(size_aware_slot_cap(12, &huge, Effort::High), 8);
        assert_eq!(size_aware_slot_cap(0, &huge, Effort::Standard), 0);
    }
}
