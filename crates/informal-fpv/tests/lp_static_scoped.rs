use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use informal_fpv::lp::{check_lp, load_upf, CheckLpOpts, LpStage, LpViolation};
use informal_fpv::Session;
use informal_upf::DEFAULT_VOLTAGE_TOL_UV;

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
        "informal_lp_scoped_{}_{n}_{tag}",
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
fn scoped_power_switch_and_retention_with_root_anchored_refs_and_bit_selects() {
    let sv = r#"
module sw_ctrl(input logic clk, input logic d, output logic [7:0] q);
  always_ff @(posedge clk) q <= {8{d}};
endmodule
module sub_comb(input logic a, output logic b);
  assign b = a;
endmodule
module top(input logic clk, input logic d, input logic iso_en, output logic b);
  logic [7:0] q;
  sw_ctrl  u_sw(.clk(clk), .d(d), .q(q));
  sub_comb u_sub(.a(q[0]), .b(b));
endmodule
"#;
    let upf = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_SW
create_supply_net VSS

create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}

add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW  -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}

create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
create_power_domain PD_SW  -elements {u_sw}  -supply {primary SS_SW}
create_power_domain PD_SUB -elements {u_sub} -supply {primary SS_SW}

set_scope u_sub
create_power_switch psw_scoped -domain /PD_SW -supply_set /SS_AON \
  -input_supply_port {in /VDD_AON} -output_supply_port {out /VDD_SW} \
  -control_port {ctrl /u_sw/q[0]} -on_state {ON in {ctrl}} -off_state {OFF {!ctrl}}
set_scope /

set_scope u_sw
set_retention ret_scoped -domain /PD_SUB -elements {/u_sub/b} -retention_supply_set /SS_SW \
  -save_signal {/u_sw/q[0] high} -restore_signal {/u_sw/q[0] low}
set_retention ret_ctrl_scoped -domain /PD_SW -elements {/u_sw/q} -retention_supply_set /SS_AON \
  -save_signal {/u_sw/q[0] high} -restore_signal {/u_sw/q[0] low}
set_scope /
"#;
    let viols = run_rtl_upf("scoped_psw_ret", sv, upf, "top");
    assert!(
        !viols.iter().any(|v| v.rule_id == "PSW_SUPPLY_UNCONNECTED"),
        "scoped /VDD_* switch supplies must resolve cleanly: {viols:#?}"
    );
    assert!(
        viols
            .iter()
            .any(|v| v.rule_id == "PSW_CTRL_UNPOWERED" && v.domain_dst.as_deref() == Some("PD_SW")),
        "scoped create_power_switch -domain /PD_SW -control_port /u_sw/q[0] must fire PSW_CTRL_UNPOWERED: {viols:#?}"
    );
    assert!(
        viols
            .iter()
            .any(|v| v.rule_id == "RET_BACKUP_OFF" && v.domain_src.as_deref() == Some("PD_SUB")),
        "scoped set_retention -domain /PD_SUB -retention_supply_set /SS_SW must fire RET_BACKUP_OFF: {viols:#?}"
    );
    assert!(
        viols
            .iter()
            .any(|v| v.rule_id == "RET_NO_ELEMENTS" && v.domain_src.as_deref() == Some("PD_SUB")),
        "scoped set_retention -domain /PD_SUB matching 0 flops must fire RET_NO_ELEMENTS: {viols:#?}"
    );
    assert!(
        viols
            .iter()
            .any(|v| v.rule_id == "RET_CTRL_UNPOWERED" && v.domain_dst.as_deref() == Some("PD_SW")),
        "scoped set_retention -save_signal /u_sw/q[0] must fire RET_CTRL_UNPOWERED: {viols:#?}"
    );
}

