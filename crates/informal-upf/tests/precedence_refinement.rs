mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::{temp_dir, MockDesignFinder};
use informal_upf::{
    parse_upf_and_resolve, AppliesTo, ClampValue, DiagSeverity, LsRequirement, NullObjectFinder,
    ObjectFinder, PstSat, Sense, SignalEndpoint, SimState, StrategyLocation, StrategyMatch,
    DEFAULT_VOLTAGE_TOL_UV,
};

#[test]
fn forward_references_update_exclude_and_4tier_precedence() {
    let upf = r#"
        upf_version 3.1
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_CORE -elements {u_core} -supply {primary SS_CORE}
        create_supply_net VDD_AON
        create_supply_net VDD_CORE
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_CORE -function {power VDD_CORE} -function {ground VSS}
        associate_supply_set SS_AON -handle PD_CORE.default_isolation

        set_isolation iso_default -domain PD_CORE -applies_to outputs -clamp_value 0 -isolation_signal iso_en -isolation_sense high
        set_isolation iso_default -domain PD_CORE -location parent -update
        set_isolation iso_src_sink -domain PD_CORE -source SS_CORE -sink SS_AON -clamp_value 1 -isolation_signal iso_en -isolation_sense high
        set_isolation iso_no_sig -domain PD_CORE -elements {u_core/dbg_bus} -no_isolation
        set_isolation iso_elem -domain PD_CORE -elements {u_core/data*} -exclude_elements {u_core/data_bypass} -clamp_value latch
    "#;

    let res = parse_upf_and_resolve(upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let pd_core = res.domain_by_name["PD_CORE"];
    let pd_top = res.domain_by_name["PD_TOP"];

    let m_src_sink = res.match_isolation(
        Some(pd_core),
        Some(pd_top),
        "u_core/ctrl_sig",
        AppliesTo::Outputs,
    );
    let iso_ss = m_src_sink.active().unwrap();
    assert_eq!(iso_ss.name, "iso_src_sink");
    assert_eq!(iso_ss.clamp_value, Some(ClampValue::One));
    assert_eq!(
        res.effective_iso_supply(iso_ss),
        Some(res.ss_by_name["SS_AON"])
    );

    let m_no = res.match_isolation(
        Some(pd_core),
        Some(pd_top),
        "u_core/dbg_bus",
        AppliesTo::Outputs,
    );
    assert!(m_no.is_suppressed());

    let m_latch = res.match_isolation(
        Some(pd_core),
        Some(pd_top),
        "u_core/data_out",
        AppliesTo::Outputs,
    );
    assert_eq!(
        m_latch.active().unwrap().clamp_value,
        Some(ClampValue::Latch)
    );

    let m_excl = res.match_isolation(
        Some(pd_core),
        Some(pd_top),
        "u_core/data_bypass",
        AppliesTo::Outputs,
    );
    assert_eq!(m_excl.active().unwrap().name, "iso_src_sink");

    let default_iso = res
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.name == "iso_default")
        .unwrap();
    assert_eq!(default_iso.location, Some(StrategyLocation::Parent));
    assert_eq!(default_iso.isolation_sense, Some(Sense::High));
}

#[test]
fn domain_conflict_and_unresolved_target() {
    let finder: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_a", "u_b"],
        &["u_a/u_shared", "u_b/u_only"],
    ));
    let conflict_upf = r#"
        upf_version 2.1
        create_power_domain PD_A -elements {u_a/u_shared}
        create_power_domain PD_B -elements {u_a/u_shared u_missing}
    "#;
    let res = parse_upf_and_resolve(conflict_upf, None, None, finder).unwrap();
    assert!(res
        .intent
        .diagnostics
        .iter()
        .any(|d| d.rule_id == "UPF_DOMAIN_CONFLICT"));
    assert!(res
        .intent
        .diagnostics
        .iter()
        .any(|d| d.rule_id == "UPF_UNRESOLVED_TARGET"));
}

