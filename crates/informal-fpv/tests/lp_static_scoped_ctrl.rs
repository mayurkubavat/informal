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
    let p = std::env::temp_dir().join(format!("informal_lp_r12_{}_{n}_{tag}", std::process::id()));
    std::fs::create_dir_all(&p).expect("mkdir");
    TempDir(p)
}

fn run_rtl_upf(tag: &str, sv: &str, upf: &str, stage: LpStage) -> Vec<LpViolation> {
    let dir = new_temp(tag);
    let sv_path = dir.0.join("design.sv");
    let upf_path = dir.0.join("intent.upf");
    std::fs::write(&sv_path, sv).expect("write sv");
    std::fs::write(&upf_path, upf).expect("write upf");

    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    s.design.files.push(sv_path);
    s.elaborate(Some("top".to_string())).expect("elaborate");
    load_upf(&mut s, &upf_path, None, None, false).expect("load_upf");
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
fn r12_bug1a_add_power_state_dotdot_logic_expr_in_set_scope() {
    let sv = r#"
module sub_blk(input logic clk, input logic d, output logic q);
  wire pwr_en = 1'b0;
  always_ff @(posedge clk) q <= d ^ pwr_en;
endmodule
module top(input logic clk, input logic pwr_en, input logic d, output logic q);
  sub_blk u_sub(.clk(clk), .d(d), .q(q));
endmodule
"#;
    // 1. `!(../pwr_en)` in `u_sub` collides with root `{pwr_en}` -> `PST_INCONSISTENT`.
    let upf_inconsistent = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_SW
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {pwr_en} -simstate NORMAL}
set_scope u_sub
add_power_state /SS_SW -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {!(../pwr_en)} -simstate NORMAL}
set_scope /
"#;
    let v_upf = run_rtl_upf("b1a_pst", sv, upf_inconsistent, LpStage::Upf);
    assert!(
        v_upf.iter().any(|v| v.rule_id == "PST_INCONSISTENT"),
        "expected PST_INCONSISTENT for !(..pwr_en) vs /pwr_en: {v_upf:#?}"
    );

    // 2. `../pwr_en` for ON and `!(../pwr_en)` for OFF in `u_sub` aligns with root `/pwr_en`
    // (`SS_SW` cannot be ON when `SS_AON` is OFF, and vice versa) -> NO spurious `ISO_MISSING`.
    let upf_aligned = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_SW
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {/pwr_en} -simstate NORMAL}
set_scope u_sub
create_power_domain PD_SUB -elements {.} -supply {primary /SS_SW}
add_power_state /SS_SW -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -logic_expr {../pwr_en}    -simstate NORMAL}
add_power_state /SS_SW -state {OFF -supply_expr {power == OFF           && ground == {FULL_ON 0.0}} -logic_expr {!(../pwr_en)} -simstate CORRUPT}
set_retention r_sub -domain PD_SUB -retention_supply_set /SS_AON -save_signal {/clk high}
set_scope /
"#;
    let v_rtl = run_rtl_upf("b1a_no_iso", sv, upf_aligned, LpStage::Rtl);
    assert!(
        !v_rtl.iter().any(|v| v.rule_id == "ISO_MISSING"),
        "aligned ../pwr_en in u_sub must not emit spurious ISO_MISSING: {v_rtl:#?}"
    );
}

