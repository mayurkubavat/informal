mod common;

use std::sync::Arc;

use common::MockDesignFinder;
use informal_upf::{
    compare_upf, compare_upf_with_config, parse_upf_and_resolve, CompareConfig, NullObjectFinder,
    UpfDiffKind, DEFAULT_VOLTAGE_TOL_UV,
};

#[test]
fn upf_10_vs_21_parity_and_compare_upf() {
    let upf_10 = r#"
        upf_version 1.0
        set_design_top chip
        create_supply_port VDD_AON -direction in
        create_supply_port VDD_CORE -direction in
        create_supply_port VSS -direction in
        create_supply_net VDD_AON
        create_supply_net VDD_CORE
        create_supply_net VSS
        connect_supply_net VDD_AON -ports VDD_AON
        connect_supply_net VDD_CORE -ports VDD_CORE
        connect_supply_net VSS -ports VSS
        create_power_domain PD_TOP -include_scope
        create_power_domain PD_CORE -elements {u_core}
        set_domain_supply_net PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        set_domain_supply_net PD_CORE -primary_power_net VDD_CORE -primary_ground_net VSS
        add_port_state VDD_AON -state {AON_ON 0.90}
        add_port_state VDD_CORE -state {CORE_ON 0.90} -state {CORE_OFF off}
        add_port_state VSS -state {GND 0.0}
        create_pst my_pst -supplies {VDD_AON VDD_CORE VSS}
        add_pst_state ALL_ON -pst my_pst -state {AON_ON CORE_ON GND}
        add_pst_state SLEEP  -pst my_pst -state {AON_ON CORE_OFF GND}
        set_isolation iso_core_v1 -domain PD_CORE -applies_to outputs -clamp_value 0 -isolation_power_net VDD_AON -isolation_ground_net VSS
        set_isolation_control iso_core_v1 -domain PD_CORE -isolation_signal iso_en -isolation_sense high -location parent
        set_retention ret_core_v1 -domain PD_CORE -retention_power_net VDD_AON -retention_ground_net VSS
        set_retention_control ret_core_v1 -domain PD_CORE -save_signal {save high} -restore_signal {restore high}
    "#;

    let upf_21 = r#"
        upf_version 2.1
        set_design_top chip
        create_supply_port VDD_AON -direction in
        create_supply_port VDD_CORE -direction in
        create_supply_port VSS -direction in
        create_supply_net VDD_AON
        create_supply_net VDD_CORE
        create_supply_net VSS
        connect_supply_net VDD_AON -ports VDD_AON
        connect_supply_net VDD_CORE -ports VDD_CORE
        connect_supply_net VSS -ports VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_CORE -function {power VDD_CORE} -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_CORE -elements {u_core} -supply {primary SS_CORE}
        add_power_state SS_AON -state {AON_ON -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_CORE -state {CORE_ON -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -logic_expr {pwr_en} -simstate NORMAL}
        add_power_state SS_CORE -state {CORE_OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {!pwr_en} -simstate CORRUPT}
        set_isolation iso_core_v2 -domain PD_CORE -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent -name_prefix ISO_
        set_retention ret_core_v2 -domain PD_CORE -retention_supply_set SS_AON -save_signal {save high} -restore_signal {restore high}
    "#;

    let r10 = parse_upf_and_resolve(upf_10, None, None, Arc::new(NullObjectFinder)).unwrap();
    let r21 = parse_upf_and_resolve(upf_21, None, None, Arc::new(NullObjectFinder)).unwrap();

    let cmp_eq = compare_upf(&r10, &r21, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        cmp_eq.equivalent,
        "UPF 1.0 and equivalent UPF 2.1 should compare equivalent, diffs: {:?}",
        cmp_eq.diffs
    );
    let cmp_cfg = compare_upf_with_config(&r10, &r21, &NullObjectFinder, &CompareConfig::default());
    assert!(cmp_cfg.equivalent);

    let upf_21_flip = upf_21.replace("-clamp_value 0", "-clamp_value 1");
    let r21_flip =
        parse_upf_and_resolve(&upf_21_flip, None, None, Arc::new(NullObjectFinder)).unwrap();
    let cmp_flip = compare_upf(&r10, &r21_flip, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!cmp_flip.equivalent);
    assert!(cmp_flip
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::IsolationStrategyMismatch));

    let upf_missing_dom = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
    "#;
    let r_miss =
        parse_upf_and_resolve(upf_missing_dom, None, None, Arc::new(NullObjectFinder)).unwrap();
    let cmp_miss = compare_upf(&r10, &r_miss, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!cmp_miss.equivalent);
    assert!(cmp_miss
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::MissingDomain));
}

