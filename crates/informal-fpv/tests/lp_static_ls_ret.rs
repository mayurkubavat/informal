use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use informal_fpv::lp::{check_lp, load_upf, CheckLpOpts, LpStage, LpViolation};
use informal_fpv::Session;

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn new_temp(tag: &str) -> TempDir {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "informal_lp_lsret_{}_{n}_{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    TempDir(dir)
}

fn run_rtl_upf(tag: &str, sv: &str, upf: &str, top: &str) -> Vec<LpViolation> {
    let dir = new_temp(tag);
    let sv_path = dir.0.join(format!("{tag}.sv"));
    let upf_path = dir.0.join(format!("{tag}.upf"));
    std::fs::write(&sv_path, sv).unwrap();
    std::fs::write(&upf_path, upf).unwrap();
    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    s.design.files.push(sv_path);
    s.elaborate(Some(top.to_string())).expect("elaborate");
    load_upf(&mut s, &upf_path, None, None, false).expect("load_upf");
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
fn ls_missing_direction_location_unisolated_input_and_redundant() {
    let sv = r#"
module blk_hi(output logic a_miss, output logic b_dir, output logic c_uniso, input logic d_red);
  assign a_miss  = 1'b1;
  assign b_dir   = 1'b0;
  assign c_uniso = 1'b1;
endmodule
module blk_lo(input logic a_miss, input logic b_dir, input logic c_uniso, output logic y);
  assign y = a_miss ^ b_dir ^ c_uniso;
endmodule
module blk_same(output logic d_red);
  assign d_red = 1'b1;
endmodule
module top(output logic y);
  logic a, b, c, d;
  blk_hi u_hi(.a_miss(a), .b_dir(b), .c_uniso(c), .d_red(d));
  blk_lo u_lo(.a_miss(a), .b_dir(b), .c_uniso(c), .y(y));
  blk_same u_same(.d_red(d));
endmodule
"#;
    let upf = r#"
upf_version 2.1
create_supply_net VDD_HI
create_supply_net VDD_LO
create_supply_net VDD_SAME
create_supply_net VSS
create_supply_set SS_HI   -function {power VDD_HI}   -function {ground VSS}
create_supply_set SS_LO   -function {power VDD_LO}   -function {ground VSS}
create_supply_set SS_SAME -function {power VDD_SAME} -function {ground VSS}

add_power_state SS_HI -state {ON -supply_expr {power == {FULL_ON 1.8} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                      -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_LO -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SAME -state {ON -supply_expr {power == {FULL_ON 1.8} && ground == {FULL_ON 0.0}} -simstate NORMAL}

create_power_domain PD_TOP  -include_scope -supply {primary SS_LO}
create_power_domain PD_HI   -elements {u_hi}   -supply {primary SS_HI}
create_power_domain PD_LO   -elements {u_lo}   -supply {primary SS_LO}
create_power_domain PD_SAME -elements {u_same} -supply {primary SS_SAME}

set_level_shifter ls_dir -domain PD_HI -applies_to outputs -elements {u_hi/b_dir} \
  -rule low_to_high -location parent
set_level_shifter ls_loc -domain PD_HI -applies_to outputs -elements {u_hi/c_uniso} \
  -rule high_to_low -input_supply_set SS_LO -location parent
set_level_shifter ls_red -domain PD_SAME -applies_to outputs -elements {u_same/d_red} \
  -location parent
"#;
    let viols = run_rtl_upf("ls_all", sv, upf, "top");
    let ids: Vec<&str> = viols.iter().map(|v| v.rule_id.as_str()).collect();
    assert!(ids.contains(&"LS_MISSING"), "{ids:?}");
    assert!(ids.contains(&"LS_DIR_MISMATCH"), "{ids:?}");
    assert!(ids.contains(&"LS_LOCATION"), "{ids:?}");
    assert!(ids.contains(&"LS_UNISOLATED_INPUT"), "{ids:?}");
    assert!(ids.contains(&"LS_REDUNDANT"), "{ids:?}");
}

#[test]
fn ret_backup_off_ctrl_unpowered_async_rst_psw_ctrl_and_port_attr() {
    let sv = r#"
module sw_ctrl_src(input logic clk, input logic raw, output logic save_sig, output logic rst_sw_n, output logic psw_ctrl);
  assign save_sig = raw;
  assign rst_sw_n = raw;
  assign psw_ctrl = raw;
endmodule
module ret_blk(input logic clk, input logic rst_sw_n, input logic d, output logic q_ret);
  always_ff @(posedge clk or negedge rst_sw_n) begin
    if (!rst_sw_n) q_ret <= 1'b0;
    else q_ret <= d;
  end
endmodule
module top(input logic clk, input logic raw, input logic d, output logic q_ret);
  logic save_sig, rst_sw_n, psw_ctrl;
  sw_ctrl_src u_ctrl(.clk(clk), .raw(raw), .save_sig(save_sig), .rst_sw_n(rst_sw_n), .psw_ctrl(psw_ctrl));
  ret_blk u_ret(.clk(clk), .rst_sw_n(rst_sw_n), .d(d), .q_ret(q_ret));
endmodule
"#;
    let upf = r#"
upf_version 2.1
create_supply_net VDD_TOP
create_supply_net VDD_SW
create_supply_net VDD_BKUP
create_supply_net VSS

create_supply_set SS_TOP  -function {power VDD_TOP}  -function {ground VSS}
create_supply_set SS_SW   -function {power VDD_SW}   -function {ground VSS}
create_supply_set SS_BKUP -function {power VDD_BKUP} -function {ground VSS}

add_power_state SS_TOP  -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                        -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_SW   -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                        -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_BKUP -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                        -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}

create_power_domain PD_TOP  -include_scope -supply {primary SS_TOP}
create_power_domain PD_CTRL -elements {u_ctrl} -supply {primary SS_SW}
create_power_domain PD_RET  -elements {u_ret}  -supply {primary SS_SW}

create_power_switch psw_ctrl_bad -domain PD_CTRL \
  -input_supply_port {in VDD_TOP} -output_supply_port {out VDD_SW} \
  -control_port {sleep u_ctrl/psw_ctrl} \
  -on_state {ON in {sleep}} -off_state {OFF {!sleep}}

set_retention ret_bad -domain PD_RET -retention_supply_set SS_BKUP \
  -save_signal {u_ctrl/save_sig high} -restore_signal {u_ctrl/save_sig low}
"#;
    let viols = run_rtl_upf("ret_psw_all", sv, upf, "top");
    let ids: Vec<&str> = viols.iter().map(|v| v.rule_id.as_str()).collect();
    assert!(ids.contains(&"RET_BACKUP_OFF"), "{ids:?}");
    assert!(ids.contains(&"RET_CTRL_UNPOWERED"), "{ids:?}");
    assert!(ids.contains(&"RET_ASYNC_RST_UNISOLATED"), "{ids:?}");
    assert!(ids.contains(&"PSW_CTRL_UNPOWERED"), "{ids:?}");
    assert!(ids.contains(&"PORT_ATTR_MISSING"), "{ids:?}");
}

#[test]
fn regression_defect_b_ls_precedence_and_3c_ret_async_rst_path_isolation() {
    let sv = r#"
module rst_src(input logic raw_rst_n, output logic rst_sw_n);
  assign rst_sw_n = raw_rst_n;
endmodule
module hi_blk(output logic s_keep, output logic s_skip);
  assign s_keep = 1'b1;
  assign s_skip = 1'b0;
endmodule
module lo_ret(input logic clk, input logic rst_n, input logic s_keep, input logic s_skip, output logic q);
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) q <= 1'b0;
    else q <= s_keep ^ s_skip;
  end
endmodule
module top(input logic clk, input logic raw_rst_n, output logic q);
  logic rst_sw_n, s_keep, s_skip;
  rst_src u_rst(.raw_rst_n(raw_rst_n), .rst_sw_n(rst_sw_n));
  hi_blk  u_hi(.s_keep(s_keep), .s_skip(s_skip));
  lo_ret  u_lo(.clk(clk), .rst_n(rst_sw_n), .s_keep(s_keep), .s_skip(s_skip), .q(q));
endmodule
"#;
    let upf = r#"
upf_version 2.1
create_supply_net VDD_HI
create_supply_net VDD_LO
create_supply_net VDD_BKUP
create_supply_net VSS
create_supply_set SS_HI   -function {power VDD_HI}   -function {ground VSS}
create_supply_set SS_LO   -function {power VDD_LO}   -function {ground VSS}
create_supply_set SS_BKUP -function {power VDD_BKUP} -function {ground VSS}

add_power_state SS_HI   -state {ON -supply_expr {power == {FULL_ON 1.8} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                        -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_LO   -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                        -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_BKUP -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}

create_power_domain PD_TOP -include_scope -supply {primary SS_BKUP}
create_power_domain PD_HI  -elements {u_hi u_rst} -supply {primary SS_HI}
create_power_domain PD_LO  -elements {u_lo}       -supply {primary SS_LO}

# [3C-rst] Async reset isolated at `u_rst/rst_sw_n` (intermediate path net/pin).
set_isolation iso_rst -domain PD_HI -applies_to outputs -elements {u_rst/rst_sw_n} \
  -clamp_value 1 -isolation_supply_set SS_BKUP -isolation_signal raw_rst_n -isolation_sense low -location parent

# Defect B (LS): `-no_shift` default overridden by explicit `-elements {u_hi/s_keep}`,
# while `u_hi/s_skip` remains suppressed by `-no_shift` (`LS_MISSING` must NOT fire for `s_skip`).
set_level_shifter ls_no_def -domain PD_HI -no_shift
set_level_shifter ls_keep   -domain PD_HI -applies_to outputs -elements {u_hi/s_keep} \
  -rule high_to_low -location parent

set_retention ret_lo -domain PD_LO -retention_supply_set SS_BKUP \
  -save_signal {raw_rst_n high} -restore_signal {raw_rst_n low}
"#;
    let viols = run_rtl_upf("ls_ret_3c", sv, upf, "top");
    assert!(
        !viols
            .iter()
            .any(|v| v.rule_id == "RET_ASYNC_RST_UNISOLATED"),
        "{viols:#?}"
    );
    assert!(
        !viols
            .iter()
            .any(|v| v.rule_id == "LS_MISSING" && v.signal.contains("s_skip")),
        "{viols:#?}"
    );
    assert!(
        viols
            .iter()
            .any(|v| v.rule_id == "LS_UNISOLATED_INPUT" && v.signal.contains("s_keep")),
        "{viols:#?}"
    );
}

#[test]
fn regression_3c_rst_comb_sw_and_par_flop_unisolated_branches() {
    // 1. `3C-rst-comb-sw`: AON input `raw_b` inverted inside switchable `u_rst` (`PD_RST`)
    // before feeding retained flop `u_ret` -> must fire `RET_ASYNC_RST_UNISOLATED`, and
    // isolating `u_rst/rst_uniso_n` must clear it (no false positive on upstream `u_rst/raw_b`).
    let sv_comb = r#"
module rst_comb_src(input logic raw_b, output logic rst_uniso_n);
  assign rst_uniso_n = ~raw_b;
endmodule
module ret_blk(input logic clk, input logic rst_n, input logic d, output logic q_ret);
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) q_ret <= 1'b0;
    else q_ret <= d;
  end
endmodule
module top(input logic clk, input logic raw_b, input logic d, output logic q_ret);
  logic rst_uniso_n;
  rst_comb_src u_rst(.raw_b(raw_b), .rst_uniso_n(rst_uniso_n));
  ret_blk      u_ret(.clk(clk), .rst_n(rst_uniso_n), .d(d), .q_ret(q_ret));
endmodule
"#;
    let upf_comb_uniso = r#"
upf_version 2.1
create_supply_net VDD_BKUP
create_supply_net VDD_SW
create_supply_net VSS
create_supply_set SS_BKUP -function {power VDD_BKUP} -function {ground VSS}
create_supply_set SS_SW   -function {power VDD_SW}   -function {ground VSS}
add_power_state SS_BKUP -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW   -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                        -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
create_power_domain PD_TOP -include_scope -supply {primary SS_BKUP}
create_power_domain PD_RST -elements {u_rst} -supply {primary SS_SW}
create_power_domain PD_RET -elements {u_ret} -supply {primary SS_SW}
set_retention ret_lo -domain PD_RET -retention_supply_set SS_BKUP \
  -save_signal {raw_b high} -restore_signal {raw_b low}
"#;
    let viols_comb = run_rtl_upf("3c_rst_comb_sw", sv_comb, upf_comb_uniso, "top");
    assert!(
        viols_comb
            .iter()
            .any(|v| v.rule_id == "RET_ASYNC_RST_UNISOLATED"),
        "expected RET_ASYNC_RST_UNISOLATED on switchable comb cell: {viols_comb:#?}"
    );

    let upf_comb_iso = format!(
        "{upf_comb_uniso}\nset_isolation iso_rst -domain PD_RST -applies_to outputs -elements {{u_rst/rst_uniso_n}} -clamp_value 1 -isolation_supply_set SS_BKUP -isolation_signal raw_b -isolation_sense high -location parent\n"
    );
    let viols_comb_iso = run_rtl_upf("3c_rst_comb_iso", sv_comb, &upf_comb_iso, "top");
    assert!(
        !viols_comb_iso
            .iter()
            .any(|v| v.rule_id == "RET_ASYNC_RST_UNISOLATED"),
        "isolated switchable comb reset should not fire RET_ASYNC_RST_UNISOLATED: {viols_comb_iso:#?}"
    );

    // 2. `3C-rst-par-flop`: switchable flop `q_src` drives BOTH `rst_iso_n` (isolated) and
    // `rst_uniso_n` (unisolated), recombined via `&` -> must fire `RET_ASYNC_RST_UNISOLATED`.
    let sv_par = r#"
module rst_par_src(input logic clk, input logic raw, output logic rst_iso_n, output logic rst_uniso_n);
  logic q_src;
  always_ff @(posedge clk) q_src <= raw;
  assign rst_iso_n   = q_src;
  assign rst_uniso_n = q_src;
endmodule
module ret_blk(input logic clk, input logic rst_n, input logic d, output logic q_ret);
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) q_ret <= 1'b0;
    else q_ret <= d;
  end
endmodule
module top(input logic clk, input logic raw, input logic iso_en, input logic d, output logic q_ret);
  logic rst_iso_n, rst_uniso_n, rst_comb_n;
  rst_par_src u_rst(.clk(clk), .raw(raw), .rst_iso_n(rst_iso_n), .rst_uniso_n(rst_uniso_n));
  assign rst_comb_n = rst_iso_n & rst_uniso_n;
  ret_blk u_ret(.clk(clk), .rst_n(rst_comb_n), .d(d), .q_ret(q_ret));
endmodule
"#;
    let upf_par = r#"
upf_version 2.1
create_supply_net VDD_BKUP
create_supply_net VDD_SW
create_supply_net VSS
create_supply_set SS_BKUP -function {power VDD_BKUP} -function {ground VSS}
create_supply_set SS_SW   -function {power VDD_SW}   -function {ground VSS}
add_power_state SS_BKUP -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW   -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                        -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
create_power_domain PD_TOP -include_scope -supply {primary SS_BKUP}
create_power_domain PD_RST -elements {u_rst} -supply {primary SS_SW}
create_power_domain PD_RET -elements {u_ret} -supply {primary SS_SW}
set_isolation iso_rst_only -domain PD_RST -applies_to outputs -elements {u_rst/rst_iso_n} \
  -clamp_value 1 -isolation_supply_set SS_BKUP -isolation_signal iso_en -isolation_sense high -location parent
set_retention ret_lo -domain PD_RET -retention_supply_set SS_BKUP \
  -save_signal {iso_en high} -restore_signal {iso_en low}
"#;
    let viols_par = run_rtl_upf("3c_rst_par_flop", sv_par, upf_par, "top");
    assert!(
        viols_par
            .iter()
            .any(|v| v.rule_id == "RET_ASYNC_RST_UNISOLATED"),
        "expected RET_ASYNC_RST_UNISOLATED when parallel branch is unisolated: {viols_par:#?}"
    );
}

