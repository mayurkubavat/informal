use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use informal_fpv::lp::{check_lp, load_upf, CheckLpOpts, LpViolation};
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
    let p = std::env::temp_dir().join(format!("informal_lp_ft_{}_{n}_{tag}", std::process::id()));
    std::fs::create_dir_all(&p).expect("mkdir");
    TempDir(p)
}

fn run_rtl_upf(tag: &str, sv: &str, upf: &str, top: &str) -> Vec<LpViolation> {
    let dir = new_temp(tag);
    let sv_path = dir.0.join("design.sv");
    let upf_path = dir.0.join("intent.upf");
    std::fs::write(&sv_path, sv).expect("write sv");
    std::fs::write(&upf_path, upf).expect("write upf");

    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    s.design.files.push(sv_path);
    s.elaborate(Some(top.to_string())).expect("elaborate");
    load_upf(&mut s, &upf_path, None, None, false).expect("load_upf");
    check_lp(&mut s, &CheckLpOpts::default()).expect("check_lp");
    s.lp.violations
}

const SUPPLY_HDR: &str = r#"
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
"#;

#[test]
fn r10_bug1_shadowed_root_slash_and_dotdot_signal_refs_in_set_scope() {
    let sv = r#"
module sub_blk(input logic clk, input logic d, output logic q, output logic y);
  wire iso_sig = 1'b1;
  wire psw_sig = 1'b1;
  wire ret_sig = 1'b0;
  always_ff @(posedge clk) q <= d;
  assign y = iso_sig ^ psw_sig ^ ret_sig;
endmodule
module sw_drv(input logic clk, input logic d, output logic s);
  always_ff @(posedge clk) s <= d;
endmodule
module top(input logic clk, input logic d, output logic y);
  logic iso_sig, psw_sig, ret_sig, q_sub;
  sw_drv u_sw_iso(.clk(clk), .d(d), .s(iso_sig));
  sw_drv u_sw_psw(.clk(clk), .d(d), .s(psw_sig));
  sw_drv u_sw_ret(.clk(clk), .d(d), .s(ret_sig));
  sub_blk u_sub(.clk(clk), .d(d), .q(q_sub), .y(y));
endmodule
"#;

    // Both `/sig` and `../sig` from `set_scope u_sub` must resolve to root switchable nets,
    // NOT the shadowed local AON `u_sub/*` nets!
    for sig_form in ["/", "../"] {
        let upf = format!(
            r#"{SUPPLY_HDR}
create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SW  -elements {{u_sw_iso u_sw_psw u_sw_ret}} -supply {{primary SS_SW1}}
set_scope u_sub
create_power_domain PD_SUB -elements {{.}} -supply {{primary /SS_SW2}}
set_isolation iso_sub -domain PD_SUB -applies_to outputs -clamp_value 0 \
  -isolation_supply_set /SS_AON -isolation_signal {sig_form}iso_sig -isolation_sense high -location parent
set_retention ret_sub -domain PD_SUB -retention_supply_set /SS_AON \
  -save_signal {{{sig_form}ret_sig high}} -restore_signal {{{sig_form}ret_sig low}}
create_power_switch psw_sub -domain PD_SUB -supply_set /SS_AON \
  -input_supply_port {{vdd_in /VDD_AON}} -output_supply_port {{vdd_out /VDD_SW2}} \
  -control_port {{ctrl {sig_form}psw_sig}} -on_state {{ON vdd_in ctrl}}
set_scope /
"#
        );
        let viols = run_rtl_upf(&format!("bug1_{}", sig_form.len()), sv, &upf, "top");
        assert!(
            viols.iter().any(|v| v.rule_id == "ISO_CTRL_UNPOWERED"),
            "expected ISO_CTRL_UNPOWERED for `{sig_form}iso_sig`: {viols:#?}"
        );
        assert!(
            viols.iter().any(|v| v.rule_id == "RET_CTRL_UNPOWERED"),
            "expected RET_CTRL_UNPOWERED for `{sig_form}ret_sig`: {viols:#?}"
        );
        assert!(
            viols.iter().any(|v| v.rule_id == "PSW_CTRL_UNPOWERED"),
            "expected PSW_CTRL_UNPOWERED for `{sig_form}psw_sig`: {viols:#?}"
        );
    }
}

