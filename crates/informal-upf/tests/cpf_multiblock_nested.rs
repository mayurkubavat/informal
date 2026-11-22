mod common;

use std::sync::Arc;

use common::{temp_dir, MockDesignFinder};
use informal_upf::{
    load_upf, load_upf_files, parse_cpf_and_resolve, resolve, DiagSeverity, NullObjectFinder,
    PstSat, SimState, UpfFileSpec, VoltageSpec, DEFAULT_VOLTAGE_TOL_UV,
};

fn default_finder() -> Arc<dyn informal_upf::ObjectFinder> {
    Arc::new(MockDesignFinder::new(
        &["u_wrap", "u_wrap/u_sub"],
        &["u_wrap", "u_wrap/u_sub"],
    ))
}

#[test]
fn bug_a_distinct_set_design_blocks_keep_separate_pst_tables() {
    let finder = default_finder();
    let multi_cpf = r#"
        set_cpf_version 2.0
        set_design blk_a
        create_power_nets -nets {VDD_A VDD_B} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_A -instances {u_wrap/u_sub}
        update_power_domain -name PD_A -primary_power_net VDD_A -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {PD_A@NC_ON} -condition {en_a} -default
        end_design blk_a

        set_design top
        create_power_domain -name PD_B -instances {u_wrap}
        update_power_domain -name PD_B -primary_power_net VDD_B -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {PD_A@NC_ON PD_B@NC_ON} -condition {!en_a} -default
        end_design top
    "#;
    let res = parse_cpf_and_resolve(multi_cpf, None, None, finder.clone()).unwrap();
    let pst_names: Vec<&str> = res
        .intent
        .pst_tables
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    assert!(
        pst_names.contains(&"cpf_power_modes::blk_a")
            && pst_names.contains(&"cpf_power_modes::top"),
        "expected separate PST tables for blk_a and top, got: {pst_names:?}"
    );
    let mut sat = PstSat::build(&res, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        !sat.is_state_space_consistent(),
        "conjoining blk_a(en_a) and top(!en_a) must yield PST_INCONSISTENT"
    );

    // Non-regression: reopening the same `top` merges into 1 PST table without duplicates.
    let reopen_cpf = r#"
        set_cpf_version 2.0
        set_design top
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -default
        end_design top

        set_design top
        create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_OFF}
        end_design top
    "#;
    let res_reopen = parse_cpf_and_resolve(reopen_cpf, None, None, finder).unwrap();
    let top_tables: Vec<_> = res_reopen
        .intent
        .pst_tables
        .iter()
        .filter(|t| t.name == "cpf_power_modes::top")
        .collect();
    assert_eq!(top_tables.len(), 1);
    assert_eq!(top_tables[0].rows.len(), 2);
    let mut sat_reopen = PstSat::build(&res_reopen, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_reopen.is_state_space_consistent());
}