#[test]
fn r12_bug1b_and_1c_negated_psw_ctrl_and_cross_scope_iso_ret_controls() {
    let sv = r#"
module sw_blk(input logic clk, input logic d, output logic s);
  always_ff @(posedge clk) s <= d;
endmodule
module sub_blk(input logic clk, input logic d, output logic q, output logic y);
  wire sig = 1'b1;
  always_ff @(posedge clk) q <= d;
  assign y = sig;
endmodule
module top(input logic clk, input logic d, output logic q_out, output logic y_sub);
  logic sig;
  sw_blk u_sw(.clk(clk), .d(d), .s(sig));
  sub_blk u_sub(.clk(clk), .d(d), .q(q_out), .y(y_sub));
endmodule
"#;
    let hdr = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_SW1
create_supply_net VDD_SW2
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW1 -function {power VDD_SW1} -function {ground VSS}
create_supply_set SS_SW2 -function {power VDD_SW2} -function {ground VSS}
add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW1 -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_SW2 -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
create_power_domain PD_SW1 -elements {u_sw} -supply {primary SS_SW1}
"#;

    // Bug 1b: `!(/sig)` and `!(../sig)` inside `set_scope u_sub` must resolve to root switchable `sig`
    // and fire `PSW_CTRL_UNPOWERED` (not get shadowed by local AON `u_sub/sig`!).
    for expr in ["!(/sig)", "!(../sig)"] {
        let upf = format!(
            r#"{hdr}
set_scope u_sub
create_power_domain PD_SUB -elements {{.}} -supply {{primary /SS_SW2}}
create_power_switch psw1 -domain PD_SUB -supply_set /SS_AON \
  -input_supply_port {{in /VDD_AON}} -output_supply_port {{out /VDD_SW2}} \
  -control_port {{ctrl {expr}}} -on_state {{ON in ctrl}}
set_retention r_sub -domain PD_SUB -retention_supply_set /SS_AON -save_signal {{/clk high}}
set_scope /
"#
        );
        let viols = run_rtl_upf("b1b_psw", sv, &upf, LpStage::Rtl);
        assert!(
            viols.iter().any(|v| v.rule_id == "PSW_CTRL_UNPOWERED"),
            "expected PSW_CTRL_UNPOWERED for `{expr}`: {viols:#?}"
        );
    }

    // Bug 1c: `set_isolation` / `set_retention` inside `u_sub`, while `set_isolation_control`
    // / `set_retention_control` at `/` passes `{sig}` (which is root switchable `sig`, NOT `u_sub/sig`).
    let upf_1c = format!(
        r#"{hdr}
set_scope u_sub
create_power_domain PD_SUB -elements {{.}} -supply {{primary /SS_SW2}}
set_isolation iso_split -domain PD_SUB -applies_to outputs -clamp_value 0 -isolation_supply_set /SS_AON -location parent
set_retention ret_split -domain PD_SUB -retention_supply_set /SS_AON
set_scope /
set_isolation_control iso_split -domain u_sub/PD_SUB -isolation_signal {{sig}} -isolation_sense high
set_retention_control ret_split -domain u_sub/PD_SUB -save_signal {{sig high}}
"#
    );
    let viols_1c = run_rtl_upf("b1c_cross_scope", sv, &upf_1c, LpStage::Rtl);
    assert!(
        viols_1c.iter().any(|v| v.rule_id == "ISO_CTRL_UNPOWERED"),
        "expected ISO_CTRL_UNPOWERED for cross-scope set_isolation_control: {viols_1c:#?}"
    );
    assert!(
        viols_1c.iter().any(|v| v.rule_id == "RET_CTRL_UNPOWERED"),
        "expected RET_CTRL_UNPOWERED for cross-scope set_retention_control: {viols_1c:#?}"
    );
}

fn run_rtl_cpf(tag: &str, sv: &str, cpf: &str) -> Vec<LpViolation> {
    let dir = new_temp(tag);
    let sv_path = dir.0.join("design.sv");
    let cpf_path = dir.0.join("intent.cpf");
    std::fs::write(&sv_path, sv).expect("write sv");
    std::fs::write(&cpf_path, cpf).expect("write cpf");

    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    s.design.files.push(sv_path);
    s.elaborate(Some("top".to_string())).expect("elaborate");
    load_upf(&mut s, &cpf_path, None, None, true).expect("load_cpf");
    check_lp(
        &mut s,
        &CheckLpOpts {
            stage: LpStage::Rtl,
            ..Default::default()
        },
    )
    .expect("check_lp");
    s.lp.violations
}

#[test]
fn r13_bug1_cpf_enable_condition_1_triggers_psw_ctrl_unpowered_without_shutoff() {
    let sv = r#"
module sub_blk(input logic clk, input logic d, output logic bad_ctrl, output logic q);
  always_ff @(posedge clk) begin
    bad_ctrl <= d;
    q <= ~d;
  end
endmodule
module top(input logic clk, input logic d, input logic pwr_en, output logic q);
  logic bad_ctrl;
  sub_blk u_sub(.clk(clk), .d(d), .bad_ctrl(bad_ctrl), .q(q));
endmodule
"#;
    let cpf_hdr = r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_SW} -voltage 0.9
