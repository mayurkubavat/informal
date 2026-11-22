mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{temp_dir, MockDesignFinder};
use informal_upf::{
    load_upf_files, parse_cpf_and_resolve, resolve, ObjectFinder, PstSat, ResolvedPowerIntent,
    SimState, UpfFileSpec, DEFAULT_VOLTAGE_TOL_UV,
};

fn default_finder() -> Arc<dyn ObjectFinder> {
    Arc::new(MockDesignFinder::new(
        &["u_wrap", "u_sub", "u_wrap/u_sub", "u_a", "u_b"],
        &["u_wrap", "u_sub", "u_wrap/u_sub", "u_a", "u_b"],
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

fn run_seq_and_nested_mode(
    tag: &str,
    files: &[(&str, Option<&str>, bool, &str)],
    top_name: &str,
    top_is_cpf: bool,
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
    let top_file = dir.join(top_name);
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
    std::fs::write(&top_file, expanded_top).unwrap();

    let mut intent_seq = load_upf_files(&specs, finder.clone()).unwrap();
    let res_seq = resolve(&mut intent_seq, finder.as_ref());
    let mut intent_nest =
        load_upf_files(&[file_spec(top_file, None, top_is_cpf)], finder.clone()).unwrap();
    let res_nest = resolve(&mut intent_nest, finder.as_ref());
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
fn bug1_cross_scope_same_name_pm_x_no_bridge_and_bridged_overwrite_isolation() {
    let dir = temp_dir("r28_bug1_pmx");
    let finder = default_finder();

    let cb = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_B} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_B -default\n\
        update_power_domain -name PD_B -primary_power_net VDD_B -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_KEEP_B -domain_conditions {PD_B@NC_ON} -default\n\
        create_power_mode -name PM_X -domain_conditions {PD_B@NC_OFF} -condition {sel_x}\n\
        end_design\n";
    let ct_ill_pmx = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_TOP} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_TOP -domain_conditions {PD_TOP@NC_ON} -default\n\
        create_power_mode -name PM_X -domain_conditions {PD_TOP@NC_ON u_b/PD_B@NC_ON} -condition {sel_b} -illegal\n\
        end_design\n";

    let cb_p = dir.join("cb.cpf");
    let ct_p = dir.join("ct.cpf");
    std::fs::write(&cb_p, cb).unwrap();
    std::fs::write(&ct_p, ct_ill_pmx).unwrap();

    // 1. BUG_PM_X_ILLEGAL_NO_BRIDGE: root `-illegal` PM_X must not delete `u_b`'s `PM_X` row.
    let mut intent = load_upf_files(
        &[
            file_spec(cb_p.clone(), Some("u_b"), true),
            file_spec(ct_p, Some("/"), true),
        ],
        finder.clone(),
    )
    .unwrap();
    let res = resolve(&mut intent, finder.as_ref());
    let tbl_b = res
        .intent
        .pst_tables
        .iter()
        .find(|t| t.scope == "u_b")
        .expect("u_b PST");
    assert!(
        tbl_b
            .rows
            .iter()
            .any(|r| r.state_name == "PM_X" && r.source_scope.as_deref() == Some("u_b")),
        "u_b/PM_X must survive root PM_X -illegal: {tbl_b:#?}"
    );
    let mut sat = PstSat::build(&res, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat.is_state_space_consistent());
    assert!(
        sat.can_be_off_while_on(res.ss_by_name["SS_u_b/PD_B"], res.ss_by_name["SS_PD_TOP"])
            .is_some(),
        "u_b/PD_B must still turn off via u_b/PM_X"
    );

    // 2. BRIDGE_PLUS_PM_X_UPD_NOT_ILLEGAL / ATTACK_BRIDGED_UB_FIRST_ROOT_PM_X_OVERWRITE_NO_ILLEGAL:
    let ca = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_A} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_A -default\n\
        update_power_domain -name PD_A -primary_power_net VDD_A -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_KEEP_A -domain_conditions {PD_A@NC_ON} -default\n\
        create_power_mode -name PM_X -domain_conditions {PD_A@NC_OFF} -condition {sel_xa}\n\
        end_design\n";
    let root_bridge_pmx = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_BRIDGE -domain_conditions {u_a/PD_A@NC_ON u_b/PD_B@NC_ON} -default\n\
        create_power_mode -name PM_X -domain_conditions {u_a/PD_A@NC_ON u_b/PD_B@NC_ON} -condition {sel_top}\n\
        end_design\n";
    let ca_p = dir.join("ca.cpf");
    let br_p = dir.join("br.cpf");
    std::fs::write(&ca_p, ca).unwrap();
    std::fs::write(&br_p, root_bridge_pmx).unwrap();
    let mut intent_br = load_upf_files(
        &[
            file_spec(cb_p.clone(), Some("u_b"), true),
            file_spec(ca_p.clone(), Some("u_a"), true),
            file_spec(br_p.clone(), Some("/"), true),
        ],
        finder.clone(),
    )
    .unwrap();
    let res_br = resolve(&mut intent_br, finder.as_ref());
    let tbl_0 = &res_br.intent.pst_tables[0];
    assert!(
        tbl_0
            .rows
            .iter()
            .any(|r| r.state_name == "PM_X" && r.source_scope.as_deref() == Some("u_b")),
        "u_b/PM_X must not be overwritten by root PM_X: {tbl_0:#?}"
    );
    assert!(
        tbl_0
            .rows
            .iter()
            .any(|r| r.state_name == "PM_X" && r.source_scope.as_deref() == Some("")),
        "root PM_X must coexist with u_b/PM_X: {tbl_0:#?}"
    );
    let mut sat_br = PstSat::build(&res_br, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_br.is_state_space_consistent());
    assert!(
        sat_br
            .diagnostics()
            .iter()
            .all(|d| d.rule_id != "PST_UNREACHABLE_STATE"),
        "unexpected PST_UNREACHABLE_STATE: {:?}",
        sat_br.diagnostics()
    );
    assert!(sat_br
        .can_be_off_while_on(
            res_br.ss_by_name["SS_u_b/PD_B"],
            res_br.ss_by_name["SS_u_a/PD_A"]
        )
        .is_some());

    // Updating u_a/PM_X after PM_BRIDGE must update Table 1 in place without touching Table 0.
    let ua_upd = dir.join("ua_upd.cpf");
    std::fs::write(
        &ua_upd,
        "set_cpf_version 2.0\nupdate_power_mode -name PM_X -condition {sel_xa_new}\nend_design\n",
    )
    .unwrap();
    let mut intent_ua = load_upf_files(
        &[
            file_spec(cb_p, Some("u_b"), true),
            file_spec(ca_p, Some("u_a"), true),
            file_spec(br_p, Some("/"), true),
            file_spec(ua_upd, Some("u_a"), true),
        ],
        finder.clone(),
    )
    .unwrap();
    let res_ua = resolve(&mut intent_ua, finder.as_ref());
    let ua_rows_t0 = res_ua.intent.pst_tables[0]
        .rows
        .iter()
        .filter(|r| r.state_name == "PM_X" && r.source_scope.as_deref() == Some("u_a"))
        .count();
    let ua_rows_t1 = res_ua.intent.pst_tables[1]
        .rows
        .iter()
        .filter(|r| r.state_name == "PM_X" && r.source_scope.as_deref() == Some("u_a"))
        .count();
    assert_eq!(ua_rows_t0, 0);
    assert_eq!(ua_rows_t1, 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bug2_nested_cpf_in_cpf_inherits_power_modes_and_nc_refresh_while_keeping_parent_wins() {
    let c1_wrap = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_SUB -instances {u_sub}\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_SUB_ON -domain_conditions {PD_SUB@NC_ON} -condition {en_sub} -default\n\
        end_design\n";
    let c2_mix = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_MIX -domain_conditions {PD_TOP@NC_ON u_wrap/PD_SUB@NC_OFF} -condition {mix_cond}\n\
        create_power_mode -name PM_TOP_ON -domain_conditions {PD_TOP@NC_ON u_wrap/PD_SUB@NC_ON} -condition {pwr_en} -default\n\
        end_design\n";
    let c3_ill = "set_cpf_version 2.0\n\
        update_power_mode -name PM_MIX -illegal\n\
        end_design\n";

    // 1. ATTACK_NESTED_CPF_IN_CPF_CROSS_SCOPE_ILLEGAL (`master.cpf` -> c1, c2, c3)
    let (res_seq, res_nest) = run_seq_and_nested_mode(
        "r28_cpf_in_cpf_cross",
        &[
            ("c1.cpf", Some("u_wrap"), true, c1_wrap),
            ("c2.cpf", Some("/"), true, c2_mix),
            ("c3.cpf", Some("/"), true, c3_ill),
        ],
        "master.cpf",
        true,
        "set_cpf_version 2.0\n\
         load_upf \"c1.cpf\" -format cpf -scope u_wrap\n\
         load_upf \"c2.cpf\" -format cpf -scope /\n\
         load_upf \"c3.cpf\" -format cpf -scope /\n",
    );
    assert_eq!(
        ss_state_summary(&res_seq, "SS_u_wrap/PD_SUB"),
        ss_state_summary(&res_nest, "SS_u_wrap/PD_SUB")
    );
    let mut sat_nest = PstSat::build(&res_nest, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_nest.is_state_space_consistent());
    assert!(
        sat_nest
            .can_be_off_while_on(
                res_nest.ss_by_name["SS_u_wrap/PD_SUB"],
                res_nest.ss_by_name["SS_PD_TOP"]
            )
            .is_none(),
        "nested master.cpf must propagate update_power_mode -illegal"
    );

    // 2. NESTED_CPF_IN_CPF_PARENT_DEFINES_PM_CHILD_MARKS_ILLEGAL (both with and without end_design before c3)
    for (tag, end_before_c3) in [("no_end", ""), ("after_end", "end_design\n")] {
        let dir = temp_dir(&format!("r28_parent_pm_{tag}"));
        let finder = default_finder();
        let c1_p = dir.join("c1.cpf");
        let c3_p = dir.join("c3.cpf");
        std::fs::write(&c1_p, c1_wrap).unwrap();
        std::fs::write(&c3_p, c3_ill).unwrap();
        let master = format!(
            "set_cpf_version 2.0\n\
             load_upf \"{}\" -format cpf -scope u_wrap\n\
             create_power_nets -nets {{VDD_AON}} -voltage 0.9\n\
             create_ground_nets -nets {{VSS}}\n\
             create_power_domain -name PD_TOP -default\n\
             update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
             create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
             create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
             create_power_mode -name PM_MIX -domain_conditions {{PD_TOP@NC_ON u_wrap/PD_SUB@NC_OFF}} -condition {{mix_cond}}\n\
             create_power_mode -name PM_TOP_ON -domain_conditions {{PD_TOP@NC_ON u_wrap/PD_SUB@NC_ON}} -condition {{pwr_en}} -default\n\
             {end_before_c3}\
             load_upf \"{}\" -format cpf -scope /\n",
            c1_p.display(),
            c3_p.display()
        );
        let res = parse_cpf_and_resolve(&master, None, None, finder).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let mut sat = PstSat::build(&res, DEFAULT_VOLTAGE_TOL_UV);
        assert!(sat.is_state_space_consistent());
        assert!(
            sat.can_be_off_while_on(
                res.ss_by_name["SS_u_wrap/PD_SUB"],
                res.ss_by_name["SS_PD_TOP"]
            )
            .is_none(),
            "{tag}: child c3.cpf must mark parent PM_MIX illegal"
        );
    }

    // 3. SUSP2B_CPF_IN_CPF_NC_REFRESH: explicit `-scope /` refreshes parent-authored NC_OFF.
    let dir_nc = temp_dir("r28_susp2b_nc");
    let finder_nc = default_finder();
    let c3_nc = dir_nc.join("c3_nc.cpf");
    std::fs::write(
        &c3_nc,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.9 -state on\nend_design\n",
    )
    .unwrap();
    let top_nc = format!(
        "set_cpf_version 2.0\n\
         create_power_nets -nets {{VDD_AON VDD_SUB}} -voltage 0.9\n\
         create_ground_nets -nets {{VSS}}\n\
         create_power_domain -name PD_TOP -default\n\
         create_power_domain -name PD_SUB -instances {{u_sub}}\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
         update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
         create_power_mode -name PM_OFF -domain_conditions {{PD_TOP@NC_ON PD_SUB@NC_OFF}} -condition {{!pwr_en}}\n\
         create_power_mode -name PM_ON -domain_conditions {{PD_TOP@NC_ON PD_SUB@NC_ON}} -condition {{pwr_en}} -default\n\
         end_design\n\
         load_upf \"{}\" -format cpf -scope /\n",
        c3_nc.display()
    );
    let res_nc = parse_cpf_and_resolve(&top_nc, None, None, finder_nc).unwrap();
    let _ = std::fs::remove_dir_all(&dir_nc);
    assert_eq!(
        ss_state_summary(&res_nc, "SS_PD_SUB"),
        vec![
            ("NC_OFF".to_string(), Some(SimState::Normal), false),
            ("NC_ON".to_string(), Some(SimState::Normal), false),
        ]
    );
}

#[test]
fn bug3_prune_orphaned_mode_state_despite_update_overlay_and_keep_dup_non_upd() {
    let c1_sh = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_sub} -shutoff_condition {!pwr_en}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        end_design\n";
    let c2_only = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_ONLY -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON}\n\
        end_design\n";
    let c2_only_direct_ill = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_ONLY -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -illegal\n\
        end_design\n";
    let u_part = "upf_version 2.1\n\
        add_power_state SS_PD_SUB -state {NC_ON -update -logic_expr {new_en}}\n";
    let c3_ill_only = "set_cpf_version 2.0\nupdate_power_mode -name PM_ONLY -illegal\nend_design\n";

    // 1. SUSP1B_upd_ill_after_upf == SUSP1B_direct_ill
    let (ctrl_seq, ctrl_nest) = run_seq_and_nested_mode(
        "r28_susp1b_ctrl",
        &[
            ("c1.cpf", None, true, c1_sh),
            ("c2.cpf", Some("/"), true, c2_only_direct_ill),
            ("u.upf", Some("/"), false, u_part),
        ],
        "top.upf",
        false,
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf\nload_upf \"c2.cpf\" -format cpf\nload_upf \"u.upf\"\n",
    );
    let (upd_seq, upd_nest) = run_seq_and_nested_mode(
        "r28_susp1b_upd",
        &[
            ("c1.cpf", None, true, c1_sh),
            ("c2.cpf", Some("/"), true, c2_only),
            ("u.upf", Some("/"), false, u_part),
            ("c3.cpf", Some("/"), true, c3_ill_only),
        ],
        "top.upf",
        false,
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf\nload_upf \"c2.cpf\" -format cpf\nload_upf \"u.upf\"\nload_upf \"c3.cpf\" -format cpf\n",
    );
    assert_eq!(
        ss_state_summary(&ctrl_seq, "SS_PD_SUB"),
        ss_state_summary(&ctrl_nest, "SS_PD_SUB")
    );
    assert_eq!(
        ss_state_summary(&upd_seq, "SS_PD_SUB"),
        ss_state_summary(&upd_nest, "SS_PD_SUB")
    );
    assert_eq!(
        ss_state_summary(&upd_seq, "SS_PD_SUB"),
        ss_state_summary(&ctrl_seq, "SS_PD_SUB")
    );
    let mut sat_upd = PstSat::build(&upd_seq, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_upd.is_state_space_consistent());
    assert!(
        sat_upd
            .can_be_off_while_on(
                upd_seq.ss_by_name["SS_PD_SUB"],
                upd_seq.ss_by_name["SS_PD_TOP"]
            )
            .is_some(),
        "fallback OFF must apply once PM_ONLY is illegal"
    );

    // 2. SUSP1_UPD_PM_ILL_AFTER_UPF_UPD == SUSP1_CTRL_DIRECT_ILL_PLUS_UPF_UPD
    let c1_nosh = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_sub}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        end_design\n";
    let c2_two = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF}\n\
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -default\n\
        end_design\n";
    let c2_two_direct = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF} -illegal\n\
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -default\n\
        end_design\n";
    let u_off_upd = "upf_version 2.1\n\
        add_power_state SS_PD_SUB -state {NC_OFF -update -logic_expr {!other_en}}\n";
    let c3_ill_off = "set_cpf_version 2.0\nupdate_power_mode -name PM_OFF -illegal\nend_design\n";

    let (s1_ctrl_s, s1_ctrl_n) = run_seq_and_nested_mode(
        "r28_susp1_ctrl",
        &[
            ("c1.cpf", None, true, c1_nosh),
            ("c2.cpf", Some("/"), true, c2_two_direct),
            ("u.upf", Some("/"), false, u_off_upd),
        ],
        "top.upf",
        false,
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf\nload_upf \"c2.cpf\" -format cpf\nload_upf \"u.upf\"\n",
    );
    let (s1_upd_s, s1_upd_n) = run_seq_and_nested_mode(
        "r28_susp1_upd",
        &[
            ("c1.cpf", None, true, c1_nosh),
            ("c2.cpf", Some("/"), true, c2_two),
            ("u.upf", Some("/"), false, u_off_upd),
            ("c3.cpf", Some("/"), true, c3_ill_off),
        ],
        "top.upf",
        false,
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf\nload_upf \"c2.cpf\" -format cpf\nload_upf \"u.upf\"\nload_upf \"c3.cpf\" -format cpf\n",
    );
    assert_eq!(
        ss_state_summary(&s1_ctrl_s, "SS_PD_SUB"),
        ss_state_summary(&s1_ctrl_n, "SS_PD_SUB")
    );
    assert_eq!(
        ss_state_summary(&s1_upd_s, "SS_PD_SUB"),
        ss_state_summary(&s1_upd_n, "SS_PD_SUB")
    );
    assert_eq!(
        ss_state_summary(&s1_upd_s, "SS_PD_SUB"),
        ss_state_summary(&s1_ctrl_s, "SS_PD_SUB")
    );
    assert!(
        !s1_upd_s.ss_states[s1_upd_s.ss_by_name["SS_PD_SUB"].0 as usize]
            .iter()
            .any(|s| s.simstate == Some(SimState::Corrupt)),
        "orphaned Corrupt NC_OFF must be pruned"
    );

    // 3. Duplicate non-`-update` UPF state after legal mode before `update_power_mode -illegal`:
    let u_dup_non_upd = "upf_version 2.1\n\
        add_power_state SS_PD_SUB -state {NC_OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {!other_en} -simstate CORRUPT}\n";
    let (dup_ctrl, _) = run_seq_and_nested_mode(
        "r28_dup_ctrl",
        &[
            ("c1.cpf", None, true, c1_nosh),
            ("c2.cpf", Some("/"), true, c2_two_direct),
            ("u.upf", Some("/"), false, u_dup_non_upd),
        ],
        "top.upf",
        false,
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf\nload_upf \"c2.cpf\" -format cpf\nload_upf \"u.upf\"\n",
    );
    let (dup_seq, dup_nest) = run_seq_and_nested_mode(
        "r28_dup_seq",
        &[
            ("c1.cpf", None, true, c1_nosh),
            ("c2.cpf", Some("/"), true, c2_two),
            ("u.upf", Some("/"), false, u_dup_non_upd),
            ("c3.cpf", Some("/"), true, c3_ill_off),
        ],
        "top.upf",
        false,
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf\nload_upf \"c2.cpf\" -format cpf\nload_upf \"u.upf\"\nload_upf \"c3.cpf\" -format cpf\n",
    );
    assert_eq!(
        ss_state_summary(&dup_seq, "SS_PD_SUB"),
        ss_state_summary(&dup_nest, "SS_PD_SUB")
    );
    assert_eq!(
        ss_state_summary(&dup_seq, "SS_PD_SUB"),
        ss_state_summary(&dup_ctrl, "SS_PD_SUB")
    );
}