#[test]
fn r10_bug2_parent_assign_passthrough_alias_traces_to_child_sink() {
    let sv = r#"
module src_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid_blk(input logic in_s, output logic out_s);
  assign out_s = in_s;
endmodule
module dst_blk(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic d, output logic y);
  logic q, m, m_alias, m_alias2;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  mid_blk u_mid(.in_s(q), .out_s(m));
  assign m_alias = m;
  assign m_alias2 = m_alias;
  dst_blk u_dst(.clk(clk), .a(m_alias2), .y(y));
endmodule
"#;
    let base_upf = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC -elements {{u_src}} -supply {{primary SS_SW1}}
create_power_domain PD_MID -elements {{u_mid}} -supply {{primary SS_SW2}}
create_power_domain PD_DST -elements {{u_dst}} -supply {{primary SS_AON}}
set_isolation iso_s -domain PD_SRC -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_m -domain PD_MID -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
set_retention r_dst -domain PD_DST -retention_supply_set SS_AON -save_signal {{clk high}}
"#
    );

    // 1. `-sink PD_TOP` must NOT suppress `ISO_FEEDTHROUGH` because real sink is `u_dst` (`PD_DST`).
    let upf_bad_sink = format!(
        "{base_upf}\nset_repeater rep_top -domain PD_MID -sink PD_TOP -repeater_supply SS_AON\n"
    );
    let v1 = run_rtl_upf("bug2_bad_sink", sv, &upf_bad_sink, "top");
    assert!(
        v1.iter().any(|v| v.rule_id == "ISO_FEEDTHROUGH"),
        "unsound FN: -sink PD_TOP must not hide uncovered u_dst: {v1:#?}"
    );

    // 2. `-sink PD_DST` MUST cleanly cover the feedthrough through `assign m_alias = m;`.
    let upf_good_sink = format!(
        "{base_upf}\nset_repeater rep_dst -domain PD_MID -sink PD_DST -repeater_supply SS_AON\n"
    );
    let v2 = run_rtl_upf("bug2_good_sink", sv, &upf_good_sink, "top");
    assert!(
        !v2.iter().any(|v| v.rule_id.starts_with("ISO_FEEDTHROUGH")),
        "expected no ISO_FEEDTHROUGH with -sink PD_DST: {v2:#?}"
    );

    // 3. Same-domain `u_dst` in `PD_MID` via `assign m_alias = m` in `top` (`PD_TOP` AON): zero `ISO_FEEDTHROUGH`.
    let upf_same = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC -elements {{u_src}} -supply {{primary SS_SW1}}
create_power_domain PD_MID -elements {{u_mid u_dst}} -supply {{primary SS_SW2}}
set_isolation iso_s -domain PD_SRC -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_m -domain PD_MID -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
set_retention r_mid -domain PD_MID -retention_supply_set SS_AON -save_signal {{clk high}}
"#
    );
    let v3 = run_rtl_upf("bug2_same_dom", sv, &upf_same, "top");
    assert!(
        !v3.iter().any(|v| v.rule_id.starts_with("ISO_FEEDTHROUGH")),
        "spurious ISO_FEEDTHROUGH on same-domain alias: {v3:#?}"
    );
}

#[test]
fn r10_bug3_multibit_exit_pin_per_bit_sink_repeaters() {
    let sv = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid_blk(input logic [1:0] in_s, output logic [1:0] out_s);
  assign out_s[0] = in_s[0];
  assign out_s[1] = in_s[1];
endmodule
module dst_blk(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic [1:0] d, output logic y1, output logic y2);
  logic [1:0] q, m;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  mid_blk u_mid(.in_s(q), .out_s(m));
  dst_blk u_dst1(.clk(clk), .a(m[0]), .y(y1));
  dst_blk u_dst2(.clk(clk), .a(m[1]), .y(y2));
endmodule
"#;
    let base_upf = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP  -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC  -elements {{u_src}}  -supply {{primary SS_SW1}}
create_power_domain PD_MID  -elements {{u_mid}}  -supply {{primary SS_SW2}}
create_power_domain PD_DST1 -elements {{u_dst1}} -supply {{primary SS_AON}}
create_power_domain PD_DST2 -elements {{u_dst2}} -supply {{primary SS_AON}}
set_isolation iso_s -domain PD_SRC -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_m -domain PD_MID -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
set_repeater rep1 -domain PD_MID -elements {{u_mid/in_s[0]}} -sink PD_DST1 -repeater_supply SS_AON
"#
    );

    // Only bit 0 covered -> bit 1 still fires `ISO_FEEDTHROUGH`.
    let v_partial = run_rtl_upf("bug3_partial", sv, &base_upf, "top");
    let fts: Vec<_> = v_partial
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH")
        .collect();
    assert_eq!(fts.len(), 1, "{v_partial:#?}");
    assert_eq!(fts[0].width_bits, 1);

    // Both bits covered by their per-bit `-sink` repeaters -> 0 `ISO_FEEDTHROUGH`.
    let full_upf = format!(
        "{base_upf}\nset_repeater rep2 -domain PD_MID -elements {{u_mid/in_s[1]}} -sink PD_DST2 -repeater_supply SS_AON\n"
    );
    let v_full = run_rtl_upf("bug3_full", sv, &full_upf, "top");
    assert!(
        !v_full
            .iter()
            .any(|v| v.rule_id.starts_with("ISO_FEEDTHROUGH")),
        "expected both bits covered: {v_full:#?}"
    );
}

