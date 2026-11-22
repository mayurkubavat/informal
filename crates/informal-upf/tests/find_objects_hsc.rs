mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::{temp_dir, MockDesignFinder, NoUnescapeFinder, RelOnlyFinder};
use informal_upf::{
    glob, parse_cpf_and_resolve, parse_upf_and_resolve, qualify_name, split_tcl_list, AppliesTo,
    DiagSeverity, ObjectFinder, ObjectType, StrategyMatch,
};

#[test]
fn regression_r6_find_objects_scope_include_hier_and_roundtrip() {
    struct R6Finder {
        scopes: BTreeSet<String>,
        insts: Vec<String>,
        ports: Vec<String>,
    }
    impl ObjectFinder for R6Finder {
        fn has_design(&self) -> bool {
            true
        }
        fn scope_exists(&self, scope_path: &str) -> bool {
            let norm = scope_path.trim_matches('/');
            norm.is_empty() || self.scopes.contains(norm)
        }
        fn find_objects(
            &self,
            scope_path: &str,
            patterns: &[String],
            obj_type: Option<ObjectType>,
            transitive: bool,
        ) -> Vec<String> {
            let items: &[String] = match obj_type {
                Some(ObjectType::Port) => &self.ports,
                Some(ObjectType::Inst | ObjectType::Reg | ObjectType::Cell) | None => &self.insts,
                _ => return Vec::new(),
            };
            let norm_scope = scope_path.trim_matches('/');
            items
                .iter()
                .filter(|obj| {
                    let rel = if norm_scope.is_empty() {
                        obj.as_str()
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
                .cloned()
                .collect()
        }
    }

    let finder = Arc::new(R6Finder {
        scopes: ["u_sub", "u_sub/u_leaf", "u_top", "u_top/u_sub", "u_other"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        insts: vec![
            "u_sub/u_reg0".to_string(),
            "u_sub/u_leaf/u_reg1".to_string(),
            "u_top/u_sub/u_reg0".to_string(),
            "u_other/u_r2".to_string(),
        ],
        ports: vec!["u_sub/in_a".to_string(), "u_sub/out_b".to_string()],
    });

    let dir = temp_dir("r6_find_objects");
    let sub_path = dir.join("sub.upf");
    std::fs::write(
        &sub_path,
        r#"
        upf_version 2.1
        create_supply_port P_SUB_A -direction in
        create_supply_port P_SUB_B -direction in
        create_supply_net VDD_SUB
        create_supply_net VSS_SUB

        set found_ports [find_objects . -object_type supply_port -pattern P_*]
        if {$found_ports ne "P_SUB_A P_SUB_B"} {
            error "expected P_SUB_A P_SUB_B, got: $found_ports"
        }
        connect_supply_net VDD_SUB -ports $found_ports
        add_port_state P_SUB_A -state {ON 0.9}
        add_port_state P_SUB_B -state {ON 0.9}

        set sub_insts [find_objects . -object_type inst -pattern *reg*]
        if {$sub_insts ne "u_reg0 u_leaf/u_reg1"} {
            error "expected relative insts, got: $sub_insts"
        }
        set non_trans [find_objects . -object_type inst -pattern *reg* -transitive false]
        if {$non_trans ne "u_reg0"} {
            error "expected non-transitive u_reg0, got: $non_trans"
        }
        create_supply_set SS_SUB -function {power VDD_SUB} -function {ground VSS_SUB}
        create_power_domain PD_REGS -elements $sub_insts -supply {primary SS_SUB}
        set_isolation iso_regs -domain PD_REGS -elements $sub_insts -clamp_value 0 -isolation_supply_set SS_SUB
        set_retention ret_regs -domain PD_REGS -elements $sub_insts -retention_supply_set SS_SUB
        set_level_shifter ls_regs -domain PD_REGS -elements $sub_insts
        set sub_io [find_objects . -object_type port -pattern *_a]
        if {$sub_io ne "in_a"} {
            error "expected relative port in_a, got: $sub_io"
        }
        set_port_attributes -ports $sub_io -driver_supply SS_SUB

        set hier_ports [find_objects . -object_type supply_port -pattern P_SUB_A -include_hier]
        if {$hier_ports ne "/u_sub/P_SUB_A"} {
            error "expected /u_sub/P_SUB_A, got: $hier_ports"
        }
        set hier_insts [find_objects . -object_type inst -pattern *reg* -include_hier]
        if {$hier_insts ne "/u_sub/u_reg0 /u_sub/u_leaf/u_reg1"} {
            error "expected /u_sub/... hier insts, got: $hier_insts"
        }
        set no_hier [find_objects . -object_type inst -pattern u_reg0 -include_hier false]
        if {$no_hier ne "u_reg0"} {
            error "expected -include_hier false to yield u_reg0, got: $no_hier"
        }
        create_supply_net VDD_SUB_HIER
        connect_supply_net VDD_SUB_HIER -ports $hier_ports
        create_power_domain PD_HIER -elements $hier_insts -supply {primary SS_SUB}

        set other_insts [find_objects /u_other -object_type inst]
        if {$other_insts ne "/u_other/u_r2"} {
            error "expected /u_other/u_r2, got: $other_insts"
        }
        create_power_domain PD_OTHER -elements $other_insts -supply {primary SS_SUB}
        set_isolation iso_other -domain PD_OTHER -elements $other_insts -clamp_value 0 -isolation_supply_set SS_SUB

        set re_rel [find_objects . -object_type inst -regexp -pattern {^u_reg}]
        if {$re_rel ne "u_reg0"} {
            error "expected regexp ^u_reg -> u_reg0, got: $re_rel"
        }
        set re_hier [find_objects . -object_type inst -include_hier -regexp -pattern {^/u_sub/u_reg}]
        if {$re_hier ne "/u_sub/u_reg0"} {
            error "expected regexp ^/u_sub/u_reg -> /u_sub/u_reg0, got: $re_hier"
        }
        set hsc_dot [find_objects . -object_type inst -pattern u_reg0 -include_hier -hsc .]
        if {$hsc_dot ne "u_sub.u_reg0"} {
            error "expected u_sub.u_reg0 without leading dot, got: $hsc_dot"
        }
    "#,
    )
    .unwrap();

    let nested_path = dir.join("nested.upf");
    std::fs::write(
        &nested_path,
        r#"
        upf_version 2.1
        set n_inst [find_objects . -object_type inst]
        if {$n_inst ne "u_reg0"} {
            error "expected nested relative u_reg0, got: $n_inst"
        }
        create_supply_net VDD_N
        create_supply_net VSS_N
        create_supply_set SS_N -function {power VDD_N} -function {ground VSS_N}
        create_power_domain PD_N -elements $n_inst -supply {primary SS_N}
        set parent_port [find_objects .. -object_type supply_port -pattern P_TOP]
        if {$parent_port ne "/u_top/P_TOP"} {
            error "expected /u_top/P_TOP from .., got: $parent_port"
        }
        connect_supply_net /u_top/VDD_TOP -ports $parent_port
    "#,
    )
    .unwrap();

    let top_upf = format!(
        r#"
        upf_version 2.1
        create_supply_port P_IN -direction in
        if {{[find_objects . -object_type supply_port -pattern P_IN] ne "P_IN"}} {{
            error "root non-hier supply_port should be P_IN"
        }}
        if {{[find_objects . -object_type supply_port -pattern P_IN -include_hier] ne "/P_IN"}} {{
            error "root -include_hier supply_port should be /P_IN"
        }}

        load_upf "{sub}" -scope u_sub

        set_scope u_sub
        set_scope ..
        if {{[find_objects . -object_type supply_port -pattern P_IN] ne "P_IN"}} {{
            error "after set_scope .. back to root, expected P_IN"
        }}

        set_scope u_top
        create_supply_port P_TOP -direction in
        create_supply_net VDD_TOP
        set_scope /
        load_upf "{nested}" -scope u_top/u_sub
    "#,
        sub = sub_path.display(),
        nested = nested_path.display()
    );

    let res = parse_upf_and_resolve(&top_upf, None, None, finder).unwrap();
    assert!(!res
        .intent
        .diagnostics
        .iter()
        .any(|d| d.rule_id == "UPF_UNRESOLVED_TARGET"));
    let vdd_sub = &res.intent.supply_nets[res.net_by_name["u_sub/VDD_SUB"].0 as usize];
    assert_eq!(
        vdd_sub.connected_ports,
        vec!["u_sub/P_SUB_A".to_string(), "u_sub/P_SUB_B".to_string()]
    );
    let vdd_sub_hier = &res.intent.supply_nets[res.net_by_name["u_sub/VDD_SUB_HIER"].0 as usize];
    assert_eq!(
        vdd_sub_hier.connected_ports,
        vec!["u_sub/P_SUB_A".to_string()]
    );
    let pd_regs = res.domain_by_name["u_sub/PD_REGS"];
    assert_eq!(
        res.domain_instances[pd_regs.0 as usize],
        ["u_sub/u_leaf/u_reg1", "u_sub/u_reg0"]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>()
    );
    let pd_other = res.domain_by_name["u_sub/PD_OTHER"];
    assert_eq!(
        res.domain_instances[pd_other.0 as usize],
        ["u_other/u_r2"]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>()
    );
    let pd_n = res.domain_by_name["u_top/u_sub/PD_N"];
    assert_eq!(
        res.domain_instances[pd_n.0 as usize],
        ["u_top/u_sub/u_reg0"]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>()
    );
    let vdd_top = &res.intent.supply_nets[res.net_by_name["u_top/VDD_TOP"].0 as usize];
    assert_eq!(vdd_top.connected_ports, vec!["u_top/P_TOP".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn regression_r10_scope_aliases_glob_escape_and_hsc_formatting() {
    let finder: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_other", "u_top", "u_top/u_a", "u_top/u_b"],
        &[
            "u_sub/bus[0]",
            "u_sub/u_reg",
            "u_other/u_reg",
            "u_top/u_a/u_reg",
        ],
    ));
    let alias_upf = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        set_scope u_sub
        create_supply_net VDD
        set _root_vdd [find_objects / -object_type supply_net -pattern VDD -include_hier -hsc .]
        create_supply_set SS_SUB -function {power VDD} -function {ground /VSS}
        set _other_inst [find_objects .. -object_type inst -pattern u_other -hsc .]
        create_supply_net u_other
        create_power_domain PD_BUS -elements {bus[0]} -supply {primary SS_SUB}
        set_scope /
    "#;
    let res_alias = parse_upf_and_resolve(alias_upf, None, None, Arc::clone(&finder)).unwrap();
    let ss_sub = res_alias
        .intent
        .supply_sets
        .iter()
        .find(|s| s.name == "u_sub/SS_SUB")
        .unwrap();
    assert_eq!(ss_sub.power.as_deref(), Some("u_sub/VDD"));
    assert_eq!(ss_sub.ground.as_deref(), Some("/VSS"));
    assert!(res_alias
        .intent
        .supply_nets
        .iter()
        .any(|n| n.name == "u_sub/u_other"));
    let pd_bus = res_alias
        .intent
        .domains
        .iter()
        .find(|d| d.name == "u_sub/PD_BUS")
        .unwrap();
    assert_eq!(pd_bus.elements, vec!["u_sub/bus[0]"]);

    let glob_shadow_upf = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        set_scope u_sub
        set _unused [find_objects / -object_type inst -pattern * -include_hier -hsc .]
        create_supply_set SS_SUB -function {power /VDD} -function {ground /VSS}
        create_power_domain PD_BUS -elements {bus[0]} -supply {primary SS_SUB}
        set_scope /
    "#;
    let res_glob = parse_upf_and_resolve(glob_shadow_upf, None, None, Arc::clone(&finder)).unwrap();
    assert!(!res_glob
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));
    let pd_glob = res_glob
        .intent
        .domains
        .iter()
        .find(|d| d.name == "u_sub/PD_BUS")
        .unwrap();
    assert!(res_glob.domain_instances[pd_glob.id.0 as usize].contains("u_sub/bus[0]"));

    let r9_3_upf = r#"
        upf_version 2.1
        set objs_from_root [find_objects u_top/u_a -object_type inst -pattern * -hsc .]
        if {$objs_from_root ne "u_top.u_a.u_reg"} {
            error "unexpected find_objects result: $objs_from_root"
        }
        set_scope u_top/u_b
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS_A -function {power VDD} -function {ground VSS}
        create_power_domain PD_A -elements $objs_from_root -supply {primary SS_A}
        set_scope /
    "#;
    let res_r9_3 = parse_upf_and_resolve(r9_3_upf, None, None, Arc::clone(&finder)).unwrap();
    assert!(!res_r9_3
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));
    assert_eq!(
        res_r9_3
            .intent
            .domains
            .iter()
            .find(|d| d.name == "u_top/u_b/PD_A")
            .unwrap()
            .elements,
        vec!["u_top/u_a/u_reg"]
    );

    // R10-3: -hsc . / -hsc : single-segment root paths from subscope & root -include_hier:
    let finder_r10_3: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_other", "u_sub/u_other"],
        &[
            "u_sub/u_reg0",
            "u_other",
            "u_other/u_r2",
            "u_sub/u_other/u_r2",
        ],
    ));
    let r10_3_upf = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS_TOP -function {power VDD} -function {ground VSS}
        set root_vdd_hier [find_objects / -object_type supply_net -pattern VDD -include_hier -hsc .]
        if {$root_vdd_hier ne "/VDD"} {
            error "expected /VDD from root -include_hier -hsc ., got: $root_vdd_hier"
        }
        set_scope u_sub
        create_supply_net VDD
        create_supply_net u_other
        set other_dot [find_objects .. -object_type inst -pattern u_other -hsc .]
        if {$other_dot ne "/u_other"} {
            error "expected /u_other from subscope find_objects .. -hsc ., got: $other_dot"
        }
        set other_colon [find_objects .. -object_type inst -pattern u_other -hsc :]
        if {$other_colon ne "/u_other"} {
            error "expected /u_other from subscope find_objects .. -hsc :, got: $other_colon"
        }
        set other_child_colon [find_objects ../u_other -object_type inst -pattern * -hsc :]
        create_supply_set SS_SUB -function [list power $root_vdd_hier] -function {ground /VSS}
        create_power_domain PD_OTHER -elements $other_dot -supply {primary SS_SUB}
        create_power_domain PD_CHILD -elements $other_child_colon -supply {primary SS_SUB}
        set_scope /
    "#;
    let res_r10_3 = parse_upf_and_resolve(r10_3_upf, None, None, finder_r10_3).unwrap();
    assert!(!res_r10_3
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));
    assert_eq!(
        res_r10_3
            .intent
            .supply_sets
            .iter()
            .find(|s| s.name == "u_sub/SS_SUB")
            .unwrap()
            .power
            .as_deref(),
        Some("/VDD")
    );
    let pd_other_3 =
        &res_r10_3.intent.domains[res_r10_3.domain_by_name["u_sub/PD_OTHER"].0 as usize];
    assert!(res_r10_3.domain_instances[pd_other_3.id.0 as usize].contains("u_other"));
    let pd_child_3 =
        &res_r10_3.intent.domains[res_r10_3.domain_by_name["u_sub/PD_CHILD"].0 as usize];
    assert!(res_r10_3.domain_instances[pd_child_3.id.0 as usize].contains("u_other/u_r2"));
}