#[test]
fn r29_bug_a_unemitted_parent_pm_survives_scoped_mid_to_root_c3() {
    let dir = temp_dir("r29_bug_a_multihop");
    let finder = default_finder();
    let c3 = "set_cpf_version 2.0\nupdate_power_mode -name PM_MIX -illegal\nend_design\n";
    let c3_p = dir.join("c3.cpf");
    std::fs::write(&c3_p, c3).unwrap();

    let mid_only = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_MID} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_MID -instances {u_sub}\n\
        update_power_domain -name PD_MID -primary_power_net VDD_MID -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_MID_ON -domain_conditions {PD_MID@NC_ON} -condition {en_mid} -default\n\
        end_design\n";
    let mid_nest = format!(
        "set_cpf_version 2.0\n\
         create_power_nets -nets {{VDD_MID}} -voltage 0.9\n\
         create_ground_nets -nets {{VSS}}\n\
         create_power_domain -name PD_MID -instances {{u_sub}}\n\
         update_power_domain -name PD_MID -primary_power_net VDD_MID -primary_ground_net VSS\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_power_mode -name PM_MID_ON -domain_conditions {{PD_MID@NC_ON}} -condition {{en_mid}} -default\n\
         load_upf \"{}\" -format cpf -scope /\n\
         end_design\n",
        c3_p.display()
    );
    let master_body = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_MIX -domain_conditions {PD_TOP@NC_ON u_wrap/PD_MID@NC_OFF} -condition {mix_cond}\n\
        create_power_mode -name PM_TOP_ON -domain_conditions {PD_TOP@NC_ON u_wrap/PD_MID@NC_ON} -condition {pwr_en} -default\n";

    let mid_only_p = dir.join("mid_only.cpf");
    let mid_nest_p = dir.join("mid.cpf");
    let master_seq_p = dir.join("master_seq.cpf");
    let master_nest_p = dir.join("master_nest.cpf");
    std::fs::write(&mid_only_p, mid_only).unwrap();
    std::fs::write(&mid_nest_p, &mid_nest).unwrap();
    std::fs::write(&master_seq_p, format!("{master_body}end_design\n")).unwrap();
    std::fs::write(
        &master_nest_p,
        format!(
            "{master_body}load_upf \"{}\" -format cpf -scope u_wrap\nend_design\n",
            mid_nest_p.display()
        ),
    )
    .unwrap();

    let mut intent_seq = load_upf_files(
        &[
            file_spec(master_seq_p, Some("/"), true),
            file_spec(mid_only_p, Some("u_wrap"), true),
            file_spec(c3_p, Some("/"), true),
        ],
        finder.clone(),
    )
    .unwrap();
    let res_seq = resolve(&mut intent_seq, finder.as_ref());
    let mut intent_nest =
        load_upf_files(&[file_spec(master_nest_p, None, true)], finder.clone()).unwrap();
    let res_nest = resolve(&mut intent_nest, finder.as_ref());
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        ss_state_summary(&res_seq, "SS_u_wrap/PD_MID"),
        ss_state_summary(&res_nest, "SS_u_wrap/PD_MID")
    );
    let mut sat_nest = PstSat::build(&res_nest, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_nest.is_state_space_consistent());
    assert!(
        sat_nest
            .can_be_off_while_on(
                res_nest.ss_by_name["SS_u_wrap/PD_MID"],
                res_nest.ss_by_name["SS_PD_TOP"]
            )
            .is_none(),
        "multi-hop master(/) -> mid(u_wrap) -> c3(/) must propagate PM_MIX -illegal"
    );
    assert!(
        res_nest
            .intent
            .pst_tables
            .iter()
            .any(|t| t.rows.iter().any(|r| r.state_name == "PM_MID_ON")),
        "mid.cpf PM_MID_ON must still be emitted: {:#?}",
        res_nest.intent.pst_tables
    );
}

