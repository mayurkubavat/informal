mod common;

use std::sync::Arc;

use common::MockDesignFinder;
use informal_upf::{
    collect_logic_expr_signal_refs, parse_cpf_and_resolve, parse_upf_and_resolve,
    qualify_signal_ref, AppliesTo, DiagSeverity, NullObjectFinder, PstSat, StrategyMatch, UpfError,
    DEFAULT_VOLTAGE_TOL_UV,
};

#[test]
fn cpf_root_scope_dot_preserved_across_domains_and_rules() {
    let finder: Arc<dyn informal_upf::ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sw", "u_aon"],
        &["u_sw", "u_aon", "u_sw/u_r0", "u_sw/q", "u_aon/u_r1"],
    ));

    let cpf = r#"
        set_cpf_version 2.0
        set_design top
        create_power_nets -nets {VDD_AON VDD_SW} -voltage 0.90
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_ALL -instances {.} -exclude_instances {u_aon} -shutoff_condition {!pwr_en}
        create_power_domain -name PD_NONE -instances {u_sw} -exclude_instances {.}
        create_power_domain -name PD_AON -instances {u_aon}
        update_power_domain -name PD_ALL -primary_power_net VDD_SW -primary_ground_net VSS
        update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
        create_global_connection -net VDD_AON -pins {VDD} -instances {.}
        create_isolation_rule -name iso_dot -from PD_ALL -to PD_AON -pins {.} -isolation_output low
        create_isolation_rule -name iso_excl_dot -from PD_ALL -to PD_AON -pins {u_sw/q} -exclude {.} -isolation_output low
        create_level_shifter_rule -name ls_dot -from PD_ALL -to PD_AON -pins {.}
        create_level_shifter_rule -name ls_excl_dot -from PD_ALL -to PD_AON -pins {u_sw/q} -exclude {.}
        create_state_retention_rule -name ret_dot -domain PD_ALL -instances {.} -secondary_power_net VDD_AON
        create_state_retention_rule -name ret_excl_dot -domain PD_ALL -instances {u_sw/u_r0} -exclude {.} -secondary_power_net VDD_AON
        end_design
    "#;
    let res = parse_cpf_and_resolve(cpf, None, None, finder).unwrap();
    assert!(
        !res.intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error),
        "unexpected errors: {:?}",
        res.intent.diagnostics
    );

    let pd_all = res.domain_by_name["PD_ALL"];
    let pd_none = res.domain_by_name["PD_NONE"];
    let pd_aon = res.domain_by_name["PD_AON"];

    assert_eq!(res.intent.domains[pd_all.0 as usize].elements, vec!["."]);
    assert_eq!(
        res.intent.domains[pd_none.0 as usize].exclude_elements,
        vec!["."]
    );
    assert!(res.domain_instances[pd_all.0 as usize].contains("u_sw"));
    assert!(res.domain_instances[pd_all.0 as usize].contains("u_sw/u_r0"));
    assert!(!res.domain_instances[pd_all.0 as usize].contains("u_aon"));
    assert!(
        res.domain_instances[pd_none.0 as usize].is_empty(),
        "expected `-exclude_instances {{.}}` at root scope to exclude all instances: {:?}",
        res.domain_instances[pd_none.0 as usize]
    );

    // `iso_dot` (`-pins {.}`) matches, while `iso_excl_dot` (`-exclude {.}`) does NOT match.
    assert!(matches!(
        res.match_isolation(Some(pd_all), Some(pd_aon), "u_sw/q", AppliesTo::Outputs),
        StrategyMatch::Matched(iso) if iso.name == "iso_dot"
    ));
    // `ls_dot` (`-pins {.}`) matches, while `ls_excl_dot` (`-exclude {.}`) does NOT match.
    assert!(matches!(
        res.match_level_shifter(Some(pd_all), Some(pd_aon), "u_sw/q", AppliesTo::Outputs),
        StrategyMatch::Matched(ls) if ls.name == "ls_dot"
    ));
    // `ret_dot` (`-instances {.}`) matches, while `ret_excl_dot` (`-exclude {.}`) does NOT match.
    assert!(matches!(
        res.match_retention(pd_all, "u_sw/u_r0"),
        StrategyMatch::Matched(ret) if ret.name == "ret_dot"
    ));

    // Standard CPF uses `-exclude`; `-exclude_pins` must remain D13-rejected.
    let bad_opt = r#"
        set_cpf_version 2.0
        create_power_domain -name PD_A -default
        create_power_domain -name PD_B -instances {u_b}
        create_isolation_rule -name iso_bad -from PD_A -to PD_B -exclude_pins {u_a/q} -isolation_output low
    "#;
    let err = parse_cpf_and_resolve(bad_opt, None, None, Arc::new(NullObjectFinder)).unwrap_err();
    assert!(
        matches!(err, UpfError::Tcl { .. })
            && err.to_string().contains("unsupported option -exclude_pins"),
        "unexpected error: {err}"
    );
}

