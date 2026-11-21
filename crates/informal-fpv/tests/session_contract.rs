//! Layer 1 TDD Schema Contract (Rust -> TypeScript):
//!
//! Serializes a canonical multi-task `informal_fpv::Session` covering EVERY
//! `Status` variant (including `BoundedProven`), `Kind`, `Vacuity`, `Environment`,
//! `ProofStats`, `under_ai`, and `ai` provenance, and compares it against
//! `ui/tests/fixtures/session-contract.json`.
//!
//! Run `INFORMAL_BLESS=1 cargo test -p informal-fpv --test session_contract`
//! whenever the serialized `Session` schema changes intentionally; the paired
//! Vitest test `ui/src/store/workbenchController.test.ts` then guarantees the
//! React UI handles every field and enum variant exhaustively.

use informal_fpv::session::{
    Clock, Environment, Kind, Property, Session, SourceLoc, Status, Vacuity, EMBEDDED,
};
use informal_fpv::stats::ProofStats;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn canonical_session() -> Session {
    let mut s = Session::new(Vec::new(), PathBuf::from("informal_work"));
    s.design.top = Some("axi_lite_demo".into());
    s.design.files = vec![PathBuf::from("axi_lite_demo.sv")];
    s.elaborated = true;
    s.clocks = vec![Clock {
        signal: "clk".into(),
        edge: "posedge",
        factor: 1,
        phase: 0,
    }];
    s.reset = Some("!rst_n".into());
    s.create_task("Protocol").unwrap();
    s.current_task = EMBEDDED.into();

    let src = Some(SourceLoc {
        file: "axi_lite_demo.sv".into(),
        line: 12,
    });

    // Helper to build a Property cleanly.
    let prop = |name: &str, kind: Kind, expr: &str, status: Status, task: &str| Property {
        name: name.into(),
        kind,
        expr: expr.into(),
        enabled: true,
        rtl: false,
        status,
        trace: None,
        witness_trace: None,
        source: src.clone(),
        engines: Vec::new(),
        ai: false,
        under_ai: Vec::new(),
        task: task.into(),
        copy_of: None,
        stats: None,
        vacuity: None,
        bound: None,
        proof_core: None,
        from_assert: None,
        rtl_from_assert: false,
    };

    // Every Status variant + Assume represented under task `Protocol` while
    // `<embedded>` has 0 properties (reproducing the multi-task app scenario).
    let mut p_proven = prop(
        "Protocol::a_aw_stable",
        Kind::Assert,
        "awvalid && !awready |=> $stable(awaddr)",
        Status::Proven {
            engine: "informal-kind".into(),
            ms: 46,
        },
        "Protocol",
    );
    p_proven.vacuity = Some(Vacuity::NonVacuous);
    p_proven.stats = Some(ProofStats {
        engine: Some("informal-kind".into()),
        wall_ms: 46,
        cpu_ms: Some(52),
        peak_rss_kb: Some(14_200),
        depth: Some(2),
        depth_kind: Some("k"),
        max_bound: None,
        invariant_clauses: Some(4),
        evidence: Some("k-induction".into()),
        certificate: None,
    });

    let mut p_bounded = prop(
        "Protocol::a_bounded_20",
        Kind::Assert,
        "wvalid && !wready |=> $stable(wdata)",
        Status::BoundedProven {
            engine: "informal-bmc".into(),
            bound: 20,
            ms: 115,
        },
        "Protocol",
    );
    p_bounded.bound = Some(20);

    let mut p_cex = prop(
        "Protocol::a_bad_hold",
        Kind::Assert,
        "arvalid |=> !arvalid",
        Status::Cex {
            engine: "informal-bmc".into(),
            depth: Some(3),
            ms: 18,
        },
        "Protocol",
    );
    p_cex.trace = Some("informal_work/a_bad_hold.vcd".into());

    let mut p_covered = prop(
        "Protocol::c_wr_txn",
        Kind::Cover,
        "awvalid && awready",
        Status::Covered {
            engine: "informal-bmc".into(),
            depth: Some(2),
            ms: 14,
        },
        "Protocol",
    );
    p_covered.trace = Some("informal_work/c_wr_txn.vcd".into());

    let p_unreach = prop(
        "Protocol::c_illegal",
        Kind::Cover,
        "bvalid && !bready && bready",
        Status::Unreachable {
            engine: "informal-ic3".into(),
            ms: 21,
        },
        "Protocol",
    );

    let p_undet = prop(
        "Protocol::a_timeout",
        Kind::Assert,
        "##[1:1000] bvalid",
        Status::Undetermined {
            reason: "timeout".into(),
        },
        "Protocol",
    );

    let p_err = prop(
        "Protocol::a_unsupported",
        Kind::Assert,
        "bad_expr",
        Status::Error {
            message: "unsupported construct".into(),
        },
        "Protocol",
    );

    let p_unproc = prop(
        "Protocol::a_pending",
        Kind::Assert,
        "arvalid && !arready |=> arvalid",
        Status::Unprocessed,
        "Protocol",
    );

    let mut p_assume = prop(
        "Protocol::m_b_valid_hold",
        Kind::Assume,
        "bvalid && !bready |=> bvalid",
        Status::Unprocessed,
        "Protocol",
    );
    p_assume.ai = true;

    let mut p_under_ai = prop(
        "Protocol::a_under_ai_check",
        Kind::Assert,
        "bready",
        Status::Proven {
            engine: "informal-kind".into(),
            ms: 12,
        },
        "Protocol",
    );
    p_under_ai.under_ai = vec!["Protocol::m_b_valid_hold".into()];
    p_under_ai.vacuity = Some(Vacuity::Vacuous);

    s.props = vec![
        p_proven, p_bounded, p_cex, p_covered, p_unreach, p_undet, p_err, p_unproc, p_assume,
        p_under_ai,
    ];
    s.environment = Some(Environment {
        task: "Protocol".into(),
        assumptions: vec!["Protocol::m_b_valid_hold".into()],
        asked: 20,
        allowed: Some(20),
        conflict_set: Vec::new(),
    });
    s
}

#[test]
fn session_contract_fixture_matches_rust_schema() {
    let fixture = repo_root().join("ui/tests/fixtures/session-contract.json");
    let json = serde_json::to_string_pretty(&canonical_session()).unwrap() + "\n";

    if std::env::var("INFORMAL_BLESS").as_deref() == Ok("1") {
        std::fs::create_dir_all(fixture.parent().unwrap()).unwrap();
        std::fs::write(&fixture, &json).unwrap();
    }

    let expected = std::fs::read_to_string(&fixture).unwrap_or_else(|e| {
        panic!(
            "missing {} ({e}); run `INFORMAL_BLESS=1 cargo test -p informal-fpv --test session_contract`",
            fixture.display()
        )
    });
    assert_eq!(
        json, expected,
        "Session JSON schema drifted from ui/tests/fixtures/session-contract.json; update UI types/renderers and re-bless with INFORMAL_BLESS=1"
    );

    // Ensure every Status variant word is covered by the fixture.
    let val: serde_json::Value = serde_json::from_str(&json).unwrap();
    let statuses: std::collections::BTreeSet<&str> = val["props"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["status"]["status"].as_str())
        .collect();
    for want in [
        "unprocessed",
        "proven",
        "bounded_proven",
        "cex",
        "covered",
        "unreachable",
        "undetermined",
        "error",
    ] {
        assert!(statuses.contains(want), "fixture missing status `{want}`");
    }
}
