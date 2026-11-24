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
        "informal_lp_cpf_r28_{}_{n}_{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&p).expect("mkdir");
    TempDir(p)
}

const WRAP_SV: &str = r#"
module sub_blk(
  input  logic clk,
  input  logic d,
  output logic q
);
  always_ff @(posedge clk) q <= d;
endmodule

module wrap_blk(
  input  logic clk,
  input  logic d,
  output logic q_sub
);
  sub_blk u_sub(.clk(clk), .d(d), .q(q_sub));
endmodule

module top(
  input  logic clk,
  input  logic d,
  input  logic pwr_en,
  input  logic new_en,
  input  logic other_en,
  input  logic ok_en,
  output logic q_sub
);
  wrap_blk u_wrap(.clk(clk), .d(d), .q_sub(q_sub));
endmodule
"#;

const AB_SV: &str = r#"
module leaf(
  input  logic clk,
  input  logic d,
  output logic q
);
  always_ff @(posedge clk) q <= d;
endmodule

module dst_blk(
  input  logic clk,
  input  logic in_sig,
  output logic out_sig
);
  always_ff @(posedge clk) out_sig <= in_sig;
endmodule

module top(
  input  logic clk,
  input  logic d,
  input  logic sel_x,
  input  logic sel_b,
  input  logic sel_top,
  output logic qa,
  output logic qb
);
  logic wa, wb;
  leaf u_a(.clk(clk), .d(d), .q(wa));
  leaf u_b(.clk(clk), .d(d), .q(wb));
  dst_blk u_dst(.clk(clk), .in_sig(wb), .out_sig(qb));
  assign qa = wa;
endmodule
"#;

fn run_seq_sv(
    tag: &str,
    sv_src: &str,
    files: &[(&str, Option<&str>, bool, &str)],
) -> Vec<LpViolation> {
    let dir = new_temp(tag);
    let sv_path = dir.0.join("design.sv");
    std::fs::write(&sv_path, sv_src).expect("write sv");
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
            stage: LpStage::All,
            ..Default::default()
        },
    )
    .expect("check_lp");
    s.lp.violations.clone()
}

fn run_nested_sv(
    tag: &str,
    sv_src: &str,
    extra_files: &[(&str, &str)],
    top_name: &str,
    top_is_cpf: bool,
    top_body: &str,
) -> Vec<LpViolation> {
    let dir = new_temp(tag);
    let sv_path = dir.0.join("design.sv");
    std::fs::write(&sv_path, sv_src).expect("write sv");
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
            stage: LpStage::All,
            ..Default::default()
        },
    )
    .expect("check_lp");
    s.lp.violations.clone()
}

fn v_keys(viols: &[LpViolation]) -> Vec<(String, String, String)> {
    viols
        .iter()
        .map(|v| {
            (
                v.rule_id.clone(),
                format!("{:?}", v.severity),
                v.signal.clone(),
            )
        })
        .collect()
}

#[test]
fn bug1_pm_x_illegal_no_bridge_and_bridge_plus_pm_x_upd_not_illegal() {
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
    let ct_ctrl = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_TOP} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_TOP -domain_conditions {PD_TOP@NC_ON} -default\n\
        create_power_mode -name PM_Y -domain_conditions {PD_TOP@NC_ON u_b/PD_B@NC_ON} -condition {sel_b} -illegal\n\
        end_design\n";
    let ct_pmx = ct_ctrl.replace("PM_Y", "PM_X");

    let v_ctrl = run_seq_sv(
        "ctrl_pmy",
        AB_SV,
        &[
            ("cb.cpf", Some("u_b"), true, cb),
            ("ct.cpf", Some("/"), true, ct_ctrl),
        ],
    );
    let v_bug = run_seq_sv(
        "bug_pmx_no_br",
        AB_SV,
        &[
            ("cb.cpf", Some("u_b"), true, cb),
            ("ct.cpf", Some("/"), true, &ct_pmx),
        ],
    );
    assert_eq!(v_keys(&v_bug), v_keys(&v_ctrl));
    assert_eq!(
        v_keys(&v_bug),
        vec![(
            "ISO_MISSING".to_string(),
            "Error".to_string(),
            "u_b/q".to_string()
        )]
    );

    // BRIDGE_PLUS_PM_X_UPD_NOT_ILLEGAL
    let ca = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_A} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_A -default\n\
        update_power_domain -name PD_A -primary_power_net VDD_A -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_KEEP_A -domain_conditions {PD_A@NC_ON} -default\n\
        end_design\n";
    let br_pmx = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_TOP} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_TOP -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_BRIDGE -domain_conditions {PD_TOP@NC_ON u_a/PD_A@NC_ON u_b/PD_B@NC_ON} -default\n\
        create_power_mode -name PM_X -domain_conditions {PD_TOP@NC_ON u_a/PD_A@NC_ON u_b/PD_B@NC_ON} -condition {sel_top}\n\
        end_design\n";
    let v_br = run_seq_sv(
        "br_pmx",
        AB_SV,
        &[
            ("cb.cpf", Some("u_b"), true, cb),
            ("ca.cpf", Some("u_a"), true, ca),
            ("br.cpf", Some("/"), true, br_pmx),
        ],
    );
    assert_eq!(
        v_keys(&v_br),
        vec![(
            "ISO_MISSING".to_string(),
            "Error".to_string(),
            "u_b/q".to_string()
        )]
    );
}

