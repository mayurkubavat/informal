//! Declarative, capability-driven portfolio routing over Btor2 and AIGER model features.
//!
//! Engines declare their domain, algorithmic kind, array/liveness support,
//! effort floor, state-bit ceiling, and multi-worker / deep-step properties via
//! [`crate::Capabilities`]. Routing decisions inspect only those capabilities
//! and [`informal_ir::Features`] — never hardcoded engine names — so new engines
//! participate automatically when registered.

pub use crate::learned::PortfolioPlan;
use crate::learned::{self, base_priority, wave1_delay};
use crate::portfolio::{self, ScheduleConfig, ScheduledEngine};
use crate::{Effort, Engine, EngineDomain, EngineKind, ModelFormat};
use informal_ir::Features;
use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

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

/// Extract structural [`Features`] from the header line of an AIGER file
/// (`aag`/`aig M I L O A [B C J F]`). Every latch is 1 state bit; outputs count
/// as bad states when no AIGER 1.9 bad section is present.
pub fn aiger_features(bytes: &[u8]) -> Option<Features> {
    let end = bytes
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(bytes.len());
    let head = std::str::from_utf8(&bytes[..end])
        .ok()?
        .trim_end_matches('\r');
    let mut it = head.split_whitespace();
    if !matches!(it.next()?, "aag" | "aig") {
        return None;
    }
    let n: Vec<usize> = it.map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let at = |i: usize| n.get(i).copied().unwrap_or(0);
    let (inputs, latches, outputs, ands) = (at(1), at(2), at(3), at(4));
    let bad = if at(5) > 0 { at(5) } else { outputs };
    Some(Features {
        nodes: inputs + latches + ands,
        inputs,
        states: latches,
        state_bits: latches as u64,
        bad,
        constraints: at(6),
        justice: at(7),
        fair: at(8),
        max_width: 1,
        ..Default::default()
    })
}

/// Select the eligible subset of `engines` for AIGER `bytes` (header prefix is
/// sufficient) at `effort`.
pub fn select_for_aiger_bytes(
    engines: &[Arc<dyn Engine>],
    bytes: &[u8],
    explicit_filter: bool,
    effort: Effort,
) -> Vec<Arc<dyn Engine>> {
    let Some(f) = aiger_features(bytes) else {
        return engines.to_vec();
    };
    select_for_features(engines, &f, explicit_filter, effort)
}

