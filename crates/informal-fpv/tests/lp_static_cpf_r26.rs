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
        "informal_lp_cpf_r26_{}_{n}_{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&p).expect("mkdir");
    TempDir(p)
}

const DESIGN_SV: &str = r#"
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
  output logic q_sub
);
  wrap_blk u_wrap(.clk(clk), .d(d), .q_sub(q_sub));
endmodule
"#;

fn run_seq(
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

fn run_nested(
    tag: &str,
    extra_files: &[(&str, &str)],
    top_body: &str,
    stage: LpStage,
) -> Vec<LpViolation> {
    let dir = new_temp(tag);
    let sv_path = dir.0.join("design.sv");
    std::fs::write(&sv_path, DESIGN_SV).expect("write sv");
    for (fname, body) in extra_files {
        std::fs::write(dir.0.join(fname), body).expect("write child");
    }
    let top_path = dir.0.join("top.upf");
    std::fs::write(&top_path, top_body).expect("write top");
    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    s.design.files.push(sv_path);
    s.elaborate(Some("top".to_string())).expect("elaborate");
    load_upf(&mut s, &top_path, None, None, false).expect("load_upf");
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
fn bug1_clean_shutoff_off_upd_then_sw_rule_and_aliases_seq_matches_nested() {
    let c1 = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB VDD_EXT} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub} -shutoff_condition {!pwr_en}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        end_design\n";
    let c2 = "set_cpf_version 2.0\n\
        create_power_switch_rule -name SW_SUB -domain PD_SUB -external_power_net VDD_EXT -enable_condition {new_en}\n\
        end_design\n";

    let c1_sw = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB VDD_EXT} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub} -shutoff_condition {!pwr_en}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        create_power_switch_rule -name SW_SUB -domain PD_SUB -external_power_net VDD_EXT\n\
        end_design\n";
    let c2_upd = "set_cpf_version 2.0\n\
        update_power_switch_rule -name SW_SUB -enable_condition {new_en}\n\
        end_design\n";

    for (tag, c1_body, c2_body, target) in [
        ("bug1_ss", c1, c2, "SS_PD_SUB"),
        ("bug1_primary", c1, c2, "PD_SUB.primary"),
        ("bug1_dom", c1, c2, "PD_SUB"),
        ("bug1c_upd_sw", c1_sw, c2_upd, "SS_PD_SUB"),
    ] {
        let u = format!(
            "upf_version 2.1\n\
             add_power_state {target} -state {{OFF -update -simstate NORMAL -supply_expr {{power == {{FULL_ON 0.9}} && ground == {{FULL_ON 0.0}}}} -logic_expr {{new_en}}}}\n"
        );
        let v_seq = run_seq(
            tag,
            &[
                ("c1.cpf", None, true, c1_body),
                ("u.upf", Some("/"), false, &u),
                ("c2.cpf", Some("/"), true, c2_body),
            ],
            LpStage::All,
        );
        let v_nest = run_nested(
            &format!("{tag}_nest"),
            &[("c1.cpf", c1_body), ("u.upf", &u), ("c2.cpf", c2_body)],
            "upf_version 2.1\nload_upf c1.cpf -format cpf\nload_upf u.upf -scope /\nload_upf c2.cpf -format cpf -scope /\n",
            LpStage::All,
        );
        assert_eq!(v_keys(&v_seq), v_keys(&v_nest), "{tag}: seq != nest");
        assert!(v_seq.is_empty(), "{tag}: expected clean, got {v_seq:#?}");
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
    let v_seq = run_seq(
        "bug1_scoped",
        &[
            ("c1.cpf", Some("/u_wrap"), true, c1_sub),
            ("u.upf", Some("/"), false, u_sub),
            ("c2.cpf", Some("/u_wrap"), true, c2),
        ],
        LpStage::All,
    );
    let v_nest = run_nested(
        "bug1_scoped_nest",
        &[("c1.cpf", c1_sub), ("u.upf", u_sub), ("c2.cpf", c2)],
        "upf_version 2.1\nload_upf c1.cpf -format cpf -scope /u_wrap\nload_upf u.upf -scope /\nload_upf c2.cpf -format cpf -scope /u_wrap\n",
        LpStage::All,
    );
    assert_eq!(v_keys(&v_seq), v_keys(&v_nest));
    assert!(v_seq.is_empty(), "scoped bug1 expected clean: {v_seq:#?}");
}