#[test]
fn resolve_precedence_updates_overlaps_and_lookup() {
    let upf_strat = r#"
        upf_version 2.1
        create_supply_net VDD_A
        create_supply_net VDD_B
        create_supply_net VDD_C
        create_supply_net VSS
        create_supply_set SS_A -function {power VDD_A} -function {ground VSS}
        create_supply_set SS_B -function {power VDD_B} -function {ground VSS}
        create_supply_set SS_C -function {power VDD_C} -function {ground VSS}
        create_power_domain PD_A -elements {u_a} -supply {primary SS_A}
        create_power_domain PD_B -elements {u_b} -supply {primary SS_B}
        create_power_domain PD_C -elements {u_c} -supply {primary SS_C}

        set_isolation iso_b -domain PD_B -clamp_value 0
        set_isolation iso_a_c -domain PD_A -elements {u_a/sig_c*} -sink PD_C -clamp_value 1

        set_level_shifter ls_b -domain PD_B -rule low_to_high
        set_level_shifter ls_b -domain PD_B -update -force_shift -source SS_A -sink SS_B -internal_supply_set SS_C

        set_isolation iso_upd -domain PD_A -clamp_value 0
        set_isolation iso_upd -domain PD_A -update -force_isolation -diff_supply_only -applies_to_clamp 1
        set_retention ret_upd -domain PD_A -save_signal {sav high}
        set_retention ret_upd -domain PD_A -update -use_retention_as_primary -retention_power_net VDD_C

        set_level_shifter ls_ov1 -domain PD_C -rule low_to_high
        set_level_shifter ls_ov2 -domain PD_C -rule high_to_low
        set_retention ret_ov1 -domain PD_C -save_signal {s1 high}
        set_retention ret_ov2 -domain PD_C -save_signal {s2 low}

        set_scope u_a
        create_supply_net VDD_IN_A
        set_scope u_sub
        create_supply_set SS_LEAF -function {power VDD_IN_A} -function {ground VSS}
        set_isolation_control iso_upd -domain PD_A -isolation_signal iso_en -isolation_sense high
        set_scope /
    "#;
    let res_s = parse_upf_and_resolve(upf_strat, None, None, Arc::new(NullObjectFinder)).unwrap();
    let pd_a = res_s.domain_by_name["PD_A"];
    let pd_b = res_s.domain_by_name["PD_B"];
    let pd_c = res_s.domain_by_name["PD_C"];

    assert!(matches!(
        res_s.match_isolation(Some(pd_c), Some(pd_b), "u_c/x", AppliesTo::Both),
        StrategyMatch::None
    ));
    assert!(matches!(
        res_s.match_isolation(Some(pd_c), Some(pd_b), "u_c/x", AppliesTo::Inputs),
        StrategyMatch::None
    ));
    assert_ne!(
        res_s
            .match_isolation(Some(pd_a), Some(pd_b), "u_a/sig_c_0", AppliesTo::Outputs)
            .active()
            .map(|i| i.name.as_str()),
        Some("iso_a_c")
    );
    assert_eq!(
        res_s
            .match_level_shifter(Some(pd_a), Some(pd_b), "u_b/in", AppliesTo::Inputs)
            .active()
            .map(|l| l.name.as_str()),
        Some("ls_b")
    );

    let iso_upd = res_s
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.name == "iso_upd")
        .unwrap();
    assert!(iso_upd.force_isolation);
    assert!(iso_upd.diff_supply_only);
    assert_eq!(iso_upd.applies_to_clamp, Some(ClampValue::One));
    assert_eq!(iso_upd.isolation_sense, Some(Sense::High));
    assert_eq!(
        iso_upd.isolation_signal.as_deref(),
        Some("u_a/u_sub/iso_en")
    );

    let ls_b = res_s
        .intent
        .level_shifter_strategies
        .iter()
        .find(|l| l.name == "ls_b")
        .unwrap();
    assert!(ls_b.force_shift);
    assert_eq!(ls_b.source_filter.as_deref(), Some("SS_A"));
    assert_eq!(ls_b.sink_filter.as_deref(), Some("SS_B"));
    assert_eq!(ls_b.internal_supply_set.as_deref(), Some("SS_C"));

    let ret_upd = res_s
        .intent
        .retention_strategies
        .iter()
        .find(|r| r.name == "ret_upd")
        .unwrap();
    assert!(ret_upd.use_retention_as_primary);
    assert_eq!(ret_upd.retention_power_net.as_deref(), Some("VDD_C"));

    let overlap_count = res_s
        .intent
        .diagnostics
        .iter()
        .filter(|d| d.rule_id == "UPF_STRATEGY_OVERLAP")
        .count();
    assert!(
        overlap_count >= 2,
        "expected LS and RET overlaps, got {:?}",
        res_s.intent.diagnostics
    );

    let ss_leaf = res_s.ss_by_name["u_a/u_sub/SS_LEAF"];
    assert_eq!(
        res_s.ss_power_net[ss_leaf.0 as usize],
        Some(res_s.net_by_name["u_a/VDD_IN_A"])
    );

    let finder: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_core", "u_core/u_leaf"],
        &["u_top_misc", "u_core", "u_core/u_leaf"],
    ));
    let memb_upf = r#"
        upf_version 2.1
        create_power_domain PD_CORE -elements {u_core}
        create_power_domain PD_TOP -include_scope
        create_power_domain PD_CHILD -scope u_core -include_scope
    "#;
    let res_m = parse_upf_and_resolve(memb_upf, None, None, finder).unwrap();
    let id_core = res_m.domain_by_name["PD_CORE"];
    let id_top = res_m.domain_by_name["PD_TOP"];
    let id_child = res_m.domain_by_name["PD_CHILD"];
    assert!(res_m.domain_instances[id_core.0 as usize].contains("u_core"));
    assert!(!res_m.domain_instances[id_top.0 as usize].contains("u_core"));
    assert!(!res_m.domain_instances[id_child.0 as usize].contains("u_core"));
    assert_eq!(res_m.instance_domain["u_core"], id_core);
}