#[test]
fn r10_bug4_same_domain_hierarchical_wrapper_feedthrough() {
    for (is_buf, exp_rule) in [(false, "ISO_FEEDTHROUGH"), (true, "ISO_FEEDTHROUGH_BUFFER")] {
        let body = if is_buf {
            "buf u_buf(out_s, in_s);"
        } else {
            "assign out_s = in_s;"
        };
        let sv = format!(
            r#"
module src_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid_blk(input logic in_s, output logic out_s);
  {body}
endmodule
module wrap_blk(input logic in_w, output logic out_w);
  mid_blk u_mid(.in_s(in_w), .out_s(out_w));
endmodule
module dst_blk(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic d, output logic y);
  logic q, w;
  src_blk  u_src(.clk(clk), .d(d), .q(q));
  wrap_blk u_wrap(.in_w(q), .out_w(w));
  dst_blk  u_dst(.clk(clk), .a(w), .y(y));
endmodule
"#
        );
        let base_upf = format!(
            r#"{SUPPLY_HDR}
create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC -elements {{u_src}}  -supply {{primary SS_SW1}}
create_power_domain PD_MID -elements {{u_wrap}} -supply {{primary SS_SW2}}
create_power_domain PD_DST -elements {{u_dst}}  -supply {{primary SS_AON}}
set_isolation iso_s -domain PD_SRC -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_m -domain PD_MID -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
"#
        );
        let viols = run_rtl_upf(&format!("bug4_{exp_rule}"), &sv, &base_upf, "top");
        assert!(
            viols.iter().any(|v| v.rule_id == exp_rule),
            "expected {exp_rule} across same-domain wrapper u_wrap -> u_mid: {viols:#?}"
        );

        let rep_upf = format!(
            "{base_upf}\nset_repeater r_wrap -domain PD_MID -elements {{u_wrap/u_mid/out_s}} -repeater_supply SS_AON\n"
        );
        let viols_rep = run_rtl_upf(&format!("bug4_rep_{exp_rule}"), &sv, &rep_upf, "top");
        assert!(
            !viols_rep
                .iter()
                .any(|v| v.rule_id.starts_with("ISO_FEEDTHROUGH")),
            "expected u_wrap/u_mid/out_s repeater to cover wrapper feedthrough: {viols_rep:#?}"
        );
    }
}