create_ground_nets -nets {VSS}
create_power_domain -name PD_TOP -default
create_power_domain -name PD_SUB -instances {u_sub}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_SUB -primary_power_net VDD_SW -primary_ground_net VSS
create_nominal_condition -name NC_ON -voltage 0.9 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_mode -name PM_ON  -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -default
create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF}
create_state_retention_rule -name RET_1 -domain PD_SUB -secondary_power_net VDD_AON -save_level {pwr_en}
"#;
    let cpf_bad = format!(
        "{cpf_hdr}\ncreate_power_switch_rule -name PSW_1 -domain PD_SUB -external_power_net VDD_AON -enable_condition_1 {{u_sub/bad_ctrl}}\nend_design\n"
    );
    let v_bad = run_rtl_cpf("r13_b1_bad", sv, &cpf_bad);
    assert!(
        v_bad.iter().any(|v| v.rule_id == "PSW_CTRL_UNPOWERED"),
        "expected PSW_CTRL_UNPOWERED for -enable_condition_1 {{u_sub/bad_ctrl}}: {v_bad:#?}"
    );
    assert!(
        !v_bad.iter().any(|v| v.rule_id == "UPF_D13_NOTE"),
        "unexpected UPF_D13_NOTE: {v_bad:#?}"
    );

    let cpf_good = format!(
        "{cpf_hdr}\ncreate_power_switch_rule -name PSW_1 -domain PD_SUB -external_power_net VDD_AON -enable_condition_1 {{pwr_en}}\nend_design\n"
    );
    let v_good = run_rtl_cpf("r13_b1_good", sv, &cpf_good);
    assert!(
        !v_good.iter().any(|v| v.rule_id == "PSW_CTRL_UNPOWERED"),
        "expected 0 PSW_CTRL_UNPOWERED for AON -enable_condition_1 {{pwr_en}}: {v_good:#?}"
    );
}

#[test]
fn r13_bug2_compound_isolation_condition_triggers_iso_ctrl_unpowered() {
    let sv = r#"
module sub_blk(input logic clk, input logic d, output logic bad_ctrl, output logic q);
  always_ff @(posedge clk) begin
    bad_ctrl <= d;
    q <= ~d;
  end
endmodule
module top(input logic clk, input logic d, input logic pwr_en, input logic iso_en, output logic q_out);
  logic bad_ctrl, q_raw;
  sub_blk u_sub(.clk(clk), .d(d), .bad_ctrl(bad_ctrl), .q(q_raw));
  assign q_out = q_raw;
endmodule
"#;
    let cpf_hdr = r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_SW} -voltage 0.9
create_ground_nets -nets {VSS}
create_power_domain -name PD_TOP -default
create_power_domain -name PD_SUB -instances {u_sub} -shutoff_condition {!pwr_en}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_SUB -primary_power_net VDD_SW -primary_ground_net VSS
create_nominal_condition -name NC_ON -voltage 0.9 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_mode -name PM_ON  -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_ON} -default
create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_SUB@NC_OFF}
create_state_retention_rule -name RET_1 -domain PD_SUB -secondary_power_net VDD_AON -save_level {pwr_en}
"#;
    let cpf_bad = format!(
        "{cpf_hdr}\ncreate_isolation_rule -name ISO_1 -from PD_SUB -to PD_TOP -isolation_condition {{u_sub/bad_ctrl & pwr_en}} -isolation_output low -isolation_target to\nend_design\n"
    );
    let v_bad = run_rtl_cpf("r13_b2_cpf_bad", sv, &cpf_bad);
    assert!(
        v_bad.iter().any(|v| v.rule_id == "ISO_CTRL_UNPOWERED"),
        "expected ISO_CTRL_UNPOWERED for compound -isolation_condition: {v_bad:#?}"
    );
    assert!(
        !v_bad
            .iter()
            .any(|v| v.rule_id == "ISO_CTRL_POLARITY_MISMATCH" || v.rule_id == "ISO_CLAMP_MISMATCH"),
        "unexpected clamp/polarity error on compound condition: {v_bad:#?}"
    );

    let cpf_good = format!(
        "{cpf_hdr}\ncreate_isolation_rule -name ISO_1 -from PD_SUB -to PD_TOP -isolation_condition {{iso_en & pwr_en}} -isolation_output low -isolation_target to\nend_design\n"
    );
    let v_good = run_rtl_cpf("r13_b2_cpf_good", sv, &cpf_good);
    assert!(
        !v_good.iter().any(|v| v.rule_id == "ISO_CTRL_UNPOWERED"),
        "expected 0 ISO_CTRL_UNPOWERED for AON compound condition: {v_good:#?}"
    );

    // UPF compound `-isolation_signal` inside `set_scope u_sub` using `!(bad_ctrl) & ../pwr_en`.
    let upf_bad = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_SW
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW  -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
set_scope u_sub
create_power_domain PD_SUB -elements {.} -supply {primary /SS_SW}
set_isolation iso_cmp -domain PD_SUB -applies_to outputs -clamp_value 0 -isolation_supply_set /SS_AON \
  -isolation_signal {!(bad_ctrl) & ../pwr_en} -isolation_sense high -location parent
set_retention r_sub -domain PD_SUB -retention_supply_set /SS_AON -save_signal {/pwr_en high}
set_scope /
"#;
    let v_upf = run_rtl_upf("r13_b2_upf_bad", sv, upf_bad, LpStage::Rtl);
    assert!(
        v_upf.iter().any(|v| v.rule_id == "ISO_CTRL_UNPOWERED"),
        "expected ISO_CTRL_UNPOWERED for UPF compound -isolation_signal: {v_upf:#?}"
    );
}

