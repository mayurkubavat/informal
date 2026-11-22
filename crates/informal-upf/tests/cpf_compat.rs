mod common;

use std::sync::Arc;

use common::temp_dir;
use informal_upf::{
    compare_upf, load_upf, parse_cpf_and_resolve, parse_upf_and_resolve, AppliesTo, DiagSeverity,
    NullObjectFinder, ObjectFinder, PstSat, Sense, StrategyLocation, StrategyMatch, UpfError,
    VoltageSpec, DEFAULT_VOLTAGE_TOL_UV,
};

#[test]
fn cpf_20_parity_and_d13_error_checking() {
    let cpf_20 = r#"
        set_cpf_version 2.0
        set_hierarchy_separator /
        set_design chip
        create_power_nets -nets {VDD_AON VDD_CORE} -voltage 0.90
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_CORE -instances {u_core} -shutoff_condition {!pwr_en}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_CORE -primary_power_net VDD_CORE -primary_ground_net VSS
        create_nominal_condition -name NC_ON -voltage 0.90 -state on
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off
        create_power_mode -name PM_ALL_ON -domain_conditions {PD_TOP@NC_ON PD_CORE@NC_ON} -default
        create_power_mode -name PM_SLEEP  -domain_conditions {PD_TOP@NC_ON PD_CORE@NC_OFF}
        create_isolation_rule -name iso_core -from PD_CORE -to PD_TOP -isolation_condition {iso_en} -isolation_output low -isolation_target to
        create_state_retention_rule -name ret_core -domain PD_CORE -save_edge {save} -restore_edge {restore} -secondary_power_net VDD_AON
        end_design
    "#;
    let r_cpf = parse_cpf_and_resolve(cpf_20, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_cpf = PstSat::build(&r_cpf, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_cpf.is_state_space_consistent());
    let pd_top_ss = r_cpf.primary_ss(r_cpf.domain_by_name["PD_TOP"]).unwrap();
    let pd_core_ss = r_cpf.primary_ss(r_cpf.domain_by_name["PD_CORE"]).unwrap();
    assert!(sat_cpf.can_be_off_while_on(pd_core_ss, pd_top_ss).is_some());
    assert!(sat_cpf.can_be_off_while_on(pd_top_ss, pd_core_ss).is_none());

    let bad_cpf = "set_cpf_version 2.0\ncreate_power_domain -name PD_A -unknown_semantic_opt 1\n";
    let cpf_err =
        parse_cpf_and_resolve(bad_cpf, None, None, Arc::new(NullObjectFinder)).unwrap_err();
    assert!(matches!(cpf_err, UpfError::Tcl { .. }));
    assert!(cpf_err
        .to_string()
        .contains("unsupported option -unknown_semantic_opt"));
}

#[test]
fn regression_mixed_load_upf_cpf_flags_direction_and_scope_checks() {
    let dir = temp_dir("mixed_cpf");
    let sub1 = dir.join("sub1.cpf");
    let sub2 = dir.join("sub2.cpf");
    let top = dir.join("top.upf");

    std::fs::write(
        &sub1,
        r#"set_cpf_version 1.1
set objs [find_objects . -object_type inst]
if {[llength $objs] == 0} {
    error "expected find_objects inside sub1.cpf"
}
create_power_nets -nets {VDD_S1} -voltage 0.90
create_ground_nets -nets {VSS}
create_power_domain -name PD_S1 -default -shutoff_condition {!en_s1}
update_power_domain -name PD_S1 -primary_power_net VDD_S1 -primary_ground_net VSS
create_nominal_condition -name NC_ON -voltage 0.90 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_mode -name PM_S1_ON -domain_conditions {PD_S1@NC_ON} -default
create_power_mode -name PM_S1_OFF -domain_conditions {PD_S1@NC_OFF}
create_isolation_rule -name iso_s1 -from PD_S1 -isolation_condition {iso_s1_en} -isolation_output low
update_isolation_rules -names iso_s1 -location FROM
"#,
    )
    .unwrap();

    std::fs::write(
        &sub2,
        r#"set_cpf_version 2.0
create_power_nets -nets {VDD_A VDD_B} -voltage 0.90
create_ground_nets -nets {VSS}
create_power_domain -name PD_A -default
create_power_domain -name PD_B -instances {u_b}
update_power_domain -name PD_A -primary_power_net VDD_A -primary_ground_net VSS
update_power_domain -name PD_B -primary_power_net VDD_B -primary_ground_net VSS
create_nominal_condition -name NC_ON -voltage 0.90 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_mode -name PM_A_ON -domain_conditions {PD_A@NC_ON}
create_power_mode -name PM_A_OFF -domain_conditions {PD_A@NC_OFF}
create_power_mode -name PM_B_ON -domain_conditions {PD_B@NC_ON}
create_power_mode -name PM_B_OFF -domain_conditions {PD_B@NC_OFF}
create_power_mode -name PM_ILL_A_OFF_B_ON -domain_conditions {PD_A@NC_OFF PD_B@NC_ON} -illegal
end_design
"#,
    )
    .unwrap();

    std::fs::write(
        &top,
        r#"upf_version 3.1
create_supply_port P_IN -direction in
create_supply_port P_OUT -direction out
create_supply_net VDD_TOP
create_supply_net VSS
create_supply_set SS_TOP -function {power VDD_TOP} -function {ground VSS}
create_power_domain PD_TOP -include_scope -supply {primary SS_TOP}
create_power_domain PD_SUB_SCOPE -scope u_sub1 -include_scope
add_power_state SS_TOP -state {ON -supply_expr {power == {FULL_ON 0.9}} -simstate NORMAL}
set in_ports [find_objects . -object_type supply_port -direction in]
if {$in_ports ne "P_IN"} { error "expected P_IN only, got $in_ports" }
set out_ports [find_objects . -object_type supply_port -direction out]
if {$out_ports ne "P_OUT"} { error "expected P_OUT only, got $out_ports" }
set_isolation iso_t1 -domain PD_TOP -transitively_included -clamp_value 0
set_isolation iso_t2 -domain PD_TOP -elements {u_x} -transitively_included false -clamp_value 1
set_level_shifter ls_neg -domain PD_TOP -threshold -.5
load_upf sub1.cpf -format cpf -scope u_sub1 -version 1.1
load_upf sub2.cpf -format cpf -scope u_sub2 -version 2.0
"#,
    )
    .unwrap();

    let finder: Arc<dyn ObjectFinder> = Arc::new(common::MockDesignFinder::new(
        &["u_sub1", "u_sub2", "u_sub2/u_b"],
        &["u_sub1/u_r0", "u_sub2/u_b/u_r1", "u_x"],
    ));
    let resolved = load_upf(&top, None, None, finder.clone()).unwrap();

    let pd_sub_scope = &resolved.intent.domains[resolved.domain_by_name["PD_SUB_SCOPE"].0 as usize];
    assert!(!pd_sub_scope.is_top);

    let ls_neg = resolved
        .intent
        .level_shifter_strategies
        .iter()
        .find(|l| l.name == "ls_neg")
        .unwrap();
    assert_eq!(ls_neg.threshold_uv, Some(-500_000));

    let iso_s1 = resolved
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.name == "iso_s1")
        .unwrap();
    assert_eq!(iso_s1.location, Some(StrategyLocation::Self_));

    assert!(!resolved.intent.pst_tables.is_empty());
    let mut sat = PstSat::build(&resolved, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat.is_state_space_consistent());
    let ss_s1 = resolved.ss_by_name["SS_u_sub1/PD_S1"];
    let ss_top = resolved.ss_by_name["SS_TOP"];
    assert!(sat.can_be_off_while_on(ss_s1, ss_top).is_some());

    let ss_a = resolved.ss_by_name["SS_u_sub2/PD_A"];
    let ss_b = resolved.ss_by_name["SS_u_sub2/PD_B"];
    assert!(sat.can_be_off_while_on(ss_a, ss_b).is_none());
    assert!(sat.can_be_off_while_on(ss_b, ss_a).is_some());

    let bad_dom_scope = "create_power_domain PD_BAD -scope u_nonexistent -include_scope\n";
    let err = parse_upf_and_resolve(bad_dom_scope, None, None, finder.clone()).unwrap_err();
    assert!(err.to_string().contains("unresolved scope"));

    let cpf_mixed = format!(
        "set_cpf_version 2.0\nset found [find_objects . -object_type inst]\nif {{[llength $found] == 0}} {{ error \"no insts\" }}\nload_upf {} -format cpf -scope u_sub1\n",
        sub1.display()
    );
    let r_cpf_mix = parse_cpf_and_resolve(&cpf_mixed, None, None, finder).unwrap();
    assert!(r_cpf_mix.domain_by_name.contains_key("u_sub1/PD_S1"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn regression_r2_cpf_from_to_shared_net_no_aliasing_and_equiv_nets() {
    let cpf = r#"
        set_cpf_version 2.0
        set_design top
        create_power_nets -nets {VDD_AON VDD_SW VDD_1 VDD_2}
        create_ground_nets -nets {VSS VSS_1 VSS_2}
        create_power_domain -name PD_1 -default
        create_power_domain -name PD_2 -instances {u_2}
        create_power_domain -name PD_3 -instances {u_3}
        create_power_domain -name PD_EQ1 -instances {u_eq1}
        create_power_domain -name PD_EQ2 -instances {u_eq2}
        update_power_domain -name PD_1 -primary_power_net VDD_SW  -primary_ground_net VSS
        update_power_domain -name PD_2 -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_3 -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_EQ1 -primary_power_net VDD_1 -primary_ground_net VSS_1 \
            -equivalent_power_nets {VDD_2} -equivalent_ground_nets {VSS_2}
        update_power_domain -name PD_EQ2 -primary_power_net VDD_2 -primary_ground_net VSS_2
        create_isolation_rule -name iso_1_to_2 -from PD_1 -to PD_2 -isolation_output low
        create_level_shifter_rule -name ls_1_to_2 -from PD_1 -to PD_2
        end_design
    "#;
    let res = parse_cpf_and_resolve(cpf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let pd1 = res.domain_by_name["PD_1"];
    let pd2 = res.domain_by_name["PD_2"];
    let pd3 = res.domain_by_name["PD_3"];

    assert!(matches!(
        res.match_isolation(Some(pd1), Some(pd2), "u_1/sig", AppliesTo::Outputs),
        StrategyMatch::Matched(iso) if iso.name == "iso_1_to_2"
    ));
    assert_eq!(
        res.match_isolation(Some(pd1), Some(pd3), "u_1/sig", AppliesTo::Outputs),
        StrategyMatch::None
    );
    assert!(matches!(
        res.match_level_shifter(Some(pd1), Some(pd2), "u_1/sig", AppliesTo::Outputs),
        StrategyMatch::Matched(ls) if ls.name == "ls_1_to_2"
    ));
    assert_eq!(
        res.match_level_shifter(Some(pd1), Some(pd3), "u_1/sig", AppliesTo::Outputs),
        StrategyMatch::None
    );

    let ss_eq1 = res.primary_ss(res.domain_by_name["PD_EQ1"]).unwrap();
    let ss_eq2 = res.primary_ss(res.domain_by_name["PD_EQ2"]).unwrap();
    assert!(res.same_supply_class(ss_eq1, ss_eq2));
    assert!(res.same_func_supply_class(ss_eq1, ss_eq2));

    let cpf_two_step = r#"
        set_cpf_version 2.0
        set_design top
        create_power_nets -nets {VDD_A VDD_B}
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_A -default
        create_power_domain -name PD_B -instances {u_b}
        update_power_domain -name PD_A -primary_power_net VDD_A -primary_ground_net VSS
        update_power_domain -name PD_A -equivalent_power_nets {VDD_B}
        update_power_domain -name PD_B -primary_power_net VDD_B -primary_ground_net VSS
        end_design
    "#;
    let res2 = parse_cpf_and_resolve(cpf_two_step, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(res2.same_supply_class(
        res2.primary_ss(res2.domain_by_name["PD_A"]).unwrap(),
        res2.primary_ss(res2.domain_by_name["PD_B"]).unwrap(),
    ));

    // R3 Item 7: CPF `update_power_domain -equivalent_*_nets` BEFORE `-primary_*_net`.
    let cpf_equiv_before = r#"
        set_cpf_version 2.0
        set_design top
        create_power_nets -nets {VDD_A VDD_B}
        create_ground_nets -nets {VSS_A VSS_B}
        create_power_domain -name PD_A -default
        create_power_domain -name PD_B -instances {u_b}
        update_power_domain -name PD_A -equivalent_power_nets {VDD_B} -equivalent_ground_nets {VSS_B}
        update_power_domain -name PD_A -primary_power_net VDD_A -primary_ground_net VSS_A
        update_power_domain -name PD_B -primary_power_net VDD_B -primary_ground_net VSS_B
        end_design
    "#;
    let res_eq =
        parse_cpf_and_resolve(cpf_equiv_before, None, None, Arc::new(NullObjectFinder)).unwrap();
    let ss_eq_a = res_eq.primary_ss(res_eq.domain_by_name["PD_A"]).unwrap();
    let ss_eq_b = res_eq.primary_ss(res_eq.domain_by_name["PD_B"]).unwrap();
    assert!(res_eq.same_supply_class(ss_eq_a, ss_eq_b));
    assert!(res_eq.same_func_supply_class(ss_eq_a, ss_eq_b));
}

#[test]
fn regression_r5_cpf_update_power_switch_rule_and_scoped_nets() {
    let dir = temp_dir("r5_cpf_psw_and_nets");
    let child_psw_cpf = dir.join("child_psw.cpf");
    std::fs::write(
        &child_psw_cpf,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_SW}
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_AON -default
        create_power_domain -name PD_SW -instances {u_sw} -shutoff_condition {shutoff}
        update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_SW -primary_power_net VDD_SW -primary_ground_net VSS
        create_power_switch_rule -name psw_rule -domain PD_SW -external_power_net VDD_AON
        update_power_switch_rule -name psw_rule -cells {PSW_CHILD}
        end_design
    "#,
    )
    .unwrap();
    let parent_psw_cpf = dir.join("parent_psw.cpf");
    std::fs::write(
        &parent_psw_cpf,
        format!(
            r#"
        set_cpf_version 2.0
        load_upf {} -format cpf -scope u_child
        create_power_nets -nets {{VDD_AON VDD_SW}}
        create_ground_nets -nets {{VSS}}
        create_power_domain -name PD_AON -default
        create_power_domain -name PD_SW -instances {{u_sw}} -shutoff_condition {{shutoff}}
        update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_SW -primary_power_net VDD_SW -primary_ground_net VSS
        create_power_switch_rule -name psw_rule -domain PD_SW -external_power_net VDD_AON
        update_power_switch_rule -name psw_rule -cells {{PSW_PARENT}}
        end_design
    "#,
            child_psw_cpf.display()
        ),
    )
    .unwrap();
    let top_psw_upf = dir.join("top_psw.upf");
    std::fs::write(
        &top_psw_upf,
        format!(
            "upf_version 2.1\nload_upf {} -format cpf\n",
            parent_psw_cpf.display()
        ),
    )
    .unwrap();
    let res_psw = load_upf(&top_psw_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let child_psw = res_psw
        .intent
        .power_switches
        .iter()
        .find(|s| s.name == "u_child/psw_rule")
        .unwrap();
    let parent_psw = res_psw
        .intent
        .power_switches
        .iter()
        .find(|s| s.name == "psw_rule")
        .unwrap();
    assert_eq!(child_psw.mapped_cells, vec!["PSW_CHILD".to_string()]);
    assert_eq!(parent_psw.mapped_cells, vec!["PSW_PARENT".to_string()]);

    let sub_cpf = dir.join("sub_scoped.cpf");
    std::fs::write(
        &sub_cpf,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_SUB}
        create_power_domain -name PD_SUB -instances {u_reg}
        update_power_domain -name PD_SUB -equivalent_power_nets {VDD_SUB}
        update_power_domain -name PD_SUB -primary_power_net VDD_ROOT -primary_ground_net VSS_ROOT
        create_isolation_rule -name iso_to_top -from PD_SUB -to PD_TOP -isolation_output low
        create_level_shifter_rule -name ls_to_top -from PD_SUB -to PD_TOP
        end_design
    "#,
    )
    .unwrap();
    let top_cpf = dir.join("top_scoped.cpf");
    std::fs::write(
        &top_cpf,
        format!(
            r#"
        set_cpf_version 2.0
        create_power_nets -nets {{VDD_ROOT}}
        create_ground_nets -nets {{VSS_ROOT}}
        create_power_domain -name PD_TOP -default
        update_power_domain -name PD_TOP -primary_power_net VDD_ROOT -primary_ground_net VSS_ROOT
        load_upf {} -format cpf -scope u_sub
        end_design
    "#,
            sub_cpf.display()
        ),
    )
    .unwrap();
    let top_scoped_upf = dir.join("top_scoped.upf");
    std::fs::write(
        &top_scoped_upf,
        format!(
            "upf_version 2.1\nload_upf {} -format cpf\n",
            top_cpf.display()
        ),
    )
    .unwrap();
    let res_scoped_cpf = load_upf(&top_scoped_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(!res_scoped_cpf
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));
    let ss_top = res_scoped_cpf
        .primary_ss(res_scoped_cpf.domain_by_name["PD_TOP"])
        .unwrap();
    let ss_sub = res_scoped_cpf
        .primary_ss(res_scoped_cpf.domain_by_name["u_sub/PD_SUB"])
        .unwrap();
    assert_eq!(
        res_scoped_cpf.intent.supply_sets[ss_sub.0 as usize].scope,
        "u_sub"
    );
    assert_eq!(
        res_scoped_cpf.ss_power_net[ss_sub.0 as usize],
        Some(res_scoped_cpf.net_by_name["VDD_ROOT"])
    );
    assert_eq!(
        res_scoped_cpf.ss_ground_net[ss_sub.0 as usize],
        Some(res_scoped_cpf.net_by_name["VSS_ROOT"])
    );
    assert!(res_scoped_cpf.same_supply_class(ss_top, ss_sub));
    let iso_to_top = res_scoped_cpf
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.name == "iso_to_top")
        .unwrap();
    assert_eq!(
        res_scoped_cpf.effective_iso_supply(iso_to_top),
        Some(ss_top)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn regression_r6_r8_cpf_shutoff_child_end_design_and_negate_bool() {
    let dir = temp_dir("r6_r8_cpf");
    let cpf_s1 = dir.join("s1.cpf");
    let cpf_s2 = dir.join("s2.cpf");
    std::fs::write(
        &cpf_s1,
        r#"
        set_cpf_version 2.0
        create_power_domain -name PD -shutoff_condition {!en} -instances {u_a}
        create_power_switch_rule -name PSW1 -domain PD -enable_condition {en}
        create_power_mode -name PM_ILL_S1 -domain_conditions {PD@NC_OFF} -illegal -condition {bad_en}
    "#,
    )
    .unwrap();
    std::fs::write(
        &cpf_s2,
        r#"
        set_cpf_version 2.0
        create_power_domain -name PD -shutoff_condition {en} -instances {u_b}
        create_power_switch_rule -name PSW2 -domain PD -enable_condition {!en}
        create_power_mode -name PM_ILL_S2 -domain_conditions {PD@NC_OFF} -illegal -condition {!bad_en}
    "#,
    )
    .unwrap();
    let cpf_top = format!(
        r#"
        set_cpf_version 2.0
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off
        create_power_nets -nets {{VDD1 VDD2}} -voltage 0.9
        create_ground_nets -nets {{VSS}}
        load_upf "{s1}" -format cpf -scope u_s1
        load_upf "{s2}" -format cpf -scope u_s2
        update_power_domain -name u_s1/PD -primary_power_net VDD1 -primary_ground_net VSS
        update_power_domain -name u_s2/PD -primary_power_net VDD2 -primary_ground_net VSS
        create_power_mode -name PM_OO -domain_conditions {{u_s1/PD@NC_ON u_s2/PD@NC_ON}} -default
        create_power_mode -name PM_OF -domain_conditions {{u_s1/PD@NC_ON u_s2/PD@NC_OFF}}
        create_power_mode -name PM_FO -domain_conditions {{u_s1/PD@NC_OFF u_s2/PD@NC_ON}}
        create_power_mode -name PM_FF -domain_conditions {{u_s1/PD@NC_OFF u_s2/PD@NC_OFF}}
        end_design
    "#,
        s1 = cpf_s1.display(),
        s2 = cpf_s2.display()
    );
    let res_cpf = parse_cpf_and_resolve(&cpf_top, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_cpf = PstSat::build(&res_cpf, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_cpf.is_state_space_consistent());
    let cpf_witnesses = sat_cpf.enumerate_symbolic_states(10);
    assert_eq!(cpf_witnesses.len(), 4);
    let cpf_both_on = cpf_witnesses
        .iter()
        .find(|w| {
            w.supply_states.get("SS_u_s1/PD").map(String::as_str) == Some("NC_ON")
                && w.supply_states.get("SS_u_s2/PD").map(String::as_str) == Some("NC_ON")
        })
        .unwrap();
    assert_eq!(cpf_both_on.logic_signals.get("u_s1/en"), Some(&true));
    assert_eq!(cpf_both_on.logic_signals.get("u_s2/en"), Some(&false));

    // Fallback without create_power_mode + compare_upf:
    let cpf_s1_fb = dir.join("s1_fb.cpf");
    let cpf_s2_fb = dir.join("s2_fb.cpf");
    std::fs::write(
        &cpf_s1_fb,
        r#"
        set_cpf_version 2.0
        create_power_domain -name PD -shutoff_condition {!en} -instances {u_a}
        create_power_switch_rule -name PSW1 -domain PD -enable_condition {en}
        create_power_mode -name PM_ILL_FALLBACK -domain_conditions {PD@MISSING_NC} -illegal -condition {bad_en}
    "#,
    )
    .unwrap();
    std::fs::write(
        &cpf_s2_fb,
        r#"
        set_cpf_version 2.0
        create_power_domain -name PD -shutoff_condition {en} -instances {u_b}
        create_power_switch_rule -name PSW2 -domain PD -enable_condition {!en}
    "#,
    )
    .unwrap();
    let cpf_fallback = format!(
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {{VDD1 VDD2}} -voltage 0.9
        create_ground_nets -nets {{VSS}}
        load_upf "{s1}" -format cpf -scope u_s1
        load_upf "{s2}" -format cpf -scope u_s2
        update_power_domain -name u_s1/PD -primary_power_net VDD1 -primary_ground_net VSS
        update_power_domain -name u_s2/PD -primary_power_net VDD2 -primary_ground_net VSS
        end_design
    "#,
        s1 = cpf_s1_fb.display(),
        s2 = cpf_s2_fb.display()
    );
    let res_fb =
        parse_cpf_and_resolve(&cpf_fallback, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_fb = PstSat::build(&res_fb, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_fb.is_state_space_consistent());
    assert_eq!(sat_fb.enumerate_symbolic_states(10).len(), 4);

    let eq_upf = r#"
        upf_version 2.1
        create_supply_net VDD1
        create_supply_net VDD2
        create_supply_net VSS
        create_supply_set SS_u_s1/PD -function {power VDD1} -function {ground VSS}
        create_power_domain u_s1/PD -elements {u_s1/u_a} -supply {primary SS_u_s1/PD}
        add_power_state SS_u_s1/PD -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {u_s1/en == 1'b1} -simstate NORMAL}
        add_power_state SS_u_s1/PD -state {OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {u_s1/en == 1'b0} -simstate CORRUPT}
        add_power_state SS_u_s1/PD -state {ILL_FB -illegal -logic_expr {u_s1/bad_en && u_s1/en}}
        create_supply_set SS_u_s2/PD -function {power VDD2} -function {ground VSS}
        create_power_domain u_s2/PD -elements {u_s2/u_b} -supply {primary SS_u_s2/PD}
        add_power_state SS_u_s2/PD -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {en_s2 == 1'b0} -simstate NORMAL}
        add_power_state SS_u_s2/PD -state {OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {en_s2 == 1'b1} -simstate CORRUPT}
    "#;
    let res_eq = parse_upf_and_resolve(eq_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let cmp = compare_upf(&res_fb, &res_eq, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(cmp.equivalent, "unexpected diffs: {:?}", cmp.diffs);

    // R7-2 child `end_design` loaded inside parent CPF:
    let child_cpf_1 = dir.join("c1.cpf");
    std::fs::write(
        &child_cpf_1,
        "set_cpf_version 2.0\ncreate_power_domain -name PD -shutoff_condition {!en} -instances {u_a}\nend_design\n",
    )
    .unwrap();
    let parent_cpf_modes = format!(
        r#"
        set_cpf_version 2.0
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off
        create_power_nets -nets {{VDD1}} -voltage 0.9
        create_ground_nets -nets {{VSS}}
        load_upf "{}" -format cpf -scope u_s1
        update_power_domain -name u_s1/PD -primary_power_net VDD1 -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {{u_s1/PD@NC_ON}} -default
        create_power_mode -name PM_OFF -domain_conditions {{u_s1/PD@NC_OFF}}
        end_design
    "#,
        child_cpf_1.display()
    );
    let res_modes =
        parse_cpf_and_resolve(&parent_cpf_modes, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut state_names: Vec<String> = res_modes
        .intent
        .power_states
        .iter()
        .filter(|s| s.target == "SS_u_s1/PD")
        .map(|s| s.state_name.clone())
        .collect();
    state_names.sort();
    assert_eq!(state_names, vec!["NC_OFF", "NC_ON"]);

    let parent_cpf_nomodes = format!(
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {{VDD1}} -voltage 0.9
        create_ground_nets -nets {{VSS}}
        load_upf "{}" -format cpf -scope u_s1
        update_power_domain -name u_s1/PD -primary_power_net VDD1 -primary_ground_net VSS
        end_design
    "#,
        child_cpf_1.display()
    );
    let res_nomodes =
        parse_cpf_and_resolve(&parent_cpf_nomodes, None, None, Arc::new(NullObjectFinder)).unwrap();
    let on_state = res_nomodes
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_u_s1/PD" && s.state_name == "ON")
        .unwrap();
    let power_rail = on_state
        .rail_states
        .iter()
        .find(|r| r.function == "power")
        .unwrap();
    assert_eq!(power_rail.voltage, VoltageSpec::Nominal(900_000));

    // R7-3 & R7-5: `negate_bool_expr` with trailing/mid-path escaped identifiers:
    let cpf_neg = r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_power_domain -name PD1 -default -shutoff_condition {en && u_child/\foo[0] }
        update_power_domain -name PD1 -primary_power_net VDD -primary_ground_net VSS
        create_power_domain -name PD2 -shutoff_condition {!(a) && !(b)} -instances {u_b}
        update_power_domain -name PD2 -primary_power_net VDD -primary_ground_net VSS
        create_power_domain -name PD3 -shutoff_condition {!c && d} -instances {u_c}
        update_power_domain -name PD3 -primary_power_net VDD -primary_ground_net VSS
        end_design
    "#;
    let res_neg = parse_cpf_and_resolve(cpf_neg, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_neg = PstSat::build(&res_neg, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!sat_neg
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "UPF_EXPR_SYNTAX"));
    assert!(sat_neg.is_state_space_consistent());
    let witnesses = sat_neg.enumerate_symbolic_states(16);
    assert!(witnesses.iter().any(|w| {
        w.supply_states.get("SS_PD3").map(String::as_str) == Some("ON")
            && w.logic_signals.get("c") == Some(&false)
            && w.logic_signals.get("d") == Some(&false)
    }));

    let cpf_mid_esc = r#"
        set_cpf_version 2.0
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off
        create_power_nets -nets {VDD} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_power_domain -name PD -default -shutoff_condition {u_top/\u_mid /sig}
        update_power_domain -name PD -primary_power_net VDD -primary_ground_net VSS
        create_power_mode -name PM_ILL -domain_conditions {PD@NC_ON} -illegal -condition {u_top/\u_mid /bad }
        end_design
    "#;
    let res_cpf_mid =
        parse_cpf_and_resolve(cpf_mid_esc, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_cpf_mid = PstSat::build(&res_cpf_mid, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!sat_cpf_mid
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "UPF_EXPR_SYNTAX"));
    assert!(sat_cpf_mid.is_state_space_consistent());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn regression_r17_r18_nested_set_design_mode_cond_and_fix8_fix9() {
    let dir = temp_dir("r17_r18_cpf");

    // C2: nested `set_design child_mod` ... `end_design child_mod`.
    let c2_child = dir.join("c2_child.cpf");
    std::fs::write(
        &c2_child,
        r#"
        set_cpf_version 2.0
        set_design child_mod
        create_power_domain -name PD_CHILD -instances {u_a} -default
        end_design child_mod
    "#,
    )
    .unwrap();
    let c2_parent = format!(
        r#"
        set_cpf_version 2.0
        set_design top_mod
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_nets -nets {{VDD1}} -voltage 0.9
        create_ground_nets -nets {{VSS}}
        create_power_domain -name PD_TOP -default
        update_power_domain -name PD_TOP -primary_power_net VDD1 -primary_ground_net VSS
        load_upf "{}" -scope u_sub -format cpf
        update_power_domain -name u_sub/PD_CHILD -primary_power_net VDD1 -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {{PD_TOP@NC_ON u_sub/PD_CHILD@NC_ON}} -default
        end_design top_mod
    "#,
        c2_child.to_string_lossy().replace('\\', "\\\\")
    );
    let res_c2 = parse_cpf_and_resolve(&c2_parent, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(!res_c2
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));

    // Legal `create_power_mode -condition` semantics:
    let cond_mode_cpf = |mode_cond: &str| {
        format!(
            r#"
            set_cpf_version 2.0
            create_nominal_condition -name NC_ON -voltage 0.9 -state on
            create_nominal_condition -name NC_OFF -voltage 0.0 -state off
            create_power_nets -nets {{VDD_SW}} -voltage 0.9
            create_ground_nets -nets {{VSS}}
            create_power_domain -name PD_SW -shutoff_condition {{!pwr_en}} -default
            update_power_domain -name PD_SW -primary_power_net VDD_SW -primary_ground_net VSS
            create_power_mode -name PM_COND -domain_conditions {{PD_SW@NC_ON}} -condition {{{mode_cond}}} -default
            end_design
        "#
        )
    };
    let res_pm_sat = parse_cpf_and_resolve(
        &cond_mode_cpf("mode_ok"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    assert!(PstSat::build(&res_pm_sat, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());
    let res_pm_unsat = parse_cpf_and_resolve(
        &cond_mode_cpf("!pwr_en"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    assert!(!PstSat::build(&res_pm_unsat, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // `create_power_switch_rule` / `update_power_switch_rule` subscope + Fix 9 in-place replacement:
    let fix9_child = dir.join("fix9_child.cpf");
    std::fs::write(
        &fix9_child,
        r#"
        set_cpf_version 2.0
        create_power_domain -name PD_C -shutoff_condition {!pwr_en} -default
        create_power_switch_rule -name PSW_C -domain PD_C -external_power_net VDD_EXT -acknowledge_receiver ack1
        update_power_switch_rule -name PSW_C -enable_condition {sw_en && !iso_hold} -acknowledge_receiver {ack2}
        end_design
    "#,
    )
    .unwrap();
    let fix9_cpf = format!(
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {{VDD_EXT}} -voltage 0.9
        create_ground_nets -nets {{VSS}}
        create_power_domain -name PD_SW -shutoff_condition {{!pwr_en}} -default
        update_power_domain -name PD_SW -primary_power_net VDD_EXT -primary_ground_net VSS
        load_upf "{}" -scope u_sub -format cpf
        end_design
    "#,
        fix9_child.to_string_lossy().replace('\\', "\\\\")
    );
    let res_fix9 =
        parse_cpf_and_resolve(&fix9_cpf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let sw_fix9 = res_fix9
        .intent
        .power_switches
        .iter()
        .find(|s| s.name == "u_sub/PSW_C")
        .expect("u_sub/PSW_C");
    assert_eq!(sw_fix9.on_states[0].expr, "u_sub/sw_en && !u_sub/iso_hold");
    assert_eq!(
        sw_fix9.off_states[0].expr,
        "!(u_sub/sw_en && !u_sub/iso_hold)"
    );
    assert_eq!(
        sw_fix9.control_ports,
        vec![(
            "ctrl".to_string(),
            "u_sub/sw_en && !u_sub/iso_hold".to_string()
        )]
    );
    assert_eq!(
        sw_fix9.ack_ports,
        vec![("ack".to_string(), "u_sub/ack2".to_string())]
    );

    // Fix 8: Logic/boolean expressions under `set_hierarchy_separator :` (CPF + UPF + ternary + slice).
    let fix8_cpf = r#"
        set_cpf_version 2.0
        set_hierarchy_separator :
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off
        create_power_nets -nets {VDD} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_A -shutoff_condition {!u_sub:pwr_en} -default
        update_power_domain -name PD_A -primary_power_net VDD -primary_ground_net VSS
        create_power_switch_rule -name PSW_A -domain PD_A -enable_condition {u_sub:sw_en}
        create_isolation_rule -name ISO_A -from PD_A -isolation_condition {!u_sub:iso_en} -isolation_output low
        create_power_mode -name PM_A -domain_conditions {PD_A@NC_ON} -condition {u_sub:mode_ok} -default
        update_power_mode -name PM_A -condition {u_sub:mode_ok && !u_sub:halt && (sel ? u_sub:a : u_sub:b) && (u_sub:bus[3:0] == 4'hF)}
        end_design
    "#;
    let res_fix8 = parse_cpf_and_resolve(fix8_cpf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let iso_a = res_fix8
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.name == "ISO_A")
        .expect("ISO_A");
    assert_eq!(iso_a.isolation_signal.as_deref(), Some("u_sub/iso_en"));
    assert_eq!(iso_a.isolation_sense, Some(Sense::Low));
    let sw_a = res_fix8
        .intent
        .power_switches
        .iter()
        .find(|s| s.name == "PSW_A")
        .expect("PSW_A");
    assert_eq!(sw_a.on_states[0].expr, "u_sub/sw_en");
    let pst_a = res_fix8.intent.pst_tables.first().expect("cpf_power_modes");
    assert_eq!(
        pst_a.rows[0].logic_expr.as_deref(),
        Some(
            "u_sub/mode_ok && !u_sub/halt && (sel ? u_sub/a : u_sub/b) && (u_sub/bus[3:0] == 4'hF)"
        )
    );
    assert!(PstSat::build(&res_fix8, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    let fix8_upf = r#"
        upf_version 2.1
        ::cpf::set_hierarchy_separator :
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD -include_scope -supply {primary SS}
        add_power_state SS -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {u_sub:en == 1'b1} -simstate NORMAL}
        add_power_state SS -state {BAD -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {u_sub:en == 1'b0} -illegal}
    "#;
    let res_fix8_upf =
        parse_upf_and_resolve(fix8_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert_eq!(
        res_fix8_upf.intent.power_states[0].logic_expr.as_deref(),
        Some("u_sub/en == 1'b1")
    );
    assert!(PstSat::build(&res_fix8_upf, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    let _ = std::fs::remove_dir_all(&dir);
}