#[test]
fn upf_create_power_domain_dash_scope_qualifies_elements_and_excludes() {
    let finder: Arc<dyn informal_upf::ObjectFinder> = Arc::new(MockDesignFinder::new(
        &[
            "u_src",
            "u_src/u_leaf",
            "u_src/u_keep",
            "u_other",
            "u_other/u_leaf",
            "u_top",
            "u_top/u_src",
            "u_top/u_src/u_leaf",
        ],
        &[
            "u_src",
            "u_src/u_leaf",
            "u_src/u_keep",
            "u_other",
            "u_other/u_leaf",
            "u_top",
            "u_top/u_src",
            "u_top/u_src/u_leaf",
            "u_src/u_leaf/u_r0",
            "u_src/u_keep/u_r1",
            "u_other/u_leaf/u_r2",
            "u_top/u_src/u_leaf/u_r3",
        ],
    ));

    // 1. Root scope: `-scope u_src -elements {.} -exclude_elements {u_leaf}`
    // must become `elements = ["u_src"]`, `exclude_elements = ["u_src/u_leaf"]`.
    let upf1 = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_SRC -scope u_src -elements {.} -exclude_elements {u_leaf} -supply {primary SS_SW}
    "#;
    let res1 = parse_upf_and_resolve(upf1, None, None, finder.clone()).unwrap();
    assert!(
        !res1
            .intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error),
        "unexpected errors: {:?}",
        res1.intent.diagnostics
    );
    let pd_src = res1.domain_by_name["PD_SRC"];
    let pd_src_def = &res1.intent.domains[pd_src.0 as usize];
    assert_eq!(pd_src_def.scope, "u_src");
    assert_eq!(pd_src_def.elements, vec!["u_src"]);
    assert_eq!(pd_src_def.exclude_elements, vec!["u_src/u_leaf"]);
    assert!(res1.domain_instances[pd_src.0 as usize].contains("u_src"));
    assert!(res1.domain_instances[pd_src.0 as usize].contains("u_src/u_keep"));
    assert!(!res1.domain_instances[pd_src.0 as usize].contains("u_src/u_leaf"));
    assert!(!res1.domain_instances[pd_src.0 as usize].contains("u_src/u_leaf/u_r0"));

    // 2. Root scope: `-scope u_src -elements {u_leaf}` must NOT claim `u_other/u_leaf`.
    let upf2 = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_LEAF -scope u_src -elements {u_leaf} -supply {primary SS_SW}
    "#;
    let res2 = parse_upf_and_resolve(upf2, None, None, finder.clone()).unwrap();
    let pd_leaf = res2.domain_by_name["PD_LEAF"];
    assert_eq!(
        res2.intent.domains[pd_leaf.0 as usize].elements,
        vec!["u_src/u_leaf"]
    );
    assert!(res2.domain_instances[pd_leaf.0 as usize].contains("u_src/u_leaf"));
    assert!(!res2.domain_instances[pd_leaf.0 as usize].contains("u_other/u_leaf"));

    // 3. Nested `set_scope u_top` + `-scope u_src -elements {u_leaf}` -> `u_top/u_src/u_leaf`.
    let upf3 = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        set_scope u_top
        create_power_domain PD_NESTED -scope u_src -elements {u_leaf} -supply {primary /SS_SW}
        set_scope /
    "#;
    let res3 = parse_upf_and_resolve(upf3, None, None, finder).unwrap();
    let pd_nested = res3.domain_by_name["u_top/PD_NESTED"];
    let pd_nested_def = &res3.intent.domains[pd_nested.0 as usize];
    assert_eq!(pd_nested_def.scope, "u_top/u_src");
    assert_eq!(pd_nested_def.elements, vec!["u_top/u_src/u_leaf"]);
    assert!(res3.domain_instances[pd_nested.0 as usize].contains("u_top/u_src/u_leaf/u_r3"));
    assert!(!res3.domain_instances[pd_nested.0 as usize].contains("u_src/u_leaf"));
}

