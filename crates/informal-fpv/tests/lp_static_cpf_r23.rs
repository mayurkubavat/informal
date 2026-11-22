use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use informal_fpv::lp::{check_lp, load_upf, CheckLpOpts, LpStage, LpViolation};
use informal_fpv::session::Session;

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn new_temp(tag: &str) -> TempDir {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "informal_lp_cpf_r23_{}_{n}_{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&p).expect("mkdir");
    TempDir(p)
}

const DESIGN_SV: &str = r#"
module sub_blk(
  input  logic clk,
  input  logic d,
  input  logic wrap_bad_ctrl,
  output logic q
);
  always_ff @(posedge clk) q <= d ^ wrap_bad_ctrl;
endmodule

module wrap_blk(
  input  logic clk,
  input  logic d,
  input  logic bad_shutoff,
  output logic q_sub,
  output logic q_wrap
);
  logic bad_ctrl_q;
  always_ff @(posedge clk) bad_ctrl_q <= bad_shutoff;
  assign q_wrap = bad_ctrl_q;
  sub_blk u_sub(.clk(clk), .d(d), .wrap_bad_ctrl(bad_ctrl_q), .q(q_sub));
endmodule

module top(
  input  logic clk,
  input  logic d,
  input  logic pwr_en,
  input  logic other_aon,
  input  logic bad_shutoff,
  output logic q_sub,
  output logic q_wrap
);
  wrap_blk u_wrap(
    .clk(clk), .d(d), .bad_shutoff(bad_shutoff),
    .q_sub(q_sub), .q_wrap(q_wrap)
  );
endmodule
"#;

fn run_seq_files(
    tag: &str,
    files: &[(&str, Option<&str>, bool, &str)],
    stage: LpStage,
) -> Vec<LpViolation> {
    let dir = new_temp(tag);
    let sv_path = dir.0.join("design.sv");
    std::fs::write(&sv_path, DESIGN_SV).expect("write sv");
    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    s.design.files.push(sv_path);
    s.elaborate(Some("top".to_string())).expect("elaborate");
    for (fname, scope, is_cpf, body) in files {
        let p = dir.0.join(fname);
        std::fs::write(&p, body).expect("write intent");
        load_upf(&mut s, &p, *scope, None, *is_cpf).expect("load_upf");
    }
    check_lp(
        &mut s,
        &CheckLpOpts {
            stage,
            ..Default::default()
        },
    )
    .expect("check_lp");
    s.lp.violations.clone()
}

fn run_nested_top(
    tag: &str,
    extra_files: &[(&str, &str)],
    top_name: &str,
    top_is_cpf: bool,
    top_body: &str,
    stage: LpStage,
) -> Vec<LpViolation> {
    let dir = new_temp(tag);
    let sv_path = dir.0.join("design.sv");
    std::fs::write(&sv_path, DESIGN_SV).expect("write sv");
    for (fname, body) in extra_files {
        std::fs::write(dir.0.join(fname), body).expect("write child");
    }
    let top_path = dir.0.join(top_name);
    std::fs::write(&top_path, top_body).expect("write top");

    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    s.design.files.push(sv_path);
    s.elaborate(Some("top".to_string())).expect("elaborate");
    load_upf(&mut s, &top_path, None, None, top_is_cpf).expect("load_upf");
    check_lp(
        &mut s,
        &CheckLpOpts {
            stage,
            ..Default::default()
        },
    )
    .expect("check_lp");
    s.lp.violations.clone()
}

