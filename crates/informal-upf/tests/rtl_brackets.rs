mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::{MockDesignFinder, NoUnescapeFinder, RelOnlyFinder};
use informal_upf::ir::{escape_rtl_brackets, is_glob_char_class};
use informal_upf::{
    glob, parse_cpf_and_resolve, parse_upf_and_resolve, qualify_name, split_tcl_list, AppliesTo,
    DiagSeverity, NullObjectFinder, ObjectFinder, StrategyMatch,
};

#[test]
fn regression_r13_rtl_brackets_find_objects_empty_brackets_and_deferred_ops() {
    let scopes = &["u_sub", "u_sub/u_arr[0]", "u_sub/u_arr0"];
    let insts = &[
        "u_sub/u_arr[0]/bus0",
        "u_sub/u_arr[0]/bus1",
        "u_sub/u_arr[0]/sig",
        "u_sub/u_arr[0]/sig2",
        "u_sub/u_arr0/bus0",
        "u_sub/u_arr0/sig",
    ];
    for finder in [
        Arc::new(MockDesignFinder::new(scopes, insts)) as Arc<dyn ObjectFinder>,
        Arc::new(NoUnescapeFinder::new(scopes, insts)) as Arc<dyn ObjectFinder>,
    ] {
        let upf = r#"
            upf_version 2.1
            create_supply_net VDD
            create_supply_net VSS
            create_supply_set SS -function {power VDD} -function {ground VSS}
            set_scope u_sub
            create_supply_net {sn_arr[0]}
            create_supply_net sn_arr0
            create_supply_port {sp_arr[0]} -direction in
            create_supply_port sp_arr0 -direction in
            create_power_domain {PD_ARR[0]} -elements {u_arr[0]/sig} -supply {primary /SS}
            create_power_domain PD_ARR0 -elements {u_arr0/sig} -supply {primary /SS}

            set found_wc [lsort [find_objects . -object_type inst -pattern {u_arr[0]/*}]]
            if {$found_wc ne {{u_arr[0]/bus0} {u_arr[0]/bus1} {u_arr[0]/sig} {u_arr[0]/sig2}}} {
                error [concat {unexpected find_objects -pattern u_arr[0]/*: } $found_wc]
            }
            set found_mix [lsort [find_objects . -object_type inst -pattern {u_arr[0]/bus[0-1]}]]
            if {$found_mix ne {{u_arr[0]/bus0} {u_arr[0]/bus1}}} {
                error [concat {unexpected find_objects -pattern u_arr[0]/bus[0-1]: } $found_mix]
            }
            set found_pre [lsort [find_objects . -object_type inst -pattern [list {u_arr\[0\]/*}]]]
            if {$found_pre ne {{u_arr[0]/bus0} {u_arr[0]/bus1} {u_arr[0]/sig} {u_arr[0]/sig2}}} {
                error [concat {unexpected find_objects -pattern pre-escaped: } $found_pre]
            }
            set found_exact [find_objects . -object_type inst -pattern {u_arr[0]/bus0}]
            if {$found_exact ne {{u_arr[0]/bus0}}} {
                error [concat {unexpected find_objects exact u_arr[0]/bus0: } $found_exact]
            }
            set found_re [lsort [find_objects . -object_type inst -regexp -pattern {^u_arr\[0\]/bus[01]$}]]
            if {$found_re ne {{u_arr[0]/bus0} {u_arr[0]/bus1}}} {
                error [concat {unexpected find_objects -regexp: } $found_re]
            }
            set found_sn [find_objects . -object_type supply_net -pattern {sn_arr[0]}]
            if {$found_sn ne {{sn_arr[0]}}} {
                error [concat {unexpected find_objects supply_net: } $found_sn]
            }
            set found_sn_re [find_objects . -object_type supply_net -regexp -pattern {^sn_arr\[0\]$}]
            if {$found_sn_re ne {{sn_arr[0]}}} {
                error [concat {unexpected find_objects supply_net -regexp: } $found_sn_re]
            }
            set found_sp [find_objects . -object_type supply_port -pattern {sp_arr[0]}]
            if {$found_sp ne {{sp_arr[0]}}} {
                error [concat {unexpected find_objects supply_port: } $found_sp]
            }
            set found_pd [find_objects . -object_type power_domain -pattern {PD_ARR[0]}]
            if {$found_pd ne {{PD_ARR[0]}}} {
                error [concat {unexpected find_objects power_domain: } $found_pd]
            }
            create_power_domain PD_FROM_FIND -elements $found_mix -supply {primary /SS}
            set_scope /
        "#;
        let res = parse_upf_and_resolve(upf, None, None, finder).unwrap();
        assert!(!res
            .intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error));
        let pd = &res.intent.domains[res.domain_by_name["u_sub/PD_FROM_FIND"].0 as usize];
        assert_eq!(
            res.domain_instances[pd.id.0 as usize],
            BTreeSet::from([
                "u_sub/u_arr[0]/bus0".to_string(),
                "u_sub/u_arr[0]/bus1".to_string()
            ])
        );
    }

    assert_eq!(
        escape_rtl_brackets("u_arr[]/bus[0]/*"),
        "u_arr\\[\\]/bus\\[0\\]/*"
    );
    assert!(!is_glob_char_class("u_arr[]/bus[0]/*"));
    assert_eq!(
        escape_rtl_brackets("u_arr[]/bus[0-1]/*"),
        "u_arr\\[\\]/bus[0-1]/*"
    );
    assert!(is_glob_char_class("u_arr[]/bus[0-1]/*"));
    assert_eq!(
        escape_rtl_brackets("u_arr[foo/bus[0]/*"),
        "u_arr\\[foo/bus\\[0\\]/*"
    );
    assert!(!is_glob_char_class("u_arr[foo/bus[0]/*"));
    assert!(!is_glob_char_class("u_arr[foo[0]/*"));
    assert!(!is_glob_char_class("a\\é[0]"));
    assert!(is_glob_char_class("a\\é[0-1]"));
    assert!(!glob(
        &escape_rtl_brackets("u_arr[]/bus[0]/*"),
        "u_arr0/sig"
    ));
    assert!(!glob(
        &escape_rtl_brackets("u_arr[]/bus[0-1]/*"),
        "u_arr0/bus0"
    ));

    let finder_r12_2: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_sub/u_arr0"],
        &["u_sub/u_arr0/sig", "u_sub/u_arr0/bus0"],
    ));
    let upf_r12_2 = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        set_scope u_sub
        create_power_domain PD_EMPTY_2 -elements {u_arr[]/bus[0]/*} -supply {primary /SS}
        create_power_domain PD_EMPTY_CC -elements {u_arr[]/bus[0-1]/*} -supply {primary /SS}
        set_scope /
    "#;
    let res_r12_2 = parse_upf_and_resolve(upf_r12_2, None, None, finder_r12_2).unwrap();
    for pd_name in ["u_sub/PD_EMPTY_2", "u_sub/PD_EMPTY_CC"] {
        let pd = &res_r12_2.intent.domains[res_r12_2.domain_by_name[pd_name].0 as usize];
        assert!(res_r12_2.domain_instances[pd.id.0 as usize].is_empty());
    }
    assert_eq!(
        res_r12_2
            .intent
            .diagnostics
            .iter()
            .filter(|d| d.rule_id == "UPF_UNRESOLVED_TARGET")
            .count(),
        2
    );

    for finder in [
        Arc::new(MockDesignFinder::new(scopes, insts)) as Arc<dyn ObjectFinder>,
        Arc::new(NoUnescapeFinder::new(scopes, insts)) as Arc<dyn ObjectFinder>,
    ] {
        let upf_r12_3 = r#"
            upf_version 2.1
            create_supply_net VDD
            create_supply_net VSS
            create_supply_set SS -function {power VDD} -function {ground VSS}
            set_scope u_sub
            create_power_domain PD_LIST_EX -elements [list {u_arr\[0\]/sig}] -supply {primary /SS}
            create_power_domain PD_DBL_EX -elements {{u_arr\[0\]/sig2}} -supply {primary /SS}
            create_power_domain PD_LIST_WC -elements [list {u_arr\[0\]/*}] -exclude_elements [list {u_arr\[0\]/sig} {u_arr\[0\]/sig2}] -supply {primary /SS}
            set_scope /
        "#;
        let res_r12_3 = parse_upf_and_resolve(upf_r12_3, None, None, finder).unwrap();
        assert!(!res_r12_3
            .intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error));
        assert_eq!(
            res_r12_3.domain_instances[res_r12_3.domain_by_name["u_sub/PD_LIST_EX"].0 as usize],
            BTreeSet::from(["u_sub/u_arr[0]/sig".to_string()])
        );
        assert_eq!(
            res_r12_3.domain_instances[res_r12_3.domain_by_name["u_sub/PD_DBL_EX"].0 as usize],
            BTreeSet::from(["u_sub/u_arr[0]/sig2".to_string()])
        );
        assert_eq!(
            res_r12_3.domain_instances[res_r12_3.domain_by_name["u_sub/PD_LIST_WC"].0 as usize],
            BTreeSet::from([
                "u_sub/u_arr[0]/bus0".to_string(),
                "u_sub/u_arr[0]/bus1".to_string()
            ])
        );
    }

    let finder_esc_cc: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_sub/u_arr0"],
        &["u_sub/u_arr0/sig"],
    ));
    let upf_esc_cc = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        set_scope u_sub
        create_power_domain PD_ESC_CC -elements [list {u_arr\[0-1\]/sig}] -supply {primary /SS}
        set_scope /
    "#;
    let res_esc_cc = parse_upf_and_resolve(upf_esc_cc, None, None, finder_esc_cc).unwrap();
    assert!(res_esc_cc
        .intent
        .diagnostics
        .iter()
        .any(|d| d.rule_id == "UPF_UNRESOLVED_TARGET"));

    let finder_def: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_sub/u_arr[0]"],
        &["u_sub/u_arr[0]/reg0"],
    ));
    let upf_def_bad = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        set_scope u_sub
        create_power_domain PD -elements {u_arr[0]/reg0} -supply {primary /SS}
        set_isolation iso_def -domain PD -isolation_supply_set /SS -clamp_value 0
        set_level_shifter ls_def -domain PD
        set_retention ret_def -domain PD -retention_supply_set /SS
        set_retention_elements ret_el -domain PD -elements {nonexistent_reg}
        map_isolation_cell iso_def -domain PD -lib_cells {ISO_CELL} -elements {nonexistent_iso_pin}
        map_level_shifter_cell ls_def -domain PD -lib_cells {LS_CELL} -elements {nonexistent_ls_pin}
        map_retention_cell ret_def -domain PD -lib_cells {RET_CELL} -elements {nonexistent_ret_pin}
        map_isolation_cell iso_unmatched -domain PD -lib_cells {ISO_CELL} -elements {nonexistent_unmatched}
        set_scope /
    "#;
    let res_def_bad =
        parse_upf_and_resolve(upf_def_bad, None, None, Arc::clone(&finder_def)).unwrap();
    for needle in [
        "u_sub/nonexistent_reg",
        "u_sub/nonexistent_iso_pin",
        "u_sub/nonexistent_ls_pin",
        "u_sub/nonexistent_ret_pin",
        "u_sub/nonexistent_unmatched",
    ] {
        assert!(res_def_bad.intent.diagnostics.iter().any(|d| {
            d.severity == DiagSeverity::Error
                && d.rule_id == "UPF_UNRESOLVED_TARGET"
                && d.message.contains(needle)
        }));
    }

    let upf_def_ok = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        set_scope u_sub
        create_power_domain PD -elements {u_arr[0]/reg0} -supply {primary /SS}
        set_isolation iso_def -domain PD -isolation_supply_set /SS -clamp_value 0
        set_level_shifter ls_def -domain PD
        set_retention ret_def -domain PD -retention_supply_set /SS
        set_retention_elements ret_el -domain PD -elements {u_arr[0]/reg0}
        map_isolation_cell iso_def -domain PD -lib_cells {ISO_CELL} -elements [list {u_arr\[0\]/reg0}]
        map_level_shifter_cell ls_def -domain PD -lib_cells {LS_CELL} -elements {u_arr[0]/reg0}
        map_retention_cell ret_def -domain PD -lib_cells {RET_CELL} -elements {u_arr[0]/reg0}
        set_scope /
    "#;
    let res_def_ok = parse_upf_and_resolve(upf_def_ok, None, None, finder_def).unwrap();
    assert!(!res_def_ok
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));
}

#[test]
fn regression_r14_brackets_no_unescape_root_anchor_and_top_dot() {
    assert_eq!(
        escape_rtl_brackets("u_arr[/bus[0-1]/*"),
        "u_arr\\[/bus[0-1]/*"
    );
    assert_eq!(escape_rtl_brackets("u_arr[a[0-1]/*"), "u_arr\\[a[0-1]/*");
    assert_eq!(escape_rtl_brackets("u_arr[a[0]/*"), "u_arr\\[a\\[0\\]/*");
    assert_eq!(
        escape_rtl_brackets("u_arr[foo/bus[0]/*"),
        "u_arr\\[foo/bus\\[0\\]/*"
    );
    assert_eq!(escape_rtl_brackets("u_arr[0"), "u_arr\\[0");
    assert_eq!(escape_rtl_brackets("u_arr[0-/]"), "u_arr\\[0-/]");
    assert_eq!(escape_rtl_brackets("u_arr[0-[]"), "u_arr\\[0-\\[\\]");
    assert_eq!(
        escape_rtl_brackets("u_arr[]/bus[0-1]/*"),
        "u_arr\\[\\]/bus[0-1]/*"
    );
    assert_eq!(
        escape_rtl_brackets("u_arr[0]/bus[0-1]"),
        "u_arr\\[0\\]/bus[0-1]"
    );

    assert!(is_glob_char_class("[!0]"));
    assert!(is_glob_char_class("[01]"));
    assert!(!is_glob_char_class("a\\é[0]"));
    assert!(is_glob_char_class("a\\é[0-1]"));
    assert!(is_glob_char_class("[0-9-a]"));
    assert!(!is_glob_char_class("u_arr[a[0]/*"));
    assert!(is_glob_char_class("u_arr[a[0-1]/*"));

    for bad_pat in [
        "u_arr[/bus[0-1]/*",
        "u_arr[a[0-1]/*",
        "u_arr[0-/]/*",
        "u_arr[0-[]/*",
    ] {
        assert!(!glob(bad_pat, "u_arr0/sig"));
        assert!(!glob(bad_pat, "u_arr0/bus0"));
        let esc = escape_rtl_brackets(bad_pat);
        assert!(!glob(&esc, "u_arr0/sig"));
        assert!(!glob(&esc, "u_arr0/bus0"));
    }
    assert!(glob("[0-9-a]", "-"));
    assert!(glob("[0-9-a]", "5"));
    assert!(glob("[0-9-a]", "a"));
    assert!(!glob("[0-9-a]", ":"));
    assert!(glob("a\\é\\[0\\]", "aé[0]"));
    assert!(glob("a\\é[0-1]", "aé1"));
    assert!(glob("u_arr[\\/-9]", "u_arr/"));
    assert!(glob("u_arr[0-\\[]", "u_arr["));
    assert!(!glob("u_arr[/]/*", "u_arr//x"));
    assert!(glob("u_arr[0-\\[]", "u_arr9"));

    let finder_t1: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_sub/u_arr0"],
        &["u_sub/u_arr0/sig", "u_sub/u_arr0/bus0"],
    ));
    let upf_t1 = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        set_scope u_sub
        create_power_domain PD_U_CC -elements {u_arr[/bus[0-1]/*} -supply {primary /SS}
        create_power_domain PD_U_NEST -elements {u_arr[a[0-1]/*} -supply {primary /SS}
        set_scope /
    "#;
    let res_t1 = parse_upf_and_resolve(upf_t1, None, None, finder_t1).unwrap();
    for pd_name in ["u_sub/PD_U_CC", "u_sub/PD_U_NEST"] {
        let pd = &res_t1.intent.domains[res_t1.domain_by_name[pd_name].0 as usize];
        assert!(res_t1.domain_instances[pd.id.0 as usize].is_empty());
    }
    assert!(
        res_t1
            .intent
            .diagnostics
            .iter()
            .filter(|d| d.rule_id == "UPF_UNRESOLVED_TARGET")
            .count()
            >= 2
    );

    let scopes_t2 = &["u_sub", "u_sub/u_arr[0]", "u_sub/u_arr[0]/u_mid"];
    let insts_t2 = &["u_sub/u_arr[0]/sig", "u_sub/u_arr[0]/u_mid/leaf"];
    let mut t2_results = Vec::new();
    for finder in [
        Arc::new(MockDesignFinder::new(scopes_t2, insts_t2)) as Arc<dyn ObjectFinder>,
        Arc::new(NoUnescapeFinder::new(scopes_t2, insts_t2)) as Arc<dyn ObjectFinder>,
    ] {
        let upf_t2_mid = r#"
            upf_version 2.1
            create_supply_net VDD
            create_supply_net VSS
            create_supply_set SS -function {power VDD} -function {ground VSS}
            set_scope u_sub
            set found_mid [lsort [find_objects . -object_type inst -pattern {u_arr[0]/*/leaf}]]
            if {$found_mid ne {{u_arr[0]/u_mid/leaf}}} {
                error [concat {unexpected mid find_objects: } $found_mid]
            }
            set found_mid_pre [lsort [find_objects . -object_type inst -pattern [list {u_arr\[0\]/*/leaf}]]]
            if {$found_mid_pre ne {{u_arr[0]/u_mid/leaf}}} {
                error [concat {unexpected pre-escaped mid find_objects: } $found_mid_pre]
            }
            create_power_domain PD_MID -elements {u_arr[0]/*/leaf} -supply {primary /SS}
            set_isolation iso_mid -domain PD_MID -isolation_supply_set /SS -clamp_value 0 -elements {u_arr[0]/*/leaf}
            set_retention ret_mid -domain PD_MID -retention_supply_set /SS -elements {u_arr[0]/*/leaf}
            set_level_shifter ls_mid -domain PD_MID -elements {u_arr[0]/*/leaf}
            set_repeater rep_mid -domain PD_MID -repeater_supply /SS -elements {u_arr[0]/*/leaf}
            set_scope /
        "#;
        let res_mid = parse_upf_and_resolve(upf_t2_mid, None, None, Arc::clone(&finder)).unwrap();
        assert!(!res_mid
            .intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error));
        let mid_set =
            res_mid.domain_instances[res_mid.domain_by_name["u_sub/PD_MID"].0 as usize].clone();
        assert_eq!(
            mid_set,
            BTreeSet::from(["u_sub/u_arr[0]/u_mid/leaf".to_string()])
        );

        let upf_t2_mid_pre = r#"
            upf_version 2.1
            create_supply_net VDD
            create_supply_net VSS
            create_supply_set SS -function {power VDD} -function {ground VSS}
            set_scope u_sub
            create_power_domain PD_MID_PRE -elements [list {u_arr\[0\]/*/leaf}] -supply {primary /SS}
            set_isolation iso_mid_pre -domain PD_MID_PRE -isolation_supply_set /SS -clamp_value 0 -elements [list {u_arr\[0\]/*/leaf}]
            set_retention ret_mid_pre -domain PD_MID_PRE -retention_supply_set /SS -elements [list {u_arr\[0\]/*/leaf}]
            set_level_shifter ls_mid_pre -domain PD_MID_PRE -elements [list {u_arr\[0\]/*/leaf}]
            set_repeater rep_mid_pre -domain PD_MID_PRE -repeater_supply /SS -elements [list {u_arr\[0\]/*/leaf}]
            set_scope /
        "#;
        let res_mid_pre =
            parse_upf_and_resolve(upf_t2_mid_pre, None, None, Arc::clone(&finder)).unwrap();
        assert!(!res_mid_pre
            .intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error));
        let mid_pre_set = res_mid_pre.domain_instances
            [res_mid_pre.domain_by_name["u_sub/PD_MID_PRE"].0 as usize]
            .clone();
        assert_eq!(mid_pre_set, mid_set);

        let upf_t2_depth = r#"
            upf_version 2.1
            create_supply_net VDD
            create_supply_net VSS
            create_supply_set SS -function {power VDD} -function {ground VSS}
            set_scope u_sub
            create_power_domain PD_DEPTH -elements [list {u_arr\[0\]/*}] -supply {primary /SS}
            set_isolation iso_depth -domain PD_DEPTH -isolation_supply_set /SS -clamp_value 0 -elements [list {u_arr\[0\]/*}]
            set_retention ret_depth -domain PD_DEPTH -retention_supply_set /SS -elements [list {u_arr\[0\]/*}]
            set_level_shifter ls_depth -domain PD_DEPTH -elements [list {u_arr\[0\]/*}]
            set_repeater rep_depth -domain PD_DEPTH -repeater_supply /SS -elements [list {u_arr\[0\]/*}]
            set_scope /
        "#;
        let res_depth = parse_upf_and_resolve(upf_t2_depth, None, None, finder).unwrap();
        assert!(!res_depth
            .intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error));
        let depth_set = res_depth.domain_instances
            [res_depth.domain_by_name["u_sub/PD_DEPTH"].0 as usize]
            .clone();
        assert_eq!(
            depth_set,
            BTreeSet::from([
                "u_sub/u_arr[0]/sig".to_string(),
                "u_sub/u_arr[0]/u_mid/leaf".to_string(),
            ])
        );
        t2_results.push((mid_set, mid_pre_set, depth_set));
    }
    assert_eq!(t2_results[0], t2_results[1]);

    // T3: Root-anchored patterns across UPF objects and design objects:
    let scopes_t3 = &["u_sub", "u_sub/u_arr[0]"];
    let insts_t3 = &["top_inst", "u_sub/u_arr[0]/bus0"];
    for finder in [
        Arc::new(MockDesignFinder::new(scopes_t3, insts_t3)) as Arc<dyn ObjectFinder>,
        Arc::new(NoUnescapeFinder::new(scopes_t3, insts_t3)) as Arc<dyn ObjectFinder>,
    ] {
        let upf_t3 = r#"
            upf_version 2.1
            create_supply_net VDD
            create_supply_net VSS
            create_supply_set SS -function {power VDD} -function {ground VSS}
            set_scope u_sub
            create_supply_net {sn_arr[0]}
            set_scope /

            if {[find_objects . -object_type supply_net -pattern {/sn_arr[0]}] ne {}} {
                error {root-anchored /sn_arr[0] leaked child supply net}
            }
            if {[find_objects . -object_type supply_net -pattern {/sn_*}] ne {}} {
                error {root-anchored /sn_* leaked child supply net}
            }
            set top_sn_nt [lsort [find_objects . -object_type supply_net -transitive false -pattern {/*}]]
            if {$top_sn_nt ne {VDD VSS}} {
                error [concat {unexpected top /* -transitive false supply_net: } $top_sn_nt]
            }
            set all_sn_t [lsort [find_objects . -object_type supply_net -transitive true -pattern {/*}]]
            if {$all_sn_t ne {VDD VSS {u_sub/sn_arr[0]}}} {
                error [concat {unexpected top /* -transitive true supply_net: } $all_sn_t]
            }
            if {[find_objects . -object_type supply_net -pattern {/u_sub/sn_arr[0]}] ne {{u_sub/sn_arr[0]}}} {
                error {root-anchored /u_sub/sn_arr[0] failed at root}
            }
            if {[find_objects . -object_type inst -pattern {/bus0}] ne {}} {
                error {root-anchored /bus0 leaked nested inst at root}
            }
            if {[find_objects . -object_type inst -pattern {/u_arr[0]/*}] ne {}} {
                error {root-anchored /u_arr[0]/* leaked nested inst at root}
            }
            if {[find_objects . -object_type inst -pattern {/u_sub/u_arr[0]/*}] ne {{u_sub/u_arr[0]/bus0}}} {
                error {root-anchored /u_sub/u_arr[0]/* failed at root}
            }
            set top_inst_nt [find_objects . -object_type inst -transitive false -pattern {/*}]
            if {$top_inst_nt ne {top_inst}} {
                error [concat {unexpected top /* -transitive false inst: } $top_inst_nt]
            }

            set_scope u_sub
            if {[find_objects . -object_type supply_net -pattern {/sn_arr[0]}] ne {}} {
                error {root-anchored /sn_arr[0] leaked child supply net in subscope}
            }
            if {[find_objects . -object_type supply_net -pattern {/sn_*}] ne {}} {
                error {root-anchored /sn_* leaked child supply net in subscope}
            }
            if {[find_objects . -object_type supply_net -pattern {/u_sub/sn_arr[0]}] ne {{sn_arr[0]}}} {
                error {root-anchored /u_sub/sn_arr[0] failed in subscope}
            }
            if {[find_objects . -object_type inst -pattern {/bus0}] ne {}} {
                error {root-anchored /bus0 leaked nested inst in subscope}
            }
            if {[find_objects . -object_type inst -pattern {/u_arr[0]/*}] ne {}} {
                error {root-anchored /u_arr[0]/* leaked nested inst in subscope}
            }
            if {[find_objects . -object_type inst -pattern {/u_sub/u_arr[0]/*}] ne {{u_arr[0]/bus0}}} {
                error {root-anchored /u_sub/u_arr[0]/* failed in subscope}
            }
            set_scope /
        "#;
        let res = parse_upf_and_resolve(upf_t3, None, None, finder).unwrap();
        assert!(!res
            .intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error));
    }

    // T4: Top-scope `-elements {.}` on strategies and unmerged `map_isolation_cell`.
    let finder_t4: Arc<dyn ObjectFinder> =
        Arc::new(MockDesignFinder::new(&["u_sub"], &["top_reg"]));
    let upf_t4 = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD_TOP -elements {.} -supply {primary SS}
        set_isolation iso_top -domain PD_TOP -isolation_supply_set SS -clamp_value 0 -elements {.}
        set_level_shifter ls_top -domain PD_TOP -elements {.}
        set_retention ret_top -domain PD_TOP -retention_supply_set SS -elements {.}
        set_repeater rep_top -domain PD_TOP -repeater_supply SS -elements {.}
        map_isolation_cell iso_unmerged -domain PD_TOP -lib_cells {ISO_X1} -elements {.}
    "#;
    let res_t4 = parse_upf_and_resolve(upf_t4, None, None, finder_t4).unwrap();
    assert!(!res_t4
        .intent
        .diagnostics
        .iter()
        .any(|d| d.rule_id == "UPF_UNRESOLVED_TARGET"));

    let _ = parse_upf_and_resolve("", None, None, Arc::new(NullObjectFinder));
}

#[test]
fn regression_r21_d1_leading_wildcards_and_sv_escaped_bracket_index() {
    assert_eq!(split_tcl_list(r"\bus [0]"), vec![r"\bus [0]"]);
    assert_eq!(split_tcl_list(r"u_top/\bus [0]"), vec![r"u_top/\bus [0]"]);
    assert_eq!(split_tcl_list(r"*\bus [0]"), vec![r"*\bus [0]"]);
    assert_eq!(
        split_tcl_list(r"\bus [0] \bus2 [1]"),
        vec![r"\bus [0]", r"\bus2 [1]"]
    );
    assert_eq!(split_tcl_list(r"*\u:esc :*"), vec![r"*\u:esc :*"]);
    assert_eq!(split_tcl_list(r"?\u:esc :*"), vec![r"?\u:esc :*"]);
    assert_eq!(split_tcl_list(r"*\u.esc .*"), vec![r"*\u.esc .*"]);
    assert_eq!(split_tcl_list(r"*\u/esc /*"), vec![r"*\u/esc /*"]);
    assert_eq!(
        split_tcl_list(r"a\ b f\ g \a b u_arr\[0\]/sig \{a\} \*foo \?bar \[0\] *\a b"),
        vec![
            "a b",
            "f g",
            "a",
            "b",
            "u_arr[0]/sig",
            "{a}",
            "*foo",
            "?bar",
            "[0]",
            "*a",
            "b"
        ]
    );

    assert_eq!(qualify_name("", r"*\u:esc :*", ':'), r"*\u:esc /*");
    assert_eq!(qualify_name("", r"?\u:esc :*", ':'), r"?\u:esc /*");
    assert_eq!(qualify_name("", r"*\u.esc .*", '.'), r"*\u.esc /*");
    assert_eq!(qualify_name("", r"*\u:esc ", ':'), r"*\u:esc ");
    assert_eq!(qualify_name("top", r"*\u:esc :*", ':'), r"top/*\u:esc /*");
    assert_eq!(qualify_name("", r"u_top/\bus [0]", '/'), r"u_top/\bus [0]");
    assert_eq!(
        qualify_name("", r"u_top:\bus [3:0]", ':'),
        r"u_top/\bus [3:0]"
    );

    let scopes = &[
        "\\u:esc ",
        "p\\u:esc ",
        "\\u.esc ",
        "\\u/esc ",
        "u_top",
        "u_top/u_arr[0]",
    ];
    let insts = &[
        "\\u:esc /reg0",
        "\\u:esc /reg1",
        "\\u:esc /skip0",
        "p\\u:esc /reg2",
        "\\u.esc /reg1",
        "\\u/esc /sig",
        "\\u:a /\\u:b /sig",
        "u_top/u_arr[0]/sig",
        "u_top/\\bus [0]",
        "u_top/\\bus [1]",
        "u_top/a b1",
    ];
    for finder in [
        Arc::new(MockDesignFinder::new(scopes, insts)) as Arc<dyn ObjectFinder>,
        Arc::new(RelOnlyFinder::new(scopes, insts)) as Arc<dyn ObjectFinder>,
        Arc::new(NoUnescapeFinder::new(scopes, insts)) as Arc<dyn ObjectFinder>,
    ] {
        let upf = r#"
            upf_version 2.1
            create_supply_net VDD
            create_supply_net VSS
            create_supply_set SS -function {power VDD} -function {ground VSS}
            ::cpf::set_hierarchy_separator :

            set r_star [lsort [find_objects . -object_type inst -pattern {*\u:esc :*}]]
            if {$r_star ne {{\u:esc /reg0} {\u:esc /reg1} {\u:esc /skip0} {p\u:esc /reg2}}} {
                error [concat {unexpected *\u:esc :* matches: } $r_star]
            }
            set r_q [find_objects . -object_type inst -pattern {?\u:esc :*}]
            if {$r_q ne {{p\u:esc /reg2}}} {
                error [concat {unexpected ?\u:esc :* matches: } $r_q]
            }
            set r_bus [find_objects . -object_type inst -pattern {u_top/\bus [0]}]
            if {$r_bus ne {{u_top/\bus [0]}}} {
                error [concat {unexpected u_top/\bus [0] matches: } $r_bus]
            }
            set r_bus_wc [find_objects . -object_type inst -pattern {*\bus [0]}]
            if {$r_bus_wc ne {{u_top/\bus [0]}}} {
                error [concat {unexpected *\bus [0] matches: } $r_bus_wc]
            }
            set r_bus_sub [find_objects u_top -object_type inst -pattern {\bus [0]}]
            if {$r_bus_sub ne {{u_top/\bus [0]}}} {
                error [concat {unexpected find_objects u_top \bus [0] matches: } $r_bus_sub]
            }
            set_scope u_top
            set r_bus_rel [find_objects . -object_type inst -pattern {\bus [0]}]
            if {$r_bus_rel ne {{\bus [0]}}} {
                error [concat {unexpected subscope \bus [0] matches: } $r_bus_rel]
            }
            set_scope /

            create_power_domain PD_LWC -elements {*\u:esc :*} -exclude_elements {*\u:esc :skip*} -supply {primary SS}
            set_isolation iso_lwc -domain PD_LWC -elements {*\u:esc :*} -exclude_elements {*\u:esc :skip*} -isolation_supply_set SS -clamp_value 0
            set_isolation iso_qwc -domain PD_LWC -elements {?\u:esc :*} -isolation_supply_set SS -clamp_value 0
            create_power_domain PD_BUS -elements {u_top/\bus [0] u_top/\bus [1]} -exclude_elements {u_top/\bus [1]} -supply {primary SS}
            set_isolation iso_bus -domain PD_BUS -elements {u_top/\bus [0]} -isolation_supply_set SS -clamp_value 0

            ::cpf::set_hierarchy_separator .
            set r_dot [find_objects . -object_type inst -pattern {*\u.esc .*}]
            if {$r_dot ne {{\u.esc /reg1}}} {
                error [concat {unexpected *\u.esc .* matches: } $r_dot]
            }
            create_power_domain PD_DOT_LWC -elements {*\u.esc .*} -supply {primary SS}
        "#;
        let res = parse_upf_and_resolve(upf, None, None, Arc::clone(&finder)).unwrap();
        assert!(
            !res.intent
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error),
            "unexpected error diagnostics: {:?}",
            res.intent.diagnostics
        );
        let pd_lwc = res.domain_by_name["PD_LWC"];
        assert_eq!(
            res.domain_instances[pd_lwc.0 as usize],
            BTreeSet::from([
                "\\u:esc /reg0".to_string(),
                "\\u:esc /reg1".to_string(),
                "p\\u:esc /reg2".to_string(),
            ])
        );
        assert!(matches!(
            res.match_isolation(Some(pd_lwc), None, "p\\u:esc /reg2", AppliesTo::Outputs),
            StrategyMatch::Matched(iso) if iso.name == "iso_qwc"
        ));
        assert!(matches!(
            res.match_isolation(Some(pd_lwc), None, "\\u:esc /reg0", AppliesTo::Outputs),
            StrategyMatch::Matched(iso) if iso.name == "iso_lwc"
        ));
        assert!(matches!(
            res.match_isolation(Some(pd_lwc), None, "\\u:esc /skip0", AppliesTo::Outputs),
            StrategyMatch::None
        ));
        assert!(matches!(
            res.match_isolation(Some(pd_lwc), None, "\\u.esc /reg1", AppliesTo::Outputs),
            StrategyMatch::None
        ));
        let pd_bus = res.domain_by_name["PD_BUS"];
        assert_eq!(
            res.domain_instances[pd_bus.0 as usize],
            BTreeSet::from(["u_top/\\bus [0]".to_string()])
        );
        assert!(matches!(
            res.match_isolation(Some(pd_bus), None, "u_top/\\bus [0]", AppliesTo::Outputs),
            StrategyMatch::Matched(iso) if iso.name == "iso_bus"
        ));
        assert!(matches!(
            res.match_isolation(Some(pd_bus), None, "u_top/\\bus [1]", AppliesTo::Outputs),
            StrategyMatch::None
        ));
        let pd_dot = res.domain_by_name["PD_DOT_LWC"];
        assert_eq!(
            res.domain_instances[pd_dot.0 as usize],
            BTreeSet::from(["\\u.esc /reg1".to_string()])
        );

        let cpf = r#"
            set_cpf_version 2.0
            set_hierarchy_separator :
            create_power_domain -name PD_CPF_DEF -default
            create_power_domain -name PD_CPF_ESC -instances {*\u:esc :*}
            create_power_domain -name PD_CPF_BUS -instances {u_top/\bus [0]}
            create_isolation_rule -name iso_cpf -from PD_CPF_BUS -pins {u_top/\bus [0]} -isolation_output low
            end_design
        "#;
        let res_cpf = parse_cpf_and_resolve(cpf, None, None, finder).unwrap();
        assert!(
            !res_cpf
                .intent
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error),
            "unexpected CPF error diagnostics: {:?}",
            res_cpf.intent.diagnostics
        );
        assert_eq!(
            res_cpf.domain_instances[res_cpf.domain_by_name["PD_CPF_ESC"].0 as usize],
            BTreeSet::from([
                "\\u:esc /reg0".to_string(),
                "\\u:esc /reg1".to_string(),
                "\\u:esc /skip0".to_string(),
                "p\\u:esc /reg2".to_string(),
            ])
        );
        assert_eq!(
            res_cpf.domain_instances[res_cpf.domain_by_name["PD_CPF_BUS"].0 as usize],
            BTreeSet::from(["u_top/\\bus [0]".to_string()])
        );
    }
}