#[test]
fn r14_bug_a_nested_scope_compound_psw_enable_and_retention_ctrl() {
    let sv = r#"
module sub_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module wrap_blk(input logic clk, input logic d, input logic other_aon, output logic wrap_bad_ctrl, output logic q);
  always_ff @(posedge clk) wrap_bad_ctrl <= d;
  sub_blk u_sub(.clk(clk), .d(d ^ other_aon), .q(q));
endmodule
module dst_blk(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic d, input logic other_aon, input logic pwr_en, output logic y);
  logic wrap_bad_ctrl, q;
  wrap_blk u_wrap(.clk(clk), .d(d), .other_aon(other_aon), .wrap_bad_ctrl(wrap_bad_ctrl), .q(q));
  dst_blk u_dst(.clk(clk), .a(q), .y(y));
endmodule
"#;
    let top_cpf = r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP VDD_SUB} -voltage 0.9
create_ground_nets -nets {VSS}
create_power_domain -name PD_TOP  -default
create_power_domain -name PD_WRAP -instances {u_wrap} -shutoff_condition {!pwr_en}
create_power_domain -name PD_DST  -instances {u_dst}
update_power_domain -name PD_TOP  -primary_power_net VDD_AON  -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
update_power_domain -name PD_DST  -primary_power_net VDD_AON  -primary_ground_net VSS
create_nominal_condition -name NC_ON  -voltage 0.9 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_mode -name PM_ON  -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON  PD_DST@NC_ON} -default
create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_OFF PD_DST@NC_ON}
end_design
"#;
    let make_sub_cpf = |en_expr: &str| {
        format!(
            r#"
set_cpf_version 2.0
create_power_domain -name PD_SUB -default
update_power_domain -name PD_SUB -primary_power_net /VDD_SUB -primary_ground_net /VSS
create_power_switch_rule -name PSW_SUB -domain PD_SUB -external_power_net /VDD_AON -enable_condition_1 {{{en_expr}}}
create_state_retention_rule -name RET_SUB -domain PD_SUB -secondary_power_net /VDD_AON -save_level {{{en_expr}}}
end_design
"#
        )
    };
    let run_nested = |tag: &str, sub_cpf: &str| -> Vec<LpViolation> {
        let dir = new_temp(tag);
        let sv_p = dir.0.join("design.sv");
        let top_p = dir.0.join("top.cpf");
        let sub_p = dir.0.join("sub.cpf");
        std::fs::write(&sv_p, sv).expect("write sv");
        std::fs::write(&top_p, top_cpf).expect("write top cpf");
        std::fs::write(&sub_p, sub_cpf).expect("write sub cpf");
        let mut s = Session::new(Vec::new(), dir.0.join("work"));
        s.design.files.push(sv_p);
        s.elaborate(Some("top".to_string())).expect("elaborate");
        load_upf(&mut s, &top_p, None, None, true).expect("load top cpf");
        load_upf(&mut s, &sub_p, Some("u_wrap/u_sub"), None, true).expect("load sub cpf");
        check_lp(
            &mut s,
            &CheckLpOpts {
                stage: LpStage::Rtl,
                ..Default::default()
            },
        )
        .expect("check_lp");
        s.lp.violations
    };

    let v_bad = run_nested("r14_b_a_bad", &make_sub_cpf("wrap_bad_ctrl & /other_aon"));
    assert!(
        v_bad.iter().any(|v| v.rule_id == "PSW_CTRL_UNPOWERED"),
        "expected PSW_CTRL_UNPOWERED on nested-scope compound -enable_condition_1: {v_bad:#?}"
    );
    assert!(
        v_bad.iter().any(|v| v.rule_id == "RET_CTRL_UNPOWERED"),
        "expected RET_CTRL_UNPOWERED on nested-scope compound -save_level: {v_bad:#?}"
    );

    let v_good = run_nested("r14_b_a_good", &make_sub_cpf("/pwr_en & /other_aon"));
    assert!(
        !v_good.iter().any(|v| matches!(
            v.rule_id.as_str(),
            "PSW_CTRL_UNPOWERED" | "RET_CTRL_UNPOWERED"
        )),
        "expected 0 PSW/RET_CTRL_UNPOWERED on AON nested-scope compound expr: {v_good:#?}"
    );

    // UPF compound `-save_condition` / `-restore_condition` inside nested `set_scope u_wrap/u_sub`.
    let upf_ret_cond = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_WRAP
