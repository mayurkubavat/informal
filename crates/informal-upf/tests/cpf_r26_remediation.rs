mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{temp_dir, MockDesignFinder};
use informal_upf::{
    load_upf, load_upf_files, resolve, ObjectFinder, PstSat, ResolvedPowerIntent, SimState,
    UpfFileSpec, DEFAULT_VOLTAGE_TOL_UV,
};

fn default_finder() -> Arc<dyn ObjectFinder> {
    Arc::new(MockDesignFinder::new(
        &["u_wrap", "u_sub", "u_wrap/u_sub"],
        &["u_wrap", "u_sub", "u_wrap/u_sub"],
    ))
}

fn file_spec(path: PathBuf, scope: Option<&str>, is_cpf: bool) -> UpfFileSpec {
    UpfFileSpec {
        path,
        scope: scope.map(str::to_string),
        version: None,
        is_cpf,
    }
}

fn run_seq_and_nested(
    tag: &str,
    files: &[(&str, Option<&str>, bool, &str)],
    top_body: &str,
) -> (ResolvedPowerIntent, ResolvedPowerIntent) {
    let dir = temp_dir(tag);
    let finder = default_finder();
    let mut specs = Vec::with_capacity(files.len());
    for (fname, scope, is_cpf, body) in files {
        let p = dir.join(fname);
        std::fs::write(&p, body).unwrap();
        specs.push(file_spec(p, *scope, *is_cpf));
    }
    let top_upf = dir.join("top.upf");
    let mut expanded_top = String::new();
    for line in top_body.lines() {
        let mut rewritten = line.to_string();
        for (fname, _, _, _) in files {
            let p = dir.join(fname);
            rewritten = rewritten.replace(fname, &p.display().to_string());
        }
        expanded_top.push_str(&rewritten);
        expanded_top.push('\n');
    }
    std::fs::write(&top_upf, expanded_top).unwrap();

    let mut intent_seq = load_upf_files(&specs, finder.clone()).unwrap();
    let res_seq = resolve(&mut intent_seq, finder.as_ref());
    let res_nest = load_upf(&top_upf, None, None, finder).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (res_seq, res_nest)
}

fn ss_state_summary(
    res: &ResolvedPowerIntent,
    ss_name: &str,
) -> Vec<(String, Option<SimState>, bool)> {
    let id = res.ss_by_name[ss_name].0 as usize;
    res.ss_states[id]
        .iter()
        .map(|s| (s.state_name.clone(), s.simstate, s.illegal))
        .collect()
}

