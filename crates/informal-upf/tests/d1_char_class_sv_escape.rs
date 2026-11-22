mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::{MockDesignFinder, NoUnescapeFinder, RelOnlyFinder};
use informal_upf::{
    glob, parse_cpf_and_resolve, parse_upf_and_resolve, qualify_name, split_tcl_list, AppliesTo,
    DiagSeverity, ObjectFinder, ObjectType, StrategyMatch,
};

#[test]
fn regression_r22_d1_char_class_before_sv_escaped_ident_units() {
    assert_eq!(split_tcl_list(r"[a-z]\u:esc :*"), vec![r"[a-z]\u:esc :*"]);
    assert_eq!(split_tcl_list(r"*[a-z]\u:esc :*"), vec![r"*[a-z]\u:esc :*"]);
    assert_eq!(split_tcl_list(r"[!0]\u:esc :*"), vec![r"[!0]\u:esc :*"]);
    assert_eq!(
        split_tcl_list(r"foo[0-9]\u.esc .*"),
        vec![r"foo[0-9]\u.esc .*"]
    );
    assert_eq!(split_tcl_list(r"[a-z]\bus [0]"), vec![r"[a-z]\bus [0]"]);
    assert_eq!(
        split_tcl_list(r"[a-z]\u_arr[0] :*"),
        vec![r"[a-z]\u_arr[0] :*"]
    );
    assert_eq!(split_tcl_list(r"[a\-z]\u:esc :*"), vec![r"[a-z]\u:esc :*"]);
    assert_eq!(
        split_tcl_list(r"[a-z]\a b \[a-z\]\a b u_arr[0]\a b u_arr\[0\]/sig a\ b f\ g \a b *\* \*"),
        vec![
            "[a-z]a",
            "b",
            "[a-z]a",
            "b",
            "u_arr[0]a",
            "b",
            "u_arr[0]/sig",
            "a b",
            "f g",
            "a",
            "b",
            "**",
            "*",
        ]
    );

    assert_eq!(qualify_name("", r"[a-z]\u:esc :*", ':'), r"[a-z]\u:esc /*");
    assert_eq!(
        qualify_name("", r"*[a-z]\u:esc :*", ':'),
        r"*[a-z]\u:esc /*"
    );
    assert_eq!(qualify_name("", r"[!0]\u:esc :*", ':'), r"[!0]\u:esc /*");
    assert_eq!(
        qualify_name("", r"foo[0-9]\u.esc .*", '.'),
        r"foo[0-9]\u.esc /*"
    );
    assert_eq!(qualify_name("", r"[a-z]\u:esc ", ':'), r"[a-z]\u:esc ");
    assert_eq!(qualify_name("", r"[a-z]./*", '/'), r"[a-z]./*");
    assert_eq!(qualify_name("", r"[a-z]../*", '/'), r"[a-z]../*");
    assert_eq!(qualify_name("", r"[a-z].:*", ':'), r"[a-z]./*");
    assert_eq!(qualify_name("", r"[a-z]..:*", ':'), r"[a-z]../*");
    assert_eq!(qualify_name("", r"[a-z]\bus [0]", ':'), r"[a-z]\bus [0]");
    assert_eq!(
        qualify_name("", r"[a-z]\u_arr[0] :*", ':'),
        r"[a-z]\u_arr[0] /*"
    );
}

