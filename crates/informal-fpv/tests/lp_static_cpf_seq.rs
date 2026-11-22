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
        "informal_lp_cpf_seq_{}_{n}_{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&p).expect("mkdir");
    TempDir(p)
}

const SEQ_SV: &str = r#"
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
) -> (Session, Vec<LpViolation>) {
    let dir = new_temp(tag);
    let sv_path = dir.0.join("design.sv");
    std::fs::write(&sv_path, SEQ_SV).expect("write sv");
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
    let viols = s.lp.violations.clone();
    (s, viols)
}

#[test]
fn r17_sequential_upf_multi_cpf_produces_fallback_states_and_psw_ctrl_unpowered() {
    let top_upf = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
"#;
    let sub1_cpf = r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_WRAP VDD_SUB} -voltage 0.9
create_nominal_condition -name NC_ON  -voltage 0.9 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_domain -name PD_WRAP -instances {u_wrap} -exclude_instances {u_wrap/u_sub} -shutoff_condition {!pwr_en}
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net /VSS
create_power_domain -name PD_SUB -instances {u_wrap/u_sub} -shutoff_condition {!u_wrap/u_sub/wrap_bad_ctrl}
update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net /VSS
end_design
"#;
    let sub2_cpf = r#"
set_cpf_version 2.0
create_power_switch_rule -name PSW_WRAP -domain /PD_WRAP -external_power_net /VDD_AON
create_power_switch_rule -name PSW_SUB  -domain /PD_SUB  -external_power_net /VDD_AON
end_design
"#;
    let (s, viols) = run_seq_files(
        "upf_cpf_cpf",
        &[
            ("top.upf", None, false, top_upf),
            ("sub1.cpf", Some("/"), true, sub1_cpf),
            ("sub2.cpf", Some("/"), true, sub2_cpf),
        ],
        LpStage::Rtl,
    );
    let stats = s.lp.last_stats.as_ref().expect("stats");
    assert!(
        stats.cnf_vars > 1 && stats.cnf_clauses > 1,
        "expected non-trivial PstSat CNF from fallback ON/OFF states: {stats:?}"
    );
    assert!(
        !viols.iter().any(|v| v.rule_id == "PSW_SUPPLY_UNCONNECTED"),
        "unexpected PSW_SUPPLY_UNCONNECTED: {viols:#?}"
    );
    assert!(
        viols
            .iter()
            .any(|v| v.rule_id == "PSW_CTRL_UNPOWERED" && v.signal.contains("wrap_bad_ctrl")),
        "expected PSW_CTRL_UNPOWERED on wrap_bad_ctrl: {viols:#?}"
    );

    // Also verify cross-scope [UPF(/), CPF(u_wrap/u_sub), CPF(/)] preserves authoring scope & nominal_conditions.
    let sub1_scoped = r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_SUB} -voltage 0.9
create_power_domain -name PD_SUB -instances {.} -shutoff_condition {!wrap_bad_ctrl}
update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net /VSS
end_design
"#;
    let sub2_scoped = r#"
set_cpf_version 2.0
create_power_switch_rule -name PSW_SUB -domain u_wrap/u_sub/PD_SUB -external_power_net /VDD_AON
end_design
"#;
    let (_, v_scoped) = run_seq_files(
        "upf_cpf_cpf_scoped",
        &[
            ("top.upf", None, false, top_upf),
            ("sub1_sc.cpf", Some("u_wrap/u_sub"), true, sub1_scoped),
            ("sub2_sc.cpf", Some("/"), true, sub2_scoped),
        ],
        LpStage::Rtl,
    );
    assert!(
        v_scoped
            .iter()
            .any(|v| v.rule_id == "PSW_CTRL_UNPOWERED" && v.signal.contains("wrap_bad_ctrl")),
        "expected PSW_CTRL_UNPOWERED on cross-scope [UPF, CPF, CPF]: {v_scoped:#?}"
    );
    assert!(
        !v_scoped
            .iter()
            .any(|v| v.rule_id == "PSW_SUPPLY_UNCONNECTED"),
        "unexpected PSW_SUPPLY_UNCONNECTED on cross-scope [UPF, CPF, CPF]: {v_scoped:#?}"
    );
}