#[test]
fn regression_3c_rst_same_domain_internal_comb_vs_clean_direct_aon_port() {
    let upf = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_SW1
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW1 -function {power VDD_SW1} -function {ground VSS}
add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW1 -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
create_power_domain PD_RET -elements {u_ret} -supply {primary SS_SW1}
set_retention ret_u -domain PD_RET -retention_supply_set SS_AON \
  -save_signal {save_en high} -restore_signal {rest_en high}
"#;

    // 1. `3C-rst-same-dom-inv`: internal switchable combinational inverter inside `PD_RET` -> fires `RET_ASYNC_RST_UNISOLATED`.
    let sv_inv = r#"
module ret_blk(input logic clk, input logic raw_rst_n, input logic d, output logic q_ret);
  logic rst_int_n;
  assign rst_int_n = ~raw_rst_n;
  always_ff @(posedge clk or negedge rst_int_n) begin
    if (!rst_int_n) q_ret <= 1'b0;
    else q_ret <= d;
  end
endmodule
module top(input logic clk, input logic raw_rst_n, input logic save_en, input logic rest_en, input logic d, output logic q_ret);
  ret_blk u_ret(.clk(clk), .raw_rst_n(raw_rst_n), .d(d), .q_ret(q_ret));
endmodule
"#;
    let viols_inv = run_rtl_upf("3c_rst_same_dom_inv", sv_inv, upf, "top");
    assert!(
        viols_inv
            .iter()
            .any(|v| v.rule_id == "RET_ASYNC_RST_UNISOLATED"),
        "expected RET_ASYNC_RST_UNISOLATED for switchable combinational cell inside PD_RET: {viols_inv:#?}"
    );

    // 2. Clean direct AON port feed into `always_ff` -> must NOT fire `RET_ASYNC_RST_UNISOLATED`.
    let sv_clean = r#"
module ret_blk(input logic clk, input logic raw_rst_n, input logic d, output logic q_ret);
  always_ff @(posedge clk or negedge raw_rst_n) begin
    if (!raw_rst_n) q_ret <= 1'b0;
    else q_ret <= d;
  end
endmodule
module top(input logic clk, input logic raw_rst_n, input logic save_en, input logic rest_en, input logic d, output logic q_ret);
  ret_blk u_ret(.clk(clk), .raw_rst_n(raw_rst_n), .d(d), .q_ret(q_ret));
endmodule
"#;
    let viols_clean = run_rtl_upf("3c_rst_direct_aon_clean", sv_clean, upf, "top");
    assert!(
        !viols_clean
            .iter()
            .any(|v| v.rule_id == "RET_ASYNC_RST_UNISOLATED"),
        "direct AON port feed must stay clean: {viols_clean:#?}"
    );
}