#[test]
fn regression_r22_d1_char_class_before_sv_escaped_ident_find_and_resolve() {
    let scopes = &[
        "1\\u:esc ",
        "\\u:esc ",
        "a\\u:esc ",
        "p\\u:esc ",
        "foo3\\u.esc ",
        "foo9\\u.esc ",
        "\\u.esc ",
        "\\u/esc ",
        "u_top",
        "u_top/\\bus [3:0]",
    ];
    let insts = &[
        "1\\u:esc /reg4",
        "\\u:esc /reg0",
        "\\u:esc /reg1",
        "a\\u:esc /reg3",
        "p\\u:esc /reg2",
        "foo3\\u.esc /reg5",
        "foo9\\u.esc /reg6",
        "\\u.esc /reg1",
        "\\u/esc /sig",
        "\\u:a /\\u:b /sig",
        "u_top/\\bus [0]",
        "u_top/\\bus [3:0]",
        "u_top/\\bus [3:0]/\\leaf [1]",
        "u_top/a b1",
        "u_top/a../leaf",
        "u_top/a./leaf",
        "u_top/u_arr[0]/sig",
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

            set r_cc [lsort [find_objects . -object_type inst -pattern {[a-z]\u:esc :*}]]
            if {$r_cc ne {{a\u:esc /reg3} {p\u:esc /reg2}}} {
                error [concat {unexpected [a-z]\u:esc :* matches: } $r_cc]
            }
            set r_cc_braced [lsort [find_objects . -object_type inst -pattern {{[a-z]\u:esc :*}}]]
            if {$r_cc_braced ne {{a\u:esc /reg3} {p\u:esc /reg2}}} {
                error [concat {unexpected braced [a-z]\u:esc :* matches: } $r_cc_braced]
            }
            set r_star_cc [lsort [find_objects . -object_type inst -pattern {*[a-z]\u:esc :*}]]
            if {$r_star_cc ne {{a\u:esc /reg3} {p\u:esc /reg2}}} {
                error [concat {unexpected *[a-z]\u:esc :* matches: } $r_star_cc]
            }
            set r_neg_cc [lsort [find_objects . -object_type inst -pattern {[!0]\u:esc :*}]]
            if {$r_neg_cc ne {{1\u:esc /reg4} {a\u:esc /reg3} {p\u:esc /reg2}}} {
                error [concat {unexpected [!0]\u:esc :* matches: } $r_neg_cc]
            }

            create_power_domain PD_CC -elements {[a-z]\u:esc :*} -supply {primary SS}
            set_isolation iso_cc -domain PD_CC -elements {[a-z]\u:esc :*} -isolation_supply_set SS -clamp_value 0

            ::cpf::set_hierarchy_separator .
            set r_dot_cc [lsort [find_objects . -object_type inst -pattern {foo[0-9]\u.esc .*}]]
            if {$r_dot_cc ne {{foo3\u.esc /reg5} {foo9\u.esc /reg6}}} {
                error [concat {unexpected foo[0-9]\u.esc .* matches: } $r_dot_cc]
            }
            create_power_domain PD_DOT_CC -elements {foo[0-9]\u.esc .*} -supply {primary SS}
            set_isolation iso_dot_cc -domain PD_DOT_CC -elements {foo[0-9]\u.esc .*} -isolation_supply_set SS -clamp_value 0
        "#;
        let res = parse_upf_and_resolve(upf, None, None, Arc::clone(&finder)).unwrap();
        assert!(
            !res.intent
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error),
            "unexpected UPF error diagnostics: {:?}",
            res.intent.diagnostics
        );
        let pd_cc = res.domain_by_name["PD_CC"];
        assert_eq!(
            res.domain_instances[pd_cc.0 as usize],
            BTreeSet::from(["a\\u:esc /reg3".to_string(), "p\\u:esc /reg2".to_string()])
        );
        assert!(matches!(
            res.match_isolation(Some(pd_cc), None, "a\\u:esc /reg3", AppliesTo::Outputs),
            StrategyMatch::Matched(iso) if iso.name == "iso_cc"
        ));
        assert!(matches!(
            res.match_isolation(Some(pd_cc), None, "1\\u:esc /reg4", AppliesTo::Outputs),
            StrategyMatch::None
        ));
        let pd_dot_cc = res.domain_by_name["PD_DOT_CC"];
        assert_eq!(
            res.domain_instances[pd_dot_cc.0 as usize],
            BTreeSet::from([
                "foo3\\u.esc /reg5".to_string(),
                "foo9\\u.esc /reg6".to_string(),
            ])
        );
        assert!(matches!(
            res.match_isolation(
                Some(pd_dot_cc),
                None,
                "foo3\\u.esc /reg5",
                AppliesTo::Outputs
            ),
            StrategyMatch::Matched(iso) if iso.name == "iso_dot_cc"
        ));

        let cpf = r#"
            set_cpf_version 2.0
            set_hierarchy_separator :
            create_power_domain -name PD_CPF_DEF -default
            create_power_domain -name PD_CC -instances {[a-z]\u:esc :*}
            create_power_domain -name PD_BUS30 -instances {u_top:\bus [3:0]}
            create_power_domain -name PD_LEAF -instances {u_top:\bus [3:0]:\leaf [1]}
            create_isolation_rule -name iso_cc -from PD_CC -pins {[a-z]\u:esc :*} -isolation_output low
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
        let cpf_pd_cc = res_cpf.domain_by_name["PD_CC"];
        assert_eq!(
            res_cpf.domain_instances[cpf_pd_cc.0 as usize],
            BTreeSet::from(["a\\u:esc /reg3".to_string(), "p\\u:esc /reg2".to_string()])
        );
        assert!(matches!(
            res_cpf.match_isolation(Some(cpf_pd_cc), None, "a\\u:esc /reg3", AppliesTo::Outputs),
            StrategyMatch::Matched(iso) if iso.name == "iso_cc"
        ));
        assert!(matches!(
            res_cpf.match_isolation(Some(cpf_pd_cc), None, "1\\u:esc /reg4", AppliesTo::Outputs),
            StrategyMatch::None
        ));
        assert_eq!(
            res_cpf.domain_instances[res_cpf.domain_by_name["PD_BUS30"].0 as usize],
            BTreeSet::from(["u_top/\\bus [3:0]".to_string()])
        );
        assert_eq!(
            res_cpf.domain_instances[res_cpf.domain_by_name["PD_LEAF"].0 as usize],
            BTreeSet::from(["u_top/\\bus [3:0]/\\leaf [1]".to_string()])
        );
    }
}