#[test]
fn bug2_nested_cpf_in_cpf_cross_scope_parent_pm_and_nc_refresh_rtl() {
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
    let c3_ill = "set_cpf_version 2.0\nupdate_power_mode -name PM_MIX -illegal\nend_design\n";

    // ATTACK_NESTED_CPF_IN_CPF_CROSS_SCOPE_ILLEGAL (`master.cpf`)
    let v_seq = run_seq_sv(
        "atk_cpf_seq",
        WRAP_SV,
        &[
            ("c1.cpf", Some("u_wrap"), true, c1_wrap),
            ("c2.cpf", Some("/"), true, c2_mix),
            ("c3.cpf", Some("/"), true, c3_ill),
        ],
    );
    let v_nest = run_nested_sv(
        "atk_cpf_nest",
        WRAP_SV,
        &[("c1.cpf", c1_wrap), ("c2.cpf", c2_mix), ("c3.cpf", c3_ill)],
        "master.cpf",
        true,
        "set_cpf_version 2.0\n\
         load_upf c1.cpf -format cpf -scope u_wrap\n\
         load_upf c2.cpf -format cpf -scope /\n\
         load_upf c3.cpf -format cpf -scope /\n",
    );
    assert_eq!(v_keys(&v_seq), v_keys(&v_nest));
    assert!(v_nest.is_empty(), "expected clean, got {v_nest:#?}");

    // SUSP2_CPF_IN_CPF & NESTED_CPF_IN_CPF_PARENT_DEFINES_PM_CHILD_MARKS_ILLEGAL
    let v_parent_pm = run_nested_sv(
        "parent_pm_child_ill",
        WRAP_SV,
        &[("c1.cpf", c1_wrap), ("c3.cpf", c3_ill)],
        "master.cpf",
        true,
        "set_cpf_version 2.0\n\
         load_upf c1.cpf -format cpf -scope u_wrap\n\
         create_power_nets -nets {VDD_AON} -voltage 0.9\n\
         create_ground_nets -nets {VSS}\n\
         create_power_domain -name PD_TOP -default\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
         create_power_mode -name PM_MIX -domain_conditions {PD_TOP@NC_ON u_wrap/PD_SUB@NC_OFF} -condition {mix_cond}\n\
         create_power_mode -name PM_TOP_ON -domain_conditions {PD_TOP@NC_ON u_wrap/PD_SUB@NC_ON} -condition {pwr_en} -default\n\
         end_design\n\
         load_upf c3.cpf -format cpf -scope /\n",
    );
    assert!(
        v_parent_pm.is_empty(),
        "expected clean, got {v_parent_pm:#?}"
    );

    // SUSP2B_CPF_IN_CPF_NC_REFRESH
    let c3_nc = "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.9 -state on\nend_design\n";
    let v_nc = run_nested_sv(
        "susp2b_nc",
        WRAP_SV,
        &[("c3_nc.cpf", c3_nc)],
        "top.cpf",
        true,
        "set_cpf_version 2.0\n\
         create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9\n\
         create_ground_nets -nets {VSS}\n\
         create_power_domain -name PD_TOP -default\n\
         create_power_domain -name PD_SUB -instances {u_wrap/u_sub}\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
         update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
         create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF} -condition {!pwr_en}\n\
         create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -condition {pwr_en} -default\n\
         end_design\n\
         load_upf c3_nc.cpf -format cpf -scope /\n",
    );
    assert!(
        v_nc.is_empty(),
        "expected clean after NC refresh, got {v_nc:#?}"
    );
}