#[test]
fn ret_async_reset_multibit_bus_checks_exact_reset_bit_slice() {
    let sv = r#"
module rst_src(input logic raw0, input logic raw1, output logic [1:0] rst_bus);
  assign rst_bus = {raw1, raw0};
endmodule
module ret_blk0(input logic clk, input logic [1:0] rst_bus, input logic d, output logic q_ret);
  always_ff @(posedge clk or negedge rst_bus[0]) begin
    if (!rst_bus[0]) q_ret <= 1'b0;
    else q_ret <= d;
  end
endmodule
module top(input logic clk, input logic raw0, input logic raw1, input logic iso_en, input logic d);
  logic [1:0] rst_bus;
  rst_src  u_src(.raw0(raw0), .raw1(raw1), .rst_bus(rst_bus));
  ret_blk0 u_ret(.clk(clk), .rst_bus(rst_bus), .d(d), .q_ret());
endmodule
"#;
    let upf_base = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_SW1
create_supply_net VDD_SW2
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW1 -function {power VDD_SW1} -function {ground VSS}
create_supply_set SS_SW2 -function {power VDD_SW2} -function {ground VSS}
add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW1 -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_SW2 -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
create_power_domain PD_SRC -elements {u_src} -supply {primary SS_SW1}
create_power_domain PD_RET -elements {u_ret} -supply {primary SS_SW2}
set_retention ret_u -domain PD_RET -retention_supply_set SS_AON \
  -save_signal {iso_en high} -restore_signal {iso_en low}
"#;

    // Bit [1] excluded, bit [0] (used by `always_ff @(negedge rst_bus[0])`) IS isolated ->
    // `ISO_MISSING` on `u_src/rst_bus[1]` (1b), and 0 `RET_ASYNC_RST_UNISOLATED`.
    let upf_bit0_iso = format!(
        "{upf_base}\nset_isolation iso_rst -domain PD_SRC -applies_to outputs -elements {{u_src/rst_bus}} -exclude_elements {{u_src/rst_bus[1]}} -clamp_value 1 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\n"
    );
    let viols_ok = run_rtl_upf("ret_rst_bus_bit0_iso", sv, &upf_bit0_iso, "top");
    let iso_miss: Vec<_> = viols_ok
        .iter()
        .filter(|v| v.rule_id == "ISO_MISSING")
        .collect();
    assert_eq!(iso_miss.len(), 1, "{viols_ok:#?}");
    assert_eq!(iso_miss[0].signal, "u_src/rst_bus[1]");
    assert_eq!(iso_miss[0].width_bits, 1);
    assert!(
        !viols_ok
            .iter()
            .any(|v| v.rule_id == "RET_ASYNC_RST_UNISOLATED"),
        "bit [0] is isolated; excluding unrelated bit [1] must not fire RET_ASYNC_RST_UNISOLATED: {viols_ok:#?}"
    );

    // Conversely, excluding bit [0] (the actual reset bit) MUST fire `RET_ASYNC_RST_UNISOLATED`.
    let upf_bit0_excl = format!(
        "{upf_base}\nset_isolation iso_rst -domain PD_SRC -applies_to outputs -elements {{u_src/rst_bus}} -exclude_elements {{u_src/rst_bus[0]}} -clamp_value 1 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\n"
    );
    let viols_bad = run_rtl_upf("ret_rst_bus_bit0_excl", sv, &upf_bit0_excl, "top");
    assert!(
        viols_bad
            .iter()
            .any(|v| v.rule_id == "RET_ASYNC_RST_UNISOLATED" && v.signal == "u_ret/rst_bus[0]"),
        "expected RET_ASYNC_RST_UNISOLATED on u_ret/rst_bus[0]: {viols_bad:#?}"
    );
}