#[test]
fn root_dot_domain_overrides_default_and_include_scope_order_independently() {
    let finder: Arc<dyn informal_upf::ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sw", "u_leaf", "u_spec"],
        &["u_sw", "u_sw/u_r0", "u_leaf", "u_leaf/u_r1", "u_spec"],
    ));

    // 1. CPF: `-default` before `-instances {.} -exclude_instances {u_leaf}` vs reverse order.
    let cpf_default_first = r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_SW} -voltage 0.90
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_AON -default
        create_power_domain -name PD_SW -instances {.} -exclude_instances {u_leaf} -shutoff_condition {!pwr_en}
        create_power_domain -name PD_SPEC -instances {u_spec}
        update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_SW -primary_power_net VDD_SW -primary_ground_net VSS
        update_power_domain -name PD_SPEC -primary_power_net VDD_AON -primary_ground_net VSS
        end_design
    "#;
    let cpf_dot_first = r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_SW} -voltage 0.90
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_SW -instances {.} -exclude_instances {u_leaf} -shutoff_condition {!pwr_en}
        create_power_domain -name PD_AON -default
        create_power_domain -name PD_SPEC -instances {u_spec}
        update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_SW -primary_power_net VDD_SW -primary_ground_net VSS
        update_power_domain -name PD_SPEC -primary_power_net VDD_AON -primary_ground_net VSS
        end_design
    "#;
    let res_cpf_1 = parse_cpf_and_resolve(cpf_default_first, None, None, finder.clone()).unwrap();
    let res_cpf_2 = parse_cpf_and_resolve(cpf_dot_first, None, None, finder.clone()).unwrap();
    for res in [&res_cpf_1, &res_cpf_2] {
        assert!(
            !res.intent
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error),
            "unexpected CPF errors: {:?}",
            res.intent.diagnostics
        );
        let pd_aon = res.domain_by_name["PD_AON"];
        let pd_sw = res.domain_by_name["PD_SW"];
        let pd_spec = res.domain_by_name["PD_SPEC"];
        assert_eq!(res.instance_domain.get("u_sw"), Some(&pd_sw));
        assert_eq!(res.instance_domain.get("u_sw/u_r0"), Some(&pd_sw));
        assert_eq!(res.instance_domain.get("u_leaf"), Some(&pd_aon));
        assert_eq!(res.instance_domain.get("u_leaf/u_r1"), Some(&pd_aon));
        assert_eq!(res.instance_domain.get("u_spec"), Some(&pd_spec));
    }

    // 2. UPF: `-include_scope` before `-elements {.} -exclude_elements {u_leaf}` vs reverse order.
    let upf_inc_first = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_SW -elements {.} -exclude_elements {u_leaf} -supply {primary SS_SW}
        create_power_domain PD_SPEC -elements {u_spec} -supply {primary SS_AON}
    "#;
    let upf_dot_first = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        create_power_domain PD_SW -elements {.} -exclude_elements {u_leaf} -supply {primary SS_SW}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        create_power_domain PD_SPEC -elements {u_spec} -supply {primary SS_AON}
    "#;
    let res_upf_1 = parse_upf_and_resolve(upf_inc_first, None, None, finder.clone()).unwrap();
    let res_upf_2 = parse_upf_and_resolve(upf_dot_first, None, None, finder.clone()).unwrap();
    for res in [&res_upf_1, &res_upf_2] {
        assert!(
            !res.intent
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error),
            "unexpected UPF errors: {:?}",
            res.intent.diagnostics
        );
        let pd_top = res.domain_by_name["PD_TOP"];
        let pd_sw = res.domain_by_name["PD_SW"];
        let pd_spec = res.domain_by_name["PD_SPEC"];
        assert_eq!(res.instance_domain.get("u_sw"), Some(&pd_sw));
        assert_eq!(res.instance_domain.get("u_sw/u_r0"), Some(&pd_sw));
        assert_eq!(res.instance_domain.get("u_leaf"), Some(&pd_top));
        assert_eq!(res.instance_domain.get("u_leaf/u_r1"), Some(&pd_top));
        assert_eq!(res.instance_domain.get("u_spec"), Some(&pd_spec));
    }

    // 3. Two root `{.}` domains must emit `UPF_DOMAIN_CONFLICT`.
    let upf_conflict = r#"
        upf_version 2.1
        create_power_domain PD_A -elements {.}
        create_power_domain PD_B -elements {.}
    "#;
    let res_conf = parse_upf_and_resolve(upf_conflict, None, None, finder.clone()).unwrap();
    assert!(
        res_conf
            .intent
            .diagnostics
            .iter()
            .any(|d| d.rule_id == "UPF_DOMAIN_CONFLICT"),
        "expected UPF_DOMAIN_CONFLICT for competing root `.` domains: {:?}",
        res_conf.intent.diagnostics
    );

    // 4. Root-normalized `/` and `..` from root scope map to `.`; from non-empty scope
    //    they map to `/` so root is never aliased onto the local scope.
    let upf_root_norm = r#"
        upf_version 2.1
        create_power_domain PD_TOP -include_scope
        create_power_domain PD_SLASH -elements {/} -exclude_elements {./u_leaf u_spec}
        create_power_domain PD_DOTDOT -scope u_sw -elements {..} -exclude_elements {..}
    "#;
    let res_norm = parse_upf_and_resolve(upf_root_norm, None, None, finder.clone()).unwrap();
    let pd_slash = res_norm.domain_by_name["PD_SLASH"];
    let pd_dotdot = res_norm.domain_by_name["PD_DOTDOT"];
    assert_eq!(
        res_norm.intent.domains[pd_slash.0 as usize].elements,
        vec!["."]
    );
    assert_eq!(
        res_norm.intent.domains[pd_dotdot.0 as usize].elements,
        vec!["/"]
    );
    assert_eq!(
        res_norm.intent.domains[pd_dotdot.0 as usize].exclude_elements,
        vec!["/"]
    );
    assert!(res_norm.domain_instances[pd_slash.0 as usize].contains("u_sw"));
    assert!(!res_norm.domain_instances[pd_slash.0 as usize].contains("u_leaf"));
    assert!(res_norm.domain_instances[pd_dotdot.0 as usize].is_empty());

    // 5. Positive scoped-root tests: `set_scope` + `-elements {/} -exclude_elements {.}`
    //    and `-scope u_sub -elements {..} -exclude_elements {u_leaf}`, plus scoped strategy
    //    `-elements {/}` and root conflict between `.` and scoped `/`.
    let finder2: Arc<dyn informal_upf::ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sw", "u_sub", "u_sub/u_leaf"],
        &["u_sw", "u_sw/q", "u_sub", "u_sub/u_leaf", "u_sub/u_keep"],
    ));

    let upf_rest = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        create_power_domain PD_AON -include_scope -supply {primary SS_AON}
        set_scope u_sub
        create_power_domain PD_REST -elements {/} -exclude_elements {.} -supply {primary /SS_SW}
        set_isolation iso_root -domain PD_REST -applies_to outputs -elements {/} -clamp_value 0 -isolation_supply_set /SS_AON -isolation_signal /iso_en -isolation_sense high -location parent
        set_scope /
    "#;
    let res_rest = parse_upf_and_resolve(upf_rest, None, None, finder2.clone()).unwrap();
    assert!(
        !res_rest
            .intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error),
        "unexpected errors: {:?}",
        res_rest.intent.diagnostics
    );
    let pd_aon_r = res_rest.domain_by_name["PD_AON"];
    let pd_rest = res_rest.domain_by_name["u_sub/PD_REST"];
    assert_eq!(res_rest.instance_domain.get("u_sw"), Some(&pd_rest));
    assert_eq!(res_rest.instance_domain.get("u_sw/q"), Some(&pd_rest));
    assert_eq!(res_rest.instance_domain.get("u_sub"), Some(&pd_aon_r));
    assert_eq!(
        res_rest.instance_domain.get("u_sub/u_leaf"),
        Some(&pd_aon_r)
    );
    assert!(matches!(
        res_rest.match_isolation(Some(pd_rest), Some(pd_aon_r), "u_sw/q", AppliesTo::Outputs),
        StrategyMatch::Matched(iso) if iso.name == "iso_root"
    ));

    let upf_dotdot_pos = r#"
        upf_version 2.1
        create_power_domain PD_AON -include_scope
        create_power_domain PD_DOTDOT -scope u_sub -elements {..} -exclude_elements {u_leaf}
    "#;
    let res_dd = parse_upf_and_resolve(upf_dotdot_pos, None, None, finder2.clone()).unwrap();
    assert!(
        !res_dd
            .intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error),
        "unexpected errors: {:?}",
        res_dd.intent.diagnostics
    );
    let pd_aon_dd = res_dd.domain_by_name["PD_AON"];
    let pd_dd = res_dd.domain_by_name["PD_DOTDOT"];
    assert_eq!(res_dd.instance_domain.get("u_sw"), Some(&pd_dd));
    assert_eq!(res_dd.instance_domain.get("u_sub"), Some(&pd_dd));
    assert_eq!(res_dd.instance_domain.get("u_sub/u_keep"), Some(&pd_dd));
    assert_eq!(res_dd.instance_domain.get("u_sub/u_leaf"), Some(&pd_aon_dd));

    let upf_root_vs_scoped_slash = r#"
        upf_version 2.1
        create_power_domain PD_ROOT -elements {.}
        set_scope u_sub
        create_power_domain PD_SCOPED_SLASH -elements {/}
        set_scope /
    "#;
    let res_rs = parse_upf_and_resolve(upf_root_vs_scoped_slash, None, None, finder2).unwrap();
    assert!(
        res_rs
            .intent
            .diagnostics
            .iter()
            .any(|d| d.rule_id == "UPF_DOMAIN_CONFLICT"),
        "expected UPF_DOMAIN_CONFLICT between root `.` and scoped `/`: {:?}",
        res_rs.intent.diagnostics
    );
}