#[test]
fn r10_bug5_case_a_mid_cell_honors_repeater_strategies() {
    let sv = r#"
module src_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module dst_blk(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic d, output logic y);
  logic w;
  src_blk u_src(.clk(clk), .d(d), .q(w));
  dst_blk u_dst(.clk(clk), .a(w), .y(y));
endmodule
"#;
    let base_upf = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP -include_scope -supply {{primary SS_SW1}}
create_power_domain PD_SRC -elements {{u_src}} -supply {{primary SS_AON}}
create_power_domain PD_DST -elements {{u_dst}} -supply {{primary SS_AON}}
"#
    );

    let v_unrep = run_rtl_upf("bug5_no_rep", sv, &base_upf, "top");
    assert!(
        v_unrep.iter().any(|v| v.rule_id == "ISO_FEEDTHROUGH"),
        "expected Case A ISO_FEEDTHROUGH in switchable PD_TOP: {v_unrep:#?}"
    );

    for rep_line in [
        "set_repeater r_top -domain PD_TOP -repeater_supply SS_AON",
        "set_repeater r_top -domain PD_TOP -elements {w} -source PD_SRC -sink PD_DST -repeater_supply SS_AON",
    ] {
        let upf = format!("{base_upf}\n{rep_line}\n");
        let v_ok = run_rtl_upf("bug5_ok", sv, &upf, "top");
        assert!(
            !v_ok
                .iter()
                .any(|v| v.rule_id.starts_with("ISO_FEEDTHROUGH")),
            "expected Case A repeater `{rep_line}` to suppress ISO_FEEDTHROUGH: {v_ok:#?}"
        );
    }

    // Negative controls: switchable repeater supply or `-exclude_elements {w}` must still fire.
    // Also `-applies_to outputs -elements {u_dst/a}` (downstream dc input pin) must NOT suppress Case A,
    // whereas `-applies_to both -elements {u_dst/a}` DOES suppress it.
    for bad_line in [
        "set_repeater r_bad_sup -domain PD_TOP -repeater_supply SS_SW1",
        "set_repeater r_bad_excl -domain PD_TOP -elements {.} -exclude_elements {w} -repeater_supply SS_AON",
        "set_repeater r_bad_out -domain PD_TOP -applies_to outputs -elements {u_dst/a} -repeater_supply SS_AON",
    ] {
        let upf = format!("{base_upf}\n{bad_line}\n");
        let v_bad = run_rtl_upf("bug5_bad", sv, &upf, "top");
        assert!(
            v_bad.iter().any(|v| v.rule_id == "ISO_FEEDTHROUGH"),
            "expected Case A `{bad_line}` to still fire ISO_FEEDTHROUGH: {v_bad:#?}"
        );
    }
    let upf_case_a_both = format!(
        "{base_upf}\nset_repeater r_both -domain PD_TOP -applies_to both -elements {{u_dst/a}} -repeater_supply SS_AON\n"
    );
    let v_both = run_rtl_upf("bug5_both", sv, &upf_case_a_both, "top");
    assert!(
        !v_both
            .iter()
            .any(|v| v.rule_id.starts_with("ISO_FEEDTHROUGH")),
        "expected Case A -applies_to both -elements {{u_dst/a}} to suppress ISO_FEEDTHROUGH: {v_both:#?}"
    );
}

#[test]
fn r10_bug6_per_dc_reaching_pins_and_nets_in_repeater_covers_dc() {
    let sv = r#"
module src_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid_blk(input logic in_s, output logic out1, output logic out2);
  wire w1, w2;
  assign w1 = in_s;
  assign w2 = in_s;
  buf u_b1(out1, w1);
  buf u_b2(out2, w2);
endmodule
module dst_blk(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic d, output logic y1, output logic y2);
  logic q, o1, o2;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  mid_blk u_mid(.in_s(q), .out1(o1), .out2(o2));
  dst_blk u_dst1(.clk(clk), .a(o1), .y(y1));
  dst_blk u_dst2(.clk(clk), .a(o2), .y(y2));
endmodule
"#;
    let base_upf = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP  -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC  -elements {{u_src}}  -supply {{primary SS_SW1}}
create_power_domain PD_MID  -elements {{u_mid}}  -supply {{primary SS_SW2}}
create_power_domain PD_DST1 -elements {{u_dst1}} -supply {{primary SS_AON}}
create_power_domain PD_DST2 -elements {{u_dst2}} -supply {{primary SS_AON}}
set_isolation iso_s -domain PD_SRC -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_m -domain PD_MID -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
"#
    );

    // 1. Covering ONLY `u_mid/out1` (or `u_mid/w1`) must STILL report `ISO_FEEDTHROUGH_BUFFER` on `out2 -> u_dst2`.
    for only_branch1 in [
        "set_repeater r1 -domain PD_MID -elements {u_mid/out1} -repeater_supply SS_AON",
        "set_repeater r1 -domain PD_MID -elements {u_mid/w1} -repeater_supply SS_AON",
    ] {
        let v1 = run_rtl_upf(
            "bug6_only1",
            sv,
            &format!("{base_upf}\n{only_branch1}\n"),
            "top",
        );
        assert!(
            v1.iter().any(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER"),
            "unsound FN for `{only_branch1}`: uncovered out2 -> u_dst2 must fire: {v1:#?}"
        );
    }

    // 2. Covering `out1` (or broad `.` excluding `out2`) + covering `out2` -> clean!
    for both in [
        "set_repeater r1 -domain PD_MID -elements {u_mid/out1} -repeater_supply SS_AON\nset_repeater r2 -domain PD_MID -elements {u_mid/out2} -repeater_supply SS_AON",
        "set_repeater r1 -domain PD_MID -elements {.} -exclude_elements {u_mid/out2} -repeater_supply SS_AON\nset_repeater r2 -domain PD_MID -elements {u_mid/out2} -repeater_supply SS_AON",
    ] {
        let v2 = run_rtl_upf("bug6_both", sv, &format!("{base_upf}\n{both}\n"), "top");
        assert!(
            !v2.iter().any(|v| v.rule_id.starts_with("ISO_FEEDTHROUGH")),
            "expected both branches covered for `{both}`: {v2:#?}"
        );
    }
}