#[test]
fn bug3_susp1b_susp1_and_partial_update_match_direct_illegal_seq_and_nested() {
    let c1_sh = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub} -shutoff_condition {!pwr_en}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        end_design\n";
    let c2_only = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_ONLY -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON}\n\
        end_design\n";
    let c2_only_dir = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_ONLY -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -illegal\n\
        end_design\n";
    let u_part =
        "upf_version 2.1\nadd_power_state SS_PD_SUB -state {NC_ON -update -logic_expr {new_en}}\n";
    let c3_only = "set_cpf_version 2.0\nupdate_power_mode -name PM_ONLY -illegal\nend_design\n";

    // SUSP1B_upd_ill_after_upf == SUSP1B_direct_ill
    let v_ctrl_1b = run_seq_sv(
        "susp1b_ctrl",
        WRAP_SV,
        &[
            ("c1.cpf", None, true, c1_sh),
            ("c2.cpf", Some("/"), true, c2_only_dir),
            ("u.upf", Some("/"), false, u_part),
        ],
    );
    let v_seq_1b = run_seq_sv(
        "susp1b_seq",
        WRAP_SV,
        &[
            ("c1.cpf", None, true, c1_sh),
            ("c2.cpf", Some("/"), true, c2_only),
            ("u.upf", Some("/"), false, u_part),
            ("c3.cpf", Some("/"), true, c3_only),
        ],
    );
    let v_nest_1b = run_nested_sv(
        "susp1b_nest",
        WRAP_SV,
        &[
            ("c1.cpf", c1_sh),
            ("c2.cpf", c2_only),
            ("u.upf", u_part),
            ("c3.cpf", c3_only),
        ],
        "top.upf",
        false,
        "upf_version 2.1\nload_upf c1.cpf -format cpf\nload_upf c2.cpf -format cpf\nload_upf u.upf\nload_upf c3.cpf -format cpf\n",
    );
    assert_eq!(v_keys(&v_seq_1b), v_keys(&v_nest_1b));
    assert_eq!(v_keys(&v_seq_1b), v_keys(&v_ctrl_1b));
    assert_eq!(
        v_keys(&v_seq_1b),
        vec![(
            "ISO_MISSING".to_string(),
            "Error".to_string(),
            "u_wrap/u_sub/q".to_string()
        )]
    );

    // SUSP1_UPD_PM_ILL_AFTER_UPF_UPD == SUSP1_CTRL_DIRECT_ILL_PLUS_UPF_UPD
    let c1_nosh = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        end_design\n";
    let c2_two = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF}\n\
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -default\n\
        end_design\n";
    let c2_two_dir = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF} -illegal\n\
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -default\n\
        end_design\n";
    let c3_off = "set_cpf_version 2.0\nupdate_power_mode -name PM_OFF -illegal\nend_design\n";

    for (tag, u_body) in [
        (
            "susp1_other_en",
            "upf_version 2.1\nadd_power_state SS_PD_SUB -state {NC_OFF -update -logic_expr {!other_en}}\n",
        ),
        (
            "3b_upd_partial",
            "upf_version 2.1\nadd_power_state SS_PD_SUB -state {NC_OFF -update -simstate NORMAL -logic_expr {ok_en}}\n",
        ),
        (
            "3a_upd_full",
            "upf_version 2.1\nadd_power_state SS_PD_SUB -state {NC_OFF -update -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL -logic_expr {ok_en}}\n",
        ),
    ] {
        let v_ctrl = run_seq_sv(
            &format!("{tag}_ctrl"),
            WRAP_SV,
            &[
                ("c1.cpf", None, true, c1_nosh),
                ("c2.cpf", Some("/"), true, c2_two_dir),
                ("u.upf", Some("/"), false, u_body),
            ],
        );
        let v_seq = run_seq_sv(
            &format!("{tag}_seq"),
            WRAP_SV,
            &[
                ("c1.cpf", None, true, c1_nosh),
                ("c2.cpf", Some("/"), true, c2_two),
                ("u.upf", Some("/"), false, u_body),
                ("c3.cpf", Some("/"), true, c3_off),
            ],
        );
        let v_nest = run_nested_sv(
            &format!("{tag}_nest"),
            WRAP_SV,
            &[
                ("c1.cpf", c1_nosh),
                ("c2.cpf", c2_two),
                ("u.upf", u_body),
                ("c3.cpf", c3_off),
            ],
            "top.upf",
            false,
            "upf_version 2.1\nload_upf c1.cpf -format cpf\nload_upf c2.cpf -format cpf\nload_upf u.upf\nload_upf c3.cpf -format cpf\n",
        );
        assert_eq!(v_keys(&v_seq), v_keys(&v_nest), "{tag}: seq != nest");
        assert_eq!(v_keys(&v_seq), v_keys(&v_ctrl), "{tag}: seq != ctrl");
        assert!(v_seq.is_empty(), "{tag}: expected clean, got {v_seq:#?}");
    }
}

