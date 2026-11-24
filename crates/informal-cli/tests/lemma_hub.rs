//! Integration tests for Phase 2 — Cross-Engine Cooperative Proof Sharing (`LemmaHub`).
//!
//! Verifies:
//! 1. Engine `Capabilities::{lemma_export, lemma_import}` metadata accuracy.
//! 2. Cross-engine handoff (IC3/BDD -> k-induction closing a non-k-inductive
//!    counter at `k <= 2`, plus live concurrent portfolio sharing).
//! 3. Adversarial zero-trust soundness (poisoned false standing & frame clauses
//!    rejected by every importing engine with zero false `Safe` verdicts).
//! 4. Multi-property shared-cone acceleration and design/constraint isolation
//!    (both direct `LemmaHub` and end-to-end SystemVerilog `Session::prove`).

use informal_bdd::BddEngine;
use informal_bmc::{Ic3Engine, Ic3Mt, KInduction, KindIc3};
use informal_car::CarEngine;
use informal_engine::{
    portfolio, Certificate, CheckRequest, Effort, Engine, LemmaHub, LemmaScope, ModelFormat,
    RunContext, SharedLemma, Verdict,
};
use informal_fpv::{Orchestration, Session, Status};
use informal_ic3::{GlaEngine, Ic3InnEngine};
use informal_imc::ImcEngine;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let id = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "informal-lemmahub-{tag}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 8-bit counter starting at 0, wrapping at `wrap_at` (`cnt == wrap_at ? 0 : cnt + 1`),
/// with `bad = (cnt == target)`.
///
/// When `wrap_at < target`, bare k-induction needs `k >= target - wrap_at` to rule out
/// unreachable simple paths `wrap_at+1 -> ... -> target`.
fn wrapping_counter_8(wrap_at: u32, target: u32) -> String {
    format!(
        "\
1 sort bitvec 8
2 sort bitvec 1
3 zero 1
4 one 1
5 constd 1 {wrap_at}
6 state 1 cnt
7 init 1 6 3
8 eq 2 6 5
9 add 1 6 4
10 ite 1 8 3 9
11 next 1 6 10
12 constd 1 {target}
13 eq 2 6 12
14 bad 13
"
    )
}

/// 4-bit input-enabled counter (`en ? cnt + 1 : cnt`) hitting `target` at
/// minimum depth `target`. Because `en` gates the LSB, `cnt[0]` is not a
/// free-running clock, so all 4 state bits remain live in every engine.
fn buggy_counter_4(target: u32) -> String {
    format!(
        "\
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
11 constd 1 {target}
12 eq 2 5 11
13 bad 12
"
    )
}

#[test]
fn capabilities_advertise_lemma_sharing_accurately() {
    if informal_sat::Cadical::signature().is_none() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    let mut fleet: Vec<Arc<dyn Engine>> = Vec::new();
    fleet.extend(informal_bmc::engines());
    fleet.extend(informal_car::engines());
    fleet.extend(informal_ic3::engines());
    fleet.extend(informal_imc::engines());
    fleet.extend(informal_bdd::engines());
    fleet.extend(informal_swarm::engines());
    fleet.extend(informal_liveness::engines());

    let expected = |name: &str| -> (bool, bool) {
        match name {
            "informal-ic3"
            | "informal-ic3-cadical"
            | "informal-ic3-mt"
            | "informal-kind-ic3"
            | "informal-car"
            | "informal-gla"
            | "informal-ic3-inn"
            | "informal-imc" => (true, true),
            "informal-kind" => (false, true),
            "informal-bcar" | "informal-bdd" => (true, false),
            _ => (false, false),
        }
    };

    for e in &fleet {
        let caps = e.capabilities();
        let (want_exp, want_imp) = expected(e.name());
        assert_eq!(
            (caps.lemma_export, caps.lemma_import),
            (want_exp, want_imp),
            "capability mismatch for {}",
            e.name()
        );
    }
}