#[test]
fn scoped_level_shifter_location_and_supply_sets() {
    let sv = r#"
module blk_lo(output logic [7:0] q);
  assign q = 8'h5a;
endmodule
module blk_hi(input logic [7:0] q, output logic y);
  assign y = ^q;
endmodule
module sub_scope(input logic a, output logic b);
  assign b = a;
endmodule
module top(input logic a, output logic y, output logic b);
  logic [7:0] q;
  blk_lo    u_lo(.q(q));
  blk_hi    u_hi(.q(q), .y(y));
  sub_scope u_sub(.a(a), .b(b));
endmodule
"#;
    let upf_self = r#"
upf_version 2.1
create_supply_net VDD_LO
create_supply_net VDD_HI
create_supply_net VSS
create_supply_set SS_LO -function {power VDD_LO} -function {ground VSS}
create_supply_set SS_HI -function {power VDD_HI} -function {ground VSS}
add_power_state SS_LO -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_HI -state {ON -supply_expr {power == {FULL_ON 1.8} && ground == {FULL_ON 0.0}} -simstate NORMAL}
create_power_domain PD_TOP -include_scope -supply {primary SS_HI}
create_power_domain PD_LO  -elements {u_lo} -supply {primary SS_LO}
create_power_domain PD_HI  -elements {u_hi} -supply {primary SS_HI}

set_scope u_sub
set_level_shifter ls_self -domain /PD_LO -applies_to outputs -elements {/} -location self
set_scope /
"#;
    let v_self = run_rtl_upf("scoped_ls_self", sv, upf_self, "top");
    assert!(
        v_self
            .iter()
            .any(|v| v.rule_id == "LS_LOCATION" && v.domain_src.as_deref() == Some("PD_LO")),
        "scoped set_level_shifter -domain /PD_LO -location self must fire LS_LOCATION: {v_self:#?}"
    );

    let upf_ss = r#"
upf_version 2.1
create_supply_net VDD_LO
create_supply_net VDD_HI
create_supply_net VSS
create_supply_set SS_LO -function {power VDD_LO} -function {ground VSS}
create_supply_set SS_HI -function {power VDD_HI} -function {ground VSS}
add_power_state SS_LO -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_HI -state {ON -supply_expr {power == {FULL_ON 1.8} && ground == {FULL_ON 0.0}} -simstate NORMAL}
create_power_domain PD_TOP -include_scope -supply {primary SS_HI}
create_power_domain PD_LO  -elements {u_lo} -supply {primary SS_LO}
create_power_domain PD_HI  -elements {u_hi} -supply {primary SS_HI}

set_scope u_sub
set_level_shifter ls_ss -domain /PD_LO -applies_to outputs -elements {/} \
  -input_supply_set /SS_HI -output_supply_set /SS_HI -location parent
set_scope /
"#;
    let v_ss = run_rtl_upf("scoped_ls_ss", sv, upf_ss, "top");
    assert!(
        v_ss.iter()
            .any(|v| v.rule_id == "LS_LOCATION" && v.message.contains("-input_supply_set")),
        "scoped set_level_shifter -input_supply_set /SS_HI must fire LS_LOCATION: {v_ss:#?}"
    );
}

#[test]
fn scoped_repeater_dot_and_slash_suppresses_feedthrough_and_honors_exclude() {
    let sv = r#"
module src_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid_blk(input logic in_s, output logic out_s);
  buf u_buf(out_s, in_s);
endmodule
module dst_blk(input logic a, output logic y);
  assign y = a;
endmodule
module top(input logic clk, input logic d, input logic iso_en, output logic y);
  logic q, ft;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  mid_blk u_mid(.in_s(q), .out_s(ft));
  dst_blk u_dst(.a(ft), .y(y));
endmodule
"#;
    let upf_hdr = r#"
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
create_power_domain PD_SRC -elements {u_src} -supply {primary SS_AON}
create_power_domain PD_MID -elements {u_mid} -supply {primary SS_SW}
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_AON}
"#;

    let upf_dot = format!(
        "{upf_hdr}\nset_scope u_mid\nset_repeater rep_dot -domain PD_MID -elements {{.}} -repeater_supply /SS_AON\nset_scope /\n"
    );
    let v_dot = run_rtl_upf("rep_scoped_dot", sv, &upf_dot, "top");
    assert!(
        !v_dot
            .iter()
            .any(|v| v.rule_id.starts_with("ISO_FEEDTHROUGH")),
        "set_scope u_mid; set_repeater -domain PD_MID -elements {{.}} -repeater_supply /SS_AON must suppress ISO_FEEDTHROUGH_BUFFER: {v_dot:#?}"
    );

    let upf_slash = format!(
        "{upf_hdr}\nset_scope u_src\nset_repeater rep_slash -domain /PD_MID -elements {{/}} -repeater_supply /SS_AON\nset_scope /\n"
    );
    let v_slash = run_rtl_upf("rep_scoped_slash", sv, &upf_slash, "top");
    assert!(
        !v_slash
            .iter()
            .any(|v| v.rule_id.starts_with("ISO_FEEDTHROUGH")),
        "set_scope u_src; set_repeater -domain /PD_MID -elements {{/}} -repeater_supply /SS_AON must suppress ISO_FEEDTHROUGH_BUFFER: {v_slash:#?}"
    );

    let upf_excl = format!(
        "{upf_hdr}\nset_scope u_mid\nset_repeater rep_excl -domain /PD_MID -elements {{.}} -exclude_elements {{in_s}} -repeater_supply /SS_AON\nset_scope /\n"
    );
    let v_excl = run_rtl_upf("rep_scoped_excl", sv, &upf_excl, "top");
    assert!(
        v_excl
            .iter()
            .any(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER"),
        "excluding feedthrough net in set_repeater must leave ISO_FEEDTHROUGH_BUFFER active: {v_excl:#?}"
    );
}

