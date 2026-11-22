use super::{audit_session, wildcard_match, AuditRuntime, FvpSpec};
use informal_fpv::{Kind, Property, Session, Status, EMBEDDED};
use serde_json::json;
use std::path::PathBuf;

#[test]
fn wildcard_matcher_handles_globs_and_exact_names() {
    assert!(wildcard_match("a_*", "a_count_max"));
    assert!(wildcard_match("CSR::a_rw_*", "CSR::a_rw_ctrl"));
    assert!(wildcard_match("c_status?", "c_status3"));
    assert!(!wildcard_match("c_status?", "c_status30"));
    assert!(!wildcard_match("a_max", "a_max2"));
}

#[test]
fn spec_validation_rejects_bad_schema_and_duplicate_ids() {
    let bad_schema = json!({
        "schema": "wrong/1",
        "requirements": []
    });
    assert!(FvpSpec::parse_value(&bad_schema).is_err());

    let dup_ids = json!({
        "schema": "informal.spec/1",
        "requirements": [
            {"id": "REQ-1", "assertions": ["a1"]},
            {"id": "REQ-1", "assertions": ["a2"]}
        ]
    });
    assert!(FvpSpec::parse_value(&dup_ids)
        .unwrap_err()
        .contains("duplicate requirement id"));

    let bad_floor = json!({
        "schema": "informal.spec/1",
        "signoff": {"coi_coverage_floor": 120.0},
        "requirements": []
    });
    assert!(FvpSpec::parse_value(&bad_floor).is_err());
}

fn make_prop(task: &str, short: &str, kind: Kind, expr: &str, status: Status) -> Property {
    Property {
        name: informal_fpv::session::qualify(task, short),
        kind,
        expr: expr.into(),
        enabled: true,
        status,
        trace: None,
        source: None,
        rtl: false,
        engines: Vec::new(),
        ai: false,
        under_ai: Vec::new(),
        vacuity: None,
        task: task.into(),
        copy_of: None,
        witness_trace: None,
        bound: None,
        proof_core: None,
        from_assert: None,
        rtl_from_assert: false,
        stats: None,
    }
}

fn make_session(props: Vec<Property>) -> Session {
    let mut s = Session::new(Vec::new(), PathBuf::from("."));
    s.elaborated = true;
    s.design.top = Some("top".into());
    s.clocks.push(informal_fpv::session::Clock {
        signal: "clk".into(),
        edge: "posedge",
        factor: 1,
        phase: 0,
    });
    s.reset = Some("!rst_n".into());
    s.design.hierarchy.push(informal_svlang::Scope {
        path: "top".into(),
        name: "top".into(),
        module: "top".into(),
        signals: vec![
            informal_svlang::ScopeSignal {
                name: "clk".into(),
                width: 1,
                kind: "port".into(),
                dir: Some("input".into()),
            },
            informal_svlang::ScopeSignal {
                name: "rst_n".into(),
                width: 1,
                kind: "port".into(),
                dir: Some("input".into()),
            },
            informal_svlang::ScopeSignal {
                name: "en".into(),
                width: 1,
                kind: "port".into(),
                dir: Some("input".into()),
            },
            informal_svlang::ScopeSignal {
                name: "state_reg".into(),
                width: 4,
                kind: "variable".into(),
                dir: None,
            },
            informal_svlang::ScopeSignal {
                name: "out_q".into(),
                width: 1,
                kind: "port".into(),
                dir: Some("output".into()),
            },
        ],
        children: Vec::new(),
    });
    s.props = props;
    s
}

fn base_rt(task: &str) -> AuditRuntime {
    AuditRuntime {
        spec_source: "inline".into(),
        task: task.into(),
        ..AuditRuntime::default()
    }
}

#[test]
fn spec_validation_checks_proof_target_and_empty_req_ids() {
    let bad_target = json!({
        "schema": "informal.spec/1",
        "signoff": {"proof_target": "assumed"},
        "requirements": []
    });
    assert!(FvpSpec::parse_value(&bad_target)
        .unwrap_err()
        .contains("signoff.proof_target"));

    let empty_id = json!({
        "schema": "informal.spec/1",
        "requirements": [{"id": "   ", "assertions": ["a1"]}]
    });
    assert!(FvpSpec::parse_value(&empty_id)
        .unwrap_err()
        .contains("non-empty `id`"));

    let bad_mut_floor = json!({
        "schema": "informal.spec/1",
        "signoff": {"mutation_score_floor": -1.0},
        "requirements": []
    });
    assert!(FvpSpec::parse_value(&bad_mut_floor)
        .unwrap_err()
        .contains("signoff.mutation_score_floor"));

    let bad_coi_floor = json!({
        "schema": "informal.spec/1",
        "signoff": {"coi_coverage_floor": 100.5},
        "requirements": []
    });
    assert!(FvpSpec::parse_value(&bad_coi_floor)
        .unwrap_err()
        .contains("signoff.coi_coverage_floor"));
}