#[test]
fn r23_fix1_sequential_update_illegal_matches_nested() {
    let c1 = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS\n\
        end_design\n";

    for (tag, state_spec, stage, expect_ret) in [
        ("off_ill", "{OFF -update -illegal}", LpStage::Rtl, true),
        (
            "off_legal_false",
            "{OFF -update -legal false}",
            LpStage::Rtl,
            true,
        ),
        ("on_ill", "{ON -update -illegal}", LpStage::Upf, false),
    ] {
        let u = format!("upf_version 2.1\nadd_power_state SS_PD_WRAP -state {state_spec}\n");
        let v_seq = run_seq_files(
            tag,
            &[("c1.cpf", None, true, c1), ("u.upf", Some("/"), false, &u)],
            stage,
        );
        let top_upf = "upf_version 2.1\nload_upf c1.cpf -format cpf\nload_upf u.upf\n";
        let v_nested = run_nested_top(
            &format!("{tag}_nest"),
            &[("c1.cpf", c1), ("u.upf", &u)],
            "top.upf",
            false,
            top_upf,
            stage,
        );
        assert_eq!(
            v_seq.len(),
            v_nested.len(),
            "{tag}: {v_seq:#?} vs {v_nested:#?}"
        );
        assert!(!v_seq.iter().any(|v| v.rule_id == "UPF_EXPR_SYNTAX"));
        assert!(!v_seq.iter().any(|v| v.rule_id == "ISO_MISSING"));
        if expect_ret {
            assert_eq!(v_seq.len(), 1);
            assert_eq!(v_seq[0].rule_id, "RET_NO_ELEMENTS");
        } else {
            assert!(v_seq.is_empty());
        }
    }
}

#[test]
fn r23_fix2_update_then_only_off_multi_and_single_upf() {
    let c1 = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS\n\
        end_design\n";
    let u1 = "upf_version 2.1\nadd_power_state SS_PD_WRAP -state {ON -update -logic_expr {1'b1}}\n";
    let u2 = "upf_version 2.1\nadd_power_state SS_PD_WRAP -state {ONLY_OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {0} -simstate CORRUPT}\n";
    let u_both = "upf_version 2.1\n\
        add_power_state SS_PD_WRAP -state {ON -update -logic_expr {1'b1}}\n\
        add_power_state SS_PD_WRAP -state {ONLY_OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {0} -simstate CORRUPT}\n";

    let v_ctrl = run_seq_files(
        "fix2_ctrl",
        &[("c1.cpf", None, true, c1), ("u2.upf", Some("/"), false, u2)],
        LpStage::Upf,
    );
    assert!(v_ctrl.iter().any(|v| v.rule_id == "PST_INCONSISTENT"));

    let v_multi = run_seq_files(
        "fix2_multi",
        &[
            ("c1.cpf", None, true, c1),
            ("u1.upf", Some("/"), false, u1),
            ("u2.upf", Some("/"), false, u2),
        ],
        LpStage::Upf,
    );
    assert!(
        v_multi.iter().any(|v| v.rule_id == "PST_INCONSISTENT"),
        "multi-file update+ONLY_OFF must emit PST_INCONSISTENT: {v_multi:#?}"
    );

    let v_single = run_seq_files(
        "fix2_single",
        &[
            ("c1.cpf", None, true, c1),
            ("u_both.upf", Some("/"), false, u_both),
        ],
        LpStage::Upf,
    );
    assert!(
        v_single.iter().any(|v| v.rule_id == "PST_INCONSISTENT"),
        "single-UPF update+ONLY_OFF must emit PST_INCONSISTENT: {v_single:#?}"
    );

    let v_upd_only = run_seq_files(
        "fix2_upd_only",
        &[("c1.cpf", None, true, c1), ("u1.upf", Some("/"), false, u1)],
        LpStage::Rtl,
    );
    assert_eq!(
        v_upd_only
            .iter()
            .filter(|v| v.rule_id == "ISO_MISSING")
            .count(),
        2
    );
}