create_supply_net VDD_SUB
create_supply_net VSS
create_supply_set SS_AON  -function {power VDD_AON}  -function {ground VSS}
create_supply_set SS_WRAP -function {power VDD_WRAP} -function {ground VSS}
create_supply_set SS_SUB  -function {power VDD_SUB}  -function {ground VSS}
add_power_state SS_AON  -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_WRAP -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                        -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_SUB  -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                        -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
create_power_domain PD_TOP  -include_scope -supply {primary SS_AON}
create_power_domain PD_WRAP -elements {u_wrap} -exclude_elements {u_wrap/u_sub} -supply {primary SS_WRAP}
set_scope u_wrap/u_sub
create_power_domain PD_SUB -elements {.} -supply {primary /SS_SUB}
set_retention ret_cond -domain PD_SUB -retention_supply_set /SS_AON \
  -save_signal {/pwr_en high} -restore_signal {/pwr_en low} \
  -save_condition {wrap_bad_ctrl & /other_aon} -restore_condition {wrap_bad_ctrl | /other_aon}
set_scope /
"#;
    let v_cond = run_rtl_upf("r14_ret_cond", sv, upf_ret_cond, LpStage::Rtl);
    let ret_ctrl_sigs: Vec<&str> = v_cond
        .iter()
        .filter(|v| v.rule_id == "RET_CTRL_UNPOWERED")
        .map(|v| v.signal.as_str())
        .collect();
    assert_eq!(
        ret_ctrl_sigs.len(),
        2,
        "expected both save/restore condition hits: {v_cond:#?}"
    );
}