#[test]
fn bug1_shutoff_off_upd_then_sw_rule_and_aliases_seq_matches_nested() {
    let c1_root = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB VDD_EXT} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_sub} -shutoff_condition {!pwr_en}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        end_design\n";
    let c2_psw = "set_cpf_version 2.0\n\
        create_power_switch_rule -name SW_SUB -domain PD_SUB -external_power_net VDD_EXT -enable_condition {new_en}\n\
        end_design\n";

    let c1_sw_no_en = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB VDD_EXT} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_sub} -shutoff_condition {!pwr_en}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        create_power_switch_rule -name SW_SUB -domain PD_SUB -external_power_net VDD_EXT\n\
        end_design\n";
    let c2_upd_sw = "set_cpf_version 2.0\n\
        update_power_switch_rule -name SW_SUB -enable_condition {new_en}\n\
        end_design\n";

    for (tag, c1, c2, target) in [
        ("bug1_ss", c1_root, c2_psw, "SS_PD_SUB"),
        ("bug1_primary", c1_root, c2_psw, "PD_SUB.primary"),
        ("bug1_dom", c1_root, c2_psw, "PD_SUB"),
        ("bug1_abs", c1_root, c2_psw, "/SS_PD_SUB"),
        ("bug1c_upd_sw", c1_sw_no_en, c2_upd_sw, "SS_PD_SUB"),
    ] {
        let u = format!(
            "upf_version 2.1\n\
             add_power_state {target} -state {{OFF -update -simstate NORMAL -supply_expr {{power == {{FULL_ON 0.9}} && ground == {{FULL_ON 0.0}}}} -logic_expr {{new_en}}}}\n"
        );
        let (res_seq, res_nest) = run_seq_and_nested(
            tag,
            &[
                ("c1.cpf", None, true, c1),
                ("u.upf", Some("/"), false, &u),
                ("c2.cpf", Some("/"), true, c2),
            ],
            "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf\nload_upf \"u.upf\" -scope /\nload_upf \"c2.cpf\" -format cpf -scope /\n",
        );
        let s_seq = ss_state_summary(&res_seq, "SS_PD_SUB");
        let s_nest = ss_state_summary(&res_nest, "SS_PD_SUB");
        assert_eq!(s_seq, s_nest, "{tag}: seq != nest");
        assert_eq!(
            s_seq,
            vec![
                ("ON".to_string(), Some(SimState::Normal), false),
                ("OFF".to_string(), Some(SimState::Normal), false),
            ],
            "{tag}"
        );
    }

    // Scoped -scope /u_wrap
    let c1_sub = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_SUB VDD_EXT} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_SUB -instances {u_sub} -shutoff_condition {!pwr_en}\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        end_design\n";
    let u_sub = "upf_version 2.1\n\
        add_power_state SS_u_wrap/PD_SUB -state {OFF -update -simstate NORMAL -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {new_en}}\n";
    let (res_seq, res_nest) = run_seq_and_nested(
        "bug1_scoped_wrap",
        &[
            ("c1_sub.cpf", Some("/u_wrap"), true, c1_sub),
            ("u.upf", Some("/"), false, u_sub),
            ("c2_sub.cpf", Some("/u_wrap"), true, c2_psw),
        ],
        "upf_version 2.1\nload_upf \"c1_sub.cpf\" -format cpf -scope /u_wrap\nload_upf \"u.upf\" -scope /\nload_upf \"c2_sub.cpf\" -format cpf -scope /u_wrap\n",
    );
    let s_seq = ss_state_summary(&res_seq, "SS_u_wrap/PD_SUB");
    let s_nest = ss_state_summary(&res_nest, "SS_u_wrap/PD_SUB");
    assert_eq!(s_seq, s_nest, "scoped_wrap seq != nest");
    assert_eq!(
        s_seq,
        vec![
            ("ON".to_string(), Some(SimState::Normal), false),
            ("OFF".to_string(), Some(SimState::Normal), false),
        ]
    );
}

