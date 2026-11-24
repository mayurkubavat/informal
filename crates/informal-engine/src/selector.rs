//! Declarative, capability-driven portfolio routing over Btor2 model features.
//!
//! Engines declare their domain, algorithmic kind, array/liveness support,
//! effort floor, state-bit ceiling, and multi-worker / deep-step properties via
//! [`crate::Capabilities`]. Routing decisions inspect only those capabilities
//! and [`informal_ir::Features`] — never hardcoded engine names — so new engines
//! participate automatically when registered.

use crate::{Effort, Engine, EngineDomain, EngineKind};
use informal_ir::Features;
use std::sync::Arc;

/// Select the eligible subset of `engines` for `btor_text` at [`Effort::Standard`].
pub fn select_for_btor(
    engines: &[Arc<dyn Engine>],
    btor_text: &str,
    explicit_filter: bool,
) -> Vec<Arc<dyn Engine>> {
    select_for_btor_with_effort(engines, btor_text, explicit_filter, Effort::Standard)
}

/// Select the eligible subset of `engines` for `btor_text` at `effort`.
pub fn select_for_btor_with_effort(
    engines: &[Arc<dyn Engine>],
    btor_text: &str,
    explicit_filter: bool,
    effort: Effort,
) -> Vec<Arc<dyn Engine>> {
    let Ok(m) = informal_ir::parse(btor_text) else {
        return engines.to_vec();
    };
    let f = Features::of(&m);
    select_for_features(engines, &f, explicit_filter, effort)
}

/// Filter `pool` by `pred`, falling back to `pool` unchanged when no engine
/// satisfies `pred`.
fn keep_or_fallback(
    pool: Vec<Arc<dyn Engine>>,
    pred: impl Fn(&Arc<dyn Engine>) -> bool,
) -> Vec<Arc<dyn Engine>> {
    let next: Vec<_> = pool.iter().filter(|e| pred(e)).cloned().collect();
    if next.is_empty() {
        pool
    } else {
        next
    }
}

