mod common;

use std::sync::Arc;
use std::time::Instant;

use common::temp_dir;
use informal_upf::{
    glob, load_upf, parse_upf_and_resolve, split_tcl_list, LsRequirement, NullObjectFinder, PstSat,
    UpfError, VoltageSpec, DEFAULT_VOLTAGE_TOL_UV,
};

#[test]
fn voltage_spec_exact_microvolt_parsing_and_tolerance() {
    assert_eq!(
        VoltageSpec::parse("0.9000").unwrap(),
        VoltageSpec::Nominal(900_000)
    );
    assert_eq!(
        VoltageSpec::parse("0.9005").unwrap(),
        VoltageSpec::Nominal(900_500)
    );
    assert_eq!(
        VoltageSpec::parse("0.9020").unwrap(),
        VoltageSpec::Nominal(902_000)
    );
    assert_eq!(
        VoltageSpec::parse("900mV").unwrap(),
        VoltageSpec::Nominal(900_000)
    );
    assert_eq!(
        VoltageSpec::parse("900000uV").unwrap(),
        VoltageSpec::Nominal(900_000)
    );
    assert_eq!(
        VoltageSpec::parse("0.9V").unwrap(),
        VoltageSpec::Nominal(900_000)
    );
    assert_eq!(
        VoltageSpec::parse("{FULL_ON 0.90}").unwrap(),
        VoltageSpec::Nominal(900_000)
    );
    assert_eq!(
        VoltageSpec::parse("{PARTIAL_ON 0.7 0.8}").unwrap(),
        VoltageSpec::Range(700_000, 800_000)
    );
    assert_eq!(
        VoltageSpec::parse("{0.8, 1.0}").unwrap(),
        VoltageSpec::Range(800_000, 1_000_000)
    );
    assert_eq!(
        VoltageSpec::parse("0.8:1.0").unwrap(),
        VoltageSpec::Range(800_000, 1_000_000)
    );
    assert_eq!(VoltageSpec::parse("OFF").unwrap(), VoltageSpec::Off);
    assert_eq!(
        VoltageSpec::parse("UNDETERMINED").unwrap(),
        VoltageSpec::Undetermined
    );
    assert_eq!(
        VoltageSpec::parse("0.1234567").unwrap(),
        VoltageSpec::Nominal(123_457)
    );

    // Within 1000 uV tolerance (0.9000 vs 0.9005 = 500 uV): no LS needed.
    let within_tol = r#"
        upf_version 2.1
        create_supply_net VDD_A
        create_supply_net VDD_B
        create_supply_net VSS
        create_supply_set SS_A -function {power VDD_A} -function {ground VSS}
        create_supply_set SS_B -function {power VDD_B} -function {ground VSS}
        add_power_state SS_A -state {ON -supply_expr {power == {FULL_ON 0.9000} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_B -state {ON -supply_expr {power == {FULL_ON 0.9005} && ground == {FULL_ON 0.0}} -simstate NORMAL}
    "#;
    let res_within =
        parse_upf_and_resolve(within_tol, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_within = PstSat::build(&res_within, DEFAULT_VOLTAGE_TOL_UV);
    let ss_a = res_within.ss_by_name["SS_A"];
    let ss_b = res_within.ss_by_name["SS_B"];
    assert_eq!(
        sat_within.voltage_relation(ss_a, ss_b, DEFAULT_VOLTAGE_TOL_UV),
        LsRequirement::None
    );

    // Over 1000 uV tolerance (0.9000 vs 0.9020 = 2000 uV): LowToHigh LS needed.
    let over_tol = within_tol.replace("0.9005", "0.9020");
    let res_over =
        parse_upf_and_resolve(&over_tol, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_over = PstSat::build(&res_over, DEFAULT_VOLTAGE_TOL_UV);
    assert!(matches!(
        sat_over.voltage_relation(
            res_over.ss_by_name["SS_A"],
            res_over.ss_by_name["SS_B"],
            DEFAULT_VOLTAGE_TOL_UV
        ),
        LsRequirement::LowToHigh { .. }
    ));
}

#[test]
fn supply_cycle_and_list_glob_helpers() {
    let cycle_upf = r#"
        upf_version 2.1
        create_supply_net VDD_A
        create_supply_net VDD_B
        create_supply_net VSS
        create_power_domain PD_TOP -include_scope
        create_power_switch sw1 -domain PD_TOP -input_supply_port {in VDD_A} -output_supply_port {out VDD_B} -control_port {c en1} -on_state {ON in {en1}} -off_state {OFF {!en1}}
        create_power_switch sw2 -domain PD_TOP -input_supply_port {in VDD_B} -output_supply_port {out VDD_A} -control_port {c en2} -on_state {ON in {en2}} -off_state {OFF {!en2}}
    "#;
    let res = parse_upf_and_resolve(cycle_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    assert!(res
        .intent
        .diagnostics
        .iter()
        .any(|d| d.rule_id == "UPF_SUPPLY_CYCLE"));

    assert!(glob("u_core/reg_[0-9]*", "u_core/reg_3_q"));
    assert!(!glob("u_core/reg_[0-9]*", "u_core/reg_x"));
    assert_eq!(
        split_tcl_list(r#"{a {b c}} "d e" f\ g"#),
        vec!["a {b c}", "d e", "f g"]
    );
}

#[test]
fn regression_ir_voltage_commas_braces_and_linear_glob() {
    assert_eq!(
        VoltageSpec::parse("{PARTIAL_ON 0.7, 0.85}").unwrap(),
        VoltageSpec::Range(700_000, 850_000)
    );
    assert_eq!(
        VoltageSpec::parse("{FULL_ON {0.8 1.0}}").unwrap(),
        VoltageSpec::Range(800_000, 1_000_000)
    );
    assert_eq!(
        VoltageSpec::parse("{FULL_ON {0.8, 1.0}}").unwrap(),
        VoltageSpec::Range(800_000, 1_000_000)
    );
    assert_eq!(
        VoltageSpec::parse("{PARTIAL_ON 0.7:0.85}").unwrap(),
        VoltageSpec::Range(700_000, 850_000)
    );

    // Pathological 24-star pattern must finish instantaneously without exponential backtracking.
    let pat = "a*".repeat(24) + "b";
    let text = "a".repeat(32) + "c";
    let t0 = Instant::now();
    assert!(!glob(&pat, &text));
    assert!(
        t0.elapsed().as_millis() < 50,
        "glob took too long: {:?}",
        t0.elapsed()
    );
    assert!(glob("a*b*c", "axxxbyyybzzzc"));
    assert!(glob("a\\", "a\\"));
}

#[test]
fn upf10_add_port_state_on_supply_port_and_create_pst() {
    let upf_ports = r#"
        upf_version 1.0
        create_supply_port VDD_AON_PORT -direction in
        create_supply_port VDD_SW_PORT -direction in
        create_supply_port VSS_PORT -direction in
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        connect_supply_net VDD_AON -ports VDD_AON_PORT
        connect_supply_net VDD_SW -ports VDD_SW_PORT
        connect_supply_net VSS -ports VSS_PORT
        create_power_domain PD_AON -include_scope
        create_power_domain PD_SW -elements {u_sw}
        set_domain_supply_net PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
        set_domain_supply_net PD_SW  -primary_power_net VDD_SW  -primary_ground_net VSS
        add_port_state VDD_AON_PORT -state {HV 1.1}
        add_port_state VDD_SW_PORT  -state {LV 0.9} -state {OFF off}
        add_port_state VSS_PORT     -state {GND 0.0}
        create_pst my_pst -supplies {VDD_AON_PORT VDD_SW_PORT VSS_PORT}
        add_pst_state S_ALL_ON -pst my_pst -state {HV LV GND}
        add_pst_state S_SLEEP  -pst my_pst -state {HV OFF GND}
    "#;
    let res_ports =
        parse_upf_and_resolve(upf_ports, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_ports = PstSat::build(&res_ports, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_ports.is_state_space_consistent());
    assert!(
        !sat_ports
            .diagnostics()
            .iter()
            .any(|d| d.rule_id == "PST_MISSING_PORT_STATE"),
        "unexpected PST_MISSING_PORT_STATE: {:?}",
        sat_ports.diagnostics()
    );
    let ss_aon = res_ports
        .primary_ss(res_ports.domain_by_name["PD_AON"])
        .unwrap();
    let ss_sw = res_ports
        .primary_ss(res_ports.domain_by_name["PD_SW"])
        .unwrap();
    assert!(sat_ports.can_be_off_while_on(ss_sw, ss_aon).is_some());
    assert!(sat_ports.can_be_off_while_on(ss_aon, ss_sw).is_none());
}

#[test]
fn regression_r2_upf10_add_port_state_on_switch_port_and_create_pst() {
    let upf = r#"
        upf_version 1.0
        create_supply_port VDD_IN_P -direction in
        create_supply_port VSS_P -direction in
        create_supply_net VDD_IN
        create_supply_net VDD_SW
        create_supply_net VSS
        connect_supply_net VDD_IN -ports VDD_IN_P
        connect_supply_net VSS -ports VSS_P
        create_power_domain PD_TOP -include_scope
        create_power_domain PD_SW -elements {u_sw}
        set_domain_supply_net PD_TOP -primary_power_net VDD_IN -primary_ground_net VSS
        set_domain_supply_net PD_SW -primary_power_net VDD_SW -primary_ground_net VSS
        create_power_switch sw -domain PD_SW \
            -input_supply_port {in VDD_IN} \
            -output_supply_port {out VDD_SW} \
            -control_port {en sw_en} \
            -on_state {ON in {en}} \
            -off_state {OFF {!en}}
        add_port_state VDD_IN_P -state {ON 0.9}
        add_port_state sw/out -state {ON 0.9} -state {OFF off}
        add_port_state VSS_P -state {GND 0.0}
        create_pst my_pst -supplies {VDD_IN_P sw/out VSS_P}
        add_pst_state S_ON  -pst my_pst -state {ON ON  GND}
        add_pst_state S_OFF -pst my_pst -state {ON OFF GND}
    "#;
    let res = parse_upf_and_resolve(upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat = PstSat::build(&res, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat.is_state_space_consistent());
    assert!(
        !sat.diagnostics()
            .iter()
            .any(|d| d.rule_id == "PST_MISSING_PORT_STATE"),
        "unexpected PST_MISSING_PORT_STATE: {:?}",
        sat.diagnostics()
    );
    let pd_top_ss = res.primary_ss(res.domain_by_name["PD_TOP"]).unwrap();
    let pd_sw_ss = res.primary_ss(res.domain_by_name["PD_SW"]).unwrap();
    let witness = sat.can_be_off_while_on(pd_sw_ss, pd_top_ss);
    assert!(witness.is_some());
    assert_eq!(
        witness
            .unwrap()
            .supply_states
            .get("PD_SW.primary")
            .map(String::as_str),
        Some("OFF_GND")
    );
    assert!(sat.can_be_off_while_on(pd_top_ss, pd_sw_ss).is_none());
    assert_eq!(
        sat.voltage_relation(pd_top_ss, pd_sw_ss, DEFAULT_VOLTAGE_TOL_UV),
        LsRequirement::None
    );

    // Also verify scoped switch port inside load_upf -scope u_sub.
    let dir = temp_dir("sw_port_scoped");
    let sub_path = dir.join("sub.upf");
    let top_path = dir.join("top.upf");
    std::fs::write(
        &sub_path,
        r#"
        upf_version 1.0
        create_supply_port VDD_IN_P -direction in
        create_supply_port VSS_P -direction in
        create_supply_net VDD_IN
        create_supply_net VDD_SW
        create_supply_net VSS
        connect_supply_net VDD_IN -ports VDD_IN_P
        connect_supply_net VSS -ports VSS_P
        create_power_domain PD_SUB -include_scope
        set_domain_supply_net PD_SUB -primary_power_net VDD_SW -primary_ground_net VSS
        create_power_switch sw_sub -domain PD_SUB \
            -input_supply_port {in VDD_IN} \
            -output_supply_port {out VDD_SW} \
            -control_port {en sw_en} \
            -on_state {ON in {en}} \
            -off_state {OFF {!en}}
        add_port_state VDD_IN_P -state {ON 0.9}
        add_port_state sw_sub/out -state {ON 0.9} -state {OFF off}
        add_port_state VSS_P -state {GND 0.0}
        create_pst sub_pst -supplies {VDD_IN_P sw_sub/out VSS_P}
        add_pst_state S_ON  -pst sub_pst -state {ON ON  GND}
        add_pst_state S_OFF -pst sub_pst -state {ON OFF GND}
    "#,
    )
    .unwrap();
    std::fs::write(
        &top_path,
        format!(
            "upf_version 1.0\nload_upf {} -scope u_sub\n",
            sub_path.display()
        ),
    )
    .unwrap();
    let res_scoped = load_upf(&top_path, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_scoped = PstSat::build(&res_scoped, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_scoped.is_state_space_consistent());
    assert!(
        sat_scoped.diagnostics().is_empty(),
        "{:?}",
        sat_scoped.diagnostics()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn regression_r5_pst_switch_backfill_and_add_pst_state_scope_isolation() {
    // 1-token unconnected switch ports + deduplicated PortStateDef entries.
    let upf_sw = r#"
        upf_version 1.0
        create_supply_port VDD_IN_P -direction in
        create_supply_port VSS_P -direction in
        create_supply_net VDD_IN
        create_supply_net VDD_SW
        create_supply_net VSS
        connect_supply_net VDD_IN -ports VDD_IN_P
        connect_supply_net VSS -ports VSS_P
        create_power_domain PD_TOP -include_scope
        create_power_domain PD_SW -elements {u_sw}
        set_domain_supply_net PD_TOP -primary_power_net VDD_IN -primary_ground_net VSS
        set_domain_supply_net PD_SW -primary_power_net VDD_SW -primary_ground_net VSS
        create_power_switch sw -domain PD_SW \
            -input_supply_port {in} \
            -output_supply_port {out} \
            -control_port {en} \
            -on_state {ON in {en}} \
            -off_state {OFF {!en}}
        connect_supply_net VDD_IN -ports {sw/in}
        connect_supply_net VDD_SW -ports {sw/out}
        add_port_state VDD_IN_P -state {ON 0.9}
        add_port_state sw/in    -state {ON 0.9}
        add_port_state sw/out   -state {ON 0.9} -state {OFF off}
        add_port_state VSS_P    -state {GND 0.0}
        create_pst my_pst -supplies {VDD_IN_P sw/out VSS_P}
        add_pst_state S_ON  -pst my_pst -state {ON ON  GND}
        add_pst_state S_OFF -pst my_pst -state {ON OFF GND}
    "#;
    let res_sw = parse_upf_and_resolve(upf_sw, None, None, Arc::new(NullObjectFinder)).unwrap();
    let vdd_in = &res_sw.intent.supply_nets[res_sw.net_by_name["VDD_IN"].0 as usize];
    assert_eq!(vdd_in.port_states.len(), 1);
    let mut sat_sw = PstSat::build(&res_sw, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_sw.is_state_space_consistent());
    assert!(sat_sw.diagnostics().is_empty());
    assert_eq!(sat_sw.enumerate_symbolic_states(10).len(), 2);

    // Fix 1a: `create_pst -supplies {/VDD_P /VSS_P}` inside `u_sub` under local shadowing.
    let upf_pst = r#"
        upf_version 1.0
        create_supply_port VDD_P
        create_supply_port VSS_P
        create_supply_net VDD_TOP
        create_supply_net VSS_TOP
        connect_supply_net VDD_TOP -ports {VDD_P}
        connect_supply_net VSS_TOP -ports {VSS_P}
        add_port_state VDD_P -state {ON_TOP 0.9} -state {OFF_TOP off}
        add_port_state VSS_P -state {GND_TOP 0.0}
        create_power_domain PD_TOP -include_scope
        set_domain_supply_net PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS_TOP

        set_scope u_sub
        create_supply_port VDD_P
        create_supply_port VSS_P
        create_supply_net VDD_TOP
        create_supply_net VSS_TOP
        connect_supply_net VDD_TOP -ports {VDD_P}
        connect_supply_net VSS_TOP -ports {VSS_P}
        add_port_state VDD_P -state {ON_SUB 1.2}
        add_port_state VSS_P -state {GND_SUB 0.0}

        create_pst pst_sub_to_root -supplies {/VDD_P /VSS_P}
        add_pst_state S_ON  -pst pst_sub_to_root -state {ON_TOP  GND_TOP}
        add_pst_state S_OFF -pst pst_sub_to_root -state {OFF_TOP GND_TOP}
        set_scope /
    "#;
    let res_pst = parse_upf_and_resolve(upf_pst, None, None, Arc::new(NullObjectFinder)).unwrap();
    let pst = res_pst
        .intent
        .pst_tables
        .iter()
        .find(|t| t.name == "u_sub/pst_sub_to_root")
        .unwrap();
    assert_eq!(
        pst.supplies,
        vec!["/VDD_TOP".to_string(), "/VSS_TOP".to_string()]
    );
    let mut sat_pst = PstSat::build(&res_pst, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        sat_pst.is_state_space_consistent(),
        "PST diags: {:?}",
        sat_pst.diagnostics()
    );
    assert!(
        sat_pst.diagnostics().is_empty(),
        "unexpected PST diags: {:?}",
        sat_pst.diagnostics()
    );
    assert_eq!(sat_pst.enumerate_symbolic_states(10).len(), 2);

    // Fix 1b: 1-token switch port backfill connecting to shadowed `/VDD_ROOT` detects cycle.
    let upf_cycle = r#"
        upf_version 1.0
        create_supply_net VDD_ROOT
        create_supply_net VSS
        create_power_domain PD_TOP -include_scope
        set_domain_supply_net PD_TOP -primary_power_net VDD_ROOT -primary_ground_net VSS

        set_scope u_sub
        create_supply_net VDD_ROOT
        create_supply_net VDD_SW
        create_power_domain PD_SUB -include_scope
        set_domain_supply_net PD_SUB -primary_power_net VDD_SW -primary_ground_net /VSS
        create_power_switch sw_sub -domain PD_SUB \
            -input_supply_port {in} \
            -output_supply_port {out VDD_SW} \
            -control_port {en en_sig} \
            -on_state {ON in {en}}
        connect_supply_net /VDD_ROOT -ports {sw_sub/in}
        create_power_switch sw_back -domain PD_SUB \
            -input_supply_port {in VDD_SW} \
            -output_supply_port {out /VDD_ROOT} \
            -control_port {en en_sig2} \
            -on_state {ON in {en}}
        set_scope /
    "#;
    let res_cyc = parse_upf_and_resolve(upf_cycle, None, None, Arc::new(NullObjectFinder)).unwrap();
    let sw_sub = res_cyc
        .intent
        .power_switches
        .iter()
        .find(|s| s.name == "u_sub/sw_sub")
        .unwrap();
    assert_eq!(
        sw_sub.input_supply_ports,
        vec![("in".to_string(), "/VDD_ROOT".to_string())]
    );
    assert!(
        res_cyc
            .intent
            .diagnostics
            .iter()
            .any(|d| d.rule_id == "UPF_SUPPLY_CYCLE"),
        "expected UPF_SUPPLY_CYCLE diagnostic, got {:?}",
        res_cyc.intent.diagnostics
    );

    // Fix 3: UPF `add_pst_state` scope isolation.
    let upf_add_pst = r#"
        upf_version 1.0
        create_supply_port VDD_P
        create_supply_port VSS_P
        add_port_state VDD_P -state {ON 0.9} -state {OFF off}
        add_port_state VSS_P -state {GND 0.0}

        set_scope u_sub
        create_supply_port VDD_S
        create_supply_port VSS_S
        add_port_state VDD_S -state {ON 1.1}
        add_port_state VSS_S -state {GND 0.0}
        create_pst my_pst -supplies {VDD_S VSS_S}
        add_pst_state S_SUB -pst my_pst -state {ON GND}
        set_scope /

        create_pst my_pst -supplies {VDD_P VSS_P}
        add_pst_state S_TOP -pst my_pst -state {ON GND}

        set_scope u_sub
        add_pst_state S_TOP_FROM_SUB -pst /my_pst -state {OFF GND}
        set_scope /
    "#;
    let res_add_pst =
        parse_upf_and_resolve(upf_add_pst, None, None, Arc::new(NullObjectFinder)).unwrap();
    let sub_pst = res_add_pst
        .intent
        .pst_tables
        .iter()
        .find(|t| t.name == "u_sub/my_pst")
        .unwrap();
    let top_pst = res_add_pst
        .intent
        .pst_tables
        .iter()
        .find(|t| t.name == "my_pst")
        .unwrap();
    assert_eq!(
        sub_pst
            .rows
            .iter()
            .map(|r| r.state_name.as_str())
            .collect::<Vec<_>>(),
        vec!["S_SUB"]
    );
    assert_eq!(
        top_pst
            .rows
            .iter()
            .map(|r| r.state_name.as_str())
            .collect::<Vec<_>>(),
        vec!["S_TOP", "S_TOP_FROM_SUB"]
    );
    let bad_pst_err = parse_upf_and_resolve(
        r#"
            upf_version 1.0
            set_scope u_sub
            create_supply_port VDD_S
            create_pst child_only_pst -supplies {VDD_S}
            set_scope /
            add_pst_state S_BAD -pst child_only_pst -state {ON}
        "#,
        None,
        None,
        Arc::new(NullObjectFinder),
    )
    .unwrap_err();
    assert!(matches!(bad_pst_err, UpfError::Tcl { .. }));
}