#[test]
fn r29_bug_a_unemitted_parent_pm_via_scoped_mid_to_root_c3_rtl() {
    let dir = new_temp("r29_bug_a_rtl");
    let c3 = "set_cpf_version 2.0\nupdate_power_mode -name PM_MIX -illegal\nend_design\n";
    let mid_only = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_MID} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_MID -instances {u_sub}\n\
        update_power_domain -name PD_MID -primary_power_net VDD_MID -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_MID_ON -domain_conditions {PD_MID@NC_ON} -condition {pwr_en} -default\n\
        end_design\n";
    let master_head = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_MIX -domain_conditions {PD_TOP@NC_ON u_wrap/PD_MID@NC_OFF} -condition {other_en}\n\
        create_power_mode -name PM_TOP_ON -domain_conditions {PD_TOP@NC_ON u_wrap/PD_MID@NC_ON} -condition {pwr_en} -default\n";
    let master_seq = format!("{master_head}end_design\n");

    let v_seq = run_seq_sv(
        "r29_bug_a_seq",
        WRAP_SV,
        &[
            ("master_p.cpf", Some("/"), true, &master_seq),
            ("mid_only.cpf", Some("u_wrap"), true, mid_only),
            ("c3.cpf", Some("/"), true, c3),
        ],
    );

    let c3_p = dir.0.join("c3.cpf");
    std::fs::write(&c3_p, c3).unwrap();
    let mid_nest = format!(
        "set_cpf_version 2.0\n\
         create_power_nets -nets {{VDD_MID}} -voltage 0.9\n\
         create_ground_nets -nets {{VSS}}\n\
         create_power_domain -name PD_MID -instances {{u_sub}}\n\
         update_power_domain -name PD_MID -primary_power_net VDD_MID -primary_ground_net VSS\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_power_mode -name PM_MID_ON -domain_conditions {{PD_MID@NC_ON}} -condition {{pwr_en}} -default\n\
         load_upf \"{}\" -format cpf -scope /\n\
         end_design\n",
        c3_p.display()
    );
    let mid_p = dir.0.join("mid.cpf");
    std::fs::write(&mid_p, &mid_nest).unwrap();
    let master_nest = format!(
        "{master_head}load_upf \"{}\" -format cpf -scope u_wrap\nend_design\n",
        mid_p.display()
    );
    let v_nest = run_nested_sv(
        "r29_bug_a_nest",
        WRAP_SV,
        &[],
        "master.cpf",
        true,
        &master_nest,
    );
    assert_eq!(v_keys(&v_seq), v_keys(&v_nest));
    assert!(v_nest.is_empty(), "expected clean, got {v_nest:#?}");
}