#[test]
fn regression_2a_normalized_sv_escaped_parent_elements_match() {
    // Live `DesignFinder` stores normalized SV names (`u_blk$1` as Inst, `u_blk$1/y` as Port/Net),
    // while UPF writes `\u_blk$1 /y`. Both `validate_strategy_elements` and
    // `matches_any_element_in_scope` must match cleanly without `UPF_UNRESOLVED_TARGET`.
    struct EscapedPortFinder;
    impl ObjectFinder for EscapedPortFinder {
        fn has_design(&self) -> bool {
            true
        }
        fn scope_exists(&self, scope_path: &str) -> bool {
            let norm = scope_path.trim_matches('/');
            norm.is_empty() || norm == "u_blk$1"
        }
        fn find_objects(
            &self,
            scope_path: &str,
            patterns: &[String],
            obj_type: Option<ObjectType>,
            transitive: bool,
        ) -> Vec<String> {
            let pool: Vec<&str> = match obj_type {
                Some(ObjectType::Inst | ObjectType::Cell) => vec!["u_blk$1"],
                Some(ObjectType::Port | ObjectType::Net | ObjectType::Pin) => vec!["u_blk$1/y"],
                None => vec!["u_blk$1", "u_blk$1/y"],
                _ => Vec::new(),
            };
            let norm_scope = scope_path.trim_matches('/');
            pool.into_iter()
                .filter(|obj| {
                    let rel = if norm_scope.is_empty() {
                        *obj
                    } else if let Some(r) = obj.strip_prefix(&format!("{norm_scope}/")) {
                        r
                    } else {
                        return false;
                    };
                    if !transitive && rel.contains('/') {
                        return false;
                    }
                    let leaf = rel.rsplit('/').next().unwrap_or(rel);
                    patterns
                        .iter()
                        .any(|p| glob(p, obj) || glob(p, rel) || glob(p, leaf))
                })
                .map(str::to_string)
                .collect()
        }
    }
    let finder = Arc::new(EscapedPortFinder);
    let upf = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD_BB -elements {\u_blk$1 } -supply {primary SS}
        set_isolation iso_bb -domain PD_BB -elements {\u_blk$1 /y} -isolation_supply_set SS -clamp_value 0
    "#;
    let res = parse_upf_and_resolve(upf, None, None, finder).unwrap();
    assert!(
        !res.intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error),
        "unexpected diagnostics: {:?}",
        res.intent.diagnostics
    );
    let pd_bb = res.domain_by_name["PD_BB"];
    assert_eq!(
        res.domain_instances[pd_bb.0 as usize],
        BTreeSet::from(["u_blk$1".to_string()])
    );
    assert!(matches!(
        res.match_isolation(Some(pd_bb), None, "u_blk$1/y", AppliesTo::Outputs),
        StrategyMatch::Matched(iso) if iso.name == "iso_bb"
    ));
}

