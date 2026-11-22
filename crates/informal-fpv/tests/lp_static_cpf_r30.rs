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
        "informal_lp_cpf_r30_{}_{n}_{tag}",
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
  input  logic other_en,
  output logic q_sub
);
  wrap_blk u_wrap(.clk(clk), .d(d), .q_sub(q_sub));
endmodule
"#;

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
fn r31_bug_d_p1_p2_p3_p4_e2_and_reauthor_rtl_static() {
    let c_imp_on = "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.9 -state on\nend_design\n";
    let mid_slash = "set_cpf_version 2.0\nload_upf \"c_imp_on.cpf\" -format cpf\nend_design\n";
    let mid_wrap = "set_cpf_version 2.0\nload_upf \"mid_slash.cpf\" -format cpf -scope /\n";

    let p1_head = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF} -condition {other_en}\n\
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -condition {pwr_en} -default\n";

    // P1a == P1b == P1d: master NC_OFF=0.0/off wins over implicit child under -scope / or u_wrap->/.
    let v_p1a = run_nested_sv(
        "p1a",
        WRAP_SV,
        &[("c_imp_on.cpf", c_imp_on)],
        "master.cpf",
        true,
        &format!("{p1_head}load_upf \"c_imp_on.cpf\" -format cpf\nend_design\n"),
    );
    let v_p1b = run_nested_sv(
        "p1b",
        WRAP_SV,
        &[("c_imp_on.cpf", c_imp_on), ("mid_slash.cpf", mid_slash)],
        "master.cpf",
        true,
        &format!("{p1_head}load_upf \"mid_slash.cpf\" -format cpf -scope /\nend_design\n"),
    );
    let v_p1d = run_nested_sv(
        "p1d",
        WRAP_SV,
        &[
            ("c_imp_on.cpf", c_imp_on),
            ("mid_slash.cpf", mid_slash),
            ("mid_wrap.cpf", mid_wrap),
        ],
        "master.cpf",
        true,
        &format!("{p1_head}load_upf \"mid_wrap.cpf\" -format cpf -scope u_wrap\nend_design\n"),
    );
    assert_eq!(v_keys(&v_p1a), v_keys(&v_p1b));
    assert_eq!(v_keys(&v_p1a), v_keys(&v_p1d));
    assert!(
        v_p1a.iter().any(|v| v.rule_id == "ISO_MISSING"),
        "expected ISO_MISSING when NC_OFF=0.0V/off wins, got {v_p1a:#?}"
    );

    // P2a == P1e == P3c: master NC_ON=0.9/on wins (no PST_INCONSISTENT).
    let c_imp_off = "set_cpf_version 2.0\ncreate_nominal_condition -name NC_ON -voltage 0.0 -state off\nend_design\n";
    let mid_cpf_off = "set_cpf_version 2.0\nload_upf \"c_imp_off.cpf\" -format cpf\n";
    let mid_upf_off = "upf_version 2.1\nload_upf \"c_imp_off.cpf\" -format cpf\n";
    let p2_head = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_TOP -domain_conditions {PD_TOP@NC_ON} -condition {pwr_en} -default\n";
    for (tag, body) in [
        (
            "p2a",
            format!("{p2_head}load_upf \"c_imp_off.cpf\" -format cpf\nend_design\n"),
        ),
        (
            "p1e",
            format!("{p2_head}load_upf \"mid_cpf_off.cpf\" -format cpf -scope /\nend_design\n"),
        ),
        (
            "p3c_noend",
            format!("{p2_head}load_upf \"mid_upf_off.upf\" -scope /\nend_design\n"),
        ),
        (
            "p3c_emitted",
            format!("{p2_head}end_design\nload_upf \"mid_upf_off.upf\" -scope /\n"),
        ),
    ] {
        let v = run_nested_sv(
            tag,
            WRAP_SV,
            &[
                ("c_imp_off.cpf", c_imp_off),
                ("mid_cpf_off.cpf", mid_cpf_off),
                ("mid_upf_off.upf", mid_upf_off),
            ],
            "master.cpf",
            true,
            &body,
        );
        assert!(v.is_empty(), "{tag}: expected clean, got {v:#?}");
    }

    // P4b == P4d vs P4c (scoped NC_OFF at u_wrap)
    let mid_wrap_head = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_SUB -instances {u_sub}\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_SUB_OFF -domain_conditions {PD_SUB@NC_OFF} -condition {other_en}\n\
        create_power_mode -name PM_SUB_ON -domain_conditions {PD_SUB@NC_ON} -condition {pwr_en} -default\n";
    let root_top = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_power_mode -name PM_TOP -domain_conditions {PD_TOP@NC_ON} -condition {pwr_en} -default\n\
        load_upf \"mid_wrap.cpf\" -format cpf -scope u_wrap\n\
        end_design\n";
    let v_p4b = run_nested_sv(
        "p4b",
        WRAP_SV,
        &[
            ("sub_imp.cpf", c_imp_on),
            (
                "mid_wrap.cpf",
                &format!("{mid_wrap_head}load_upf \"sub_imp.cpf\" -format cpf\nend_design\n"),
            ),
        ],
        "master.cpf",
        true,
        root_top,
    );
    let v_p4d = run_nested_sv(
        "p4d",
        WRAP_SV,
        &[
            ("sub_imp.cpf", c_imp_on),
            (
                "mid2.cpf",
                "set_cpf_version 2.0\nload_upf \"sub_imp.cpf\" -format cpf\n",
            ),
            (
                "mid_wrap.cpf",
                &format!(
                    "{mid_wrap_head}load_upf \"mid2.cpf\" -format cpf -scope /u_wrap\nend_design\n"
                ),
            ),
        ],
        "master.cpf",
        true,
        root_top,
    );
    let v_p4c = run_nested_sv(
        "p4c",
        WRAP_SV,
        &[
            ("sub_exp.cpf", c_imp_on),
            ("mid_wrap.cpf", &format!("{mid_wrap_head}load_upf \"sub_exp.cpf\" -format cpf -scope /u_wrap\nend_design\n")),
        ],
        "master.cpf",
        true,
        root_top,
    );
    assert_eq!(v_keys(&v_p4b), v_keys(&v_p4d));
    assert!(v_p4b.iter().any(|v| v.rule_id == "ISO_MISSING"));
    assert!(v_p4c.is_empty(), "P4c expected clean, got {v_p4c:#?}");

    // E2 (implicit_mid -> mid(-scope u_wrap) -> c3(-scope /)) & ADV_IMPLICIT_MID_REAUTHOR_CLEARS_BADGE
    let v_e2 = run_nested_sv(
        "e2",
        WRAP_SV,
        &[
            ("c3.cpf", c_imp_on),
            (
                "mid_w.cpf",
                "set_cpf_version 2.0\nload_upf \"c3.cpf\" -format cpf -scope /\n",
            ),
            (
                "imp_mid.cpf",
                "set_cpf_version 2.0\nload_upf \"mid_w.cpf\" -format cpf -scope u_wrap\n",
            ),
        ],
        "master.cpf",
        true,
        &format!("{p1_head}load_upf \"imp_mid.cpf\" -format cpf\nend_design\n"),
    );
    assert!(v_e2.is_empty(), "E2 expected clean, got {v_e2:#?}");

    let p_standby = p1_head.replace("0.0 -state off", "0.5 -state standby");
    let v_reauth = run_nested_sv(
        "reauth",
        WRAP_SV,
        &[
            ("c3.cpf", c_imp_on),
            (
                "imp_mid.cpf",
                "set_cpf_version 2.0\nload_upf \"c3.cpf\" -format cpf -scope /\ncreate_nominal_condition -name NC_OFF -voltage 0.0 -state off\n",
            ),
        ],
        "master.cpf",
        true,
        &format!("{p_standby}load_upf \"imp_mid.cpf\" -format cpf\nend_design\n"),
    );
    assert!(
        v_reauth.iter().any(|v| v.rule_id == "LS_MISSING")
            && !v_reauth.iter().any(|v| v.rule_id == "ISO_MISSING"),
        "expected LS_MISSING (0.5V standby wins) without ISO_MISSING, got {v_reauth:#?}"
    );
}