#[test]
fn disabled_properties_detected_with_task_qualification_and_globs() {
    let proven = Status::Proven {
        engine: "ric3".into(),
        ms: 1,
    };
    let covered = Status::Covered {
        engine: "bmc".into(),
        depth: Some(1),
        ms: 1,
    };

    let mut p_emb_dis = make_prop(EMBEDDED, "p_emb", Kind::Assert, "en", proven.clone());
    p_emb_dis.enabled = false;

    let mut p_task_dis = make_prop("my_task", "p_foo", Kind::Assert, "en", proven.clone());
    p_task_dis.enabled = false;

    let p_shadow_emb = make_prop(EMBEDDED, "p_shadow", Kind::Assert, "en", proven.clone());
    let mut p_shadow_task = make_prop("my_task", "p_shadow", Kind::Assert, "en", proven.clone());
    p_shadow_task.enabled = false;

    let mut c_emb_dis = make_prop(EMBEDDED, "c_emb", Kind::Cover, "en", covered.clone());
    c_emb_dis.enabled = false;

    let mut c_task_dis = make_prop("my_task", "c_foo", Kind::Cover, "en", covered);
    c_task_dis.enabled = false;

    let p_task_ok = make_prop("my_task", "p_ok", Kind::Assert, "en", proven);

    let mut s = make_session(vec![
        p_emb_dis,
        p_task_dis,
        p_shadow_emb,
        p_shadow_task,
        c_emb_dis,
        c_task_dis,
        p_task_ok,
    ]);
    s.tasks.push(informal_fpv::session::Task::new("my_task"));

    // 1. `<embedded>`: exact, glob, `<embedded>::` prefix, and cross-task `my_task::*`
    let spec_emb = FvpSpec::parse_value(&json!({
        "schema": "informal.spec/1",
        "design": {"top": "top", "clocks": ["clk"], "reset": "!rst_n"},
        "requirements": [
            {"id": "R1", "assertions": ["p_emb"], "covers": ["c_emb"]},
            {"id": "R2", "assertions": ["p_em*"], "covers": []},
            {"id": "R3", "assertions": ["<embedded>::p_emb"], "covers": []},
            {"id": "R4", "assertions": ["my_task::p_foo"], "covers": []},
            {"id": "R5", "assertions": ["my_task::p_fo*"], "covers": []},
            {"id": "R6", "assertions": ["p_foo"], "covers": []},
            {"id": "R7", "assertions": ["c_emb"], "covers": []}
        ]
    }))
    .unwrap();
    let out_emb = audit_session(&s, &spec_emb, &base_rt(EMBEDDED));
    let reqs_emb = out_emb["requirements"].as_array().unwrap();
    assert!(reqs_emb[0]["issues"][0]
        .as_str()
        .unwrap()
        .contains("assertion `p_emb` exists but is disabled"));
    assert!(reqs_emb[0]["issues"][1]
        .as_str()
        .unwrap()
        .contains("witness cover `c_emb` exists but is disabled"));
    assert!(reqs_emb[1]["issues"][0]
        .as_str()
        .unwrap()
        .contains("assertion `p_emb` exists but is disabled"));
    assert!(reqs_emb[2]["issues"][0]
        .as_str()
        .unwrap()
        .contains("assertion `p_emb` exists but is disabled"));
    assert!(reqs_emb[3]["issues"][0]
        .as_str()
        .unwrap()
        .contains("assertion `my_task::p_foo` exists but is disabled"));
    assert!(reqs_emb[4]["issues"][0]
        .as_str()
        .unwrap()
        .contains("assertion `my_task::p_foo` exists but is disabled"));
    // Unqualified `p_foo` in `<embedded>` does not match `my_task::p_foo`.
    assert!(reqs_emb[5]["issues"][0]
        .as_str()
        .unwrap()
        .contains("target assertion `p_foo` is missing"));
    // Disabled cover `c_emb` must NOT match as an assertion.
    assert!(reqs_emb[6]["issues"][0]
        .as_str()
        .unwrap()
        .contains("target assertion `c_emb` is missing"));

    // 2. Named task `my_task`: unqualified, glob, qualified, shadowing, and disabled cover.
    let spec_task = FvpSpec::parse_value(&json!({
        "schema": "informal.spec/1",
        "design": {"top": "top", "clocks": ["clk"], "reset": "!rst_n"},
        "requirements": [
            {"id": "T1", "assertions": ["p_foo"], "covers": ["c_foo"]},
            {"id": "T2", "assertions": ["p_fo*"], "covers": []},
            {"id": "T3", "assertions": ["my_task::p_foo"], "covers": []},
            {"id": "T4", "assertions": ["my_task::p_fo*"], "covers": []},
            {"id": "T5", "assertions": ["p_shadow"], "covers": []},
            {"id": "T6", "assertions": ["p_ok"], "covers": []}
        ]
    }))
    .unwrap();
    let out_task = audit_session(&s, &spec_task, &base_rt("my_task"));
    let reqs_task = out_task["requirements"].as_array().unwrap();
    for (idx, expected) in [
        (0, "assertion `my_task::p_foo` exists but is disabled"),
        (1, "assertion `my_task::p_foo` exists but is disabled"),
        (2, "assertion `my_task::p_foo` exists but is disabled"),
        (3, "assertion `my_task::p_foo` exists but is disabled"),
        // `my_task::p_shadow` (disabled) shadows `<embedded>::p_shadow` (enabled).
        (4, "assertion `my_task::p_shadow` exists but is disabled"),
    ] {
        assert_eq!(reqs_task[idx]["status"], "missing_assert");
        assert!(
            reqs_task[idx]["issues"][0]
                .as_str()
                .unwrap()
                .contains(expected),
            "req {idx}: {:?}",
            reqs_task[idx]["issues"]
        );
    }
    assert!(reqs_task[0]["issues"][1]
        .as_str()
        .unwrap()
        .contains("witness cover `my_task::c_foo` exists but is disabled"));
    assert_eq!(reqs_task[5]["status"], "verified");
}