#[test]
fn bug2_off_upd_plus_nc_refresh_and_late_psw_seq_matches_nested() {
    let c1 = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub}\n\
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

    for (tag, target) in [
        ("bug2_ss", "SS_PD_SUB"),
        ("bug2_abs", "/SS_PD_SUB"),
        ("bug2_primary", "PD_SUB.primary"),
        ("bug2_dom", "PD_SUB"),
    ] {
        let u = format!(
            "upf_version 2.1\n\
             add_power_state {target} -state {{OFF -update -supply_expr {{power == OFF && ground == {{FULL_ON 0.0}}}} -logic_expr {{!other_en}} -simstate CORRUPT}}\n"
        );
        let v_seq = run_seq(
            tag,
            &[
                ("c1.cpf", None, true, c1),
                ("u.upf", Some("/"), false, &u),
                ("c2.cpf", Some("/"), true, c2),
                ("c3.cpf", Some("/"), true, c3),
            ],
            LpStage::All,
        );
        let v_nest = run_nested(
            &format!("{tag}_nest"),
            &[("c1.cpf", c1), ("u.upf", &u), ("c2.cpf", c2), ("c3.cpf", c3)],
            "upf_version 2.1\nload_upf c1.cpf -format cpf\nload_upf u.upf\nload_upf c2.cpf -format cpf\nload_upf c3.cpf -format cpf\n",
            LpStage::All,
        );
        assert_eq!(v_keys(&v_seq), v_keys(&v_nest), "{tag}: seq != nest");
        assert!(v_seq.is_empty(), "{tag}: expected clean, got {v_seq:#?}");
    }

    // c1 -> c2 -> u.upf(OFF -update) -> c3(NC OFF=0.9V on)
    let u = "upf_version 2.1\nadd_power_state SS_PD_SUB -state {OFF -update -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {!other_en} -simstate CORRUPT}\n";
    let v_seq_u = run_seq(
        "bug2_c1_c2_u_c3",
        &[
            ("c1.cpf", None, true, c1),
            ("c2.cpf", Some("/"), true, c2),
            ("u.upf", Some("/"), false, u),
            ("c3.cpf", Some("/"), true, c3),
        ],
        LpStage::All,
    );
    let v_nest_u = run_nested(
        "bug2_c1_c2_u_c3_nest",
        &[("c1.cpf", c1), ("c2.cpf", c2), ("u.upf", u), ("c3.cpf", c3)],
        "upf_version 2.1\nload_upf c1.cpf -format cpf\nload_upf c2.cpf -format cpf\nload_upf u.upf\nload_upf c3.cpf -format cpf\n",
        LpStage::All,
    );
    assert_eq!(v_keys(&v_seq_u), v_keys(&v_nest_u));
    assert!(v_seq_u.is_empty(), "got {v_seq_u:#?}");

    // c1 -> c2(PM1: PD_TOP@NC_ON, PD_SUB@OFF) -> u.upf(SS_PD_SUB OFF -update) -> c3(NC_ON)
    let c3_nc_on = "set_cpf_version 2.0\ncreate_nominal_condition -name NC_ON -voltage 0.8 -state on\nend_design\n";
    let v_seq_p = run_seq(
        "bug2_keep_sub",
        &[
            ("c1.cpf", None, true, c1),
            ("c2.cpf", Some("/"), true, c2),
            ("u.upf", Some("/"), false, u),
            ("c3.cpf", Some("/"), true, c3_nc_on),
        ],
        LpStage::All,
    );
    let v_nest_p = run_nested(
        "bug2_keep_sub_nest",
        &[("c1.cpf", c1), ("c2.cpf", c2), ("u.upf", u), ("c3.cpf", c3_nc_on)],
        "upf_version 2.1\nload_upf c1.cpf -format cpf\nload_upf c2.cpf -format cpf\nload_upf u.upf\nload_upf c3.cpf -format cpf\n",
        LpStage::All,
    );
    assert_eq!(v_keys(&v_seq_p), v_keys(&v_nest_p));
    assert!(v_seq_p.iter().any(|v| v.rule_id == "ISO_MISSING"));

    // LATE_PSW_AND_PM_IN_C2
    let c1_late = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB VDD_EXT} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        end_design\n";
    let c2_late = "set_cpf_version 2.0\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_switch_rule -name SW_SUB -domain PD_SUB -external_power_net VDD_EXT -enable_condition {pwr_en}\n\
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -condition {pwr_en} -default\n\
        end_design\n";
    let v_seq_l = run_seq(
        "late_psw_pm_c2",
        &[
            ("c1.cpf", None, true, c1_late),
            ("u.upf", Some("/"), false, u),
            ("c2.cpf", Some("/"), true, c2_late),
        ],
        LpStage::All,
    );
    let v_nest_l = run_nested(
        "late_psw_pm_c2_nest",
        &[("c1.cpf", c1_late), ("u.upf", u), ("c2.cpf", c2_late)],
        "upf_version 2.1\nload_upf c1.cpf -format cpf\nload_upf u.upf\nload_upf c2.cpf -format cpf\n",
        LpStage::All,
    );
    assert_eq!(
        v_keys(&v_seq_l),
        v_keys(&v_nest_l),
        "LATE_PSW_AND_PM_IN_C2 seq != nest"
    );
    assert!(
        v_seq_l.is_empty(),
        "LATE_PSW_AND_PM_IN_C2 expected clean: {v_seq_l:#?}"
    );
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

    let v_ctrl = run_seq(
        "adv1_ctrl",
        &[
            ("c1.cpf", Some("u_wrap"), true, c1_wrap),
            ("c2.cpf", Some("/"), true, c2_mix_direct),
        ],
        LpStage::All,
    );
    assert!(
        v_ctrl.is_empty(),
        "direct -illegal expected clean: {v_ctrl:#?}"
    );

    let v_seq = run_seq(
        "adv1_seq",
        &[
            ("c1.cpf", Some("u_wrap"), true, c1_wrap),
            ("c2.cpf", Some("/"), true, c2_mix_upd),
            ("c3.cpf", Some("/"), true, c3_illegal),
        ],
        LpStage::All,
    );
    let v_nest = run_nested(
        "adv1_nest",
        &[
            ("c1.cpf", c1_wrap),
            ("c2.cpf", c2_mix_upd),
            ("c3.cpf", c3_illegal),
        ],
        "upf_version 2.1\nload_upf c1.cpf -format cpf -scope u_wrap\nload_upf c2.cpf -format cpf -scope /\nload_upf c3.cpf -format cpf -scope /\n",
        LpStage::All,
    );
    assert_eq!(v_keys(&v_seq), v_keys(&v_nest), "ADV1 seq != nest");
    assert_eq!(v_keys(&v_seq), v_keys(&v_ctrl), "ADV1 upd != direct");
    assert!(v_seq.is_empty(), "ADV1 expected clean: {v_seq:#?}");

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
    let v_dt_seq = run_seq(
        "adv1_dt_seq",
        &[
            ("c1.cpf", Some("u_wrap"), true, &c1_top),
            ("c2.cpf", Some("/"), true, c2_top),
            ("c3.cpf", Some("/"), true, c3_top),
        ],
        LpStage::All,
    );
    let v_dt_nest = run_nested(
        "adv1_dt_nest",
        &[("c1.cpf", &c1_top), ("c2.cpf", c2_top), ("c3.cpf", c3_top)],
        "upf_version 2.1\nload_upf c1.cpf -format cpf -scope u_wrap\nload_upf c2.cpf -format cpf -scope /\nload_upf c3.cpf -format cpf -scope /\n",
        LpStage::All,
    );
    assert_eq!(v_keys(&v_dt_seq), v_keys(&v_dt_nest));
    assert!(
        v_dt_seq.is_empty(),
        "ADV1 set_design top expected clean: {v_dt_seq:#?}"
    );

    // Negative isolation control: u_a/PM_X vs u_b/PM_X non-overlapping scoped tables
    let ab_sv = "module leaf(input logic clk, input logic d, output logic q);\n\
        always_ff @(posedge clk) q <= d;\nendmodule\n\
        module top(input logic clk, input logic d, input logic sel_x, output logic qa, output logic qb);\n\
        leaf u_a(.clk(clk), .d(d), .q(qa));\n\
        leaf u_b(.clk(clk), .d(d), .q(qb));\nendmodule\n";
    let mk_sc = |pd: &str, vdd: &str| {
        format!(
            "set_cpf_version 2.0\n\
             create_power_nets -nets {{{vdd}}} -voltage 0.9\n\
             create_ground_nets -nets {{VSS}}\n\
             create_power_domain -name {pd} -default\n\
             update_power_domain -name {pd} -primary_power_net {vdd} -primary_ground_net VSS\n\
             create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
             create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
             create_power_mode -name PM_KEEP -domain_conditions {{{pd}@NC_ON}} -default\n\
             create_power_mode -name PM_X -domain_conditions {{{pd}@NC_OFF}} -condition {{sel_x}}\n\
             end_design\n"
        )
    };
    let dir = new_temp("adv1_ab_iso");
    let sv_p = dir.0.join("ab.sv");
    std::fs::write(&sv_p, ab_sv).unwrap();
    let ca_p = dir.0.join("ca.cpf");
    let cb_p = dir.0.join("cb.cpf");
    let cu_p = dir.0.join("cu.cpf");
    std::fs::write(&ca_p, mk_sc("PD_A", "VDD_A")).unwrap();
    std::fs::write(&cb_p, mk_sc("PD_B", "VDD_B")).unwrap();
    std::fs::write(
        &cu_p,
        "set_cpf_version 2.0\nupdate_power_mode -name PM_X -illegal\nend_design\n",
    )
    .unwrap();
    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    s.design.files.push(sv_p);
    s.elaborate(Some("top".to_string())).unwrap();
    load_upf(&mut s, &ca_p, Some("u_a"), None, true).unwrap();
    load_upf(&mut s, &cb_p, Some("u_b"), None, true).unwrap();
    load_upf(&mut s, &cu_p, Some("u_a"), None, true).unwrap();
    check_lp(
        &mut s,
        &CheckLpOpts {
            stage: LpStage::All,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        !s.lp.violations.iter().any(|v| v.signal.contains("PD_A")),
        "u_a/PM_X should be illegal so PD_A cannot turn off: {:?}",
        s.lp.violations
    );
    let resolved = s.lp.resolved.as_ref().expect("resolved");
    let pd_b = resolved
        .intent
        .domains
        .iter()
        .find(|d| d.name == "u_b/PD_B")
        .expect("u_b/PD_B");
    let ss_b = resolved.primary_ss(pd_b.id).expect("ss_b");
    assert!(
        resolved.ss_states[ss_b.0 as usize]
            .iter()
            .any(|st| !st.illegal && st.rail_states.iter().any(|r| r.is_off)),
        "u_b/PM_X must remain intact so u_b/PD_B still has legal OFF state: {:?}",
        resolved.ss_states[ss_b.0 as usize]
    );
}