#[test]
fn r31_mixed_p3a_p3b_and_p3c_with_child_power_mode_refresh() {
    let c3_on = "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.9 -state on\nend_design\n";
    let p1_head = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.0 -state off\n\
        create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF} -condition {other_en}\n\
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -condition {pwr_en} -default\n";

    // P3a: CPF -> UPF(-scope u_wrap) -> CPF(-scope /)
    let v_p3a = run_nested_sv(
        "p3a",
        WRAP_SV,
        &[
            ("c3.cpf", c3_on),
            (
                "mid.upf",
                "upf_version 2.1\nload_upf \"c3.cpf\" -format cpf -scope /\n",
            ),
        ],
        "master.cpf",
        true,
        &format!("{p1_head}end_design\nload_upf \"mid.upf\" -scope u_wrap\n"),
    );
    assert!(v_p3a.is_empty(), "P3a expected clean, got {v_p3a:#?}");

    // P3b: UPF -> CPF(-scope /) -> UPF(-scope u_wrap) -> CPF(-scope /)
    let v_p3b = run_nested_sv(
        "p3b",
        WRAP_SV,
        &[
            ("c3.cpf", c3_on),
            (
                "mid.upf",
                "upf_version 2.1\nload_upf \"c3.cpf\" -format cpf -scope /\n",
            ),
            (
                "base.cpf",
                &format!("{p1_head}load_upf \"mid.upf\" -scope u_wrap\nend_design\n"),
            ),
        ],
        "top.upf",
        false,
        "upf_version 2.1\nload_upf \"base.cpf\" -format cpf -scope /\n",
    );
    assert!(v_p3b.is_empty(), "P3b expected clean, got {v_p3b:#?}");

    // P3c where c_imp_off also defines a child power_mode using NC_ON: master's NC_ON=0.9V must refresh it.
    let c_imp_pm = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub}\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.0 -state off\n\
        create_power_mode -name PM_SUB -domain_conditions {PD_SUB@NC_ON} -condition {pwr_en} -default\n\
        end_design\n";
    let v_p3c_pm = run_nested_sv(
        "p3c_pm",
        WRAP_SV,
        &[
            ("c_imp_pm.cpf", c_imp_pm),
            ("mid.upf", "upf_version 2.1\nload_upf \"c_imp_pm.cpf\" -format cpf\n"),
        ],
        "master.cpf",
        true,
        "set_cpf_version 2.0\n\
         create_power_nets -nets {VDD_AON} -voltage 0.9\n\
         create_ground_nets -nets {VSS}\n\
         create_power_domain -name PD_TOP -default\n\
         update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
         create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
         create_power_mode -name PM_TOP -domain_conditions {PD_TOP@NC_ON} -condition {pwr_en} -default\n\
         end_design\n\
         load_upf \"mid.upf\" -scope /\n",
    );
    assert!(
        v_p3c_pm.is_empty(),
        "P3c with child PM using NC_ON must refresh to 0.9V/on (zero ISO_MISSING/PST_INCONSISTENT), got {v_p3c_pm:#?}"
    );
}