#[test]
fn compare_retention_removal_and_corruptible_supply() {
    let golden = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_CORE
        create_supply_net VSS
        create_supply_set SS_AON  -function {power VDD_AON}  -function {ground VSS}
        create_supply_set SS_CORE -function {power VDD_CORE} -function {ground VSS}
        create_power_domain PD_TOP  -include_scope -supply {primary SS_AON}
        create_power_domain PD_CORE -elements {u_core} -supply {primary SS_CORE}
        add_power_state SS_AON  -state {ON  -supply_expr {power == {FULL_ON 0.9}} -simstate NORMAL}
        add_power_state SS_CORE -state {ON  -supply_expr {power == {FULL_ON 0.9}} -simstate NORMAL}
        add_power_state SS_CORE -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
        set_retention ret_c -domain PD_CORE -retention_supply_set SS_AON -save_signal {save high} -restore_signal {restore high}
    "#;
    let revised_bad_ret_sup = golden.replace(
        "-retention_supply_set SS_AON",
        "-retention_supply_set SS_CORE",
    );
    let rg = parse_upf_and_resolve(golden, None, None, Arc::new(NullObjectFinder)).unwrap();
    let rr = parse_upf_and_resolve(&revised_bad_ret_sup, None, None, Arc::new(NullObjectFinder))
        .unwrap();
    let cmp = compare_upf(&rg, &rr, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!cmp.equivalent);
    assert!(cmp
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::RetentionStrategyMismatch));
}

#[test]
fn regression_r5_compare_domain_slash_and_ss_handle() {
    let base = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_SW  -elements {u_sw} -supply {primary SS_SW}
        add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_SW  -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_SW  -state {OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -simstate CORRUPT}
    "#;

    let g_iso = format!(
        "{base}\nset_isolation iso_1 -domain /PD_SW -isolation_supply_set SS_AON -clamp_value 0 -isolation_signal iso_en -isolation_sense high -location self\n"
    );
    let r_iso_bad = format!(
        "{base}\nset_isolation iso_1 -domain /PD_SW -isolation_supply_set SS_SW -clamp_value 0 -isolation_signal iso_en -isolation_sense high -location self\n"
    );
    let cmp_iso_bad = compare_upf(
        &parse_upf_and_resolve(&g_iso, None, None, Arc::new(NullObjectFinder)).unwrap(),
        &parse_upf_and_resolve(&r_iso_bad, None, None, Arc::new(NullObjectFinder)).unwrap(),
        &NullObjectFinder,
        DEFAULT_VOLTAGE_TOL_UV,
    );
    assert!(!cmp_iso_bad.equivalent);
    assert!(cmp_iso_bad
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::IsolationStrategyMismatch));

    let g_ret = format!(
        "{base}\nset_retention ret_1 -domain /PD_SW -retention_supply_set SS_AON -save_signal {{save high}} -restore_signal {{restore high}}\n"
    );
    let r_ret_bad = format!(
        "{base}\nset_retention ret_1 -domain /PD_SW -retention_supply_set SS_SW -save_signal {{save high}} -restore_signal {{restore high}}\n"
    );
    let cmp_ret_bad = compare_upf(
        &parse_upf_and_resolve(&g_ret, None, None, Arc::new(NullObjectFinder)).unwrap(),
        &parse_upf_and_resolve(&r_ret_bad, None, None, Arc::new(NullObjectFinder)).unwrap(),
        &NullObjectFinder,
        DEFAULT_VOLTAGE_TOL_UV,
    );
    assert!(!cmp_ret_bad.equivalent);
    assert!(cmp_ret_bad
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::RetentionStrategyMismatch));

    let g_all = format!(
        "{base}\n\
         set_isolation iso_1 -domain PD_SW -isolation_supply_set SS_AON -clamp_value 0 -isolation_signal iso_en -isolation_sense high -location self\n\
         set_level_shifter ls_1 -domain PD_SW -applies_to inputs -input_supply_set PD_TOP.primary -output_supply_set PD_SW.primary -rule high_to_low -location self\n\
         set_retention ret_1 -domain PD_SW -retention_supply_set SS_AON -save_signal {{save high}} -restore_signal {{restore high}}\n"
    );
    let r_all = format!(
        "{base}\n\
         set_isolation iso_1 -domain /PD_SW -isolation_supply_set /SS_AON -clamp_value 0 -isolation_signal iso_en -isolation_sense high -location self\n\
         set_level_shifter ls_1 -domain /PD_SW -applies_to inputs -input_supply_set SS_AON -output_supply_set SS_SW -rule high_to_low -location self\n\
         set_retention ret_1 -domain /PD_SW -retention_supply_set /SS_AON -save_signal {{save high}} -restore_signal {{restore high}}\n"
    );
    let cmp_eq = compare_upf(
        &parse_upf_and_resolve(&g_all, None, None, Arc::new(NullObjectFinder)).unwrap(),
        &parse_upf_and_resolve(&r_all, None, None, Arc::new(NullObjectFinder)).unwrap(),
        &NullObjectFinder,
        DEFAULT_VOLTAGE_TOL_UV,
    );
    assert!(cmp_eq.equivalent, "unexpected diffs: {:?}", cmp_eq.diffs);
}