#[test]
fn r29_bug_b_same_scope_child_set_design_propagates_and_scoped_remains_isolated() {
    let m0 = "set_cpf_version 2.0\nset_design top_master\n";
    let c1_no_end = "set_cpf_version 2.0\n\
        set_design sub_a\n\
        create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_MIX -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF} -condition {mix_cond}\n\
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -condition {pwr_en} -default\n";
    let c3 = "set_cpf_version 2.0\nupdate_power_mode -name PM_MIX -illegal\nend_design\n";

    let (res_seq, res_nest_cpf) = run_seq_and_nested_mode(
        "r29_bug_b_cpf",
        &[
            ("m0.cpf", None, true, m0),
            ("c1.cpf", Some("/"), true, c1_no_end),
            ("c3.cpf", Some("/"), true, c3),
        ],
        "master.cpf",
        true,
        "set_cpf_version 2.0\n\
         set_design top_master\n\
         load_upf \"c1.cpf\" -format cpf -scope /\n\
         load_upf \"c3.cpf\" -format cpf -scope /\n\
         end_design\n",
    );
    let (_, res_nest_upf) = run_seq_and_nested_mode(
        "r29_bug_b_upf",
        &[
            ("m0.cpf", None, true, m0),
            ("c1.cpf", Some("/"), true, c1_no_end),
            ("c3.cpf", Some("/"), true, c3),
        ],
        "master.upf",
        false,
        "upf_version 2.1\n\
         load_upf \"m0.cpf\" -format cpf\n\
         load_upf \"c1.cpf\" -format cpf -scope /\n\
         load_upf \"c3.cpf\" -format cpf -scope /\n",
    );
    assert_eq!(
        ss_state_summary(&res_seq, "SS_PD_SUB"),
        ss_state_summary(&res_nest_cpf, "SS_PD_SUB")
    );
    assert_eq!(
        ss_state_summary(&res_nest_upf, "SS_PD_SUB"),
        ss_state_summary(&res_nest_cpf, "SS_PD_SUB")
    );
    let mut sat_cpf = PstSat::build(&res_nest_cpf, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_cpf.is_state_space_consistent());
    assert!(
        sat_cpf
            .can_be_off_while_on(
                res_nest_cpf.ss_by_name["SS_PD_SUB"],
                res_nest_cpf.ss_by_name["SS_PD_TOP"]
            )
            .is_none(),
        "nested CPF-in-CPF same-scope set_design without end_design must propagate active_design"
    );

    // Scoped child set_design and same-scope child with explicit end_design must preserve parent design_top.
    let dir = temp_dir("r29_design_top_preserve");
    let finder = default_finder();
    let wrap_p = dir.join("wrap.cpf");
    let bogus_p = dir.join("bogus.cpf");
    let c3_p = dir.join("c3.cpf");
    std::fs::write(&wrap_p, "set_cpf_version 2.0\nset_design wrap_blk\n").unwrap();
    std::fs::write(
        &bogus_p,
        "set_cpf_version 2.0\nset_design bogus_child\nend_design\n",
    )
    .unwrap();
    std::fs::write(&c3_p, c3).unwrap();
    let top_preserve = format!(
        "set_cpf_version 2.0\n\
         set_design top\n\
         create_power_nets -nets {{VDD_AON VDD_SUB}} -voltage 0.9\n\
         create_ground_nets -nets {{VSS}}\n\
         create_power_domain -name PD_TOP -default\n\
         create_power_domain -name PD_SUB -instances {{u_wrap/u_sub}}\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
         update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
         create_power_mode -name PM_MIX -domain_conditions {{PD_TOP@NC_ON PD_SUB@NC_OFF}} -condition {{mix_cond}}\n\
         create_power_mode -name PM_ON -domain_conditions {{PD_TOP@NC_ON PD_SUB@NC_ON}} -condition {{pwr_en}} -default\n\
         load_upf \"{}\" -format cpf -scope u_wrap\n\
         load_upf \"{}\" -format cpf -scope /\n\
         load_upf \"{}\" -format cpf -scope /\n\
         end_design\n",
        wrap_p.display(),
        bogus_p.display(),
        c3_p.display()
    );
    let res_pres = parse_cpf_and_resolve(&top_preserve, None, None, finder).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let mut sat_pres = PstSat::build(&res_pres, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat_pres.is_state_space_consistent());
    assert!(
        sat_pres
            .can_be_off_while_on(
                res_pres.ss_by_name["SS_PD_SUB"],
                res_pres.ss_by_name["SS_PD_TOP"]
            )
            .is_none(),
        "scoped/ended child set_design must preserve parent design_top `top`"
    );
}