#[test]
fn r17_sequential_cpf_end_design_repro_a_and_repro_b() {
    // Repro A: `sub_a.cpf` at `u_wrap/u_sub` w/ `end_design` + `top_a_seq.cpf` at `/` w/ `end_design`.
    let sub_a = r#"
set_cpf_version 2.0
create_power_domain -name PD_SUB -instances {.} -shutoff_condition {!wrap_bad_ctrl}
update_power_domain -name PD_SUB -primary_power_net /VDD_SUB -primary_ground_net /VSS
end_design
"#;
    let top_a = r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP VDD_SUB} -voltage 0.9
create_ground_nets -nets {VSS}
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {u_wrap} -exclude_instances {u_wrap/u_sub} -shutoff_condition {!pwr_en}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
create_power_switch_rule -name PSW_SUB -domain u_wrap/u_sub/PD_SUB -external_power_net VDD_AON
end_design
"#;
    let (_, va) = run_seq_files(
        "repro_a",
        &[
            ("sub_a.cpf", Some("u_wrap/u_sub"), true, sub_a),
            ("top_a_seq.cpf", Some("/"), true, top_a),
        ],
        LpStage::Rtl,
    );
    assert!(
        !va.iter().any(|v| v.rule_id == "PSW_SUPPLY_UNCONNECTED"),
        "Repro A: unexpected PSW_SUPPLY_UNCONNECTED: {va:#?}"
    );
    assert!(
        va.iter()
            .any(|v| v.rule_id == "PSW_CTRL_UNPOWERED" && v.signal.contains("wrap_bad_ctrl")),
        "Repro A: expected PSW_CTRL_UNPOWERED on wrap_bad_ctrl: {va:#?}"
    );
    assert!(
        va.iter().any(|v| v.rule_id == "ISO_MISSING"),
        "Repro A: expected ISO_MISSING: {va:#?}"
    );
    assert!(
        va.iter().any(|v| v.rule_id == "RET_NO_ELEMENTS"),
        "Repro A: expected RET_NO_ELEMENTS: {va:#?}"
    );

    // Repro B: `top_b.cpf` w/ `end_design` + `sub_b.cpf` updating `/PSW_WRAP` + `create_power_mode` + `end_design`.
    let top_b = r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
create_ground_nets -nets {VSS}
create_nominal_condition -name NC_ON -voltage 0.9 -state on
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!bad_shutoff}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
create_power_switch_rule -name PSW_WRAP -domain PD_WRAP -external_power_net VDD_AON
end_design
"#;
    let sub_b = r#"
set_cpf_version 2.0
update_power_switch_rule -name /PSW_WRAP -enable_condition_1 {/other_aon}
create_power_mode -name PM_ON -domain_conditions {/PD_TOP@NC_ON /PD_WRAP@NC_ON} -condition {!/other_aon} -default
end_design
"#;
    let (_, vb) = run_seq_files(
        "repro_b",
        &[
            ("top_b.cpf", None, true, top_b),
            ("sub_b.cpf", Some("u_wrap/u_sub"), true, sub_b),
        ],
        LpStage::Upf,
    );
    assert!(
        vb.iter().any(|v| v.rule_id == "PST_INCONSISTENT"),
        "Repro B: expected PST_INCONSISTENT: {vb:#?}"
    );
}

#[test]
fn r18_three_sequential_cpfs_and_upf_plus_three_cpfs_override_nominal_conditions() {
    // 1. Probe 3: [UPF(1.0V), CPF1(NC_ON=0.9V), CPF2(NC_ON=1.0V), CPF3(PM using NC_ON)] -> 0 LS_MISSING.
    let top_upf = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 1.0} && ground == {FULL_ON 0.0}} -simstate NORMAL}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
"#;
    let c1 = r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_SUB} -voltage 1.0
create_nominal_condition -name NC_ON -voltage 0.9 -state on
create_power_domain -name PD_SUB -instances {u_wrap/u_sub}
update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net /VSS
end_design
"#;
    let c2 = r#"
set_cpf_version 2.0
create_nominal_condition -name NC_ON -voltage 1.0 -state on
end_design
"#;
    let c3 = r#"