#[test]
fn stage1_pst_missing_port_state_with_leading_slash_supplies() {
    let dir = new_temp("pst_slash");
    let upf_path = dir.0.join("pst_slash.upf");
    let upf = r#"
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
create_power_domain PD_SW  -supply {primary SS_SW}
create_pst my_pst -supplies {/VDD_AON /VDD_SW}
"#;
    std::fs::write(&upf_path, upf).unwrap();
    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    load_upf(&mut s, &upf_path, None, None, false).unwrap();
    check_lp(
        &mut s,
        &CheckLpOpts {
            stage: LpStage::Upf,
            tol_uv: DEFAULT_VOLTAGE_TOL_UV,
            ..Default::default()
        },
    )
    .unwrap();
    let missing: Vec<_> =
        s.lp.violations
            .iter()
            .filter(|v| v.rule_id == "PST_MISSING_PORT_STATE")
            .collect();
    assert_eq!(missing.len(), 2, "{:#?}", s.lp.violations);
}

#[test]
fn port_attributes_honor_exclude_elements_and_domains() {
    let sv = r#"
module sub_blk(input logic x, output logic z);
  assign z = x;
endmodule
module top(input logic in_a, input logic in_b, output logic out_y);
  sub_blk u_sub(.x(in_a ^ in_b), .z(out_y));
endmodule
"#;
    let upf_hdr = r#"
upf_version 2.1
create_supply_net VDD_SW
create_supply_net VDD_AON
create_supply_net VSS
create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
add_power_state SS_SW  -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
create_power_domain PD_TOP   -include_scope -supply {primary SS_SW}
create_power_domain PD_OTHER -elements {u_sub} -supply {primary SS_AON}
"#;

    let upf_excl = format!(
        "{upf_hdr}\nset_port_attributes -elements {{.}} -exclude_elements {{in_a}} -driver_supply SS_AON -receiver_supply SS_AON\n"
    );
    let v_excl = run_rtl_upf("pa_excl", sv, &upf_excl, "top");
    let pa_excl: Vec<_> = v_excl
        .iter()
        .filter(|v| v.rule_id == "PORT_ATTR_MISSING")
        .map(|v| v.signal.as_str())
        .collect();
    assert_eq!(pa_excl, vec!["in_a"], "{v_excl:#?}");

    let upf_other_dom = format!(
        "{upf_hdr}\nset_port_attributes -elements {{.}} -domains {{PD_OTHER}} -driver_supply SS_AON -receiver_supply SS_AON\n"
    );
    let v_other = run_rtl_upf("pa_wrong_dom", sv, &upf_other_dom, "top");
    let pa_other: Vec<_> = v_other
        .iter()
        .filter(|v| v.rule_id == "PORT_ATTR_MISSING")
        .map(|v| v.signal.as_str())
        .collect();
    assert!(
        pa_other.contains(&"in_a") && pa_other.contains(&"in_b") && pa_other.contains(&"out_y"),
        "wrong -domains {{PD_OTHER}} must not suppress PORT_ATTR_MISSING on PD_TOP: {v_other:#?}"
    );

    let upf_right_dom = format!(
        "{upf_hdr}\nset_port_attributes -elements {{.}} -domains {{PD_TOP}} -driver_supply SS_AON -receiver_supply SS_AON\n"
    );
    let v_right = run_rtl_upf("pa_right_dom", sv, &upf_right_dom, "top");
    assert!(
        !v_right.iter().any(|v| v.rule_id == "PORT_ATTR_MISSING"),
        "matching -domains {{PD_TOP}} must clear PORT_ATTR_MISSING: {v_right:#?}"
    );
}