#[test]
fn cross_engine_handoff_accelerates_kinduction_and_portfolio() {
    if informal_sat::Cadical::signature().is_none() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    let dir = TempDir::new("handoff");
    let model = dir.write("wrap200.btor2", &wrapping_counter_8(200, 250));
    let req = CheckRequest::new(model, ModelFormat::Btor2, Duration::from_secs(15));

    // 1. Bare k-induction capped at k <= 2 CANNOT close this non-k-inductive counter
    //    because 248 -> 249 -> 250 is a valid 3-state unreachable simple path.
    let kind_cap2 = KInduction { max_depth: Some(2) };
    let bare = kind_cap2
        .check(&req, &RunContext::new(req.timeout))
        .unwrap();
    assert!(
        matches!(bare.verdict, Verdict::Unknown { .. }),
        "expected bare k-induction(k<=2) to fail, got {:?}",
        bare.verdict
    );

    // 2. Run `informal-bdd` into a shared LemmaHub to publish its certified standing invariant.
    let hub = Arc::new(LemmaHub::new());
    let bdd_ctx = RunContext::new(req.timeout).with_lemma_hub(hub.clone());
    let bdd_res = BddEngine::default().check(&req, &bdd_ctx).unwrap();
    assert_eq!(bdd_res.verdict, Verdict::Safe);
    assert!(
        hub.stats().published_standing > 0,
        "expected BDD certified proof to populate LemmaHub: {:?}",
        hub.stats()
    );

    // 3. Now k-induction(k <= 2) with the SAME LemmaHub imports the standing invariant,
    //    validates it via Houdini, and immediately proves & certifies Safe at k <= 2!
    let kind_ctx = RunContext::new(req.timeout).with_lemma_hub(hub.clone());
    let boosted = kind_cap2.check(&req, &kind_ctx).unwrap();
    assert_eq!(
        boosted.verdict,
        Verdict::Safe,
        "boosted k-induction failed: {boosted:?}"
    );
    assert!(
        matches!(
            boosted.certificate,
            Some(Certificate::KInduction { k, .. }) if k <= 2
        ),
        "expected KInduction certificate at k <= 2, got {:?}",
        boosted.certificate
    );
    assert!(
        hub.stats().accepted > 0,
        "expected k-induction to accept shared standing invariants: {:?}",
        hub.stats()
    );

    // 4. Live concurrent portfolio run across diverse exporters & importers.
    let engines: Vec<Arc<dyn Engine>> = vec![
        Arc::new(Ic3Engine { mini: true }),
        Arc::new(KInduction { max_depth: None }),
        Arc::new(CarEngine { backward: false }),
        Arc::new(GlaEngine),
        Arc::new(Ic3InnEngine),
        Arc::new(BddEngine::default()),
    ];
    let outcome = portfolio::run(&engines, &req, None);
    assert!(outcome.conflicts.is_empty(), "{:?}", outcome.conflicts);
    assert_eq!(portfolio::verdict(&outcome), Verdict::Safe);
    let stats = outcome
        .lemma_stats
        .expect("portfolio with >=2 engines must attach a LemmaHub");
    assert!(
        stats.published_standing > 0 || stats.published_frame > 0,
        "expected live portfolio engines to publish lemmas: {stats:?}"
    );
}