/// Format-aware structural routing for any Btor2 or AIGER model file on disk.
///
/// Filters to engines supporting `format`, extracts features (reading only the
/// header line for AIGER models), and applies [`select_for_features`].
pub fn select_for_model(
    engines: &[Arc<dyn Engine>],
    path: &Path,
    format: ModelFormat,
    explicit_filter: bool,
    effort: Effort,
) -> Vec<Arc<dyn Engine>> {
    let fmt_ok: Vec<_> = engines
        .iter()
        .filter(|e| e.capabilities().formats.contains(&format))
        .cloned()
        .collect();
    if fmt_ok.is_empty() {
        return Vec::new();
    }
    match format {
        ModelFormat::Btor2 => match std::fs::read_to_string(path) {
            Ok(text) => select_for_btor_with_effort(&fmt_ok, &text, explicit_filter, effort),
            Err(_) => fmt_ok,
        },
        ModelFormat::Aiger => {
            let mut buf = [0u8; 512];
            let Ok(mut f) = std::fs::File::open(path) else {
                return fmt_ok;
            };
            let Ok(n) = f.read(&mut buf) else {
                return fmt_ok;
            };
            select_for_aiger_bytes(&fmt_ok, &buf[..n], explicit_filter, effort)
        }
    }
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

/// Build a complete staged-wave [`PortfolioPlan`] from structural [`Features`].
///
/// - Under `explicit_filter`, returns [`select_for_features`] verbatim in Wave 0
///   with [`ScheduleConfig::unlimited`].
/// - Under `Effort::Low` or liveness/array/combinational cones, preserves hard
///   capability routing in Wave 0 ordered by fallback priority.
/// - For sequential safety models at `Standard+`, delegates to the Phase 3
///   learned Ridge ranker (`INFORMAL_SELECTOR=rules` falls back to static rules).
pub fn plan_for_features(
    engines: &[Arc<dyn Engine>],
    f: &Features,
    explicit_filter: bool,
    effort: Effort,
    timeout: Duration,
) -> PortfolioPlan {
    if explicit_filter {
        let chosen = select_for_features(engines, f, true, effort);
        return PortfolioPlan {
            schedule: chosen.into_iter().map(ScheduledEngine::immediate).collect(),
            config: ScheduleConfig::unlimited(),
        };
    }
    let host_cfg = ScheduleConfig::default_for_host();
    if effort == Effort::Low || f.justice > 0 || f.array_states > 0 || f.has_arrays || f.states == 0
    {
        let mut wave0 = select_for_features(engines, f, false, effort);
        wave0.sort_by_key(|e| base_priority(e, f));
        return PortfolioPlan {
            schedule: wave0.into_iter().map(ScheduledEngine::immediate).collect(),
            config: host_cfg,
        };
    }

    let cap = learned::size_aware_slot_cap(host_cfg.max_slots, f, effort);
    if learned::enabled() {
        return learned::plan_sequential_safety(engines, f, effort, timeout, cap);
    }

    let config = ScheduleConfig { max_slots: cap };
    let mut wave0 = select_for_features(engines, f, false, effort);
    if !portfolio::staged_enabled() {
        return PortfolioPlan {
            schedule: wave0.into_iter().map(ScheduledEngine::immediate).collect(),
            config,
        };
    }
    wave0.sort_by_key(|e| base_priority(e, f));
    let mut seen: HashSet<String> = wave0.iter().map(|e| e.name().to_string()).collect();
    let mut schedule: Vec<ScheduledEngine> =
        wave0.into_iter().map(ScheduledEngine::immediate).collect();
    if effort == Effort::Standard {
        let mut reserves: Vec<Arc<dyn Engine>> =
            select_for_features(engines, f, false, Effort::High)
                .into_iter()
                .filter(|e| !seen.contains(e.name()))
                .collect();
        reserves.sort_by_key(|e| base_priority(e, f));
        let delay = wave1_delay(timeout);
        for e in reserves {
            if seen.insert(e.name().to_string()) {
                schedule.push(ScheduledEngine::delayed(e, delay));
            }
        }
    }
    PortfolioPlan { schedule, config }
}

/// Build a staged-wave portfolio schedule from structural [`Features`].
pub fn schedule_for_features(
    engines: &[Arc<dyn Engine>],
    f: &Features,
    explicit_filter: bool,
    effort: Effort,
    timeout: Duration,
) -> Vec<ScheduledEngine> {
    plan_for_features(engines, f, explicit_filter, effort, timeout).schedule
}

/// Complete staged-wave [`PortfolioPlan`] for a Btor2 model text.
pub fn plan_for_btor_with_effort(
    engines: &[Arc<dyn Engine>],
    btor_text: &str,
    explicit_filter: bool,
    effort: Effort,
    timeout: Duration,
) -> PortfolioPlan {
    let Ok(m) = informal_ir::parse(btor_text) else {
        return PortfolioPlan {
            schedule: engines
                .iter()
                .cloned()
                .map(ScheduledEngine::immediate)
                .collect(),
            config: if explicit_filter {
                ScheduleConfig::unlimited()
            } else {
                ScheduleConfig::default_for_host()
            },
        };
    };
    let f = Features::of(&m);
    plan_for_features(engines, &f, explicit_filter, effort, timeout)
}

/// Complete staged-wave [`PortfolioPlan`] for AIGER header `bytes`.
pub fn plan_for_aiger_bytes(
    engines: &[Arc<dyn Engine>],
    bytes: &[u8],
    explicit_filter: bool,
    effort: Effort,
    timeout: Duration,
) -> PortfolioPlan {
    let Some(f) = aiger_features(bytes) else {
        return PortfolioPlan {
            schedule: engines
                .iter()
                .cloned()
                .map(ScheduledEngine::immediate)
                .collect(),
            config: if explicit_filter {
                ScheduleConfig::unlimited()
            } else {
                ScheduleConfig::default_for_host()
            },
        };
    };
    plan_for_features(engines, &f, explicit_filter, effort, timeout)
}

/// Format-aware staged-wave [`PortfolioPlan`] for any Btor2 or AIGER model file.
pub fn plan_for_model(
    engines: &[Arc<dyn Engine>],
    path: &Path,
    format: ModelFormat,
    explicit_filter: bool,
    effort: Effort,
    timeout: Duration,
) -> PortfolioPlan {
    let fmt_ok: Vec<_> = engines
        .iter()
        .filter(|e| e.capabilities().formats.contains(&format))
        .cloned()
        .collect();
    let fallback = |pool: Vec<Arc<dyn Engine>>| PortfolioPlan {
        schedule: pool.into_iter().map(ScheduledEngine::immediate).collect(),
        config: if explicit_filter {
            ScheduleConfig::unlimited()
        } else {
            ScheduleConfig::default_for_host()
        },
    };
    if fmt_ok.is_empty() {
        return fallback(Vec::new());
    }
    match format {
        ModelFormat::Btor2 => match std::fs::read_to_string(path) {
            Ok(text) => plan_for_btor_with_effort(&fmt_ok, &text, explicit_filter, effort, timeout),
            Err(_) => fallback(fmt_ok),
        },
        ModelFormat::Aiger => {
            let mut buf = [0u8; 512];
            let Ok(mut f) = std::fs::File::open(path) else {
                return fallback(fmt_ok);
            };
            let Ok(n) = f.read(&mut buf) else {
                return fallback(fmt_ok);
            };
            plan_for_aiger_bytes(&fmt_ok, &buf[..n], explicit_filter, effort, timeout)
        }
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

    #[test]
    fn aiger_header_routes_safety_size_and_liveness() {
        let f_all = fleet();
        let has = |list: &[String], name: &str| list.iter().any(|n| n == name);

        // Large AIGER 1.9 safety model (500 latches > BDD cap 200).
        let big_aig = b"aig 1200 100 500 0 600 1 2\n";
        let feat = aiger_features(big_aig).unwrap();
        assert_eq!(feat.states, 500);
        assert_eq!(feat.state_bits, 500);
        assert_eq!(feat.bad, 1);
        assert_eq!(feat.constraints, 2);
        assert_eq!(feat.justice, 0);
        let sel_big = names(&select_for_aiger_bytes(
            &f_all,
            big_aig,
            false,
            Effort::Standard,
        ));
        assert!(!has(&sel_big, "bdd"));
        assert!(!has(&sel_big, "klive"));
        assert!(has(&sel_big, "ic3-mt"));
        assert!(!has(&sel_big, "ic3-single"));

        // Small AIGER 1.0 safety model (40 latches <= 200): keeps BDD.
        let small_aig = b"aag 90 10 40 1 40\n";
        let sel_small = names(&select_for_aiger_bytes(
            &f_all,
            small_aig,
            false,
            Effort::Standard,
        ));
        assert!(has(&sel_small, "bdd"));
        assert!(has(&sel_small, "ic3-mt"));

        // AIGER 1.9 liveness model (justice = 1): routes to liveness-capable engines.
        let live_aig = b"aig 50 5 12 0 33 0 0 1 1\n";
        let sel_live = names(&select_for_aiger_bytes(
            &f_all,
            live_aig,
            false,
            Effort::Standard,
        ));
        assert!(has(&sel_live, "klive"));
        assert!(!has(&sel_live, "bdd"));
    }
}