#[test]
fn r10_edge_cases_parent_alias_dual_consumer_and_unsplit_bus_per_bit_sinks() {
    // 1. Parent pass-through `assign m_alias = m;` driving BOTH `u_dst` AND a flop in `top` (`PD_TOP`):
    // `-sink PD_DST` alone must still fire `ISO_FEEDTHROUGH` because `top` also consumes `m_alias`.
    let sv_dual = r#"
module src_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid_blk(input logic in_s, output logic out_s);
  assign out_s = in_s;
endmodule
module dst_blk(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic d, output logic y1, output logic y2);
  logic q, m, m_alias, top_q;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  mid_blk u_mid(.in_s(q), .out_s(m));
  assign m_alias = m;
  always_ff @(posedge clk) top_q <= m_alias;
  dst_blk u_dst(.clk(clk), .a(m_alias), .y(y1));
  assign y2 = top_q;
endmodule
"#;
    let upf_dual = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC -elements {{u_src}} -supply {{primary SS_SW1}}
create_power_domain PD_MID -elements {{u_mid}} -supply {{primary SS_SW2}}
create_power_domain PD_DST -elements {{u_dst}} -supply {{primary SS_AON}}
set_isolation iso_s -domain PD_SRC -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_m -domain PD_MID -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
set_repeater rep_dst -domain PD_MID -sink PD_DST -repeater_supply SS_AON
"#
    );
    let v_dual = run_rtl_upf("dual_cons", sv_dual, &upf_dual, "top");
    assert!(
        v_dual.iter().any(|v| v.rule_id == "ISO_FEEDTHROUGH"),
        "expected ISO_FEEDTHROUGH when parent flop also consumes m_alias: {v_dual:#?}"
    );

    // 2. Unsplit 2-bit `assign out_s = in_s;` in `mid_blk`, sliced at `top` (`m[0] -> u_dst1`, `m[1] -> u_dst2`):
    let sv_unsplit = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid_blk(input logic [1:0] in_s, output logic [1:0] out_s);
  assign out_s = in_s;
endmodule
module dst_blk(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic [1:0] d, output logic y1, output logic y2);
  logic [1:0] q, m;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  mid_blk u_mid(.in_s(q), .out_s(m));
  dst_blk u_dst1(.clk(clk), .a(m[0]), .y(y1));
  dst_blk u_dst2(.clk(clk), .a(m[1]), .y(y2));
endmodule
"#;
    let upf_unsplit = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP  -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC  -elements {{u_src}}  -supply {{primary SS_SW1}}
create_power_domain PD_MID  -elements {{u_mid}}  -supply {{primary SS_SW2}}
create_power_domain PD_DST1 -elements {{u_dst1}} -supply {{primary SS_AON}}
create_power_domain PD_DST2 -elements {{u_dst2}} -supply {{primary SS_AON}}
set_isolation iso_s -domain PD_SRC -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_m -domain PD_MID -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
set_repeater rep1 -domain PD_MID -elements {{u_mid/out_s[0]}} -sink PD_DST1 -repeater_supply SS_AON
set_repeater rep2 -domain PD_MID -elements {{u_mid/out_s[1]}} -sink PD_DST2 -repeater_supply SS_AON
"#
    );
    let v_unsplit = run_rtl_upf("unsplit_2b", sv_unsplit, &upf_unsplit, "top");
    assert!(
        !v_unsplit
            .iter()
            .any(|v| v.rule_id.starts_with("ISO_FEEDTHROUGH")),
        "expected unsplit out_s sliced at top to be covered per bit: {v_unsplit:#?}"
    );
}