#[test]
fn r23_fix3_same_scope_siblings_delegation_and_probe5() {
    let sub1 = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_domain -name PD_SUB -instances {.} -shutoff_condition {!en_sub}\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        create_power_mode -name PM_SUB -domain_conditions {PD_SUB@NC_ON} -condition {en_sub} -default\n\
        end_design\n";
    let sub2 = "set_cpf_version 2.0\ncreate_nominal_condition -name NC_ON -voltage 0.0 -state off\nend_design\n";

    let v_root = run_nested_top(
        "fix3_root",
        &[("sub1.cpf", sub1), ("sub2.cpf", sub2)],
        "parent.cpf",
        true,
        "set_cpf_version 2.0\nload_upf sub1.cpf -format cpf\nload_upf sub2.cpf -format cpf\n",
        LpStage::Upf,
    );
    assert!(v_root.iter().any(|v| v.rule_id == "PST_INCONSISTENT"));

    let wrap =
        "set_cpf_version 2.0\nload_upf sub1.cpf -format cpf\nload_upf sub2.cpf -format cpf\n";
    let v_wrap = run_nested_top(
        "fix3_wrap",
        &[("sub1.cpf", sub1), ("sub2.cpf", sub2), ("wrap.cpf", wrap)],
        "root.cpf",
        true,
        "set_cpf_version 2.0\nload_upf wrap.cpf -format cpf -scope u_wrap\n",
        LpStage::Upf,
    );
    assert!(v_wrap.iter().any(|v| v.rule_id == "PST_INCONSISTENT"));

    let mid1 = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_W} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_domain -name PD_WRAP -instances {.} -shutoff_condition {!pwr_en}\n\
        update_power_domain -name PD_WRAP -primary_power_net VDD_W -primary_ground_net VSS\n\
        create_power_mode -name PM_WRAP -domain_conditions {PD_WRAP@NC_ON} -condition {pwr_en} -default\n\
        end_design\n";
    let leaf2 = sub2;
    let mid2 = "set_cpf_version 2.0\nload_upf leaf2.cpf -format cpf\n";
    let v_deleg = run_nested_top(
        "fix3_deleg",
        &[("mid1.cpf", mid1), ("leaf2.cpf", leaf2), ("mid2.cpf", mid2)],
        "root.cpf",
        true,
        "set_cpf_version 2.0\nload_upf mid1.cpf -format cpf -scope u_wrap\nload_upf mid2.cpf -format cpf -scope u_wrap\n",
        LpStage::Upf,
    );
    assert!(v_deleg.iter().any(|v| v.rule_id == "PST_INCONSISTENT"));

    let child_ov = "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OV -voltage 0.0 -state off\nend_design\n";
    let probe5 = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_TOP} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_nominal_condition -name NC_OV -voltage 0.9 -state on\n\
        create_power_domain -name PD_TOP -default -shutoff_condition {!pwr_en}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\n\
        create_power_mode -name PM_TOP -domain_conditions {PD_TOP@NC_OV} -condition {pwr_en} -default\n\
        end_design\n\
        load_upf child_ov.cpf -format cpf\n";
    let v_p5 = run_nested_top(
        "fix3_probe5",
        &[("child_ov.cpf", child_ov)],
        "parent.cpf",
        true,
        probe5,
        LpStage::Upf,
    );
    assert!(v_p5.is_empty(), "Probe 5 must remain clean: {v_p5:#?}");
}