#[test]
fn bug2_off_upd_then_pm_then_nc_refresh_and_aliases_seq_matches_nested() {
    let c1 = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_sub}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        end_design\n";
    let c2 = "set_cpf_version 2.0\n\
        create_nominal_condition -name OFF -voltage 0.0 -state off\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_SUB@OFF} -condition {!pwr_en}\n\
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -condition {pwr_en} -default\n\
        end_design\n";
    let c3 = "set_cpf_version 2.0\n\
        create_nominal_condition -name OFF -voltage 0.9 -state on\n\
        end_design\n";

    // Test all 4 target aliases + both orderings (c1 -> u -> c2 -> c3 and c1 -> c2 -> u -> c3)
    for (tag, target) in [
        ("ss", "SS_PD_SUB"),
        ("abs_ss", "/SS_PD_SUB"),
        ("primary", "PD_SUB.primary"),
        ("domain", "PD_SUB"),
    ] {
        let u = format!(
            "upf_version 2.1\n\
             add_power_state {target} -state {{OFF -update -supply_expr {{power == OFF && ground == {{FULL_ON 0.0}}}} -logic_expr {{!other_en}} -simstate CORRUPT}}\n"
        );
        let (res_seq, res_nest) = run_seq_and_nested(
            &format!("bug2_{tag}"),
            &[
                ("c1.cpf", None, true, c1),
                ("u.upf", Some("/"), false, &u),
                ("c2.cpf", Some("/"), true, c2),
                ("c3.cpf", Some("/"), true, c3),
            ],
            "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf\nload_upf \"u.upf\"\nload_upf \"c2.cpf\" -format cpf\nload_upf \"c3.cpf\" -format cpf\n",
        );
        let s_seq = ss_state_summary(&res_seq, "SS_PD_SUB");
        let s_nest = ss_state_summary(&res_nest, "SS_PD_SUB");
        assert_eq!(s_seq, s_nest, "{tag}: seq != nest");
        assert_eq!(
            s_seq,
            vec![
                ("OFF".to_string(), Some(SimState::Normal), false),
                ("NC_ON".to_string(), Some(SimState::Normal), false),
            ],
            "{tag}: expected single refreshed OFF (Normal) + NC_ON"
        );
        assert!(PstSat::build(&res_seq, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());
    }

    // Order: c1 -> c2 -> u.upf(OFF -update) -> c3(NC OFF=0.9V on)
    let u_after_c2 = "upf_version 2.1\n\
        add_power_state SS_PD_SUB -state {OFF -update -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {!other_en} -simstate CORRUPT}\n";
    let (res_seq, res_nest) = run_seq_and_nested(
        "bug2_c1_c2_u_c3",
        &[
            ("c1.cpf", None, true, c1),
            ("c2.cpf", Some("/"), true, c2),
            ("u.upf", Some("/"), false, u_after_c2),
            ("c3.cpf", Some("/"), true, c3),
        ],
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf\nload_upf \"c2.cpf\" -format cpf\nload_upf \"u.upf\"\nload_upf \"c3.cpf\" -format cpf\n",
    );
    assert_eq!(
        ss_state_summary(&res_seq, "SS_PD_SUB"),
        ss_state_summary(&res_nest, "SS_PD_SUB")
    );
    assert_eq!(
        ss_state_summary(&res_seq, "SS_PD_SUB"),
        vec![
            ("OFF".to_string(), Some(SimState::Normal), false),
            ("NC_ON".to_string(), Some(SimState::Normal), false),
        ]
    );

    // Order: c1 -> c2(PM1: PD_TOP@NC_ON, PD_SUB@OFF) -> u.upf(SS_PD_SUB OFF -update) -> c3(NC_ON updated)
    // SS_PD_SUB's OFF -update MUST be preserved!
    let c3_nc_on = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_ON -voltage 0.8 -state on\n\
        end_design\n";
    let u_sub_corrupt = "upf_version 2.1\n\
        add_power_state SS_PD_SUB -state {OFF -update -logic_expr {!other_en} -simstate CORRUPT}\n";
    let (res_seq_p, res_nest_p) = run_seq_and_nested(
        "bug2_preserve_sub_upd",
        &[
            ("c1.cpf", None, true, c1),
            ("c2.cpf", Some("/"), true, c2),
            ("u.upf", Some("/"), false, u_sub_corrupt),
            ("c3.cpf", Some("/"), true, c3_nc_on),
        ],
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf\nload_upf \"c2.cpf\" -format cpf\nload_upf \"u.upf\"\nload_upf \"c3.cpf\" -format cpf\n",
    );
    let sub_seq = &res_seq_p.ss_states[res_seq_p.ss_by_name["SS_PD_SUB"].0 as usize];
    let sub_nest = &res_nest_p.ss_states[res_nest_p.ss_by_name["SS_PD_SUB"].0 as usize];
    assert_eq!(sub_seq.len(), sub_nest.len());
    assert_eq!(sub_seq.len(), 2);
    let off_st = sub_seq.iter().find(|s| s.state_name == "OFF").unwrap();
    assert_eq!(off_st.logic_expr.as_deref(), Some("!other_en"));
    assert_eq!(off_st.simstate, Some(SimState::Corrupt));

    // Scoped -scope u_wrap Bug 2
    let c1_wrap = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_SUB -instances {u_sub}\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        end_design\n";
    let c2_wrap = "set_cpf_version 2.0\n\
        create_nominal_condition -name OFF -voltage 0.0 -state off\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_OFF -domain_conditions {PD_SUB@OFF} -condition {!pwr_en}\n\
        create_power_mode -name PM_ON -domain_conditions {PD_SUB@NC_ON} -condition {pwr_en} -default\n\
        end_design\n";
    let u_wrap = "upf_version 2.1\n\
        add_power_state PD_SUB.primary -state {OFF -update -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {!other_en} -simstate CORRUPT}\n";
    let (res_seq_w, res_nest_w) = run_seq_and_nested(
        "bug2_scoped_wrap",
        &[
            ("c1.cpf", Some("u_wrap"), true, c1_wrap),
            ("u.upf", Some("u_wrap"), false, u_wrap),
            ("c2.cpf", Some("u_wrap"), true, c2_wrap),
            ("c3.cpf", Some("u_wrap"), true, c3),
        ],
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf -scope u_wrap\nload_upf \"u.upf\" -scope u_wrap\nload_upf \"c2.cpf\" -format cpf -scope u_wrap\nload_upf \"c3.cpf\" -format cpf -scope u_wrap\n",
    );
    assert_eq!(
        ss_state_summary(&res_seq_w, "SS_u_wrap/PD_SUB"),
        ss_state_summary(&res_nest_w, "SS_u_wrap/PD_SUB")
    );
    assert_eq!(
        ss_state_summary(&res_seq_w, "SS_u_wrap/PD_SUB"),
        vec![
            ("OFF".to_string(), Some(SimState::Normal), false),
            ("NC_ON".to_string(), Some(SimState::Normal), false),
        ]
    );
}