#[test]
fn regression_r2_lookup_scoped_no_false_tail_strip_and_power_state_update_merge() {
    let bad_prefix = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS}
        set_isolation iso_bad -domain u_nonexistent/PD_TOP -clamp_value 0
    "#;
    let res_bad =
        parse_upf_and_resolve(bad_prefix, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(
        res_bad
            .intent
            .diagnostics
            .iter()
            .any(|d| d.rule_id == "UPF_UNRESOLVED_TARGET" && d.severity == DiagSeverity::Error),
        "expected UPF_UNRESOLVED_TARGET for u_nonexistent/PD_TOP, got {:?}",
        res_bad.intent.diagnostics
    );

    let dir = temp_dir("grandchild_lookup");
    let leaf = dir.join("leaf.upf");
    let mid = dir.join("mid.upf");
    let top = dir.join("top.upf");
    std::fs::write(
        &leaf,
        r#"
        upf_version 2.1
        create_power_domain PD_LEAF -include_scope
        set_domain_supply_net PD_LEAF -primary_power_net u_a/VDD_A -primary_ground_net VSS_TOP
    "#,
    )
    .unwrap();
    std::fs::write(
        &mid,
        format!(
            "upf_version 2.1\ncreate_supply_net VDD_A\nload_upf {} -scope u_b\n",
            leaf.display()
        ),
    )
    .unwrap();
    std::fs::write(
        &top,
        format!(
            "upf_version 2.1\ncreate_supply_net VSS_TOP\nload_upf {} -scope u_a\n",
            mid.display()
        ),
    )
    .unwrap();
    let res_gc = informal_upf::load_upf(&top, None, None, Arc::new(NullObjectFinder)).unwrap();
    let leaf_ss = res_gc
        .primary_ss(res_gc.domain_by_name["u_a/u_b/PD_LEAF"])
        .unwrap();
    assert!(res_gc.ss_power_net[leaf_ss.0 as usize].is_some());
    assert!(res_gc.ss_ground_net[leaf_ss.0 as usize].is_some());
    assert!(
        res_gc.intent.diagnostics.is_empty(),
        "{:?}",
        res_gc.intent.diagnostics
    );
    let _ = std::fs::remove_dir_all(&dir);

    let upd = r#"
        upf_version 2.1
        create_supply_net VDD_A
        create_supply_net VDD_B
        create_supply_net VSS
        create_supply_set SS_A -function {power VDD_A} -function {ground VSS}
        create_supply_set SS_B -function {power VDD_B} -function {ground VSS}
        add_power_state SS_A -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_A -state {ON -update -logic_expr {en == 1}}
        add_power_state SS_B -state {ON -supply_expr {power == {FULL_ON 1.2} && ground == {FULL_ON 0.0}} -simstate NORMAL}
    "#;
    let res_upd = parse_upf_and_resolve(upd, None, None, Arc::new(NullObjectFinder)).unwrap();
    let ss_a = res_upd.ss_by_name["SS_A"];
    let ss_b = res_upd.ss_by_name["SS_B"];
    let st_on = &res_upd.ss_states[ss_a.0 as usize][0];
    assert_eq!(st_on.simstate, Some(SimState::Normal));
    assert!(st_on.supply_expr.is_some());
    assert_eq!(st_on.logic_expr.as_deref(), Some("en == 1"));
    assert!(!st_on.rail_states.is_empty());

    let mut sat_upd = PstSat::build(&res_upd, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_upd.is_state_space_consistent());
    assert!(matches!(
        sat_upd.voltage_relation(ss_a, ss_b, DEFAULT_VOLTAGE_TOL_UV),
        LsRequirement::LowToHigh { .. }
    ));
}