#[test]
fn r10_fix1_ls_location_self_without_applies_to() {
    let sv = r#"
module blk_lo(output logic [7:0] q);
  assign q = 8'h5a;
endmodule
module blk_hi(input logic [7:0] q, output logic y);
  assign y = ^q;
endmodule
module sub_scope(input logic a, output logic b);
  assign b = a;
endmodule
module top(input logic a, output logic y, output logic b);
  logic [7:0] q;
  blk_lo    u_lo(.q(q));
  blk_hi    u_hi(.q(q), .y(y));
  sub_scope u_sub(.a(a), .b(b));
endmodule
"#;
    let upf = r#"
upf_version 2.1
create_supply_net VDD_LO
create_supply_net VDD_HI
create_supply_net VSS
create_supply_set SS_LO -function {power VDD_LO} -function {ground VSS}
create_supply_set SS_HI -function {power VDD_HI} -function {ground VSS}
add_power_state SS_LO -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_HI -state {ON -supply_expr {power == {FULL_ON 1.8} && ground == {FULL_ON 0.0}} -simstate NORMAL}
create_power_domain PD_TOP -include_scope -supply {primary SS_HI}
create_power_domain PD_LO  -elements {u_lo} -supply {primary SS_LO}
create_power_domain PD_HI  -elements {u_hi} -supply {primary SS_HI}

set_scope u_sub
set_level_shifter ls_self_default -domain /PD_LO -elements {/} -location self
set_scope /
"#;
    let viols = run_rtl_upf("r10_ls_self_no_applies", sv, upf, "top");
    assert!(
        viols
            .iter()
            .any(|v| v.rule_id == "LS_LOCATION" && v.domain_src.as_deref() == Some("PD_LO")),
        "omitted -applies_to defaults to both and must fire LS_LOCATION for -location self: {viols:#?}"
    );
}

#[test]
fn r10_fix2_repeater_supply_must_cover_downstream_cells() {
    let sv = r#"
module src_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid_blk(input logic in_s, output logic out_s);
  buf u_buf(out_s, in_s);
endmodule
module dst_blk(input logic a, output logic y);
  assign y = a;
endmodule
module top(input logic clk, input logic d, input logic iso_en, output logic y);
  logic q, ft;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  mid_blk u_mid(.in_s(q), .out_s(ft));
  dst_blk u_dst(.a(ft), .y(y));
endmodule
"#;
    let upf_hdr = r#"
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
create_power_domain PD_SRC -elements {u_src} -supply {primary SS_SW1}
create_power_domain PD_MID -elements {u_mid} -supply {primary SS_SW2}
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_AON}
set_isolation iso_src -domain PD_SRC -isolation_supply_set SS_AON -clamp_value 0 -isolation_signal iso_en -isolation_sense high
set_isolation iso_mid -domain PD_MID -isolation_supply_set SS_AON -clamp_value 0 -isolation_signal iso_en -isolation_sense high
"#;
    let upf_sw_rep = format!(
        "{upf_hdr}\nset_repeater r_bad -domain PD_MID -elements {{.}} -repeater_supply SS_SW1\n"
    );
    let v_bad = run_rtl_upf("r10_rep_sw1", sv, &upf_sw_rep, "top");
    assert!(
        v_bad
            .iter()
            .any(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER"),
        "repeater_supply SS_SW1 can turn off while downstream PD_DST is ON, so ISO_FEEDTHROUGH_BUFFER must fire: {v_bad:#?}"
    );

    let upf_aon_rep = format!(
        "{upf_hdr}\nset_repeater r_ok -domain PD_MID -elements {{.}} -repeater_supply SS_AON\n"
    );
    let v_ok = run_rtl_upf("r10_rep_aon", sv, &upf_aon_rep, "top");
    assert!(
        !v_ok
            .iter()
            .any(|v| v.rule_id.starts_with("ISO_FEEDTHROUGH")),
        "repeater_supply SS_AON stays ON for both src and downstream dst: {v_ok:#?}"
    );
}