#[test]
fn resolved_power_intent_scoped_lookup_helpers() {
    let finder: Arc<dyn informal_upf::ObjectFinder> =
        Arc::new(MockDesignFinder::new(&["u_sub", "u_sub/u_leaf"], &[]));
    let upf = r#"
        upf_version 2.1
        create_supply_net VDD_TOP
        create_supply_net VSS
        create_supply_set SS_TOP -function {power VDD_TOP} -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_TOP}
        set_scope u_sub
        create_supply_net VDD_SUB
        create_supply_set SS_SUB -function {power VDD_SUB} -function {ground /VSS}
        create_power_domain PD_SUB -elements {.} -supply {primary SS_SUB}
        set_scope /
    "#;
    let res = parse_upf_and_resolve(upf, None, None, finder).unwrap();
    let pd_top = res.resolve_domain("/PD_TOP", "u_sub").unwrap();
    assert_eq!(res.domain_name(pd_top), Some("PD_TOP"));
    assert_eq!(res.resolve_domain("PD_TOP", "u_sub/u_leaf"), Some(pd_top));
    let pd_sub = res.resolve_domain("PD_SUB", "u_sub").unwrap();
    assert_eq!(res.domain_name(pd_sub), Some("u_sub/PD_SUB"));
    assert_eq!(res.resolve_domain("/u_sub/PD_SUB", ""), Some(pd_sub));

    let ss_top = res.resolve_supply_set("/SS_TOP", "u_sub").unwrap();
    assert_eq!(
        res.resolve_supply_set("/PD_TOP.primary", "u_sub"),
        Some(ss_top)
    );
    assert_eq!(
        res.resolve_supply_set("PD_TOP.primary", "u_sub/u_leaf"),
        Some(ss_top)
    );
    let ss_sub = res.resolve_supply_set("SS_SUB", "u_sub").unwrap();
    assert_eq!(
        res.resolve_supply_set("PD_SUB.primary", "u_sub"),
        Some(ss_sub)
    );

    assert!(res.resolve_supply_net("/VDD_TOP", "u_sub").is_some());
    assert!(res.resolve_supply_net("VDD_TOP", "u_sub/u_leaf").is_some());
    assert!(res.resolve_supply_net("VDD_SUB", "u_sub").is_some());

    assert!(res.element_matches_in_scope(&["u_sub".to_string()], "u_sub", "u_sub/ft_buf"));
    assert!(res.element_matches_in_scope(&[".".to_string()], "", "u_sub/ft_buf"));
    assert!(!res.element_matches_in_scope(&[".".to_string()], "u_sub", "top_sig"));
    assert!(res.element_matches_in_scope(&["/".to_string()], "u_sub", "top_sig"));
}