set_cpf_version 2.0
create_power_mode -name PM_ON -domain_conditions {/PD_SUB@NC_ON} -default
end_design
"#;
    let (_, vp3) = run_seq_files(
        "probe3_upf_3cpf",
        &[
            ("top.upf", None, false, top_upf),
            ("c1.cpf", Some("/"), true, c1),
            ("c2.cpf", Some("/"), true, c2),
            ("c3.cpf", Some("/"), true, c3),
        ],
        LpStage::Rtl,
    );
    assert!(
        !vp3.iter().any(|v| v.rule_id == "LS_MISSING"),
        "c2.cpf NC_ON=1.0V override must eliminate false LS_MISSING: {vp3:#?}"
    );

    // 2. run3.tcl: [s1.cpf(NC_OV=0.9V/on), s2_only.cpf(NC_OV=0.0V/off), s3_mode.cpf(PM_OV)] -> PST_INCONSISTENT.
    let s1 = r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
create_ground_nets -nets {VSS}
create_nominal_condition -name NC_OV -voltage 0.9 -state on
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
end_design
"#;
    let s2 = r#"
set_cpf_version 2.0
create_nominal_condition -name NC_OV -voltage 0.0 -state off
end_design
"#;
    let s3 = r#"
set_cpf_version 2.0
create_power_mode -name PM_OV -domain_conditions {/PD_WRAP@NC_OV} -condition {pwr_en} -default
end_design
"#;
    let (_, vrun3) = run_seq_files(
        "run3_tcl_3cpf",
        &[
            ("s1.cpf", None, true, s1),
            ("s2_only.cpf", Some("/"), true, s2),
            ("s3_mode.cpf", Some("/"), true, s3),
        ],
        LpStage::Upf,
    );
    assert!(
        vrun3.iter().any(|v| v.rule_id == "PST_INCONSISTENT"),
        "s2_only.cpf NC_OV=0.0V/off override must trigger PST_INCONSISTENT in s3_mode.cpf: {vrun3:#?}"
    );
}

