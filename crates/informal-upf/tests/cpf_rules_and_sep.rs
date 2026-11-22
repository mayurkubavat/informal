mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::{temp_dir, MockDesignFinder};
use informal_upf::{
    load_cpf, load_upf, parse_cpf_and_resolve, DiagSeverity, NullObjectFinder, ObjectFinder,
    PstSat, Sense, StrategyLocation, DEFAULT_VOLTAGE_TOL_UV,
};

#[test]
fn regression_r2_cpf_update_rules_scope_isolation() {
    let dir = temp_dir("cpf_update_scope");
    let c1_path = dir.join("c1.cpf");
    let c2_path = dir.join("c2.cpf");
    let top_path = dir.join("top.upf");

    std::fs::write(
        &c1_path,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_SW}
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_AON -default
        create_power_domain -name PD_SW -instances {u_sw}
        update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_SW -primary_power_net VDD_SW -primary_ground_net VSS
        create_isolation_rule -name iso_rule -from PD_SW -to PD_AON -isolation_output low
        update_isolation_rules -names iso_rule -location to -cells {ISO_1}
        create_level_shifter_rule -name ls_rule -from PD_SW -to PD_AON
        update_level_shifter_rules -names ls_rule -location to -cells {LS_1}
        create_state_retention_rule -name ret_rule -domain PD_SW -secondary_power_net VDD_AON
        end_design
    "#,
    )
    .unwrap();

    std::fs::write(
        &c2_path,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_SW}
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_AON -default
        create_power_domain -name PD_SW -instances {u_sw}
        update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_SW -primary_power_net VDD_SW -primary_ground_net VSS
        create_isolation_rule -name iso_rule -from PD_SW -to PD_AON -isolation_output high
        update_isolation_rules -names iso_rule -location from -cells {ISO_2}
        create_level_shifter_rule -name ls_rule -from PD_SW -to PD_AON
        update_level_shifter_rules -names ls_rule -location from -cells {LS_2}
        create_state_retention_rule -name ret_rule -domain PD_SW -secondary_power_net VDD_AON
        update_state_retention_rules -names ret_rule -cells {RET_2} -use_secondary_for_output
        end_design
    "#,
    )
    .unwrap();

    std::fs::write(
        &top_path,
        format!(
            "upf_version 2.1\nload_upf {} -format cpf -scope u_1\nload_upf {} -format cpf -scope u_2\n",
            c1_path.display(),
            c2_path.display()
        ),
    )
    .unwrap();

    let res = load_upf(&top_path, None, None, Arc::new(NullObjectFinder)).unwrap();
    let iso_u1 = res
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.scope == "u_1" && i.name == "iso_rule")
        .unwrap();
    let iso_u2 = res
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.scope == "u_2" && i.name == "iso_rule")
        .unwrap();
    assert_eq!(iso_u1.location, Some(StrategyLocation::Parent));
    assert_eq!(iso_u1.mapped_cells, vec!["ISO_1".to_string()]);
    assert_eq!(iso_u2.location, Some(StrategyLocation::Self_));
    assert_eq!(iso_u2.mapped_cells, vec!["ISO_2".to_string()]);

    let ls_u1 = res
        .intent
        .level_shifter_strategies
        .iter()
        .find(|l| l.scope == "u_1" && l.name == "ls_rule")
        .unwrap();
    let ls_u2 = res
        .intent
        .level_shifter_strategies
        .iter()
        .find(|l| l.scope == "u_2" && l.name == "ls_rule")
        .unwrap();
    assert_eq!(ls_u1.location, Some(StrategyLocation::Parent));
    assert_eq!(ls_u1.mapped_cells, vec!["LS_1".to_string()]);
    assert_eq!(ls_u2.location, Some(StrategyLocation::Self_));
    assert_eq!(ls_u2.mapped_cells, vec!["LS_2".to_string()]);

    let ret_u1 = res
        .intent
        .retention_strategies
        .iter()
        .find(|r| r.scope == "u_1" && r.name == "ret_rule")
        .unwrap();
    let ret_u2 = res
        .intent
        .retention_strategies
        .iter()
        .find(|r| r.scope == "u_2" && r.name == "ret_rule")
        .unwrap();
    assert!(!ret_u1.use_retention_as_primary);
    assert!(ret_u1.mapped_cells.is_empty());
    assert!(ret_u2.use_retention_as_primary);
    assert_eq!(ret_u2.mapped_cells, vec!["RET_2".to_string()]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn regression_r3_cpf_update_rules_parent_to_child() {
    let dir = temp_dir("cpf_parent_child_rules");
    let child_cpf = dir.join("child.cpf");
    let parent_cpf = dir.join("parent.cpf");
    let top_upf = dir.join("top.upf");
    std::fs::write(
        &child_cpf,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_SW}
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_AON -default
        create_power_domain -name PD_SW -instances {u_sw}
        update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_SW -primary_power_net VDD_SW -primary_ground_net VSS
        create_isolation_rule -name iso_rule -from PD_SW -to PD_AON -isolation_output low
        update_isolation_rules -names iso_rule -location from -cells {ISO_CHILD}
        create_level_shifter_rule -name ls_rule -from PD_SW -to PD_AON
        update_level_shifter_rules -names ls_rule -location from -cells {LS_CHILD}
        create_state_retention_rule -name ret_rule -domain PD_SW -secondary_power_net VDD_AON
        update_state_retention_rules -names ret_rule -cells {RET_CHILD}
        end_design
    "#,
    )
    .unwrap();
    std::fs::write(
        &parent_cpf,
        format!(
            r#"
        set_cpf_version 2.0
        load_upf {} -format cpf -scope u_child
        create_power_nets -nets {{VDD_AON VDD_SW}}
        create_ground_nets -nets {{VSS}}
        create_power_domain -name PD_AON -default
        create_power_domain -name PD_SW -instances {{u_sw}}
        update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_SW -primary_power_net VDD_SW -primary_ground_net VSS
        create_isolation_rule -name iso_rule -from PD_SW -to PD_AON -isolation_output low
        update_isolation_rules -names iso_rule -location to -cells {{ISO_PARENT}}
        update_isolation_rules -names u_child/iso_rule -cells {{ISO_CHILD_EXPLICIT}}
        create_level_shifter_rule -name ls_rule -from PD_SW -to PD_AON
        update_level_shifter_rules -names ls_rule -location to -cells {{LS_PARENT}}
        create_state_retention_rule -name ret_rule -domain PD_SW -secondary_power_net VDD_AON
        update_state_retention_rules -names ret_rule -cells {{RET_PARENT}} -use_secondary_for_output
        end_design
    "#,
            child_cpf.display()
        ),
    )
    .unwrap();
    std::fs::write(
        &top_upf,
        format!(
            "upf_version 2.1\nload_upf {} -format cpf -scope u_parent\n",
            parent_cpf.display()
        ),
    )
    .unwrap();

    let res_cpf = load_upf(&top_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let child_iso = res_cpf
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.scope == "u_parent/u_child" && i.name == "iso_rule")
        .unwrap();
    let parent_iso = res_cpf
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.scope == "u_parent" && i.name == "iso_rule")
        .unwrap();
    assert_eq!(child_iso.location, Some(StrategyLocation::Self_));
    assert_eq!(
        child_iso.mapped_cells,
        vec!["ISO_CHILD".to_string(), "ISO_CHILD_EXPLICIT".to_string()]
    );
    assert_eq!(parent_iso.location, Some(StrategyLocation::Parent));
    assert_eq!(parent_iso.mapped_cells, vec!["ISO_PARENT".to_string()]);

    let child_ls = res_cpf
        .intent
        .level_shifter_strategies
        .iter()
        .find(|l| l.scope == "u_parent/u_child" && l.name == "ls_rule")
        .unwrap();
    let parent_ls = res_cpf
        .intent
        .level_shifter_strategies
        .iter()
        .find(|l| l.scope == "u_parent" && l.name == "ls_rule")
        .unwrap();
    assert_eq!(child_ls.location, Some(StrategyLocation::Self_));
    assert_eq!(child_ls.mapped_cells, vec!["LS_CHILD".to_string()]);
    assert_eq!(parent_ls.location, Some(StrategyLocation::Parent));
    assert_eq!(parent_ls.mapped_cells, vec!["LS_PARENT".to_string()]);

    let child_ret = res_cpf
        .intent
        .retention_strategies
        .iter()
        .find(|r| r.scope == "u_parent/u_child" && r.name == "ret_rule")
        .unwrap();
    let parent_ret = res_cpf
        .intent
        .retention_strategies
        .iter()
        .find(|r| r.scope == "u_parent" && r.name == "ret_rule")
        .unwrap();
    assert!(!child_ret.use_retention_as_primary);
    assert_eq!(child_ret.mapped_cells, vec!["RET_CHILD".to_string()]);
    assert!(parent_ret.use_retention_as_primary);
    assert_eq!(parent_ret.mapped_cells, vec!["RET_PARENT".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn regression_r8_cpf_find_objects_hsc_roundtrip_and_signal_sense() {
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
    let cpf_roundtrip = r#"
        set_cpf_version 2.0
        set_hierarchy_separator .
        create_power_nets -nets {VDD} -voltage 0.9
        create_ground_nets -nets {VSS}
        ::upf::set_scope u_top/u_sub
        set found_out [::upf::find_objects ../u_other -object_type inst -pattern * -hsc .]
        if {$found_out ne "u_top.u_other.u_reg_other"} {
            error "expected u_top.u_other.u_reg_other, got: $found_out"
        }
        create_power_domain -name PD_OTHER -instances $found_out
        update_power_domain -name PD_OTHER -primary_power_net /VDD -primary_ground_net /VSS
        create_isolation_rule -name iso_c -from PD_OTHER -pins $found_out -isolation_condition {!(a) && !(b)} -isolation_output low
        create_level_shifter_rule -name ls_c -from PD_OTHER -pins $found_out
        create_state_retention_rule -name ret_c -domain PD_OTHER -instances $found_out -save_edge {!(a || b)} -restore_level {!bus[3:0]}
        ::upf::set_scope /
        end_design
    "#;
    let res_cpf_rt = parse_cpf_and_resolve(cpf_roundtrip, None, None, finder).unwrap();
    let pd_cpf_other =
        &res_cpf_rt.intent.domains[res_cpf_rt.domain_by_name["u_top/u_sub/PD_OTHER"].0 as usize];
    assert_eq!(pd_cpf_other.elements, vec!["u_top/u_other/u_reg_other"]);
    assert!(res_cpf_rt.domain_instances[pd_cpf_other.id.0 as usize]
        .contains("u_top/u_other/u_reg_other"));
    let iso_c = res_cpf_rt
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.name == "iso_c")
        .unwrap();
    assert_eq!(iso_c.elements, vec!["u_top/u_other/u_reg_other"]);
    assert_eq!(
        iso_c.isolation_signal.as_deref(),
        Some("!(u_top/u_sub/a) && !(u_top/u_sub/b)")
    );
    assert_eq!(iso_c.isolation_sense, Some(Sense::High));
    let ls_c = res_cpf_rt
        .intent
        .level_shifter_strategies
        .iter()
        .find(|l| l.name == "ls_c")
        .unwrap();
    assert_eq!(ls_c.elements, vec!["u_top/u_other/u_reg_other"]);
    let ret_c = res_cpf_rt
        .intent
        .retention_strategies
        .iter()
        .find(|r| r.name == "ret_c")
        .unwrap();
    assert_eq!(ret_c.elements, vec!["u_top/u_other/u_reg_other"]);
    assert_eq!(
        ret_c.save_signal.as_deref(),
        Some("u_top/u_sub/a || u_top/u_sub/b")
    );
    assert_eq!(ret_c.save_sense, Some(Sense::Negedge));
    assert_eq!(
        ret_c.restore_signal.as_deref(),
        Some("u_top/u_sub/bus[3:0]")
    );
    assert_eq!(ret_c.restore_sense, Some(Sense::Low));

    let cpf_sense = r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_TOP -default
        update_power_domain -name PD_TOP -primary_power_net VDD -primary_ground_net VSS
        create_power_domain -name PD_SW -instances {u_sw} -shutoff_condition {!u_top/\u_mid /sig}
        update_power_domain -name PD_SW -primary_power_net VDD -primary_ground_net VSS
        create_power_switch_rule -name PSW1 -domain PD_SW
        create_isolation_rule -name i1 -from PD_SW -isolation_condition {!(a) && !(b)} -isolation_output low
        create_isolation_rule -name i2 -from PD_SW -isolation_condition {!(a || b)} -isolation_output low
        create_isolation_rule -name i3 -from PD_SW -isolation_condition {!(iso_en)} -isolation_output low
        create_isolation_rule -name i4 -from PD_SW -isolation_condition {!bus[3:0]} -isolation_output low
        create_isolation_rule -name i5 -from PD_SW -isolation_condition {!u_top/\u_mid /sig} -isolation_output low
        create_state_retention_rule -name r1 -domain PD_SW -save_edge {!bus[3:0]} -save_level {!(iso_en)} -restore_edge {!(a) && !(b)}
        end_design
    "#;
    let res_sense =
        parse_cpf_and_resolve(cpf_sense, None, None, Arc::new(NullObjectFinder)).unwrap();
    let get_iso = |name: &str| {
        res_sense
            .intent
            .isolation_strategies
            .iter()
            .find(|i| i.name == name)
            .unwrap()
    };
    assert_eq!(
        get_iso("i1").isolation_signal.as_deref(),
        Some("!(a) && !(b)")
    );
    assert_eq!(get_iso("i1").isolation_sense, Some(Sense::High));
    assert_eq!(get_iso("i2").isolation_signal.as_deref(), Some("a || b"));
    assert_eq!(get_iso("i2").isolation_sense, Some(Sense::Low));
    assert_eq!(get_iso("i3").isolation_signal.as_deref(), Some("iso_en"));
    assert_eq!(get_iso("i3").isolation_sense, Some(Sense::Low));
    assert_eq!(get_iso("i4").isolation_signal.as_deref(), Some("bus[3:0]"));
    assert_eq!(get_iso("i4").isolation_sense, Some(Sense::Low));
    assert_eq!(
        get_iso("i5").isolation_signal.as_deref(),
        Some("u_top/\\u_mid /sig")
    );
    assert_eq!(get_iso("i5").isolation_sense, Some(Sense::Low));

    let psw = res_sense
        .intent
        .power_switches
        .iter()
        .find(|s| s.name == "PSW1")
        .unwrap();
    assert_eq!(psw.on_states[0].expr, "u_top/\\u_mid /sig");
    let mut sat_sense = PstSat::build(&res_sense, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!sat_sense
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "UPF_EXPR_SYNTAX"));
    assert!(sat_sense.is_state_space_consistent());
}

