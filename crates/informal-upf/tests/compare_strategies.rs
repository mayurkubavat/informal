mod common;

use std::sync::Arc;

use common::MockDesignFinder;
use informal_upf::{
    compare_upf, parse_upf_and_resolve, NullObjectFinder, ObjectFinder, UpfDiffKind,
    DEFAULT_VOLTAGE_TOL_UV,
};

#[test]
fn regression_compare_upf_catches_extra_strategies_filters_fields_and_iso_off_supply() {
    let base = r#"
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
        add_power_state SS_CORE -state {OFF -supply_expr {power == OFF}           -simstate CORRUPT}
        set_isolation iso1 -domain PD_CORE -clamp_value 0 -isolation_supply_set SS_AON -diff_supply_only -source SS_CORE -sink SS_AON
        set_level_shifter ls1 -domain PD_CORE -rule low_to_high -threshold 0.2 -source SS_CORE -sink SS_AON -input_supply_set SS_CORE -output_supply_set SS_AON
        set_retention ret1 -domain PD_CORE -retention_supply_set SS_AON -save_signal {s1 high} -save_condition {pwr_down == 1} -restore_condition {pwr_up == 1} -retention_condition {ret_en == 1}
    "#;
    let rg = parse_upf_and_resolve(base, None, None, Arc::new(NullObjectFinder)).unwrap();

    let two_ret = format!(
        "{base}\nset_retention ret2 -domain PD_CORE -retention_supply_set SS_AON -save_signal {{s2 high}}\n"
    );
    let r_two_ret =
        parse_upf_and_resolve(&two_ret, None, None, Arc::new(NullObjectFinder)).unwrap();
    let cmp_ret_len = compare_upf(&r_two_ret, &rg, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!cmp_ret_len.equivalent);
    assert!(cmp_ret_len
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::RetentionStrategyMismatch));

    let two_ls = format!(
        "{base}\nset_level_shifter ls2 -domain PD_CORE -rule high_to_low -source SS_CORE -sink SS_AON\n"
    );
    let r_two_ls = parse_upf_and_resolve(&two_ls, None, None, Arc::new(NullObjectFinder)).unwrap();
    let cmp_ls_len = compare_upf(&r_two_ls, &rg, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!cmp_ls_len.equivalent);
    assert!(cmp_ls_len
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::LevelShifterStrategyMismatch));

    for (pat, rep) in [
        ("pwr_down == 1", "pwr_down == 0"),
        ("pwr_up == 1", "pwr_up == 0"),
        ("ret_en == 1", "ret_en == 0"),
        (
            "-save_signal {s1 high}",
            "-save_signal {s1 high} -use_retention_as_primary",
        ),
    ] {
        let rev = base.replace(pat, rep);
        let rr = parse_upf_and_resolve(&rev, None, None, Arc::new(NullObjectFinder)).unwrap();
        assert!(
            !compare_upf(&rg, &rr, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV).equivalent,
            "expected diff for `{pat}` -> `{rep}`"
        );
    }

    for (pat, rep) in [
        ("-threshold 0.2", "-threshold 0.5"),
        ("-threshold 0.2", "-threshold 0.2 -force_shift"),
        ("-input_supply_set SS_CORE", "-input_supply_set SS_AON"),
        ("-output_supply_set SS_AON", "-output_supply_set SS_CORE"),
        (
            "-source SS_CORE -sink SS_AON -input_supply_set",
            "-source SS_AON -sink SS_AON -input_supply_set",
        ),
    ] {
        let rev = base.replace(pat, rep);
        assert_ne!(rev, base, "pattern `{pat}` did not match");
        let rr = parse_upf_and_resolve(&rev, None, None, Arc::new(NullObjectFinder)).unwrap();
        assert!(
            !compare_upf(&rg, &rr, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV).equivalent,
            "expected diff for `{pat}` -> `{rep}`"
        );
    }

    for (pat, rep) in [
        (" -diff_supply_only", ""),
        ("-clamp_value 0", "-clamp_value 0 -force_isolation"),
        ("-clamp_value 0", "-clamp_value 0 -applies_to_clamp 1"),
        (
            "-diff_supply_only -source SS_CORE -sink SS_AON",
            "-diff_supply_only -source SS_CORE -sink SS_CORE",
        ),
    ] {
        let rev = base.replace(pat, rep);
        assert_ne!(rev, base, "pattern `{pat}` did not match");
        let rr = parse_upf_and_resolve(&rev, None, None, Arc::new(NullObjectFinder)).unwrap();
        assert!(
            !compare_upf(&rg, &rr, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV).equivalent,
            "expected diff for `{pat}` -> `{rep}`"
        );
    }

    let rev_bad_iso_sup = base.replace(
        "-isolation_supply_set SS_AON",
        "-isolation_supply_set SS_CORE",
    );
    let r_bis =
        parse_upf_and_resolve(&rev_bad_iso_sup, None, None, Arc::new(NullObjectFinder)).unwrap();
    let cmp_bis = compare_upf(&rg, &r_bis, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!cmp_bis.equivalent);
    assert!(cmp_bis
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::IsolationStrategyMismatch));
}