#[test]
fn ret_async_reset_bitwise_conj_split_per_bit_isolation() {
    let sv = r#"
module rst_src(input logic clk, input logic raw0, input logic raw1, output logic [1:0] rst_bus);
  always_ff @(posedge clk) rst_bus <= {raw1, raw0};
endmodule
module ret_blk(input logic clk, input logic rst_n, input logic d, output logic q_ret);
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) q_ret <= 1'b0;
    else q_ret <= d;
  end
endmodule
module top(input logic clk, input logic raw0, input logic raw1, input logic iso_en, input logic d);
  logic [1:0] rst_bus;
  logic rst_comb;
  rst_src u_src(.clk(clk), .raw0(raw0), .raw1(raw1), .rst_bus(rst_bus));
  assign rst_comb = rst_bus[1] & rst_bus[0];
  ret_blk u_ret(.clk(clk), .rst_n(rst_comb), .d(d), .q_ret());
endmodule
"#;
    let upf_base = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_SW1
create_supply_net VDD_SW2
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW1 -function {power VDD_SW1} -function {ground VSS}
create_supply_set SS_SW2 -function {power VDD_SW2} -function {ground VSS}
add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW1 -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_SW2 -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
create_power_domain PD_SRC -elements {u_src} -supply {primary SS_SW1}
create_power_domain PD_RET -elements {u_ret} -supply {primary SS_SW2}
set_retention ret_u -domain PD_RET -retention_supply_set SS_AON \
  -save_signal {iso_en high} -restore_signal {iso_en low}