#[test]
fn r30_bug_c_multihop_explicit_scope_nc_override_and_implicit_parent_wins() {
    let dir = temp_dir("r30_bug_c_nc");
    let finder: Arc<dyn ObjectFinder> = Arc::new(MockDesignFinder::new(
        &[
            "u_wrap",
            "u_sub",
            "u_wrap/u_sub",
            "u_wrap/u_inner",
            "u_wrap/u_inner/u_sub",
        ],
        &[
            "u_wrap",
            "u_sub",
            "u_wrap/u_sub",
            "u_wrap/u_inner",
            "u_wrap/u_inner/u_sub",
        ],
    ));
    let c3_nc_p = dir.join("c3_nc.cpf");
    std::fs::write(
        &c3_nc_p,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.9 -state on\nend_design\n",
    )
    .unwrap();
    let mid_p = dir.join("mid.cpf");
    std::fs::write(
        &mid_p,
        format!(
            "set_cpf_version 2.0\n\
             create_power_nets -nets {{VDD_MID}} -voltage 0.9\n\
             create_ground_nets -nets {{VSS}}\n\
             create_power_domain -name PD_MID -instances {{u_sub}}\n\
             update_power_domain -name PD_MID -primary_power_net VDD_MID -primary_ground_net VSS\n\
             create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
             create_power_mode -name PM_MID_ON -domain_conditions {{PD_MID@NC_ON}} -condition {{en_mid}} -default\n\
             load_upf \"{}\" -format cpf -scope /\n\
             end_design\n",
            c3_nc_p.display()
        ),
    )
    .unwrap();
    let master_head = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_MIX -domain_conditions {PD_TOP@NC_ON u_wrap/PD_MID@NC_OFF} -condition {mix_cond}\n\
        create_power_mode -name PM_TOP_ON -domain_conditions {PD_TOP@NC_ON u_wrap/PD_MID@NC_ON} -condition {pwr_en} -default\n";
    for (tag, pre, post) in [
        ("unemitted_end", "", "end_design\n"),
        ("unemitted_noend", "", ""),
        ("emitted_before", "end_design\n", ""),
    ] {
        let m_p = dir.join(format!("m_{tag}.cpf"));
        std::fs::write(
            &m_p,
            format!(
                "{master_head}{pre}load_upf \"{}\" -format cpf -scope u_wrap\n{post}",
                mid_p.display()
            ),
        )
        .unwrap();
        let mut intent = load_upf_files(&[file_spec(m_p, None, true)], finder.clone()).unwrap();
        let res = resolve(&mut intent, finder.as_ref());
        assert_eq!(
            ss_state_summary(&res, "SS_u_wrap/PD_MID"),
            vec![
                ("NC_OFF".to_string(), Some(SimState::Normal), false),
                ("NC_ON".to_string(), Some(SimState::Normal), false),
            ],
            "{tag}: explicit -scope / NC_OFF=0.9V/on must overwrite parent across mid(u_wrap)"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
