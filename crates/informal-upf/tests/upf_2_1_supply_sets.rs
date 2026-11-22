mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::temp_dir;
use informal_upf::{
    parse_upf_and_resolve, DiagSeverity, LsRequirement, NullObjectFinder, PstSat, SimState,
    VoltageSpec, DEFAULT_VOLTAGE_TOL_UV,
};

#[test]
fn implicit_iso_ret_supply_bridging_and_set_equivalent_function_only() {
    let upf_bridge_equiv = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_1
        create_supply_net VDD_2
        create_supply_net VSS_1
        create_supply_net VSS_2
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS_1}
        create_supply_set SS_1   -function {power VDD_1}   -function {ground VSS_1}
        create_supply_set SS_2   -function {power VDD_2}   -function {ground VSS_2}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_1   -elements {u_1} -supply {primary SS_1}
        create_power_domain PD_2   -elements {u_2} -supply {primary SS_2}
        add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_1   -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_1   -state {OFF -supply_expr {power == OFF           && ground == {FULL_ON 0.0}} -simstate CORRUPT}
        add_power_state SS_2   -state {ON  -supply_expr {power == {FULL_ON 1.8} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        set_equivalent -nets {VSS_1 VSS_2}
        set_isolation iso1 -domain PD_1 -isolation_power_net VDD_AON -isolation_ground_net VSS_1 -clamp_value 0
        set_retention ret1 -domain PD_1 -retention_power_net VDD_AON
    "#;
    let res_be =
        parse_upf_and_resolve(upf_bridge_equiv, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_be = PstSat::build(&res_be, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_be.is_state_space_consistent());
    assert!(
        !sat_be
            .diagnostics()
            .iter()
            .any(|d| d.rule_id == "PST_MISSING_PORT_STATE"),
        "implicit __iso_ss/__ret_ss should bridge VDD_AON states: {:?}",
        sat_be.diagnostics()
    );
    assert!(!res_be.same_supply_class(res_be.ss_by_name["SS_1"], res_be.ss_by_name["SS_2"]));
    assert!(matches!(
        sat_be.voltage_relation(
            res_be.ss_by_name["SS_1"],
            res_be.ss_by_name["SS_2"],
            DEFAULT_VOLTAGE_TOL_UV
        ),
        LsRequirement::LowToHigh { .. }
    ));

    // `-function_only` skips voltage UPF_EQUIV_MISMATCH while keeping LS voltage checks.
    let upf_fn_only = r#"
        upf_version 2.1
        create_supply_net VDD_1
        create_supply_net VDD_2
        create_supply_net VSS
        create_supply_set SS_1 -function {power VDD_1} -function {ground VSS}
        create_supply_set SS_2 -function {power VDD_2} -function {ground VSS}
        add_power_state SS_1 -state {ON -supply_expr {power == {FULL_ON 0.9}} -simstate NORMAL}
        add_power_state SS_2 -state {ON -supply_expr {power == {FULL_ON 1.2}} -simstate NORMAL}
        set_equivalent -sets {SS_1 SS_2} -function_only
    "#;
    let res_fo =
        parse_upf_and_resolve(upf_fn_only, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_fo = PstSat::build(&res_fo, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_fo.is_state_space_consistent());
    assert!(!sat_fo
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "UPF_EQUIV_MISMATCH"));
    assert!(matches!(
        sat_fo.voltage_relation(
            res_fo.ss_by_name["SS_1"],
            res_fo.ss_by_name["SS_2"],
            DEFAULT_VOLTAGE_TOL_UV
        ),
        LsRequirement::LowToHigh { .. }
    ));
}

#[test]
fn regression_r2_negated_supply_expr_not_off() {
    let single_neg = r#"
        upf_version 2.1
        create_supply_net VDD_A
        create_supply_net VDD_B
        create_supply_net VSS
        create_supply_set SS_A -function {power VDD_A} -function {ground VSS}
        create_supply_set SS_B -function {power VDD_B} -function {ground VSS}
        add_power_state SS_A -state {ON -supply_expr {!(power == OFF) && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_B -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
    "#;
    let res1 = parse_upf_and_resolve(single_neg, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat1 = PstSat::build(&res1, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        sat1.is_state_space_consistent(),
        "diags: {:?}",
        sat1.diagnostics()
    );
    assert!(sat1.diagnostics().is_empty());
    let ss_a1 = res1.ss_by_name["SS_A"];
    let ss_b1 = res1.ss_by_name["SS_B"];
    assert!(matches!(
        sat1.voltage_relation(ss_a1, ss_b1, DEFAULT_VOLTAGE_TOL_UV),
        LsRequirement::Undetermined { .. }
    ));

    // Same with `power != OFF`.
    let single_neq = single_neg.replace("!(power == OFF)", "power != OFF");
    let res_neq =
        parse_upf_and_resolve(&single_neq, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_neq = PstSat::build(&res_neq, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_neq.is_state_space_consistent());
    assert!(sat_neq.diagnostics().is_empty());

    // Paired with OFF state: ON state must NOT be dropped.
    let paired = format!(
        "{single_neg}\nadd_power_state SS_A -state {{OFF -supply_expr {{power == OFF && ground == {{FULL_ON 0.0}}}} -simstate OFF}}\n"
    );
    let res2 = parse_upf_and_resolve(&paired, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat2 = PstSat::build(&res2, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat2.is_state_space_consistent());
    let witnesses = sat2.enumerate_symbolic_states(10);
    let ss_a_states: BTreeSet<String> = witnesses
        .iter()
        .filter_map(|w| w.supply_states.get("SS_A").cloned())
        .collect();
    assert_eq!(
        ss_a_states,
        BTreeSet::from(["ON".to_string(), "OFF".to_string()])
    );
    let ss_a2 = res2.ss_by_name["SS_A"];
    let ss_b2 = res2.ss_by_name["SS_B"];
    let off_w = sat2.can_be_off_while_on(ss_a2, ss_b2);
    assert!(off_w.is_some());
    assert_eq!(
        off_w.unwrap().supply_states.get("SS_A").map(String::as_str),
        Some("OFF")
    );
    assert!(sat2.can_be_off_while_on(ss_b2, ss_a2).is_none());

    // Negated predicate on an unconstrained rail must not collapse to -const_true.
    let unconstrained_neg = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VDD_AUX
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {aux VDD_AUX} -function {ground VSS}
        add_power_state SS -state {ON -supply_expr {power == {FULL_ON 0.9} && !(aux == {FULL_ON 1.2})} -simstate NORMAL}
    "#;
    let res3 =
        parse_upf_and_resolve(unconstrained_neg, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat3 = PstSat::build(&res3, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat3.is_state_space_consistent());
}

#[test]
fn regression_r3_leading_negated_supply_expr() {
    let upf_lead_1 = r#"
        upf_version 2.1
        create_supply_net VDD_A
        create_supply_net VDD_B
        create_supply_net VDD_AUX
        create_supply_net VSS
        create_supply_set SS_A -function {power VDD_A} -function {aux VDD_AUX} -function {ground VSS}
        create_supply_set SS_B -function {power VDD_B} -function {ground VSS}
        add_power_state SS_A -state {ON -supply_expr {!(aux == OFF) && power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_B -state {ON -supply_expr {power == {FULL_ON 1.1} && ground == {FULL_ON 0.0}} -simstate NORMAL}
    "#;
    let res_l1 = parse_upf_and_resolve(upf_lead_1, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_l1 = PstSat::build(&res_l1, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_l1.is_state_space_consistent());
    assert!(matches!(
        sat_l1.voltage_relation(
            res_l1.ss_by_name["SS_A"],
            res_l1.ss_by_name["SS_B"],
            DEFAULT_VOLTAGE_TOL_UV
        ),
        LsRequirement::LowToHigh { .. }
    ));

    let upf_lead_2 = r#"
        upf_version 2.1
        create_supply_net VDD_A
        create_supply_net VDD_B
        create_supply_net VSS
        create_supply_set SS_A -function {power VDD_A} -function {ground VSS}
        create_supply_set SS_B -function {power VDD_B} -function {ground VSS}
        add_power_state SS_A -state {ON -supply_expr {!(power == OFF) && power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_B -state {ON -supply_expr {power == {FULL_ON 1.1} && ground == {FULL_ON 0.0}} -simstate NORMAL}
    "#;
    let res_l2 = parse_upf_and_resolve(upf_lead_2, None, None, Arc::new(NullObjectFinder)).unwrap();
    let ss_a_l2 = res_l2.ss_by_name["SS_A"];
    let rails_l2 = &res_l2.ss_states[ss_a_l2.0 as usize][0].rail_states;
    assert!(rails_l2
        .iter()
        .any(|r| r.function == "power" && r.voltage == VoltageSpec::Nominal(900_000)));
    assert!(rails_l2
        .iter()
        .any(|r| r.function == "ground" && r.voltage == VoltageSpec::Nominal(0)));
    let mut sat_l2 = PstSat::build(&res_l2, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_l2.is_state_space_consistent());
    assert!(matches!(
        sat_l2.voltage_relation(ss_a_l2, res_l2.ss_by_name["SS_B"], DEFAULT_VOLTAGE_TOL_UV),
        LsRequirement::LowToHigh { .. }
    ));

    let upf_lead_3 = r#"
        upf_version 2.1
        create_supply_net VDD_A
        create_supply_net VDD_B
        create_supply_net VDD_AUX
        create_supply_net VSS
        create_supply_set SS_A -function {power VDD_A} -function {aux VDD_AUX} -function {ground VSS}
        create_supply_set SS_B -function {power VDD_B} -function {ground VSS}
        add_power_state SS_A -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_A -state {OFF -supply_expr {!(aux == {FULL_ON 0.8}) && power == OFF} -simstate CORRUPT}
        add_power_state SS_B -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
    "#;
    let res_l3 = parse_upf_and_resolve(upf_lead_3, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_l3 = PstSat::build(&res_l3, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_l3.is_state_space_consistent());
    assert!(sat_l3
        .can_be_off_while_on(res_l3.ss_by_name["SS_A"], res_l3.ss_by_name["SS_B"])
        .is_some());

    let upf_lead_4 = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        add_power_state SS -state {ON -supply_expr {!(power == OFF) && ground == {FULL_ON 0.5}} -simstate NORMAL}
    "#;
    let res_l4 = parse_upf_and_resolve(upf_lead_4, None, None, Arc::new(NullObjectFinder)).unwrap();
    let ss_l4 = res_l4.ss_by_name["SS"];
    assert!(res_l4.ss_states[ss_l4.0 as usize][0]
        .rail_states
        .iter()
        .any(|r| r.function == "ground" && r.voltage == VoltageSpec::Nominal(500_000)));
    let mut sat_l4 = PstSat::build(&res_l4, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_l4.is_state_space_consistent());

    let upf_lead_5 = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        add_power_state SS -state {S -supply_expr {!(power == {FULL_ON 0.9}) || ground == OFF} -simstate CORRUPT}
    "#;
    let res_l5 = parse_upf_and_resolve(upf_lead_5, None, None, Arc::new(NullObjectFinder)).unwrap();
    let ss_l5 = res_l5.ss_by_name["SS"];
    assert!(res_l5.ss_states[ss_l5.0 as usize][0]
        .rail_states
        .iter()
        .any(|r| r.function == "ground" && r.is_off && r.voltage == VoltageSpec::Off));
}

#[test]
fn regression_r3_scope_hijack_legal_override_and_abs_paths() {
    let upf = r#"
        upf_version 2.1
        create_supply_net VDD_TOP
        create_supply_net VSS
        create_supply_set SS_SHARED -function {power VDD_TOP} -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_SHARED}
        add_power_state SS_SHARED -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        create_supply_net VDD_FLIP
        create_supply_set SS_FLIP -function {power VDD_FLIP} -function {ground VSS}
        add_power_state SS_FLIP -state {S_FLIP -supply_expr {power == {FULL_ON 0.8} && ground == {FULL_ON 0.0}} -illegal}
        add_power_state SS_FLIP -state {S_FLIP -update -legal -simstate NORMAL}

        set_scope u_sub
        create_supply_net VDD_SUB
        create_supply_net VDD_TOP
        create_supply_net VSS
        create_supply_net /VDD_ROOT_DECL
        create_supply_set SS_SHARED -function {power VDD_SUB} -function {ground VSS}
        create_power_domain PD_SUB -include_scope -supply {primary SS_SHARED}
        create_power_domain PD_TOP -elements {u_inner} -supply {primary SS_SHARED}
        add_power_state SS_SHARED -state {ON -supply_expr {power == {FULL_ON 1.2} && ground == {FULL_ON 0.0}} -simstate NORMAL}

        add_power_state /PD_TOP -state {ON -update -logic_expr {top_en == 1}}

        create_supply_set SS_USE_ROOT -function {power /VDD_TOP} -function {ground /VSS}
        add_power_state SS_USE_ROOT -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        set_isolation iso_root -domain /PD_TOP -clamp_value 0 -isolation_supply_set /SS_SHARED
        set_scope /

        create_supply_net VDD_PARENT_ONLY
        create_supply_set SS_PARENT_ONLY -function {power VDD_PARENT_ONLY} -function {ground VSS}
        create_power_domain PD_PARENT_ONLY -elements {u_p} -supply {primary SS_PARENT_ONLY}
        add_power_state SS_PARENT_ONLY -state {ON -supply_expr {power == {FULL_ON 0.95} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        set_scope u_sub
        add_power_state PD_PARENT_ONLY -state {ON -update -logic_expr {p_en == 1}}
        set_scope /
    "#;
    let res = parse_upf_and_resolve(upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(
        !res.intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error),
        "unexpected error diags: {:?}",
        res.intent.diagnostics
    );
    assert!(res.intent.diagnostics.iter().any(|d| {
        d.rule_id == "UPF_SUPPLY_UNCONNECTED" && d.message.contains("`u_sub/VDD_TOP`")
    }));

    let top_ss = res.ss_by_name["SS_SHARED"];
    let sub_ss = res.ss_by_name["u_sub/SS_SHARED"];
    let top_on = res.ss_states[top_ss.0 as usize]
        .iter()
        .find(|s| s.state_name == "ON")
        .unwrap();
    let sub_on = res.ss_states[sub_ss.0 as usize]
        .iter()
        .find(|s| s.state_name == "ON")
        .unwrap();
    assert_eq!(top_on.logic_expr.as_deref(), Some("top_en == 1"));
    assert_eq!(sub_on.logic_expr, None);

    let p_only_ss = res.ss_by_name["SS_PARENT_ONLY"];
    let p_only_on = res.ss_states[p_only_ss.0 as usize]
        .iter()
        .find(|s| s.state_name == "ON")
        .unwrap();
    assert_eq!(p_only_on.logic_expr.as_deref(), Some("p_en == 1"));

    let flip_ss = res.ss_by_name["SS_FLIP"];
    let s_flip = res.ss_states[flip_ss.0 as usize]
        .iter()
        .find(|s| s.state_name == "S_FLIP")
        .unwrap();
    assert!(!s_flip.illegal);
    assert_eq!(s_flip.simstate, Some(SimState::Normal));
    let mut sat = PstSat::build(&res, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat.is_state_space_consistent());

    assert!(res.net_by_name.contains_key("VDD_ROOT_DECL"));
    let use_root_ss = res.ss_by_name["u_sub/SS_USE_ROOT"];
    assert_eq!(
        res.ss_power_net[use_root_ss.0 as usize],
        Some(res.net_by_name["VDD_TOP"])
    );
    assert_eq!(
        res.ss_ground_net[use_root_ss.0 as usize],
        Some(res.net_by_name["VSS"])
    );
    let iso_root = res
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.name == "iso_root")
        .unwrap();
    assert_eq!(res.effective_iso_supply(iso_root), Some(top_ss));
}

#[test]
fn regression_r6_scoped_logic_expr_escaped_idents_and_update_donor_cloning() {
    let dir = temp_dir("r6_scoped_logic");

    // 1. UPF scoped `-logic_expr` non-collision:
    let sub1 = dir.join("sub1.upf");
    let sub2 = dir.join("sub2.upf");
    std::fs::write(
        &sub1,
        r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS1 -function {power VDD} -function {ground VSS}
        create_power_domain PD1 -include_scope -supply {primary SS1}
        add_power_state SS1 -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {en == 1'b1} -simstate NORMAL}
        add_power_state SS1 -state {OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {en == 1'b0} -simstate CORRUPT}
    "#,
    )
    .unwrap();
    std::fs::write(
        &sub2,
        r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS2 -function {power VDD} -function {ground VSS}
        create_power_domain PD2 -include_scope -supply {primary SS2}
        add_power_state SS2 -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {en == 1'b0} -simstate NORMAL}
        add_power_state SS2 -state {OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {en == 1'b1} -simstate CORRUPT}
    "#,
    )
    .unwrap();
    let top_upf = format!(
        r#"
        upf_version 2.1
        load_upf "{}" -scope u_s1
        load_upf "{}" -scope u_s2
    "#,
        sub1.display(),
        sub2.display()
    );
    let res1 = parse_upf_and_resolve(&top_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat1 = PstSat::build(&res1, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat1.is_state_space_consistent());
    let witnesses = sat1.enumerate_symbolic_states(10);
    assert_eq!(witnesses.len(), 4, "witnesses: {witnesses:?}");
    let both_on = witnesses
        .iter()
        .find(|w| {
            w.supply_states.get("u_s1/SS1").map(String::as_str) == Some("ON")
                && w.supply_states.get("u_s2/SS2").map(String::as_str) == Some("ON")
        })
        .expect("both ON witness must exist without cross-scope `en` collision");
    assert_eq!(both_on.logic_signals.get("u_s1/en"), Some(&true));
    assert_eq!(both_on.logic_signals.get("u_s2/en"), Some(&false));
    let ss1_id = res1.ss_by_name["u_s1/SS1"];
    let ss2_id = res1.ss_by_name["u_s2/SS2"];
    assert!(sat1.can_be_off_while_on(ss1_id, ss2_id).is_some());
    assert!(sat1.can_be_off_while_on(ss2_id, ss1_id).is_some());

    // 2. Root-anchored `/top_en` vs relative `en` & hierarchical `u_child/en` / `u_child.en`:
    let abs_hier_upf = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS_ROOT -function {power VDD} -function {ground VSS}
        add_power_state SS_ROOT -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {top_en == 1'b1} -simstate NORMAL}

        set_scope u_s1
        create_supply_net VDD1
        create_supply_set SS1 -function {power VDD1} -function {ground /VSS}
        add_power_state SS1 -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {/top_en == 1'b1 && u_child/en == 1'b1 && u_child.en == 1'b1} -simstate NORMAL}

        set_scope /u_s2
        create_supply_net VDD2
        create_supply_set SS2 -function {power VDD2} -function {ground /VSS}
        add_power_state SS2 -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {/top_en == 1'b0} -simstate NORMAL}
        set_scope /
    "#;
    let res_abs =
        parse_upf_and_resolve(abs_hier_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_abs = PstSat::build(&res_abs, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        !sat_abs.is_state_space_consistent(),
        "/top_en == 1'b1 in u_s1 must collide with /top_en == 1'b0 in u_s2"
    );
    let hier_match_upf = r#"
        upf_version 2.1
        set_scope u_s1
        create_supply_net VDD1
        create_supply_net VSS1
        create_supply_set SS1 -function {power VDD1} -function {ground VSS1}
        add_power_state SS1 -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {u_child/en == 4'hA && u_child.en == 4'hA} -simstate NORMAL}
        set_scope /
    "#;
    let res_hier =
        parse_upf_and_resolve(hier_match_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_hier = PstSat::build(&res_hier, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_hier.is_state_space_consistent());
    let hw = sat_hier.enumerate_symbolic_states(1);
    assert_eq!(hw.len(), 1);
    assert!(hw[0]
        .logic_signals
        .keys()
        .any(|k| k.starts_with("u_s1/u_child/en[")));

    // 3. Escaped identifiers across scopes & hierarchy:
    let esc_upf = r#"
        upf_version 2.1
        set_scope u_s1
        create_supply_net VDD1
        create_supply_net VSS1
        create_supply_set SS1 -function {power VDD1} -function {ground VSS1}
        add_power_state SS1 -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {\foo[0] == 1'b1 && \foo.bar == 1'b1 && u_child/\foo[0] == 1'b1 && /\top_esc[0] == 1'b1} -simstate NORMAL}
        set_scope /u_s2
        create_supply_net VDD2
        create_supply_net VSS2
        create_supply_set SS2 -function {power VDD2} -function {ground VSS2}
        add_power_state SS2 -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {\foo[0] == 1'b0 && \foo.bar == 1'b0 && u_child/\foo[0] == 1'b0 && /\top_esc[0] == 1'b1} -simstate NORMAL}
        set_scope /
    "#;
    let res_esc = parse_upf_and_resolve(esc_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_esc = PstSat::build(&res_esc, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        sat_esc.is_state_space_consistent(),
        "escaped idents in u_s1 and u_s2 must not collide: {:?}",
        sat_esc.diagnostics()
    );
    let esc_w = sat_esc.enumerate_symbolic_states(1);
    assert_eq!(esc_w.len(), 1);
    let ls = &esc_w[0].logic_signals;
    assert_eq!(ls.get("u_s1/foo[0]"), Some(&true));
    assert_eq!(ls.get("u_s1/foo.bar"), Some(&true));
    assert_eq!(ls.get("u_s1/u_child/foo[0]"), Some(&true));
    assert_eq!(ls.get("u_s2/foo[0]"), Some(&false));
    assert_eq!(ls.get("u_s2/foo.bar"), Some(&false));
    assert_eq!(ls.get("u_s2/u_child/foo[0]"), Some(&false));
    assert_eq!(ls.get("top_esc[0]"), Some(&true));

    // 4. Cross-scope `add_power_state -update` and implicit donor cloning:
    let donor_upf = r#"
        upf_version 2.1
        create_supply_net VDD_TOP
        create_supply_net VSS
        create_supply_set SS_TOP -function {power VDD_TOP} -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_TOP}
        add_power_state SS_TOP -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {top_en == 1'b1} -simstate NORMAL}
        add_power_state SS_TOP -state {OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {top_en == 1'b0} -simstate CORRUPT}

        set_scope u_sub
        add_power_state /PD_TOP -state {ON -update -logic_expr {/top_en == 1'b1}}
        add_power_state /SS_TOP -state {ON -update -simstate NORMAL}

        create_supply_net VDD_ISO_SUB
        connect_supply_net /VDD_TOP -ports {VDD_ISO_SUB}
        create_power_domain PD_SUB -elements {u_cell} -supply {primary /SS_TOP}
        set_isolation iso_sub -domain PD_SUB -clamp_value 0 -isolation_power_net /VDD_TOP -isolation_ground_net /VSS

        create_supply_net VDD_SUB_CONFLICT
        create_supply_set SS_SUB_CONFLICT -function {power VDD_SUB_CONFLICT} -function {ground /VSS}
        add_power_state SS_SUB_CONFLICT -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {top_en == 1'b0} -simstate NORMAL}
        set_scope /
    "#;
    let res_donor =
        parse_upf_and_resolve(donor_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_donor = PstSat::build(&res_donor, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        sat_donor.is_state_space_consistent(),
        "root `/top_en` on SS_TOP + implicit iso supply set must not collide with `u_sub/top_en`: {:?}",
        sat_donor.diagnostics()
    );
    let dw = sat_donor.enumerate_symbolic_states(4);
    assert_eq!(dw.len(), 2, "expected 2 states (SS_TOP ON/OFF), got {dw:?}");
    let on_w = dw
        .iter()
        .find(|w| w.supply_states.get("SS_TOP").map(String::as_str) == Some("ON"))
        .unwrap();
    assert_eq!(on_w.logic_signals.get("top_en"), Some(&true));
    assert_eq!(on_w.logic_signals.get("u_sub/top_en"), Some(&false));
    let _ = std::fs::remove_dir_all(&dir);
}