"#;

    // Separate per-bit isolation strategies covering both rst_bus[0] and rst_bus[1]
    // must clear both `ISO_MISSING` and `RET_ASYNC_RST_UNISOLATED`.
    let upf_split = format!(
        "{upf_base}\nset_isolation iso_0 -domain PD_SRC -applies_to outputs -elements {{u_src/rst_bus[0]}} -clamp_value 1 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\nset_isolation iso_1 -domain PD_SRC -applies_to outputs -elements {{u_src/rst_bus[1]}} -clamp_value 1 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\n"
    );
    let viols_clean = run_rtl_upf("ret_conj_split_clean", sv, &upf_split, "top");
    assert!(
        !viols_clean.iter().any(|v| matches!(
            v.rule_id.as_str(),
            "ISO_MISSING" | "RET_ASYNC_RST_UNISOLATED"
        )),
        "per-bit isolation on both rst_bus[0] and rst_bus[1] must stay clean: {viols_clean:#?}"
    );

    // Leaving bit [1] unisolated must still fire `RET_ASYNC_RST_UNISOLATED`.
    let upf_partial = format!(
        "{upf_base}\nset_isolation iso_0 -domain PD_SRC -applies_to outputs -elements {{u_src/rst_bus[0]}} -clamp_value 1 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\n"
    );
    let viols_bad = run_rtl_upf("ret_conj_split_partial", sv, &upf_partial, "top");
    assert!(
        viols_bad
            .iter()
            .any(|v| v.rule_id == "RET_ASYNC_RST_UNISOLATED" && v.signal == "u_ret/rst_n"),
        "expected RET_ASYNC_RST_UNISOLATED when rst_bus[1] is unisolated: {viols_bad:#?}"
    );
}