#[test]
fn r19_cpf_upf_cpf_override_seq_distinct_modes_and_multi_end_design() {
    // 1. Bug 1: [CPF1, CPF2, UPF, CPF3] retains CPF2's NC_OV=0.0V/off override -> PST_INCONSISTENT.
    let s1 = r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
create_ground_nets -nets {VSS}
create_nominal_condition -name NC_OV -voltage 0.9 -state on
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
end_design
"#;
    let s2 = r#"
set_cpf_version 2.0
create_nominal_condition -name NC_OV -voltage 0.0 -state off
end_design
"#;
    let s3_upf = "upf_version 2.1\n";
    let s4 = r#"
set_cpf_version 2.0
create_power_mode -name PM_OV -domain_conditions {/PD_WRAP@NC_OV} -condition {pwr_en} -default
end_design
"#;
    let (_, vb1) = run_seq_files(
        "r19_bug1_cpf_cpf_upf_cpf",
        &[
            ("s1.cpf", None, true, s1),
            ("s2.cpf", Some("/"), true, s2),
            ("s3.upf", Some("/"), false, s3_upf),
            ("s4.cpf", Some("/"), true, s4),
        ],
        LpStage::Upf,
    );
    assert!(
        vb1.iter().any(|v| v.rule_id == "PST_INCONSISTENT"),
        "Bug 1: [CPF1, CPF2, UPF, CPF3] must emit PST_INCONSISTENT: {vb1:#?}"
    );

    // Also verify [UPF, CPF1, UPF2, CPF2] propagates CPF1's nominal_condition across UPF2.
    let c1_pos = r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
create_ground_nets -nets {VSS}
create_nominal_condition -name NC_OV -voltage 0.0 -state off
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
end_design
"#;
    let (_, vb1_upf_first) = run_seq_files(
        "r19_bug1_upf_cpf_upf_cpf",
        &[
            ("u0.upf", None, false, "upf_version 2.1\n"),
            ("c1.cpf", Some("/"), true, c1_pos),
            ("u2.upf", Some("/"), false, "upf_version 2.1\n"),
            ("c2.cpf", Some("/"), true, s4),
        ],
        LpStage::Upf,
    );
    assert!(
        vb1_upf_first
            .iter()
            .any(|v| v.rule_id == "PST_INCONSISTENT"),
        "Bug 1: [UPF, CPF1, UPF2, CPF2] must retain CPF1 state across UPF2: {vb1_upf_first:#?}"
    );

    // 2. Bug 2: Sequential [m1.cpf(PM_ON), m2.cpf(PM_OFF)] -> 0 PST_INCONSISTENT + RTL ISO_MISSING detected.
    let m1 = r#"
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
"#;
    let m2 = r#"
set_cpf_version 2.0
create_power_mode -name PM_OFF -domain_conditions {/PD_TOP@NC_ON /PD_WRAP@NC_OFF}
end_design
"#;
    let (_, vb2) = run_seq_files(
        "r19_bug2_seq_modes",
        &[("m1.cpf", None, true, m1), ("m2.cpf", Some("/"), true, m2)],
        LpStage::Rtl,
    );
    assert!(
        !vb2.iter().any(|v| v.rule_id == "PST_INCONSISTENT"),
        "Bug 2: sequential distinct power modes must not trigger PST_INCONSISTENT: {vb2:#?}"
    );
    assert!(
        vb2.iter().any(|v| v.rule_id == "ISO_MISSING"),
        "Bug 2: PM_OFF must allow PD_WRAP off while PD_TOP on -> ISO_MISSING: {vb2:#?}"
    );

    // 3. Bug 3: Single CPF file with two `set_design ... end_design` blocks evaluates second block.
    let multi = r#"
set_cpf_version 2.0
set_design sub_blk
create_power_nets -nets {VDD_SUB} -voltage 0.9
create_ground_nets -nets {VSS}
create_nominal_condition -name NC_ON -voltage 0.9 -state on
create_power_domain -name PD_SUB -instances {u_wrap/u_sub}
update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS
create_power_mode -name PM_SUB_ON -domain_conditions {PD_SUB@NC_ON} -default
end_design sub_blk

set_design top
create_power_nets -nets {VDD_WRAP} -voltage 0.9
create_nominal_condition -name NC_OV -voltage 0.0 -state off
create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
create_power_mode -name PM_OV -domain_conditions {/PD_WRAP@NC_OV} -condition {pwr_en} -default
end_design top
"#;
    let (_, vb3) = run_seq_files(
        "r19_bug3_multi_end_design",
        &[("multi.cpf", None, true, multi)],
        LpStage::Upf,
    );
    assert!(
        vb3.iter().any(|v| v.rule_id == "PST_INCONSISTENT"),
        "Bug 3: second set_design ... end_design block must trigger PST_INCONSISTENT: {vb3:#?}"
    );
}

#[test]
fn r20_nested_upf_multi_cpf_rtl_crossing_and_late_overrides() {
    let dir = new_temp("r20_nested_rtl");
    let sv_path = dir.0.join("design.sv");
    std::fs::write(&sv_path, SEQ_SV).expect("write sv");

    // 1. `hyp3_nested_rtl_crossing`: top.upf -> load_upf m1.cpf + load_upf m2.cpf detects ISO_MISSING.
    let m1 = dir.0.join("m1.cpf");
    let m2 = dir.0.join("m2.cpf");
    let top_upf = dir.0.join("top.upf");
    std::fs::write(
        &m1,
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
        &m2,
        "set_cpf_version 2.0\ncreate_power_mode -name PM_OFF -domain_conditions {/PD_TOP@NC_ON /PD_WRAP@NC_OFF}\nend_design\n",
    )
    .unwrap();
    std::fs::write(
        &top_upf,
        format!(
            "upf_version 2.1\nload_upf {} -format cpf -scope /\nload_upf {} -format cpf -scope /\n",
            m1.display(),
            m2.display()
        ),
    )
    .unwrap();

    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    s.design.files.push(sv_path);
    s.elaborate(Some("top".to_string())).expect("elaborate");
    load_upf(&mut s, &top_upf, None, None, false).expect("load_upf");
    check_lp(
        &mut s,
        &CheckLpOpts {
            stage: LpStage::Rtl,
            ..Default::default()
        },
    )
    .expect("check_lp");
    assert!(
        !s.lp
            .violations
            .iter()
            .any(|v| v.rule_id == "PST_UNREACHABLE_STATE" || v.rule_id == "PST_INCONSISTENT"),
        "unexpected PST errors: {:#?}",
        s.lp.violations
    );
    assert!(
        s.lp.violations
            .iter()
            .any(|v| v.rule_id == "ISO_MISSING" && v.signal.contains("u_wrap/q_wrap")),
        "expected ISO_MISSING on u_wrap/q_wrap: {:#?}",
        s.lp.violations
    );

    // 2. Late `create_nominal_condition` voltage override (0.9V -> 1.1V) after PM_ON emits `LS_MISSING`.
    let c_pm = r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9
create_ground_nets -nets {VSS}
create_nominal_condition -name NC_TOP -voltage 0.9 -state on
create_nominal_condition -name NC_WRAP -voltage 0.9 -state on
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_TOP PD_WRAP@NC_WRAP} -default
end_design
"#;
    let c_ov = r#"