#[test]
fn regression_r15_r16_r17_r18_find_objects_sep_dot_d1_fix7_b1_and_parent_dotdot() {
    // R14-2: find_objects -pattern with active sep . & : across RelOnlyFinder:
    let rel_finder: Arc<dyn ObjectFinder> = Arc::new(RelOnlyFinder::new(
        &["u_sub", "u_sub/u_arr[0]"],
        &[
            "top_inst",
            "u_sub/sub_inst",
            "u_sub/u_arr[0]/bus0",
            "u_sub/bus[0:3]",
        ],
    ));
    let upf_sep_dot = r#"
        upf_version 2.1
        ::cpf::set_hierarchy_separator .
        create_supply_net VDD
        set_scope u_sub
        create_supply_net {sn_arr[0]}
        set_scope /

        set sn_hit [find_objects . -object_type supply_net -pattern {u_sub.sn_arr[0]}]
        if {$sn_hit ne {{u_sub/sn_arr[0]}}} {
            error [concat {expected u_sub/sn_arr[0], got: } $sn_hit]
        }
        set inst_hit [find_objects . -object_type inst -pattern {u_sub.u_arr[0].*}]
        if {$inst_hit ne {{u_sub/u_arr[0]/bus0}}} {
            error [concat {expected u_sub/u_arr[0]/bus0, got: } $inst_hit]
        }
        set_scope u_sub
        set sub_hit [find_objects . -object_type inst -pattern {u_arr[0].*}]
        if {$sub_hit ne {{u_arr[0]/bus0}}} {
            error [concat {expected u_arr[0]/bus0, got: } $sub_hit]
        }
        set abs_non_bracket [find_objects . -object_type inst -pattern {/u_sub/sub_inst}]
        if {$abs_non_bracket ne {sub_inst}} {
            error [concat {expected sub_inst via RelOnlyFinder /u_sub/sub_inst, got: } $abs_non_bracket]
        }
        set abs_wc_non_bracket [find_objects . -object_type inst -pattern {/u_sub/*_inst}]
        if {$abs_wc_non_bracket ne {sub_inst}} {
            error [concat {expected sub_inst via RelOnlyFinder /u_sub/*_inst, got: } $abs_wc_non_bracket]
        }
        set_scope /

        ::cpf::set_hierarchy_separator :
        set colon_slice [find_objects . -object_type inst -pattern {u_sub:bus[0:3]}]
        if {$colon_slice ne {{u_sub/bus[0:3]}}} {
            error [concat {expected u_sub/bus[0:3] with : sep, got: } $colon_slice]
        }
    "#;
    let res_sep_dot =
        parse_upf_and_resolve(upf_sep_dot, None, None, Arc::clone(&rel_finder)).unwrap();
    assert!(!res_sep_dot
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));

    let cpf_sep_dot = r#"
        set_cpf_version 2.0
        set_hierarchy_separator .
        set cpf_inst [find_objects . -object_type inst -pattern {u_sub.u_arr[0].*}]
        if {$cpf_inst ne {{u_sub/u_arr[0]/bus0}}} {
            error [concat {CPF expected u_sub/u_arr[0]/bus0, got: } $cpf_inst]
        }
        end_design
    "#;
    let res_cpf_sep = parse_cpf_and_resolve(cpf_sep_dot, None, None, rel_finder).unwrap();
    assert!(!res_cpf_sep
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));

    // R16 `./` pattern:
    let ds_finder: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_sub/u_leaf"],
        &["u_sub/sig_a", "u_sub/u_leaf/sig_b"],
    ));
    let dot_slash_upf = r#"
        upf_version 2.1
        set hits1 [find_objects . -pattern {./u_sub/sig_a} -object_type inst]
        set hits2 [find_objects . -pattern {././u_sub/*} -object_type inst]
        if {$hits1 ne {u_sub/sig_a}} {
            error [concat {expected u_sub/sig_a, got: } $hits1]
        }
        if {$hits2 ne {u_sub/sig_a u_sub/u_leaf/sig_b}} {
            error [concat {expected u_sub/sig_a u_sub/u_leaf/sig_b, got: } $hits2]
        }
    "#;
    let res_ds = parse_upf_and_resolve(dot_slash_upf, None, None, ds_finder).unwrap();
    assert!(!res_ds
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));

    // R17 B1_dot_subscope + -hsc . round-trip:
    let dir = temp_dir("r17_r18_hsc");
    let b1_scopes = &["u_top", "u_top/u_sub"];
    let b1_insts = &["u_top/u_sub/r0", "u_top/u_sub/r1"];
    let b1_sub_cpf = dir.join("b1_sub.cpf");
    std::fs::write(
        &b1_sub_cpf,
        r#"
        set_cpf_version 2.0
        set_hierarchy_separator .
        set hits [find_objects . -object_type inst -pattern {././u_sub.*}]
        if {[llength $hits] != 2} {
            error "expected 2 hits for ././u_sub.*, got: $hits"
        }
        end_design
    "#,
    )
    .unwrap();
    let b1_top = format!(
        "upf_version 2.1\nload_upf \"{}\" -scope u_top -format cpf\n",
        b1_sub_cpf.to_string_lossy().replace('\\', "\\\\")
    );
    for f in [
        Arc::new(MockDesignFinder::new(b1_scopes, b1_insts)) as Arc<dyn ObjectFinder>,
        Arc::new(RelOnlyFinder::new(b1_scopes, b1_insts)) as Arc<dyn ObjectFinder>,
        Arc::new(NoUnescapeFinder::new(b1_scopes, b1_insts)) as Arc<dyn ObjectFinder>,
    ] {
        let res = parse_upf_and_resolve(&b1_top, None, None, f).unwrap();
        assert!(!res
            .intent
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagSeverity::Error));
    }

    let hsc_finder: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["u_sub", "u_sub/u_leaf"],
        &["u_sub/u_leaf/reg_q"],
    ));
    let hsc_sub = dir.join("hsc_sub.upf");
    std::fs::write(
        &hsc_sub,
        r#"
        upf_version 2.1
        set objs [find_objects . -pattern {./u_leaf/*} -object_type inst -hsc .]
        if {$objs ne {u_leaf.reg_q}} {
            error "unexpected find_objects -hsc . result: $objs"
        }
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD_LEAF -elements $objs -supply {primary SS}
        set_isolation iso_leaf -domain PD_LEAF -elements $objs -isolation_supply_set SS -isolation_signal iso_en -isolation_sense high -clamp_value 0
    "#,
    )
    .unwrap();
    let hsc_top = format!(
        "upf_version 2.1\nload_upf \"{}\" -scope u_sub\n",
        hsc_sub.to_string_lossy().replace('\\', "\\\\")
    );
    let res_hsc = parse_upf_and_resolve(&hsc_top, None, None, hsc_finder).unwrap();
    assert!(!res_hsc
        .intent
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagSeverity::Error));
    assert_eq!(
        res_hsc
            .intent
            .domains
            .iter()
            .find(|d| d.name == "u_sub/PD_LEAF")
            .unwrap()
            .elements,
        vec!["u_sub/u_leaf/reg_q"]
    );

    // R18 D1 SV escaped idents + Fix 7 / B1 / leading `../`:
    assert_eq!(qualify_name("", "u_sub:bus[0:3]", ':'), "u_sub/bus[0:3]");
    assert_eq!(
        qualify_name("top", "u_sub:bus[0:3]", ':'),
        "top/u_sub/bus[0:3]"
    );
    assert_eq!(
        qualify_name("", "\\u:esc :bus[0:3]", ':'),
        "\\u:esc /bus[0:3]"
    );
    assert_eq!(
        qualify_name("top", "\\u:esc :bus[0:3]", ':'),
        "top/\\u:esc /bus[0:3]"
    );
    assert_eq!(
        qualify_name("", "u_sub:\\bus[0:3] ", ':'),
        "u_sub/\\bus[0:3] "
    );
    assert_eq!(
        qualify_name("top", "u_sub:\\bus[0:3] ", ':'),
        "top/u_sub/\\bus[0:3] "
    );
    assert_eq!(qualify_name("", "\\u.esc .sig", '.'), "\\u.esc /sig");
    assert_eq!(qualify_name("top", "\\u.esc .sig", '.'), "top/\\u.esc /sig");
    assert_eq!(qualify_name("", "\\u/esc /../sig", '/'), "sig");
    assert_eq!(qualify_name("top", "\\u:esc /../sig", ':'), "top/sig");
    assert_eq!(qualify_name("\\u/esc ", "../sig", '/'), "sig");

    let esc_finder: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &["\\u:esc ", "\\u/esc "],
        &["\\u:esc /u_sub/\\bus[0:3] ", "\\u/esc /sig"],
    ));
    let esc_upf = r#"
        upf_version 2.1
        ::cpf::set_hierarchy_separator :
        set_scope {\u:esc }
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        create_power_domain PD_ESC -include_scope -supply {primary SS}
        set_isolation iso_esc -domain PD_ESC -elements {{u_sub:\bus[0:3] }} -isolation_supply_set SS -isolation_signal iso_en -isolation_sense high -clamp_value 0
        set_scope /
        ::cpf::set_hierarchy_separator /
        set hsc_res [lindex [find_objects {\u/esc } -pattern * -object_type inst -hsc .] 0]
        if {$hsc_res ne {\u/esc .sig}} {
            error "expected {\\u/esc .sig}, got: $hsc_res"
        }
    "#;
    let res_esc = parse_upf_and_resolve(esc_upf, None, None, esc_finder).unwrap();
    assert_eq!(
        res_esc
            .intent
            .isolation_strategies
            .iter()
            .find(|i| i.name == "iso_esc")
            .unwrap()
            .elements,
        vec!["\\u:esc /u_sub/\\bus[0:3] "]
    );

    let f_scopes = ["u_top", "u_top/u_sub", "u_top/u_other"];
    let f_insts = ["u_top/u_sub/r0", "u_top/u_other/r1"];
    for finder in [
        Arc::new(MockDesignFinder::new(&f_scopes, &f_insts)) as Arc<dyn ObjectFinder>,
        Arc::new(RelOnlyFinder::new(&f_scopes, &f_insts)) as Arc<dyn ObjectFinder>,
        Arc::new(NoUnescapeFinder::new(&f_scopes, &f_insts)) as Arc<dyn ObjectFinder>,
    ] {
        let upf_find = r#"
            upf_version 2.1
            create_supply_net VDD
            create_supply_net VSS
            create_supply_net u_top/u_sub/VDD_L
            create_supply_set SS -function {power VDD} -function {ground VSS}
            set_scope u_top
            set r0 [lindex [find_objects . -pattern {./u_sub/./*} -object_type inst -hsc . -include_hier] 0]
            if {$r0 ne {u_top.u_sub.r0}} {
                error "expected u_top.u_sub.r0, got: $r0"
            }
            set nets [find_objects . -pattern {./u_sub/./*} -object_type supply_net]
            if {$nets ne {u_sub/VDD_L}} {
                error "expected u_sub/VDD_L, got: $nets"
            }
            set_scope /
            create_power_domain PD_ALIAS -elements "/./$r0" -supply {primary SS}
            set_isolation iso_alias -domain PD_ALIAS -elements "/././$r0" -isolation_supply_set SS -isolation_signal iso_en -isolation_sense high -clamp_value 0
            set_scope u_top/u_sub
            ::cpf::set_hierarchy_separator .
            set sib [find_objects . -pattern {../u_other.*} -object_type inst]
            if {$sib ne {/u_top/u_other/r1}} {
                error "expected /u_top/u_other/r1 via ../u_other.*, got: $sib"
            }
            set_scope /
        "#;
        let res_find = parse_upf_and_resolve(upf_find, None, None, finder).unwrap();
        assert_eq!(
            res_find
                .intent
                .domains
                .iter()
                .find(|d| d.name == "PD_ALIAS")
                .unwrap()
                .elements,
            vec!["u_top/u_sub/r0"]
        );
        assert_eq!(
            res_find
                .intent
                .isolation_strategies
                .iter()
                .find(|i| i.name == "iso_alias")
                .unwrap()
                .elements,
            vec!["u_top/u_sub/r0"]
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn regression_r20_d1_sv_escaped_wildcard_glob_and_tcl_list_splitting() {
    assert_eq!(split_tcl_list(r"\u:esc :*"), vec![r"\u:esc :*"]);
    assert_eq!(split_tcl_list(r"\u/esc .sig"), vec![r"\u/esc .sig"]);
    assert_eq!(
        split_tcl_list(r"{a {b c}} f\ g \a b"),
        vec!["a {b c}", "f g", "a", "b"]
    );

    let scopes = &["\\u:esc ", "\\u.esc ", "\\u/esc "];
    let insts = &["\\u:esc /reg0", "\\u.esc /reg1", "\\u/esc /sig"];
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

            set dbl_colon [find_objects . -object_type inst -pattern {{\u:esc :*}}]
            if {$dbl_colon ne {{\u:esc /reg0}}} {
                error "expected {{\\u:esc /reg0}} for double-braced wildcard, got: $dbl_colon"
            }
            set sgl_colon [find_objects . -object_type inst -pattern {\u:esc :*}]
            if {$sgl_colon ne {{\u:esc /reg0}}} {
                error "expected {{\\u:esc /reg0}} for single-braced wildcard, got: $sgl_colon"
            }

            create_power_domain PD_DBL -elements {{\u:esc :*}} -supply {primary SS}
            set_isolation iso_dbl -domain PD_DBL -elements {{\u:esc :*}} -isolation_supply_set SS -clamp_value 0

            ::cpf::set_hierarchy_separator .
            set sgl_dot [find_objects . -object_type inst -pattern {\u.esc .*}]
            if {$sgl_dot ne {{\u.esc /reg1}}} {
                error "expected {{\\u.esc /reg1}} for single-braced dot wildcard, got: $sgl_dot"
            }
            create_power_domain PD_DOT -elements {\u.esc .*} -supply {primary SS}

            ::cpf::set_hierarchy_separator /
            set hsc_res [lindex [find_objects {\u/esc } -pattern * -object_type inst -hsc .] 0]
            create_power_domain PD_HSC_RES -elements $hsc_res -supply {primary SS}
        "#;
        let res = parse_upf_and_resolve(upf, None, None, finder).unwrap();
        assert!(
            !res.intent
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagSeverity::Error),
            "unexpected error diagnostics: {:?}",
            res.intent.diagnostics
        );
        let pd_dbl = res.domain_by_name["PD_DBL"];
        assert_eq!(
            res.domain_instances[pd_dbl.0 as usize],
            BTreeSet::from(["\\u:esc /reg0".to_string()])
        );
        assert!(matches!(
            res.match_isolation(Some(pd_dbl), None, "\\u:esc /reg0", AppliesTo::Outputs),
            StrategyMatch::Matched(iso) if iso.name == "iso_dbl"
        ));
        assert!(matches!(
            res.match_isolation(Some(pd_dbl), None, "\\u.esc /reg1", AppliesTo::Outputs),
            StrategyMatch::None
        ));
        let pd_dot = res.domain_by_name["PD_DOT"];
        assert_eq!(
            res.domain_instances[pd_dot.0 as usize],
            BTreeSet::from(["\\u.esc /reg1".to_string()])
        );
        let pd_hsc = res.domain_by_name["PD_HSC_RES"];
        assert_eq!(
            res.domain_instances[pd_hsc.0 as usize],
            BTreeSet::from(["\\u/esc /sig".to_string()])
        );
    }
}