#[test]
fn scoped_signal_refs_preserve_root_anchor_for_slash_and_dotdot() {
    assert_eq!(qualify_signal_ref("u_sub", "/sig", '/'), "/sig");
    assert_eq!(qualify_signal_ref("u_sub", "../sig", '/'), "/sig");
    assert_eq!(
        qualify_signal_ref("u_parent/u_sub", "../../sig", '/'),
        "/sig"
    );
    assert_eq!(
        qualify_signal_ref("u_parent/u_sub", "../sig", '/'),
        "/u_parent/sig"
    );
    assert_eq!(
        qualify_signal_ref("u_sub", "u_child/../sig", '/'),
        "u_sub/sig"
    );
    assert_eq!(qualify_signal_ref("u_sub", "sig", '/'), "u_sub/sig");
    assert_eq!(
        qualify_signal_ref("u_sub", r"../\esc$sig [0]", '/'),
        r"/\esc$sig [0]"
    );

    let finder: Arc<dyn informal_upf::ObjectFinder> =
        Arc::new(MockDesignFinder::new(&["u_sub"], &["u_sub", "u_sub/u_ff"]));
    let upf = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
        set_scope u_sub
        create_power_domain PD_SUB -elements {.} -supply {primary /SS_SW}
        set_isolation iso_s -domain PD_SUB -isolation_supply_set /SS_AON -isolation_signal /iso_sig -isolation_sense high -clamp_value 0
        set_isolation iso_d -domain PD_SUB -isolation_supply_set /SS_AON -isolation_signal ../iso_sig2 -isolation_sense high -clamp_value 0
        set_isolation_control iso_d -domain PD_SUB -isolation_signal ../iso_sig2 -isolation_sense high
        set_retention ret_s -domain PD_SUB -retention_supply_set /SS_AON -save_signal {/save_sig high} -restore_signal {../rest_sig low}
        create_power_switch psw_s -domain PD_SUB -input_supply_port {vdd_in /VDD_AON} -output_supply_port {vdd_out /VDD_SW} -control_port {ctrl1 /psw_en} -control_port {ctrl2 ../psw_en2} -on_state {ON vdd_in ctrl1}
        set_scope /
    "#;
    let res = parse_upf_and_resolve(upf, None, None, finder).unwrap();
    let iso_s = res
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.name == "iso_s")
        .unwrap();
    let iso_d = res
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.name == "iso_d")
        .unwrap();
    assert_eq!(iso_s.isolation_signal.as_deref(), Some("/iso_sig"));
    assert_eq!(iso_d.isolation_signal.as_deref(), Some("/iso_sig2"));
    let ret = &res.intent.retention_strategies[0];
    assert_eq!(ret.save_signal.as_deref(), Some("/save_sig"));
    assert_eq!(ret.restore_signal.as_deref(), Some("/rest_sig"));
    let psw = &res.intent.power_switches[0];
    assert_eq!(psw.control_ports[0].1, "/psw_en");
    assert_eq!(psw.control_ports[1].1, "/psw_en2");
}