set_cpf_version 2.0
create_nominal_condition -name NC_WRAP -voltage 1.1 -state on
end_design
"#;
    let (_, v_ls) = run_seq_files(
        "r20_late_nc_ls",
        &[
            ("c_pm.cpf", None, true, c_pm),
            ("c_ov.cpf", Some("/"), true, c_ov),
        ],
        LpStage::Rtl,
    );
    assert!(
        v_ls.iter().any(|v| v.rule_id == "LS_MISSING"),
        "late NC_WRAP 0.9V->1.1V override after PM_ON must trigger LS_MISSING: {v_ls:#?}"
    );

    // 3. Late `update_power_switch_rule` after `PM_ON` triggers RTL `PSW_CTRL_UNPOWERED` + `PST_INCONSISTENT`.
    let c_psw1 = r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP VDD_SUB} -voltage 0.9
create_ground_nets -nets {VSS}
create_nominal_condition -name NC_ON -voltage 0.9 -state on
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {u_wrap} -exclude_instances {u_wrap/u_sub} -shutoff_condition {!pwr_en}
create_power_domain -name PD_SUB -instances {u_wrap/u_sub}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS
create_power_switch_rule -name PSW_SUB -domain PD_SUB -external_power_net VDD_AON -enable_condition {other_aon}
create_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -condition {other_aon} -default
end_design
"#;
    let c_psw2 = r#"
set_cpf_version 2.0
update_power_switch_rule -name /PSW_SUB -enable_condition {!other_aon && u_wrap/u_sub/wrap_bad_ctrl}
end_design
"#;
    let (_, v_psw) = run_seq_files(
        "r20_late_psw_rtl",
        &[
            ("c_psw1.cpf", None, true, c_psw1),
            ("c_psw2.cpf", Some("/"), true, c_psw2),
        ],
        LpStage::Rtl,
    );
    assert!(
        v_psw.iter().any(|v| v.rule_id == "PST_INCONSISTENT"),
        "late update_power_switch_rule must emit PST_INCONSISTENT: {v_psw:#?}"
    );
    assert!(
        v_psw
            .iter()
            .any(|v| v.rule_id == "PSW_CTRL_UNPOWERED" && v.signal.contains("wrap_bad_ctrl")),
        "late update_power_switch_rule must emit PSW_CTRL_UNPOWERED: {v_psw:#?}"
    );
}

fn run_loaded_in_dir(
    dir: &std::path::Path,
    tag: &str,
    files: &[(&str, Option<&str>, bool)],
    stage: LpStage,
) -> Vec<LpViolation> {
    let sv_path = dir.join("design.sv");
    let mut s = Session::new(Vec::new(), dir.join(format!("work_{tag}")));
    s.design.files.push(sv_path);
    s.elaborate(Some("top".to_string())).expect("elaborate");
    for (fname, scope, is_cpf) in files {
        load_upf(&mut s, &dir.join(fname), *scope, None, *is_cpf).expect("load_upf");
    }
    check_lp(
        &mut s,
        &CheckLpOpts {
            stage,
            ..Default::default()
        },
    )
    .expect("check_lp");
    s.lp.violations
}

