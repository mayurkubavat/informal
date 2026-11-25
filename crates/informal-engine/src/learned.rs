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
    /// Guarantees Wave 0 includes at least one prover and (when available) one
    /// dedicated bug-finder before filling remaining slots using predicted
    /// utility minus `redundancy_penalty` per already-picked engine of the
    /// same [`EngineKind`].
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

        let mut picked_idx: Vec<usize> = Vec::with_capacity(candidates.len());
        let mut picked_set: HashSet<usize> = HashSet::with_capacity(candidates.len());
        let mut used_slots = 0usize;
        let mut kind_counts: HashMap<EngineKind, usize> = HashMap::new();

        let fits = |idx: usize, used: usize| -> bool {
            let need = candidates[idx].capabilities().slots();
            used == 0 || wave0_slot_cap == 0 || used + need <= wave0_slot_cap
        };

        let pick = |idx: usize,
                    picked_idx: &mut Vec<usize>,
                    picked_set: &mut HashSet<usize>,
                    used_slots: &mut usize,
                    kind_counts: &mut HashMap<EngineKind, usize>| {
            let caps = candidates[idx].capabilities();
            picked_idx.push(idx);
            picked_set.insert(idx);
            *used_slots += caps.slots();
            *kind_counts.entry(caps.kind).or_insert(0) += 1;
        };

        // 1. Best prover that fits.
        if let Some(best_p) = (0..candidates.len())
            .filter(|&i| candidates[i].capabilities().proves && fits(i, used_slots))
            .max_by(|&a, &b| scores[a].total_cmp(&scores[b]).then_with(|| b.cmp(&a)))
        {
            pick(
                best_p,
                &mut picked_idx,
                &mut picked_set,
                &mut used_slots,
                &mut kind_counts,
            );
        }

        // 2. Best dedicated bug-finder (BMC/Swarm) that fits.
        if let Some(best_b) = (0..candidates.len())
            .filter(|&i| {
                let c = candidates[i].capabilities();
                !picked_set.contains(&i) && c.finds_bugs && !c.proves && fits(i, used_slots)
            })
            .max_by(|&a, &b| scores[a].total_cmp(&scores[b]).then_with(|| b.cmp(&a)))
        {
            pick(
                best_b,
                &mut picked_idx,
                &mut picked_set,
                &mut used_slots,
                &mut kind_counts,
            );
        }

        // 3. Greedily fill remaining Wave-0 slots with diversity penalty.
        loop {
            let next = (0..candidates.len())
                .filter(|&i| !picked_set.contains(&i) && fits(i, used_slots))
                .max_by(|&a, &b| {
                    let pa = scores[a]
                        - self.redundancy_penalty
                            * (*kind_counts
                                .get(&candidates[a].capabilities().kind)
                                .unwrap_or(&0) as f64);
                    let pb = scores[b]
                        - self.redundancy_penalty
                            * (*kind_counts
                                .get(&candidates[b].capabilities().kind)
                                .unwrap_or(&0) as f64);
                    pa.total_cmp(&pb).then_with(|| b.cmp(&a))
                });
            match next {
                Some(i) => pick(
                    i,
                    &mut picked_idx,
                    &mut picked_set,
                    &mut used_slots,
                    &mut kind_counts,
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
            let pa = scores[a]
                - 0.5
                    * self.redundancy_penalty
                    * (*kind_counts
                        .get(&candidates[a].capabilities().kind)
                        .unwrap_or(&0) as f64);
            let pb = scores[b]
                - 0.5
                    * self.redundancy_penalty
                    * (*kind_counts
                        .get(&candidates[b].capabilities().kind)
                        .unwrap_or(&0) as f64);
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

/// Built-in default Ridge model (`lam = 50.0`, 5-fold CV `34/60`, OOF PAR-2
/// `585.6 s` vs `584.5 s` 15-engine VBS on HWMCC'24 `sample60`).
pub fn builtin_model() -> SelectorModel {
    let entries: &[(&str, [f64; NUM_FEATURES])] = &[
        (
            "informal-bcar",
            [
                0.18168, -0.01068, -0.01068, -0.02177, -0.01336, 0.02651, -0.00850, 0.04187, 0.0,
                0.0,
            ],
        ),
        (
            "informal-bdd",
            [
                0.20727, 0.00235, 0.00235, -0.01272, 0.00650, 0.0, -0.01927, -0.00939, 0.0, 0.0,
            ],
        ),
        (
            "informal-bmc",
            [
                0.15329, -0.01016, -0.01016, -0.00309, 0.00255, -0.00432, 0.01135, 0.02815, 0.0,
                0.0,
            ],
        ),
        (
            "informal-bmc-10",
            [
                0.12391, -0.01096, -0.01096, -0.00735, -0.00788, 0.00742, 0.00800, 0.02459, 0.0,
                0.0,
            ],
        ),
        (
            "informal-bmc-65",
            [
                0.08133, -0.00586, -0.00586, -0.01413, -0.01554, 0.00871, -0.00879, -0.00712, 0.0,
                0.0,
            ],
        ),
        (
            "informal-car",
            [
                0.16200, -0.01518, -0.01518, -0.03000, -0.02448, 0.01802, -0.01134, 0.04325, 0.0,
                0.0,
            ],
        ),
        (
            "informal-gla",
            [
                0.22145, -0.01819, -0.01819, -0.03637, -0.01790, 0.04329, -0.01472, 0.03967, 0.0,
                0.0,
            ],
        ),
        (
            "informal-ic3",
            [
                0.23710, -0.00767, -0.00767, -0.04626, -0.01682, 0.03532, -0.04308, -0.00129, 0.0,
                0.0,
            ],
        ),
        (
            "informal-ic3-cadical",
            [
                0.23148, -0.01502, -0.01502, -0.03582, -0.02245, 0.04504, -0.01815, 0.03378, 0.0,
                0.0,
            ],
        ),
        (
            "informal-ic3-inn",
            [
                0.17721, -0.01478, -0.01478, -0.02884, -0.02234, 0.03366, -0.01077, 0.04077, 0.0,
                0.0,
            ],
        ),
        (
            "informal-ic3-mt",
            [
                0.25824, -0.01342, -0.01342, -0.03954, -0.01847, 0.04243, -0.02464, 0.03130, 0.0,
                0.0,
            ],
        ),
        (
            "informal-imc",
            [
                0.19422, -0.01323, -0.01323, -0.02799, -0.00417, 0.03021, -0.01291, 0.04800, 0.0,
                0.0,
            ],
        ),
        (
            "informal-kind",
            [
                0.24119, -0.02735, -0.02735, -0.05652, -0.03123, 0.02035, -0.02497, 0.02716, 0.0,
                0.0,
            ],
        ),
        (
            "informal-kind-ic3",
            [
                0.26756, -0.02431, -0.02431, -0.05573, -0.02421, 0.03463, -0.02834, 0.02831, 0.0,
                0.0,
            ],
        ),
        (
            "informal-swarm",
            [
                0.07046, -0.00835, -0.00835, -0.00472, -0.01468, 0.01472, 0.00870, 0.03136, 0.0,
                0.0,
            ],
        ),
    ];
    SelectorModel {
        version: 1,
        means: [
            0.0,
            8.33390,
            8.33390,
            11.50043,
            7.30604,
            1.54986,
            3.25878,
            0.79856,
            0.0,
            std::f64::consts::LN_2,
        ],
        stds: [
            1.0, 2.96139, 2.96139, 2.34146, 2.30924, 1.56772, 1.91640, 1.23075, 1.0, 1.0,
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
        assert_eq!(m2.weights.len(), 15);

        let pool = vec![
            stub("informal-ic3-mt", EngineKind::Ic3, true, 4),
            stub("informal-ic3", EngineKind::Ic3, true, 1),
            stub("informal-kind-ic3", EngineKind::Hybrid, true, 1),
            stub("informal-kind", EngineKind::KInduction, true, 1),
            stub("informal-bcar", EngineKind::Car, true, 1),
            stub("informal-imc", EngineKind::Interpolation, true, 1),
            stub("informal-bmc", EngineKind::Bmc, false, 1),
            stub("informal-swarm", EngineKind::Swarm, false, 1),
        ];

        // Huge RISC-V-like cone: cap=6 must include both a prover and BMC without
        // exceeding 6 slots.
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
        assert!(w0
            .iter()
            .any(|e| e.capabilities().finds_bugs && !e.capabilities().proves));
        assert!(!w1.is_empty(), "overflow engines must queue in Wave 1");
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