#[test]
fn late_psw_and_pm_in_c2_and_non_upd_illegal_nc_seq_matches_nested() {
    // LATE_PSW_AND_PM_IN_C2
    let c1_late = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB VDD_EXT} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_sub}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        end_design\n";
    let u_late = "upf_version 2.1\n\
        add_power_state SS_PD_SUB -state {OFF -update -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {!pwr_en} -simstate CORRUPT}\n";
    let c2_late = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_switch_rule -name SW_SUB -domain PD_SUB -external_power_net VDD_EXT -enable_condition {pwr_en}\n\
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -condition {pwr_en} -default\n\
        end_design\n";
    let (res_seq_l, res_nest_l) = run_seq_and_nested(
        "late_psw_pm_c2",
        &[
            ("c1.cpf", None, true, c1_late),
            ("u.upf", Some("/"), false, u_late),
            ("c2.cpf", Some("/"), true, c2_late),
        ],
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf\nload_upf \"u.upf\"\nload_upf \"c2.cpf\" -format cpf\n",
    );
    assert_eq!(
        ss_state_summary(&res_seq_l, "SS_PD_SUB"),
        ss_state_summary(&res_nest_l, "SS_PD_SUB")
    );
    assert_eq!(
        ss_state_summary(&res_seq_l, "SS_PD_SUB"),
        vec![("NC_ON".to_string(), Some(SimState::Normal), false)]
    );

    // SEEN_STATES_NON_UPD_ILLEGAL_NC
    let c1_top = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_TOP} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\n\
        end_design\n";
    let u_ill = "upf_version 2.1\nadd_power_state SS_PD_TOP -state {NC_ON -illegal}\n";
    let c2_top = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_T -domain_conditions {PD_TOP@NC_ON} -condition {pwr_en} -default\n\
        end_design\n";
    let (res_seq_i, res_nest_i) = run_seq_and_nested(
        "seen_non_upd_ill",
        &[
            ("c1.cpf", None, true, c1_top),
            ("u.upf", Some("/"), false, u_ill),
            ("c2.cpf", Some("/"), true, c2_top),
        ],
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf\nload_upf \"u.upf\"\nload_upf \"c2.cpf\" -format cpf\n",
    );
    assert_eq!(
        ss_state_summary(&res_seq_i, "SS_PD_TOP"),
        ss_state_summary(&res_nest_i, "SS_PD_TOP")
    );
    let mut sat_seq_i = PstSat::build(&res_seq_i, DEFAULT_VOLTAGE_TOL_UV);
    let mut sat_nest_i = PstSat::build(&res_nest_i, DEFAULT_VOLTAGE_TOL_UV);
    assert!(!sat_seq_i.is_state_space_consistent());
    assert!(!sat_nest_i.is_state_space_consistent());
    let rules_seq: Vec<&str> = sat_seq_i
        .diagnostics()
        .iter()
        .map(|d| d.rule_id.as_str())
        .collect();
    let rules_nest: Vec<&str> = sat_nest_i
        .diagnostics()
        .iter()
        .map(|d| d.rule_id.as_str())
        .collect();
    assert_eq!(rules_seq, rules_nest);
    assert!(rules_seq.contains(&"PST_INCONSISTENT"));
    assert!(rules_seq.contains(&"PST_UNREACHABLE_STATE"));
}

