mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use common::check_logic;
use informal_upf::{
    parse_logic_expr, parse_upf_and_resolve, LsRequirement, NullObjectFinder, PstSat, SimState,
    DEFAULT_VOLTAGE_TOL_UV,
};

#[test]
fn pst_sat_empty_guard_illegal_bv_logic_equiv_mismatch_and_32_domain_perf() {
    let empty_upf = r#"
        upf_version 2.1
        create_supply_net VDD_EMPTY
        create_supply_net VDD_AON
        create_supply_net VSS
        create_supply_set SS_EMPTY -function {power VDD_EMPTY} -function {ground VSS}
        create_supply_set SS_AON   -function {power VDD_AON}   -function {ground VSS}
        add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
    "#;
    let res_empty =
        parse_upf_and_resolve(empty_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_empty = PstSat::build(&res_empty, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_empty.is_state_space_consistent());
    assert!(sat_empty
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "PST_MISSING_PORT_STATE"));
    assert!(sat_empty
        .can_be_off_while_on(
            res_empty.ss_by_name["SS_EMPTY"],
            res_empty.ss_by_name["SS_AON"]
        )
        .is_some());
    assert!(matches!(
        sat_empty.voltage_relation(
            res_empty.ss_by_name["SS_EMPTY"],
            res_empty.ss_by_name["SS_AON"],
            DEFAULT_VOLTAGE_TOL_UV
        ),
        LsRequirement::Undetermined { .. }
    ));

    let bv_upf = r#"
        upf_version 3.1
        create_supply_net VDD_A
        create_supply_net VDD_B
        create_supply_net VSS
        create_supply_set SS_A -function {power VDD_A} -function {ground VSS}
        create_supply_set SS_B -function {power VDD_B} -function {ground VSS}
        add_power_state SS_A -state {ON  -supply_expr {power == {FULL_ON 0.90}} -logic_expr {u_pmu.mode[1:0] >= 2'd2} -simstate NORMAL}
        add_power_state SS_A -state {OFF -supply_expr {power == OFF}            -logic_expr {u_pmu/mode[1:0] <  2'd2} -simstate CORRUPT_ON_ACTIVITY}
        add_power_state SS_B -state {ON  -supply_expr {power == {FULL_ON 0.75}} -logic_expr {en_b} -simstate NORMAL}
        add_power_state SS_B -state {OFF -supply_expr {power == OFF}            -logic_expr {!en_b} -simstate OFF}
        add_power_state SS_B -state {ILL_AB -logic_expr {(u_pmu/mode[1:0] < 2'd2) && en_b} -illegal}
        set_equivalent -sets {SS_A SS_B}
    "#;
    let res_bv = parse_upf_and_resolve(bv_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_bv = PstSat::build(&res_bv, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_bv.is_state_space_consistent());
    assert!(sat_bv
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "UPF_EQUIV_MISMATCH"));
    assert!(sat_bv
        .can_be_off_while_on(res_bv.ss_by_name["SS_A"], res_bv.ss_by_name["SS_B"])
        .is_none());
    let states = sat_bv.enumerate_symbolic_states(10);
    assert!(!states.is_empty());
    let exported = sat_bv.export_logic_constraints();
    assert!(exported.iter().any(|c| c.is_illegal));
    assert!(exported
        .iter()
        .any(|c| c.simstate == Some(SimState::CorruptOnActivity)));

    // 32-domain synthetic SoC PST SAT benchmark (<100 ms).
    let mut soc = String::from("upf_version 3.1\ncreate_supply_net VSS\n");
    for i in 0..32 {
        soc.push_str(&format!(
            "create_supply_net VDD_{i}\n\
             create_supply_set SS_{i} -function {{power VDD_{i}}} -function {{ground VSS}}\n\
             create_power_domain PD_{i} -elements {{u_d{i}}} -supply {{primary SS_{i}}}\n\
             add_power_state SS_{i} -state {{ON  -supply_expr {{power == {{FULL_ON 0.90}}}} -logic_expr {{pwr_ctrl[{i}]}} -simstate NORMAL}}\n\
             add_power_state SS_{i} -state {{OFF -supply_expr {{power == OFF}} -logic_expr {{!pwr_ctrl[{i}]}} -simstate CORRUPT}}\n"
        ));
    }
    let res_soc = parse_upf_and_resolve(&soc, None, None, Arc::new(NullObjectFinder)).unwrap();
    let t0 = Instant::now();
    let mut sat_soc = PstSat::build(&res_soc, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_soc.is_state_space_consistent());
    for i in 0..31 {
        let sa = res_soc.ss_by_name[&format!("SS_{i}")];
        let sb = res_soc.ss_by_name[&format!("SS_{}", i + 1)];
        assert!(sat_soc.can_be_off_while_on(sa, sb).is_some());
        assert_eq!(
            sat_soc.voltage_relation(sa, sb, DEFAULT_VOLTAGE_TOL_UV),
            LsRequirement::None
        );
    }
    let elapsed_ms = t0.elapsed().as_millis();
    assert!(
        elapsed_ms < 100,
        "32-domain SoC PST SAT took {elapsed_ms} ms (expected < 100 ms)"
    );
}

#[test]
fn illegal_without_equiv_inconsistent_pst_unreachable_and_dvfs_sinz_amo() {
    let ill_cross = r#"
        upf_version 3.1
        create_supply_net VDD_C
        create_supply_net VDD_D
        create_supply_net VSS
        create_supply_set SS_C -function {power VDD_C} -function {ground VSS}
        create_supply_set SS_D -function {power VDD_D} -function {ground VSS}
        add_power_state SS_C -state {ON  -supply_expr {power == {FULL_ON 0.9}} -logic_expr {en_c}  -simstate NORMAL}
        add_power_state SS_C -state {OFF -supply_expr {power == OFF}           -logic_expr {!en_c} -simstate CORRUPT}
        add_power_state SS_D -state {ON  -supply_expr {power == {FULL_ON 0.9}} -logic_expr {en_d}  -simstate NORMAL}
        add_power_state SS_D -state {OFF -supply_expr {power == OFF}           -logic_expr {!en_d} -simstate CORRUPT}
        add_power_state SS_D -state {ILL_C_OFF_D_ON -logic_expr {!en_c && en_d} -illegal}
    "#;
    let res_ill = parse_upf_and_resolve(ill_cross, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_ill = PstSat::build(&res_ill, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_ill.is_state_space_consistent());
    assert!(sat_ill
        .can_be_off_while_on(res_ill.ss_by_name["SS_C"], res_ill.ss_by_name["SS_D"])
        .is_none());
    assert!(sat_ill
        .can_be_off_while_on(res_ill.ss_by_name["SS_D"], res_ill.ss_by_name["SS_C"])
        .is_some());

    let unsat_upf = r#"
        upf_version 3.1
        create_supply_net VDD_X
        create_supply_net VSS
        create_supply_set SS_X -function {power VDD_X} -function {ground VSS}
        add_power_state SS_X -state {ON  -supply_expr {power == {FULL_ON 0.9}} -logic_expr {pwr_ctrl[0]}  -simstate NORMAL}
        add_power_state SS_X -state {OFF -supply_expr {power == OFF}           -logic_expr {!pwr_ctrl[0]} -simstate CORRUPT}
        add_power_state SS_X -state {ILL_ON  -logic_expr {pwr_ctrl[0]}  -illegal}
        add_power_state SS_X -state {ILL_OFF -logic_expr {!pwr_ctrl[0]} -illegal}
    "#;
    let res_unsat =
        parse_upf_and_resolve(unsat_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_unsat = PstSat::build(&res_unsat, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!sat_unsat.is_state_space_consistent());
    assert!(sat_unsat
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "PST_INCONSISTENT"));

    let bad_pst = r#"
        upf_version 1.0
        create_supply_net VDD_A
        create_supply_net VSS
        add_port_state VDD_A -state {ON 0.90}
        add_port_state VSS -state {GND 0.0}
        create_pst p -supplies {VDD_A VSS}
        add_pst_state GOOD -pst p -state {ON GND}
        add_pst_state BAD  -pst p -state {BOGUS GND}
    "#;
    let res_bpst = parse_upf_and_resolve(bad_pst, None, None, Arc::new(NullObjectFinder)).unwrap();
    let sat_bpst = PstSat::build(&res_bpst, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_bpst
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "PST_UNREACHABLE_STATE"));

    let mut dvfs = String::from(
        "upf_version 3.1\ncreate_supply_net VDD_DVFS\ncreate_supply_net VSS\n\
         create_supply_set SS_DVFS -function {power VDD_DVFS} -function {ground VSS}\n",
    );
    for k in 0..20 {
        let mv = 600 + k * 25;
        dvfs.push_str(&format!(
            "add_power_state SS_DVFS -state {{S_{k} -supply_expr {{power == {{FULL_ON {mv}mV}}}} -simstate NORMAL}}\n"
        ));
    }
    let res_dvfs = parse_upf_and_resolve(&dvfs, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_dvfs = PstSat::build(&res_dvfs, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_dvfs.is_state_space_consistent());
    let enum_dvfs = sat_dvfs.enumerate_symbolic_states(25);
    assert_eq!(enum_dvfs.len(), 20);

    let tern_upf = r#"
        upf_version 3.1
        create_supply_net VDD_T
        create_supply_net VSS
        create_supply_set SS_T -function {power VDD_T} -function {ground VSS}
        add_power_state SS_T -state {ON  -supply_expr {power == {FULL_ON 0.9}} -logic_expr {(sel ? (val << 1) : 2'b00) == 2'b10} -simstate NORMAL}
        add_power_state SS_T -state {OFF -supply_expr {power == OFF}           -logic_expr {!sel} -simstate CORRUPT}
    "#;
    let res_tern = parse_upf_and_resolve(tern_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_tern = PstSat::build(&res_tern, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_tern.is_state_space_consistent());
    let tern_states: BTreeSet<String> = sat_tern
        .enumerate_symbolic_states(10)
        .into_iter()
        .filter_map(|w| w.supply_states.get("SS_T").cloned())
        .collect();
    assert_eq!(
        tern_states,
        BTreeSet::from(["ON".to_string(), "OFF".to_string()])
    );
}

#[test]
fn regression_pst_sat_single_outcome_disjunction_unsliced_bv_and_syntax() {
    let single_ill = r#"
        upf_version 3.1
        create_supply_net VDD_A
        create_supply_net VDD_B
        create_supply_net VSS
        create_supply_set SS_A -function {power VDD_A} -function {ground VSS}
        create_supply_set SS_B -function {power VDD_B} -function {ground VSS}
        add_power_state SS_A -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_A -state {ILL_OFF -supply_expr {power == OFF} -logic_expr {en} -illegal}
        add_power_state SS_B -state {ON  -supply_expr {power == {FULL_ON 0.9}} -logic_expr {en}  -simstate NORMAL}
        add_power_state SS_B -state {OFF -supply_expr {power == OFF}           -logic_expr {!en} -simstate CORRUPT}
    "#;
    let res_si = parse_upf_and_resolve(single_ill, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_si = PstSat::build(&res_si, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_si.is_state_space_consistent());
    assert!(sat_si
        .can_be_off_while_on(res_si.ss_by_name["SS_B"], res_si.ss_by_name["SS_A"])
        .is_some());
    let b_states: BTreeSet<String> = sat_si
        .enumerate_symbolic_states(10)
        .into_iter()
        .filter_map(|w| w.supply_states.get("SS_B").cloned())
        .collect();
    assert!(b_states.contains("ON") && b_states.contains("OFF"));

    let disj_upf = r#"
        upf_version 3.1
        create_supply_net VDD_A
        create_supply_net VDD_B
        create_supply_net VSS
        create_supply_set SS_A -function {power VDD_A} -function {ground VSS}
        create_supply_set SS_B -function {power VDD_B} -function {ground VSS}
        add_power_state SS_A -state {DVFS -supply_expr {(power == {FULL_ON 0.9} || power == {FULL_ON 1.1}) && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_B -state {ON   -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
    "#;
    let res_disj = parse_upf_and_resolve(disj_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_disj = PstSat::build(&res_disj, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_disj.is_state_space_consistent());
    assert!(matches!(
        sat_disj.voltage_relation(
            res_disj.ss_by_name["SS_A"],
            res_disj.ss_by_name["SS_B"],
            DEFAULT_VOLTAGE_TOL_UV
        ),
        LsRequirement::HighToLow { .. }
    ));

    let unsliced_upf = r#"
        upf_version 3.1
        create_supply_net VDD_A
        create_supply_net VDD_B
        create_supply_net VSS
        create_supply_set SS_A -function {power VDD_A} -function {ground VSS}
        create_supply_set SS_B -function {power VDD_B} -function {ground VSS}
        add_power_state SS_A -state {ON -supply_expr {power == {FULL_ON 0.9}} -logic_expr {u_pmu/mode[1:0] == 2'b10} -simstate NORMAL}
        add_power_state SS_B -state {ON -supply_expr {power == {FULL_ON 0.9}} -logic_expr {u_pmu.mode == 2 && mode == 2} -simstate NORMAL}
    "#;
    let res_un =
        parse_upf_and_resolve(unsliced_upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_un = PstSat::build(&res_un, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_un.is_state_space_consistent());

    assert!(parse_logic_expr("a + b > 0").is_err());
    assert!(parse_logic_expr("a == 1 garbage").is_err());
    assert!(parse_logic_expr("en_a )").is_err());
    assert!(parse_logic_expr("mode == 2'b1x").is_err());

    let bad_ill = r#"
        upf_version 3.1
        create_supply_net VDD_A
        create_supply_net VSS
        create_supply_set SS_A -function {power VDD_A} -function {ground VSS}
        add_power_state SS_A -state {ON -supply_expr {power == {FULL_ON 0.9}} -simstate NORMAL}
        add_power_state SS_A -state {ILL_BAD -logic_expr {a ==} -illegal}
    "#;
    let res_bi = parse_upf_and_resolve(bad_ill, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_bi = PstSat::build(&res_bi, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_bi.is_state_space_consistent());
    assert!(sat_bi
        .diagnostics()
        .iter()
        .any(|d| d.rule_id == "UPF_EXPR_SYNTAX"));
}

#[test]
fn soc_32_and_64_domain_and_65535_shift_perf_benchmarks() {
    // Warm up Tcl interpreter before per-expression <25ms latency assertions.
    check_logic("1'b1", true);

    for (expr, expected) in [
        ("bus == 4'h0 && ((~4'hF) >> 65535) == 1'b1", false),
        ("bus_b == '1 && ((~bus_b) >> 65535) == 1'b1", false),
        ("(((~bus) >> 65535) == 1'b1) && bus == '1", false),
        ("((bus >> 65535) ^ (x == x)) == 1'b0 && bus == 8'hFF", false),
        ("((bus >> 65535) ? 1'b0 : 1'b1) && bus == 8'hFF", true),
        ("((bus >> 65535) | 4'h0) == 4'h0 && bus == 8'hFF", true),
        ("((bus >> 65535) ^ 4'h0) == 4'h0 && bus == 8'hFF", true),
        ("((bus >> 65535) & 4'hF) == 4'h0 && bus == 8'hFF", true),
        ("((~(bus >> 65535)) & 4'hF) == 4'hF && bus == 8'hFF", true),
        ("(bus >> 65535) < 4'h1 && bus == 8'hFF", true),
        ("!(bus >> 65535) && bus == 8'hFF", true),
        ("((bus >> 65535) << 1) == 4'h0 && bus == 8'hFF", true),
        ("((bus >> 65535) ^ (~4'h0)) == 8'hFF && bus == 8'hFF", true),
    ] {
        let t = Instant::now();
        check_logic(expr, expected);
        assert!(
            t.elapsed().as_millis() < 25,
            "65535-shift `{expr}` took {:?} (>=25ms)",
            t.elapsed()
        );
    }

    // 32 and 64-domain SoC < 100ms (with and without create_pst):
    for n_dom in [32usize, 64] {
        let mut upf = String::from(
            "upf_version 2.1\ncreate_supply_net VSS\nadd_port_state VSS -state {GND 0.0}\n",
        );
        let mut pst_supplies = String::from("VSS");
        let mut row_all_on = String::from("GND");
        for i in 0..n_dom {
            upf.push_str(&format!(
                "create_supply_net VDD_{i}\n\
                 create_supply_set SS_{i} -function {{power VDD_{i}}} -function {{ground VSS}}\n\
                 create_power_domain PD_{i} -elements {{u_{i}}} -supply {{primary SS_{i}}}\n\
                 add_power_state SS_{i} -state {{ON -supply_expr {{power == {{FULL_ON 0.9}} && ground == {{FULL_ON 0.0}}}} -logic_expr {{en_{i} && !sleep_{i}}} -simstate NORMAL}}\n\
                 add_power_state SS_{i} -state {{OFF -supply_expr {{power == OFF && ground == {{FULL_ON 0.0}}}} -logic_expr {{!en_{i} || sleep_{i}}} -simstate CORRUPT}}\n"
            ));
            pst_supplies.push_str(&format!(" SS_{i}"));
            row_all_on.push_str(" ON");
        }
        upf.push_str(&format!(
            "create_pst PST_{n_dom} -supplies {{{pst_supplies}}}\nadd_pst_state ALL_ON -pst PST_{n_dom} -state {{{row_all_on}}}\n"
        ));
        let t_soc = Instant::now();
        let res = parse_upf_and_resolve(&upf, None, None, Arc::new(NullObjectFinder)).unwrap();
        let mut sat = PstSat::build(&res, DEFAULT_VOLTAGE_TOL_UV);
        assert!(sat.is_state_space_consistent());
        assert!(!sat.enumerate_symbolic_states(1).is_empty());
        assert!(
            t_soc.elapsed().as_millis() < 100,
            "{n_dom}-domain SoC took {:?} (>=100ms)",
            t_soc.elapsed()
        );
    }
}