#[test]
fn compare_upf_symmetric_iso_and_ret_supply_adequacy() {
    let base = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_SW  -elements {u_sw} -supply {primary SS_SW}
        add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9}} -simstate NORMAL}
        add_power_state SS_SW  -state {ON  -supply_expr {power == {FULL_ON 0.9}} -simstate NORMAL}
        add_power_state SS_SW  -state {OFF -supply_expr {power == OFF}           -simstate CORRUPT}
    "#;

    let iso_good = parse_upf_and_resolve(
        &format!(
            "{base}\nset_isolation iso1 -domain PD_SW -isolation_supply_set SS_AON -clamp_value 0\n"
        ),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let iso_bad = parse_upf_and_resolve(
        &format!(
            "{base}\nset_isolation iso1 -domain PD_SW -isolation_supply_set SS_SW -clamp_value 0\n"
        ),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    assert!(
        !compare_upf(
            &iso_good,
            &iso_bad,
            &NullObjectFinder,
            DEFAULT_VOLTAGE_TOL_UV
        )
        .equivalent
    );
    assert!(
        !compare_upf(
            &iso_bad,
            &iso_good,
            &NullObjectFinder,
            DEFAULT_VOLTAGE_TOL_UV
        )
        .equivalent
    );

    let ret_good = parse_upf_and_resolve(
        &format!("{base}\nset_retention ret1 -domain PD_SW -retention_supply_set SS_AON -save_signal {{save high}} -restore_signal {{restore high}}\n"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let ret_bad = parse_upf_and_resolve(
        &format!("{base}\nset_retention ret1 -domain PD_SW -retention_supply_set SS_SW -save_signal {{save high}} -restore_signal {{restore high}}\n"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    assert!(
        !compare_upf(
            &ret_good,
            &ret_bad,
            &NullObjectFinder,
            DEFAULT_VOLTAGE_TOL_UV
        )
        .equivalent
    );
    assert!(
        !compare_upf(
            &ret_bad,
            &ret_good,
            &NullObjectFinder,
            DEFAULT_VOLTAGE_TOL_UV
        )
        .equivalent
    );

    let no_ret_a = parse_upf_and_resolve(
        &format!(
            "{base}\nset_retention ret1 -domain PD_SW -no_retention -retention_supply_set SS_AON\n"
        ),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let no_ret_b = parse_upf_and_resolve(
        &format!(
            "{base}\nset_retention ret1 -domain PD_SW -no_retention -retention_supply_set SS_SW\n"
        ),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let cmp_noret = compare_upf(
        &no_ret_a,
        &no_ret_b,
        &NullObjectFinder,
        DEFAULT_VOLTAGE_TOL_UV,
    );
    assert!(cmp_noret.equivalent, "diffs: {:?}", cmp_noret.diffs);
}

#[test]
fn compare_upf_supply_ref_eq_canonical_filter_and_live_finder_fallback() {
    let mk = |net_name: &str| {
        format!(
            r#"
            upf_version 2.1
            create_supply_net VDD_AON
            create_supply_net {net_name}
            create_supply_net VSS
            create_supply_set SS_AON -function {{power VDD_AON}} -function {{ground VSS}}
            create_supply_set SS_X   -function {{power {net_name}}} -function {{ground VSS}}
            create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
            create_power_domain PD_SW  -elements {{u_sw}} -supply {{primary SS_X}}
            add_power_state SS_AON -state {{ON -supply_expr {{power == {{FULL_ON 0.9}}}} -simstate NORMAL}}
            add_power_state SS_X   -state {{ON -supply_expr {{power == {{FULL_ON 0.7}}}} -simstate NORMAL}}
            set_level_shifter ls1 -domain PD_SW -applies_to inputs -input_supply_set SS_X -output_supply_set SS_AON -rule low_to_high
        "#
        )
    };
    let r1 = parse_upf_and_resolve(&mk("VDD_1"), None, None, Arc::new(NullObjectFinder)).unwrap();
    let r2 = parse_upf_and_resolve(&mk("VDD_2"), None, None, Arc::new(NullObjectFinder)).unwrap();
    let cmp = compare_upf(&r1, &r2, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!cmp.equivalent);
    assert!(cmp
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::LevelShifterStrategyMismatch));

    let upf10 = r#"
        upf_version 1.0
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_power_domain PD_TOP -include_scope
        create_power_domain PD_SW  -elements {u_sw}
        set_domain_supply_net PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        set_domain_supply_net PD_SW  -primary_power_net VDD_SW  -primary_ground_net VSS
        add_port_state VDD_AON -state {ON 0.9}
        add_port_state VDD_SW  -state {ON 0.9} -state {OFF off}
        add_port_state VSS     -state {GND 0.0}
        create_pst my_pst -supplies {VDD_AON VDD_SW VSS}
        add_pst_state S_ON  -pst my_pst -state {ON ON  GND}
        add_pst_state S_OFF -pst my_pst -state {ON OFF GND}
        set_isolation iso1 -domain PD_SW -applies_to outputs -sink PD_TOP.primary -clamp_value 0 -isolation_power_net VDD_AON -isolation_ground_net VSS
    "#;
    let upf21 = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_SW  -elements {u_sw} -supply {primary SS_SW}
        add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_SW  -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_SW  -state {OFF -supply_expr {power == OFF           && ground == {FULL_ON 0.0}} -simstate CORRUPT}
        set_isolation iso1 -domain PD_SW -applies_to outputs -sink SS_AON -clamp_value 0 -isolation_supply_set SS_AON
    "#;
    let rg = parse_upf_and_resolve(upf10, None, None, Arc::new(NullObjectFinder)).unwrap();
    let rr = parse_upf_and_resolve(upf21, None, None, Arc::new(NullObjectFinder)).unwrap();
    let cmp2 = compare_upf(&rg, &rr, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(cmp2.equivalent, "unexpected diffs: {:?}", cmp2.diffs);

    let mk_elem = |elem: &str| {
        format!(
            r#"
            upf_version 2.1
            create_supply_net VDD
            create_supply_net VSS
            create_supply_set SS -function {{power VDD}} -function {{ground VSS}}
            create_power_domain PD -elements {{{elem}}} -supply {{primary SS}}
            add_power_state SS -state {{ON -supply_expr {{power == {{FULL_ON 0.9}}}} -simstate NORMAL}}
        "#
        )
    };
    let ra =
        parse_upf_and_resolve(&mk_elem("u_a"), None, None, Arc::new(NullObjectFinder)).unwrap();
    let rb =
        parse_upf_and_resolve(&mk_elem("u_b"), None, None, Arc::new(NullObjectFinder)).unwrap();
    let live_finder = MockDesignFinder::new(&["u_a", "u_b"], &["u_a/u_r", "u_b/u_r"]);
    let cmp3 = compare_upf(&ra, &rb, &live_finder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!cmp3.equivalent);
    assert!(cmp3
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::DomainMembershipMismatch));
}

#[test]
fn regression_r9_r10_compare_ret_deep_sleep_equiv_sink_and_simstate() {
    let r9_5a_g = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_SW  -elements {u_sw} -supply {primary SS_SW}
        add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9}} -logic_expr {1'b1} -simstate NORMAL}
        add_power_state SS_SW  -state {ON  -supply_expr {power == {FULL_ON 0.9}} -logic_expr {ret_en && sw_en} -simstate NORMAL}
        add_power_state SS_SW  -state {OFF -supply_expr {power == OFF}           -logic_expr {!(ret_en && sw_en)} -simstate CORRUPT}
        set_retention ret1 -domain PD_SW -retention_supply_set SS_AON -save_signal {save high} -restore_signal {restore high}
    "#;
    let r9_5a_r = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_SW  -elements {u_sw} -supply {primary SS_SW}
        add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9}} -logic_expr {ret_en} -simstate NORMAL}
        add_power_state SS_AON -state {OFF -supply_expr {power == OFF}           -logic_expr {!ret_en} -simstate CORRUPT}
        add_power_state SS_SW  -state {ON  -supply_expr {power == {FULL_ON 0.9}} -logic_expr {ret_en && sw_en} -simstate NORMAL}
        add_power_state SS_SW  -state {OFF -supply_expr {power == OFF}           -logic_expr {!(ret_en && sw_en)} -simstate CORRUPT}
        set_retention ret1 -domain PD_SW -retention_supply_set SS_AON -save_signal {save high} -restore_signal {restore high}
    "#;
    let rg_r9_5a = parse_upf_and_resolve(r9_5a_g, None, None, Arc::new(NullObjectFinder)).unwrap();
    let rr_r9_5a = parse_upf_and_resolve(r9_5a_r, None, None, Arc::new(NullObjectFinder)).unwrap();
    let cmp_r9_5a = compare_upf(
        &rg_r9_5a,
        &rr_r9_5a,
        &NullObjectFinder,
        DEFAULT_VOLTAGE_TOL_UV,
    );
    assert!(!cmp_r9_5a.equivalent);
    assert!(cmp_r9_5a
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::RetentionStrategyMismatch));

    let mk_equiv_sink = |sink: &str| {
        format!(
            r#"
            upf_version 2.1
            create_supply_net VDD_1
            create_supply_net VDD_2
            create_supply_net VDD_SW
            create_supply_net VSS
            create_supply_set SS_AON_1 -function {{power VDD_1}}  -function {{ground VSS}}
            create_supply_set SS_AON_2 -function {{power VDD_2}}  -function {{ground VSS}}
            create_supply_set SS_SW    -function {{power VDD_SW}} -function {{ground VSS}}
            set_equivalent -sets {{SS_AON_1 SS_AON_2}}
            create_power_domain PD_TOP -include_scope -supply {{primary SS_AON_1}}
            create_power_domain PD_SW  -elements {{u_sw}} -supply {{primary SS_SW}}
            add_power_state SS_AON_1 -state {{ON  -supply_expr {{power == {{FULL_ON 0.9}}}} -simstate NORMAL}}
            add_power_state SS_AON_2 -state {{ON  -supply_expr {{power == {{FULL_ON 0.9}}}} -simstate NORMAL}}
            add_power_state SS_SW    -state {{ON  -supply_expr {{power == {{FULL_ON 0.9}}}} -simstate NORMAL}}
            add_power_state SS_SW    -state {{OFF -supply_expr {{power == OFF}} -simstate CORRUPT}}
            set_isolation iso1 -domain PD_SW -applies_to outputs -sink {sink} -clamp_value 0 -isolation_supply_set SS_AON_1
        "#
        )
    };
    let rg_r9_5b = parse_upf_and_resolve(
        &mk_equiv_sink("SS_AON_1"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let rr_r9_5b = parse_upf_and_resolve(
        &mk_equiv_sink("SS_AON_2"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    assert!(
        compare_upf(
            &rg_r9_5b,
            &rr_r9_5b,
            &NullObjectFinder,
            DEFAULT_VOLTAGE_TOL_UV
        )
        .equivalent
    );

    // R10-2: CORRUPT_STATE_ON_ACTIVITY & CORRUPT_STATE_ON_CHANGE in ret_strategy_always_on:
    let mk_ret_upf = |aon_sleep_state: Option<&str>, sw_sleep_sim: &str| {
        let aon_states = match aon_sleep_state {
            None => r#"add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9}} -logic_expr {1'b1} -simstate NORMAL}"#.to_string(),
            Some(sim) => format!(
                "add_power_state SS_AON -state {{ON -supply_expr {{power == {{FULL_ON 0.9}}}} -logic_expr {{ret_en}} -simstate NORMAL}}\n\
                 add_power_state SS_AON -state {{SLEEP -supply_expr {{power == {{FULL_ON 0.6}}}} -logic_expr {{!ret_en}} -simstate {sim}}}"
            ),
        };
        format!(
            r#"
            upf_version 2.1
            create_supply_net VDD_AON
            create_supply_net VDD_SW
            create_supply_net VSS
            create_supply_set SS_AON -function {{power VDD_AON}} -function {{ground VSS}}
            create_supply_set SS_SW  -function {{power VDD_SW}}  -function {{ground VSS}}
            create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
            create_power_domain PD_SW  -elements {{u_sw}} -supply {{primary SS_SW}}
            {aon_states}
            add_power_state SS_SW  -state {{ON  -supply_expr {{power == {{FULL_ON 0.9}}}} -logic_expr {{ret_en && sw_en}} -simstate NORMAL}}
            add_power_state SS_SW  -state {{OFF -supply_expr {{power == OFF}} -logic_expr {{!(ret_en && sw_en)}} -simstate {sw_sleep_sim}}}
            set_retention ret1 -domain PD_SW -retention_supply_set SS_AON -save_signal {{save high}} -restore_signal {{restore high}}
        "#
        )
    };
    let g_ret = parse_upf_and_resolve(
        &mk_ret_upf(None, "CORRUPT"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    for sim in ["CORRUPT_STATE_ON_ACTIVITY", "CORRUPT_STATE_ON_CHANGE"] {
        let r_ret = parse_upf_and_resolve(
            &mk_ret_upf(Some(sim), "CORRUPT"),
            None,
            None,
            Arc::new(NullObjectFinder),
        )
        .unwrap();
        let cmp_f = compare_upf(&g_ret, &r_ret, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
        let cmp_b = compare_upf(&r_ret, &g_ret, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
        assert!(!cmp_f.equivalent && !cmp_b.equivalent);
        assert!(cmp_f
            .diffs
            .iter()
            .any(|d| d.kind == UpfDiffKind::RetentionStrategyMismatch));
        assert!(cmp_b
            .diffs
            .iter()
            .any(|d| d.kind == UpfDiffKind::RetentionStrategyMismatch));
    }
    let g_csoa_pd = parse_upf_and_resolve(
        &mk_ret_upf(None, "CORRUPT_STATE_ON_ACTIVITY"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let r_csoa_both = parse_upf_and_resolve(
        &mk_ret_upf(
            Some("CORRUPT_STATE_ON_ACTIVITY"),
            "CORRUPT_STATE_ON_ACTIVITY",
        ),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let cmp_csoa = compare_upf(
        &g_csoa_pd,
        &r_csoa_both,
        &NullObjectFinder,
        DEFAULT_VOLTAGE_TOL_UV,
    );
    assert!(!cmp_csoa.equivalent);
    assert!(cmp_csoa
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::RetentionStrategyMismatch));
    assert!(
        compare_upf(
            &r_csoa_both,
            &r_csoa_both,
            &NullObjectFinder,
            DEFAULT_VOLTAGE_TOL_UV
        )
        .equivalent
    );
}

#[test]
fn regression_r17_compare_tol_uv_boundaries_and_distinct_voltage_aon_retention() {
    let v_upf = |vb: &str| {
        format!(
            r#"
            upf_version 2.1
            create_supply_net VDD_A
            create_supply_net VDD_B
            create_supply_net VSS
            create_supply_set SS_A -function {{power VDD_A}} -function {{ground VSS}}
            create_supply_set SS_B -function {{power VDD_B}} -function {{ground VSS}}
            create_power_domain PD_A -elements {{u_a}} -supply {{primary SS_A}}
            create_power_domain PD_B -elements {{u_b}} -supply {{primary SS_B}}
            add_power_state SS_A -state {{ON -supply_expr {{power == {{FULL_ON 0.900}} && ground == {{FULL_ON 0.0}}}} -simstate NORMAL}}
            add_power_state SS_B -state {{ON -supply_expr {{power == {{FULL_ON {vb}}} && ground == {{FULL_ON 0.0}}}} -simstate NORMAL}}
        "#
        )
    };
    let g_900 =
        parse_upf_and_resolve(&v_upf("0.900"), None, None, Arc::new(NullObjectFinder)).unwrap();
    let r_901 =
        parse_upf_and_resolve(&v_upf("0.901"), None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(!compare_upf(&g_900, &r_901, &NullObjectFinder, 999).equivalent);
    assert!(compare_upf(&g_900, &r_901, &NullObjectFinder, 1000).equivalent);
    assert!(compare_upf(&g_900, &r_901, &NullObjectFinder, 1001).equivalent);

    let aon_ret_upf = |ret_v: &str| {
        format!(
            r#"
            upf_version 2.1
            create_supply_net VDD_SW
            create_supply_net VDD_AON
            create_supply_net VSS
            create_supply_set SS_SW  -function {{power VDD_SW}}  -function {{ground VSS}}
            create_supply_set SS_AON -function {{power VDD_AON}} -function {{ground VSS}}
            create_power_domain PD -include_scope -supply {{primary SS_SW}}
            add_power_state SS_SW  -state {{ON  -supply_expr {{power == {{FULL_ON 0.9}} && ground == {{FULL_ON 0.0}}}} -logic_expr {{en}}  -simstate NORMAL}}
            add_power_state SS_SW  -state {{OFF -supply_expr {{power == OFF && ground == {{FULL_ON 0.0}}}} -logic_expr {{!en}} -simstate CORRUPT}}
            add_power_state SS_AON -state {{ON  -supply_expr {{power == {{FULL_ON {ret_v}}} && ground == {{FULL_ON 0.0}}}} -simstate NORMAL}}
            set_retention ret_aon -domain PD -retention_supply_set SS_AON -save_signal {{save high}} -restore_signal {{rest low}}
        "#
        )
    };
    let g_aon =
        parse_upf_and_resolve(&aon_ret_upf("0.9"), None, None, Arc::new(NullObjectFinder)).unwrap();
    let r_aon =
        parse_upf_and_resolve(&aon_ret_upf("1.2"), None, None, Arc::new(NullObjectFinder)).unwrap();
    let cmp_aon = compare_upf(&g_aon, &r_aon, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        cmp_aon.equivalent,
        "distinct-voltage AON retention supplies must remain equivalent: {:?}",
        cmp_aon.diffs
    );
}