#[test]
fn regression_r11_associate_supply_set_handles_and_rtl_bracket_membership() {
    let dir = temp_dir("r11_handles");
    let sub_file = dir.join("sub_handles.upf");
    std::fs::write(
        &sub_file,
        r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS_SUB -function {power VDD} -function {ground VSS}
        create_power_domain PD_SUB -include_scope -supply {primary SS_SUB}
        add_power_state SS_SUB -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
    "#,
    )
    .unwrap();
    let r10_1_upf = format!(
        r#"
        upf_version 2.1
        ::cpf::set_hierarchy_separator .
        create_supply_net VDD_TOP
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_TOP -function {{power VDD_TOP}} -function {{ground VSS}}
        create_supply_set SS_SW  -function {{power VDD_SW}}  -function {{ground VSS}}
        create_power_domain PD_TOP -include_scope -supply {{primary SS_TOP}}
        create_power_domain PD_SW  -elements {{u_sw}} -supply {{primary SS_SW}}
        create_power_domain PD_IMP -elements {{u_imp}}
        set_domain_supply_net PD_IMP -primary_power_net VDD_TOP -primary_ground_net VSS
        add_power_state SS_TOP -state {{ON -supply_expr {{power == {{FULL_ON 0.9}} && ground == {{FULL_ON 0.0}}}} -simstate NORMAL}}
        add_power_state SS_SW  -state {{ON -supply_expr {{power == {{FULL_ON 0.9}} && ground == {{FULL_ON 0.0}}}} -simstate NORMAL}}
        add_power_state SS_SW  -state {{OFF -supply_expr {{power == OFF && ground == {{FULL_ON 0.0}}}} -simstate CORRUPT}}
        add_power_state PD_IMP.primary -state {{ON -supply_expr {{power == {{FULL_ON 0.9}} && ground == {{FULL_ON 0.0}}}} -simstate NORMAL}}
        load_upf "{sub}" -scope u_sub
        associate_supply_set PD_TOP.primary -handle PD_SW.default_isolation
        associate_supply_set PD_IMP.primary -handle PD_SW.default_retention
        create_pst PST_MAIN -supplies {{PD_TOP.primary PD_SW.primary PD_SW.default_isolation u_sub.PD_SUB.primary u_sub.VDD}}
        add_port_state u_sub.VDD -state {{ON 0.9}}
        add_pst_state S_ON  -pst PST_MAIN -state {{ON ON  ON ON ON}}
        add_pst_state S_OFF -pst PST_MAIN -state {{ON OFF ON ON ON}}
        set_scope u_sub
        associate_supply_set /PD_TOP.primary -handle PD_SUB.default_isolation
        set_scope /
    "#,
        sub = sub_file.display()
    );
    let finder_r10_1: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sw", "u_imp", "u_sub"],
        &["u_sw/u_reg", "u_imp/u_reg", "u_sub/u_reg"],
    ));
    let res_r10_1 = parse_upf_and_resolve(&r10_1_upf, None, None, finder_r10_1).unwrap();
    assert!(
        !res_r10_1
            .intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error),
        "unexpected R10-1 error diags: {:?}",
        res_r10_1.intent.diagnostics
    );
    let mut sat_r10_1 = PstSat::build(&res_r10_1, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        sat_r10_1.is_state_space_consistent(),
        "R10-1 PST should be consistent, diags: {:?}",
        sat_r10_1.diagnostics()
    );
    assert!(!sat_r10_1
        .diagnostics()
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));
    let _ = std::fs::remove_dir_all(&dir);

    let finder_r10_4: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_sub/u_arr[0]"],
        &["u_sub/bus[0]", "u_sub/u_arr[0]/sig", "u_sub/u_arr[0]/sig2"],
    ));
    let r10_4_upf = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        set_scope u_sub
        create_power_domain PD_BUS -elements {bus[0] u_arr[0]/sig} -supply {primary /SS}
        set_isolation iso_a -domain PD_BUS -elements {u_arr[0]/sig} -isolation_supply_set /SS -clamp_value 0
        set_retention ret_a -domain PD_BUS -elements {u_arr[0]/sig} -retention_supply_set /SS -save_signal {save high}
        create_power_domain PD_WILDCARD -elements {u_arr[0]/*} -exclude_elements {u_arr[0]/sig} -supply {primary /SS}
        set_scope /
    "#;
    let res_r10_4 = parse_upf_and_resolve(r10_4_upf, None, None, finder_r10_4).unwrap();
    assert!(!res_r10_4
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));
    let pd_bus_4 = &res_r10_4.intent.domains[res_r10_4.domain_by_name["u_sub/PD_BUS"].0 as usize];
    assert!(res_r10_4.domain_instances[pd_bus_4.id.0 as usize].contains("u_sub/bus[0]"));
    assert!(res_r10_4.domain_instances[pd_bus_4.id.0 as usize].contains("u_sub/u_arr[0]/sig"));
    let pd_wc_4 =
        &res_r10_4.intent.domains[res_r10_4.domain_by_name["u_sub/PD_WILDCARD"].0 as usize];
    assert!(res_r10_4.domain_instances[pd_wc_4.id.0 as usize].contains("u_sub/u_arr[0]/sig2"));
    assert!(!res_r10_4.domain_instances[pd_wc_4.id.0 as usize].contains("u_sub/u_arr[0]/sig"));

    let finder_coll: Arc<dyn ObjectFinder> =
        Arc::new(MockDesignFinder::new(&["u_sub"], &["u_sub/bus0"]));
    let coll_upf = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        set_scope u_sub
        create_power_domain PD_MISS -elements {bus[0]} -supply {primary /SS}
        create_power_domain PD_KEEP -elements {bus[0-1]} -exclude_elements {bus[0]} -supply {primary /SS}
        set_scope /
    "#;
    let res_coll = parse_upf_and_resolve(coll_upf, None, None, finder_coll).unwrap();
    assert!(res_coll.intent.diagnostics.iter().any(|d| {
        d.severity == DiagSeverity::Error
            && d.rule_id == "UPF_UNRESOLVED_TARGET"
            && d.message.contains("u_sub/bus[0]")
    }));
    let pd_miss = &res_coll.intent.domains[res_coll.domain_by_name["u_sub/PD_MISS"].0 as usize];
    assert!(res_coll.domain_instances[pd_miss.id.0 as usize].is_empty());
    let pd_keep = &res_coll.intent.domains[res_coll.domain_by_name["u_sub/PD_KEEP"].0 as usize];
    assert!(res_coll.domain_instances[pd_keep.id.0 as usize].contains("u_sub/bus0"));
}

#[test]
fn regression_r12_resolve_rtl_brackets_and_strategy_elements() {
    let finder_wc: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_sub/u_arr0"],
        &["u_sub/u_arr0/sig", "u_sub/u_arr0/sig2"],
    ));
    let upf_wc = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        set_scope u_sub
        create_power_domain PD_ARR -elements {u_arr[0]/*} -supply {primary /SS}
        create_power_domain PD_EXCL -elements {u_arr0/*} -exclude_elements {u_arr[0]/*} -supply {primary /SS}
        set_scope /
    "#;
    let res_wc = parse_upf_and_resolve(upf_wc, None, None, finder_wc).unwrap();
    assert!(res_wc.intent.diagnostics.iter().any(|d| {
        d.severity == DiagSeverity::Error
            && d.rule_id == "UPF_UNRESOLVED_TARGET"
            && d.message.contains("u_sub/u_arr[0]/*")
    }));
    let pd_arr = &res_wc.intent.domains[res_wc.domain_by_name["u_sub/PD_ARR"].0 as usize];
    assert!(res_wc.domain_instances[pd_arr.id.0 as usize].is_empty());
    let pd_excl = &res_wc.intent.domains[res_wc.domain_by_name["u_sub/PD_EXCL"].0 as usize];
    assert!(res_wc.domain_instances[pd_excl.id.0 as usize].contains("u_sub/u_arr0/sig"));
    assert!(res_wc.domain_instances[pd_excl.id.0 as usize].contains("u_sub/u_arr0/sig2"));

    let finder_mixed: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_sub/u_arr[0]", "u_sub/u_arr0"],
        &[
            "u_sub/u_arr[0]/bus0",
            "u_sub/u_arr[0]/bus1",
            "u_sub/u_arr[0]/bus2",
            "u_sub/u_arr0/bus0",
        ],
    ));
    let upf_mixed = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        set_scope u_sub
        create_power_domain PD_MIXED -elements {u_arr[0]/bus[0-1]} -supply {primary /SS}
        create_power_domain PD_MIX_EXCL -elements {u_arr[0]/*} -exclude_elements {u_arr[0]/bus[0-1]} -supply {primary /SS}
        set_scope /
    "#;
    let res_mixed = parse_upf_and_resolve(upf_mixed, None, None, finder_mixed).unwrap();
    assert!(!res_mixed
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));
    let pd_mixed = &res_mixed.intent.domains[res_mixed.domain_by_name["u_sub/PD_MIXED"].0 as usize];
    let mixed_insts = &res_mixed.domain_instances[pd_mixed.id.0 as usize];
    assert!(mixed_insts.contains("u_sub/u_arr[0]/bus0"));
    assert!(mixed_insts.contains("u_sub/u_arr[0]/bus1"));
    assert!(!mixed_insts.contains("u_sub/u_arr0/bus0"));
    assert!(!mixed_insts.contains("u_sub/u_arr[0]/bus2"));
    let pd_mix_excl =
        &res_mixed.intent.domains[res_mixed.domain_by_name["u_sub/PD_MIX_EXCL"].0 as usize];
    assert_eq!(
        res_mixed.domain_instances[pd_mix_excl.id.0 as usize],
        BTreeSet::from(["u_sub/u_arr[0]/bus2".to_string()])
    );

    let finder_ls_rep: Arc<dyn ObjectFinder> =
        Arc::new(MockDesignFinder::new(&["u_sub"], &["u_sub/bus0"]));
    let upf_ls_rep = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        set_scope u_sub
        create_power_domain PD -elements {bus0} -supply {primary /SS}
        set_level_shifter ls_miss -domain PD -elements {bus[0]}
        set_repeater rep_miss -domain PD -elements {bus[0]}
        set_repeater rep_bad_dom -domain PD_UNKNOWN
        set_scope /
    "#;
    let res_ls_rep = parse_upf_and_resolve(upf_ls_rep, None, None, finder_ls_rep).unwrap();
    for msg_part in [
        "set_level_shifter `ls_miss` element `u_sub/bus[0]`",
        "set_repeater `rep_miss` element `u_sub/bus[0]`",
        "set_repeater `rep_bad_dom` references unknown domain `u_sub/PD_UNKNOWN`",
    ] {
        assert!(res_ls_rep.intent.diagnostics.iter().any(|d| {
            d.severity == DiagSeverity::Error
                && d.rule_id == "UPF_UNRESOLVED_TARGET"
                && d.message.contains(msg_part)
        }));
    }
}

#[test]
fn regression_r15_top_dot_membership_and_r18_char_class_child_signal() {
    let finder_r14_1: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub"],
        &["top_inst", "u_sub/sub_inst"],
    ));
    let upf_dot = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD_TOP -elements {.} -supply {primary SS}
        set_isolation iso_top -domain PD_TOP -isolation_supply_set SS -clamp_value 0 -elements {.}
        set_level_shifter ls_top -domain PD_TOP -elements {.}
        set_retention ret_top -domain PD_TOP -retention_supply_set SS -elements {.}
        create_power_domain PD_OTHER -elements {top_inst} -supply {primary SS}
        set_isolation iso_other -domain PD_OTHER -isolation_supply_set SS -clamp_value 0 -elements {u_sub/sub_inst}
        set_scope u_sub
        create_power_domain PD_SUB -elements {.} -supply {primary /SS}
        set_isolation iso_sub -domain PD_SUB -isolation_supply_set /SS -clamp_value 0 -elements {.}
        set_scope /
    "#;
    let res_dot = parse_upf_and_resolve(upf_dot, None, None, Arc::clone(&finder_r14_1)).unwrap();
    assert!(!res_dot
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));
    let pd_top = res_dot.domain_by_name["PD_TOP"];
    let pd_other = res_dot.domain_by_name["PD_OTHER"];
    let pd_sub = res_dot.domain_by_name["u_sub/PD_SUB"];
    assert!(
        res_dot.domain_instances[pd_top.0 as usize].is_empty(),
        "scoped PD_SUB -elements {{.}} inherits u_sub/sub_inst transitively: {:?}",
        res_dot.domain_instances[pd_top.0 as usize]
    );
    assert_eq!(
        res_dot.domain_instances[pd_sub.0 as usize],
        BTreeSet::from(["u_sub".to_string(), "u_sub/sub_inst".to_string()])
    );
    assert_eq!(
        res_dot.domain_instances[pd_other.0 as usize],
        BTreeSet::from(["top_inst".to_string()])
    );
    assert!(matches!(
        res_dot.match_isolation(Some(pd_top), Some(pd_other), "top_inst/out_sig", AppliesTo::Outputs),
        StrategyMatch::Matched(s) if s.name == "iso_top"
    ));
    assert!(matches!(
        res_dot.match_level_shifter(Some(pd_top), Some(pd_other), "top_inst/out_sig", AppliesTo::Outputs),
        StrategyMatch::Matched(s) if s.name == "ls_top"
    ));
    assert!(matches!(
        res_dot.match_retention(pd_top, "top_inst/reg_q"),
        StrategyMatch::Matched(s) if s.name == "ret_top"
    ));
    assert!(matches!(
        res_dot.match_isolation(Some(pd_sub), Some(pd_top), "u_sub/out_sig", AppliesTo::Outputs),
        StrategyMatch::Matched(s) if s.name == "iso_sub"
    ));
    assert!(matches!(
        res_dot.match_isolation(
            Some(pd_other),
            Some(pd_top),
            "top_inst/out_sig",
            AppliesTo::Outputs
        ),
        StrategyMatch::None
    ));

    let upf_dot_excl = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD_ALL -elements {.} -supply {primary SS}
        create_power_domain PD_NONE -include_scope -exclude_elements {.} -supply {primary SS}
        set_isolation iso_excl -domain PD_ALL -isolation_supply_set SS -clamp_value 0 -elements {.} -exclude_elements {.}
        set_retention ret_excl -domain PD_ALL -retention_supply_set SS -elements {.} -exclude_elements {.}
    "#;
    let res_excl = parse_upf_and_resolve(upf_dot_excl, None, None, finder_r14_1).unwrap();
    let pd_all = res_excl.domain_by_name["PD_ALL"];
    let pd_none = res_excl.domain_by_name["PD_NONE"];
    assert_eq!(
        res_excl.domain_instances[pd_all.0 as usize],
        BTreeSet::from(["top_inst".to_string(), "u_sub/sub_inst".to_string()])
    );
    assert!(res_excl.domain_instances[pd_none.0 as usize].is_empty());
    assert!(matches!(
        res_excl.match_isolation(Some(pd_all), None, "top_inst/sig", AppliesTo::Outputs),
        StrategyMatch::None
    ));
    assert!(matches!(
        res_excl.match_retention(pd_all, "top_inst/reg_q"),
        StrategyMatch::None
    ));

    // D2: `-elements {u_arr[0-1]}` matches child signal `u_arr0/sig`.
    let d2_finder: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_arr0", "u_arr1"],
        &["u_arr0/r", "u_arr1/r"],
    ));
    let d2_upf = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS_TOP -function {power VDD} -function {ground VSS}
        create_supply_set SS_ARR -function {power VDD} -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_TOP}
        create_power_domain PD_ARR -elements {u_arr[0-1]} -supply {primary SS_ARR}
        set_isolation iso_cc -domain PD_ARR -applies_to outputs -isolation_supply_set SS_TOP -isolation_signal iso_en -isolation_sense high -clamp_value 0 -elements {u_arr[0-1]}
    "#;
    let res_d2 = parse_upf_and_resolve(d2_upf, None, None, d2_finder).unwrap();
    let pd_arr = res_d2
        .domain_by_name
        .get("PD_ARR")
        .copied()
        .expect("PD_ARR");
    let m_d2 = res_d2.match_isolation(Some(pd_arr), None, "u_arr0/sig", AppliesTo::Outputs);
    assert!(
        matches!(m_d2, StrategyMatch::Matched(_)),
        "expected child signal `u_arr0/sig` to match `-elements {{u_arr[0-1]}}`, got {:?}",
        m_d2
    );
}

#[test]
fn scoped_dot_elements_honor_transitive_true_and_explicit_false() {
    let finder: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_mid", "u_mid/u_leaf"],
        &["u_mid", "u_mid/u_leaf", "u_mid/u_leaf/u_deep"],
    ));
    let upf_trans = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS}
        set_scope u_mid
        create_power_domain PD_MID -elements {.} -supply {primary /SS}
        set_scope /
    "#;
    let res = parse_upf_and_resolve(upf_trans, None, None, Arc::clone(&finder)).unwrap();
    let pd_mid = res.domain_by_name["u_mid/PD_MID"];
    assert_eq!(
        res.domain_instances[pd_mid.0 as usize],
        BTreeSet::from([
            "u_mid".to_string(),
            "u_mid/u_leaf".to_string(),
            "u_mid/u_leaf/u_deep".to_string(),
        ])
    );

    let upf_nontrans = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS}
        set_scope u_mid
        create_power_domain PD_MID -elements {.} -transitive FALSE -supply {primary /SS}
        set_scope /
    "#;
    let res_nt = parse_upf_and_resolve(upf_nontrans, None, None, finder).unwrap();
    let pd_mid_nt = res_nt.domain_by_name["u_mid/PD_MID"];
    assert_eq!(
        res_nt.domain_instances[pd_mid_nt.0 as usize],
        BTreeSet::from(["u_mid".to_string()])
    );
}

#[test]
fn crossing_exclude_elements_slice_and_base_coverage() {
    let upf = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_SRC -elements {u_src} -supply {primary SS_SW}
        create_power_domain PD_DST -elements {u_dst} -supply {primary SS_AON}
        set_isolation iso_s -domain PD_SRC -applies_to outputs -elements {u_src/out_bus} -exclude_elements {u_src/out_bus[3:0]} -clamp_value 0 -isolation_supply_set SS_AON
        set_isolation iso_whole -domain PD_DST -applies_to inputs -exclude_elements {u_dst/d} -clamp_value 0 -isolation_supply_set SS_AON
        set_level_shifter ls_s -domain PD_SRC -applies_to outputs -elements {u_src/out_bus} -exclude_elements {u_src/out_bus[3:0]} -rule low_to_high
    "#;
    let res = parse_upf_and_resolve(upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let pd_src = res.domain_by_name["PD_SRC"];
    let pd_dst = res.domain_by_name["PD_DST"];
    let src_lo = [SignalEndpoint {
        upf_path: "u_src/out_bus",
        full_path: "top.u_src.out_bus",
        esc_upf_path: None,
        width: 8,
        packed_bounds: Some((7, 0)),
        ranges: &[(0, 3)],
    }];
    let src_hi = [SignalEndpoint {
        upf_path: "u_src/out_bus",
        full_path: "top.u_src.out_bus",
        esc_upf_path: None,
        width: 8,
        packed_bounds: Some((7, 0)),
        ranges: &[(4, 7)],
    }];
    let dst_lo = [SignalEndpoint {
        upf_path: "u_dst/d",
        full_path: "top.u_dst.d",
        esc_upf_path: None,
        width: 8,
        packed_bounds: Some((7, 0)),
        ranges: &[(0, 3)],
    }];
    assert!(matches!(
        res.match_isolation_crossing(
            Some(pd_src),
            Some(pd_dst),
            &src_lo,
            &dst_lo,
            AppliesTo::Both
        ),
        StrategyMatch::None
    ));
    assert!(matches!(
        res.match_isolation_crossing(Some(pd_src), Some(pd_dst), &src_hi, &dst_lo, AppliesTo::Both),
        StrategyMatch::Matched(iso) if iso.name == "iso_s"
    ));
    assert!(matches!(
        res.match_level_shifter_crossing(
            Some(pd_src),
            Some(pd_dst),
            &src_lo,
            &dst_lo,
            AppliesTo::Both
        ),
        StrategyMatch::None
    ));
    assert!(matches!(
        res.match_level_shifter_crossing(Some(pd_src), Some(pd_dst), &src_hi, &dst_lo, AppliesTo::Both),
        StrategyMatch::Matched(ls) if ls.name == "ls_s"
    ));
    assert!(matches!(
        res.match_isolation(
            Some(pd_dst),
            Some(pd_src),
            "u_dst/d[3:0]",
            AppliesTo::Inputs
        ),
        StrategyMatch::None
    ));
}