#[test]
fn r15_cross_scope_retention_conditions_and_cpf_update_power_switch_rule() {
    let sv = r#"
module sw_blk(input logic clk, input logic d, output logic shadow_ctrl);
  always_ff @(posedge clk) shadow_ctrl <= d;
endmodule
module sub_blk(input logic clk, input logic d, output logic q);
  wire shadow_ctrl = 1'b1;
  always_ff @(posedge clk) q <= d ^ shadow_ctrl;
endmodule
module wrap_blk(input logic clk, input logic d, input logic other_aon, output logic wrap_bad_ctrl, output logic q);
  always_ff @(posedge clk) wrap_bad_ctrl <= d;
  sub_blk u_sub(.clk(clk), .d(d ^ other_aon), .q(q));
endmodule
module top(input logic clk, input logic d, input logic other_aon, input logic pwr_en, output logic q);
  logic wrap_bad_ctrl, shadow_ctrl;
  sw_blk u_sw(.clk(clk), .d(d), .shadow_ctrl(shadow_ctrl));
  wrap_blk u_wrap(.clk(clk), .d(d), .other_aon(other_aon), .wrap_bad_ctrl(wrap_bad_ctrl), .q(q));
endmodule
"#;
    let hdr = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_WRAP
create_supply_net VDD_SUB
create_supply_net VDD_SW1
create_supply_net VSS
create_supply_set SS_AON  -function {power VDD_AON}  -function {ground VSS}
create_supply_set SS_WRAP -function {power VDD_WRAP} -function {ground VSS}
create_supply_set SS_SUB  -function {power VDD_SUB}  -function {ground VSS}
create_supply_set SS_SW1  -function {power VDD_SW1}  -function {ground VSS}
add_power_state SS_AON  -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_WRAP -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                        -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_SUB  -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                        -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_SW1  -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                        -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
create_power_domain PD_TOP  -include_scope -supply {primary SS_AON}
create_power_domain PD_SW1  -elements {u_sw} -supply {primary SS_SW1}
create_power_domain PD_WRAP -elements {u_wrap} -exclude_elements {u_wrap/u_sub} -supply {primary SS_WRAP}
create_power_domain PD_SUB  -elements {u_wrap/u_sub} -supply {primary SS_SUB}
"#;

    // R1: `set_retention` at `/`, `set_retention_control` in `u_wrap/u_sub` with `-save_condition` only.
    let u_r1 = format!(
        r#"{hdr}
set_retention ret_wrap -domain /PD_WRAP -retention_supply_set SS_AON -save_signal {{pwr_en high}}
set_scope u_wrap/u_sub
set_retention_control ret_wrap -domain /PD_WRAP -save_condition {{wrap_bad_ctrl & /other_aon}}
set_scope /
"#
    );
    let v_r1 = run_rtl_upf("r15_r1", sv, &u_r1, LpStage::Rtl);
    assert!(
        v_r1.iter().any(
            |v| v.rule_id == "RET_CTRL_UNPOWERED" && v.domain_src.as_deref() == Some("PD_WRAP")
        ),
        "R1 expected RET_CTRL_UNPOWERED from PD_WRAP: {v_r1:#?}"
    );

    // R2: `set_retention` at `/`, `set_retention_control` in `u_wrap/u_sub` with `-restore_condition` only.
    let u_r2 = format!(
        r#"{hdr}
set_retention ret_wrap -domain /PD_WRAP -retention_supply_set SS_AON -restore_signal {{pwr_en low}}
set_scope u_wrap/u_sub
set_retention_control ret_wrap -domain /PD_WRAP -restore_condition {{wrap_bad_ctrl & /other_aon}}
set_scope /
"#
    );
    let v_r2 = run_rtl_upf("r15_r2", sv, &u_r2, LpStage::Rtl);
    assert!(
        v_r2.iter().any(
            |v| v.rule_id == "RET_CTRL_UNPOWERED" && v.domain_src.as_deref() == Some("PD_WRAP")
        ),
        "R2 expected RET_CTRL_UNPOWERED from PD_WRAP: {v_r2:#?}"
    );

    // R3_FN: `set_retention` in `u_wrap/u_sub`, `set_retention_control` at `/` with shadowed `{shadow_ctrl & pwr_en}` -> PD_SW1.
    let u_r3 = format!(
        r#"{hdr}
set_scope u_wrap/u_sub
set_retention ret_sub -domain /PD_SUB -retention_supply_set /SS_AON
set_scope /
set_retention_control ret_sub -domain /PD_SUB -save_condition {{shadow_ctrl & pwr_en}}
"#
    );
    let v_r3 = run_rtl_upf("r15_r3", sv, &u_r3, LpStage::Rtl);
    assert!(
        v_r3.iter()
            .any(|v| v.rule_id == "RET_CTRL_UNPOWERED"
                && v.domain_src.as_deref() == Some("PD_SW1")),
        "R3_FN expected RET_CTRL_UNPOWERED on root switchable shadow_ctrl (PD_SW1): {v_r3:#?}"
    );

    // R4: `set_retention -save_condition {wrap_bad_ctrl & /other_aon}` in `u_wrap/u_sub`,
    // then `set_retention_control -save_signal {/clk high}` at `/` must NOT clobber `save_condition` scope.
    let u_r4 = format!(
        r#"{hdr}
set_scope u_wrap/u_sub
set_retention ret_sub -domain /PD_SUB -retention_supply_set /SS_AON -save_condition {{wrap_bad_ctrl & /other_aon}}
set_scope /
set_retention_control ret_sub -domain /PD_SUB -save_signal {{/clk high}}
"#
    );
    let v_r4 = run_rtl_upf("r15_r4", sv, &u_r4, LpStage::Rtl);
    assert!(
        v_r4.iter()
            .any(|v| v.rule_id == "RET_CTRL_UNPOWERED" && v.domain_src.as_deref() == Some("PD_WRAP")),
        "R4 expected RET_CTRL_UNPOWERED even after root set_retention_control -save_signal: {v_r4:#?}"
    );

    // R5: `set_retention -update` in `u_wrap/u_sub` with `-save_condition` only.
    let u_r5 = format!(
        r#"{hdr}
set_retention ret_wrap -domain /PD_WRAP -retention_supply_set SS_AON -save_signal {{pwr_en high}}
set_scope u_wrap/u_sub
set_retention ret_wrap -domain /PD_WRAP -save_condition {{wrap_bad_ctrl & /other_aon}} -update
set_scope /
"#
    );
    let v_r5 = run_rtl_upf("r15_r5", sv, &u_r5, LpStage::Rtl);
    assert!(
        v_r5.iter().any(
            |v| v.rule_id == "RET_CTRL_UNPOWERED" && v.domain_src.as_deref() == Some("PD_WRAP")
        ),
        "R5 expected RET_CTRL_UNPOWERED for set_retention -update: {v_r5:#?}"
    );

    // `retention_condition` in nested scope + cross-scope `-update` triggers `RET_CTRL_UNPOWERED` (plus negative control).
    let u_ret_cond = format!(
        r#"{hdr}
set_retention ret_sub -domain /PD_SUB -retention_supply_set /SS_AON -save_signal {{/clk high}} -retention_condition {{shadow_ctrl & pwr_en}}
"#
    );
    let v_rc = run_rtl_upf("r15_ret_cond", sv, &u_ret_cond, LpStage::Rtl);
    assert!(
        v_rc.iter()
            .any(|v| v.rule_id == "RET_CTRL_UNPOWERED"
                && v.domain_src.as_deref() == Some("PD_SW1")),
        "expected RET_CTRL_UNPOWERED for unpowered -retention_condition: {v_rc:#?}"
    );
    let u_ret_cond_ok = format!(
        r#"{hdr}
set_retention ret_sub -domain /PD_SUB -retention_supply_set /SS_AON -save_signal {{/clk high}} -retention_condition {{pwr_en & other_aon}}
"#
    );
    let v_rc_ok = run_rtl_upf("r15_ret_cond_ok", sv, &u_ret_cond_ok, LpStage::Rtl);
    assert!(
        !v_rc_ok.iter().any(|v| v.rule_id == "RET_CTRL_UNPOWERED"),
        "expected 0 RET_CTRL_UNPOWERED for AON -retention_condition: {v_rc_ok:#?}"
    );

    // A5a: Cross-scope CPF `update_power_switch_rule -name /PSW_WRAP` from `u_wrap/u_sub` fires `PSW_CTRL_UNPOWERED`.
    let dir = new_temp("r15_a5a");
    let sv_p = dir.0.join("design.sv");
    let top_p = dir.0.join("top.cpf");
    let sub_p = dir.0.join("sub.cpf");
    std::fs::write(&sv_p, sv).unwrap();
    std::fs::write(
        &top_p,
        r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_WRAP VDD_SUB} -voltage 0.9
create_ground_nets -nets {VSS}
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {u_wrap} -exclude_instances {u_wrap/u_sub} -shutoff_condition {!pwr_en}
create_power_domain -name PD_SUB -instances {u_wrap/u_sub}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
update_power_domain -name PD_SUB -primary_power_net VDD_SUB -primary_ground_net VSS
create_nominal_condition -name NC_ON -voltage 0.9 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_mode -name PM_ON  -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_ON  PD_SUB@NC_ON} -default
create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_WRAP@NC_OFF PD_SUB@NC_ON}
create_power_switch_rule -name PSW_WRAP -domain PD_WRAP -external_power_net VDD_AON -enable_condition_1 {pwr_en}
end_design
"#,
    )
    .unwrap();
    std::fs::write(
        &sub_p,
        r#"
set_cpf_version 2.0
update_power_switch_rule -name /PSW_WRAP -enable_condition_1 {wrap_bad_ctrl & /other_aon}
end_design
"#,
    )
    .unwrap();
    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    s.design.files.push(sv_p);
    s.elaborate(Some("top".to_string())).unwrap();
    load_upf(&mut s, &top_p, None, None, true).unwrap();
    load_upf(&mut s, &sub_p, Some("u_wrap/u_sub"), None, true).unwrap();
    check_lp(
        &mut s,
        &CheckLpOpts {
            stage: LpStage::Rtl,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        s.lp.violations.iter().any(
            |v| v.rule_id == "PSW_CTRL_UNPOWERED" && v.domain_src.as_deref() == Some("PD_WRAP")
        ),
        "A5a expected PSW_CTRL_UNPOWERED on cross-scope update_power_switch_rule: {:#?}",
        s.lp.violations
    );
}