#[test]
fn regression_r3_transitive_elements_hierarchy_and_cpf_to_upf_version_reset() {
    let finder: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_mid", "u_mid/u_leaf", "u_mid$0", "u_mid$0/u_leaf"],
        &[
            "u_mid",
            "u_mid/u_leaf",
            "u_mid/u_excl",
            "u_mid$0",
            "u_mid$0/u_leaf",
            "u_other",
        ],
    ));
    let upf = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS_TOP -function {power VDD} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD} -function {ground VSS}
        create_power_domain PD_TOP -include_scope -supply {primary SS_TOP}
        create_power_domain PD_SRC -elements {u_mid} -exclude_elements {u_mid/u_excl} -supply {primary SS_SW}
        create_power_domain PD_ESC -elements {\u_mid$0 } -supply {primary SS_SW}
    "#;
    let res = parse_upf_and_resolve(upf, None, None, Arc::clone(&finder)).unwrap();
    assert!(
        !res.intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error),
        "{:?}",
        res.intent.diagnostics
    );
    let pd_top = res.domain_by_name["PD_TOP"];
    let pd_src = res.domain_by_name["PD_SRC"];
    let pd_esc = res.domain_by_name["PD_ESC"];
    assert_eq!(
        res.domain_instances[pd_src.0 as usize],
        BTreeSet::from(["u_mid".to_string(), "u_mid/u_leaf".to_string()])
    );
    assert_eq!(
        res.domain_instances[pd_esc.0 as usize],
        BTreeSet::from(["u_mid$0".to_string(), "u_mid$0/u_leaf".to_string()])
    );
    assert_eq!(
        res.domain_instances[pd_top.0 as usize],
        BTreeSet::from(["u_mid/u_excl".to_string(), "u_other".to_string()])
    );

    // Child domain refining `u_mid/u_leaf` inside `PD_SRC -elements {u_mid}` must NOT emit
    // `UPF_DOMAIN_CONFLICT`, regardless of order.
    let child_upf = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD_SRC  -elements {u_mid} -supply {primary SS}
        create_power_domain PD_LEAF -elements {u_mid/u_leaf} -supply {primary SS}
    "#;
    let res_child = parse_upf_and_resolve(child_upf, None, None, finder).unwrap();
    assert!(
        !res_child
            .intent
            .diagnostics
            .iter()
            .any(|d| d.rule_id == "UPF_DOMAIN_CONFLICT"),
        "{:?}",
        res_child.intent.diagnostics
    );
    assert_eq!(
        res_child.domain_instances[res_child.domain_by_name["PD_LEAF"].0 as usize],
        BTreeSet::from(["u_mid/u_leaf".to_string()])
    );
    assert_eq!(
        res_child.domain_instances[res_child.domain_by_name["PD_SRC"].0 as usize],
        BTreeSet::from(["u_mid".to_string(), "u_mid/u_excl".to_string()])
    );

    // Minor note check: `specs[0]` = CPF + `specs[1]` = unversioned UPF clears `Cpf*` during `specs[1]`.
    let dir = common::temp_dir("cpf_to_upf_ver");
    let cpf_p = dir.join("top.cpf");
    let upf_p = dir.join("sub.upf");
    std::fs::write(
        &cpf_p,
        "set_cpf_version 1.1\ncreate_power_domain -name PD_TOP -default\nend_design\n",
    )
    .unwrap();
    std::fs::write(&upf_p, "create_power_domain PD_SUB -elements {u_mid}\n").unwrap();
    let specs = vec![
        informal_upf::UpfFileSpec {
            path: cpf_p,
            scope: None,
            version: None,
            is_cpf: true,
        },
        informal_upf::UpfFileSpec {
            path: upf_p,
            scope: Some("u_mid".to_string()),
            version: None,
            is_cpf: false,
        },
    ];
    let intent = informal_upf::load_upf_files(&specs, Arc::new(informal_upf::NullObjectFinder))
        .expect("load_upf_files cpf+upf");
    assert!(intent.domains.iter().any(|d| d.name == "u_mid/PD_SUB"));
    let _ = std::fs::remove_dir_all(&dir);
}