#[test]
fn bug_b_post_end_design_switch_rule_refresh_seq_and_multiblock() {
    let dir = temp_dir("bug_b_psw");
    let finder = default_finder();

    // 1. Sequential [m1.cpf(PM_ON + PSW_WRAP(pwr_en) + end_design), m2.cpf(update_power_switch_rule !pwr_en)]
    let m1 = dir.join("m1.cpf");
    let m2 = dir.join("m2.cpf");
    std::fs::write(
        &m1,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_WRAP -instances {u_wrap}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_switch_rule -name PSW_WRAP -domain PD_WRAP -external_power_net VDD_AON -enable_condition {pwr_en}
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -condition {pwr_en} -default
        end_design
    "#,
    )
    .unwrap();
    std::fs::write(
        &m2,
        "set_cpf_version 2.0\nupdate_power_switch_rule -name /PSW_WRAP -enable_condition {!pwr_en}\nend_design\n",
    )
    .unwrap();

    let mut intent_seq = load_upf_files(
        &[
            UpfFileSpec {
                path: m1,
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
    let res_seq = resolve(&mut intent_seq, finder.as_ref());
    let wrap_on = res_seq
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_PD_WRAP" && s.state_name == "NC_ON")
        .expect("SS_PD_WRAP NC_ON");
    assert_eq!(wrap_on.logic_expr.as_deref(), Some("!pwr_en"));
    assert!(!PstSat::build(&res_seq, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 2. Multi-block single CPF: Block 1 emits PM_ON, Block 2 updates PSW_WRAP -> PST_INCONSISTENT.
    let multi_psw = r#"
        set_cpf_version 2.0
        set_design blk
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_WRAP -instances {u_wrap}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_switch_rule -name PSW_WRAP -domain PD_WRAP -external_power_net VDD_AON -enable_condition {pwr_en}
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -condition {pwr_en} -default
        end_design blk

        set_design top
        update_power_switch_rule -name /PSW_WRAP -enable_condition {!pwr_en}
        end_design top
    "#;
    let res_multi = parse_cpf_and_resolve(multi_psw, None, None, finder.clone()).unwrap();
    assert!(!PstSat::build(&res_multi, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 3. Post-PM `create_power_switch_rule` on a domain initially lacking `-shutoff_condition`.
    let late_create_psw = r#"
        set_cpf_version 2.0
        set_design blk
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_WRAP -instances {u_wrap}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -condition {pwr_en} -default
        end_design blk

        set_design top
        create_power_switch_rule -name PSW_WRAP -domain PD_WRAP -external_power_net VDD_AON -enable_condition {!pwr_en}
        end_design top
    "#;
    let res_late_create = parse_cpf_and_resolve(late_create_psw, None, None, finder).unwrap();
    assert!(!PstSat::build(&res_late_create, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bug_b_late_nominal_condition_and_illegal_mode_refresh() {
    let dir = temp_dir("bug_b_nc");
    let finder = default_finder();

    let c1 = dir.join("c1.cpf");
    let u = dir.join("u.upf");
    let c2 = dir.join("c2.cpf");
    std::fs::write(
        &c1,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -condition {pwr_en} -default
        end_design
    "#,
    )
    .unwrap();
    std::fs::write(&u, "upf_version 2.1\n").unwrap();
    std::fs::write(
        &c2,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_ON -voltage 0.0 -state off\nend_design\n",
    )
    .unwrap();

    // 1. [CPF1(PM_ON), UPF, CPF2(NC_ON=off)] -> PST_INCONSISTENT + updated PowerStateDef
    let mut intent_1 = load_upf_files(
        &[
            UpfFileSpec {
                path: c1.clone(),
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: u,
                scope: Some("/".into()),
                version: None,
                is_cpf: false,
            },
            UpfFileSpec {
                path: c2.clone(),
                scope: Some("/".into()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_1 = resolve(&mut intent_1, finder.as_ref());
    let st_wrap = res_1
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_PD_WRAP" && s.state_name == "NC_ON")
        .unwrap();
    assert_eq!(st_wrap.simstate, Some(SimState::Corrupt));
    assert_eq!(st_wrap.rail_states[0].voltage, VoltageSpec::Off);
    assert!(!PstSat::build(&res_1, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 2. [CPF1(PM_ON), CPF2(NC_ON=off)] -> PST_INCONSISTENT
    let mut intent_2 = load_upf_files(
        &[
            UpfFileSpec {
                path: c1,
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: c2,
                scope: Some("/".into()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_2 = resolve(&mut intent_2, finder.as_ref());
    assert!(!PstSat::build(&res_2, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 3. Reopened `set_design top` Block 2 overrides `NC_ON` after Block 1 emitted `PM_ON`.
    let reopen_nc = r#"
        set_cpf_version 2.0
        set_design top
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -condition {pwr_en} -default
        end_design top

        set_design top
        create_nominal_condition -name NC_ON -voltage 0.0 -state off
        end_design top
    "#;
    let res_3 = parse_cpf_and_resolve(reopen_nc, None, None, finder.clone()).unwrap();
    assert!(!PstSat::build(&res_3, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 4. Already-emitted `-illegal` mode refreshes on late `create_nominal_condition` / `update_power_switch_rule`.
    let ill_refresh = r#"
        set_cpf_version 2.0
        set_design top
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_nominal_condition -name NC_CHK -voltage 0.9 -state on
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -condition {pwr_en} -default
        create_power_mode -name PM_ILL -domain_conditions {PD_WRAP@NC_CHK} -illegal
        end_design top

        set_design top
        create_nominal_condition -name NC_CHK -voltage 0.0 -state off
        end_design top
    "#;
    let res_ill = parse_cpf_and_resolve(ill_refresh, None, None, finder.clone()).unwrap();
    assert_eq!(res_ill.intent.illegal_combinations.len(), 1);
    assert_eq!(
        res_ill.intent.illegal_combinations[0]
            .fallback_logic_expr
            .as_deref(),
        Some("(!/pwr_en)")
    );
    assert!(PstSat::build(&res_ill, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // Also verify `update_power_switch_rule` refreshes an already-emitted `-illegal` mode.
    let ill_psw_refresh = r#"
        set_cpf_version 2.0
        set_design top
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_switch_rule -name PSW_WRAP -domain PD_WRAP -external_power_net VDD_AON -enable_condition {pwr_en}
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -condition {pwr_en} -default
        create_power_mode -name PM_ILL -domain_conditions {PD_WRAP@NC_ON} -illegal
        end_design top

        set_design top
        update_power_switch_rule -name /PSW_WRAP -enable_condition {!pwr_en}
        end_design top
    "#;
    let res_ill_psw = parse_cpf_and_resolve(ill_psw_refresh, None, None, finder).unwrap();
    assert_eq!(res_ill_psw.intent.illegal_combinations.len(), 1);
    assert_eq!(
        res_ill_psw.intent.illegal_combinations[0]
            .fallback_logic_expr
            .as_deref(),
        Some("(!/pwr_en)")
    );
    assert!(!PstSat::build(&res_ill_psw, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bug_c_nested_load_upf_multi_cpf_and_parent_wins() {
    let dir = temp_dir("bug_c_nested");
    let finder = default_finder();

    // 1. `hyp3b_positive_valid_pm_on`: top.upf loads m1.cpf (end_design, no PM) + m2.cpf (PM_ON).
    let m1 = dir.join("m1.cpf");
    let m2 = dir.join("m2.cpf");
    let top_upf = dir.join("top_b.upf");
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
        end_design
    "#,
    )
    .unwrap();
    std::fs::write(
        &m2,
        "set_cpf_version 2.0\ncreate_power_mode -name PM_ON -domain_conditions {/PD_TOP@NC_ON /PD_WRAP@NC_ON} -default\nend_design\n",
    )
    .unwrap();
    std::fs::write(
        &top_upf,
        format!(
            "upf_version 2.1\nload_upf {} -format cpf -scope /\nload_upf {} -format cpf -scope /\n",
            m1.display(),
            m2.display()
        ),
    )
    .unwrap();
    let res_b = load_upf(&top_upf, None, None, finder.clone()).unwrap();
    let wrap_states: Vec<&str> = res_b
        .intent
        .power_states
        .iter()
        .filter(|s| s.target == "SS_PD_WRAP")
        .map(|s| s.state_name.as_str())
        .collect();
    assert_eq!(
        wrap_states,
        vec!["NC_ON"],
        "premature fallback ON/OFF must be stripped once m2.cpf declares PM_ON"
    );
    let mut sat_b = PstSat::build(&res_b, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_b.is_state_space_consistent());
    assert!(!sat_b
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "PST_UNREACHABLE_STATE"));

    // 2. `hyp3_nested_rtl_crossing`: top.upf loads m1(PM_ON) + m2(PM_OFF) -> PM_OFF reachable.
    let m1_on = dir.join("m1_on.cpf");
    let m2_off = dir.join("m2_off.cpf");
    let top_cross = dir.join("top_cross.upf");
    std::fs::write(
        &m1_on,
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
        &m2_off,
        "set_cpf_version 2.0\ncreate_power_mode -name PM_OFF -domain_conditions {/PD_TOP@NC_ON /PD_WRAP@NC_OFF}\nend_design\n",
    )
    .unwrap();
    std::fs::write(
        &top_cross,
        format!(
            "upf_version 2.1\nload_upf {} -format cpf -scope /\nload_upf {} -format cpf -scope /\n",
            m1_on.display(),
            m2_off.display()
        ),
    )
    .unwrap();
    let res_cross = load_upf(&top_cross, None, None, finder.clone()).unwrap();
    let mut sat_cross = PstSat::build(&res_cross, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_cross.is_state_space_consistent());
    assert!(!sat_cross
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "PST_UNREACHABLE_STATE"));
    let ss_top = res_cross.ss_by_name["SS_PD_TOP"];
    let ss_wrap = res_cross.ss_by_name["SS_PD_WRAP"];
    assert!(sat_cross.can_be_off_while_on(ss_wrap, ss_top).is_some());

    // 3. Parent-CPF -> child-CPF preserves parent-wins `nominal_conditions` (`Probe 5`).
    let child_cpf = dir.join("child_pw.cpf");
    std::fs::write(
        &child_cpf,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OV -voltage 0.0 -state off\nend_design\n",
    )
    .unwrap();
    let parent_cpf = format!(
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {{VDD_AON VDD_WRAP}} -voltage 0.9
        create_ground_nets -nets {{VSS}}
        create_nominal_condition -name NC_OV -voltage 0.9 -state on
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_WRAP -instances {{u_wrap}} -shutoff_condition {{!pwr_en}}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_mode -name PM_PRE -domain_conditions {{PD_WRAP@NC_OV}} -condition {{pwr_en}} -default
        end_design
        load_upf "{}" -format cpf -scope u_wrap
        create_power_mode -name PM_POST -domain_conditions {{PD_WRAP@NC_OV}} -condition {{pwr_en}}
        end_design
    "#,
        child_cpf.display()
    );
    let res_pw = parse_cpf_and_resolve(&parent_cpf, None, None, finder).unwrap();
    assert!(PstSat::build(&res_pw, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bug_d_nested_set_design_stack_sat_unsat_and_mismatch() {
    let finder = default_finder();

    // 1. SAT nested `set_design top ... set_design sub_blk ... end_design sub_blk ... end_design top`
    let nested_sat = r#"
        set_cpf_version 2.0
        set_design top
        create_power_nets -nets {VDD_AON VDD_WRAP VDD_SUB} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off
        create_power_domain -name PD_TOP -default

        set_design sub_blk
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub} -shutoff_condition {!u_wrap/u_sub/wrap_bad_ctrl}
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS
        end_design sub_blk

        create_power_domain -name PD_WRAP -instances {u_wrap} -exclude_instances {u_wrap/u_sub} -shutoff_condition {!pwr_en}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -default
        create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_OFF}
        end_design top
    "#;
    let res_sat = parse_cpf_and_resolve(nested_sat, None, None, finder.clone()).unwrap();
    assert!(!res_sat
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));
    assert_eq!(res_sat.intent.design_top.as_deref(), Some("top"));
    assert!(
        res_sat
            .intent
            .pst_tables
            .iter()
            .any(|t| t.name == "cpf_power_modes::top" && t.rows.len() == 2),
        "post-inner-end_design modes must be attributed to `top`: {:#?}",
        res_sat.intent.pst_tables
    );
    let mut sat = PstSat::build(&res_sat, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat.is_state_space_consistent());

    // 2. UNSAT nested `set_design`: inner sub_blk contradicts outer top.
    let nested_unsat = r#"
        set_cpf_version 2.0
        set_design top
        create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_TOP -default

        set_design sub_blk
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub}
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS
        create_power_mode -name PM_SUB -domain_conditions {PD_SUB@NC_ON} -condition {en_sub} -default
        end_design sub_blk

        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        create_power_mode -name PM_TOP -domain_conditions {PD_TOP@NC_ON} -condition {!en_sub} -default
        end_design top
    "#;
    let res_unsat = parse_cpf_and_resolve(nested_unsat, None, None, finder).unwrap();
    assert!(!PstSat::build(&res_unsat, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 3. Mismatched `end_design top` while inside `sub_blk` must still fail loudly.
    let bad_nested = r#"
        set_cpf_version 2.0
        set_design top
        set_design sub_blk
        end_design top
    "#;
    let err =
        parse_cpf_and_resolve(bad_nested, None, None, Arc::new(NullObjectFinder)).unwrap_err();
    assert!(err.to_string().contains("does not match set_design"));
}

fn file_spec(path: std::path::PathBuf, scope: Option<&str>, is_cpf: bool) -> UpfFileSpec {
    UpfFileSpec {
        path,
        scope: scope.map(str::to_string),
        version: None,
        is_cpf,
    }
}

#[test]
fn adv_x1_update_power_mode_scope_and_design_collision() {
    let dir = temp_dir("adv_x1");
    let finder = default_finder();

    // 1. Cross-scope collision (`adv_x1`): `m3.cpf -scope u_wrap` updates u_wrap's PM_ILL only; root PM_ILL stays illegal.
    let m1 = dir.join("m1.cpf");
    let m2 = dir.join("m2.cpf");
    let m3 = dir.join("m3.cpf");
    std::fs::write(
        &m1,
        "set_cpf_version 2.0\ncreate_power_nets -nets {VDD_W} -voltage 0.9\ncreate_ground_nets -nets {VSS}\ncreate_nominal_condition -name NC_ON -voltage 0.9 -state on\ncreate_power_domain -name PD_WRAP -instances {.} -shutoff_condition {!en_wrap}\nupdate_power_domain -name PD_WRAP -primary_power_net VDD_W -primary_ground_net VSS\ncreate_power_mode -name PM_WRAP -domain_conditions {PD_WRAP@NC_ON} -condition {en_wrap} -default\ncreate_power_mode -name PM_ILL -domain_conditions {PD_WRAP@NC_ON} -condition {!en_wrap} -illegal\nend_design\n",
    )
    .unwrap();
    std::fs::write(
        &m2,
        "set_cpf_version 2.0\ncreate_power_nets -nets {VDD_T} -voltage 0.9\ncreate_nominal_condition -name NC_ON -voltage 0.9 -state on\ncreate_power_domain -name PD_TOP -default\nupdate_power_domain -name PD_TOP -primary_power_net VDD_T -primary_ground_net VSS\ncreate_power_mode -name PM_TOP -domain_conditions {PD_TOP@NC_ON} -condition {en_top} -default\ncreate_power_mode -name PM_ILL -domain_conditions {PD_TOP@NC_ON} -condition {en_top} -illegal\nend_design\n",
    )
    .unwrap();
    std::fs::write(
        &m3,
        "set_cpf_version 2.0\nupdate_power_mode -name PM_ILL -illegal false\nend_design\n",
    )
    .unwrap();
    let mut intent_x1 = load_upf_files(
        &[
            file_spec(m1, Some("u_wrap"), true),
            file_spec(m2, Some("/"), true),
            file_spec(m3, Some("u_wrap"), true),
        ],
        finder.clone(),
    )
    .unwrap();
    let res_x1 = resolve(&mut intent_x1, finder.as_ref());
    assert!(
        !PstSat::build(&res_x1, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent(),
        "root PM_ILL must remain illegal when u_wrap PM_ILL is cleared"
    );

    // 2. Cross-design collision (`adv_x1b`): updating blk_a's PM_ILL must not clear blk_b's PM_ILL.
    let single_x1b = r#"set_cpf_version 2.0
set_design blk_a
create_power_nets -nets {VDD_A VDD_B} -voltage 0.9
create_ground_nets -nets {VSS}
create_nominal_condition -name NC_ON -voltage 0.9 -state on
create_power_domain -name PD_A -instances {u_wrap/u_sub} -shutoff_condition {!en_a}
update_power_domain -name PD_A -primary_power_net VDD_A -primary_ground_net VSS
create_power_mode -name PM_A -domain_conditions {PD_A@NC_ON} -condition {en_a} -default
create_power_mode -name PM_ILL -domain_conditions {PD_A@NC_ON} -condition {!en_a} -illegal
end_design blk_a

set_design blk_b
create_power_domain -name PD_B -instances {u_wrap} -exclude_instances {u_wrap/u_sub} -shutoff_condition {!en_b}
update_power_domain -name PD_B -primary_power_net VDD_B -primary_ground_net VSS
create_power_mode -name PM_B -domain_conditions {PD_B@NC_ON} -condition {en_b} -default
create_power_mode -name PM_ILL -domain_conditions {PD_B@NC_ON} -condition {en_b} -illegal
end_design blk_b

set_design blk_a
update_power_mode -name PM_ILL -illegal false
end_design blk_a
"#;
    let res_x1b = parse_cpf_and_resolve(single_x1b, None, None, finder).unwrap();
    assert!(
        !PstSat::build(&res_x1b, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent(),
        "blk_b PM_ILL must remain illegal when blk_a PM_ILL is cleared"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn adv_x2_nested_sibling_cpf_nc_override_refreshes_pm_sub() {
    let dir = temp_dir("adv_x2");
    let finder = default_finder();

    let sub1_a = dir.join("sub1_a.cpf");
    let sub1_b = dir.join("sub1_b.cpf");
    let sub2_a = dir.join("sub2_a.cpf");
    std::fs::write(
        &sub1_a,
        "set_cpf_version 2.0\ncreate_power_nets -nets {VDD_SUB} -voltage 0.9\ncreate_power_domain -name PD_SUB -instances {.} -shutoff_condition {!en_sub}\nupdate_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net /VSS\ncreate_power_mode -name PM_SUB -domain_conditions {PD_SUB@NC_ON} -condition {en_sub} -default\nend_design\n",
    )
    .unwrap();
    std::fs::write(
        &sub1_b,
        "set_cpf_version 2.0\ncreate_power_nets -nets {VDD_SUB} -voltage 0.9\ncreate_nominal_condition -name NC_ON -voltage 0.9 -state on\ncreate_power_domain -name PD_SUB -instances {.} -shutoff_condition {!en_sub}\nupdate_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net /VSS\ncreate_power_mode -name PM_SUB -domain_conditions {PD_SUB@NC_ON} -condition {en_sub} -default\nend_design\n",
    )
    .unwrap();
    std::fs::write(
        &sub2_a,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_ON -voltage 0.0 -state off\nend_design\n",
    )
    .unwrap();
    for (tag, s1) in [("adv_x2", &sub1_a), ("adv_x2b", &sub1_b)] {
        let parent = format!(
            "set_cpf_version 2.0\ncreate_power_nets -nets {{VDD_TOP}} -voltage 0.9\ncreate_ground_nets -nets {{VSS}}\ncreate_nominal_condition -name NC_ON -voltage 0.9 -state on\ncreate_power_domain -name PD_TOP -default\nupdate_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\ncreate_power_mode -name PM_TOP -domain_conditions {{PD_TOP@NC_ON}} -condition {{en_top}} -default\nend_design\nload_upf \"{}\" -format cpf -scope u_wrap\nload_upf \"{}\" -format cpf -scope u_wrap\n",
            s1.display(),
            sub2_a.display()
        );
        let res = parse_cpf_and_resolve(&parent, None, None, finder.clone()).unwrap();
        assert!(
            !PstSat::build(&res, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent(),
            "{tag}: sub2.cpf NC_ON=off at u_wrap must refresh PM_SUB -> PST_INCONSISTENT"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn adv_fb2_sequential_cpf_fallback_with_upf_add_power_state() {
    let dir = temp_dir("adv_fb2");
    let finder = default_finder();
    let c1 = dir.join("c1.cpf");
    std::fs::write(
        &c1,
        "set_cpf_version 2.0\ncreate_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9\ncreate_ground_nets -nets {VSS}\ncreate_power_domain -name PD_TOP -default\ncreate_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}\nupdate_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\nupdate_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS\nend_design\n",
    )
    .unwrap();

    // Bug 1: `-update` in sequential UPF keeps fallback ON/OFF and updates ON before -update applies.
    let u_upd = dir.join("u_upd.upf");
    std::fs::write(
        &u_upd,
        "upf_version 2.1\nadd_power_state SS_PD_WRAP -state {ON -update -logic_expr {pwr_en && other_aon}}\n",
    )
    .unwrap();
    let mut i_upd = load_upf_files(
        &[
            file_spec(c1.clone(), None, true),
            file_spec(u_upd, Some("/"), false),
        ],
        finder.clone(),
    )
    .unwrap();
    let r_upd = resolve(&mut i_upd, finder.as_ref());
    let wrap_states = &r_upd.ss_states[r_upd.ss_by_name["SS_PD_WRAP"].0 as usize];
    assert!(
        wrap_states.iter().any(|s| s.state_name == "ON"
            && s.simstate == Some(SimState::Normal)
            && s.logic_expr.as_deref() == Some("pwr_en && other_aon")),
        "expected updated fallback ON state: {wrap_states:#?}"
    );
    assert!(
        wrap_states
            .iter()
            .any(|s| s.state_name == "OFF" && s.simstate == Some(SimState::Corrupt)),
        "expected fallback OFF state preserved alongside -update: {wrap_states:#?}"
    );

    // Bug 2: domain / `.primary` / `/SS_*` targets in sequential UPF suppress fallback ON/OFF.
    for (tag, target) in [
        ("pd", "PD_WRAP"),
        ("pd_prim", "PD_WRAP.primary"),
        ("abs_ss", "/SS_PD_WRAP"),
    ] {
        let u_add = dir.join(format!("u_{tag}.upf"));
        std::fs::write(
            &u_add,
            format!("upf_version 2.1\nadd_power_state {target} -state {{RUN -supply_expr {{power == {{FULL_ON 0.9}} && ground == {{FULL_ON 0.0}}}} -logic_expr {{1'b1}} -simstate NORMAL}}\n"),
        )
        .unwrap();
        let mut i_add = load_upf_files(
            &[
                file_spec(c1.clone(), None, true),
                file_spec(u_add, Some("/"), false),
            ],
            finder.clone(),
        )
        .unwrap();
        let r_add = resolve(&mut i_add, finder.as_ref());
        let names: Vec<&str> = r_add.ss_states[r_add.ss_by_name["SS_PD_WRAP"].0 as usize]
            .iter()
            .map(|s| s.state_name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["RUN"],
            "target `{target}` must strip fallback ON/OFF"
        );
    }

    // Bug 3: `-illegal`-only add_power_state preserves fallback ON/OFF so PST remains SAT.
    let u_ill = dir.join("u_ill.upf");
    std::fs::write(
        &u_ill,
        "upf_version 2.1\nadd_power_state SS_PD_WRAP -state {BAD -logic_expr {1'b0} -illegal}\n",
    )
    .unwrap();
    let mut i_ill = load_upf_files(
        &[
            file_spec(c1, None, true),
            file_spec(u_ill, Some("/"), false),
        ],
        finder.clone(),
    )
    .unwrap();
    let r_ill = resolve(&mut i_ill, finder.as_ref());
    assert!(PstSat::build(&r_ill, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());
    let _ = std::fs::remove_dir_all(&dir);
}