#[test]
fn r29_bug_b_nested_set_design_collision_and_scoped_isolation_rtl() {
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
        create_power_mode -name PM_MIX -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF} -condition {other_en}\n\
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -condition {pwr_en} -default\n";
    let c3 = "set_cpf_version 2.0\nupdate_power_mode -name PM_MIX -illegal\nend_design\n";

    let v_seq = run_seq_sv(
        "r29_bug_b_seq",
        WRAP_SV,
        &[
            ("m0.cpf", None, true, m0),
            ("c1.cpf", Some("/"), true, c1_no_end),
            ("c3.cpf", Some("/"), true, c3),
        ],
    );
    let v_upf = run_nested_sv(
        "r29_bug_b_upf",
        WRAP_SV,
        &[("m0.cpf", m0), ("c1.cpf", c1_no_end), ("c3.cpf", c3)],
        "master.upf",
        false,
        "upf_version 2.1\nload_upf m0.cpf -format cpf\nload_upf c1.cpf -format cpf -scope /\nload_upf c3.cpf -format cpf -scope /\n",
    );
    let v_cpf = run_nested_sv(
        "r29_bug_b_cpf",
        WRAP_SV,
        &[("c1.cpf", c1_no_end), ("c3.cpf", c3)],
        "master.cpf",
        true,
        "set_cpf_version 2.0\nset_design top_master\nload_upf c1.cpf -format cpf -scope /\nload_upf c3.cpf -format cpf -scope /\nend_design\n",
    );
    assert_eq!(v_keys(&v_seq), v_keys(&v_upf));
    assert_eq!(v_keys(&v_seq), v_keys(&v_cpf));
    assert!(v_cpf.is_empty(), "expected clean, got {v_cpf:#?}");

    // Scoped child set_design + same-scope child with end_design preserves parent `set_design top`.
    let v_pres = run_nested_sv(
        "r29_bug_b_pres",
        WRAP_SV,
        &[
            ("wrap.cpf", "set_cpf_version 2.0\nset_design wrap_blk\n"),
            ("bogus.cpf", "set_cpf_version 2.0\nset_design bogus_child\nend_design\n"),
            ("c3.cpf", c3),
        ],
        "master.cpf",
        true,
        "set_cpf_version 2.0\n\
         set_design top\n\
         create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9\n\
         create_ground_nets -nets {VSS}\n\
         create_power_domain -name PD_TOP -default\n\
         create_power_domain -name PD_SUB -instances {u_wrap/u_sub}\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
         update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
         create_power_mode -name PM_MIX -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF} -condition {other_en}\n\
         create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -condition {pwr_en} -default\n\
         load_upf wrap.cpf -format cpf -scope u_wrap\n\
         load_upf bogus.cpf -format cpf -scope /\n\
         load_upf c3.cpf -format cpf -scope /\n\
         end_design\n",
    );
    assert!(v_pres.is_empty(), "expected clean, got {v_pres:#?}");
}

#[test]
fn r30_bug_c_nc_multihop_emitted_four_hop_and_implicit_parent_wins_rtl() {
    let dir = new_temp("r30_bug_c_rtl");
    let c3_nc = "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.9 -state on\nend_design\n";
    let c3_p = dir.0.join("c3_nc.cpf");
    std::fs::write(&c3_p, c3_nc).unwrap();
    let mid_only = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_MID} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_MID -instances {u_sub}\n\
        update_power_domain -name PD_MID -primary_power_net VDD_MID -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_MID_ON -domain_conditions {PD_MID@NC_ON} -condition {pwr_en} -default\n\
        end_design\n";
    let mid_nest = format!(
        "set_cpf_version 2.0\n\
         create_power_nets -nets {{VDD_MID}} -voltage 0.9\n\
         create_ground_nets -nets {{VSS}}\n\
         create_power_domain -name PD_MID -instances {{u_sub}}\n\
         update_power_domain -name PD_MID -primary_power_net VDD_MID -primary_ground_net VSS\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_power_mode -name PM_MID_ON -domain_conditions {{PD_MID@NC_ON}} -condition {{pwr_en}} -default\n\
         load_upf \"{}\" -format cpf -scope /\n\
         end_design\n",
        c3_p.display()
    );
    let mid_p = dir.0.join("mid.cpf");
    std::fs::write(&mid_p, &mid_nest).unwrap();

    let master_head = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_MIX -domain_conditions {PD_TOP@NC_ON u_wrap/PD_MID@NC_OFF} -condition {other_en}\n\
        create_power_mode -name PM_TOP_ON -domain_conditions {PD_TOP@NC_ON u_wrap/PD_MID@NC_ON} -condition {pwr_en} -default\n";

    let v_seq = run_seq_sv(
        "r30_nc_seq",
        WRAP_SV,
        &[
            (
                "master_p.cpf",
                Some("/"),
                true,
                &format!("{master_head}end_design\n"),
            ),
            ("mid_only.cpf", Some("u_wrap"), true, mid_only),
            ("c3_nc.cpf", Some("/"), true, c3_nc),
        ],
    );
    let v_upf = run_nested_sv(
        "r30_nc_upf",
        WRAP_SV,
        &[("master_p.cpf", &format!("{master_head}end_design\n"))],
        "master.upf",
        false,
        &format!(
            "upf_version 2.1\nload_upf master_p.cpf -format cpf -scope /\nload_upf \"{}\" -format cpf -scope u_wrap\n",
            mid_p.display()
        ),
    );
    for (tag, pre, post) in [
        ("unemitted_end", "", "end_design\n"),
        ("unemitted_noend", "", ""),
        ("emitted_before", "end_design\n", ""),
    ] {
        let v_cpf = run_nested_sv(
            &format!("r30_nc_{tag}"),
            WRAP_SV,
            &[],
            "master.cpf",
            true,
            &format!(
                "{master_head}{pre}load_upf \"{}\" -format cpf -scope u_wrap\n{post}",
                mid_p.display()
            ),
        );
        assert_eq!(v_keys(&v_seq), v_keys(&v_cpf), "{tag}: seq != cpf");
        assert_eq!(v_keys(&v_upf), v_keys(&v_cpf), "{tag}: upf != cpf");
        assert!(v_cpf.is_empty(), "{tag}: expected clean, got {v_cpf:#?}");
    }

    // 4-hop: master(/) -> mid1(u_wrap) -> mid2(u_inner) -> c3_nc(/)
    let sv4 = r#"