#[test]
fn regression_r2_compare_upf_order_independent_buckets_and_internal_supply_set() {
    let header = r#"
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

    let iso_a = "set_isolation iso_a -domain PD_SW -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso0 -isolation_sense high";
    let iso_b = "set_isolation iso_b -domain PD_SW -clamp_value 1 -isolation_supply_set SS_AON -isolation_signal iso1 -isolation_sense low";
    let ls_a = "set_level_shifter ls_a -domain PD_SW -rule low_to_high -threshold 0.1 -input_supply_set SS_SW -output_supply_set SS_AON -internal_supply_set SS_AON";
    let ls_b = "set_level_shifter ls_b -domain PD_SW -rule high_to_low -threshold 0.2 -input_supply_set SS_AON -output_supply_set SS_SW -internal_supply_set SS_SW";
    let ret_a = "set_retention ret_a -domain PD_SW -retention_supply_set SS_AON -save_signal {s0 high} -restore_signal {r0 high}";
    let ret_b = "set_retention ret_b -domain PD_SW -retention_supply_set SS_AON -save_signal {s1 low}  -restore_signal {r1 low}";

    let golden = format!("{header}\n{iso_a}\n{iso_b}\n{ls_a}\n{ls_b}\n{ret_a}\n{ret_b}\n");
    let revised = format!("{header}\n{iso_b}\n{iso_a}\n{ls_b}\n{ls_a}\n{ret_b}\n{ret_a}\n");

    let rg = parse_upf_and_resolve(&golden, None, None, Arc::new(NullObjectFinder)).unwrap();
    let rr = parse_upf_and_resolve(&revised, None, None, Arc::new(NullObjectFinder)).unwrap();
    let cmp = compare_upf(&rg, &rr, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        cmp.equivalent,
        "reordered strategies in same bucket must compare equivalent, got diffs: {:?}",
        cmp.diffs
    );

    let revised_internal = format!(
        "{header}\n{iso_a}\n{iso_b}\n{}\n{ls_b}\n{ret_a}\n{ret_b}\n",
        ls_a.replace("-internal_supply_set SS_AON", "-internal_supply_set SS_SW")
    );
    let r_int =
        parse_upf_and_resolve(&revised_internal, None, None, Arc::new(NullObjectFinder)).unwrap();
    let cmp_int = compare_upf(&rg, &r_int, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!cmp_int.equivalent);
    assert!(cmp_int.diffs.iter().any(|d| {
        d.kind == UpfDiffKind::LevelShifterStrategyMismatch
            && d.golden_detail.contains("internal_supply_set")
    }));
}