#[test]
fn r12_bug2_bug3_bug6_bug7_parent_alias_distinct_bits_same_dc_and_wrapper_outputs() {
    // 1. Bug 2: `assign m_hi = m_root[3:2]` -> parent `PD_TOP` consumes `m_hi`,
    // `-applies_to inputs -elements {m_hi}` must match `dst_net = m_hi` with zero `ISO_MISSING`.
    let sv_b2 = r#"
module src_blk(input logic clk, input logic [3:0] d, output logic [3:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module top(input logic clk, input logic iso_en, input logic [3:0] d, output logic [1:0] y);
  logic [3:0] m_root;
  logic [1:0] m_hi;
  src_blk u_src(.clk(clk), .d(d), .q(m_root));
  assign m_hi = m_root[3:2];
  always_ff @(posedge clk) y <= m_hi;
endmodule
"#;
    let upf_b2 = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC -elements {{u_src}} -supply {{primary SS_SW1}}
set_isolation iso_hi -domain PD_TOP -applies_to inputs -elements {{m_hi}} -clamp_value 0 \
  -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location self
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
"#
    );
    let v_b2 = run_rtl_upf("b2_mhi", sv_b2, &upf_b2, "top");
    assert!(
        !v_b2.iter().any(|v| v.rule_id == "ISO_MISSING"),
        "expected no ISO_MISSING when -elements targets parent alias m_hi: {v_b2:#?}"
    );

    // 2. Bug 3: 2-bit `u_src/q[1:0]` with only bit 0 feeding through to 2 sinks (`u_dst1`, `u_dst2`)
    // and bit 1 unconnected must report `u_src/q[0]` (`width_bits == 1`), never `u_src/q[1:0]`.
    let sv_b3 = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid_blk(input logic [1:0] in_s, output logic out0);
  assign out0 = in_s[0];
endmodule
module dst_blk(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic [1:0] d, output logic y1, output logic y2);
  logic [1:0] q;
  logic o0;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  mid_blk u_mid(.in_s(q), .out0(o0));
  dst_blk u_dst1(.clk(clk), .a(o0), .y(y1));
  dst_blk u_dst2(.clk(clk), .a(o0), .y(y2));
endmodule
"#;
    let upf_b3 = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP  -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC  -elements {{u_src}}  -supply {{primary SS_SW1}}
create_power_domain PD_MID  -elements {{u_mid}}  -supply {{primary SS_SW2}}
create_power_domain PD_DST1 -elements {{u_dst1}} -supply {{primary SS_AON}}
create_power_domain PD_DST2 -elements {{u_dst2}} -supply {{primary SS_AON}}
set_isolation iso_s -domain PD_SRC -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_m -domain PD_MID -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
"#
    );
    let v_b3 = run_rtl_upf("b3_fanout", sv_b3, &upf_b3, "top");
    let ft_b3: Vec<_> = v_b3
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH")
        .collect();
    assert_eq!(ft_b3.len(), 1, "expected 1 single-bit hit: {ft_b3:#?}");
    assert_eq!(ft_b3[0].signal, "u_src/q[0]");
    assert_eq!(ft_b3[0].width_bits, 1);

    // 3. Bug 6: Two exit pins `out1` and `out2` feeding SAME `u_dst(.a(o1), .b(o2))`:
    // covering only `u_mid/out1` must still report uncovered `u_mid/out2 -> u_dst.b`
    // for both buffered and mixed (assign + buf) paths.
    let sv_b6_buf = r#"
module src_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid2_buf(input logic in_s, output logic out1, output logic out2);
  buf u_b1(out1, in_s);
  buf u_b2(out2, in_s);
endmodule
module dst2_blk(input logic clk, input logic a, input logic b, output logic y);
  always_ff @(posedge clk) y <= a ^ b;
endmodule
module top(input logic clk, input logic d, output logic y);
  logic q, o1, o2;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  mid2_buf u_mid(.in_s(q), .out1(o1), .out2(o2));
  dst2_blk u_dst(.clk(clk), .a(o1), .b(o2), .y(y));
endmodule
"#;
    let upf_b6_only1 = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC -elements {{u_src}} -supply {{primary SS_SW1}}
create_power_domain PD_MID -elements {{u_mid}} -supply {{primary SS_SW2}}
create_power_domain PD_DST -elements {{u_dst}} -supply {{primary SS_AON}}
set_isolation iso_s -domain PD_SRC -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_m -domain PD_MID -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
set_repeater r1 -domain PD_MID -applies_to outputs -elements {{u_mid/out1}} -repeater_supply SS_AON
"#
    );
    let v_b6_buf = run_rtl_upf("b6_samedc_buf", sv_b6_buf, &upf_b6_only1, "top");
    assert!(
        v_b6_buf
            .iter()
            .any(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER"),
        "expected ISO_FEEDTHROUGH_BUFFER for uncovered out2 -> u_dst.b: {v_b6_buf:#?}"
    );

    let sv_b6_mix = r#"
module src_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid2_mix(input logic in_s, output logic out1, output logic out2);
  assign out1 = in_s;
  buf u_b2(out2, in_s);
endmodule
module dst2_blk(input logic clk, input logic a, input logic b, output logic y);
  always_ff @(posedge clk) y <= a ^ b;
endmodule
module top(input logic clk, input logic d, output logic y);
  logic q, o1, o2;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  mid2_mix u_mid(.in_s(q), .out1(o1), .out2(o2));
  dst2_blk u_dst(.clk(clk), .a(o1), .b(o2), .y(y));
endmodule
"#;
    let v_b6_mix1 = run_rtl_upf("b6_mix1", sv_b6_mix, &upf_b6_only1, "top");
    assert!(
        v_b6_mix1
            .iter()
            .any(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER"),
        "expected ISO_FEEDTHROUGH_BUFFER on out2 when only out1 covered: {v_b6_mix1:#?}"
    );
    let upf_b6_only2 = upf_b6_only1.replace("u_mid/out1", "u_mid/out2");
    let v_b6_mix2 = run_rtl_upf("b6_mix2", sv_b6_mix, &upf_b6_only2, "top");
    assert!(
        v_b6_mix2.iter().any(|v| v.rule_id == "ISO_FEEDTHROUGH"),
        "expected ISO_FEEDTHROUGH on out1 when only out2 covered: {v_b6_mix2:#?}"
    );

    // 4. Bug 7: `set_repeater -applies_to outputs -elements {u_wrap/u_mid/out_s}` suppresses feedthrough.
    let sv_b7 = r#"
module src_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_mid(input logic in_s, output logic out_s);
  buf u_b(out_s, in_s);
endmodule
module wrap_blk(input logic in_w, output logic out_w);
  inner_mid u_mid(.in_s(in_w), .out_s(out_w));
endmodule
module dst_blk(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic d, output logic y);
  logic q, w;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  wrap_blk u_wrap(.in_w(q), .out_w(w));
  dst_blk u_dst(.clk(clk), .a(w), .y(y));
endmodule
"#;
    let upf_b7 = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC -elements {{u_src}} -supply {{primary SS_SW1}}
create_power_domain PD_MID -elements {{u_wrap}} -supply {{primary SS_SW2}}
create_power_domain PD_DST -elements {{u_dst}} -supply {{primary SS_AON}}
set_isolation iso_s -domain PD_SRC -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_m -domain PD_MID -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
set_repeater rep_in -domain PD_MID -applies_to outputs -elements {{u_wrap/u_mid/out_s}} -repeater_supply SS_AON
"#
    );
    let v_b7 = run_rtl_upf("b7_wrap_out", sv_b7, &upf_b7, "top");
    assert!(
        !v_b7.iter().any(|v| v.rule_id.starts_with("ISO_FEEDTHROUGH")),
        "expected -applies_to outputs -elements {{u_wrap/u_mid/out_s}} to suppress feedthrough: {v_b7:#?}"
    );
}