#[test]
fn adversarial_false_lemmas_are_rejected_by_every_importing_engine() {
    if informal_sat::Cadical::signature().is_none() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    let dir = TempDir::new("adversarial");
    let buggy = dir.write("bug7.btor2", &buggy_counter_4(7));
    let safe = dir.write("safe_wrap.btor2", &wrapping_counter_8(10, 14));

    let buggy_aig = informal_aig::load(&buggy).unwrap();
    let buggy_dhash = informal_bmc::store::design_hash(&buggy_aig);
    let buggy_phash = informal_bmc::store::property_hash(&buggy_aig);

    let safe_aig = informal_aig::load(&safe).unwrap();
    let safe_dhash = informal_bmc::store::design_hash(&safe_aig);
    let safe_phash = informal_bmc::store::property_hash(&safe_aig);

    let poison_hub = |dhash: u64, phash: u64| -> Arc<LemmaHub> {
        let hub = Arc::new(LemmaHub::new());
        let adv = hub.next_publisher_id();
        // Inject FALSE standing clauses:
        // - `!(cnt == 0)` (blocks Init: clause `b0 \/ b1 \/ b2 \/ b3`)
        // - `!(cnt == 7)` (blocks reachable bad state 7: `!b0 \/ !b1 \/ !b2 \/ b3`)
        // - `!(cnt == 3)` (blocks reachable state 3: `!b0 \/ !b1 \/ b2 \/ b3`)
        // Plus FALSE bounded frame clauses at high level 10!
        hub.publish_batch(vec![
            SharedLemma {
                design_hash: dhash,
                property_hash: phash,
                scope: LemmaScope::Standing,
                clause: vec![(0, false), (1, false), (2, false), (3, false)],
                source: "adversary".into(),
                publisher_id: adv,
            },
            SharedLemma {
                design_hash: dhash,
                property_hash: phash,
                scope: LemmaScope::Standing,
                clause: vec![(0, true), (1, true), (2, true), (3, false)],
                source: "adversary".into(),
                publisher_id: adv,
            },
            SharedLemma {
                design_hash: dhash,
                property_hash: phash,
                scope: LemmaScope::Standing,
                clause: vec![(0, true), (1, true), (2, false), (3, false)],
                source: "adversary".into(),
                publisher_id: adv,
            },
            SharedLemma {
                design_hash: dhash,
                property_hash: phash,
                scope: LemmaScope::Frame {
                    level: 10,
                    folded: None,
                },
                clause: vec![(0, false), (1, false), (2, false), (3, false)],
                source: "adversary".into(),
                publisher_id: adv,
            },
            // Poisoned frame clause blocking state `cnt == 1` (reachable at step 1!)
            // claimed at `level = 10`: false at every level >= 1, must be rejected.
            SharedLemma {
                design_hash: dhash,
                property_hash: phash,
                scope: LemmaScope::Frame {
                    level: 10,
                    folded: None,
                },
                clause: vec![(0, true), (1, false), (2, false), (3, false)],
                source: "adversary".into(),
                publisher_id: adv,
            },
        ]);
        hub
    };

    let mut importers: Vec<Box<dyn Engine>> = vec![
        Box::new(Ic3Engine { mini: true }),
        Box::new(Ic3Engine { mini: false }),
        Box::new(Ic3Mt::default()),
        Box::new(KInduction {
            max_depth: Some(25),
        }),
        Box::new(KindIc3 { ic3_share: 0.5 }),
        Box::new(CarEngine { backward: false }),
        Box::new(GlaEngine),
        Box::new(Ic3InnEngine),
    ];
    if informal_sat::Interpolator::available() {
        importers.push(Box::new(ImcEngine));
    }

    let req_bug = CheckRequest::new(buggy, ModelFormat::Btor2, Duration::from_secs(15));
    let req_safe = CheckRequest::new(safe, ModelFormat::Btor2, Duration::from_secs(15));

    for eng in &importers {
        // A. Buggy model with poisoned hub MUST return Unsafe(7) and reject false clauses!
        let hub_b = poison_hub(buggy_dhash, buggy_phash);
        let ctx_b = RunContext::new(req_bug.timeout).with_lemma_hub(hub_b.clone());
        let res_b = eng.check(&req_bug, &ctx_b).unwrap();
        assert_eq!(
            res_b.verdict,
            Verdict::Unsafe { depth: Some(7) },
            "engine {} was fooled by poisoned hub on buggy model: {:?}",
            eng.name(),
            res_b
        );
        assert_eq!(
            hub_b.stats().accepted,
            0,
            "engine {} accepted a poisoned false clause on buggy model: {:?}",
            eng.name(),
            hub_b.stats()
        );
        assert!(
            hub_b.stats().rejected >= 3,
            "engine {} did not reject poisoned standing clauses: {:?}",
            eng.name(),
            hub_b.stats()
        );

        // B. Safe model with poisoned hub MUST still certify Safe and reject false clauses!
        let hub_s = poison_hub(safe_dhash, safe_phash);
        let ctx_s = RunContext::new(req_safe.timeout).with_lemma_hub(hub_s.clone());
        let res_s = eng.check(&req_safe, &ctx_s).unwrap();
        assert_eq!(
            res_s.verdict,
            Verdict::Safe,
            "engine {} failed on safe model with poisoned hub: {:?}",
            eng.name(),
            res_s
        );
        assert!(
            res_s.certificate.is_some(),
            "engine {} returned Safe without certificate",
            eng.name()
        );
        assert!(
            hub_s.stats().rejected >= 1,
            "engine {} did not reject poisoned clauses on safe model: {:?}",
            eng.name(),
            hub_s.stats()
        );
    }
}