#[test]
fn compare_upf_wildcard_bipartite_20_strategies_and_rem_pairs_blame() {
    let header = r#"
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
    let g_upf = format!(
        "{header}\n\
         set_isolation iso_1 -domain PD_SW -clamp_value 0 -location automatic -isolation_supply_set SS_AON\n\
         set_isolation iso_2 -domain PD_SW -clamp_value 0 -location parent    -isolation_supply_set SS_AON\n"
    );
    let r_upf = format!(
        "{header}\n\
         set_isolation iso_1 -domain PD_SW -clamp_value 0 -location parent -isolation_supply_set SS_AON\n\
         set_isolation iso_2 -domain PD_SW -clamp_value 0 -location self   -isolation_supply_set SS_AON\n"
    );
    let rg = parse_upf_and_resolve(&g_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let rr = parse_upf_and_resolve(&r_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(compare_upf(&rg, &rr, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV).equivalent);

    let g_ren = format!(
        "{header}\n\
         set_isolation iso_1 -domain PD_SW -clamp_value 0 -isolation_supply_set SS_AON\n\
         set_isolation iso_2 -domain PD_SW -clamp_value 0 -location self -isolation_supply_set SS_AON\n"
    );
    let r_ren = format!(
        "{header}\n\
         set_isolation iso_a -domain PD_SW -clamp_value 0 -location parent -isolation_supply_set SS_AON\n\
         set_isolation iso_b -domain PD_SW -clamp_value 0 -location self   -isolation_supply_set SS_AON\n"
    );
    let rg2 = parse_upf_and_resolve(&g_ren, None, None, Arc::new(NullObjectFinder)).unwrap();
    let rr2 = parse_upf_and_resolve(&r_ren, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(compare_upf(&rg2, &rr2, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV).equivalent);

    // 20-strategy wildcard bucket + asymmetric count:
    let mut g_strats = String::new();
    let mut r_strats = String::new();
    for i in 0..10 {
        g_strats.push_str(&format!(
            "set_isolation iso_{} -domain PD_SW -clamp_value 0 -location automatic -isolation_supply_set SS_AON\n",
            i + 10
        ));
        r_strats.push_str(&format!(
            "set_isolation iso_{} -domain PD_SW -clamp_value 0 -location self -isolation_supply_set SS_AON\n",
            i
        ));
    }
    for i in 0..10 {
        g_strats.push_str(&format!(
            "set_isolation iso_{} -domain PD_SW -clamp_value 0 -location parent -isolation_supply_set SS_AON\n",
            i
        ));
        r_strats.push_str(&format!(
            "set_isolation iso_{} -domain PD_SW -clamp_value 0 -location parent -isolation_supply_set SS_AON\n",
            i + 10
        ));
    }
    let rg20 = parse_upf_and_resolve(
        &format!("{header}\n{g_strats}"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let rr20 = parse_upf_and_resolve(
        &format!("{header}\n{r_strats}"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    assert!(compare_upf(&rg20, &rr20, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV).equivalent);
    assert!(compare_upf(&rr20, &rg20, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV).equivalent);

    let r_21_strats = format!(
        "{r_strats}\nset_isolation iso_extra -domain PD_SW -clamp_value 0 -location parent -isolation_supply_set SS_AON\n"
    );
    let rr21 = parse_upf_and_resolve(
        &format!("{header}\n{r_21_strats}"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let cmp_asym = compare_upf(&rg20, &rr21, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!cmp_asym.equivalent);
    assert!(cmp_asym
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::IsolationStrategyMismatch));

    // Dedup elements and min-cost pairing blame:
    let dup_g = parse_upf_and_resolve(
        &format!(
            "{header}\nset_isolation iso1 -domain PD_SW -elements {{u_sw/a u_sw/a}} -clamp_value 0 -isolation_supply_set SS_AON\n"
        ),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let dup_r = parse_upf_and_resolve(
        &format!(
            "{header}\nset_isolation iso1 -domain PD_SW -elements {{u_sw/a}} -clamp_value 0 -isolation_supply_set SS_AON\n"
        ),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    assert!(compare_upf(&dup_g, &dup_r, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV).equivalent);

    let g_pair = parse_upf_and_resolve(
        &format!(
            "{header}\n\
             set_isolation iso_a -domain PD_SW -clamp_value 0 -isolation_signal en_a -isolation_sense high -isolation_supply_set SS_AON\n\
             set_isolation iso_b -domain PD_SW -clamp_value 1 -isolation_signal en_b -isolation_sense high -isolation_supply_set SS_AON\n"
        ),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let r_pair = parse_upf_and_resolve(
        &format!(
            "{header}\n\
             set_isolation iso_a -domain PD_SW -clamp_value 1 -isolation_signal en_a -isolation_sense high -isolation_supply_set SS_AON\n\
             set_isolation iso_b -domain PD_SW -clamp_value 0 -isolation_signal en_b -isolation_sense high -isolation_supply_set SS_AON\n"
        ),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let cmp_pair = compare_upf(&g_pair, &r_pair, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!cmp_pair.equivalent);
    assert_eq!(cmp_pair.diffs.len(), 2, "diffs: {:?}", cmp_pair.diffs);
    for d in &cmp_pair.diffs {
        assert!(d.golden_detail.contains("clamp_value"));
        assert!(!d.golden_detail.contains("isolation_signal"));
        assert_eq!(
            d.golden_detail
                .split("iso `")
                .nth(1)
                .and_then(|s| s.split('`').next()),
            d.revised_detail
                .split("iso `")
                .nth(1)
                .and_then(|s| s.split('`').next()),
        );
    }
}

#[test]
fn regression_r9_r14_r16_compare_retention_conditions_and_rtl_brackets() {
    // R9-4: normalized `-save_condition` / `-restore_condition` / `-retention_condition`.
    let r9_4_golden = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_SW  -elements {u_sub} -supply {primary SS_SW}
        add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9}} -simstate NORMAL}
        add_power_state SS_SW  -state {ON  -supply_expr {power == {FULL_ON 0.9}} -simstate NORMAL}
        add_power_state SS_SW  -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
        set_retention r1 -domain PD_SW -retention_supply_set SS_AON -save_condition {u_sub/save_c} -restore_condition {u_sub/rest_c} -retention_condition {u_sub/ret_cond}
    "#;
    let r9_4_revised = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_SW  -elements {u_sub} -supply {primary SS_SW}
        add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9}} -simstate NORMAL}
        add_power_state SS_SW  -state {ON  -supply_expr {power == {FULL_ON 0.9}} -simstate NORMAL}
        add_power_state SS_SW  -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
        set_scope u_sub
        set_retention r1 -domain /PD_SW -retention_supply_set /SS_AON -save_condition {save_c} -restore_condition {rest_c} -retention_condition {ret_cond}
        set_scope /
    "#;
    let rg_r9_4 =
        parse_upf_and_resolve(r9_4_golden, None, None, Arc::new(NullObjectFinder)).unwrap();
    let rr_r9_4 =
        parse_upf_and_resolve(r9_4_revised, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(
        compare_upf(
            &rg_r9_4,
            &rr_r9_4,
            &NullObjectFinder,
            DEFAULT_VOLTAGE_TOL_UV
        )
        .equivalent
    );

    // probe_3c: RTL-bracketed vs pre-escaped elements:
    let golden_upf = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD -elements {u_arr[0]/sig u_arr[0]/*} -supply {primary SS}
        set_isolation iso -domain PD -isolation_supply_set SS -clamp_value 0 -elements {u_arr[0]/sig}
        set_level_shifter ls -domain PD -elements {u_arr[0]/*}
        set_retention ret -domain PD -retention_supply_set SS -elements {u_arr[0]/sig}
    "#;
    let revised_upf = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD -elements [list {u_arr\[0\]/sig} {u_arr\[0\]/*}] -supply {primary SS}
        set_isolation iso -domain PD -isolation_supply_set SS -clamp_value 0 -elements [list {u_arr\[0\]/sig}]
        set_level_shifter ls -domain PD -elements [list {u_arr\[0\]/*}]
        set_retention ret -domain PD -retention_supply_set SS -elements [list {u_arr\[0\]/sig}]
    "#;
    for finder in [
        Arc::new(NullObjectFinder) as Arc<dyn ObjectFinder>,
        Arc::new(MockDesignFinder::new(
            &["u_arr[0]"],
            &["u_arr[0]/sig", "u_arr[0]/other"],
        )) as Arc<dyn ObjectFinder>,
    ] {
        let g = parse_upf_and_resolve(golden_upf, None, None, Arc::clone(&finder)).unwrap();
        let r = parse_upf_and_resolve(revised_upf, None, None, Arc::clone(&finder)).unwrap();
        assert!(compare_upf(&g, &r, finder.as_ref(), DEFAULT_VOLTAGE_TOL_UV).equivalent);
    }

    let cc_upf = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD -elements {u_arr[0-1]/sig} -supply {primary SS}
    "#;
    let lit_upf = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD -elements [list {u_arr\[0-1\]/sig}] -supply {primary SS}
    "#;
    let g_cc = parse_upf_and_resolve(cc_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let r_lit = parse_upf_and_resolve(lit_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(!compare_upf(&g_cc, &r_lit, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV).equivalent);

    // Bracketed control signals (`isolation_signal`, `save_signal`, `restore_signal`):
    let ctrl_golden = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD -elements {u_arr[0]/sig} -supply {primary SS}
        set_isolation iso -domain PD -isolation_supply_set SS -clamp_value 0 -isolation_signal {u_arr[0]/iso_en} -isolation_sense high -elements {u_arr[0]/sig}
        set_retention ret -domain PD -retention_supply_set SS -save_signal {{u_arr[0]/save} high} -restore_signal {{u_arr[0]/rest} low} -elements {u_arr[0]/sig}
    "#;
    let ctrl_revised = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD -elements [list {u_arr\[0\]/sig}] -supply {primary SS}
        set_isolation iso -domain PD -isolation_supply_set SS -clamp_value 0 -isolation_signal {u_arr\[0\]/iso_en} -isolation_sense high -elements [list {u_arr\[0\]/sig}]
        set_retention ret -domain PD -retention_supply_set SS -save_signal [list {u_arr\[0\]/save} high] -restore_signal [list {u_arr\[0\]/rest} low] -elements [list {u_arr\[0\]/sig}]
    "#;
    let g_ctrl =
        parse_upf_and_resolve(ctrl_golden, None, None, Arc::new(NullObjectFinder)).unwrap();
    let r_ctrl =
        parse_upf_and_resolve(ctrl_revised, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(compare_upf(&g_ctrl, &r_ctrl, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV).equivalent);

    let ctrl_cc = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD -elements {u_arr[0]/sig} -supply {primary SS}
        set_isolation iso -domain PD -isolation_supply_set SS -clamp_value 0 -isolation_signal {u_arr[0-1]/iso_en} -isolation_sense high -elements {u_arr[0]/sig}
    "#;
    let ctrl_lit = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD -elements {u_arr[0]/sig} -supply {primary SS}
        set_isolation iso -domain PD -isolation_supply_set SS -clamp_value 0 -isolation_signal {u_arr\[0-1\]/iso_en} -isolation_sense high -elements {u_arr[0]/sig}
    "#;
    let g_cc_ctrl = parse_upf_and_resolve(ctrl_cc, None, None, Arc::new(NullObjectFinder)).unwrap();
    let r_lit_ctrl =
        parse_upf_and_resolve(ctrl_lit, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(
        !compare_upf(
            &g_cc_ctrl,
            &r_lit_ctrl,
            &NullObjectFinder,
            DEFAULT_VOLTAGE_TOL_UV
        )
        .equivalent
    );

    // R16 RetSortKey retention condition whitespace normalization:
    let ret_golden = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD -elements {u_sub/reg1 u_sub/reg2} -supply {primary SS}
        set_retention r1 -domain PD -retention_supply_set SS -save_signal {save high} -save_condition {en && !rst} -restore_signal {rest low} -restore_condition {rst_n || wake} -elements {u_sub/reg1}
        set_retention r2 -domain PD -retention_supply_set SS -save_signal {save high} -save_condition {!en && rst} -restore_signal {rest low} -restore_condition {!rst_n && !wake} -elements {u_sub/reg2}
    "#;
    let ret_revised = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD -elements {u_sub/reg1 u_sub/reg2} -supply {primary SS}
        set_retention r2_rev -domain PD -retention_supply_set SS -save_signal {save high} -save_condition {!en&&rst} -restore_signal {rest low} -restore_condition {!rst_n&&!wake} -elements {u_sub/reg2}
        set_retention r1_rev -domain PD -retention_supply_set SS -save_signal {save high} -save_condition {en&&!rst} -restore_signal {rest low} -restore_condition {rst_n||wake} -elements {u_sub/reg1}
    "#;
    let g_ret = parse_upf_and_resolve(ret_golden, None, None, Arc::new(NullObjectFinder)).unwrap();
    let r_ret = parse_upf_and_resolve(ret_revised, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(compare_upf(&g_ret, &r_ret, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV).equivalent);
}

#[test]
fn regression_r23_scoped_sv_escaped_index_and_single_domain_iso_supply() {
    // 1. `compare_upf` with `set_scope {u_top/\bus [0]}` & `{u_top/\gen [3:0]}` vs root `-save_condition`.
    let mk_ret_scope = |scope_stmt: &str, cond: &str, dom_ref: &str, ss_ref: &str| {
        format!(
            r#"
            upf_version 2.1
            create_supply_net VDD_AON
            create_supply_net VDD_SW
            create_supply_net VSS
            create_supply_set SS_AON -function {{power VDD_AON}} -function {{ground VSS}}
            create_supply_set SS_SW  -function {{power VDD_SW}}  -function {{ground VSS}}
            create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
            create_power_domain PD_SW  -elements {{u_top/\bus [0] u_top/\gen [3:0]}} -supply {{primary SS_SW}}
            add_power_state SS_AON -state {{ON  -supply_expr {{power == {{FULL_ON 0.9}}}} -simstate NORMAL}}
            add_power_state SS_SW  -state {{ON  -supply_expr {{power == {{FULL_ON 0.9}}}} -simstate NORMAL}}
            add_power_state SS_SW  -state {{OFF -supply_expr {{power == OFF}} -simstate CORRUPT}}
            {scope_stmt}
            set_retention ret1 -domain {dom_ref} -retention_supply_set {ss_ref} -save_condition {{{cond}}}
            set_scope /
        "#
        )
    };
    let g_bus0 = parse_upf_and_resolve(
        &mk_ret_scope(
            "set_scope {u_top/\\bus [0]}",
            "save_en",
            "/PD_SW",
            "/SS_AON",
        ),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let r_bus0 = parse_upf_and_resolve(
        &mk_ret_scope("", "u_top/\\bus [0]/save_en", "PD_SW", "SS_AON"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let cmp_bus0 = compare_upf(&g_bus0, &r_bus0, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        cmp_bus0.equivalent,
        "unexpected diffs: {:?}",
        cmp_bus0.diffs
    );

    let g_gen30 = parse_upf_and_resolve(
        &mk_ret_scope(
            "set_scope {u_top/\\gen [3:0]}",
            "save_en",
            "/PD_SW",
            "/SS_AON",
        ),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let r_gen30 = parse_upf_and_resolve(
        &mk_ret_scope("", "u_top/\\gen [3:0]/save_en", "PD_SW", "SS_AON"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let cmp_gen30 = compare_upf(
        &g_gen30,
        &r_gen30,
        &NullObjectFinder,
        DEFAULT_VOLTAGE_TOL_UV,
    );
    assert!(
        cmp_gen30.equivalent,
        "unexpected diffs: {:?}",
        cmp_gen30.diffs
    );

    // 2. Single-domain switchable UPF: `-isolation_supply_set SS_AON` vs `SS_SW` must fail.
    let mk_single_domain = |iso_ss: &str, applies: &str| {
        format!(
            r#"
            upf_version 2.1
            create_supply_net VDD_AON
            create_supply_net VDD_AON2
            create_supply_net VDD_SW
            create_supply_net VSS
            create_supply_set SS_AON  -function {{power VDD_AON}}  -function {{ground VSS}}
            create_supply_set SS_AON2 -function {{power VDD_AON2}} -function {{ground VSS}}
            create_supply_set SS_SW   -function {{power VDD_SW}}   -function {{ground VSS}}
            create_power_domain PD -include_scope -supply {{primary SS_SW}}
            add_power_state SS_AON  -state {{ON  -supply_expr {{power == {{FULL_ON 0.9}}}} -simstate NORMAL}}
            add_power_state SS_AON2 -state {{ON  -supply_expr {{power == {{FULL_ON 0.9}}}} -simstate NORMAL}}
            add_power_state SS_SW   -state {{ON  -supply_expr {{power == {{FULL_ON 0.9}}}} -simstate NORMAL}}
            add_power_state SS_SW   -state {{OFF -supply_expr {{power == OFF}} -simstate CORRUPT}}
            set_isolation iso1 -domain PD -isolation_supply_set {iso_ss} -clamp_value 0 {applies}
        "#
        )
    };
    let s_aon = parse_upf_and_resolve(
        &mk_single_domain("SS_AON", ""),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let s_aon2 = parse_upf_and_resolve(
        &mk_single_domain("SS_AON2", ""),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let s_sw = parse_upf_and_resolve(
        &mk_single_domain("SS_SW", ""),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();

    assert!(compare_upf(&s_aon, &s_aon2, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV).equivalent);

    let cmp_bad_fwd = compare_upf(&s_aon, &s_sw, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!cmp_bad_fwd.equivalent);
    assert!(cmp_bad_fwd
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::IsolationStrategyMismatch));

    let cmp_bad_rev = compare_upf(&s_sw, &s_aon, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!cmp_bad_rev.equivalent);
    assert!(cmp_bad_rev
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::IsolationStrategyMismatch));

    let s_both_aon = parse_upf_and_resolve(
        &mk_single_domain("SS_AON", "-applies_to both"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let s_both_sw = parse_upf_and_resolve(
        &mk_single_domain("SS_SW", "-applies_to both"),
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    let cmp_both = compare_upf(
        &s_both_aon,
        &s_both_sw,
        &NullObjectFinder,
        DEFAULT_VOLTAGE_TOL_UV,
    );
    assert!(!cmp_both.equivalent);
    assert!(cmp_both
        .diffs
        .iter()
        .any(|d| d.kind == UpfDiffKind::IsolationStrategyMismatch));

    // Switchable iso supply `SS_SW_ISO` that remains `NORMAL` whenever `PD` is ON or OFF:
    let sw_iso_upf = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW_ISO
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON    -function {power VDD_AON}    -function {ground VSS}
        create_supply_set SS_SW_ISO -function {power VDD_SW_ISO} -function {ground VSS}
        create_supply_set SS_SW     -function {power VDD_SW}     -function {ground VSS}
        create_power_domain PD -include_scope -supply {primary SS_SW}
        add_power_state SS_AON    -state {ON  -supply_expr {power == {FULL_ON 0.9}} -simstate NORMAL}
        add_power_state SS_SW_ISO -state {ON  -supply_expr {power == {FULL_ON 0.9}} -logic_expr {pd_en || iso_keep_on} -simstate NORMAL}
        add_power_state SS_SW_ISO -state {OFF -supply_expr {power == OFF} -logic_expr {!(pd_en || iso_keep_on)} -simstate CORRUPT}
        add_power_state SS_SW     -state {ON  -supply_expr {power == {FULL_ON 0.9}} -logic_expr {pd_en && iso_keep_on} -simstate NORMAL}
        add_power_state SS_SW     -state {OFF -supply_expr {power == OFF} -logic_expr {!pd_en && iso_keep_on} -simstate CORRUPT}
        set_isolation iso1 -domain PD -isolation_supply_set SS_SW_ISO -clamp_value 0
    "#;
    let s_sw_iso =
        parse_upf_and_resolve(sw_iso_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let cmp_sw_iso = compare_upf(&s_aon, &s_sw_iso, &NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        cmp_sw_iso.equivalent,
        "switchable iso supply always NORMAL when PD ON/OFF should match AON, diffs: {:?}",
        cmp_sw_iso.diffs
    );
}