#[test]
fn r13_bug3_downstream_dc_input_pin_never_matches_applies_to_outputs_repeater() {
    let sv = r#"
module src_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_mid(input logic in_s, output logic out_s);
  buf u_b(out_s, in_s);
endmodule
module inner_dst(input logic clk, input logic in_d, output logic y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module wrap_blk(input logic clk, input logic in_w, output logic y);
  logic mid_o;
  inner_mid u_mid(.in_s(in_w), .out_s(mid_o));
  inner_dst u_dst(.clk(clk), .in_d(mid_o), .y(y));
endmodule
module top(input logic clk, input logic d, output logic y);
  logic q;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  wrap_blk u_wrap(.clk(clk), .in_w(q), .y(y));
endmodule
"#;
    let base_upf = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC -elements {{u_src}} -supply {{primary SS_SW1}}
create_power_domain PD_MID -elements {{u_wrap}} -exclude_elements {{u_wrap/u_dst}} -supply {{primary SS_SW2}}
create_power_domain PD_DST -elements {{u_wrap/u_dst}} -supply {{primary SS_AON}}
set_isolation iso_s -domain PD_SRC -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_m -domain PD_MID -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
"#
    );

    // 1. `-applies_to outputs -elements {u_wrap/u_dst/in_d}` targets downstream PD_DST's input pin,
    // NOT a PD_MID output pin, so `ISO_FEEDTHROUGH_BUFFER` on `u_src/q` MUST still fire.
    let upf_out_dst_in = format!(
        "{base_upf}\nset_repeater r_bad_out -domain PD_MID -applies_to outputs -elements {{u_wrap/u_dst/in_d}} -repeater_supply SS_AON\n"
    );
    let v_out = run_rtl_upf("b3_out_dst_in", sv, &upf_out_dst_in, "top");
    assert!(
        v_out
            .iter()
            .any(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER" && v.signal == "u_src/q"),
        "-applies_to outputs on downstream dc input pin u_wrap/u_dst/in_d must NOT suppress feedthrough: {v_out:#?}"
    );

    // 2. `-applies_to inputs -elements {u_wrap/u_dst/in_d}` must NOT suppress either.
    let upf_in_dst_in = format!(
        "{base_upf}\nset_repeater r_bad_in -domain PD_MID -applies_to inputs -elements {{u_wrap/u_dst/in_d}} -repeater_supply SS_AON\n"
    );
    let v_in = run_rtl_upf("b3_in_dst_in", sv, &upf_in_dst_in, "top");
    assert!(
        v_in.iter()
            .any(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER" && v.signal == "u_src/q"),
        "-applies_to inputs on u_wrap/u_dst/in_d must NOT suppress feedthrough: {v_in:#?}"
    );

    // 3. `-applies_to both -elements {u_wrap/u_dst/in_d}` and `-applies_to outputs -elements {u_wrap/u_mid/out_s}`
    // preserve their covering behavior on `u_src/q`.
    for rep in [
        "set_repeater r_both -domain PD_MID -applies_to both -elements {u_wrap/u_dst/in_d} -repeater_supply SS_AON",
        "set_repeater r_mid_out -domain PD_MID -applies_to outputs -elements {u_wrap/u_mid/out_s} -repeater_supply SS_AON",
    ] {
        let upf_ok = format!("{base_upf}\n{rep}\n");
        let v_ok = run_rtl_upf("b3_ok", sv, &upf_ok, "top");
        assert!(
            !v_ok
                .iter()
                .any(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER" && v.signal == "u_src/q"),
            "expected `{rep}` to suppress ISO_FEEDTHROUGH_BUFFER on u_src/q: {v_ok:#?}"
        );
    }
}

#[test]
fn r14_bug_b_per_bit_buf_applies_to_both_dst_pin_slice() {
    let sv = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_mid(input logic [1:0] in_s, output logic [1:0] out_s);
  buf u_b0(out_s[0], in_s[0]);
  buf u_b1(out_s[1], in_s[1]);
endmodule
module inner_dst(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module wrap_blk(input logic clk, input logic [1:0] in_w, output logic [1:0] y);
  logic [1:0] mid_o;
  inner_mid u_mid(.in_s(in_w), .out_s(mid_o));
  inner_dst u_dst(.clk(clk), .in_d(mid_o), .y(y));
endmodule
module top(input logic clk, input logic [1:0] d, output logic [1:0] y);
  logic [1:0] q;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  wrap_blk u_wrap(.clk(clk), .in_w(q), .y(y));
endmodule
"#;
    let upf = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC -elements {{u_src}} -supply {{primary SS_SW1}}
create_power_domain PD_MID -elements {{u_wrap}} -exclude_elements {{u_wrap/u_dst}} -supply {{primary SS_SW2}}
create_power_domain PD_DST -elements {{u_wrap/u_dst}} -supply {{primary SS_AON}}
set_isolation iso_s -domain PD_SRC -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_m -domain PD_MID -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
set_repeater r0 -domain PD_MID -applies_to both -elements {{u_wrap/u_dst/in_d[0]}} -repeater_supply SS_AON
"#
    );
    let viols = run_rtl_upf("r14_b_buf_slice", sv, &upf, "top");
    let fts: Vec<_> = viols
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER")
        .collect();
    assert_eq!(fts.len(), 1, "expected only bit [1] uncovered: {viols:#?}");
    assert_eq!(fts[0].signal, "u_src/q[1]");
    assert_eq!(fts[0].width_bits, 1);
}