#[test]
fn r24_fix_a_multi_domain_emitted_and_deferred_fallback_mix() {
    let c1 = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_A -default -shutoff_condition {!pwr_en}\n\
        update_power_domain -name PD_A -primary_power_net VDD_AON -primary_ground_net VSS\n\
        end_design\n";
    let c2 = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_B} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_B -instances {u_wrap} -shutoff_condition {!other_aon}\n\
        update_power_domain -name PD_B -primary_power_net VDD_B -primary_ground_net VSS\n\
        end_design\n";
    let b1 = "upf_version 2.1\nadd_power_state SS_PD_B -state {ON -update -logic_expr {1'b1}}\n";
    let b2 = "upf_version 2.1\nadd_power_state SS_PD_B -state {ONLY_OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {0} -simstate CORRUPT}\n";
    let u2 = format!("{b1}{b2}");

    let v_ctrl = run_seq_files(
        "r24_a_ctrl",
        &[
            ("c2.cpf", None, true, c2),
            ("u2.upf", Some("/"), false, &u2),
        ],
        LpStage::Upf,
    );
    assert!(v_ctrl.iter().any(|v| v.rule_id == "PST_INCONSISTENT"));

    let top_nested = "upf_version 2.1\nload_upf c1.cpf -format cpf\nload_upf parent_b.cpf -format cpf\nload_upf u2.upf\n";
    let v_nested = run_nested_top(
        "r24_a_nested",
        &[
            ("c1.cpf", c1),
            ("c2.cpf", c2),
            (
                "parent_b.cpf",
                "set_cpf_version 2.0\nload_upf c2.cpf -format cpf\n",
            ),
            ("u2.upf", &u2),
        ],
        "top.upf",
        false,
        top_nested,
        LpStage::Upf,
    );
    assert!(
        v_nested.iter().any(|v| v.rule_id == "PST_INCONSISTENT"),
        "nested mixed must return PST_INCONSISTENT: {v_nested:#?}"
    );

    let v_seq = run_seq_files(
        "r24_a_seq",
        &[
            ("c1.cpf", None, true, c1),
            ("c2.cpf", Some("/"), true, c2),
            ("u2.upf", Some("/"), false, &u2),
        ],
        LpStage::Upf,
    );
    assert!(
        v_seq.iter().any(|v| v.rule_id == "PST_INCONSISTENT"),
        "sequential mixed must return PST_INCONSISTENT: {v_seq:#?}"
    );

    let dir = new_temp("r24_a_ch1");
    let sv_path = dir.0.join("design.sv");
    std::fs::write(&sv_path, DESIGN_SV).unwrap();
    std::fs::write(dir.0.join("c1.cpf"), c1).unwrap();
    std::fs::write(
        dir.0.join("top_a.upf"),
        "upf_version 2.1\nload_upf c1.cpf -format cpf\n",
    )
    .unwrap();
    std::fs::write(dir.0.join("c2.cpf"), c2).unwrap();
    std::fs::write(dir.0.join("u_b1.upf"), b1).unwrap();
    std::fs::write(dir.0.join("u_b2.upf"), b2).unwrap();
    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    s.design.files.push(sv_path);
    s.elaborate(Some("top".to_string())).unwrap();
    load_upf(&mut s, &dir.0.join("top_a.upf"), None, None, false).unwrap();
    load_upf(&mut s, &dir.0.join("c2.cpf"), Some("/"), None, true).unwrap();
    load_upf(&mut s, &dir.0.join("u_b1.upf"), Some("/"), None, false).unwrap();
    load_upf(&mut s, &dir.0.join("u_b2.upf"), Some("/"), None, false).unwrap();
    check_lp(
        &mut s,
        &CheckLpOpts {
            stage: LpStage::Upf,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        s.lp.violations
            .iter()
            .any(|v| v.rule_id == "PST_INCONSISTENT"),
        "Challenge 1 mixed nested+seq must return PST_INCONSISTENT: {:#?}",
        s.lp.violations
    );
}

#[test]
fn r24_fix_b_parent_wins_nc_with_child_pm_upf_and_rtl() {
    let child_upf = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_OV -voltage 0.0 -state off\n\
        create_power_nets -nets {VDD_CHILD} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_CHILD -instances {u_wrap} -shutoff_condition {!other_aon}\n\
        update_power_domain -name PD_CHILD -primary_power_net VDD_CHILD -primary_ground_net VSS\n\
        create_power_mode -name PM_CHILD -domain_conditions {PD_CHILD@NC_OV} -condition {other_aon}\n\
        end_design\n";
    let parent_upf = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_TOP} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_nominal_condition -name NC_OV -voltage 0.9 -state on\n\
        create_power_domain -name PD_TOP -default -shutoff_condition {!pwr_en}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\n\
        create_power_mode -name PM_TOP -domain_conditions {PD_TOP@NC_OV} -condition {pwr_en} -default\n\
        end_design\n\
        load_upf child.cpf -format cpf\n";
    let v_upf = run_nested_top(
        "r24_b_upf",
        &[("child.cpf", child_upf)],
        "parent.cpf",
        true,
        parent_upf,
        LpStage::Upf,
    );
    assert!(
        v_upf.is_empty(),
        "edge3D_parent_wins_with_child_pm: {v_upf:#?}"
    );

    let child_rtl = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_OV -voltage 0.0 -state off\n\
        create_power_nets -nets {VDD_CHILD} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_CHILD -instances {u_wrap}\n\
        update_power_domain -name PD_CHILD -primary_power_net VDD_CHILD -primary_ground_net VSS\n\
        create_power_mode -name PM_CHILD -domain_conditions {PD_CHILD@NC_OV}\n\
        end_design\n";
    let parent_rtl = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_TOP} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_nominal_condition -name NC_OV -voltage 0.9 -state on\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\n\
        create_power_mode -name PM_TOP -domain_conditions {PD_TOP@NC_OV} -default\n\
        end_design\n\
        load_upf child.cpf -format cpf\n";
    let v_rtl = run_nested_top(
        "r24_b_rtl",
        &[("child.cpf", child_rtl)],
        "parent.cpf",
        true,
        parent_rtl,
        LpStage::Rtl,
    );
    assert!(
        v_rtl.is_empty(),
        "bug_child_overrides_nc must be clean: {v_rtl:#?}"
    );

    let gp_rtl = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_TOP} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_nominal_condition -name NC_OV -voltage 0.9 -state on\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\n\
        create_power_mode -name PM_TOP -domain_conditions {PD_TOP@NC_OV} -default\n\
        end_design\n\
        load_upf mid.cpf -format cpf\n";
    let v_3lvl = run_nested_top(
        "r24_b_3lvl",
        &[
            ("child.cpf", child_rtl),
            (
                "mid.cpf",
                "set_cpf_version 2.0\nload_upf child.cpf -format cpf\n",
            ),
        ],
        "gp.cpf",
        true,
        gp_rtl,
        LpStage::Rtl,
    );
    assert!(
        v_3lvl.is_empty(),
        "3-level gp->mid->child must be clean: {v_3lvl:#?}"
    );
}