#[test]
fn r10_fix3_scoped_bare_relative_signal_resolution() {
    let sv = r#"
module ctrl_blk(input logic clk, input logic d, output logic [1:0] \esc$sig , output logic iso_en);
  always_ff @(posedge clk) \esc$sig [0] <= d;
  assign \esc$sig [1] = 1'b1;
  always_ff @(posedge clk) iso_en <= d;
endmodule
module sw_blk(input logic clk, input logic d, output logic [3:0] q);
  always_ff @(posedge clk) q <= {4{d}};
endmodule
module leaf_blk(input logic a, output logic y);
  assign y = a;
endmodule
module mid_wrap(input logic a, output logic y);
  leaf_blk u_mid(.a(a), .y(y));
endmodule
module top(input logic clk, input logic d, output logic y);
  logic [1:0] \esc$sig ;
  logic iso_en;
  logic [3:0] q;
  ctrl_blk u_ctrl(.clk(clk), .d(d), .\esc$sig (\esc$sig ), .iso_en(iso_en));
  sw_blk   u_sw(.clk(clk), .d(d), .q(q));
  mid_wrap u_sub(.a(q[0]), .y(y));
endmodule
"#;
    let upf_hdr = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_SW
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW  -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
create_power_domain PD_TOP  -include_scope -supply {primary SS_AON}
create_power_domain PD_CTRL -elements {u_ctrl} -supply {primary SS_SW}
create_power_domain PD_SW   -elements {u_sw}   -supply {primary SS_SW}
create_power_domain PD_SUB  -elements {u_sub}  -supply {primary SS_AON}
"#;
    let upf_esc = format!(
        "{upf_hdr}\nset_scope u_sub\nset_isolation iso_esc -domain /PD_SW -applies_to outputs -isolation_supply_set /SS_AON -clamp_value 0 -isolation_signal {{u_ctrl/\\esc$sig [0]}} -isolation_sense high\nset_scope /\n"
    );
    let v_esc = run_rtl_upf("r10_bare_esc", sv, &upf_esc, "top");
    assert!(
        v_esc.iter().any(|v| v.rule_id == "ISO_CTRL_UNPOWERED"),
        "bare-relative escaped signal u_ctrl/\\esc$sig [0] in set_scope u_sub must fire ISO_CTRL_UNPOWERED: {v_esc:#?}"
    );

    let upf_nested = format!(
        "{upf_hdr}\nset_scope u_sub/u_mid\nset_isolation iso_nest -domain /PD_SW -applies_to outputs -isolation_supply_set /SS_AON -clamp_value 0 -isolation_signal {{u_ctrl/iso_en}} -isolation_sense high\nset_scope /\n"
    );
    let v_nest = run_rtl_upf("r10_bare_nest", sv, &upf_nested, "top");
    assert!(
        v_nest.iter().any(|v| v.rule_id == "ISO_CTRL_UNPOWERED"),
        "bare-relative u_ctrl/iso_en in nested set_scope u_sub/u_mid must fire ISO_CTRL_UNPOWERED: {v_nest:#?}"
    );

    let upf_psw_ret = format!(
        "{upf_hdr}\nset_scope u_sub\ncreate_power_switch psw1 -domain /PD_SW -supply_set /SS_AON -input_supply_port {{in /VDD_AON}} -output_supply_port {{out /VDD_SW}} -control_port {{ctrl u_sw/q[0]}} -on_state {{ON in {{ctrl}}}} -off_state {{OFF {{!ctrl}}}}\nset_retention r1 -domain /PD_SW -elements {{/u_sw/q}} -retention_supply_set /SS_AON -save_signal {{u_sw/q[0] high}} -restore_signal {{u_sw/q[0] low}}\nset_scope /\n"
    );
    let v_pr = run_rtl_upf("r10_bare_psw_ret", sv, &upf_psw_ret, "top");
    assert!(
        v_pr.iter().any(|v| v.rule_id == "PSW_CTRL_UNPOWERED"),
        "bare-relative u_sw/q[0] in set_scope u_sub must fire PSW_CTRL_UNPOWERED: {v_pr:#?}"
    );
    assert!(
        v_pr.iter().any(|v| v.rule_id == "RET_CTRL_UNPOWERED"),
        "bare-relative u_sw/q[0] in set_scope u_sub must fire RET_CTRL_UNPOWERED: {v_pr:#?}"
    );
}