#[test]
fn r16_cpf_h1_cross_scope_shutoff_and_h2_cross_file_update_power_switch_rule() {
    let sv = r#"
module sw_blk(input logic clk, input logic d, output logic bad_shutoff);
  always_ff @(posedge clk) bad_shutoff <= d;
endmodule
module sub_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module wrap_blk(input logic clk, input logic d, input logic other_aon, output logic wrap_bad_ctrl, output logic q);
  always_ff @(posedge clk) wrap_bad_ctrl <= d;
  sub_blk u_sub(.clk(clk), .d(d ^ other_aon), .q(q));
endmodule
module top(input logic clk, input logic d, input logic other_aon, input logic pwr_en, output logic q);
  logic bad_shutoff, wrap_bad_ctrl;
  sw_blk u_sw(.clk(clk), .d(d), .bad_shutoff(bad_shutoff));
  wrap_blk u_wrap(.clk(clk), .d(d), .other_aon(other_aon), .wrap_bad_ctrl(wrap_bad_ctrl), .q(q));
endmodule
"#;

    // H2: Cross-file `update_power_switch_rule -name /PSW_WRAP -enable_condition_1 {/other_aon}`
    // must remove stale `"ctrl"` synthesized from `PD_WRAP -shutoff_condition {!bad_shutoff}`.
    let dir_h2 = new_temp("r16_h2");
    let sv_h2 = dir_h2.0.join("design.sv");
    let top_h2 = dir_h2.0.join("top.cpf");
    let sub_h2 = dir_h2.0.join("sub.cpf");
    std::fs::write(&sv_h2, sv).unwrap();
    std::fs::write(
        &top_h2,
        r#"
set_cpf_version 2.0
create_power_nets -nets {VDD_AON VDD_SW1 VDD_WRAP VDD_SUB} -voltage 0.9
create_ground_nets -nets {VSS}
create_power_domain -name PD_TOP -default
create_power_domain -name PD_SW1 -instances {u_sw} -shutoff_condition {!pwr_en}
create_power_domain -name PD_WRAP -instances {u_wrap} -exclude_instances {u_wrap/u_sub} -shutoff_condition {!bad_shutoff}
create_power_domain -name PD_SUB -instances {u_wrap/u_sub}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_SW1 -primary_power_net VDD_SW1 -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
update_power_domain -name PD_SUB -primary_power_net VDD_AON -primary_ground_net VSS
create_nominal_condition -name NC_ON -voltage 0.9 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_mode -name PM_ON  -domain_conditions {PD_TOP@NC_ON PD_SW1@NC_ON PD_WRAP@NC_ON} -default
create_power_mode -name PM_OFF -domain_conditions {PD_TOP@NC_ON PD_SW1@NC_OFF PD_WRAP@NC_OFF}
create_power_switch_rule -name PSW_WRAP -domain PD_WRAP -external_power_net VDD_AON
end_design
"#,
    )
    .unwrap();
    std::fs::write(
        &sub_h2,
        r#"
set_cpf_version 2.0
update_power_switch_rule -name /PSW_WRAP -enable_condition_1 {/other_aon}
end_design
"#,
    )
    .unwrap();
    let mut s_h2 = Session::new(Vec::new(), dir_h2.0.join("work"));
    s_h2.design.files.push(sv_h2);
    s_h2.elaborate(Some("top".to_string())).unwrap();
    load_upf(&mut s_h2, &top_h2, None, None, true).unwrap();
    load_upf(&mut s_h2, &sub_h2, Some("u_wrap/u_sub"), None, true).unwrap();
    check_lp(
        &mut s_h2,
        &CheckLpOpts {
            stage: LpStage::Rtl,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        !s_h2
            .lp
            .violations
            .iter()
            .any(|v| v.rule_id == "PSW_CTRL_UNPOWERED"),
        "H2: expected 0 PSW_CTRL_UNPOWERED after replacing synthesized ctrl cross-file: {:#?}",
        s_h2.lp.violations
    );

    // H1: Cross-scope & cross-file `create_power_domain -shutoff_condition {!wrap_bad_ctrl}`
    // in `u_wrap/u_sub` inherited by `create_power_switch_rule` at `/` must fire `PSW_CTRL_UNPOWERED`
    // (while AON `-shutoff_condition {!pwr_en}` passes with 0 `PSW_CTRL_UNPOWERED`).
    for (tag, shutoff_cond, expect_err) in [
        ("r16_h1_bad", "!wrap_bad_ctrl", true),
        ("r16_h1_good", "!pwr_en", false),
    ] {
        let dir = new_temp(tag);
        let sv_p = dir.0.join("design.sv");
        let sub_p = dir.0.join("sub.cpf");
        let top_p = dir.0.join("top.cpf");
        std::fs::write(&sv_p, sv).unwrap();
        std::fs::write(
            &sub_p,
            format!(
                r#"
set_cpf_version 2.0
create_power_domain -name PD_SUB -instances {{.}} -shutoff_condition {{{shutoff_cond}}}
update_power_domain -name PD_SUB -primary_power_net /VDD_SUB -primary_ground_net /VSS
end_design
"#
            ),
        )
        .unwrap();
        std::fs::write(
            &top_p,
            format!(
                r#"
set_cpf_version 2.0
create_power_nets -nets {{VDD_AON VDD_WRAP VDD_SUB}} -voltage 0.9
create_ground_nets -nets {{VSS}}
create_power_domain -name PD_TOP -default
create_power_domain -name PD_WRAP -instances {{u_wrap}} -exclude_instances {{u_wrap/u_sub}} -shutoff_condition {{!pwr_en}}
update_power_domain -name PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_WRAP -primary_power_net VDD_WRAP -primary_ground_net VSS
load_upf -format cpf -scope u_wrap/u_sub {}
create_power_switch_rule -name PSW_SUB -domain u_wrap/u_sub/PD_SUB -external_power_net VDD_AON
end_design
"#,
                sub_p.display()
            ),
        )
        .unwrap();
        let mut s = Session::new(Vec::new(), dir.0.join("work"));
        s.design.files.push(sv_p);
        s.elaborate(Some("top".to_string())).unwrap();
        load_upf(&mut s, &top_p, None, None, true).unwrap();
        check_lp(
            &mut s,
            &CheckLpOpts {
                stage: LpStage::Rtl,
                ..Default::default()
            },
        )
        .unwrap();
        let has_err = s.lp.violations.iter().any(|v| {
            v.rule_id == "PSW_CTRL_UNPOWERED" && v.domain_src.as_deref() == Some("PD_WRAP")
        });
        assert_eq!(
            has_err, expect_err,
            "{tag}: unexpected PSW_CTRL_UNPOWERED result: {:#?}",
            s.lp.violations
        );
    }
}