#[test]
fn signoff_gates_forbid_under_ai_and_dead_end_checks() {
    let mut p = make_prop(
        EMBEDDED,
        "a_ok",
        Kind::Assert,
        "en",
        Status::Proven {
            engine: "ric3".into(),
            ms: 1,
        },
    );
    p.under_ai = vec!["m_ai".into()];
    let s = make_session(vec![p]);

    let spec = FvpSpec::parse_value(&json!({
        "schema": "informal.spec/1",
        "design": {"top": "top", "clocks": ["clk"], "reset": "!rst_n"},
        "requirements": [{"id": "R1", "assertions": ["a_ok"], "covers": []}],
        "signoff": {
            "proof_target": "proven",
            "forbid_under_ai": true,
            "require_dead_end_check": true,
            "max_dead_ends": 0
        }
    }))
    .unwrap();

    let mut rt = base_rt(EMBEDDED);
    let out = audit_session(&s, &spec, &rt);
    assert_eq!(out["signoff_ready"], false);
    assert_eq!(out["requirements"][0]["status"], "undetermined");
    let gates = out["gates"].as_array().unwrap();
    let ai_gate = gates.iter().find(|g| g["gate"] == "ai_provenance").unwrap();
    assert_eq!(ai_gate["ok"], false);
    let de_gate = gates
        .iter()
        .find(|g| g["gate"] == "zero_dead_ends")
        .unwrap();
    assert_eq!(de_gate["ok"], false);
    assert_eq!(de_gate["actual"], "not checked (pass check_dead_end=true)");

    // Clear AI dependency, then trigger dead_end_found = true.
    let mut s_clean_ai = s;
    s_clean_ai.props[0].under_ai.clear();
    rt.dead_end_checked = true;
    rt.dead_end_found = true;
    rt.dead_end_depth = Some(3);
    let out_de = audit_session(&s_clean_ai, &spec, &rt);
    assert_eq!(out_de["signoff_ready"], false);
    assert_eq!(out_de["boundary_audit"]["ok"], false);
    assert_eq!(out_de["requirements"][0]["status"], "overconstrained");
    let de_gate_fail = out_de["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["gate"] == "zero_dead_ends")
        .unwrap();
    assert_eq!(de_gate_fail["ok"], false);
    assert!(de_gate_fail["actual"].as_str().unwrap().contains("depth 3"));
}

#[test]
fn boundary_allow_internal_assumes_waiver_and_forbidden_precedence() {
    let p = make_prop(
        EMBEDDED,
        "a_ok",
        Kind::Assert,
        "en",
        Status::Proven {
            engine: "ric3".into(),
            ms: 1,
        },
    );
    let assume_int = make_prop(
        EMBEDDED,
        "m_int",
        Kind::Assume,
        "state_reg == 4'd0",
        Status::Unprocessed,
    );
    let s = make_session(vec![p, assume_int]);
    let rt = base_rt(EMBEDDED);

    // 1. Default: internal_state rejected.
    let spec_default = FvpSpec::parse_value(&json!({
        "schema": "informal.spec/1",
        "design": {"top": "top", "clocks": ["clk"], "reset": "!rst_n"},
        "boundary": {"allowed_assume_inputs": ["en"]},
        "requirements": [{"id": "R1", "assertions": ["a_ok"], "covers": []}]
    }))
    .unwrap();
    let out_default = audit_session(&s, &spec_default, &rt);
    assert_eq!(out_default["signoff_ready"], false);
    assert_eq!(out_default["boundary_audit"]["ok"], false);
    assert_eq!(out_default["requirements"][0]["status"], "overconstrained");

    // 2. Waived via `allow_internal_assumes`.
    let spec_waived = FvpSpec::parse_value(&json!({
        "schema": "informal.spec/1",
        "design": {"top": "top", "clocks": ["clk"], "reset": "!rst_n"},
        "boundary": {
            "allowed_assume_inputs": ["en"],
            "allow_internal_assumes": ["state_reg"]
        },
        "requirements": [{"id": "R1", "assertions": ["a_ok"], "covers": []}]
    }))
    .unwrap();
    let out_waived = audit_session(&s, &spec_waived, &rt);
    assert_eq!(out_waived["signoff_ready"], true);
    assert_eq!(out_waived["boundary_audit"]["ok"], true);
    assert_eq!(out_waived["requirements"][0]["status"], "verified");

    // 3. `forbidden_assume_signals` overrides `allow_internal_assumes`.
    let spec_forbidden = FvpSpec::parse_value(&json!({
        "schema": "informal.spec/1",
        "design": {"top": "top", "clocks": ["clk"], "reset": "!rst_n"},
        "boundary": {
            "allowed_assume_inputs": ["en"],
            "allow_internal_assumes": ["state_reg"],
            "forbidden_assume_signals": ["state_reg"]
        },
        "requirements": [{"id": "R1", "assertions": ["a_ok"], "covers": []}]
    }))
    .unwrap();
    let out_forbidden = audit_session(&s, &spec_forbidden, &rt);
    assert_eq!(out_forbidden["signoff_ready"], false);
    assert_eq!(out_forbidden["boundary_audit"]["ok"], false);
    assert!(
        out_forbidden["boundary_audit"]["overconstrained_assumptions"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("boundary.forbidden_assume_signals")
    );
}

#[test]
fn bounded_proven_respects_min_bound() {
    let p_bnd = make_prop(
        EMBEDDED,
        "a_bnd",
        Kind::Assert,
        "en",
        Status::BoundedProven {
            engine: "bmc".into(),
            bound: 8,
            ms: 1,
        },
    );
    let s = make_session(vec![p_bnd]);
    let rt = base_rt(EMBEDDED);

    let make_spec = |target: &str, min_bound: u32| {
        FvpSpec::parse_value(&json!({
            "schema": "informal.spec/1",
            "design": {"top": "top", "clocks": ["clk"], "reset": "!rst_n"},
            "requirements": [{"id": "R1", "assertions": ["a_bnd"], "covers": []}],
            "signoff": {"proof_target": target, "min_bound": min_bound}
        }))
        .unwrap()
    };

    let out_proven = audit_session(&s, &make_spec("proven", 0), &rt);
    assert_eq!(out_proven["signoff_ready"], false);
    assert_eq!(out_proven["requirements"][0]["status"], "undetermined");

    let out_too_low = audit_session(&s, &make_spec("bounded_proven", 10), &rt);
    assert_eq!(out_too_low["signoff_ready"], false);
    assert_eq!(out_too_low["requirements"][0]["status"], "undetermined");

    let out_ok = audit_session(&s, &make_spec("bounded_proven", 8), &rt);
    assert_eq!(out_ok["signoff_ready"], true);
    assert_eq!(out_ok["requirements"][0]["status"], "verified");
}
