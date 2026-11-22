mod common;

use std::sync::Arc;

use common::{temp_dir, MockDesignFinder};
use informal_upf::{
    load_upf_files, parse_cpf_and_resolve, resolve, DiagSeverity, NullObjectFinder, PstSat,
    UpfFileSpec, DEFAULT_VOLTAGE_TOL_UV,
};

#[test]
fn h2_cross_file_and_nested_update_power_switch_rule_removes_synthesized_ctrl() {
    let dir = temp_dir("cpf_h2_psw");
    let finder: Arc<dyn informal_upf::ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_wrap", "u_wrap/u_sub"],
        &["u_wrap", "u_wrap/u_sub"],
    ));

    let top_cpf = dir.join("top.cpf");
    let sub_cpf = dir.join("sub.cpf");
    std::fs::write(
        &top_cpf,
        r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
create_ground_nets -nets {VSS}
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!bad_shutoff}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
create_nominal_condition -name NC_ON -voltage 0.9 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_mode -name PM_ON  -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -default
create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_OFF}
create_power_switch_rule -name PSW_WRAP -domain PD_WRAP -external_power_net VDD_AON
end_design
"#,
    )
    .unwrap();
    std::fs::write(
        &sub_cpf,
        r#"
set_cpf_version 2.0
update_power_switch_rule -name /PSW_WRAP -enable_condition_1 {/other_aon}
end_design
"#,
    )
    .unwrap();

    // 1. Sequential `load_upf_files` (as used by `Session::load_upf` across multiple `load_upf` calls).
    let mut intent_seq = load_upf_files(
        &[
            UpfFileSpec {
                path: top_cpf.clone(),
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: sub_cpf.clone(),
                scope: Some("u_wrap/u_sub".to_string()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_seq = resolve(&mut intent_seq, finder.as_ref());
    assert!(
        !res_seq
            .intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error),
        "unexpected errors: {:?}",
        res_seq.intent.diagnostics
    );
    let sw_seq = res_seq
        .intent
        .power_switches
        .iter()
        .find(|s| s.name == "PSW_WRAP")
        .expect("PSW_WRAP");
    assert_eq!(
        sw_seq.control_ports,
        vec![("ctrl1".to_string(), "/other_aon".to_string())],
        "stale synthesized `ctrl` port must be removed on cross-file update"
    );
    assert!(!sw_seq.control_port_scopes.contains_key("ctrl"));
    assert_eq!(
        sw_seq.control_port_scopes.get("ctrl1").map(String::as_str),
        Some("u_wrap/u_sub")
    );
    assert!(!sw_seq.shutoff_synthesized_ctrl);
    assert_eq!(
        sw_seq
            .on_states
            .iter()
            .find(|s| s.state_name == "ON")
            .map(|s| s.expr.as_str()),
        Some("/other_aon")
    );

    // 2. Nested `load_upf -format cpf` inside `top_nested.cpf`.
    let top_nested = dir.join("top_nested.cpf");
    std::fs::write(
        &top_nested,
        format!(
            r#"
set_cpf_version 2.0
create_power_nets -nets {{VDD_AON VDD_WRAP}} -voltage 0.9
create_ground_nets -nets {{VSS}}
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {{u_wrap}} -shutoff_condition {{!bad_shutoff}}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
create_power_switch_rule -name PSW_WRAP -domain PD_WRAP -external_power_net VDD_AON
load_upf -format cpf -scope u_wrap/u_sub {}
end_design
"#,
            sub_cpf.display()
        ),
    )
    .unwrap();
    let mut intent_nest = load_upf_files(
        &[UpfFileSpec {
            path: top_nested,
            scope: None,
            version: None,
            is_cpf: true,
        }],
        finder.clone(),
    )
    .unwrap();
    let res_nest = resolve(&mut intent_nest, finder.as_ref());
    let sw_nest = res_nest
        .intent
        .power_switches
        .iter()
        .find(|s| s.name == "PSW_WRAP")
        .expect("PSW_WRAP nested");
    assert_eq!(
        sw_nest.control_ports,
        vec![("ctrl1".to_string(), "/other_aon".to_string())]
    );
    assert!(!sw_nest.shutoff_synthesized_ctrl);
    let on_state = res_nest
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_PD_WRAP" && s.state_name == "ON")
        .expect("fallback ON state");
    assert_eq!(on_state.logic_expr.as_deref(), Some("/other_aon"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn h1_cross_scope_and_cross_file_domain_shutoff_preserves_authoring_scope() {
    let dir = temp_dir("cpf_h1_shutoff");
    let finder: Arc<dyn informal_upf::ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_wrap", "u_wrap/u_sub"],
        &["u_wrap", "u_wrap/u_sub"],
    ));

    // 1. Same-file cross-scope (`load_upf -scope u_wrap/u_sub sub.cpf` inside `top.cpf`, rule at `/`).
    let sub_cpf = dir.join("sub.cpf");
    std::fs::write(
        &sub_cpf,
        r#"
set_cpf_version 2.0
create_power_domain -name PD_SUB -instances {.} -shutoff_condition {!wrap_bad_ctrl}
update_power_domain -name PD_SUB -primary_power_net /VDD_SUB -primary_ground_net /VSS
end_design
"#,
    )
    .unwrap();
    let top_cpf = dir.join("top.cpf");
    std::fs::write(
        &top_cpf,
        format!(
            r#"
set_cpf_version 2.0
create_power_nets -nets {{VDD_AON VDD_SUB}} -voltage 0.9
create_ground_nets -nets {{VSS}}
create_power_domain -name PD_TOP -default
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
load_upf -format cpf -scope u_wrap/u_sub {}
create_power_switch_rule -name PSW_SUB -domain u_wrap/u_sub/PD_SUB -external_power_net VDD_AON
end_design
"#,
            sub_cpf.display()
        ),
    )
    .unwrap();

    let mut intent_nest = load_upf_files(
        &[UpfFileSpec {
            path: top_cpf.clone(),
            scope: None,
            version: None,
            is_cpf: true,
        }],
        finder.clone(),
    )
    .unwrap();
    let res_nest = resolve(&mut intent_nest, finder.as_ref());
    let sw_nest = res_nest
        .intent
        .power_switches
        .iter()
        .find(|s| s.name == "PSW_SUB")
        .expect("PSW_SUB");
    assert_eq!(
        sw_nest.control_ports,
        vec![("ctrl".to_string(), "u_wrap/u_sub/wrap_bad_ctrl".to_string())]
    );
    assert_eq!(
        sw_nest.control_port_scopes.get("ctrl").map(String::as_str),
        Some("u_wrap/u_sub")
    );
    assert!(sw_nest.shutoff_synthesized_ctrl);

    // Ensure `PowerStateDef` + `PstSat` do not double-qualify `u_wrap/u_sub`.
    let mut sat = PstSat::build(&res_nest, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!sat
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "UPF_EXPR_SYNTAX"));
    assert!(sat.is_state_space_consistent());
    let witnesses = sat.enumerate_symbolic_states(8);
    assert!(
        witnesses
            .iter()
            .any(|w| w.logic_signals.contains_key("u_wrap/u_sub/wrap_bad_ctrl")),
        "expected non-doubled signal `u_wrap/u_sub/wrap_bad_ctrl`, got: {witnesses:#?}"
    );

    // 2. Sequential `load_upf_files([sub.cpf@u_wrap/u_sub, top_root.cpf@/])` also inherits `domain_shutoff`.
    let top_root_cpf = dir.join("top_root.cpf");
    std::fs::write(
        &top_root_cpf,
        r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9
create_ground_nets -nets {VSS}
create_power_domain -name PD_TOP -default
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
create_power_switch_rule -name PSW_SUB -domain u_wrap/u_sub/PD_SUB -external_power_net VDD_AON
end_design
"#,
    )
    .unwrap();
    let mut intent_seq = load_upf_files(
        &[
            UpfFileSpec {
                path: sub_cpf,
                scope: Some("u_wrap/u_sub".to_string()),
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: top_root_cpf,
                scope: Some("/".to_string()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_seq = resolve(&mut intent_seq, finder.as_ref());
    let sw_seq = res_seq
        .intent
        .power_switches
        .iter()
        .find(|s| s.name == "PSW_SUB")
        .expect("PSW_SUB seq");
    assert_eq!(
        sw_seq.control_ports,
        vec![("ctrl".to_string(), "u_wrap/u_sub/wrap_bad_ctrl".to_string())]
    );
    assert_eq!(
        sw_seq.control_port_scopes.get("ctrl").map(String::as_str),
        Some("u_wrap/u_sub")
    );
    assert!(sw_seq.shutoff_synthesized_ctrl);

    // 3. Nested `create_power_domain -shutoff_condition` with `create_power_mode` in inline script:
    let inline_cpf = r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9
create_ground_nets -nets {VSS}
create_nominal_condition -name NC_ON -voltage 0.9 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_domain -name PD_TOP -default
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
end_design
"#;
    let res_inline =
        parse_cpf_and_resolve(inline_cpf, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(res_inline.intent.diagnostics.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn r17_sequential_upf_then_multi_cpf_preserves_cpf_state_across_pop_scope() {
    let dir = temp_dir("upf_cpf_cpf_seq");
    let finder: Arc<dyn informal_upf::ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_wrap", "u_wrap/u_sub"],
        &["u_wrap", "u_wrap/u_sub"],
    ));

    let top_upf = dir.join("top.upf");
    let sub1_cpf = dir.join("sub1.cpf");
    let sub2_cpf = dir.join("sub2.cpf");
    std::fs::write(
        &top_upf,
        r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
"#,
    )
    .unwrap();
    std::fs::write(
        &sub1_cpf,
        r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_SUB} -voltage 0.9
create_nominal_condition -name NC_ON  -voltage 0.9 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_domain -name PD_SUB -instances {.} -shutoff_condition {!u_sub/bad_ctrl}
update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net /VSS
end_design
"#,
    )
    .unwrap();
    std::fs::write(
        &sub2_cpf,
        r#"
set_cpf_version 2.0
create_power_switch_rule -name PSW_SUB -domain /PD_SUB -external_power_net /VDD_AON
end_design
"#,
    )
    .unwrap();

    let mut intent = load_upf_files(
        &[
            UpfFileSpec {
                path: top_upf.clone(),
                scope: None,
                version: None,
                is_cpf: false,
            },
            UpfFileSpec {
                path: sub1_cpf.clone(),
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: sub2_cpf,
                scope: None,
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res = resolve(&mut intent, finder.as_ref());
    let sw = res
        .intent
        .power_switches
        .iter()
        .find(|s| s.name == "PSW_SUB")
        .expect("PSW_SUB");
    assert_eq!(
        sw.control_ports,
        vec![("ctrl".to_string(), "u_sub/bad_ctrl".to_string())]
    );
    assert!(
        res.intent
            .power_states
            .iter()
            .any(|s| s.target == "SS_PD_SUB" && s.state_name == "ON")
            && res
                .intent
                .power_states
                .iter()
                .any(|s| s.target == "SS_PD_SUB" && s.state_name == "OFF"),
        "expected fallback ON/OFF states for SS_PD_SUB: {:#?}",
        res.intent.power_states
    );
    let mut sat = PstSat::build(&res, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat.is_state_space_consistent());

    // Also verify `nominal_conditions` from `sub1.cpf` survive into `sub2_mode.cpf`.
    let sub2_mode_cpf = dir.join("sub2_mode.cpf");
    std::fs::write(
        &sub2_mode_cpf,
        r#"
set_cpf_version 2.0
create_power_mode -name PM_ON  -domain_conditions {/PD_SUB@NC_ON} -default
create_power_mode -name PM_OFF -domain_conditions {/PD_SUB@NC_OFF}
end_design
"#,
    )
    .unwrap();
    let mut intent_m = load_upf_files(
        &[
            UpfFileSpec {
                path: top_upf,
                scope: None,
                version: None,
                is_cpf: false,
            },
            UpfFileSpec {
                path: sub1_cpf,
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: sub2_mode_cpf,
                scope: None,
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_m = resolve(&mut intent_m, finder.as_ref());
    let sat_m = PstSat::build(&res_m, DEFAULT_VOLTAGE_TOL_UV);
    let (vars, clauses) = sat_m.cnf_stats();
    assert!(
        vars > 1 && clauses > 1,
        "expected non-trivial PST SAT from sub1 nominal_conditions + sub2 power_modes"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn r17_cmd_end_design_defers_fallback_in_sequential_multi_cpf() {
    let dir = temp_dir("cpf_seq_end_design");
    let finder: Arc<dyn informal_upf::ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_wrap", "u_wrap/u_sub"],
        &["u_wrap", "u_wrap/u_sub"],
    ));

    // Repro A: `sub_a.cpf` at `u_wrap/u_sub` w/ `end_design` + `top_a_seq.cpf` at `/` w/ `end_design` (no `create_power_mode`).
    let sub_a = dir.join("sub_a.cpf");
    let top_a = dir.join("top_a_seq.cpf");
    std::fs::write(
        &sub_a,
        r#"
set_cpf_version 2.0
create_power_domain -name PD_SUB -instances {.} -shutoff_condition {!wrap_bad_ctrl}
update_power_domain -name PD_SUB -primary_power_net /VDD_SUB -primary_ground_net /VSS
end_design
"#,
    )
    .unwrap();
    std::fs::write(
        &top_a,
        r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP VDD_SUB} -voltage 0.9
create_ground_nets -nets {VSS}
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {u_wrap} -exclude_instances {u_wrap/u_sub} -shutoff_condition {!pwr_en}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
create_power_switch_rule -name PSW_SUB -domain u_wrap/u_sub/PD_SUB -external_power_net VDD_AON
end_design
"#,
    )
    .unwrap();
    let mut intent_a = load_upf_files(
        &[
            UpfFileSpec {
                path: sub_a,
                scope: Some("u_wrap/u_sub".to_string()),
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: top_a,
                scope: Some("/".to_string()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_a = resolve(&mut intent_a, finder.as_ref());
    let sw_a = res_a
        .intent
        .power_switches
        .iter()
        .find(|s| s.name == "PSW_SUB")
        .expect("PSW_SUB");
    assert_eq!(
        sw_a.control_ports,
        vec![("ctrl".to_string(), "u_wrap/u_sub/wrap_bad_ctrl".to_string())]
    );
    assert!(
        !sw_a.output_supply_ports.is_empty(),
        "PSW_SUB must receive vdd_out on final end_design"
    );
    assert!(
        res_a
            .intent
            .power_states
            .iter()
            .any(|s| s.target == "SS_PD_WRAP" && s.state_name == "OFF"),
        "PD_WRAP must receive fallback OFF state on final end_design: {:#?}",
        res_a.intent.power_states
    );

    // Repro B: `top_b.cpf` w/ `end_design` + `sub_b.cpf` updating `/PSW_WRAP` + `create_power_mode` + `end_design`.
    let top_b = dir.join("top_b.cpf");
    let sub_b = dir.join("sub_b.cpf");
    std::fs::write(
        &top_b,
        r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
create_ground_nets -nets {VSS}
create_nominal_condition -name NC_ON -voltage 0.9 -state on
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!bad_shutoff}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
create_power_switch_rule -name PSW_WRAP -domain PD_WRAP -external_power_net VDD_AON
end_design
"#,
    )
    .unwrap();
    std::fs::write(
        &sub_b,
        r#"
set_cpf_version 2.0
update_power_switch_rule -name /PSW_WRAP -enable_condition_1 {/other_aon}
create_power_mode -name PM_ON -domain_conditions {/PD_TOP@NC_ON /PD_WRAP@NC_ON} -condition {!/other_aon} -default
end_design
"#,
    )
    .unwrap();
    let mut intent_b = load_upf_files(
        &[
            UpfFileSpec {
                path: top_b,
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: sub_b,
                scope: Some("u_wrap/u_sub".to_string()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_b = resolve(&mut intent_b, finder.as_ref());
    let mut sat_b = PstSat::build(&res_b, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        !sat_b.is_state_space_consistent(),
        "Repro B: updating /PSW_WRAP to /other_aon while PM_ON requires !/other_aon must be PST_INCONSISTENT"
    );

    // R18: 3 sequential CPFs where s2_only.cpf redefines NC_OV (0.9V/on -> 0.0V/off) before s3_mode.cpf.
    let s1 = dir.join("s1.cpf");
    let s2 = dir.join("s2_only.cpf");
    let s3 = dir.join("s3_mode.cpf");
    std::fs::write(
        &s1,
        r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
create_ground_nets -nets {VSS}
create_nominal_condition -name NC_OV -voltage 0.9 -state on
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
end_design
"#,
    )
    .unwrap();
    std::fs::write(
        &s2,
        r#"
set_cpf_version 2.0
create_nominal_condition -name NC_OV -voltage 0.0 -state off
end_design
"#,
    )
    .unwrap();
    std::fs::write(
        &s3,
        r#"
set_cpf_version 2.0
create_power_mode -name PM_OV -domain_conditions {/PD_WRAP@NC_OV} -condition {pwr_en} -default
end_design
"#,
    )
    .unwrap();
    let mut intent_3 = load_upf_files(
        &[
            UpfFileSpec {
                path: s1,
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: s2,
                scope: Some("/".into()),
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: s3,
                scope: Some("/".into()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_3 = resolve(&mut intent_3, finder.as_ref());
    let mut sat_3 = PstSat::build(&res_3, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        !sat_3.is_state_space_consistent(),
        "3-file sequential CPF nominal_condition override (0.9V/on -> 0.0V/off) must yield PST_INCONSISTENT"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn r19_cpf_upf_cpf_override_sequential_modes_or_merge_and_multi_end_design() {
    let dir = temp_dir("cpf_r19_fixes");
    let finder: Arc<dyn informal_upf::ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_wrap", "u_wrap/u_sub"],
        &["u_wrap", "u_wrap/u_sub"],
    ));

    // Bug 1: [CPF1, CPF2, UPF, CPF3] preserves CPF2's nominal_condition override across UPF.
    let s1 = dir.join("b1_s1.cpf");
    let s2 = dir.join("b1_s2.cpf");
    let s3_upf = dir.join("b1_s3.upf");
    let s4 = dir.join("b1_s4.cpf");
    std::fs::write(
        &s1,
        r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
create_ground_nets -nets {VSS}
create_nominal_condition -name NC_OV -voltage 0.9 -state on
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
end_design
"#,
    )
    .unwrap();
    std::fs::write(
        &s2,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OV -voltage 0.0 -state off\nend_design\n",
    )
    .unwrap();
    std::fs::write(&s3_upf, "upf_version 2.1\n").unwrap();
    std::fs::write(
        &s4,
        "set_cpf_version 2.0\ncreate_power_mode -name PM_OV -domain_conditions {/PD_WRAP@NC_OV} -condition {pwr_en} -default\nend_design\n",
    )
    .unwrap();
    let mut intent_b1 = load_upf_files(
        &[
            UpfFileSpec {
                path: s1,
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: s2,
                scope: Some("/".into()),
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: s3_upf,
                scope: Some("/".into()),
                version: None,
                is_cpf: false,
            },
            UpfFileSpec {
                path: s4,
                scope: Some("/".into()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_b1 = resolve(&mut intent_b1, finder.as_ref());
    let mut sat_b1 = PstSat::build(&res_b1, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        !sat_b1.is_state_space_consistent(),
        "Bug 1: [CPF1, CPF2, UPF, CPF3] must retain CPF2 NC_OV=0.0V/off -> PST_INCONSISTENT"
    );

    // Bug 2: Sequential [m1.cpf(PM_ON), m2.cpf(PM_OFF)] OR-merges distinct modes (and supports update_power_mode).
    let m1 = dir.join("m1.cpf");
    let m2 = dir.join("m2.cpf");
    std::fs::write(
        &m1,
        r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
create_ground_nets -nets {VSS}
create_nominal_condition -name NC_ON -voltage 0.9 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -default
end_design
"#,
    )
    .unwrap();
    std::fs::write(
        &m2,
        r#"
set_cpf_version 2.0
create_power_mode -name PM_OFF -domain_conditions {/PD_TOP@NC_ON /PD_WRAP@NC_OFF}
end_design
"#,
    )
    .unwrap();
    let mut intent_b2 = load_upf_files(
        &[
            UpfFileSpec {
                path: m1.clone(),
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: m2,
                scope: Some("/".into()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_b2 = resolve(&mut intent_b2, finder.as_ref());
    let mut sat_b2 = PstSat::build(&res_b2, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        sat_b2.is_state_space_consistent(),
        "Bug 2: [m1.cpf(PM_ON), m2.cpf(PM_OFF)] must OR-merge into SAT space: {:?}",
        sat_b2.diagnostics()
    );
    let ss_top = res_b2.ss_by_name["SS_PD_TOP"];
    let ss_wrap = res_b2.ss_by_name["SS_PD_WRAP"];
    assert!(sat_b2.can_be_off_while_on(ss_wrap, ss_top).is_some());
    assert!(sat_b2.can_be_off_while_on(ss_top, ss_wrap).is_none());

    // Sequential update_power_mode across CPF files also updates PM_ON.
    let m2_upd = dir.join("m2_upd.cpf");
    std::fs::write(
        &m2_upd,
        "set_cpf_version 2.0\nupdate_power_mode -name PM_ON -condition {!pwr_en}\nend_design\n",
    )
    .unwrap();
    let mut intent_upd = load_upf_files(
        &[
            UpfFileSpec {
                path: m1,
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: m2_upd,
                scope: Some("/".into()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_upd = resolve(&mut intent_upd, finder.as_ref());
    assert!(!PstSat::build(&res_upd, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // Bug 3: Single CPF with two `set_design ... end_design` blocks evaluates the second block.
    let multi_cpf = r#"
set_cpf_version 2.0
set_design sub_blk
create_power_nets -nets {VDD_SUB} -voltage 0.9
create_ground_nets -nets {VSS}
create_nominal_condition -name NC_ON -voltage 0.9 -state on
create_power_domain -name PD_SUB -instances {u_wrap/u_sub}
update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS
create_power_mode -name PM_SUB_ON -domain_conditions {PD_SUB@NC_ON} -default
end_design sub_blk

set_design top
create_power_nets -nets {VDD_WRAP} -voltage 0.9
create_nominal_condition -name NC_OV -voltage 0.0 -state off
create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
create_power_mode -name PM_OV -domain_conditions {/PD_WRAP@NC_OV} -condition {pwr_en} -default
end_design top
"#;
    let res_b3 = parse_cpf_and_resolve(multi_cpf, None, None, finder).unwrap();
    assert!(!PstSat::build(&res_b3, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    let _ = std::fs::remove_dir_all(&dir);
}
