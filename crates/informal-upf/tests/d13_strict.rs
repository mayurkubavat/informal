mod common;

use std::sync::Arc;

use common::{temp_dir, MockDesignFinder};
use informal_upf::{
    load_upf, normalize_scope, parse_cpf_and_resolve, parse_logic_expr_in_scope,
    parse_upf_and_resolve, qualify_name, qualify_ref, DiagSeverity, NullObjectFinder, ObjectFinder,
    ObjectType, PstSat, UpfError, DEFAULT_VOLTAGE_TOL_UV,
};

#[test]
fn nested_3_level_load_upf_reentrancy_and_file_line_d13_errors() {
    let dir = temp_dir("nested");
    let leaf = dir.join("leaf.upf");
    let sub = dir.join("sub.upf");
    let top = dir.join("top.upf");

    std::fs::write(
        &leaf,
        r#"upf_version 2.1
create_supply_net VDD_L
create_supply_net VSS
create_supply_set SS_LEAF -function {power VDD_L} -function {ground VSS}
create_power_domain PD_LEAF -include_scope -supply {primary SS_LEAF}
set_isolation iso_leaf -domain PD_LEAF -clamp_value 0 -name_prefix ISO_L_
"#,
    )
    .unwrap();

    std::fs::write(
        &sub,
        r#"upf_version 2.1
set sub_regs [find_objects . -pattern *reg* -object_type inst]
if {[llength $sub_regs] == 0} {
    error "expected sub_regs inside u_sub"
}
create_supply_net VDD_S
create_supply_net VSS
create_supply_set SS_SUB -function {power VDD_S} -function {ground VSS}
create_power_domain PD_SUB -include_scope -supply {primary SS_SUB}
load_upf leaf.upf -scope u_leaf
set_scope ..
"#,
    )
    .unwrap();

    std::fs::write(
        &top,
        r#"upf_version 3.1
set_design_top soc_top
create_supply_net VDD_AON
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
load_upf [file join [file dirname [info script]] sub.upf] -scope u_sub
"#,
    )
    .unwrap();

    let finder: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_sub/u_leaf"],
        &["u_sub/u_reg0", "u_sub/u_leaf/u_reg1"],
    ));
    let resolved = load_upf(&top, None, None, finder.clone()).unwrap();
    assert!(resolved.domain_by_name.contains_key("PD_TOP"));
    assert!(resolved.domain_by_name.contains_key("u_sub/PD_SUB"));
    assert!(resolved.domain_by_name.contains_key("u_sub/u_leaf/PD_LEAF"));
    assert!(resolved
        .intent
        .diagnostics
        .iter()
        .any(|d| d.rule_id == "UPF_D13_NOTE" && d.severity == DiagSeverity::Note));

    // Verify D13 hard error inside 3rd-level nested file reports exact `leaf.upf:line`.
    std::fs::write(
        &leaf,
        "upf_version 2.1\ncreate_power_domain PD_LEAF -include_scope\n# comment\nset_isolation iso_bad -domain PD_LEAF -value_conversion foo\n",
    )
    .unwrap();
    let err = load_upf(&top, None, None, finder.clone()).unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("leaf.upf:4:") && err_msg.contains("unsupported option -value_conversion"),
        "unexpected D13 error message: {err_msg}"
    );

    // Verify unresolved `-scope` errors when design is loaded.
    let bad_top = dir.join("bad_scope.upf");
    std::fs::write(&bad_top, "load_upf sub.upf -scope u_nonexistent\n").unwrap();
    let err_scope = load_upf(&bad_top, None, None, finder).unwrap_err();
    assert!(
        err_scope.to_string().contains("unresolved scope"),
        "unexpected scope error: {err_scope}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn d13_hard_semantic_errors_across_upf_and_cpf() {
    let err_upf_iso_opt = parse_upf_and_resolve(
        "create_power_domain PD_A -include_scope\nset_isolation iso1 -domain PD_A -value_conversion foo\n",
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap_err();
    assert!(
        matches!(&err_upf_iso_opt, UpfError::Tcl { message, .. } if message.contains("unsupported option -value_conversion")),
        "unexpected err: {err_upf_iso_opt}"
    );

    let err_upf_iso_val = parse_upf_and_resolve(
        "create_power_domain PD_A -include_scope\nset_isolation iso1 -domain PD_A -clamp_value bogus\n",
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap_err();
    assert!(matches!(err_upf_iso_val, UpfError::Tcl { .. }));

    let err_upf_ls = parse_upf_and_resolve(
        "create_power_domain PD_A -include_scope\nset_level_shifter ls1 -domain PD_A -rule bogus_rule\n",
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap_err();
    assert!(matches!(err_upf_ls, UpfError::Tcl { .. }));

    let err_cpf_ver = parse_cpf_and_resolve(
        "set_cpf_version 3.0\n",
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap_err();
    assert!(matches!(err_cpf_ver, UpfError::Tcl { .. }));
}

#[test]
fn regression_r8_resolve_scope_arg_with_dot_separator() {
    struct R8DotFinder;
    impl ObjectFinder for R8DotFinder {
        fn has_design(&self) -> bool {
            true
        }
        fn scope_exists(&self, scope: &str) -> bool {
            matches!(
                scope,
                "u_top" | "u_top/u_sub" | "u_top/u_sub/u_leaf" | "u_top/u_other" | "u_other"
            )
        }
        fn find_objects(
            &self,
            scope_path: &str,
            patterns: &[String],
            obj_type: Option<ObjectType>,
            transitive: bool,
        ) -> Vec<String> {
            if !matches!(obj_type, Some(ObjectType::Inst) | None) {
                return Vec::new();
            }
            let insts = [
                "u_top/u_sub/u_reg_sub",
                "u_top/u_sub/u_leaf/u_reg_leaf",
                "u_top/u_other/u_reg_other",
                "u_other/u_reg_root_other",
            ];
            let norm_scope = scope_path.trim_matches('/');
            let prefix = if norm_scope.is_empty() {
                String::new()
            } else {
                format!("{norm_scope}/")
            };
            insts
                .iter()
                .filter(|&&obj| {
                    let Some(rel) = obj.strip_prefix(&prefix) else {
                        return false;
                    };
                    if !transitive && rel.contains('/') {
                        return false;
                    }
                    let short = rel.rsplit('/').next().unwrap_or(rel);
                    patterns.iter().any(|pat| {
                        informal_upf::glob(pat, obj)
                            || informal_upf::glob(pat, rel)
                            || informal_upf::glob(pat, short)
                    })
                })
                .map(|s| s.to_string())
                .collect()
        }
    }

    let dir = temp_dir("r8_dot_scope");
    let dot_sub = dir.join("dot_sub.cpf");
    std::fs::write(
        &dot_sub,
        r#"
        set_cpf_version 2.0
        set_hierarchy_separator .
        set sib [find_objects ../u_other -object_type inst -pattern *]
        if {$sib ne "/u_top/u_other/u_reg_other"} {
            error "expected /u_top/u_other/u_reg_other, got: $sib"
        }
        set leaf [find_objects ./u_leaf -object_type inst -pattern *]
        if {$leaf ne "u_leaf/u_reg_leaf"} {
            error "expected u_leaf/u_reg_leaf, got: $leaf"
        }
        set top_other [find_objects ../../u_other -object_type inst -pattern *]
        if {$top_other ne "/u_other/u_reg_root_other"} {
            error "expected /u_other/u_reg_root_other, got: $top_other"
        }
        set backtrack [find_objects u_a/../u_leaf -object_type inst -pattern *]
        if {$backtrack ne "u_leaf/u_reg_leaf"} {
            error "expected u_leaf/u_reg_leaf via u_a/../u_leaf, got: $backtrack"
        }
        set dot_child [find_objects u_leaf -object_type inst -pattern * -hsc .]
        if {$dot_child ne "u_leaf.u_reg_leaf"} {
            error "expected u_leaf.u_reg_leaf, got: $dot_child"
        }
        end_design
    "#,
    )
    .unwrap();
    let dot_top = format!(
        "upf_version 2.1\nload_upf \"{}\" -format cpf -scope u_top/u_sub\n",
        dot_sub.display()
    );
    parse_upf_and_resolve(&dot_top, None, None, Arc::new(R8DotFinder)).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn regression_r9_scope_traversal_qualify_handles_and_relative_logic_exprs() {
    assert_eq!(normalize_scope("../u_other", '.'), "u_other");
    assert_eq!(normalize_scope("u_a/../u_b", '.'), "u_b");
    assert_eq!(normalize_scope("./u_leaf", '.'), "u_leaf");
    assert_eq!(normalize_scope("/u_top.u_other", '.'), "u_top/u_other");

    assert_eq!(
        qualify_name("u_top/u_sub", "./u_leaf", '.'),
        "u_top/u_sub/u_leaf"
    );
    assert_eq!(
        qualify_name("u_top/u_sub", "../u_other", '.'),
        "u_top/u_other"
    );
    assert_eq!(qualify_name("u_top/u_sub", "../../u_root", '.'), "u_root");
    assert_eq!(
        qualify_name("u_top/u_sub", "u_a/../u_b", '.'),
        "u_top/u_sub/u_b"
    );

    assert_eq!(
        qualify_ref("u_top/u_sub", "./u_leaf", '.'),
        "u_top/u_sub/u_leaf"
    );
    assert_eq!(
        qualify_ref("u_top/u_sub", "../u_other", '.'),
        "u_top/u_other"
    );
    assert_eq!(qualify_ref("u_top/u_sub", "../../u_root", '.'), "u_root");
    assert_eq!(
        qualify_ref("u_top/u_sub", "u_a/../u_b", '.'),
        "u_top/u_sub/u_b"
    );
    assert_eq!(
        qualify_ref("u_top/u_sub", "/u_top.u_other", '.'),
        "/u_top/u_other"
    );
    assert_eq!(
        qualify_ref("u_top/u_sub", "./u_leaf", '/'),
        "u_top/u_sub/u_leaf"
    );
    assert_eq!(
        qualify_ref("u_top/u_sub", "../u_other", '/'),
        "u_top/u_other"
    );

    let finder: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &[
            "u_top",
            "u_top/u_sub",
            "u_top/u_sub/u_leaf",
            "u_top/u_other",
            "u_top/u_sub/u_b",
            "u_other",
        ],
        &[
            "u_top/u_sub/u_reg0",
            "u_top/u_sub/u_leaf/u_reg_leaf",
            "u_top/u_sub/u_leaf/u_reg_leaf2",
            "u_top/u_other/u_reg_other",
            "u_top/u_sub/u_b/u_reg_b",
            "u_other/u_reg_root",
        ],
    ));

    let dir = temp_dir("r9_scope_roundtrip");
    for sep in ['/', '.'] {
        let child_file = dir.join(format!(
            "child_{}.upf",
            if sep == '/' { "slash" } else { "dot" }
        ));
        std::fs::write(
            &child_file,
            r#"
            create_supply_net VDD_CHILD
            create_supply_net VSS_CHILD
            create_supply_set SS_CHILD -function {power VDD_CHILD} -function {ground VSS_CHILD}
            create_power_domain PD_LEAF -elements {u_reg_leaf} -supply {primary SS_CHILD}
            set_isolation iso_leaf -domain PD_LEAF -isolation_supply_set ../../SS_TOP -clamp_value 0
            set_scope ../u_b
            if {[set_scope] ne "u_top/u_sub/u_b"} {
                error "expected u_top/u_sub/u_b inside child set_scope ../u_b, got: [set_scope]"
            }
        "#,
        )
        .unwrap();

        let top_script = format!(
            r#"
            upf_version 2.1
            ::cpf::set_hierarchy_separator {sep}
            create_supply_net VDD_TOP
            create_supply_net VSS_TOP
            create_supply_set SS_TOP -function {{power VDD_TOP}} -function {{ground VSS_TOP}}
            set_scope u_top/u_sub
            create_power_domain PD_SUB -elements {{u_reg0}} -supply {{primary ../SS_TOP}}
            load_upf "{child}" -scope ./u_leaf
            if {{[set_scope] ne "u_top/u_sub"}} {{
                error "expected restore to u_top/u_sub, got: [set_scope]"
            }}
            set_scope ../u_other
            if {{[set_scope] ne "u_top/u_other"}} {{
                error "expected u_top/u_other, got: [set_scope]"
            }}
            set_scope u_a/../../u_sub/u_a/../u_b
            if {{[set_scope] ne "u_top/u_sub/u_b"}} {{
                error "expected u_top/u_sub/u_b via u_a/../../u_sub/u_a/../u_b, got: [set_scope]"
            }}
            set_scope ..
            if {{[set_scope] ne "u_top/u_sub"}} {{
                error "expected u_top/u_sub via .., got: [set_scope]"
            }}
            set_scope ..
            if {{[set_scope] ne "u_top"}} {{
                error "expected u_top via .., got: [set_scope]"
            }}
            set_scope /
            if {{[set_scope] ne "/"}} {{
                error "expected /, got: [set_scope]"
            }}
            set_scope u_top/u_sub
            set nested_other [find_objects ../u_other -object_type inst -pattern * -hsc .]
            if {{$nested_other ne "u_top.u_other.u_reg_other"}} {{
                error "expected u_top.u_other.u_reg_other, got: $nested_other"
            }}
            set in_scope_dot [find_objects u_leaf -object_type inst -pattern u_reg_leaf2 -hsc .]
            if {{$in_scope_dot ne "u_leaf.u_reg_leaf2"}} {{
                error "expected u_leaf.u_reg_leaf2, got: $in_scope_dot"
            }}
            set hier_dot [find_objects . -object_type inst -pattern u_reg0 -include_hier -hsc .]
            if {{$hier_dot ne "u_top.u_sub.u_reg0"}} {{
                error "expected u_top.u_sub.u_reg0, got: $hier_dot"
            }}
            create_power_domain PD_OTHER -elements $nested_other -supply {{primary /SS_TOP}}
            set_isolation iso_other -domain PD_OTHER -elements $nested_other -isolation_supply_set /SS_TOP -clamp_value 0
            set_retention ret_other -domain PD_OTHER -elements $nested_other -retention_supply_set /SS_TOP -save_signal {{save high}}
            set_level_shifter ls_other -domain PD_OTHER -elements $nested_other -rule low_to_high
            create_power_domain PD_LEAF_RT -elements $in_scope_dot -supply {{primary /SS_TOP}}
        "#,
            child = child_file.display()
        );
        let res = parse_upf_and_resolve(&top_script, None, None, finder.clone()).unwrap();
        let pd_sub = &res.intent.domains[res.domain_by_name["u_top/u_sub/PD_SUB"].0 as usize];
        assert_eq!(pd_sub.primary_supply_set.as_deref(), Some("u_top/SS_TOP"));
        let iso_leaf = res
            .intent
            .isolation_strategies
            .iter()
            .find(|i| i.name == "iso_leaf")
            .unwrap();
        assert_eq!(
            iso_leaf.isolation_supply_set.as_deref(),
            Some("u_top/SS_TOP")
        );
        let pd_other = &res.intent.domains[res.domain_by_name["u_top/u_sub/PD_OTHER"].0 as usize];
        assert_eq!(pd_other.elements, vec!["u_top/u_other/u_reg_other"]);
        assert!(res.domain_instances[pd_other.id.0 as usize].contains("u_top/u_other/u_reg_other"));
    }

    let bad_set_scope = r#"
        upf_version 2.1
        ::cpf::set_hierarchy_separator .
        set_scope u_top/u_sub
        set_scope ../u_does_not_exist
    "#;
    assert!(matches!(
        parse_upf_and_resolve(bad_set_scope, None, None, finder.clone()),
        Err(UpfError::Tcl { .. })
    ));
    let bad_child = dir.join("bad_child.upf");
    std::fs::write(&bad_child, "create_supply_net VDD\n").unwrap();
    let bad_nested_scope = format!(
        "upf_version 2.1\n::cpf::set_hierarchy_separator .\nset_scope u_top/u_sub\nload_upf \"{}\" -scope ../u_does_not_exist\n",
        bad_child.display()
    );
    assert!(matches!(
        parse_upf_and_resolve(&bad_nested_scope, None, None, finder.clone()),
        Err(UpfError::Tcl { .. })
    ));

    // R9-1: qualify_handle under sep . for u_sub.SS_AON & PD_TOP.primary:
    let sub_upf_path = dir.join("sub_r9_1.upf");
    std::fs::write(
        &sub_upf_path,
        r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SUB
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SUB -function {power VDD_SUB} -function {ground VSS}
        create_power_domain PD_SUB -include_scope -supply {primary SS_SUB}
        add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_SUB -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
    "#,
    )
    .unwrap();
    let r9_1_upf = format!(
        r#"
        upf_version 2.1
        ::cpf::set_hierarchy_separator .
        create_supply_net VDD_TOP
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_TOP -function {{power VDD_TOP}} -function {{ground VSS}}
        create_supply_set SS_SW  -function {{power VDD_SW}}  -function {{ground VSS}}
        create_power_domain PD_TOP -include_scope -supply {{primary SS_TOP}}
        create_power_domain PD_SW  -elements {{u_other}} -supply {{primary SS_SW}}
        add_power_state SS_TOP -state {{ON -supply_expr {{power == {{FULL_ON 0.9}} && ground == {{FULL_ON 0.0}}}} -simstate NORMAL}}
        add_power_state PD_TOP.primary -state {{ON2 -supply_expr {{power == {{FULL_ON 0.9}} && ground == {{FULL_ON 0.0}}}} -simstate NORMAL}}
        add_power_state SS_SW  -state {{ON -supply_expr {{power == {{FULL_ON 0.9}} && ground == {{FULL_ON 0.0}}}} -simstate NORMAL}}
        add_power_state SS_SW  -state {{OFF -supply_expr {{power == OFF && ground == {{FULL_ON 0.0}}}} -simstate CORRUPT}}
        load_upf "{sub}" -scope u_sub
        create_power_domain PD_WRAP -elements {{u_sub}} -supply {{primary u_sub.SS_AON}}
        set_isolation iso1 -domain PD_SW -isolation_supply_set u_sub.SS_AON -clamp_value 0
        add_power_state u_sub.SS_SUB -state {{OFF -supply_expr {{power == OFF && ground == {{FULL_ON 0.0}}}} -simstate CORRUPT}}
        set_equivalent -sets {{SS_TOP u_sub.SS_SUB}}
    "#,
        sub = sub_upf_path.display()
    );
    let finder_r9_1: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_other", "u_top", "u_top/u_a", "u_top/u_b"],
        &[
            "u_sub/bus[0]",
            "u_sub/u_reg",
            "u_other/u_reg",
            "u_top/u_a/u_reg",
        ],
    ));
    let res_r9_1 = parse_upf_and_resolve(&r9_1_upf, None, None, finder_r9_1).unwrap();
    assert!(!res_r9_1
        .intent
        .diagnostics
        .iter()
        .any(|d| d.rule_id == "UPF_UNRESOLVED_TARGET"));
    assert_eq!(
        res_r9_1
            .intent
            .domains
            .iter()
            .find(|d| d.name == "PD_WRAP")
            .unwrap()
            .primary_supply_set
            .as_deref(),
        Some("u_sub/SS_AON")
    );

    // R9-2: `./` and `../` relative paths in `-logic_expr` and CPF compound conditions:
    assert!(parse_logic_expr_in_scope("../pmu/pwr_ok && local_en", "u_top/u_sub").is_ok());
    assert!(parse_logic_expr_in_scope("./local_en && ../../top_en", "u_top/u_sub").is_ok());
    let r9_2_upf = r#"
        upf_version 2.1
        set_scope u_top/u_sub
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD -include_scope -supply {primary SS}
        add_power_state SS -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {../pmu/pwr_ok && ./local_en} -simstate NORMAL}
        set_scope /
    "#;
    let res_r9_2 = parse_upf_and_resolve(r9_2_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_r9_2 = PstSat::build(&res_r9_2, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_r9_2.is_state_space_consistent());
    assert!(!sat_r9_2
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "UPF_EXPR_SYNTAX"));

    let r9_2_cpf = r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD VSS}
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_SW -instances {u_reg}
        create_isolation_rule -name iso_rel -from PD_SW -isolation_condition {!(../pmu/iso_en) && !(local_en)} -isolation_output low
        end_design
    "#;
    let res_cpf_rel = parse_cpf_and_resolve(
        r9_2_cpf,
        Some("u_top/u_sub"),
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap();
    assert_eq!(
        res_cpf_rel.intent.isolation_strategies[0]
            .isolation_signal
            .as_deref(),
        Some("!(u_top/pmu/iso_en) && !(u_top/u_sub/local_en)")
    );

    let _ = std::fs::remove_dir_all(&dir);
}
