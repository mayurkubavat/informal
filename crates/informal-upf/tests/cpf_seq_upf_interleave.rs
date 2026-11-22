mod common;

use std::sync::Arc;

use common::{temp_dir, MockDesignFinder};
use informal_upf::{
    load_upf, load_upf_files, parse_cpf_and_resolve, resolve, PstSat, SimState, UpfFileSpec,
    VoltageSpec, DEFAULT_VOLTAGE_TOL_UV,
};

fn default_finder() -> Arc<dyn informal_upf::ObjectFinder> {
    Arc::new(MockDesignFinder::new(
        &["u_wrap", "u_wrap/u_sub"],
        &["u_wrap", "u_wrap/u_sub"],
    ))
}

#[test]
fn finding1_and_fb2_intervening_upf_preserves_cpf_state_and_strips_fallback() {
    let dir = temp_dir("f1_fb2");
    let finder = default_finder();

    // 1. Finding 1: [u0.upf(m1.cpf), u1.upf, m2.cpf(update_power_switch_rule !pwr_en)]
    let u0 = dir.join("u0.upf");
    let m1 = dir.join("m1.cpf");
    let u1 = dir.join("u1.upf");
    let m2 = dir.join("m2.cpf");
    std::fs::write(
        &m1,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_WRAP -instances {u_wrap}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_switch_rule -name PSW_WRAP -domain PD_WRAP -external_power_net VDD_AON -enable_condition {pwr_en}
        end_design
    "#,
    )
    .unwrap();
    std::fs::write(
        &u0,
        format!(
            "upf_version 2.1\nload_upf {} -format cpf -scope /\n",
            m1.display()
        ),
    )
    .unwrap();
    std::fs::write(&u1, "upf_version 2.1\n").unwrap();
    std::fs::write(
        &m2,
        "set_cpf_version 2.0\nupdate_power_switch_rule -name /PSW_WRAP -enable_condition {!pwr_en}\nend_design\n",
    )
    .unwrap();

    let mut intent_f1 = load_upf_files(
        &[
            UpfFileSpec {
                path: u0,
                scope: None,
                version: None,
                is_cpf: false,
            },
            UpfFileSpec {
                path: u1.clone(),
                scope: Some("/".into()),
                version: None,
                is_cpf: false,
            },
            UpfFileSpec {
                path: m2.clone(),
                scope: Some("/".into()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_f1 = resolve(&mut intent_f1, finder.as_ref());
    let wrap_on = res_f1
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_PD_WRAP" && s.state_name == "ON")
        .expect("SS_PD_WRAP ON");
    assert_eq!(wrap_on.logic_expr.as_deref(), Some("!pwr_en"));

    // 2. `fb2`: [CPF1(fallback), UPF(loads CPF2)] -> strips fallback, inherits NC_ON -> SAT.
    let c1 = dir.join("c1.cpf");
    let c2 = dir.join("c2.cpf");
    let u_load_c2 = dir.join("u_load_c2.upf");
    std::fs::write(
        &c1,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        end_design
    "#,
    )
    .unwrap();
    std::fs::write(
        &c2,
        "set_cpf_version 2.0\ncreate_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -default\nend_design\n",
    )
    .unwrap();
    std::fs::write(
        &u_load_c2,
        format!("upf_version 2.1\nload_upf {} -format cpf\n", c2.display()),
    )
    .unwrap();
    let mut intent_fb2 = load_upf_files(
        &[
            UpfFileSpec {
                path: c1.clone(),
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: u_load_c2,
                scope: Some("/".into()),
                version: None,
                is_cpf: false,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_fb2 = resolve(&mut intent_fb2, finder.as_ref());
    let wrap_names: Vec<&str> = res_fb2
        .intent
        .power_states
        .iter()
        .filter(|s| s.target == "SS_PD_WRAP")
        .map(|s| s.state_name.as_str())
        .collect();
    assert_eq!(wrap_names, vec!["NC_ON"]);
    assert!(PstSat::build(&res_fb2, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 3. [CPF1(fallback), UPF(add_power_state)] -> strips fallback ON/OFF, keeps UPF states;
    //    also verify `-update` updates fallback ON instead of stripping it.
    let u_add = dir.join("u_add.upf");
    std::fs::write(
        &u_add,
        r#"
        upf_version 2.1
        add_power_state SS_PD_WRAP -state {RUN -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {pwr_en} -simstate NORMAL}
        add_power_state SS_PD_WRAP -state {SLP -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {!pwr_en} -simstate CORRUPT}
    "#,
    )
    .unwrap();
    let mut intent_add = load_upf_files(
        &[
            UpfFileSpec {
                path: c1.clone(),
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: u_add,
                scope: Some("/".into()),
                version: None,
                is_cpf: false,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_add = resolve(&mut intent_add, finder.as_ref());
    let add_names: Vec<&str> = res_add
        .intent
        .power_states
        .iter()
        .filter(|s| s.target == "SS_PD_WRAP")
        .map(|s| s.state_name.as_str())
        .collect();
    assert_eq!(add_names, vec!["RUN", "SLP"]);

    let u_upd = dir.join("u_upd.upf");
    std::fs::write(
        &u_upd,
        "upf_version 2.1\nadd_power_state SS_PD_WRAP -state {ON -update -logic_expr {pwr_en && other_aon}}\n",
    )
    .unwrap();
    let top_with_c1_and_upd = dir.join("top_upd.upf");
    std::fs::write(
        &top_with_c1_and_upd,
        format!(
            "upf_version 2.1\nload_upf {} -format cpf\nload_upf {}\n",
            c1.display(),
            u_upd.display()
        ),
    )
    .unwrap();
    let res_upd = load_upf(&top_with_c1_and_upd, None, None, finder.clone()).unwrap();
    let wrap_ssid = res_upd.ss_by_name["SS_PD_WRAP"].0 as usize;
    let upd_names: Vec<&str> = res_upd.ss_states[wrap_ssid]
        .iter()
        .map(|s| s.state_name.as_str())
        .collect();
    assert_eq!(upd_names, vec!["ON", "OFF"]);
    let upd_on = res_upd.ss_states[wrap_ssid]
        .iter()
        .find(|s| s.state_name == "ON")
        .unwrap();
    assert_eq!(upd_on.logic_expr.as_deref(), Some("pwr_en && other_aon"));

    // 4. Empty-mode CPF (only nom_v.is_on_known() fallback) + trailing UPF still emits fallback ON.
    let c_nom = dir.join("c_nom.cpf");
    std::fs::write(
        &c_nom,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_TOP -default
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        end_design
    "#,
    )
    .unwrap();
    let mut intent_tail = load_upf_files(
        &[
            UpfFileSpec {
                path: c_nom,
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: u1,
                scope: Some("/".into()),
                version: None,
                is_cpf: false,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_tail = resolve(&mut intent_tail, finder.as_ref());
    assert!(
        res_tail
            .intent
            .power_states
            .iter()
            .any(|s| s.target == "SS_PD_TOP" && s.state_name == "ON"),
        "trailing UPF must still trigger deferred nom_v fallback ON: {:#?}",
        res_tail.intent.power_states
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn finding2_and_d7_sequential_upf_nested_cpf_and_parent_late_nc_override() {
    let dir = temp_dir("f2_d7");
    let finder = default_finder();

    // Finding 2: [top.upf, sub1.upf(m1.cpf), sub2.upf(m2.cpf)] retains m1.cpf state on pop.
    let top_upf = dir.join("top.upf");
    let sub1_upf = dir.join("sub1.upf");
    let sub2_upf = dir.join("sub2.upf");
    let m1_cpf = dir.join("m1.cpf");
    let m2_cpf = dir.join("m2.cpf");
    std::fs::write(&top_upf, "upf_version 2.1\n").unwrap();
    std::fs::write(
        &m1_cpf,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -default
        end_design
    "#,
    )
    .unwrap();
    std::fs::write(
        &m2_cpf,
        "set_cpf_version 2.0\ncreate_power_mode -name PM_OFF -domain_conditions {/PD_TOP@NC_ON /PD_WRAP@NC_OFF}\nend_design\n",
    )
    .unwrap();
    std::fs::write(
        &sub1_upf,
        format!(
            "upf_version 2.1\nload_upf {} -format cpf -scope /\n",
            m1_cpf.display()
        ),
    )
    .unwrap();
    std::fs::write(
        &sub2_upf,
        format!(
            "upf_version 2.1\nload_upf {} -format cpf -scope /\n",
            m2_cpf.display()
        ),
    )
    .unwrap();

    let mut intent_f2 = load_upf_files(
        &[
            UpfFileSpec {
                path: top_upf,
                scope: None,
                version: None,
                is_cpf: false,
            },
            UpfFileSpec {
                path: sub1_upf,
                scope: Some("/".into()),
                version: None,
                is_cpf: false,
            },
            UpfFileSpec {
                path: sub2_upf,
                scope: Some("/".into()),
                version: None,
                is_cpf: false,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_f2 = resolve(&mut intent_f2, finder.as_ref());
    let mut sat_f2 = PstSat::build(&res_f2, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_f2.is_state_space_consistent());
    assert!(!sat_f2
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "PST_UNREACHABLE_STATE"));
    let ss_top = res_f2.ss_by_name["SS_PD_TOP"];
    let ss_wrap = res_f2.ss_by_name["SS_PD_WRAP"];
    assert!(sat_f2.can_be_off_while_on(ss_wrap, ss_top).is_some());

    // `d7`: parent CPF `set_design top` -> nested `load_upf sub.cpf -format cpf` -> parent late NC override -> PST_INCONSISTENT.
    let sub_cpf = dir.join("sub_d7.cpf");
    std::fs::write(
        &sub_cpf,
        r#"
        set_cpf_version 2.0
        create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
        update_power_domain -name PD_WRAP -primary_power_net /VDD_WRAP -primary_ground_net /VSS
        create_power_mode -name PM_SUB -domain_conditions {PD_WRAP@NC_ON} -condition {pwr_en} -default
        end_design
    "#,
    )
    .unwrap();
    let top_d7 = format!(
        r#"
        set_cpf_version 2.0
        set_design top
        create_power_nets -nets {{VDD_AON VDD_WRAP}} -voltage 0.9
        create_ground_nets -nets {{VSS}}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_TOP -default
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        load_upf "{}" -format cpf
        create_nominal_condition -name NC_ON -voltage 0.0 -state off
        end_design top
    "#,
        sub_cpf.display()
    );
    let res_d7 = parse_cpf_and_resolve(&top_d7, None, None, finder).unwrap();
    assert!(!PstSat::build(&res_d7, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scoped_nominal_conditions_nc7_b8_fb3_and_forward_ref_nc6() {
    let dir = temp_dir("nc_scoped_fwd");
    let finder = default_finder();

    // 1. `nc7` + `B8`: `-scope u_wrap(/u_sub)` NC_ON=0.0V/off does not mutate root `("", "NC_ON") = 0.9V/on`.
    let c1 = dir.join("c1.cpf");
    let c2 = dir.join("c2.cpf");
    let c3 = dir.join("c3.cpf");
    std::fs::write(
        &c1,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_WRAP VDD_SUB} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_TOP -default
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        create_power_mode -name PM_TOP1 -domain_conditions {PD_TOP@NC_ON} -condition {pwr_en} -default
        end_design
    "#,
    )
    .unwrap();
    std::fs::write(
        &c2,
        r#"
        set_cpf_version 2.0
        create_nominal_condition -name NC_ON -voltage 0.0 -state off
        create_power_domain -name PD_SUB -instances {.} -shutoff_condition {!wrap_bad_ctrl}
        update_power_domain -name PD_SUB -primary_power_net /VDD_SUB -primary_ground_net /VSS
        create_power_mode -name PM_SUB -domain_conditions {PD_SUB@NC_ON} -condition {!wrap_bad_ctrl} -default
        end_design
    "#,
    )
    .unwrap();
    std::fs::write(
        &c3,
        r#"
        set_cpf_version 2.0
        create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_mode -name PM_TOP2 -domain_conditions {PD_WRAP@NC_ON} -condition {pwr_en} -default
        end_design
    "#,
    )
    .unwrap();
    let mut intent_nc7 = load_upf_files(
        &[
            UpfFileSpec {
                path: c1,
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: c2,
                scope: Some("u_wrap/u_sub".into()),
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: c3,
                scope: Some("/".into()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_nc7 = resolve(&mut intent_nc7, finder.as_ref());
    let wrap_st = res_nc7
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_PD_WRAP" && s.state_name == "NC_ON")
        .unwrap();
    assert_eq!(wrap_st.simstate, Some(SimState::Normal));
    assert_eq!(
        wrap_st.rail_states[0].voltage,
        VoltageSpec::Nominal(900_000)
    );
    assert!(PstSat::build(&res_nc7, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 2. `fb3`: same-scope override inside `-scope u_wrap/u_sub` refreshes PM_SUB -> PST_INCONSISTENT.
    let fb_root = dir.join("fb_root.cpf");
    let fb_s1 = dir.join("fb_s1.cpf");
    let fb_s2 = dir.join("fb_s2.cpf");
    std::fs::write(
        &fb_root,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_TOP -default
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        end_design
    "#,
    )
    .unwrap();
    std::fs::write(
        &fb_s1,
        r#"
        set_cpf_version 2.0
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_SUB -instances {.} -shutoff_condition {!wrap_bad_ctrl}
        update_power_domain -name PD_SUB -primary_power_net /VDD_SUB -primary_ground_net /VSS
        create_power_mode -name PM_SUB -domain_conditions {PD_SUB@NC_ON} -condition {wrap_bad_ctrl} -default
        end_design
    "#,
    )
    .unwrap();
    std::fs::write(
        &fb_s2,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_ON -voltage 0.0 -state off\nend_design\n",
    )
    .unwrap();
    let mut intent_fb3 = load_upf_files(
        &[
            UpfFileSpec {
                path: fb_root,
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: fb_s1,
                scope: Some("u_wrap/u_sub".into()),
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: fb_s2,
                scope: Some("u_wrap/u_sub".into()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_fb3 = resolve(&mut intent_fb3, finder.as_ref());
    assert!(!PstSat::build(&res_fb3, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 3. `nc6`: forward-referenced NC_LATE defined in a later file refreshes PM_ON -> PST_INCONSISTENT,
    //    while a scoped NC_LATE at `u_wrap/u_sub` does NOT spuriously refresh root PM_ON.
    let fwd1 = dir.join("fwd1.cpf");
    let fwd2_root = dir.join("fwd2_root.cpf");
    let fwd2_sub = dir.join("fwd2_sub.cpf");
    std::fs::write(
        &fwd1,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_TOP -default
        create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_LATE} -condition {pwr_en} -default
        end_design
    "#,
    )
    .unwrap();
    std::fs::write(
        &fwd2_root,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_LATE -voltage 0.0 -state off\nend_design\n",
    )
    .unwrap();
    std::fs::write(
        &fwd2_sub,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_LATE -voltage 0.0 -state off\nend_design\n",
    )
    .unwrap();

    let mut intent_nc6 = load_upf_files(
        &[
            UpfFileSpec {
                path: fwd1.clone(),
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: fwd2_root,
                scope: Some("/".into()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_nc6 = resolve(&mut intent_nc6, finder.as_ref());
    assert!(!PstSat::build(&res_nc6, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    let mut intent_nc6_iso = load_upf_files(
        &[
            UpfFileSpec {
                path: fwd1,
                scope: None,
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: fwd2_sub,
                scope: Some("u_wrap/u_sub".into()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_nc6_iso = resolve(&mut intent_nc6_iso, finder.as_ref());
    assert!(PstSat::build(&res_nc6_iso, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bug_a5_nc8_nc11_illegal_mode_isolation_and_a4_nc9_design_top_isolation() {
    let dir = temp_dir("a5_a4_nc8_9_11");
    let finder = default_finder();

    // 1. `A5` / `nc8`: two `set_design` blocks defining `-illegal` mode `PM_ILL` (domain_conditions + global).
    let a5_dom = r#"
        set_cpf_version 2.0
        set_design blk_a
        create_power_nets -nets {VDD_A VDD_B} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_A -instances {u_wrap/u_sub} -shutoff_condition {!en_a}
        update_power_domain -name PD_A -primary_power_net VDD_A -primary_ground_net VSS
        create_power_mode -name PM_A -domain_conditions {PD_A@NC_ON} -condition {en_a} -default
        create_power_mode -name PM_ILL -domain_conditions {PD_A@NC_ON} -condition {en_a} -illegal
        end_design blk_a

        set_design top
        create_power_domain -name PD_B -instances {u_wrap} -shutoff_condition {!en_b}
        update_power_domain -name PD_B -primary_power_net VDD_B -primary_ground_net VSS
        create_power_mode -name PM_B -domain_conditions {PD_B@NC_ON} -condition {en_b} -default
        create_power_mode -name PM_ILL -domain_conditions {PD_B@NC_ON} -condition {!en_b} -illegal
        end_design top
    "#;
    let res_a5 = parse_cpf_and_resolve(a5_dom, None, None, finder.clone()).unwrap();
    assert_eq!(res_a5.intent.illegal_combinations.len(), 2);
    assert!(!PstSat::build(&res_a5, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    let a5_global = r#"
        set_cpf_version 2.0
        set_design blk_a
        create_power_nets -nets {VDD_A VDD_B} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_A -instances {u_wrap/u_sub} -shutoff_condition {!en_a}
        update_power_domain -name PD_A -primary_power_net VDD_A -primary_ground_net VSS
        create_power_mode -name PM_A -domain_conditions {PD_A@NC_ON} -condition {en_a} -default
        create_power_mode -name PM_ILL -condition {en_a} -illegal
        end_design blk_a

        set_design top
        create_power_domain -name PD_B -instances {u_wrap} -shutoff_condition {!en_b}
        update_power_domain -name PD_B -primary_power_net VDD_B -primary_ground_net VSS
        create_power_mode -name PM_B -domain_conditions {PD_B@NC_ON} -condition {en_b} -default
        create_power_mode -name PM_ILL -condition {!en_b} -illegal
        end_design top
    "#;
    let res_a5_glob = parse_cpf_and_resolve(a5_global, None, None, finder.clone()).unwrap();
    assert!(!PstSat::build(&res_a5_glob, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 2. `nc11`: two distinct `-scope`s defining `-illegal` mode `PM_ILL`.
    let nc11_a = dir.join("nc11_a.cpf");
    let nc11_b = dir.join("nc11_b.cpf");
    std::fs::write(
        &nc11_a,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_W} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_W -instances {.} -shutoff_condition {!bad_shutoff}
        update_power_domain -name PD_W -primary_power_net VDD_W -primary_ground_net VSS
        create_power_mode -name PM_W -domain_conditions {PD_W@NC_ON} -condition {bad_shutoff} -default
        create_power_mode -name PM_ILL -domain_conditions {PD_W@NC_ON} -condition {bad_shutoff} -illegal
        end_design
    "#,
    )
    .unwrap();
    std::fs::write(
        &nc11_b,
        r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_S} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_S -instances {.} -shutoff_condition {!wrap_bad_ctrl}
        update_power_domain -name PD_S -primary_power_net VDD_S -primary_ground_net VSS
        create_power_mode -name PM_S -domain_conditions {PD_S@NC_ON} -condition {wrap_bad_ctrl} -default
        create_power_mode -name PM_ILL -domain_conditions {PD_S@NC_ON} -condition {!wrap_bad_ctrl} -illegal
        end_design
    "#,
    )
    .unwrap();
    let mut intent_nc11 = load_upf_files(
        &[
            UpfFileSpec {
                path: nc11_a,
                scope: Some("u_wrap".into()),
                version: None,
                is_cpf: true,
            },
            UpfFileSpec {
                path: nc11_b,
                scope: Some("u_wrap/u_sub".into()),
                version: None,
                is_cpf: true,
            },
        ],
        finder.clone(),
    )
    .unwrap();
    let res_nc11 = resolve(&mut intent_nc11, finder.as_ref());
    assert_eq!(res_nc11.intent.illegal_combinations.len(), 2);
    assert!(!PstSat::build(&res_nc11, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 3. `update_power_mode -illegal false` flips `-illegal` mode back to legal and removes stale illegal entry.
    let flip_legal = r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_A} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_A -default -shutoff_condition {!en_a}
        update_power_domain -name PD_A -primary_power_net VDD_A -primary_ground_net VSS
        create_power_mode -name PM_ILL -domain_conditions {PD_A@NC_ON} -condition {en_a} -illegal
        end_design
        update_power_mode -name PM_ILL -illegal false
        end_design
    "#;
    let res_flip = parse_cpf_and_resolve(flip_legal, None, None, finder.clone()).unwrap();
    assert!(res_flip.intent.illegal_combinations.is_empty());
    assert!(PstSat::build(&res_flip, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 4. `A4` / `A4b` / `nc9`: mixing `design_top == None` and `Some("blk_a")` (both orders) keeps separate PST tables.
    let a4_none_then_set = r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_A VDD_B} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_A -instances {u_wrap/u_sub}
        update_power_domain -name PD_A -primary_power_net VDD_A -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {PD_A@NC_ON} -condition {en_a} -default

        set_design blk_a
        create_power_domain -name PD_B -instances {u_wrap}
        update_power_domain -name PD_B -primary_power_net VDD_B -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {PD_A@NC_ON PD_B@NC_ON} -condition {!en_a} -default
        end_design blk_a
    "#;
    let res_a4 = parse_cpf_and_resolve(a4_none_then_set, None, None, finder.clone()).unwrap();
    assert_eq!(res_a4.intent.pst_tables.len(), 2);
    assert!(!PstSat::build(&res_a4, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    let a4b_set_then_none = r#"
        set_cpf_version 2.0
        set_design blk_a
        create_power_nets -nets {VDD_A} -voltage 0.9
        create_ground_nets -nets {VSS}
        create_nominal_condition -name NC_ON -voltage 0.9 -state on
        create_power_domain -name PD_A -instances {u_wrap/u_sub}
        update_power_domain -name PD_A -primary_power_net VDD_A -primary_ground_net VSS
        create_power_mode -name PM_ON -domain_conditions {PD_A@NC_ON} -condition {en_a} -default
        end_design blk_a

        create_power_mode -name PM_ON -domain_conditions {PD_A@NC_ON} -condition {!en_a} -default
        end_design
    "#;
    let res_a4b = parse_cpf_and_resolve(a4b_set_then_none, None, None, finder.clone()).unwrap();
    assert_eq!(res_a4b.intent.pst_tables.len(), 2);
    assert!(!PstSat::build(&res_a4b, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 5. D13 strict rejection of `update_power_domain -shutoff_condition`.
    let d13_upd = r#"
        set_cpf_version 2.0
        create_power_domain -name PD_TOP -default
        update_power_domain -name PD_TOP -shutoff_condition {!pwr_en}
    "#;
    let err = parse_cpf_and_resolve(d13_upd, None, None, finder).unwrap_err();
    assert!(err
        .to_string()
        .contains("unsupported option -shutoff_condition"));

    let _ = std::fs::remove_dir_all(&dir);
}