#[test]
fn r10_fix4_port_attributes_escaped_top_prefixed_and_split_inout() {
    let sv_esc = r#"
module sub_blk(input logic x, output logic z);
  assign z = x;
endmodule
module top(input logic \esc$in , output logic out_y);
  sub_blk u_sub(.x(\esc$in ), .z(out_y));
endmodule
"#;
    let upf_hdr = r#"
upf_version 2.1
create_supply_net VDD_SW
create_supply_net VDD_AON
create_supply_net VSS
create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
add_power_state SS_SW  -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
create_power_domain PD_TOP   -include_scope -supply {primary SS_SW}
create_power_domain PD_OTHER -elements {u_sub} -supply {primary SS_AON}
"#;
    let upf_esc = format!(
        "{upf_hdr}\nset_port_attributes -ports {{\\esc$in  out_y}} -driver_supply SS_AON -receiver_supply SS_AON\n"
    );
    let v_esc = run_rtl_upf("r10_pa_esc", sv_esc, &upf_esc, "top");
    assert!(
        !v_esc.iter().any(|v| v.rule_id == "PORT_ATTR_MISSING"),
        "escaped -ports {{\\esc$in }} must match top port: {v_esc:#?}"
    );

    let upf_top = format!(
        "{upf_hdr}\nset_port_attributes -ports {{top/\\esc$in  top/out_y}} -driver_supply SS_AON -receiver_supply SS_AON\n"
    );
    let v_top = run_rtl_upf("r10_pa_top", sv_esc, &upf_top, "top");
    assert!(
        !v_top.iter().any(|v| v.rule_id == "PORT_ATTR_MISSING"),
        "top/-prefixed -ports must match top ports: {v_top:#?}"
    );

    let sv_io = r#"
module sub_blk(input logic x, output logic z);
  assign z = x;
endmodule
module top(input logic in_a, inout wire io_p, output logic out_y);
  sub_blk u_sub(.x(in_a), .z(out_y));
  assign io_p = out_y;
endmodule
"#;
    let upf_split_io = format!(
        "{upf_hdr}\nset_port_attributes -ports {{in_a io_p}} -applies_to inputs -driver_supply SS_AON\nset_port_attributes -ports {{out_y io_p}} -applies_to outputs -receiver_supply SS_AON\n"
    );
    let v_io = run_rtl_upf("r10_pa_inout_split", sv_io, &upf_split_io, "top");
    assert!(
        !v_io.iter().any(|v| v.rule_id == "PORT_ATTR_MISSING"),
        "split -applies_to inputs + outputs declarations must satisfy inout port: {v_io:#?}"
    );

    let upf_half_io = format!(
        "{upf_hdr}\nset_port_attributes -ports {{in_a io_p}} -applies_to inputs -driver_supply SS_AON\nset_port_attributes -ports {{out_y}} -applies_to outputs -receiver_supply SS_AON\n"
    );
    let v_half = run_rtl_upf("r10_pa_inout_half", sv_io, &upf_half_io, "top");
    let missing_half: Vec<_> = v_half
        .iter()
        .filter(|v| v.rule_id == "PORT_ATTR_MISSING")
        .map(|v| v.signal.as_str())
        .collect();
    assert_eq!(missing_half, vec!["io_p"], "{v_half:#?}");
}