#[test]
fn r24_fix_c_hyp1_seq_vs_nested_rtl() {
    let c1 = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS\n\
        end_design\n";

    let cases = [
        (
            "hyp1_named_on",
            "upf_version 2.1\nadd_power_state SS_PD_WRAP -state {ON -update -logic_expr {1'b0}}\n",
            "set_cpf_version 2.0\ncreate_nominal_condition -name ON -voltage 0.9 -state on\ncreate_power_mode -name PM_W -domain_conditions {PD_TOP@ON PD_WRAP@ON} -condition {pwr_en} -default\nend_design\n",
            0usize,
            0usize,
        ),
        (
            "hyp1_ill_off",
            "upf_version 2.1\nadd_power_state SS_PD_WRAP -state {OFF -update -illegal}\n",
            "set_cpf_version 2.0\ncreate_nominal_condition -name OFF -voltage 0.0 -state off\ncreate_nominal_condition -name ON -voltage 0.9 -state on\ncreate_power_mode -name PM_W -domain_conditions {PD_TOP@ON PD_WRAP@OFF} -condition {!pwr_en}\nend_design\n",
            2usize,
            1usize,
        ),
        (
            "hyp1_corrupt",
            "upf_version 2.1\nadd_power_state SS_PD_WRAP -state {OFF -update -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {!pwr_en} -simstate CORRUPT}\n",
            "set_cpf_version 2.0\ncreate_nominal_condition -name NC_ON -voltage 0.9 -state on\ncreate_power_mode -name PM_W -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -condition {pwr_en} -default\nend_design\n",
            0usize,
            0usize,
        ),
    ];

    for (tag, u, c2, exp_iso, exp_ret) in cases {
        let v_seq = run_seq_files(
            tag,
            &[
                ("c1.cpf", None, true, c1),
                ("u.upf", Some("/"), false, u),
                ("c2.cpf", Some("/"), true, c2),
            ],
            LpStage::Rtl,
        );
        let v_nest = run_nested_top(
            &format!("{tag}_nest"),
            &[("c1.cpf", c1), ("u.upf", u), ("c2.cpf", c2)],
            "top.upf",
            false,
            "upf_version 2.1\nload_upf c1.cpf -format cpf\nload_upf u.upf\nload_upf c2.cpf -format cpf\n",
            LpStage::Rtl,
        );
        assert_eq!(
            v_seq.len(),
            v_nest.len(),
            "{tag}: {v_seq:#?} vs {v_nest:#?}"
        );
        assert_eq!(
            v_seq.iter().filter(|v| v.rule_id == "ISO_MISSING").count(),
            exp_iso
        );
        assert_eq!(
            v_seq
                .iter()
                .filter(|v| v.rule_id == "RET_NO_ELEMENTS")
                .count(),
            exp_ret
        );
    }
}