#[test]
fn regression_r16_r17_r18_cpf_sep_restore_literal_dot_and_c1_b3() {
    let finder: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_sub/u_leaf"],
        &["u_sub/sig_a", "u_sub/u_leaf/sig_b"],
    ));
    let dir = temp_dir("r16_r18_sep");

    // R16: nested load_upf -format cpf with sep . restores / on pop + UpdateCpf* pre-normalization:
    let child_cpf = r#"
        set_cpf_version 2.0
        set_hierarchy_separator .
        set child_hits [find_objects . -pattern {u_leaf.*} -object_type inst]
        if {$child_hits ne {u_leaf/sig_b}} {
            error [concat {expected child sep . hit u_leaf/sig_b, got: } $child_hits]
        }
        end_design
    "#;
    let child_path = dir.join("child.cpf");
    std::fs::write(&child_path, child_cpf).unwrap();
    let parent_file = dir.join("parent.upf");
    let parent_upf = format!(
        r#"
        upf_version 2.1
        load_upf "{}" -scope u_sub -format cpf
        set after_pop [find_objects . -pattern {{u_sub/*}} -object_type inst]
        if {{$after_pop ne {{u_sub/sig_a u_sub/u_leaf/sig_b}}}} {{
            error [concat {{parent sep corrupted after child CPF pop, got: }} $after_pop]
        }}
        "#,
        child_path.to_string_lossy().replace('\\', "\\\\")
    );
    std::fs::write(&parent_file, &parent_upf).unwrap();
    let res_nested = load_upf(&parent_file, None, None, Arc::clone(&finder)).unwrap();
    assert!(!res_nested
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));

    let sub_rules_cpf = r#"
        set_cpf_version 2.0
        create_power_domain -name PD_C -instances u_leaf -default
        create_isolation_rule -name ISO_C -from PD_C -isolation_condition en -isolation_output low
        create_level_shifter_rule -name LS_C -from PD_C -to PD_C
        create_state_retention_rule -name RET_C -domain PD_C -restore_edge rest
        end_design
    "#;
    let sub_rules_path = dir.join("sub_rules.cpf");
    std::fs::write(&sub_rules_path, sub_rules_cpf).unwrap();
    let top_cpf_path = dir.join("top.cpf");
    let cpf_sep_update = format!(
        r#"
        set_cpf_version 2.0
        create_power_domain -name PD_TOP -default
        set_hierarchy_separator .
        load_upf "{}" -scope u_sub -format cpf
        update_isolation_rules -names {{u_sub.ISO_C}} -location to -cells {{ISO_CELL}}
        update_level_shifter_rules -names {{u_sub.LS_C}} -location from -cells {{LS_CELL}}
        update_state_retention_rules -names {{u_sub.RET_C}} -cell_type FF -cells {{RET_CELL}}
        set_hierarchy_separator /
        end_design
    "#,
        sub_rules_path.to_string_lossy().replace('\\', "\\\\")
    );
    std::fs::write(&top_cpf_path, &cpf_sep_update).unwrap();
    let res_cpf_upd = load_cpf(&top_cpf_path, None, None, finder).unwrap();
    assert!(!res_cpf_upd
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));
    assert_eq!(
        res_cpf_upd
            .intent
            .isolation_strategies
            .iter()
            .find(|i| i.name == "ISO_C")
            .unwrap()
            .location,
        Some(StrategyLocation::Parent)
    );
    assert_eq!(
        res_cpf_upd
            .intent
            .level_shifter_strategies
            .iter()
            .find(|l| l.name == "LS_C")
            .unwrap()
            .location,
        Some(StrategyLocation::Self_)
    );
    assert!(res_cpf_upd
        .intent
        .retention_strategies
        .iter()
        .find(|r| r.name == "RET_C")
        .unwrap()
        .mapped_cells
        .contains(&"RET_CELL".to_string()));

    // R17 C1/C4 & B3:
    let c1_cpf = r#"
        set_cpf_version 2.0
        create_power_domain -name PD_C -default
        create_isolation_rule -name iso.v1 -from PD_C -isolation_condition en -isolation_output low
        create_isolation_rule -name iso_1 -from PD_C -isolation_condition en2 -isolation_output high
        create_level_shifter_rule -name ls.v1 -from PD_C -to PD_C
        create_state_retention_rule -name ret.v1 -domain PD_C -restore_edge rest
        update_isolation_rules -names {iso.v1} -location to -cells {ISO_DOT}
        update_isolation_rules -names {./iso_1} -location from -cells {ISO_DS}
        update_level_shifter_rules -names {ls.v1} -location from -cells {LS_DOT}
        update_state_retention_rules -names {ret.v1} -cell_type FF -cells {RET_DOT}
        set_hierarchy_separator .
        end_design
    "#;
    let res_c1_r17 = parse_cpf_and_resolve(c1_cpf, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert_eq!(
        res_c1_r17
            .intent
            .isolation_strategies
            .iter()
            .find(|i| i.name == "iso.v1")
            .unwrap()
            .mapped_cells,
        vec!["ISO_DOT"]
    );
    assert_eq!(
        res_c1_r17
            .intent
            .isolation_strategies
            .iter()
            .find(|i| i.name == "iso_1")
            .unwrap()
            .mapped_cells,
        vec!["ISO_DS"]
    );

    let b3_child = dir.join("b3_child.upf");
    std::fs::write(
        &b3_child,
        "upf_version 2.1\n::cpf::set_hierarchy_separator /\ncreate_supply_net VDD.child\ncreate_supply_net VSS\n",
    )
    .unwrap();
    let b3_parent = format!(
        "set_cpf_version 2.0\nset_hierarchy_separator .\ncreate_power_nets -nets {{VDD_TOP}} -voltage 0.9\ncreate_ground_nets -nets {{VSS}}\nload_upf \"{}\" -scope u_sub -format upf\nend_design\n",
        b3_child.to_string_lossy().replace('\\', "\\\\")
    );
    let res_b3 = parse_cpf_and_resolve(&b3_parent, None, None, Arc::new(NullObjectFinder)).unwrap();
    let b3_net_names: Vec<&str> = res_b3
        .intent
        .supply_nets
        .iter()
        .map(|n| n.name.as_str())
        .collect();
    assert!(b3_net_names.contains(&"u_sub/VDD.child"));
    assert!(!b3_net_names.contains(&"u_sub/VDD/child"));

    // R18 C1 literal-dot rule names & inner `./`/`..` under `sep=.`:
    let c1_child = dir.join("c1_child.cpf");
    std::fs::write(
        &c1_child,
        r#"
        set_cpf_version 2.0
        set_hierarchy_separator .
        create_power_domain -name PD_SUB -default
        create_isolation_rule -name iso.v1 -from PD_SUB -isolation_condition en_sub -isolation_output low
        update_isolation_rules -names {iso.v1} -location to -cells {ISO_SUB_DOT}
        update_isolation_rules -names {../iso.v1} -cells {ISO_UP_DOT}
        end_design
    "#,
    )
    .unwrap();
    let c1_parent = format!(
        r#"
        set_cpf_version 2.0
        set_hierarchy_separator .
        create_power_domain -name PD_TOP -default
        create_isolation_rule -name iso.v1 -from PD_TOP -isolation_condition en1 -isolation_output low
        create_isolation_rule -name iso_1 -from PD_TOP -isolation_condition en2 -isolation_output high
        create_level_shifter_rule -name ls.v1 -from PD_TOP -to PD_TOP
        create_state_retention_rule -name ret.v1 -domain PD_TOP -restore_edge rest
        load_upf "{}" -scope u_sub -format cpf
        update_isolation_rules -names {{iso.v1 ./u_sub/../iso_1}} -location from -cells {{ISO_ROOT}}
        update_isolation_rules -names {{u_sub.iso.v1}} -cells {{ISO_QUAL_DOT}}
        update_isolation_rules -names {{*}} -cells {{ISO_STAR_ROOT}}
        update_level_shifter_rules -names {{ls.v1}} -location to -cells {{LS_DOT}}
        update_state_retention_rules -names {{ret.v1}} -cells {{RET_DOT}}
        end_design
    "#,
        c1_child.to_string_lossy().replace('\\', "\\\\")
    );
    let res_c1 = parse_cpf_and_resolve(&c1_parent, None, None, Arc::new(NullObjectFinder)).unwrap();
    let root_iso_dot = res_c1
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.scope.is_empty() && i.name == "iso.v1")
        .unwrap();
    assert_eq!(root_iso_dot.location, Some(StrategyLocation::Self_));
    assert!(root_iso_dot.mapped_cells.contains(&"ISO_ROOT".to_string()));
    assert!(root_iso_dot
        .mapped_cells
        .contains(&"ISO_STAR_ROOT".to_string()));
    assert!(root_iso_dot
        .mapped_cells
        .contains(&"ISO_UP_DOT".to_string()));
    assert!(res_c1
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.scope.is_empty() && i.name == "iso_1")
        .unwrap()
        .mapped_cells
        .contains(&"ISO_ROOT".to_string()));
    let sub_iso_dot = res_c1
        .intent
        .isolation_strategies
        .iter()
        .find(|i| i.scope == "u_sub" && i.name == "iso.v1")
        .unwrap();
    assert!(sub_iso_dot
        .mapped_cells
        .contains(&"ISO_SUB_DOT".to_string()));
    assert!(sub_iso_dot
        .mapped_cells
        .contains(&"ISO_QUAL_DOT".to_string()));
    assert!(!sub_iso_dot
        .mapped_cells
        .contains(&"ISO_STAR_ROOT".to_string()));
    let ls_dot = res_c1
        .intent
        .level_shifter_strategies
        .iter()
        .find(|l| l.name == "ls.v1")
        .unwrap();
    assert_eq!(ls_dot.location, Some(StrategyLocation::Parent));
    assert_eq!(ls_dot.mapped_cells, vec!["LS_DOT"]);
    let ret_dot = res_c1
        .intent
        .retention_strategies
        .iter()
        .find(|r| r.name == "ret.v1")
        .unwrap();
    assert_eq!(ret_dot.mapped_cells, vec!["RET_DOT"]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn regression_r20_cpf_switch_enable_overrides_domain_shutoff_and_root_ternary() {
    let cpf_with_modes = r#"
        set_cpf_version 2.0
        set_hierarchy_separator :
        create_power_nets -nets {VDD_AON VDD_SW} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off
        create_power_domain -name PD_AON -default
        update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
        create_power_domain -name PD_SW -instances {u_sw} -shutoff_condition {u_sub:old_sleep}
        update_power_domain -name PD_SW -primary_power_net VDD_SW -primary_ground_net VSS
        create_power_switch_rule -name PSW1 -domain PD_SW -external_power_net VDD_AON
        update_power_switch_rule -name PSW1 -enable_condition {u_sub:new_en}
        create_power_mode -name PM_ON -domain_conditions {PD_AON@NC_ON PD_SW@NC_ON} -default
        create_power_mode -name PM_OFF -domain_conditions {PD_AON@NC_ON PD_SW@NC_OFF} -condition {sel ? :u_sub:a : :u_sub:b}
        end_design
    "#;
    let res_modes =
        parse_cpf_and_resolve(cpf_with_modes, None, None, Arc::new(NullObjectFinder)).unwrap();
    let on_state = res_modes
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_PD_SW" && s.state_name == "NC_ON")
        .unwrap();
    let off_state = res_modes
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_PD_SW" && s.state_name == "NC_OFF")
        .unwrap();
    assert_eq!(on_state.logic_expr.as_deref(), Some("u_sub/new_en"));
    assert_eq!(off_state.logic_expr.as_deref(), Some("!u_sub/new_en"));
    let pm_off = &res_modes.intent.pst_tables[0]
        .rows
        .iter()
        .find(|r| r.state_name == "PM_OFF")
        .unwrap();
    assert_eq!(
        pm_off.logic_expr.as_deref(),
        Some("sel ? /u_sub/a : /u_sub/b")
    );
    let mut sat = PstSat::build(&res_modes, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!sat
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "UPF_EXPR_SYNTAX"));
    assert!(sat.is_state_space_consistent());

    // Also verify fallback ON/OFF synthesis without `create_power_mode`:
    let cpf_fallback = r#"
        set_cpf_version 2.0
        set_hierarchy_separator :
        create_power_nets -nets {VDD_AON VDD_SW} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_SW -instances {u_sw} -shutoff_condition {u_sub:old_sleep}
        update_power_domain -name PD_SW -primary_power_net VDD_SW -primary_ground_net VSS
        create_power_switch_rule -name PSW1 -domain PD_SW -external_power_net VDD_AON
        update_power_switch_rule -name PSW1 -enable_condition {u_sub:new_en}
        end_design
    "#;
    let res_fb =
        parse_cpf_and_resolve(cpf_fallback, None, None, Arc::new(NullObjectFinder)).unwrap();
    let fb_on = res_fb
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_PD_SW" && s.state_name == "ON")
        .unwrap();
    let fb_off = res_fb
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_PD_SW" && s.state_name == "OFF")
        .unwrap();
    assert_eq!(fb_on.logic_expr.as_deref(), Some("u_sub/new_en"));
    assert_eq!(fb_off.logic_expr.as_deref(), Some("!u_sub/new_en"));
}

#[test]
fn scoped_cpf_dot_instances_transitive_and_cross_format_version_resets() {
    let dir = temp_dir("cpf_dot_ver");
    let sub_cpf = dir.join("sub.cpf");
    std::fs::write(
        &sub_cpf,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_SUB} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_SUB -instances {.}
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS
        end_design
        "#,
    )
    .unwrap();
    let finder: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_mid", "u_mid/u_leaf"],
        &["u_mid", "u_mid/u_leaf", "u_mid/u_leaf/u_deep"],
    ));
    let res_cpf = informal_upf::load_cpf(&sub_cpf, Some("u_mid"), None, finder).unwrap();
    let pd_sub = res_cpf.domain_by_name["u_mid/PD_SUB"];
    assert_eq!(
        res_cpf.domain_instances[pd_sub.0 as usize],
        BTreeSet::from([
            "u_mid".to_string(),
            "u_mid/u_leaf".to_string(),
            "u_mid/u_leaf/u_deep".to_string(),
        ])
    );

    // 1. Nested `load_upf sub.upf -format upf -scope u_mid` from `top.cpf` (`set_cpf_version 1.1`):
    // `[upf_version]` inside unversioned `sub.upf` must evaluate to `2.1` (not `cpf-1.1`).
    let child_upf = dir.join("child_unver.upf");
    let child_ver_log = dir.join("child_ver.txt");
    std::fs::write(
        &child_upf,
        format!(
            "set fp [open \"{}\" w]; puts $fp [upf_version]; close $fp\ncreate_power_domain PD_SUB -include_scope\n",
            child_ver_log.display()
        ),
    )
    .unwrap();
    let top_cpf = dir.join("top.cpf");
    std::fs::write(
        &top_cpf,
        format!(
            "set_cpf_version 1.1\ncreate_power_domain -name PD_TOP -default\nload_upf \"{}\" -format upf -scope u_mid\nend_design\n",
            child_upf.display()
        ),
    )
    .unwrap();
    let _res_nest =
        informal_upf::load_cpf(&top_cpf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let got_nested = std::fs::read_to_string(&child_ver_log).unwrap();
    assert_eq!(got_nested.trim(), "2.1");

    // 2. 3-file sequential `[CPF, UPF(3.1), unversioned UPF]`: `_pop_scope` after `specs[1]` must
    // not leak `specs[0]`'s `Cpf1_1` into `specs[2]`.
    let mid_upf = dir.join("mid31.upf");
    std::fs::write(
        &mid_upf,
        "upf_version 3.1\ncreate_power_domain PD_M -include_scope\n",
    )
    .unwrap();
    let leaf_ver_log = dir.join("leaf_ver.txt");
    let leaf_upf = dir.join("leaf_unver.upf");
    std::fs::write(
        &leaf_upf,
        format!(
            "set fp [open \"{}\" w]; puts $fp [upf_version]; close $fp\ncreate_power_domain PD_L -include_scope\n",
            leaf_ver_log.display()
        ),
    )
    .unwrap();
    let top_only_cpf = dir.join("top_seq.cpf");
    std::fs::write(
        &top_only_cpf,
        "set_cpf_version 1.1\ncreate_power_domain -name PD_TOP -default\nend_design\n",
    )
    .unwrap();
    let specs = vec![
        informal_upf::UpfFileSpec {
            path: top_only_cpf,
            scope: None,
            version: None,
            is_cpf: true,
        },
        informal_upf::UpfFileSpec {
            path: mid_upf,
            scope: Some("u_mid".to_string()),
            version: None,
            is_cpf: false,
        },
        informal_upf::UpfFileSpec {
            path: leaf_upf,
            scope: Some("u_leaf".to_string()),
            version: None,
            is_cpf: false,
        },
    ];
    let _res_seq = informal_upf::load_upf_files(&specs, Arc::new(NullObjectFinder)).unwrap();
    let got_seq = std::fs::read_to_string(&leaf_ver_log).unwrap();
    assert_eq!(got_seq.trim(), "2.1");

    let _ = std::fs::remove_dir_all(&dir);
}