#[test]
fn ret_async_reset_coarse_isolation_masked_by_per_bit_no_isolation() {
    let sv = r#"
module rst_src(input logic clk, input logic raw0, input logic raw1, output logic [1:0] rst_bus);
  always_ff @(posedge clk) rst_bus <= {raw1, raw0};
endmodule
module ret_blk(input logic clk, input logic rst_n, input logic d, output logic q_ret);
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) q_ret <= 1'b0;
    else q_ret <= d;
  end
endmodule
module top(input logic clk, input logic raw0, input logic raw1, input logic iso_en, input logic d);
  logic [1:0] rst_bus;
  logic rst_comb;
  rst_src u_src(.clk(clk), .raw0(raw0), .raw1(raw1), .rst_bus(rst_bus));
  assign rst_comb = rst_bus[1] & rst_bus[0];
  ret_blk u_ret(.clk(clk), .rst_n(rst_comb), .d(d), .q_ret());
endmodule
"#;
    let upf_base = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_SW1
create_supply_net VDD_SW2
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW1 -function {power VDD_SW1} -function {ground VSS}
create_supply_set SS_SW2 -function {power VDD_SW2} -function {ground VSS}
add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW1 -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_SW2 -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
create_power_domain PD_SRC -elements {u_src} -supply {primary SS_SW1}
create_power_domain PD_RET -elements {u_ret} -supply {primary SS_SW2}
set_retention ret_u -domain PD_RET -retention_supply_set SS_AON \
  -save_signal {iso_en high} -restore_signal {iso_en low}