#[test]
fn r10_fix5_repeater_filters_2hop_buf_and_intermediate_exclude() {
    let sv = r#"
module src_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid_blk(input logic in1, input logic in2, output logic out1, output logic out2);
  wire ft_buf1, ft_buf2;
  assign ft_buf1 = in1;
  buf u_b1(out1, ft_buf1);
  assign ft_buf2 = in2;
  buf u_b2(out2, ft_buf2);
endmodule
module dst_blk(input logic a, output logic y);
  assign y = a;
endmodule
module top(input logic clk, input logic d, input logic iso_en, output logic y1, output logic y2);
  logic q1, q2, ft1, ft2;
  src_blk u_src1(.clk(clk), .d(d), .q(q1));
  src_blk u_src2(.clk(clk), .d(d), .q(q2));
  mid_blk u_mid(.in1(q1), .in2(q2), .out1(ft1), .out2(ft2));
  dst_blk u_dst1(.a(ft1), .y(y1));
  dst_blk u_dst2(.a(ft2), .y(y2));
endmodule
"#;
    let upf_hdr = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_SW
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW  -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
create_power_domain PD_TOP  -include_scope -supply {primary SS_AON}
create_power_domain PD_SRC1 -elements {u_src1} -supply {primary SS_AON}
create_power_domain PD_SRC2 -elements {u_src2} -supply {primary SS_AON}
create_power_domain PD_MID  -elements {u_mid}  -supply {primary SS_SW}
create_power_domain PD_DST1 -elements {u_dst1} -supply {primary SS_AON}
create_power_domain PD_DST2 -elements {u_dst2} -supply {primary SS_AON}
"#;
    let v_norep = run_rtl_upf("r10_2hop_norep", sv, upf_hdr, "top");
    let ft_buf_viols: Vec<_> = v_norep
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER")
        .collect();
    assert_eq!(
        ft_buf_viols.len(),
        2,
        "2-hop assign+buf must classify as ISO_FEEDTHROUGH_BUFFER: {v_norep:#?}"
    );
    assert!(
        !v_norep.iter().any(|v| v.rule_id == "ISO_FEEDTHROUGH"),
        "2-hop assign+buf must not emit pure-wire ISO_FEEDTHROUGH: {v_norep:#?}"
    );

    let upf_src1 = format!(
        "{upf_hdr}\nset_repeater r_s1 -domain PD_MID -source PD_SRC1 -repeater_supply SS_AON\n"
    );
    let v_src1 = run_rtl_upf("r10_rep_src1", sv, &upf_src1, "top");
    let rem_src: Vec<_> = v_src1
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER")
        .map(|v| v.signal.as_str())
        .collect();
    assert_eq!(rem_src, vec!["u_src2/q"], "{v_src1:#?}");

    let upf_sink1 = format!(
        "{upf_hdr}\nset_repeater r_d1 -domain PD_MID -sink PD_DST1 -repeater_supply SS_AON\n"
    );
    let v_sink1 = run_rtl_upf("r10_rep_sink1", sv, &upf_sink1, "top");
    let rem_sink: Vec<_> = v_sink1
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER")
        .map(|v| v.signal.as_str())
        .collect();
    assert_eq!(rem_sink, vec!["u_src2/q"], "{v_sink1:#?}");

    let upf_dir_mismatch = format!(
        "{upf_hdr}\nset_repeater r_bad_dir -domain PD_MID -applies_to inputs -elements {{u_mid/out1 u_mid/out2}} -repeater_supply SS_AON\n"
    );
    let v_dir = run_rtl_upf("r10_rep_dir", sv, &upf_dir_mismatch, "top");
    assert_eq!(
        v_dir
            .iter()
            .filter(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER")
            .count(),
        2,
        "-applies_to inputs with output elements must not match: {v_dir:#?}"
    );

    let upf_excl_mid = format!(
        "{upf_hdr}\nset_repeater r_excl -domain PD_MID -elements {{/}} -exclude_elements {{u_mid/ft_buf1}} -repeater_supply SS_AON\n"
    );
    let v_excl = run_rtl_upf("r10_rep_excl_mid", sv, &upf_excl_mid, "top");
    let rem_excl: Vec<_> = v_excl
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER")
        .map(|v| v.signal.as_str())
        .collect();
    assert_eq!(rem_excl, vec!["u_src1/q"], "{v_excl:#?}");
}