#[test]
fn r32_bug_e_ch1_mid_and_ch9_upf_mid_cexp_then_cimp_rtl_static() {
    let c_exp =
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.9 -state on\nend_design\n";
    let c_imp =
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.0 -state off\nend_design\n";
    let mid_cpf =
        "set_cpf_version 2.0\nload_upf \"c_exp.cpf\" -format cpf -scope /\nload_upf \"c_imp.cpf\" -format cpf\n";
    let mid_upf =
        "upf_version 2.1\nload_upf \"c_exp.cpf\" -format cpf -scope /\nload_upf \"c_imp.cpf\" -format cpf\n";
    let mid1_upf = "upf_version 2.1\nload_upf \"c_exp.cpf\" -format cpf -scope /\n";
    let mid2_upf = "upf_version 2.1\nload_upf \"c_imp.cpf\" -format cpf\n";

    let master_head = "set_cpf_version 2.0\n\
        create_power_nets -nets {VDD_AON VDD_SUB} -voltage 0.9\n\
        create_ground_nets -nets {VSS}\n\
        create_power_domain -name PD_TOP -default\n\
        create_power_domain -name PD_SUB -instances {u_wrap/u_sub}\n\
        update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\n\
        update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS\n\
        create_nominal_condition -name NC_ON -voltage 0.9 -state on\n\
        create_nominal_condition -name NC_OFF -voltage 0.5 -state standby\n\
        create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF} -condition {other_en}\n\
        create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -condition {pwr_en} -default\n";

    let extra = [
        ("c_exp.cpf", c_exp),
        ("c_imp.cpf", c_imp),
        ("mid.cpf", mid_cpf),
        ("mid.upf", mid_upf),
        ("mid1.upf", mid1_upf),
        ("mid2.upf", mid2_upf),
    ];

    for (tag, top_body) in [
        (
            "ch1_direct",
            format!("{master_head}load_upf \"c_exp.cpf\" -format cpf -scope /\nload_upf \"c_imp.cpf\" -format cpf\nend_design\n"),
        ),
        (
            "ch1_mid_noscope",
            format!("{master_head}load_upf \"mid.cpf\" -format cpf\nend_design\n"),
        ),
        (
            "ch1_mid_slash",
            format!("{master_head}load_upf \"mid.cpf\" -format cpf -scope /\nend_design\n"),
        ),
        (
            "ch9_upf_mid",
            format!("{master_head}end_design\nload_upf \"mid.upf\" -scope /\n"),
        ),
        (
            "ch9_two_upf_mids",
            format!("{master_head}end_design\nload_upf \"mid1.upf\" -scope /\nload_upf \"mid2.upf\" -scope /\n"),
        ),
    ] {
        let viols = run_nested_sv(tag, WRAP_SV, &extra, "master.cpf", true, &top_body);
        assert!(viols.is_empty(), "{tag}: expected zero violations (0.9V/on wins), got {viols:#?}");
    }

    // Also verify TOP_UPF_C1_C2_C2_WINS_NESTED under RTL static: c2 (0.5V standby) overwrites c1 (0.9V on) -> LS_MISSING.
    let c1_on = format!("{master_head}end_design\n").replace("0.5 -state standby", "0.9 -state on");
    let c2_standby =
        "set_cpf_version 2.0\ncreate_nominal_condition -name NC_OFF -voltage 0.5 -state standby\nend_design\n";
    let v_top = run_nested_sv(
        "top_upf_c1_c2",
        WRAP_SV,
        &[("c1.cpf", &c1_on), ("c2.cpf", c2_standby)],
        "top.upf",
        false,
        "upf_version 2.1\nload_upf \"c1.cpf\" -format cpf\nload_upf \"c2.cpf\" -format cpf\n",
    );
    assert!(
        v_top.iter().any(|v| v.rule_id == "LS_MISSING")
            && !v_top.iter().any(|v| v.rule_id == "ISO_MISSING"),
        "TOP_UPF_C1_C2_C2_WINS_NESTED must let c2 (0.5V standby) win -> LS_MISSING, got {v_top:#?}"
    );
}
