mod common;

use std::sync::Arc;

use common::{temp_dir, MockDesignFinder};
use informal_upf::{
    load_upf, load_upf_files, parse_cpf_and_resolve, resolve, PstSat, SimState, UpfFileSpec,
    DEFAULT_VOLTAGE_TOL_UV,
};

fn default_finder() -> Arc<dyn informal_upf::ObjectFinder> {
    Arc::new(MockDesignFinder::new(
        &["u_wrap", "u_wrap/u_sub"],
        &["u_wrap", "u_wrap/u_sub"],
    ))
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
fn fix1_sequential_update_illegal_and_legal_false_splice() {
    let dir = temp_dir("r23_fix1");
    let finder = default_finder();
    let c1 = dir.join("c1.cpf");
    std::fs::write(
        &c1,
        "set_cpf_version 2.0\n\
         create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9\n\
         create_ground_nets -nets {VSS}\n\
         create_power_domain -name PD_TOP -default\n\
         create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
         update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS\n\
         end_design\n",
    )
    .unwrap();

    for (tag, state_spec, expected_illegal_name) in [
        ("upd_illegal", "{OFF -update -illegal}", "OFF"),
        ("upd_legal_false", "{OFF -update -legal false}", "OFF"),
        ("on_upd_illegal", "{ON -update -illegal}", "ON"),
    ] {
        let u_seq = dir.join(format!("u_{tag}.upf"));
        std::fs::write(
            &u_seq,
            format!("upf_version 2.1\nadd_power_state SS_PD_WRAP -state {state_spec}\n"),
        )
        .unwrap();
        let top_upf = dir.join(format!("top_{tag}.upf"));
        std::fs::write(
            &top_upf,
            format!(
                "upf_version 2.1\nload_upf \"{}\" -format cpf\nload_upf \"{}\"\n",
                c1.display(),
                u_seq.display()
            ),
        )
        .unwrap();

        let mut intent_seq = load_upf_files(
            &[
                file_spec(c1.clone(), None, true),
                file_spec(u_seq, Some("/"), false),
            ],
            finder.clone(),
        )
        .unwrap();
        let res_seq = resolve(&mut intent_seq, finder.as_ref());
        let res_nested = load_upf(&top_upf, None, None, finder.clone()).unwrap();

        let wrap_seq = &res_seq.ss_states[res_seq.ss_by_name["SS_PD_WRAP"].0 as usize];
        let wrap_nested = &res_nested.ss_states[res_nested.ss_by_name["SS_PD_WRAP"].0 as usize];
        assert_eq!(
            wrap_seq.len(),
            2,
            "{tag}: expected 2 states (ON, OFF), got: {wrap_seq:#?}"
        );
        assert_eq!(wrap_seq.len(), wrap_nested.len());
        let ill_seq = wrap_seq
            .iter()
            .find(|s| s.state_name == expected_illegal_name)
            .unwrap();
        assert!(
            ill_seq.illegal,
            "{tag}: expected {expected_illegal_name} illegal=true: {ill_seq:#?}"
        );
        assert!(
            ill_seq.supply_expr.is_some() && ill_seq.logic_expr.is_some(),
            "{tag}: spliced fallback must carry supply/logic expr: {ill_seq:#?}"
        );

        let mut sat = PstSat::build(&res_seq, DEFAULT_VOLTAGE_TOL_UV);
        assert!(
            !sat.diagnostics()
                .iter()
                .any(|d| d.rule_id == "UPF_EXPR_SYNTAX"),
            "{tag}: unexpected UPF_EXPR_SYNTAX: {:?}",
            sat.diagnostics()
        );
        assert!(sat.is_state_space_consistent());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fix2_update_then_non_update_strips_stale_cpf_fallback_states() {
    let dir = temp_dir("r23_fix2");
    let finder = default_finder();
    let c1 = dir.join("c1.cpf");
    std::fs::write(
        &c1,
        "set_cpf_version 2.0\n\
         create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9\n\
         create_ground_nets -nets {VSS}\n\
         create_power_domain -name PD_TOP -default\n\
         create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
         update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS\n\
         end_design\n",
    )
    .unwrap();

    // 1. Multi-file sequential: [c1.cpf, u1.upf(ON -update), u2.upf(ONLY_OFF -logic_expr {0})]
    let u1 = dir.join("u1.upf");
    let u2 = dir.join("u2.upf");
    std::fs::write(
        &u1,
        "upf_version 2.1\nadd_power_state SS_PD_WRAP -state {ON -update -logic_expr {1'b1}}\n",
    )
    .unwrap();
    std::fs::write(
        &u2,
        "upf_version 2.1\nadd_power_state SS_PD_WRAP -state {ONLY_OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {0} -simstate CORRUPT}\n",
    )
    .unwrap();
    let mut intent_multi = load_upf_files(
        &[
            file_spec(c1.clone(), None, true),
            file_spec(u1.clone(), Some("/"), false),
            file_spec(u2.clone(), Some("/"), false),
        ],
        finder.clone(),
    )
    .unwrap();
    let res_multi = resolve(&mut intent_multi, finder.as_ref());
    let names_multi: Vec<&str> = res_multi.ss_states[res_multi.ss_by_name["SS_PD_WRAP"].0 as usize]
        .iter()
        .map(|s| s.state_name.as_str())
        .collect();
    assert_eq!(names_multi, vec!["ONLY_OFF"]);
    assert!(!PstSat::build(&res_multi, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 2. Single-UPF sequential: [c1.cpf, u_both.upf(ON -update; ONLY_OFF -logic_expr {0})]
    let u_both = dir.join("u_both.upf");
    std::fs::write(
        &u_both,
        "upf_version 2.1\n\
         add_power_state SS_PD_WRAP -state {ON -update -logic_expr {1'b1}}\n\
         add_power_state SS_PD_WRAP -state {ONLY_OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {0} -simstate CORRUPT}\n",
    )
    .unwrap();
    let mut intent_single = load_upf_files(
        &[
            file_spec(c1.clone(), None, true),
            file_spec(u_both.clone(), Some("/"), false),
        ],
        finder.clone(),
    )
    .unwrap();
    let res_single = resolve(&mut intent_single, finder.as_ref());
    let names_single: Vec<&str> = res_single.ss_states
        [res_single.ss_by_name["SS_PD_WRAP"].0 as usize]
        .iter()
        .map(|s| s.state_name.as_str())
        .collect();
    assert_eq!(names_single, vec!["ONLY_OFF"]);
    assert!(!PstSat::build(&res_single, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 3. Nested UPF parent loading c1.cpf (eager fallback emitted) + u1 + u2
    let top_nested = dir.join("top_nested.upf");
    std::fs::write(
        &top_nested,
        format!(
            "upf_version 2.1\nload_upf \"{}\" -format cpf\nload_upf \"{}\"\nload_upf \"{}\"\n",
            c1.display(),
            u1.display(),
            u2.display()
        ),
    )
    .unwrap();
    let res_nested = load_upf(&top_nested, None, None, finder.clone()).unwrap();
    let names_nested: Vec<&str> = res_nested.ss_states
        [res_nested.ss_by_name["SS_PD_WRAP"].0 as usize]
        .iter()
        .map(|s| s.state_name.as_str())
        .collect();
    assert_eq!(names_nested, vec!["ONLY_OFF"]);
    assert!(!PstSat::build(&res_nested, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 4. Non-regression: update-only keeps fallback ON/OFF intact.
    let mut intent_upd = load_upf_files(
        &[file_spec(c1, None, true), file_spec(u1, Some("/"), false)],
        finder.clone(),
    )
    .unwrap();
    let res_upd = resolve(&mut intent_upd, finder.as_ref());
    let wrap_upd = &res_upd.ss_states[res_upd.ss_by_name["SS_PD_WRAP"].0 as usize];
    assert!(wrap_upd
        .iter()
        .any(|s| s.state_name == "ON" && s.simstate == Some(SimState::Normal)));
    assert!(wrap_upd
        .iter()
        .any(|s| s.state_name == "OFF" && s.simstate == Some(SimState::Corrupt)));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fix3_same_scope_sibling_and_delegated_leaf_nc_override_vs_parent_wins() {
    let dir = temp_dir("r23_fix3");
    let finder = default_finder();

    let sub1 = dir.join("sub1.cpf");
    let sub2 = dir.join("sub2.cpf");
    std::fs::write(
        &sub1,
        "set_cpf_version 2.0\n\
         create_power_nets -nets {VDD_SUB} -voltage 0.9\n\
         create_ground_nets -nets {VSS}\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_power_domain -name PD_SUB -instances {.} -shutoff_condition {!en_sub}\n\
         update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
         create_power_mode -name PM_SUB -domain_conditions {PD_SUB@NC_ON} -condition {en_sub} -default\n\
         end_design\n",
    )
    .unwrap();
    std::fs::write(
        &sub2,
        "set_cpf_version 2.0\n\
         create_nominal_condition -name NC_ON -voltage 0.0 -state off\n\
         end_design\n",
    )
    .unwrap();

    // 1. Same-scope root siblings (parent.cpf loads sub1 + sub2 without -scope).
    let parent_root = format!(
        "set_cpf_version 2.0\nload_upf \"{}\" -format cpf\nload_upf \"{}\" -format cpf\n",
        sub1.display(),
        sub2.display()
    );
    let res_root = parse_cpf_and_resolve(&parent_root, None, None, finder.clone()).unwrap();
    assert!(!PstSat::build(&res_root, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 2. Same-scope siblings inside wrap.cpf loaded at -scope u_wrap.
    let wrap_cpf = dir.join("wrap.cpf");
    std::fs::write(
        &wrap_cpf,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf\nload_upf \"{}\" -format cpf\n",
            sub1.display(),
            sub2.display()
        ),
    )
    .unwrap();
    let root_wrap = format!(
        "set_cpf_version 2.0\nload_upf \"{}\" -format cpf -scope u_wrap\n",
        wrap_cpf.display()
    );
    let res_wrap = parse_cpf_and_resolve(&root_wrap, None, None, finder.clone()).unwrap();
    assert!(!PstSat::build(&res_wrap, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 3. Delegated leaf override: mid1(-scope u_wrap) + mid2(-scope u_wrap) -> load_upf leaf2.
    let mid1 = dir.join("mid1.cpf");
    let leaf2 = dir.join("leaf2.cpf");
    let mid2 = dir.join("mid2.cpf");
    std::fs::write(
        &mid1,
        "set_cpf_version 2.0\n\
         create_power_nets -nets {VDD_WRAP} -voltage 0.9\n\
         create_ground_nets -nets {VSS}\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_power_domain -name PD_WRAP -instances {.} -shutoff_condition {!en_wrap}\n\
         update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS\n\
         create_power_mode -name PM_WRAP -domain_conditions {PD_WRAP@NC_ON} -condition {en_wrap} -default\n\
         end_design\n",
    )
    .unwrap();
    std::fs::write(
        &leaf2,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_ON -voltage 0.0 -state off\nend_design\n",
    )
    .unwrap();
    std::fs::write(
        &mid2,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf\n",
            leaf2.display()
        ),
    )
    .unwrap();
    let root_deleg = format!(
        "set_cpf_version 2.0\nload_upf \"{}\" -format cpf -scope u_wrap\nload_upf \"{}\" -format cpf -scope u_wrap\n",
        mid1.display(),
        mid2.display()
    );
    let res_deleg = parse_cpf_and_resolve(&root_deleg, None, None, finder.clone()).unwrap();
    assert!(!PstSat::build(&res_deleg, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 4. Probe 5 parent-wins (same scope, without -scope) AND wrap.cpf authoring NC_OV while
    //    allowing sub2 to override sub1's NC_ON.
    let child_ov = dir.join("child_ov.cpf");
    std::fs::write(
        &child_ov,
        "set_cpf_version 2.0\n\
         create_nominal_condition -name NC_OV -voltage 0.0 -state off\n\
         create_nominal_condition -name NC_ON -voltage 0.0 -state off\n\
         end_design\n",
    )
    .unwrap();
    let probe5_same_scope = format!(
        "set_cpf_version 2.0\n\
         create_power_nets -nets {{VDD_TOP}} -voltage 0.9\n\
         create_ground_nets -nets {{VSS}}\n\
         create_nominal_condition -name NC_OV -voltage 0.9 -state on\n\
         create_power_domain -name PD_TOP -default -shutoff_condition {{!en_top}}\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\n\
         create_power_mode -name PM_TOP -domain_conditions {{PD_TOP@NC_OV}} -condition {{en_top}} -default\n\
         end_design\n\
         load_upf \"{}\" -format cpf\n",
        child_ov.display()
    );
    let res_p5 = parse_cpf_and_resolve(&probe5_same_scope, None, None, finder.clone()).unwrap();
    assert!(
        PstSat::build(&res_p5, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent(),
        "Probe 5 same-scope parent-authored NC_OV must win over child NC_OV=off"
    );

    let sub1_nc_on = dir.join("sub1_nc_on.cpf");
    std::fs::write(
        &sub1_nc_on,
        "set_cpf_version 2.0\n\
         create_power_nets -nets {VDD_SUB} -voltage 0.9\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_power_domain -name PD_SUB -instances {.} -shutoff_condition {!en_sub}\n\
         update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net /VSS\n\
         create_power_mode -name PM_SUB -domain_conditions {PD_SUB@NC_ON} -condition {en_sub} -default\n\
         end_design\n",
    )
    .unwrap();
    let wrap_mixed = dir.join("wrap_mixed.cpf");
    std::fs::write(
        &wrap_mixed,
        format!(
            "set_cpf_version 2.0\n\
             create_power_nets -nets {{VDD_W}} -voltage 0.9\n\
             create_ground_nets -nets {{VSS}}\n\
             create_nominal_condition -name NC_OV -voltage 0.9 -state on\n\
             create_power_domain -name PD_W -instances {{.}} -shutoff_condition {{!en_w}}\n\
             update_power_domain -name PD_W -primary_power_net VDD_W -primary_ground_net VSS\n\
             create_power_mode -name PM_W -domain_conditions {{PD_W@NC_OV}} -condition {{en_w}} -default\n\
             end_design\n\
             load_upf \"{}\" -format cpf\n\
             load_upf \"{}\" -format cpf\n",
            sub1_nc_on.display(),
            child_ov.display()
        ),
    )
    .unwrap();
    let root_mixed = format!(
        "set_cpf_version 2.0\nload_upf \"{}\" -format cpf -scope u_wrap\n",
        wrap_mixed.display()
    );
    let res_mixed = parse_cpf_and_resolve(&root_mixed, None, None, finder).unwrap();
    let st_w = res_mixed
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_u_wrap/PD_W" && s.state_name == "NC_OV")
        .unwrap();
    assert_eq!(st_w.simstate, Some(SimState::Normal));
    let st_sub = res_mixed
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_u_wrap/PD_SUB" && s.state_name == "NC_ON")
        .unwrap();
    assert_eq!(st_sub.simstate, Some(SimState::Corrupt));
    let mut sat_mixed = PstSat::build(&res_mixed, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_mixed.is_state_space_consistent());
    assert!(
        sat_mixed
            .diagnostics()
            .iter()
            .any(|d| d.rule_id == "PST_UNREACHABLE_STATE" && d.message.contains("PM_SUB")),
        "expected PM_SUB unreachable when sub2 overrides sub1's NC_ON to off: {:?}",
        sat_mixed.diagnostics()
    );
    assert!(
        !sat_mixed
            .diagnostics()
            .iter()
            .any(|d| d.rule_id == "PST_UNREACHABLE_STATE" && d.message.contains("PM_W")),
        "PM_W must remain reachable via parent-authored NC_OV=on: {:?}",
        sat_mixed.diagnostics()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn r24_fix_a_multi_domain_emitted_and_deferred_fallback_mix() {
    let dir = temp_dir("r24_fix_a");
    let finder = default_finder();
    let c1 = dir.join("c1.cpf");
    let c2 = dir.join("c2.cpf");
    let u_b1 = dir.join("u_b1.upf");
    let u_b2 = dir.join("u_b2.upf");
    let u_both = dir.join("u_both.upf");
    std::fs::write(
        &c1,
        "set_cpf_version 2.0\n\
         create_power_nets -nets {VDD_AON} -voltage 0.9\n\
         create_ground_nets -nets {VSS}\n\
         create_power_domain -name PD_A -default -shutoff_condition {!pwr_en}\n\
         update_power_domain -name PD_A -primary_power_net VDD_AON -primary_ground_net VSS\n\
         end_design\n",
    )
    .unwrap();
    std::fs::write(
        &c2,
        "set_cpf_version 2.0\n\
         create_power_nets -nets {VDD_B} -voltage 0.9\n\
         create_ground_nets -nets {VSS}\n\
         create_power_domain -name PD_B -instances {u_wrap} -shutoff_condition {!other_aon}\n\
         update_power_domain -name PD_B -primary_power_net VDD_B -primary_ground_net VSS\n\
         end_design\n",
    )
    .unwrap();
    let b1_src =
        "upf_version 2.1\nadd_power_state SS_PD_B -state {ON -update -logic_expr {1'b1}}\n";
    let b2_src = "upf_version 2.1\nadd_power_state SS_PD_B -state {ONLY_OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {0} -simstate CORRUPT}\n";
    std::fs::write(&u_b1, b1_src).unwrap();
    std::fs::write(&u_b2, b2_src).unwrap();
    std::fs::write(&u_both, format!("{b1_src}{b2_src}")).unwrap();

    let top_a = dir.join("top_a.upf");
    std::fs::write(
        &top_a,
        format!(
            "upf_version 2.1\nload_upf \"{}\" -format cpf\n",
            c1.display()
        ),
    )
    .unwrap();
    let parent_b = dir.join("parent_b.cpf");
    std::fs::write(
        &parent_b,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf\n",
            c2.display()
        ),
    )
    .unwrap();
    let top_nested = dir.join("top_nested.upf");
    std::fs::write(
        &top_nested,
        format!(
            "upf_version 2.1\nload_upf \"{}\" -format cpf\nload_upf \"{}\" -format cpf\nload_upf \"{}\"\n",
            c1.display(),
            parent_b.display(),
            u_both.display()
        ),
    )
    .unwrap();

    let res_nested = load_upf(&top_nested, None, None, finder.clone()).unwrap();
    let b_nested: Vec<&str> = res_nested.ss_states[res_nested.ss_by_name["SS_PD_B"].0 as usize]
        .iter()
        .map(|s| s.state_name.as_str())
        .collect();
    assert_eq!(b_nested, vec!["ONLY_OFF"]);
    assert!(!PstSat::build(&res_nested, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    let mut intent_seq = load_upf_files(
        &[
            file_spec(top_a.clone(), None, false),
            file_spec(c2.clone(), Some("/"), true),
            file_spec(u_both, Some("/"), false),
        ],
        finder.clone(),
    )
    .unwrap();
    let res_seq = resolve(&mut intent_seq, finder.as_ref());
    let b_seq: Vec<&str> = res_seq.ss_states[res_seq.ss_by_name["SS_PD_B"].0 as usize]
        .iter()
        .map(|s| s.state_name.as_str())
        .collect();
    assert_eq!(b_seq, vec!["ONLY_OFF"]);
    assert!(!PstSat::build(&res_seq, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    let mut intent_ch1 = load_upf_files(
        &[
            file_spec(top_a, None, false),
            file_spec(c2, Some("/"), true),
            file_spec(u_b1, Some("/"), false),
            file_spec(u_b2, Some("/"), false),
        ],
        finder.clone(),
    )
    .unwrap();
    let res_ch1 = resolve(&mut intent_ch1, finder.as_ref());
    let b_ch1: Vec<&str> = res_ch1.ss_states[res_ch1.ss_by_name["SS_PD_B"].0 as usize]
        .iter()
        .map(|s| s.state_name.as_str())
        .collect();
    assert_eq!(b_ch1, vec!["ONLY_OFF"]);
    assert!(!PstSat::build(&res_ch1, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn r24_fix_b_parent_wins_nc_refreshes_child_power_mode() {
    let dir = temp_dir("r24_fix_b");
    let finder = default_finder();
    let child = dir.join("child.cpf");
    std::fs::write(
        &child,
        "set_cpf_version 2.0\n\
         create_nominal_condition -name NC_OV -voltage 0.0 -state off\n\
         create_power_nets -nets {VDD_CHILD} -voltage 0.9\n\
         create_ground_nets -nets {VSS}\n\
         create_power_domain -name PD_CHILD -instances {u_wrap} -shutoff_condition {!other_aon}\n\
         update_power_domain -name PD_CHILD -primary_power_net VDD_CHILD -primary_ground_net VSS\n\
         create_power_mode -name PM_CHILD -domain_conditions {PD_CHILD@NC_OV} -condition {other_aon}\n\
         end_design\n",
    )
    .unwrap();

    let parent = format!(
        "set_cpf_version 2.0\n\
         create_power_nets -nets {{VDD_TOP}} -voltage 0.9\n\
         create_ground_nets -nets {{VSS}}\n\
         create_nominal_condition -name NC_OV -voltage 0.9 -state on\n\
         create_power_domain -name PD_TOP -default -shutoff_condition {{!pwr_en}}\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\n\
         create_power_mode -name PM_TOP -domain_conditions {{PD_TOP@NC_OV}} -condition {{pwr_en}} -default\n\
         end_design\n\
         load_upf \"{}\" -format cpf\n",
        child.display()
    );
    let res = parse_cpf_and_resolve(&parent, None, None, finder.clone()).unwrap();
    let st_child = res
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_PD_CHILD" && s.state_name == "NC_OV")
        .unwrap();
    assert_eq!(st_child.simstate, Some(SimState::Normal));
    let mut sat = PstSat::build(&res, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat.is_state_space_consistent());
    assert!(
        !sat.diagnostics()
            .iter()
            .any(|d| d.rule_id == "PST_UNREACHABLE_STATE"),
        "parent NC_OV=on must refresh PM_CHILD: {:?}",
        sat.diagnostics()
    );

    let mid = dir.join("mid.cpf");
    std::fs::write(
        &mid,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf\n",
            child.display()
        ),
    )
    .unwrap();
    let gp = format!(
        "set_cpf_version 2.0\n\
         create_power_nets -nets {{VDD_TOP}} -voltage 0.9\n\
         create_ground_nets -nets {{VSS}}\n\
         create_nominal_condition -name NC_OV -voltage 0.9 -state on\n\
         create_power_domain -name PD_TOP -default -shutoff_condition {{!pwr_en}}\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\n\
         create_power_mode -name PM_TOP -domain_conditions {{PD_TOP@NC_OV}} -condition {{pwr_en}} -default\n\
         end_design\n\
         load_upf \"{}\" -format cpf\n",
        mid.display()
    );
    let res_3lvl = parse_cpf_and_resolve(&gp, None, None, finder).unwrap();
    let st_3lvl = res_3lvl
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_PD_CHILD" && s.state_name == "NC_OV")
        .unwrap();
    assert_eq!(st_3lvl.simstate, Some(SimState::Normal));
    let mut sat_3lvl = PstSat::build(&res_3lvl, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_3lvl.is_state_space_consistent());
    assert!(
        !sat_3lvl
            .diagnostics()
            .iter()
            .any(|d| d.rule_id == "PST_UNREACHABLE_STATE"),
        "3-level gp->mid->child must refresh PM_CHILD: {:?}",
        sat_3lvl.diagnostics()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn r24_fix_c_seq_fallback_update_then_power_mode_matches_nested() {
    let dir = temp_dir("r24_fix_c");
    let finder = default_finder();
    let c1 = dir.join("c1.cpf");
    std::fs::write(
        &c1,
        "set_cpf_version 2.0\n\
         create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9\n\
         create_ground_nets -nets {VSS}\n\
         create_power_domain -name PD_TOP -default\n\
         create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
         update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS\n\
         end_design\n",
    )
    .unwrap();

    // Case 1: HYP1_NAMED_ON targeting PD_WRAP domain name
    let u_on = dir.join("u_on.upf");
    let c2_on = dir.join("c2_on.cpf");
    let top_on = dir.join("top_on.upf");
    std::fs::write(
        &u_on,
        "upf_version 2.1\nadd_power_state PD_WRAP -state {ON -update -logic_expr {1'b0}}\n",
    )
    .unwrap();
    std::fs::write(
        &c2_on,
        "set_cpf_version 2.0\n\
         create_nominal_condition -name ON -voltage 0.9 -state on\n\
         create_power_mode -name PM_W -domain_conditions {PD_TOP@ON PD_WRAP@ON} -condition {pwr_en} -default\n\
         end_design\n",
    )
    .unwrap();
    std::fs::write(
        &top_on,
        format!(
            "upf_version 2.1\nload_upf \"{}\" -format cpf\nload_upf \"{}\"\nload_upf \"{}\" -format cpf\n",
            c1.display(),
            u_on.display(),
            c2_on.display()
        ),
    )
    .unwrap();
    let mut seq_on = load_upf_files(
        &[
            file_spec(c1.clone(), None, true),
            file_spec(u_on, Some("/"), false),
            file_spec(c2_on, Some("/"), true),
        ],
        finder.clone(),
    )
    .unwrap();
    let res_seq_on = resolve(&mut seq_on, finder.as_ref());
    let res_nest_on = load_upf(&top_on, None, None, finder.clone()).unwrap();
    assert!(PstSat::build(&res_seq_on, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());
    assert!(PstSat::build(&res_nest_on, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // Case 2: HYP1_ILL_OFF_NAMED_OFF targeting PD_WRAP.primary
    let u_ill = dir.join("u_ill.upf");
    let c2_off = dir.join("c2_off.cpf");
    let top_off = dir.join("top_off.upf");
    std::fs::write(
        &u_ill,
        "upf_version 2.1\nadd_power_state PD_WRAP.primary -state {OFF -update -illegal}\n",
    )
    .unwrap();
    std::fs::write(
        &c2_off,
        "set_cpf_version 2.0\n\
         create_nominal_condition -name OFF -voltage 0.0 -state off\n\
         create_nominal_condition -name ON -voltage 0.9 -state on\n\
         create_power_mode -name PM_W -domain_conditions {PD_TOP@ON PD_WRAP@OFF} -condition {!pwr_en}\n\
         end_design\n",
    )
    .unwrap();
    std::fs::write(
        &top_off,
        format!(
            "upf_version 2.1\nload_upf \"{}\" -format cpf\nload_upf \"{}\"\nload_upf \"{}\" -format cpf\n",
            c1.display(),
            u_ill.display(),
            c2_off.display()
        ),
    )
    .unwrap();
    let mut seq_off = load_upf_files(
        &[
            file_spec(c1.clone(), None, true),
            file_spec(u_ill, Some("/"), false),
            file_spec(c2_off, Some("/"), true),
        ],
        finder.clone(),
    )
    .unwrap();
    let res_seq_off = resolve(&mut seq_off, finder.as_ref());
    let res_nest_off = load_upf(&top_off, None, None, finder.clone()).unwrap();
    assert_eq!(
        res_seq_off.ss_states[res_seq_off.ss_by_name["SS_PD_WRAP"].0 as usize].len(),
        res_nest_off.ss_states[res_nest_off.ss_by_name["SS_PD_WRAP"].0 as usize].len()
    );
    let mut sat_seq_off = PstSat::build(&res_seq_off, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_seq_off.is_state_space_consistent());
    assert!(
        sat_seq_off.diagnostics().is_empty(),
        "{:?}",
        sat_seq_off.diagnostics()
    );

    // Case 3: HYP1_CORRUPT targeting /SS_PD_WRAP
    let u_cor = dir.join("u_cor.upf");
    let c2_cor = dir.join("c2_cor.cpf");
    std::fs::write(
        &u_cor,
        "upf_version 2.1\nadd_power_state /SS_PD_WRAP -state {OFF -update -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {!pwr_en} -simstate CORRUPT}\n",
    )
    .unwrap();
    std::fs::write(
        &c2_cor,
        "set_cpf_version 2.0\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_power_mode -name PM_W -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -condition {pwr_en} -default\n\
         end_design\n",
    )
    .unwrap();
    let mut seq_cor = load_upf_files(
        &[
            file_spec(c1, None, true),
            file_spec(u_cor, Some("/"), false),
            file_spec(c2_cor, Some("/"), true),
        ],
        finder.clone(),
    )
    .unwrap();
    let res_seq_cor = resolve(&mut seq_cor, finder.as_ref());
    let wrap_cor: Vec<&str> = res_seq_cor.ss_states
        [res_seq_cor.ss_by_name["SS_PD_WRAP"].0 as usize]
        .iter()
        .map(|s| s.state_name.as_str())
        .collect();
    assert_eq!(wrap_cor, vec!["NC_ON"]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn r25_nomv_only_off_update_preserved_seq_matches_nested() {
    let dir = temp_dir("r25_nomv");
    let finder = default_finder();
    let c1 = dir.join("c1.cpf");
    std::fs::write(
        &c1,
        "set_cpf_version 2.0\n\
         create_power_nets -nets {VDD_TOP} -voltage 0.9\n\
         create_ground_nets -nets {VSS}\n\
         create_power_domain -name PD_TOP -default\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\n\
         end_design\n",
    )
    .unwrap();
    let c1_unk = dir.join("c1_unk.cpf");
    std::fs::write(
        &c1_unk,
        "set_cpf_version 2.0\ncreate_power_nets -nets {VDD_TOP}\ncreate_ground_nets -nets {VSS}\n\
         create_power_domain -name PD_TOP -default\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\nend_design\n",
    )
    .unwrap();
    let c2_nc = dir.join("c2_nc.cpf");
    std::fs::write(
        &c2_nc,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_power_mode -name PM_T -domain_conditions {PD_TOP@NC_ON} -condition {pwr_en} -default\nend_design\n",
    )
    .unwrap();
    let c2_off = dir.join("c2_off.cpf");
    std::fs::write(
        &c2_off,
        "set_cpf_version 2.0\ncreate_nominal_condition -name OFF -voltage 0.0 -state off\n\
         create_power_mode -name PM_T -domain_conditions {PD_TOP@OFF} -condition {!pwr_en}\nend_design\n",
    )
    .unwrap();

    for (tag, base, u_src, c2, exp_states, exp_syntax) in [
        ("off_upd", &c1, "upf_version 2.1\nadd_power_state SS_PD_TOP -state {OFF -update -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {!pwr_en} -simstate CORRUPT}\n", &c2_nc, vec!["OFF", "NC_ON"], false),
        ("off_ill", &c1, "upf_version 2.1\nadd_power_state SS_PD_TOP -state {OFF -update -illegal}\n", &c2_off, vec!["OFF"], false),
        ("on_upd", &c1, "upf_version 2.1\nadd_power_state SS_PD_TOP -state {ON -update -logic_expr {1'b0}}\n", &c2_nc, vec!["NC_ON"], false),
        ("unk_off", &c1_unk, "upf_version 2.1\nadd_power_state SS_PD_TOP -state {OFF -update -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {!pwr_en} -simstate CORRUPT}\n", &c2_nc, vec!["OFF", "NC_ON"], false),
    ] {
        let u = dir.join(format!("u_{tag}.upf"));
        let top = dir.join(format!("top_{tag}.upf"));
        std::fs::write(&u, u_src).unwrap();
        std::fs::write(&top, format!("upf_version 2.1\nload_upf \"{}\" -format cpf\nload_upf \"{}\"\nload_upf \"{}\" -format cpf\n", base.display(), u.display(), c2.display())).unwrap();
        let mut seq = load_upf_files(&[file_spec(base.clone(), None, true), file_spec(u, Some("/"), false), file_spec(c2.clone(), Some("/"), true)], finder.clone()).unwrap();
        let res_seq = resolve(&mut seq, finder.as_ref());
        let res_nest = load_upf(&top, None, None, finder.clone()).unwrap();
        let st_seq: Vec<&str> = res_seq.ss_states[res_seq.ss_by_name["SS_PD_TOP"].0 as usize].iter().map(|s| s.state_name.as_str()).collect();
        let st_nest: Vec<&str> = res_nest.ss_states[res_nest.ss_by_name["SS_PD_TOP"].0 as usize].iter().map(|s| s.state_name.as_str()).collect();
        assert_eq!(st_seq, exp_states, "{tag}");
        assert_eq!(st_seq, st_nest, "{tag}: seq != nest");
        let mut sat_seq = PstSat::build(&res_seq, DEFAULT_VOLTAGE_TOL_UV);
        let sat_nest = PstSat::build(&res_nest, DEFAULT_VOLTAGE_TOL_UV);
        assert!(sat_seq.is_state_space_consistent(), "{tag}: {:?}", sat_seq.diagnostics());
        assert_eq!(sat_seq.diagnostics().iter().any(|d| d.rule_id == "UPF_EXPR_SYNTAX"), exp_syntax, "{tag}");
        assert_eq!(sat_seq.diagnostics().len(), sat_nest.diagnostics().len(), "{tag}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