#[test]
fn r12_scoped_logic_expr_negated_psw_ctrl_and_cross_scope_controls() {
    // 1. Bug 1a: `add_power_state -logic_expr {!(../pwr_en)}` inside 1-level & 2-level `set_scope`
    // must resolve to root `/pwr_en` (producing `PST_INCONSISTENT` against root `{pwr_en}`).
    let finder: Arc<dyn informal_upf::ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_top", "u_top/u_sub"],
        &["u_sub", "u_top", "u_top/u_sub"],
    ));
    let upf_1a = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {pwr_en} -simstate NORMAL}
        set_scope u_sub
        add_power_state /SS_SW -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {!(../pwr_en)} -simstate NORMAL}
        set_scope /
    "#;
    let res_1a = parse_upf_and_resolve(upf_1a, None, None, finder.clone()).unwrap();
    let mut sat_1a = PstSat::build(&res_1a, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        !sat_1a.is_state_space_consistent(),
        "!(../pwr_en) from u_sub must collide with root pwr_en"
    );
    assert!(sat_1a
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "PST_INCONSISTENT"));

    // 2-level `set_scope u_top/u_sub`: `!(../../pwr_en)` -> root `pwr_en` (inconsistent),
    // while `!(../pwr_en)` -> `u_top/pwr_en` (consistent with root `pwr_en`, inconsistent with `u_top`'s `pwr_en`).
    let upf_1a_2lvl = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {pwr_en} -simstate NORMAL}
        set_scope u_top/u_sub
        add_power_state /SS_SW -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {!(../../pwr_en)} -simstate NORMAL}
        set_scope /
    "#;
    let res_1a_2lvl = parse_upf_and_resolve(upf_1a_2lvl, None, None, finder.clone()).unwrap();
    let mut sat_1a_2lvl = PstSat::build(&res_1a_2lvl, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!sat_1a_2lvl.is_state_space_consistent());

    let upf_1a_2lvl_parent = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        set_scope u_top
        add_power_state /SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {pwr_en} -simstate NORMAL}
        set_scope u_sub
        add_power_state /SS_SW -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {!(../pwr_en)} -simstate NORMAL}
        set_scope /
    "#;
    let res_1a_parent =
        parse_upf_and_resolve(upf_1a_2lvl_parent, None, None, finder.clone()).unwrap();
    let mut sat_1a_parent = PstSat::build(&res_1a_parent, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        !sat_1a_parent.is_state_space_consistent(),
        "!(../pwr_en) in u_top/u_sub must collide with u_top/pwr_en"
    );

    // 2. Bug 1b: `create_power_switch -control_port {ctrl !(/sig)}` and `{ctrl !(../sig)}`
    let upf_1b = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        set_scope u_sub
        create_power_domain PD_SUB -elements {.} -supply {primary /SS_SW}
        create_power_switch psw_neg -domain PD_SUB -input_supply_port {in /VDD_AON} -output_supply_port {out /VDD_SW} \
          -control_port {c1 !(/sig)} -control_port {c2 !(../sig)} -ack_port {a1 !(../ack_sig)} -on_state {ON in c1}
        set_scope /u_top/u_sub
        create_power_switch psw_2lvl -domain /u_sub/PD_SUB -input_supply_port {in /VDD_AON} -output_supply_port {out /VDD_SW} \
          -control_port {c3 !(../sig)} -control_port {c4 !(../../sig)} -on_state {ON in c3}
        set_scope /
    "#;
    let res_1b = parse_upf_and_resolve(upf_1b, None, None, finder.clone()).unwrap();
    let psw1 = &res_1b.intent.power_switches[0];
    assert_eq!(psw1.control_ports[0].1, "!(/sig)");
    assert_eq!(psw1.control_ports[1].1, "!(/sig)");
    assert_eq!(psw1.ack_ports[0].1, "!(/ack_sig)");
    assert_eq!(
        collect_logic_expr_signal_refs(&psw1.control_ports[0].1).unwrap(),
        vec!["/sig"]
    );
    let psw2 = &res_1b.intent.power_switches[1];
    assert_eq!(psw2.control_ports[0].1, "!(u_top/sig)");
    assert_eq!(psw2.control_ports[1].1, "!(/sig)");

    // 3. Bug 1c: cross-scope `set_isolation_control` & `set_retention_control` record control scopes.
    let upf_1c = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        set_scope u_sub
        create_power_domain PD_SUB -elements {.} -supply {primary /SS_SW}
        set_isolation iso_split -domain PD_SUB -applies_to outputs -isolation_supply_set /SS_AON -clamp_value 0
        set_retention ret_split -domain PD_SUB -retention_supply_set /SS_AON
        set_scope /
        set_isolation_control iso_split -domain u_sub/PD_SUB -isolation_signal {sig} -isolation_sense high
        set_retention_control ret_split -domain u_sub/PD_SUB -save_signal {sig high} -restore_signal {rst low}
    "#;
    let res_1c = parse_upf_and_resolve(upf_1c, None, None, finder).unwrap();
    let iso_split = &res_1c.intent.isolation_strategies[0];
    assert_eq!(iso_split.scope, "u_sub");
    assert_eq!(iso_split.ctrl_scope.as_deref(), Some(""));
    assert_eq!(iso_split.isolation_signal.as_deref(), Some("sig"));
    let ret_split = &res_1c.intent.retention_strategies[0];
    assert_eq!(ret_split.scope, "u_sub");
    assert_eq!(ret_split.save_scope.as_deref(), Some(""));
    assert_eq!(ret_split.restore_scope.as_deref(), Some(""));

    // 4. Reviewer Minor Finding 1: CPF `-acknowledge_receiver` preserves root anchor via `qualify_signal_ref`.
    let cpf_ack = r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_SW} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_SW -instances {u_sub}
        update_power_domain -name PD_SW -primary_power_net VDD_SW -primary_ground_net VSS
        create_power_switch_rule -name psw_ack -domain PD_SW -external_power_net VDD_AON -enable_condition {../en} -acknowledge_receiver {../ack_sig}
        update_power_switch_rule -name psw_ack -acknowledge_receiver {/ack_sig2}
        end_design
    "#;
    let res_ack =
        parse_cpf_and_resolve(cpf_ack, Some("u_sub"), None, Arc::new(NullObjectFinder)).unwrap();
    assert_eq!(res_ack.intent.power_switches[0].ack_ports[0].1, "/ack_sig2");
}