#[test]
fn multi_property_shared_cone_accelerates_sibling_and_isolates_different_design() {
    if informal_sat::Cadical::signature().is_none() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    let dir = TempDir::new("multiprop");
    // Property 1 & Property 2 share the EXACT same 8-bit wrapping counter (0..=200),
    // differing only in their bad output (250 vs 240).
    let prop1 = dir.write("prop1.btor2", &wrapping_counter_8(200, 250));
    let prop2 = dir.write("prop2.btor2", &wrapping_counter_8(200, 240));
    // Property 3 is a DIFFERENT design (free-running 8-bit counter that DOES hit 240!).
    let prop3 = dir.write(
        "prop3_diff_design.btor2",
        "\
1 sort bitvec 8
2 sort bitvec 1
3 zero 1
4 one 1
5 state 1 cnt
6 init 1 5 3
7 add 1 5 4
8 next 1 5 7
9 constd 1 240
10 eq 2 5 9
11 bad 10
",
    );

    let hub = Arc::new(LemmaHub::new());
    let req1 = CheckRequest::new(prop1, ModelFormat::Btor2, Duration::from_secs(15));
    let req2 = CheckRequest::new(prop2, ModelFormat::Btor2, Duration::from_secs(15));
    let req3 = CheckRequest::new(prop3, ModelFormat::Btor2, Duration::from_secs(15));

    // Bare k-induction(k<=2) fails on prop2 (238 -> 239 -> 240 unreachable path).
    let kind2 = KInduction { max_depth: Some(2) };
    assert!(matches!(
        kind2
            .check(&req2, &RunContext::new(req2.timeout))
            .unwrap()
            .verdict,
        Verdict::Unknown { .. }
    ));

    // Prove prop1 with BDD into shared hub -> publishes standing invariant for design_hash.
    let r1 = BddEngine::default()
        .check(
            &req1,
            &RunContext::new(req1.timeout).with_lemma_hub(hub.clone()),
        )
        .unwrap();
    assert_eq!(r1.verdict, Verdict::Safe);

    // Sibling prop2 immediately proves Safe at k <= 2 by importing prop1's invariant!
    let r2 = kind2
        .check(
            &req2,
            &RunContext::new(req2.timeout).with_lemma_hub(hub.clone()),
        )
        .unwrap();
    assert_eq!(r2.verdict, Verdict::Safe);
    assert!(hub.stats().accepted > 0, "{:?}", hub.stats());

    // Different design prop3 MUST NOT accept prop1's invariant (different design_hash)
    // and MUST NOT claim Safe!
    let acc_before = hub.stats().accepted;
    let r3 = kind2
        .check(
            &req3,
            &RunContext::new(req3.timeout).with_lemma_hub(hub.clone()),
        )
        .unwrap();
    assert_ne!(r3.verdict, Verdict::Safe, "prop3 is actually unsafe!");
    assert_eq!(
        hub.stats().accepted,
        acc_before,
        "different design must not import sibling invariants"
    );
}