#[test]
fn r21_sequential_upf_cpf_interleave_and_scoped_nc_rtl() {
    let dir = new_temp("r21_f1_rtl");
    std::fs::write(dir.0.join("design.sv"), SEQ_SV).expect("write sv");
    let m1_path = dir.0.join("m1.cpf");
    std::fs::write(
        &m1_path,
        "set_cpf_version 2.0\ncreate_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9\ncreate_ground_nets -nets {VSS}\ncreate_power_domain -name PD_TOP -default\ncreate_power_domain -name PD_WRAP -instances {u_wrap}\nupdate_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\nupdate_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS\ncreate_power_switch_rule -name PSW_WRAP -domain PD_WRAP -external_power_net VDD_AON -enable_condition {1'b1}\nend_design\n",
    )
    .expect("write m1");
    std::fs::write(
        dir.0.join("u0.upf"),
        format!(
            "upf_version 2.1\nload_upf {} -format cpf -scope /\n",
            m1_path.display()
        ),
    )
    .expect("write u0");
    std::fs::write(dir.0.join("u1.upf"), "upf_version 2.1\n").expect("write u1");
    std::fs::write(
        dir.0.join("m2.cpf"),
        "set_cpf_version 2.0\nupdate_power_switch_rule -name /PSW_WRAP -enable_condition {1'b0}\nend_design\n",
    )
    .expect("write m2");

    // Finding 1: [u0(loads m1), u1, m2] emits 2 ISO_MISSING (u_wrap/q_sub & u_wrap/q_wrap).
    let v_f1 = run_loaded_in_dir(
        &dir.0,
        "f1",
        &[
            ("u0.upf", None, false),
            ("u1.upf", Some("/"), false),
            ("m2.cpf", Some("/"), true),
        ],
        LpStage::Rtl,
    );
    assert_eq!(
        v_f1.iter().filter(|v| v.rule_id == "ISO_MISSING").count(),
        2,
        "Finding 1 must detect 2 ISO_MISSING across intervening u1.upf: {v_f1:#?}"
    );

    // `[m1(fallback), u1(loads m2)]`: m2 inherits m1 nominal_conditions/domains & strips fallback.
    let c2_pm = dir.0.join("c2_pm.cpf");
    std::fs::write(
        dir.0.join("c1_fb.cpf"),
        "set_cpf_version 2.0\ncreate_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9\ncreate_ground_nets -nets {VSS}\ncreate_nominal_condition -name NC_ON -voltage 0.9 -state on\ncreate_nominal_condition -name NC_OFF -voltage 0.0 -state off\ncreate_power_domain -name PD_TOP -default\ncreate_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}\nupdate_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\nupdate_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS\nend_design\n",
    )
    .expect("write c1_fb");
    std::fs::write(
        &c2_pm,
        "set_cpf_version 2.0\ncreate_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -default\ncreate_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_OFF}\nend_design\n",
    )
    .expect("write c2_pm");
    std::fs::write(
        dir.0.join("u1_c2.upf"),
        format!(
            "upf_version 2.1\nload_upf {} -format cpf\n",
            c2_pm.display()
        ),
    )
    .expect("write u1_c2");
    let v_m1_u1 = run_loaded_in_dir(
        &dir.0,
        "m1_u1",
        &[("c1_fb.cpf", None, true), ("u1_c2.upf", Some("/"), false)],
        LpStage::Rtl,
    );
    assert!(!v_m1_u1.iter().any(|v| v.rule_id == "PST_INCONSISTENT"));
    assert!(v_m1_u1.iter().any(|v| v.rule_id == "ISO_MISSING"));

    // Finding 2: [top.upf, sub1.upf(loads m1_on), sub2.upf(loads m2_off)] -> ISO_MISSING, 0 PST_UNREACHABLE_STATE.
    let m1_on_p = dir.0.join("m1_on.cpf");
    let m2_off_p = dir.0.join("m2_off.cpf");
    std::fs::write(
        &m1_on_p,
        "set_cpf_version 2.0\ncreate_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9\ncreate_ground_nets -nets {VSS}\ncreate_nominal_condition -name NC_ON -voltage 0.9 -state on\ncreate_nominal_condition -name NC_OFF -voltage 0.0 -state off\ncreate_power_domain -name PD_TOP -default\ncreate_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}\nupdate_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\nupdate_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS\ncreate_power_mode -name PM_ON -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON} -default\nend_design\n",
    )
    .expect("write m1_on");
    std::fs::write(
        &m2_off_p,
        "set_cpf_version 2.0\ncreate_power_mode -name PM_OFF -domain_conditions {/PD_TOP@NC_ON /PD_WRAP@NC_OFF}\nend_design\n",
    )
    .expect("write m2_off");
    std::fs::write(
        dir.0.join("sub1.upf"),
        format!(
            "upf_version 2.1\nload_upf {} -format cpf -scope /\n",
            m1_on_p.display()
        ),
    )
    .expect("write sub1");
    std::fs::write(
        dir.0.join("sub2.upf"),
        format!(
            "upf_version 2.1\nload_upf {} -format cpf -scope /\n",
            m2_off_p.display()
        ),
    )
    .expect("write sub2");
    let v_f2 = run_loaded_in_dir(
        &dir.0,
        "f2",
        &[
            ("u1.upf", None, false),
            ("sub1.upf", Some("/"), false),
            ("sub2.upf", Some("/"), false),
        ],
        LpStage::Rtl,
    );
    assert!(
        !v_f2
            .iter()
            .any(|v| v.rule_id == "PST_UNREACHABLE_STATE" || v.rule_id == "PST_INCONSISTENT"),
        "Finding 2 must not drop m1.cpf state on pop: {v_f2:#?}"
    );
    assert!(v_f2.iter().any(|v| v.rule_id == "ISO_MISSING"));

    // `nc7` & `nc11` RTL+CPF: scoped NC_ON=off doesn't clobber root; two-scope PM_ILL preserves PST_INCONSISTENT.
    let (_, v_nc7) = run_seq_files(
        "r21_nc7_rtl",
        &[
            ("nc7_1.cpf", None, true, "set_cpf_version 2.0\ncreate_power_nets -nets {VDD_AON VDD_WRAP VDD_SUB} -voltage 0.9\ncreate_ground_nets -nets {VSS}\ncreate_nominal_condition -name NC_ON -voltage 0.9 -state on\ncreate_power_domain -name PD_TOP -default\nupdate_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\ncreate_power_mode -name PM_TOP1 -domain_conditions {PD_TOP@NC_ON} -condition {pwr_en} -default\nend_design\n"),
            ("nc7_2.cpf", Some("u_wrap/u_sub"), true, "set_cpf_version 2.0\ncreate_nominal_condition -name NC_ON -voltage 0.0 -state off\ncreate_power_domain -name PD_SUB -instances {.} -shutoff_condition {!wrap_bad_ctrl}\nupdate_power_domain -name PD_SUB -primary_power_net /VDD_SUB -primary_ground_net /VSS\ncreate_power_mode -name PM_SUB -domain_conditions {PD_SUB@NC_ON} -condition {!wrap_bad_ctrl} -default\nend_design\n"),
            ("nc7_3.cpf", Some("/"), true, "set_cpf_version 2.0\ncreate_power_domain -name PD_WRAP -instances {u_wrap} -exclude_instances {u_wrap/u_sub} -shutoff_condition {!pwr_en}\nupdate_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS\ncreate_power_mode -name PM_TOP2 -domain_conditions {PD_WRAP@NC_ON} -condition {pwr_en} -default\nend_design\n"),
        ],
        LpStage::Rtl,
    );
    assert!(!v_nc7.iter().any(|v| v.rule_id == "PST_INCONSISTENT"));
    assert!(v_nc7.iter().any(|v| v.rule_id == "ISO_MISSING"));

    let (_, v_nc11) = run_seq_files(
        "r21_nc11_rtl",
        &[
            ("nc11_a.cpf", Some("u_wrap"), true, "set_cpf_version 2.0\ncreate_power_nets -nets {VDD_W} -voltage 0.9\ncreate_ground_nets -nets {VSS}\ncreate_nominal_condition -name NC_ON -voltage 0.9 -state on\ncreate_power_domain -name PD_W -instances {.} -shutoff_condition {!bad_shutoff}\nupdate_power_domain -name PD_W -primary_power_net VDD_W -primary_ground_net VSS\ncreate_power_mode -name PM_W -domain_conditions {PD_W@NC_ON} -condition {bad_shutoff} -default\ncreate_power_mode -name PM_ILL -domain_conditions {PD_W@NC_ON} -condition {bad_shutoff} -illegal\nend_design\n"),
            ("nc11_b.cpf", Some("u_wrap/u_sub"), true, "set_cpf_version 2.0\ncreate_power_nets -nets {VDD_S} -voltage 0.9\ncreate_ground_nets -nets {VSS}\ncreate_nominal_condition -name NC_ON -voltage 0.9 -state on\ncreate_power_domain -name PD_S -instances {.} -shutoff_condition {!wrap_bad_ctrl}\nupdate_power_domain -name PD_S -primary_power_net VDD_S -primary_ground_net VSS\ncreate_power_mode -name PM_S -domain_conditions {PD_S@NC_ON} -condition {wrap_bad_ctrl} -default\ncreate_power_mode -name PM_ILL -domain_conditions {PD_S@NC_ON} -condition {!wrap_bad_ctrl} -illegal\nend_design\n"),
        ],
        LpStage::Rtl,
    );
    assert!(v_nc11.iter().any(|v| v.rule_id == "PST_INCONSISTENT"));
}