#[test]
fn r13_cpf_power_switch_enable_condition_1_and_2_without_shutoff() {
    let cpf = r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_SW1 VDD_SW2} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_AON -default
        create_power_domain -name PD_SW1 -instances {u_sw1}
        create_power_domain -name PD_SW2 -instances {u_sw2}
        update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_SW1 -primary_power_net VDD_SW1 -primary_ground_net VSS
        update_power_domain -name PD_SW2 -primary_power_net VDD_SW2 -primary_ground_net VSS
        create_power_switch_rule -name PSW1 -domain PD_SW1 -external_power_net VDD_AON -enable_condition_1 {u_sw1/bad_ctrl}
        create_power_switch_rule -name PSW2 -domain PD_SW2 -external_power_net VDD_AON -enable_condition_1 {en1} -enable_condition_2 {!en2}
        update_power_switch_rule -name PSW1 -enable_condition_1 {../top_en && u_sw1/bad_ctrl}
        end_design
    "#;
    let res = parse_cpf_and_resolve(cpf, Some("u_sub"), None, Arc::new(NullObjectFinder)).unwrap();
    assert!(
        !res.intent
            .diagnostics
            .iter()
            .any(|d| d.rule_id == "UPF_D13_NOTE" && d.message.contains("enable_condition")),
        "enable_condition_1/2 must not emit UPF_D13_NOTE: {:?}",
        res.intent.diagnostics
    );

    let psw1 = res
        .intent
        .power_switches
        .iter()
        .find(|s| s.name == "u_sub/PSW1")
        .expect("u_sub/PSW1");
    assert_eq!(
        psw1.control_ports,
        vec![(
            "ctrl1".to_string(),
            "/top_en && u_sub/u_sw1/bad_ctrl".to_string()
        )]
    );
    assert_eq!(psw1.on_states[0].expr, "/top_en && u_sub/u_sw1/bad_ctrl");
    assert_eq!(
        psw1.off_states[0].expr,
        "!(/top_en && u_sub/u_sw1/bad_ctrl)"
    );

    let psw2 = res
        .intent
        .power_switches
        .iter()
        .find(|s| s.name == "u_sub/PSW2")
        .expect("u_sub/PSW2");
    assert_eq!(
        psw2.control_ports,
        vec![
            ("ctrl1".to_string(), "u_sub/en1".to_string()),
            ("ctrl2".to_string(), "!u_sub/en2".to_string()),
        ]
    );
    assert_eq!(psw2.on_states[0].expr, "u_sub/en1 && (!u_sub/en2)");
    assert_eq!(psw2.off_states[0].expr, "!(u_sub/en1 && (!u_sub/en2))");

    let mut sat = PstSat::build(&res, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat.is_state_space_consistent());
}