#[test]
fn adv1_cross_scope_update_pm_illegal_and_isolation_control_seq_matches_nested() {
    let c1_wrap = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_SUB -instances {u_sub}\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_SUB_ON -domain_conditions {PD_SUB@NC_ON} -condition {en_sub} -default\n\
        end_design\n";
    let c2_mix_upd = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_MIX -domain_conditions {PD_TOP@NC_ON u_wrap/PD_SUB@NC_OFF} -condition {mix_cond}\n\
        create_power_mode -name PM_TOP_ON -domain_conditions {PD_TOP@NC_ON u_wrap/PD_SUB@NC_ON} -condition {pwr_en} -default\n\
        end_design\n";
    let c2_mix_direct = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_MIX -domain_conditions {PD_TOP@NC_ON u_wrap/PD_SUB@NC_OFF} -condition {mix_cond} -illegal\n\
        create_power_mode -name PM_TOP_ON -domain_conditions {PD_TOP@NC_ON u_wrap/PD_SUB@NC_ON} -condition {pwr_en} -default\n\
        end_design\n";
    let c3_illegal = "set_cpf_version 2.0\n\
        update_power_mode -name PM_MIX -illegal\n\
        end_design\n";

    let (res_ctrl, res_ctrl_n) = run_seq_and_nested(
        "adv1_ctrl_direct",
        &[
            ("c1.cpf", Some("u_wrap"), true, c1_wrap),
            ("c2.cpf", Some("/"), true, c2_mix_direct),
        ],
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf -scope u_wrap\nload_upf \"c2.cpf\" -format cpf -scope /\n",
    );
    let (res_seq, res_nest) = run_seq_and_nested(
        "adv1_cross_scope",
        &[
            ("c1.cpf", Some("u_wrap"), true, c1_wrap),
            ("c2.cpf", Some("/"), true, c2_mix_upd),
            ("c3.cpf", Some("/"), true, c3_illegal),
        ],
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf -scope u_wrap\nload_upf \"c2.cpf\" -format cpf -scope /\nload_upf \"c3.cpf\" -format cpf -scope /\n",
    );

    let pst_rows = |r: &ResolvedPowerIntent| -> Vec<String> {
        r.intent
            .pst_tables
            .iter()
            .flat_map(|t| {
                t.rows
                    .iter()
                    .map(|row| format!("{}:{}", t.name, row.state_name))
            })
            .collect()
    };
    assert_eq!(pst_rows(&res_seq), pst_rows(&res_nest), "seq != nest");
    assert_eq!(pst_rows(&res_seq), pst_rows(&res_ctrl), "upd != direct");
    assert_eq!(pst_rows(&res_ctrl), pst_rows(&res_ctrl_n));
    assert!(
        !pst_rows(&res_seq).iter().any(|r| r.ends_with(":PM_MIX")),
        "stale PM_MIX row remained: {:?}",
        pst_rows(&res_seq)
    );
    for (label, r) in [("seq", &res_seq), ("nest", &res_nest)] {
        let mut sat = PstSat::build(r, DEFAULT_VOLTAGE_TOL_UV);
        assert!(sat.is_state_space_consistent(), "{label}: inconsistent");
        assert!(
            sat.diagnostics()
                .iter()
                .all(|d| d.rule_id != "PST_UNREACHABLE_STATE"),
            "{label}: unexpected diagnostics {:?}",
            sat.diagnostics()
        );
        let sub_ss = r.ss_by_name["SS_u_wrap/PD_SUB"];
        let top_ss = r.ss_by_name["SS_PD_TOP"];
        assert!(
            sat.can_be_off_while_on(sub_ss, top_ss).is_none(),
            "{label}: u_wrap/PD_SUB must not turn off once PM_MIX is illegal"
        );
    }

    // set_design top variant (without PM_TOP_ON)
    let c1_top = format!("set_design top\n{c1_wrap}");
    let c2_top = "set_cpf_version 2.0\nset_design top\n\
        create_power_nets -nets {VDD_AON} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_MIX -domain_conditions {PD_TOP@NC_ON u_wrap/PD_SUB@NC_OFF} -condition {mix_cond}\n\
        end_design\n";
    let c3_top = "set_cpf_version 2.0\nset_design top\nupdate_power_mode -name PM_MIX -illegal\nend_design\n";
    let (res_dt_seq, res_dt_nest) = run_seq_and_nested(
        "adv1_set_design_top",
        &[
            ("c1.cpf", Some("u_wrap"), true, &c1_top),
            ("c2.cpf", Some("/"), true, c2_top),
            ("c3.cpf", Some("/"), true, c3_top),
        ],
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf -scope u_wrap\nload_upf \"c2.cpf\" -format cpf -scope /\nload_upf \"c3.cpf\" -format cpf -scope /\n",
    );
    assert_eq!(pst_rows(&res_dt_seq), pst_rows(&res_dt_nest));
    assert_eq!(
        pst_rows(&res_dt_seq),
        vec!["cpf_power_modes::top:PM_SUB_ON".to_string()]
    );
    let mut sat_dt = PstSat::build(&res_dt_seq, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_dt.is_state_space_consistent());
    assert!(sat_dt
        .diagnostics()
        .iter()
        .all(|d| d.rule_id != "PST_UNREACHABLE_STATE"));

    // Negative isolation control: unrelated non-overlapping scoped tables u_a/PM_X vs u_b/PM_X
    let dir_iso = temp_dir("adv1_iso_ctrl");
    let finder_ab: Arc<dyn ObjectFinder> =
        Arc::new(MockDesignFinder::new(&["u_a", "u_b"], &["u_a", "u_b"]));
    let mk_scoped_cpf = |pd: &str, vdd: &str| {
        format!(
            "set_cpf_version 2.0\n\
             create_power_nets -nets {{{vdd}}} -voltage 0.9\n\
             create_ground_nets -nets {{VSS}}\n\
             create_power_domain -name {pd} -default\n\
             update_power_domain -name {pd} -primary_power_net {vdd} -primary_ground_net VSS\n\
             create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
             create_nominal_condition -name NC_ALT -voltage 0.8 -state on\n\
             create_power_mode -name PM_KEEP -domain_conditions {{{pd}@NC_ON}} -default\n\
             create_power_mode -name PM_X -domain_conditions {{{pd}@NC_ALT}} -condition {{sel_x}}\n\
             end_design\n"
        )
    };
    let ca_p = dir_iso.join("ca.cpf");
    let cb_p = dir_iso.join("cb.cpf");
    let cu_p = dir_iso.join("cu.cpf");
    std::fs::write(&ca_p, mk_scoped_cpf("PD_A", "VDD_A")).unwrap();
    std::fs::write(&cb_p, mk_scoped_cpf("PD_B", "VDD_B")).unwrap();
    std::fs::write(
        &cu_p,
        "set_cpf_version 2.0\nupdate_power_mode -name PM_X -illegal\nend_design\n",
    )
    .unwrap();
    let mut intent_ab = load_upf_files(
        &[
            file_spec(ca_p, Some("u_a"), true),
            file_spec(cb_p, Some("u_b"), true),
            file_spec(cu_p, Some("u_a"), true),
        ],
        finder_ab.clone(),
    )
    .unwrap();
    let res_ab = resolve(&mut intent_ab, finder_ab.as_ref());
    let _ = std::fs::remove_dir_all(&dir_iso);
    let tbl_a = res_ab
        .intent
        .pst_tables
        .iter()
        .find(|t| t.scope == "u_a")
        .unwrap();
    let tbl_b = res_ab
        .intent
        .pst_tables
        .iter()
        .find(|t| t.scope == "u_b")
        .unwrap();
    assert!(!tbl_a.rows.iter().any(|r| r.state_name == "PM_X"));
    assert!(tbl_b.rows.iter().any(|r| r.state_name == "PM_X"));

    // Bridged u_a + u_b control: t0 gains SS_u_b/PD_B from root PM_AB, then u_b updates PM_X -illegal
    let dir_br = temp_dir("adv1_bridge_ctrl");
    let ca_b = dir_br.join("ca.cpf");
    let cb_b = dir_br.join("cb.cpf");
    let cab_b = dir_br.join("cab.cpf");
    let cu_b = dir_br.join("cu.cpf");
    std::fs::write(&ca_b, mk_scoped_cpf("PD_A", "VDD_A")).unwrap();
    std::fs::write(&cb_b, mk_scoped_cpf("PD_B", "VDD_B")).unwrap();
    std::fs::write(
        &cab_b,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_ON -voltage 0.9 -state on\ncreate_power_mode -name PM_AB -domain_conditions {u_a/PD_A@NC_ON u_b/PD_B@NC_ON}\nend_design\n",
    )
    .unwrap();
    std::fs::write(
        &cu_b,
        "set_cpf_version 2.0\nupdate_power_mode -name PM_X -illegal\nend_design\n",
    )
    .unwrap();
    let mut intent_br = load_upf_files(
        &[
            file_spec(ca_b, Some("u_a"), true),
            file_spec(cb_b, Some("u_b"), true),
            file_spec(cab_b, Some("/"), true),
            file_spec(cu_b, Some("u_b"), true),
        ],
        finder_ab.clone(),
    )
    .unwrap();
    let res_br = resolve(&mut intent_br, finder_ab.as_ref());
    let _ = std::fs::remove_dir_all(&dir_br);
    let br_tbl_b = res_br
        .intent
        .pst_tables
        .iter()
        .find(|t| t.scope == "u_b")
        .unwrap();
    assert!(!br_tbl_b.rows.iter().any(|r| r.state_name == "PM_X"));

    // Toggle -illegal false back to legal on cross-scope PM_MIX
    let c4_relegal =
        "set_cpf_version 2.0\nupdate_power_mode -name PM_MIX -illegal false\nend_design\n";
    let (res_re_seq, res_re_nest) = run_seq_and_nested(
        "adv1_relegal",
        &[
            ("c1.cpf", Some("u_wrap"), true, c1_wrap),
            ("c2.cpf", Some("/"), true, c2_mix_upd),
            ("c3.cpf", Some("/"), true, c3_illegal),
            ("c4.cpf", Some("/"), true, c4_relegal),
        ],
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf -scope u_wrap\nload_upf \"c2.cpf\" -format cpf -scope /\nload_upf \"c3.cpf\" -format cpf -scope /\nload_upf \"c4.cpf\" -format cpf -scope /\n",
    );
    assert_eq!(pst_rows(&res_re_seq), pst_rows(&res_re_nest));
    assert!(pst_rows(&res_re_seq).iter().any(|r| r.ends_with(":PM_MIX")));
    assert!(res_re_seq.intent.illegal_combinations.is_empty());
    let mut sat_re = PstSat::build(&res_re_seq, DEFAULT_VOLTAGE_TOL_UV);
    let sub_ss = res_re_seq.ss_by_name["SS_u_wrap/PD_SUB"];
    let top_ss = res_re_seq.ss_by_name["SS_PD_TOP"];
    assert!(sat_re.can_be_off_while_on(sub_ss, top_ss).is_some());
}