#[test]
fn r22_adv_fb2_sequential_cpf_upf_add_power_state_rtl() {
    let c1 = "set_cpf_version 2.0\ncreate_power_nets -nets {VDD_AON VDD_WRAP} -voltage 0.9\ncreate_ground_nets -nets {VSS}\ncreate_power_domain -name PD_TOP -default\ncreate_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}\nupdate_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS\nupdate_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS\nend_design\n";
    let u_upd = "upf_version 2.1\nadd_power_state SS_PD_WRAP -state {ON -update -logic_expr {pwr_en && other_aon}}\n";
    let (_, v_upd) = run_seq_files(
        "r22_adv_fb2_upd",
        &[
            ("c1.cpf", None, true, c1),
            ("u_upd.upf", Some("/"), false, u_upd),
        ],
        LpStage::Rtl,
    );
    assert_eq!(
        v_upd.iter().filter(|v| v.rule_id == "ISO_MISSING").count(),
        2,
        "{v_upd:#?}"
    );
    assert_eq!(
        v_upd
            .iter()
            .filter(|v| v.rule_id == "RET_NO_ELEMENTS")
            .count(),
        1,
        "{v_upd:#?}"
    );

    for (tag, tgt) in [
        ("pd", "PD_WRAP"),
        ("prim", "PD_WRAP.primary"),
        ("abs", "/SS_PD_WRAP"),
    ] {
        let u_add = format!("upf_version 2.1\nadd_power_state {tgt} -state {{RUN -supply_expr {{power == {{FULL_ON 0.9}} && ground == {{FULL_ON 0.0}}}} -logic_expr {{1'b1}} -simstate NORMAL}}\n");
        let (_, v_add) = run_seq_files(
            tag,
            &[
                ("c1.cpf", None, true, c1),
                ("u_add.upf", Some("/"), false, &u_add),
            ],
            LpStage::Rtl,
        );
        assert!(v_add.is_empty(), "expected [] for {tgt}, got {v_add:#?}");
    }

    let u_ill =
        "upf_version 2.1\nadd_power_state SS_PD_WRAP -state {BAD -logic_expr {1'b0} -illegal}\n";
    let (_, v_ill) = run_seq_files(
        "r22_adv_fb2_ill",
        &[
            ("c1.cpf", None, true, c1),
            ("u_ill.upf", Some("/"), false, u_ill),
        ],
        LpStage::Upf,
    );
    assert!(
        v_ill.is_empty(),
        "expected [] for -illegal only, got {v_ill:#?}"
    );
}