#[test]
fn r15_cross_scope_ret_cond_scopes_and_cpf_update_power_switch_rule() {
    let finder: Arc<dyn informal_upf::ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_wrap", "u_wrap/u_sub"],
        &["u_wrap", "u_wrap/u_sub"],
    ));
    let upf = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        set_scope u_wrap/u_sub
        create_power_domain PD_SUB -elements {.} -supply {primary /SS_SW}
        set_retention ret_c -domain PD_SUB -retention_supply_set /SS_AON \
          -save_condition {wrap_bad & /aon_ok} -retention_condition {ret_bad}
        set_scope /
        set_retention_control ret_c -domain u_wrap/u_sub/PD_SUB \
          -save_signal {/clk high} -restore_condition {top_bad}
        set_retention ret_c -domain u_wrap/u_sub/PD_SUB -retention_condition {top_ret_bad} -update
    "#;
    let res = parse_upf_and_resolve(upf, None, None, finder).unwrap();
    let ret = &res.intent.retention_strategies[0];
    assert_eq!(ret.scope, "u_wrap/u_sub");
    assert_eq!(ret.save_scope.as_deref(), Some(""));
    assert_eq!(ret.save_cond_scope, None);
    assert_eq!(ret.restore_cond_scope.as_deref(), Some(""));
    assert_eq!(ret.retention_cond_scope.as_deref(), Some(""));

    let dir = std::env::temp_dir().join(format!("informal_upf_r15_cpf_psw_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let top_cpf = dir.join("top.cpf");
    let sub_cpf = dir.join("sub.cpf");
    std::fs::write(
        &top_cpf,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_switch_rule -name PSW_WRAP -domain PD_WRAP -external_power_net VDD_AON -enable_condition_1 {pwr_en}
        end_design
    "#,
    )
    .unwrap();
    std::fs::write(
        &sub_cpf,
        r#"
        set_cpf_version 2.0
        update_power_switch_rule -name /PSW_WRAP -enable_condition_1 {wrap_bad_ctrl & /other_aon}
        end_design
    "#,
    )
    .unwrap();
    let specs = [
        informal_upf::UpfFileSpec {
            path: top_cpf,
            scope: None,
            version: None,
            is_cpf: true,
        },
        informal_upf::UpfFileSpec {
            path: sub_cpf,
            scope: Some("u_wrap/u_sub".to_string()),
            version: None,
            is_cpf: true,
        },
    ];
    let intent = informal_upf::load_upf_files(&specs, Arc::new(NullObjectFinder)).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let psw = intent
        .power_switches
        .iter()
        .find(|s| s.name == "PSW_WRAP")
        .expect("PSW_WRAP");
    assert_eq!(
        psw.control_ports,
        vec![(
            "ctrl1".to_string(),
            "u_wrap/u_sub/wrap_bad_ctrl & /other_aon".to_string()
        )]
    );
    assert_eq!(
        psw.control_port_scopes.get("ctrl1").map(String::as_str),
        Some("u_wrap/u_sub")
    );
}