#[test]
fn r25_nomv_only_off_update_rtl_and_upf_seq_matches_nested() {
    let c1 = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_TOP} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\n\
        end_design\n";
    let c2_nc = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_T -domain_conditions {PD_TOP@NC_ON} -condition {pwr_en} -default\n\
        end_design\n";
    let c2_off = "set_cpf_version 2.0\n\
        create_nominal_condition -name OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_T -domain_conditions {PD_TOP@OFF} -condition {!pwr_en}\n\
        end_design\n";
    let u_off_upd = "upf_version 2.1\nadd_power_state SS_PD_TOP -state {OFF -update -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {!pwr_en} -simstate CORRUPT}\n";
    let u_off_ill = "upf_version 2.1\nadd_power_state SS_PD_TOP -state {OFF -update -illegal}\n";
    let u_on_upd =
        "upf_version 2.1\nadd_power_state SS_PD_TOP -state {ON -update -logic_expr {1'b0}}\n";

    let key = |v: &LpViolation| (v.rule_id.clone(), v.signal.clone());
    for (tag, u, c2, stage, exp_ret, exp_syntax) in [
        (
            "nomv_off_upd",
            u_off_upd,
            c2_nc,
            LpStage::Rtl,
            1usize,
            false,
        ),
        (
            "nomv_off_ill",
            u_off_ill,
            c2_off,
            LpStage::Upf,
            0usize,
            false,
        ),
        ("nomv_on_upd", u_on_upd, c2_nc, LpStage::Rtl, 0usize, false),
    ] {
        let v_seq = run_seq_files(
            tag,
            &[
                ("c1.cpf", None, true, c1),
                ("u.upf", Some("/"), false, u),
                ("c2.cpf", Some("/"), true, c2),
            ],
            stage,
        );
        let v_nest = run_nested_top(
            &format!("{tag}_nest"),
            &[("c1.cpf", c1), ("u.upf", u), ("c2.cpf", c2)],
            "top.upf",
            false,
            "upf_version 2.1\nload_upf c1.cpf -format cpf\nload_upf u.upf\nload_upf c2.cpf -format cpf\n",
            stage,
        );
        let ks: Vec<_> = v_seq.iter().map(key).collect();
        let kn: Vec<_> = v_nest.iter().map(key).collect();
        assert_eq!(ks, kn, "{tag}: seq != nest");
        assert_eq!(
            v_seq
                .iter()
                .filter(|v| v.rule_id == "RET_NO_ELEMENTS")
                .count(),
            exp_ret,
            "{tag}: {v_seq:#?}"
        );
        assert_eq!(
            v_seq.iter().any(|v| v.rule_id == "UPF_EXPR_SYNTAX"),
            exp_syntax,
            "{tag}: {v_seq:#?}"
        );
    }

    // Scoped -scope u_wrap nom_v-only domain preserves OFF -update
    let c1_sub = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_SUB -instances {.}\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        end_design\n";
    let u_sub = "upf_version 2.1\nadd_power_state PD_SUB.primary -state {OFF -update -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {!pwr_en} -simstate CORRUPT}\n";
    let c2_sub = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_S -domain_conditions {PD_SUB@NC_ON} -condition {pwr_en} -default\n\
        end_design\n";
    let v_sub_seq = run_seq_files(
        "scoped_nomv_off",
        &[
            ("c1.cpf", Some("u_wrap"), true, c1_sub),
            ("u.upf", Some("u_wrap"), false, u_sub),
            ("c2.cpf", Some("u_wrap"), true, c2_sub),
        ],
        LpStage::Rtl,
    );
    let v_sub_nest = run_nested_top(
        "scoped_nomv_off_nest",
        &[("c1.cpf", c1_sub), ("u.upf", u_sub), ("c2.cpf", c2_sub)],
        "top.upf",
        false,
        "upf_version 2.1\nload_upf c1.cpf -format cpf -scope u_wrap\nload_upf u.upf -scope u_wrap\nload_upf c2.cpf -format cpf -scope u_wrap\n",
        LpStage::Rtl,
    );
    let ks_sub: Vec<_> = v_sub_seq.iter().map(key).collect();
    let kn_sub: Vec<_> = v_sub_nest.iter().map(key).collect();
    assert_eq!(ks_sub, kn_sub, "scoped nom_v off seq != nest");
    assert_eq!(v_sub_seq.len(), 1, "{v_sub_seq:#?}");
    assert!(
        v_sub_seq
            .iter()
            .any(|v| v.rule_id == "RET_NO_ELEMENTS" && v.signal == "u_wrap/PD_SUB"),
        "expected RET_NO_ELEMENTS on u_wrap/PD_SUB: {v_sub_seq:#?}"
    );
}