module sub_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_blk(input logic clk, input logic d, output logic q_in);
  sub_blk u_sub(.clk(clk), .d(d), .q(q_in));
endmodule
module wrap_blk(input logic clk, input logic d, output logic q_sub);
  inner_blk u_inner(.clk(clk), .d(d), .q_in(q_sub));
endmodule
module top(input logic clk, input logic d, input logic pwr_en, input logic other_en, output logic q_sub);
  wrap_blk u_wrap(.clk(clk), .d(d), .q_sub(q_sub));
endmodule
"#;
    let mid2_p = dir.0.join("mid2.cpf");
    std::fs::write(
        &mid2_p,
        format!(
            "set_cpf_version 2.0\n\
             create_power_nets -nets {{VDD_IN}} -voltage 0.9\n\
             create_ground_nets -nets {{VSS}}\n\
             create_power_domain -name PD_IN -instances {{u_sub}}\n\
             update_power_domain -name PD_IN -primary_power_net VDD_IN -primary_ground_net VSS\n\
             create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
             create_power_mode -name PM_IN_ON -domain_conditions {{PD_IN@NC_ON}} -condition {{pwr_en}} -default\n\
             load_upf \"{}\" -format cpf -scope /\n\
             end_design\n",
            c3_p.display()
        ),
    )
    .unwrap();
    let mid1_p = dir.0.join("mid1.cpf");
    std::fs::write(
        &mid1_p,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf -scope u_inner\n",
            mid2_p.display()
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
         create_power_mode -name PM_MIX -domain_conditions {{PD_TOP@NC_ON u_wrap/u_inner/PD_IN@NC_OFF}} -condition {{other_en}}\n\
         create_power_mode -name PM_TOP_ON -domain_conditions {{PD_TOP@NC_ON u_wrap/u_inner/PD_IN@NC_ON}} -condition {{pwr_en}} -default\n\
         load_upf \"{}\" -format cpf -scope u_wrap\n\
         end_design\n",
        mid1_p.display()
    );
    let v_4hop = run_nested_sv("r30_4hop", sv4, &[], "master4.cpf", true, &master4);
    assert!(v_4hop.is_empty(), "4-hop expected clean, got {v_4hop:#?}");

    // Preserved FIX3_IMPLICIT_SAME_SCOPE_PARENT_WINS across intermediate implicit hops:
    // master(/) defines NC_ON=0.9V/on, loads imp1 (no -scope) -> imp2 (no -scope) setting NC_ON=0.0V/off.
    let imp2_p = dir.0.join("imp2.cpf");
    std::fs::write(
        &imp2_p,
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_ON -voltage 0.0 -state off\nend_design\n",
    )
    .unwrap();
    let imp1_p = dir.0.join("imp1.cpf");
    std::fs::write(
        &imp1_p,
        format!(
            "set_cpf_version 2.0\nload_upf \"{}\" -format cpf\n",
            imp2_p.display()
        ),
    )
    .unwrap();
    let v_fix3 = run_nested_sv(
        "r30_fix3_imp",
        WRAP_SV,
        &[],
        "master_fix3.cpf",
        true,
        &format!(
            "set_cpf_version 2.0\n\
             create_power_nets -nets {{VDD_AON VDD_SUB}} -voltage 0.9\n\
             create_ground_nets -nets {{VSS}}\n\
             create_power_domain -name PD_TOP -default\n\
             create_power_domain -name PD_SUB -instances {{u_wrap/u_sub}}\n\
             update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
             update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
             create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
             create_power_mode -name PM_ON -domain_conditions {{PD_TOP@NC_ON PD_SUB@NC_ON}} -condition {{pwr_en}} -default\n\
             load_upf \"{}\" -format cpf\n\
             end_design\n",
            imp1_p.display()
        ),
    );
    assert!(
        v_fix3.is_empty(),
        "FIX3 parent-wins expected clean, got {v_fix3:#?}"
    );
}
