mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{temp_dir, MockDesignFinder};
use informal_upf::{
    load_upf_files, parse_cpf_and_resolve, resolve, ObjectFinder, PstSat, ResolvedPowerIntent,
    SimState, UpfFileSpec, DEFAULT_VOLTAGE_TOL_UV,
};

fn finder_4hop() -> Arc<dyn ObjectFinder> {
    Arc::new(MockDesignFinder::new(
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
fn r30_four_hop_explicit_scope_nc_override_and_mixed_upf_cpf_chains() {
    let dir = temp_dir("r30_4hop_mixed");
    let finder = finder_4hop();
    let c3_nc = dir.join("c3_nc.cpf");
    std::fs::write(
        &c3_nc,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.9 -state on\nend_design\n",
    )
    .unwrap();

    // 1. 4-hop CPF chain: master(/) -> mid1(u_wrap) -> mid2(u_inner) -> c3_nc(/)
    let mid2 = dir.join("mid2.cpf");
    std::fs::write(
        &mid2,
        format!(
            "set_cpf_version 2.0\n\
             create_power_nets -nets {{VDD_IN}} -voltage 0.9\n\
             create_ground_nets -nets {{VSS}}\n\
             create_power_domain -name PD_IN -instances {{u_sub}}\n\
             update_power_domain -name PD_IN -primary_power_net VDD_IN -primary_ground_net VSS\n\
             create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
             create_power_mode -name PM_IN_ON -domain_conditions {{PD_IN@NC_ON}} -condition {{en_in}} -default\n\
             load_upf \"{}\" -format cpf -scope /\n\
             end_design\n",
            c3_nc.display()
        ),
    )
    .unwrap();
    let mid1 = dir.join("mid1.cpf");
    std::fs::write(
        &mid1,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf -scope u_inner\n",
            mid2.display()
        ),
    )
    .unwrap();
    let master4 = format!(
        "set_cpf_version 2.0\n\
         create_power_nets -nets {{VDD_AON}} -voltage 0.9\n\
         create_ground_nets -nets {{VSS}}\n\
         create_power_domain -name PD_TOP -default\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
         create_power_mode -name PM_MIX -domain_conditions {{PD_TOP@NC_ON u_wrap/u_inner/PD_IN@NC_OFF}} -condition {{mix_cond}}\n\
         create_power_mode -name PM_TOP_ON -domain_conditions {{PD_TOP@NC_ON u_wrap/u_inner/PD_IN@NC_ON}} -condition {{pwr_en}} -default\n\
         load_upf \"{}\" -format cpf -scope u_wrap\n\
         end_design\n",
        mid1.display()
    );
    let res4 = parse_cpf_and_resolve(&master4, None, None, finder.clone()).unwrap();
    assert_eq!(
        ss_state_summary(&res4, "SS_u_wrap/u_inner/PD_IN"),
        vec![
            ("NC_OFF".to_string(), Some(SimState::Normal), false),
            ("NC_ON".to_string(), Some(SimState::Normal), false),
        ]
    );
    let mut sat4 = PstSat::build(&res4, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat4.is_state_space_consistent());
    assert!(sat4
        .can_be_off_while_on(
            res4.ss_by_name["SS_u_wrap/u_inner/PD_IN"],
            res4.ss_by_name["SS_PD_TOP"]
        )
        .is_none());

    // 2. Mixed CPF -> UPF(-scope u_wrap) -> CPF(-scope /) chain
    let upf_mid = dir.join("upf_mid.upf");
    std::fs::write(
        &upf_mid,
        format!(
            "upf_version 2.1\n\
             create_power_domain PD_MID -elements {{u_sub}}\n\
             load_upf \"{}\" -format cpf -scope /\n",
            c3_nc.display()
        ),
    )
    .unwrap();
    let master_mixed = format!(
        "set_cpf_version 2.0\n\
         create_power_nets -nets {{VDD_AON}} -voltage 0.9\n\
         create_ground_nets -nets {{VSS}}\n\
         create_power_domain -name PD_TOP -default\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
         create_power_mode -name PM_MIX -domain_conditions {{PD_TOP@NC_ON u_wrap/PD_MID@NC_OFF}} -condition {{mix_cond}}\n\
         create_power_mode -name PM_TOP_ON -domain_conditions {{PD_TOP@NC_ON u_wrap/PD_MID@NC_ON}} -condition {{pwr_en}} -default\n\
         end_design\n\
         load_upf \"{}\" -scope u_wrap\n",
        upf_mid.display()
    );
    let res_mixed = parse_cpf_and_resolve(&master_mixed, None, None, finder.clone()).unwrap();
    assert_eq!(
        ss_state_summary(&res_mixed, "SS_u_wrap/PD_MID"),
        vec![
            ("NC_OFF".to_string(), Some(SimState::Normal), false),
            ("NC_ON".to_string(), Some(SimState::Normal), false),
        ]
    );

    // 3. Reverse UPF -> CPF(-scope u_wrap) -> UPF(-scope u_inner) -> CPF(-scope /)
    let mid_upf2 = dir.join("mid_upf2.upf");
    std::fs::write(
        &mid_upf2,
        format!(
            "upf_version 2.1\nload_upf \"{}\" -format cpf -scope /\n",
            c3_nc.display()
        ),
    )
    .unwrap();
    let mid_cpf1 = dir.join("mid_cpf1.cpf");
    std::fs::write(
        &mid_cpf1,
        format!(
            "set_cpf_version 2.0\n\
             create_power_nets -nets {{VDD_MID}} -voltage 0.9\n\
             create_ground_nets -nets {{VSS}}\n\
             create_power_domain -name PD_MID -instances {{u_inner/u_sub}}\n\
             update_power_domain -name PD_MID -primary_power_net VDD_MID -primary_ground_net VSS\n\
             create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
             create_power_mode -name PM_MID_ON -domain_conditions {{PD_MID@NC_ON}} -condition {{en_mid}} -default\n\
             load_upf \"{}\" -scope u_inner\n\
             end_design\n",
            mid_upf2.display()
        ),
    )
    .unwrap();
    let base_cpf = dir.join("base.cpf");
    std::fs::write(
        &base_cpf,
        "set_cpf_version 2.0\n\
         create_power_nets -nets {VDD_AON} -voltage 0.9\n\
         create_ground_nets -nets {VSS}\n\
         create_power_domain -name PD_TOP -default\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
         create_power_mode -name PM_MIX -domain_conditions {PD_TOP@NC_ON u_wrap/PD_MID@NC_OFF} -condition {mix_cond}\n\
         create_power_mode -name PM_TOP_ON -domain_conditions {PD_TOP@NC_ON u_wrap/PD_MID@NC_ON} -condition {pwr_en} -default\n\
         end_design\n",
    )
    .unwrap();
    let top_upf = dir.join("top.upf");
    std::fs::write(
        &top_upf,
        format!(
            "upf_version 2.1\nload_upf \"{}\" -format cpf -scope /\nload_upf \"{}\" -format cpf -scope u_wrap\n",
            base_cpf.display(),
            mid_cpf1.display()
        ),
    )
    .unwrap();
    let mut intent_rev =
        load_upf_files(&[file_spec(top_upf, None, false)], finder.clone()).unwrap();
    let res_rev = resolve(&mut intent_rev, finder.as_ref());
    assert_eq!(
        ss_state_summary(&res_rev, "SS_u_wrap/PD_MID"),
        vec![
            ("NC_OFF".to_string(), Some(SimState::Normal), false),
            ("NC_ON".to_string(), Some(SimState::Normal), false),
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn r30_preserved_fix3_implicit_same_scope_parent_wins_across_hops_and_explicit_siblings() {
    let dir = temp_dir("r30_fix3_hops");
    let finder = finder_4hop();

    // 1. Multi-hop implicit same-scope children (no -scope) must NOT overwrite parent NC_ON.
    let imp2 = dir.join("imp2.cpf");
    std::fs::write(
        &imp2,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_ON -voltage 0.0 -state off\nend_design\n",
    )
    .unwrap();
    let imp1 = dir.join("imp1.cpf");
    std::fs::write(
        &imp1,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf\n",
            imp2.display()
        ),
    )
    .unwrap();
    let master_imp = format!(
        "set_cpf_version 2.0\n\
         create_power_nets -nets {{VDD_AON}} -voltage 0.9\n\
         create_ground_nets -nets {{VSS}}\n\
         create_power_domain -name PD_TOP -default -shutoff_condition {{!pwr_en}}\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_power_mode -name PM_TOP -domain_conditions {{PD_TOP@NC_ON}} -condition {{pwr_en}} -default\n\
         load_upf \"{}\" -format cpf\n\
         end_design\n",
        imp1.display()
    );
    let res_imp = parse_cpf_and_resolve(&master_imp, None, None, finder.clone()).unwrap();
    assert_eq!(
        ss_state_summary(&res_imp, "SS_PD_TOP"),
        vec![("NC_ON".to_string(), Some(SimState::Normal), false)]
    );
    assert!(PstSat::build(&res_imp, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent());

    // 2. Explicit -scope / override followed by an untouched implicit child preserves the override,
    //    while a subsequent implicit child directly authoring NC_OFF is still blocked by parent-wins.
    let c3_explicit = dir.join("c3_exp.cpf");
    std::fs::write(
        &c3_explicit,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.9 -state on\nend_design\n",
    )
    .unwrap();
    let mid_exp = dir.join("mid_exp.cpf");
    std::fs::write(
        &mid_exp,
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
            c3_explicit.display()
        ),
    )
    .unwrap();
    let imp_after = dir.join("imp_after.cpf");
    std::fs::write(
        &imp_after,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.0 -state off\nend_design\n",
    )
    .unwrap();
    let master_seq_check = format!(
        "set_cpf_version 2.0\n\
         create_power_nets -nets {{VDD_AON}} -voltage 0.9\n\
         create_ground_nets -nets {{VSS}}\n\
         create_power_domain -name PD_TOP -default\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
         create_power_mode -name PM_MIX -domain_conditions {{PD_TOP@NC_ON u_wrap/PD_MID@NC_OFF}} -condition {{mix_cond}}\n\
         create_power_mode -name PM_TOP_ON -domain_conditions {{PD_TOP@NC_ON u_wrap/PD_MID@NC_ON}} -condition {{pwr_en}} -default\n\
         load_upf \"{}\" -format cpf -scope u_wrap\n\
         load_upf \"{}\" -format cpf\n\
         end_design\n",
        mid_exp.display(),
        imp_after.display()
    );
    let res_after = parse_cpf_and_resolve(&master_seq_check, None, None, finder).unwrap();
    assert_eq!(
        ss_state_summary(&res_after, "SS_u_wrap/PD_MID"),
        vec![
            ("NC_OFF".to_string(), Some(SimState::Normal), false),
            ("NC_ON".to_string(), Some(SimState::Normal), false),
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn r31_bug_d_implicit_child_under_explicit_scope_mid_preserves_parent_and_badges() {
    let dir = temp_dir("r31_bug_d");
    let finder = finder_4hop();

    let c_imp_on = dir.join("c_imp_on.cpf");
    std::fs::write(
        &c_imp_on,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.9 -state on\nend_design\n",
    )
    .unwrap();
    let mid_slash_imp = dir.join("mid_slash_imp.cpf");
    std::fs::write(
        &mid_slash_imp,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf\nend_design\n",
            c_imp_on.display()
        ),
    )
    .unwrap();
    let mid_wrap_then_slash = dir.join("mid_wrap_then_slash.cpf");
    std::fs::write(
        &mid_wrap_then_slash,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf -scope /\n",
            mid_slash_imp.display()
        ),
    )
    .unwrap();

    let p1_head = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF} -condition {!pwr_en}\n\
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -condition {pwr_en} -default\n";

    // P1a (direct implicit), P1b (mid -scope / -> c_imp), P1d (mid1 -scope u_wrap -> mid2 -scope / -> c_imp)
    for (tag, child_path, scope_flag) in [
        ("P1a", &c_imp_on, ""),
        ("P1b", &mid_slash_imp, "-scope /"),
        ("P1d", &mid_wrap_then_slash, "-scope u_wrap"),
    ] {
        let m = format!(
            "{p1_head}load_upf \"{}\" -format cpf {scope_flag}\nend_design\n",
            child_path.display()
        );
        let res = parse_cpf_and_resolve(&m, None, None, finder.clone()).unwrap();
        assert_eq!(
            ss_state_summary(&res, "SS_PD_SUB"),
            vec![
                ("NC_OFF".to_string(), Some(SimState::Corrupt), false),
                ("NC_ON".to_string(), Some(SimState::Normal), false),
            ],
            "{tag}: implicit child must not overwrite master NC_OFF=0.0V/off"
        );
    }

    // P2a == P1e == P3c (NC_ON=0.9/on preserved across implicit child under CPF/UPF -scope /)
    let c_imp_off = dir.join("c_imp_off.cpf");
    std::fs::write(
        &c_imp_off,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_ON -voltage 0.0 -state off\nend_design\n",
    )
    .unwrap();
    let mid_cpf_on = dir.join("mid_cpf_on.cpf");
    std::fs::write(
        &mid_cpf_on,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf\n",
            c_imp_off.display()
        ),
    )
    .unwrap();
    let mid_upf_on = dir.join("mid_upf_on.upf");
    std::fs::write(
        &mid_upf_on,
        format!(
            "upf_version 2.1\nload_upf \"{}\" -format cpf\n",
            c_imp_off.display()
        ),
    )
    .unwrap();
    let p2_head = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_TOP -domain_conditions {PD_TOP@NC_ON} -condition {pwr_en} -default\n";
    for (tag, body) in [
        (
            "P2a",
            format!(
                "{p2_head}load_upf \"{}\" -format cpf\nend_design\n",
                c_imp_off.display()
            ),
        ),
        (
            "P1e",
            format!(
                "{p2_head}load_upf \"{}\" -format cpf -scope /\nend_design\n",
                mid_cpf_on.display()
            ),
        ),
        (
            "P3c_noend",
            format!(
                "{p2_head}load_upf \"{}\" -scope /\nend_design\n",
                mid_upf_on.display()
            ),
        ),
        (
            "P3c_emitted",
            format!(
                "{p2_head}end_design\nload_upf \"{}\" -scope /\n",
                mid_upf_on.display()
            ),
        ),
    ] {
        let res = parse_cpf_and_resolve(&body, None, None, finder.clone()).unwrap();
        assert_eq!(
            ss_state_summary(&res, "SS_PD_TOP"),
            vec![("NC_ON".to_string(), Some(SimState::Normal), false)],
            "{tag}: NC_ON must remain 0.9V/Normal"
        );
        assert!(
            PstSat::build(&res, DEFAULT_VOLTAGE_TOL_UV).is_state_space_consistent(),
            "{tag}: PST must remain consistent"
        );
    }

    // P4b == P4d (scoped NC_OFF at u_wrap preserved across mid2 -scope /u_wrap -> sub_imp) vs P4c
    let sub_imp_on = dir.join("sub_imp_on.cpf");
    std::fs::write(
        &sub_imp_on,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.9 -state on\nend_design\n",
    )
    .unwrap();
    let mid2_uwrap = dir.join("mid2_uwrap.cpf");
    std::fs::write(
        &mid2_uwrap,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf\n",
            sub_imp_on.display()
        ),
    )
    .unwrap();
    let mid_wrap_head = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_SUB -instances {u_sub}\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_SUB_OFF -domain_conditions {PD_SUB@NC_OFF} -condition {!sub_en}\n\
        create_power_mode -name PM_SUB_ON -domain_conditions {PD_SUB@NC_ON} -condition {sub_en} -default\n";
    let mid_wrap_p4b = dir.join("mid_wrap_p4b.cpf");
    std::fs::write(
        &mid_wrap_p4b,
        format!(
            "{mid_wrap_head}load_upf \"{}\" -format cpf\nend_design\n",
            sub_imp_on.display()
        ),
    )
    .unwrap();
    let mid_wrap_p4d = dir.join("mid_wrap_p4d.cpf");
    std::fs::write(
        &mid_wrap_p4d,
        format!(
            "{mid_wrap_head}load_upf \"{}\" -format cpf -scope /u_wrap\nend_design\n",
            mid2_uwrap.display()
        ),
    )
    .unwrap();
    let mid_wrap_p4c = dir.join("mid_wrap_p4c.cpf");
    std::fs::write(
        &mid_wrap_p4c,
        format!(
            "{mid_wrap_head}load_upf \"{}\" -format cpf -scope /u_wrap\nend_design\n",
            sub_imp_on.display()
        ),
    )
    .unwrap();
    let root_wrap = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_TOP -domain_conditions {PD_TOP@NC_ON} -condition {pwr_en} -default\n";
    for (tag, mw, exp_off) in [
        ("P4b", &mid_wrap_p4b, SimState::Corrupt),
        ("P4d", &mid_wrap_p4d, SimState::Corrupt),
        ("P4c", &mid_wrap_p4c, SimState::Normal),
    ] {
        let m = format!(
            "{root_wrap}load_upf \"{}\" -format cpf -scope u_wrap\nend_design\n",
            mw.display()
        );
        let res = parse_cpf_and_resolve(&m, None, None, finder.clone()).unwrap();
        assert_eq!(
            ss_state_summary(&res, "SS_u_wrap/PD_SUB"),
            vec![
                ("NC_OFF".to_string(), Some(exp_off), false),
                ("NC_ON".to_string(), Some(SimState::Normal), false),
            ],
            "{tag}: unexpected u_wrap/PD_SUB NC_OFF state"
        );
    }

    // E2 (implicit_mid -> mid(-scope u_wrap) -> c3(-scope /)) & ADV_IMPLICIT_MID_REAUTHOR_CLEARS_BADGE
    let c3_exp_on = dir.join("c3_exp_on.cpf");
    std::fs::write(
        &c3_exp_on,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.9 -state on\nend_design\n",
    )
    .unwrap();
    let mid_for_e2 = dir.join("mid_for_e2.cpf");
    std::fs::write(
        &mid_for_e2,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf -scope /\n",
            c3_exp_on.display()
        ),
    )
    .unwrap();
    let imp_mid_e2 = dir.join("imp_mid_e2.cpf");
    std::fs::write(
        &imp_mid_e2,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf -scope u_wrap\n",
            mid_for_e2.display()
        ),
    )
    .unwrap();
    let res_e2 = parse_cpf_and_resolve(
        &format!(
            "{p1_head}load_upf \"{}\" -format cpf\nend_design\n",
            imp_mid_e2.display()
        ),
        None,
        None,
        finder.clone(),
    )
    .unwrap();
    assert_eq!(
        ss_state_summary(&res_e2, "SS_PD_SUB"),
        vec![
            ("NC_OFF".to_string(), Some(SimState::Normal), false),
            ("NC_ON".to_string(), Some(SimState::Normal), false),
        ]
    );

    let imp_mid_reauth = dir.join("imp_mid_reauth.cpf");
    std::fs::write(
        &imp_mid_reauth,
        format!(
            "set_cpf_version 2.0\n\
             load_upf \"{}\" -format cpf -scope /\n\
             create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n",
            c3_exp_on.display()
        ),
    )
    .unwrap();
    let p_standby_head = p1_head.replace("0.0 -state off", "0.5 -state standby");
    let res_reauth = parse_cpf_and_resolve(
        &format!(
            "{p_standby_head}load_upf \"{}\" -format cpf\nend_design\n",
            imp_mid_reauth.display()
        ),
        None,
        None,
        finder,
    )
    .unwrap();
    assert_eq!(
        ss_state_summary(&res_reauth, "SS_PD_SUB"),
        vec![
            ("NC_OFF".to_string(), Some(SimState::Normal), false),
            ("NC_ON".to_string(), Some(SimState::Normal), false),
        ]
    );
    let nc_off_def = res_reauth
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_PD_SUB" && s.state_name == "NC_OFF")
        .unwrap();
    assert!(
        nc_off_def
            .supply_expr
            .as_deref()
            .is_some_and(|e| e.contains("0.5")),
        "master's 0.5V standby must survive imp_mid re-authoring: {nc_off_def:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn r32_bug_e_cexp_then_cimp_under_cpf_or_upf_mid_preserves_explicit_override_and_badge() {
    let dir = temp_dir("r32_bug_e");
    let finder = finder_4hop();

    let c_exp = dir.join("c_exp.cpf");
    std::fs::write(
        &c_exp,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.9 -state on\nend_design\n",
    )
    .unwrap();
    let c_imp = dir.join("c_imp.cpf");
    std::fs::write(
        &c_imp,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.0 -state off\nend_design\n",
    )
    .unwrap();

    let mid_cpf = dir.join("mid.cpf");
    std::fs::write(
        &mid_cpf,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf -scope /\nload_upf \"{}\" -format cpf\n",
            c_exp.display(),
            c_imp.display()
        ),
    )
    .unwrap();
    let mid_upf = dir.join("mid.upf");
    std::fs::write(
        &mid_upf,
        format!(
            "upf_version 2.1\nload_upf \"{}\" -format cpf -scope /\nload_upf \"{}\" -format cpf\n",
            c_exp.display(),
            c_imp.display()
        ),
    )
    .unwrap();
    let mid1_upf = dir.join("mid1.upf");
    std::fs::write(
        &mid1_upf,
        format!(
            "upf_version 2.1\nload_upf \"{}\" -format cpf -scope /\n",
            c_exp.display()
        ),
    )
    .unwrap();
    let mid2_upf = dir.join("mid2.upf");
    std::fs::write(
        &mid2_upf,
        format!(
            "upf_version 2.1\nload_upf \"{}\" -format cpf\n",
            c_imp.display()
        ),
    )
    .unwrap();
    let w1_cpf = dir.join("w1.cpf");
    std::fs::write(
        &w1_cpf,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf -scope /\n",
            c_exp.display()
        ),
    )
    .unwrap();
    let w2_cpf = dir.join("w2.cpf");
    std::fs::write(
        &w2_cpf,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf\n",
            c_imp.display()
        ),
    )
    .unwrap();
    let mid_two_wrap = dir.join("mid_two_wrap.cpf");
    std::fs::write(
        &mid_two_wrap,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf -scope /\nload_upf \"{}\" -format cpf -scope /\n",
            w1_cpf.display(),
            w2_cpf.display()
        ),
    )
    .unwrap();

    let master_head = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.5 -state standby\n\
        create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF} -condition {!pwr_en}\n\
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -condition {pwr_en} -default\n";

    let variants = [
        (
            "CH1_DIRECT_CEXP_THEN_CIMP",
            format!(
                "{master_head}load_upf \"{}\" -format cpf -scope /\nload_upf \"{}\" -format cpf\nend_design\n",
                c_exp.display(),
                c_imp.display()
            ),
        ),
        (
            "CH1_MID_NO_SCOPE_CEXP_THEN_CIMP",
            format!(
                "{master_head}load_upf \"{}\" -format cpf\nend_design\n",
                mid_cpf.display()
            ),
        ),
        (
            "CH1_MID_SLASH_CEXP_THEN_CIMP",
            format!(
                "{master_head}load_upf \"{}\" -format cpf -scope /\nend_design\n",
                mid_cpf.display()
            ),
        ),
        (
            "CH9_UPF_MID_CEXP_THEN_CIMP",
            format!(
                "{master_head}end_design\nload_upf \"{}\" -scope /\n",
                mid_upf.display()
            ),
        ),
        (
            "CH9_TWO_UPF_MIDS_CEXP_THEN_CIMP",
            format!(
                "{master_head}end_design\nload_upf \"{}\" -scope /\nload_upf \"{}\" -scope /\n",
                mid1_upf.display(),
                mid2_upf.display()
            ),
        ),
        (
            "ADV_4HOP_MID_TWO_WRAPPERS",
            format!(
                "{master_head}load_upf \"{}\" -format cpf\nend_design\n",
                mid_two_wrap.display()
            ),
        ),
    ];

    for (tag, script) in variants {
        let res = parse_cpf_and_resolve(&script, None, None, finder.clone()).unwrap();
        assert_eq!(
            ss_state_summary(&res, "SS_PD_SUB"),
            vec![
                ("NC_OFF".to_string(), Some(SimState::Normal), false),
                ("NC_ON".to_string(), Some(SimState::Normal), false),
            ],
            "{tag}: NC_OFF must remain 0.9V/Normal"
        );
        let nc_off = res
            .intent
            .power_states
            .iter()
            .find(|s| s.target == "SS_PD_SUB" && s.state_name == "NC_OFF")
            .unwrap();
        assert!(
            nc_off
                .supply_expr
                .as_deref()
                .is_some_and(|e| e.contains("0.9")),
            "{tag}: expected 0.9V from c_exp, got {nc_off:?}"
        );
    }

    // TOP_UPF_C1_C2_C2_WINS_NESTED: two implicit CPF children under root UPF -> c2 overwrites c1.
    let top_upf = dir.join("top_c1_c2.upf");
    let c1_standby = dir.join("c1_standby.cpf");
    std::fs::write(&c1_standby, master_head.to_string() + "end_design\n").unwrap();
    std::fs::write(
        &top_upf,
        format!(
            "upf_version 2.1\nload_upf \"{}\" -format cpf\nload_upf \"{}\" -format cpf\n",
            c1_standby.display(),
            c_exp.display()
        ),
    )
    .unwrap();
    let mut intent_top =
        load_upf_files(&[file_spec(top_upf, None, false)], finder.clone()).unwrap();
    let res_top = resolve(&mut intent_top, finder.as_ref());
    let nc_off_top = res_top
        .intent
        .power_states
        .iter()
        .find(|s| s.target == "SS_PD_SUB" && s.state_name == "NC_OFF")
        .unwrap();
    assert!(
        nc_off_top
            .supply_expr
            .as_deref()
            .is_some_and(|e| e.contains("0.9")),
        "TOP_UPF_C1_C2_C2_WINS_NESTED must let c2 win: {nc_off_top:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