#[test]
fn session_multi_property_prove_shares_lemmas_across_assertions() {
    if informal_sat::Cadical::signature().is_none() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    let dir = TempDir::new("session-share");
    let sv = dir.write(
        "counter.sv",
        "\
module counter(input logic clk, input logic rst);
  logic [5:0] cnt;
  always_ff @(posedge clk) begin
    if (rst) cnt <= 6'd0;
    else if (cnt == 6'd40) cnt <= 6'd0;
    else cnt <= cnt + 6'd1;
  end
  a_not_55: assert property (@(posedge clk) disable iff (rst) cnt != 6'd55);
  a_not_60: assert property (@(posedge clk) disable iff (rst) cnt != 6'd60);
endmodule
",
    );
    let engines: Vec<Arc<dyn Engine>> = vec![
        Arc::new(BddEngine::default()),
        Arc::new(Ic3Engine { mini: true }),
        Arc::new(KInduction { max_depth: None }),
    ];
    let mut sess = Session::new(engines, dir.0.join("work"));
    sess.effort = Effort::Standard;
    sess.orchestration = Orchestration::Off;
    sess.preconditions = false;
    sess.time_limit = Duration::from_secs(15);
    sess.design.files.push(sv);
    sess.elaborate(Some("counter".into())).unwrap();
    sess.clocks.push(informal_fpv::session::Clock {
        signal: "clk".into(),
        edge: "posedge",
        factor: 1,
        phase: 0,
    });
    sess.reset = Some("rst".into());

    let proved = sess.prove(&[]).unwrap();
    assert_eq!(proved.len(), 2);
    for &idx in &proved {
        assert!(
            matches!(sess.props[idx].status, Status::Proven { .. }),
            "property {} was not proven: {:?}",
            sess.props[idx].name,
            sess.props[idx].status
        );
    }
    let stats = sess
        .last_lemma_stats
        .as_ref()
        .expect("session prove must record LemmaHub stats");
    assert!(
        stats.published_standing > 0,
        "expected task LemmaHub to record published standing invariants: {stats:?}"
    );
}

#[test]
fn clock_folded_standing_invariants_share_across_folded_engines() {
    if informal_sat::Cadical::signature().is_none() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    // 8-bit wrapping counter gated by an explicit free-running clock latch `clk`
    // (`clk` toggles every step; `cnt` increments on `clk == 1`, wrapping at 80).
    // Folded engines fold `clk` and emit `FoldedStanding { clock, phase }`.
    let clocked = |target: u32| {
        format!(
            "\
1 sort bitvec 8
2 sort bitvec 1
3 zero 1
4 state 1 cnt
5 init 1 4 3
6 zero 2
7 state 2 clk
8 init 2 7 6
9 not 2 7
10 next 2 7 9
11 one 1
12 add 1 4 11
13 constd 1 80
14 eq 2 4 13
15 ite 1 14 3 12
16 ite 1 7 15 4
17 next 1 4 16
18 constd 1 {target}
19 eq 2 4 18
20 bad 19
"
        )
    };
    let dir = TempDir::new("folded-share");
    let p1 = dir.write("clk_p1.btor2", &clocked(120));
    let p2 = dir.write("clk_p2.btor2", &clocked(110));
    let req1 = CheckRequest::new(p1, ModelFormat::Btor2, Duration::from_secs(15));
    let req2 = CheckRequest::new(p2, ModelFormat::Btor2, Duration::from_secs(15));

    // Bare k-induction(k <= 2) cannot close p2 (108 -> 109 -> 110 unreachable chain).
    let kind2 = KInduction { max_depth: Some(2) };
    assert!(matches!(
        kind2
            .check(&req2, &RunContext::new(req2.timeout))
            .unwrap()
            .verdict,
        Verdict::Unknown { .. }
    ));

    // Prove p1 with BDD (clock-folded) -> publishes FoldedStanding into hub.
    let hub = Arc::new(LemmaHub::new());
    let r1 = BddEngine::default()
        .check(
            &req1,
            &RunContext::new(req1.timeout).with_lemma_hub(hub.clone()),
        )
        .unwrap();
    assert_eq!(r1.verdict, Verdict::Safe);
    assert!(matches!(r1.certificate, Some(Certificate::TwoStep { .. })));
    assert!(hub.stats().published_standing > 0);

    // Folded k-induction(k <= 2) on sibling p2 imports FoldedStanding and closes Safe!
    let r2 = kind2
        .check(
            &req2,
            &RunContext::new(req2.timeout).with_lemma_hub(hub.clone()),
        )
        .unwrap();
    assert_eq!(r2.verdict, Verdict::Safe);
    assert!(matches!(
        r2.certificate,
        Some(Certificate::TwoStepKInduction { k, .. }) if k <= 2
    ));
    assert!(hub.stats().accepted > 0, "{:?}", hub.stats());
}

#[test]
fn aiger_selector_keeps_standard_lean_and_unlocks_secondary_engines_at_high() {
    if informal_sat::Cadical::signature().is_none() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    let mut fleet: Vec<Arc<dyn Engine>> = Vec::new();
    fleet.extend(informal_bmc::engines());
    fleet.extend(informal_car::engines());
    fleet.extend(informal_ic3::engines());
    fleet.extend(informal_imc::engines());
    fleet.extend(informal_bdd::engines());
    fleet.extend(informal_swarm::engines());
    fleet.extend(informal_liveness::engines());

    // Large AIGER safety header (500 latches > BDD cap 200).
    let big_aig = b"aig 2000 100 500 0 1400 1\n";
    let std_names: Vec<String> =
        informal_engine::selector::select_for_aiger_bytes(&fleet, big_aig, false, Effort::Standard)
            .into_iter()
            .map(|e| e.name().to_string())
            .collect();
    let has = |list: &[String], name: &str| list.iter().any(|n| n == name);

    // Standard keeps the lean 7-engine diverse core without redundant/overlapping threads.
    for core in [
        "informal-bmc",
        "informal-bmc-10",
        "informal-kind",
        "informal-ic3-mt",
        "informal-gla",
        "informal-car",
        "informal-swarm",
    ] {
        assert!(has(&std_names, core), "missing {core} from {std_names:?}");
    }
    for secondary in [
        "informal-bmc-65",
        "informal-kind-ic3",
        "informal-ic3-cadical",
        "informal-bcar",
        "informal-ic3-inn",
        "informal-imc",
        "informal-bdd",
        "informal-ic3",
    ] {
        assert!(
            !has(&std_names, secondary),
            "unexpected {secondary} in Standard large-AIGER fleet: {std_names:?}"
        );
    }

    // High unlocks the secondary engines while still respecting BDD's max_state_bits cap.
    let high_names: Vec<String> =
        informal_engine::selector::select_for_aiger_bytes(&fleet, big_aig, false, Effort::High)
            .into_iter()
            .map(|e| e.name().to_string())
            .collect();
    for secondary in [
        "informal-bmc-65",
        "informal-kind-ic3",
        "informal-ic3-cadical",
        "informal-bcar",
        "informal-ic3-inn",
        "informal-imc",
    ] {
        assert!(
            has(&high_names, secondary),
            "High effort must include {secondary}: {high_names:?}"
        );
    }
    assert!(!has(&high_names, "informal-bdd"));
}