"#;

    let cases = [
        (
            "dom_noiso1",
            "set_isolation iso_all -domain PD_SRC -applies_to outputs -clamp_value 1 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\nset_isolation iso_no1 -domain PD_SRC -applies_to outputs -elements {u_src/rst_bus[1]} -no_isolation",
        ),
        (
            "bus_noiso1",
            "set_isolation iso_bus -domain PD_SRC -applies_to outputs -elements {u_src/rst_bus} -clamp_value 1 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\nset_isolation iso_no1 -domain PD_SRC -applies_to outputs -elements {u_src/rst_bus[1]} -no_isolation",
        ),
        (
            "slice_noiso1",
            "set_isolation iso_slice -domain PD_SRC -applies_to outputs -elements {u_src/rst_bus[1:0]} -clamp_value 1 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\nset_isolation iso_no1 -domain PD_SRC -applies_to outputs -elements {u_src/rst_bus[1]} -no_isolation",
        ),
    ];
    for (tag, strat) in cases {
        let upf = format!("{upf_base}\n{strat}\n");
        let viols = run_rtl_upf(tag, sv, &upf, "top");
        assert!(
            viols
                .iter()
                .any(|v| v.rule_id == "RET_ASYNC_RST_UNISOLATED" && v.signal == "u_ret/rst_n"),
            "[{tag}] per-bit -no_isolation on rst_bus[1] must trigger RET_ASYNC_RST_UNISOLATED: {viols:#?}"
        );
    }
}