/// Select the eligible subset of `engines` from extracted structural [`Features`].
///
/// Routing policy:
/// - **Liveness (`justice > 0`)**: keeps liveness-capable engines (`caps.liveness`);
///   under auto-routing applies `max_state_bits` and `min_effort <= effort`.
/// - **Safety (`justice == 0`)**: drops liveness-only engines (`kind == Liveness`).
///   When `explicit_filter` is set, returns that set verbatim.
/// - **Arrays (`array_states > 0 || has_arrays`)**: routes to array-capable engines
///   (`caps.arrays`), applying `min_effort <= effort` with non-empty fallback.
/// - **State-bit cap & effort floor**: drops engines with `max_state_bits < state_bits`
///   or `min_effort > effort` (each with non-empty fallback).
/// - **Combinational (`states == 0`)**: keeps lightweight 1-step BMC, k-induction,
///   and generic `Other` engines, avoiding multi-worker IC3 spin-up.
/// - **Sequential shedding**: when a multi-worker bit-level IC3 engine is active,
///   sheds its redundant low-effort single-worker bit-level IC3 peer; sheds
///   `deep_step` BMC on tiny (`state_bits <= 16`) or arithmetic-heavy cones.
pub fn select_for_features(
    engines: &[Arc<dyn Engine>],
    f: &Features,
    explicit_filter: bool,
    effort: Effort,
) -> Vec<Arc<dyn Engine>> {
    if f.justice > 0 {
        let live: Vec<_> = engines
            .iter()
            .filter(|e| e.capabilities().liveness)
            .cloned()
            .collect();
        if live.is_empty() {
            return engines.to_vec();
        }
        if explicit_filter {
            return live;
        }
        let capped = keep_or_fallback(live, |e| {
            e.capabilities()
                .max_state_bits
                .is_none_or(|max| f.state_bits <= max)
        });
        return keep_or_fallback(capped, |e| e.capabilities().min_effort <= effort);
    }

    let base: Vec<_> = engines
        .iter()
        .filter(|e| e.capabilities().kind != EngineKind::Liveness)
        .cloned()
        .collect();
    if explicit_filter {
        return base;
    }

    if f.array_states > 0 || f.has_arrays {
        let arr: Vec<_> = base
            .iter()
            .filter(|e| e.capabilities().arrays)
            .cloned()
            .collect();
        if !arr.is_empty() {
            let capped = keep_or_fallback(arr, |e| {
                e.capabilities()
                    .max_state_bits
                    .is_none_or(|max| f.state_bits <= max)
            });
            return keep_or_fallback(capped, |e| e.capabilities().min_effort <= effort);
        }
    }

    let cur = keep_or_fallback(base, |e| {
        e.capabilities()
            .max_state_bits
            .is_none_or(|max| f.state_bits <= max)
    });
    let cur = keep_or_fallback(cur, |e| e.capabilities().min_effort <= effort);

    if effort == Effort::Low {
        return cur;
    }

    if f.states == 0 {
        return keep_or_fallback(cur, |e| {
            let c = e.capabilities();
            (c.kind == EngineKind::Bmc && !c.deep_step && c.min_effort <= Effort::Low)
                || c.kind == EngineKind::KInduction
                || c.kind == EngineKind::Other
        });
    }

    let has_multi_bit_ic3 = cur.iter().any(|e| {
        let c = e.capabilities();
        c.domain == EngineDomain::BitLevel && c.kind == EngineKind::Ic3 && c.multi_worker
    });
    let arith_heavy = f.arith_ops >= 8 && f.max_width >= 16;
    let tiny_state = f.state_bits <= 16;

    keep_or_fallback(cur, |e| {
        let c = e.capabilities();
        if has_multi_bit_ic3
            && c.domain == EngineDomain::BitLevel
            && c.kind == EngineKind::Ic3
            && !c.multi_worker
            && c.min_effort <= Effort::Low
        {
            return false;
        }
        if c.deep_step && (tiny_state || arith_heavy) {
            return false;
        }
        true
    })
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

    fn stub(name: &'static str, caps: Capabilities) -> Arc<dyn Engine> {
        Arc::new(Stub { name, caps })
    }

    fn fleet() -> Vec<Arc<dyn Engine>> {
        let bit_formats = vec![ModelFormat::Aiger, ModelFormat::Btor2];
        vec![
            stub(
                "bmc-1",
                Capabilities {
                    formats: bit_formats.clone(),
                    finds_bugs: true,
                    liveness: true,
                    domain: EngineDomain::BitLevel,
                    kind: EngineKind::Bmc,
                    min_effort: Effort::Low,
                    certifies: true,
                    ..Default::default()
                },
            ),
            stub(
                "bmc-65",
                Capabilities {
                    formats: bit_formats.clone(),
                    finds_bugs: true,
                    liveness: true,
                    domain: EngineDomain::BitLevel,
                    kind: EngineKind::Bmc,
                    min_effort: Effort::Standard,
                    certifies: true,
                    deep_step: true,
                    ..Default::default()
                },
            ),
            stub(
                "kind",
                Capabilities {
                    formats: bit_formats.clone(),
                    proves: true,
                    finds_bugs: true,
                    liveness: true,
                    domain: EngineDomain::BitLevel,
                    kind: EngineKind::KInduction,
                    min_effort: Effort::Low,
                    certifies: true,
                    ..Default::default()
                },
            ),
            stub(
                "ic3-single",
                Capabilities {
                    formats: bit_formats.clone(),
                    proves: true,
                    finds_bugs: true,
                    liveness: true,
                    domain: EngineDomain::BitLevel,
                    kind: EngineKind::Ic3,
                    min_effort: Effort::Low,
                    certifies: true,
                    ..Default::default()
                },
            ),
            stub(
                "ic3-mt",
                Capabilities {
                    formats: bit_formats.clone(),
                    proves: true,
                    finds_bugs: true,
                    liveness: true,
                    domain: EngineDomain::BitLevel,
                    kind: EngineKind::Ic3,
                    min_effort: Effort::Standard,
                    certifies: true,
                    multi_worker: true,
                    ..Default::default()
                },
            ),
            stub(
                "klive",
                Capabilities {
                    formats: bit_formats.clone(),
                    proves: true,
                    finds_bugs: true,
                    liveness: true,
                    domain: EngineDomain::BitLevel,
                    kind: EngineKind::Liveness,
                    min_effort: Effort::Low,
                    certifies: true,
                    ..Default::default()
                },
            ),
            stub(
                "wbmc",
                Capabilities {
                    formats: vec![ModelFormat::Btor2],
                    finds_bugs: true,
                    domain: EngineDomain::WordLevel,
                    kind: EngineKind::Bmc,
                    arrays: true,
                    min_effort: Effort::Low,
                    ..Default::default()
                },
            ),
            stub(
                "wkind",
                Capabilities {
                    formats: vec![ModelFormat::Btor2],
                    proves: true,
                    finds_bugs: true,
                    domain: EngineDomain::WordLevel,
                    kind: EngineKind::KInduction,
                    arrays: true,
                    min_effort: Effort::Standard,
                    ..Default::default()
                },
            ),
            stub(
                "bdd",
                Capabilities {
                    formats: bit_formats,
                    proves: true,
                    finds_bugs: true,
                    domain: EngineDomain::BitLevel,
                    kind: EngineKind::Bdd,
                    min_effort: Effort::Standard,
                    max_state_bits: Some(200),
                    certifies: true,
                    ..Default::default()
                },
            ),
            stub(
                "heavy-signoff",
                Capabilities {
                    formats: vec![ModelFormat::Btor2],
                    proves: true,
                    domain: EngineDomain::WordLevel,
                    kind: EngineKind::Ic3,
                    min_effort: Effort::High,
                    ..Default::default()
                },
            ),
        ]
    }

    fn names(es: &[Arc<dyn Engine>]) -> Vec<String> {
        es.iter().map(|e| e.name().to_string()).collect()
    }

    #[test]
    fn routes_liveness_arrays_effort_state_caps_and_shedding_declaratively() {
        let f_all = fleet();
        let has = |list: &[String], name: &str| list.iter().any(|n| n == name);

        // 1. Liveness at Standard keeps all liveness-capable engines (drops wbmc/wkind/bdd/heavy).
        let live_f = Features {
            justice: 1,
            states: 2,
            state_bits: 4,
            ..Default::default()
        };
        let live_std = names(&select_for_features(
            &f_all,
            &live_f,
            false,
            Effort::Standard,
        ));
        assert!(has(&live_std, "klive"));
        assert!(has(&live_std, "ic3-mt"));
        assert!(!has(&live_std, "wbmc"));
        assert!(!has(&live_std, "bdd"));

        // Liveness at Low keeps only Low-effort liveness engines.
        let live_low = names(&select_for_features(&f_all, &live_f, false, Effort::Low));
        assert_eq!(live_low, vec!["bmc-1", "kind", "ic3-single", "klive"]);

        // 2. Arrays route exclusively to array-capable engines.
        let arr_f = Features {
            has_arrays: true,
            array_states: 1,
            states: 1,
            ..Default::default()
        };
        assert_eq!(
            names(&select_for_features(
                &f_all,
                &arr_f,
                false,
                Effort::Standard
            )),
            vec!["wbmc", "wkind"]
        );
        assert_eq!(
            names(&select_for_features(&f_all, &arr_f, false, Effort::Low)),
            vec!["wbmc"]
        );

        // 3. Effort::Low safety sweep drops heavy / Standard+ engines, keeps Low safety ones.
        let seq_big = Features {
            states: 10,
            state_bits: 300,
            ..Default::default()
        };
        assert_eq!(
            names(&select_for_features(&f_all, &seq_big, false, Effort::Low)),
            vec!["bmc-1", "kind", "ic3-single", "wbmc"]
        );

        // 4. max_state_bits drops bdd when state_bits > 200, keeps bdd when state_bits <= 200.
        let std_big = names(&select_for_features(
            &f_all,
            &seq_big,
            false,
            Effort::Standard,
        ));
        assert!(!has(&std_big, "bdd"));
        assert!(!has(&std_big, "heavy-signoff"));
        assert!(has(&std_big, "bmc-65"));
        assert!(has(&std_big, "ic3-mt"));
        assert!(!has(&std_big, "ic3-single"));

        let seq_med = Features {
            states: 4,
            state_bits: 64,
            ..Default::default()
        };
        let high_med = names(&select_for_features(&f_all, &seq_med, false, Effort::High));
        assert!(has(&high_med, "bdd"));
        assert!(has(&high_med, "heavy-signoff"));

        // 5. Tiny state or arith-heavy sheds deep_step BMC and redundant single-worker IC3.
        let tiny_f = Features {
            states: 2,
            state_bits: 8,
            ..Default::default()
        };
        let std_tiny = names(&select_for_features(
            &f_all,
            &tiny_f,
            false,
            Effort::Standard,
        ));
        assert!(!has(&std_tiny, "bmc-65"));
        assert!(!has(&std_tiny, "ic3-single"));
        assert!(has(&std_tiny, "ic3-mt"));
        assert!(has(&std_tiny, "bdd"));

        // 6. Purely combinational keeps lightweight BMC + k-induction, skips IC3/BDD/deep-step.
        let comb_f = Features {
            states: 0,
            state_bits: 0,
            ..Default::default()
        };
        assert_eq!(
            names(&select_for_features(
                &f_all,
                &comb_f,
                false,
                Effort::Standard
            )),
            vec!["bmc-1", "kind", "wbmc", "wkind"]
        );

        // 7. Explicit filter keeps every non-liveness-only engine regardless of caps/effort.
        let exp = names(&select_for_features(&f_all, &seq_big, true, Effort::Low));
        assert!(has(&exp, "bdd"));
        assert!(has(&exp, "heavy-signoff"));
        assert!(!has(&exp, "klive"));
    }
}
