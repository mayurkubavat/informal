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

fn run_rtl_upf(tag: &str, sv: &str, upf: &str) -> Vec<LpViolation> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = TempDir(std::env::temp_dir().join(format!(
        "informal_lp_ft_ca_{}_{n}_{tag}",
        std::process::id()
    )));
    std::fs::create_dir_all(&dir.0).expect("mkdir");
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
            stage: LpStage::Rtl,
            ..Default::default()
        },
    )
    .expect("check_lp");
    s.lp.violations
}

#[test]
fn r15_comb_port_expr_bit_mixing_is_not_suppressed_by_single_bit_repeater() {
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
  inner_dst u_dst(.clk(clk), .in_d(mid_o + 2'b01), .y(y));
endmodule
module top(input logic clk, input logic [1:0] d, output logic [1:0] y);
  logic [1:0] q;
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

    let upf_bit0 = format!(
        "{base_upf}\nset_repeater r0 -domain PD_MID -applies_to both -elements {{u_wrap/u_dst/in_d[0]}} -repeater_supply SS_AON\n"
    );
    let v_bit0 = run_rtl_upf("comb_bit0", sv, &upf_bit0);
    let fts0: Vec<_> = v_bit0
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER")
        .collect();
    assert!(
        fts0.iter().any(|v| v.signal.contains("u_src/q")
            && (v.signal == "u_src/q[1:0]" || v.signal == "u_src/q[0]")),
        "single-bit repeater on bit-mixing Comb in_d[0] must NOT suppress u_src/q[0]: {v_bit0:#?}"
    );
    assert!(
        fts0.iter()
            .any(|v| v.signal == "u_src/q[1:0]" || v.signal == "u_src/q[1]"),
        "expected u_src/q[1] also uncovered: {v_bit0:#?}"
    );

    let upf_full = format!(
        "{base_upf}\nset_repeater r_all -domain PD_MID -applies_to both -elements {{u_wrap/u_dst/in_d[1:0]}} -repeater_supply SS_AON\n"
    );
    let v_full = run_rtl_upf("comb_full", sv, &upf_full);
    assert!(
        !v_full
            .iter()
            .any(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER" && v.signal.starts_with("u_src/q")),
        "full-bus repeater on in_d[1:0] should cover Comb crossing on u_src/q: {v_full:#?}"
    );
}

#[test]
fn r15_case_a_per_bit_and_swapped_mid_nets_only_cover_matched_lane() {
    let sv_per_bit = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module dst_blk(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module top(input logic clk, input logic [1:0] d, output logic [1:0] y);
  logic [1:0] wq, mid_wire;
  src_blk u_src(.clk(clk), .d(d), .q(wq));
  assign mid_wire[0] = wq[0];
  assign mid_wire[1] = wq[1];
  dst_blk u_dst(.clk(clk), .in_d(mid_wire), .y(y));
endmodule
"#;
    let base_upf = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP -include_scope -supply {{primary SS_SW2}}
create_power_domain PD_SRC -elements {{u_src}} -supply {{primary SS_AON}}
create_power_domain PD_DST -elements {{u_dst}} -supply {{primary SS_AON}}
set_retention r_top -domain PD_TOP -retention_supply_set SS_AON -save_signal {{clk high}}
"#
    );

    // 1. Per-bit Case A with repeater on mid_wire[0] -> only u_src/q[1] fires `ISO_FEEDTHROUGH`.
    let upf_mid0 = format!(
        "{base_upf}\nset_repeater r0 -domain PD_TOP -applies_to both -elements {{mid_wire[0]}} -repeater_supply SS_AON\n"
    );
    let v_pb = run_rtl_upf("ca_pb_0", sv_per_bit, &upf_mid0);
    let fts_pb: Vec<_> = v_pb
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH")
        .collect();
    assert_eq!(fts_pb.len(), 1, "expected only u_src/q[1] open: {v_pb:#?}");
    assert_eq!(fts_pb[0].signal, "u_src/q[1]");
    assert_eq!(fts_pb[0].width_bits, 1);

    // 2. Swapped Case A (`mid_wire[0] = wq[1]; mid_wire[1] = wq[0];`) with repeater on mid_wire[0] -> only u_src/q[0] fires.
    let sv_swapped = sv_per_bit
        .replace("assign mid_wire[0] = wq[0];", "assign mid_wire[0] = wq[1];")
        .replace("assign mid_wire[1] = wq[1];", "assign mid_wire[1] = wq[0];");
    let v_sw = run_rtl_upf("ca_sw_0", &sv_swapped, &upf_mid0);
    let fts_sw: Vec<_> = v_sw
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH")
        .collect();
    assert_eq!(
        fts_sw.len(),
        1,
        "expected only u_src/q[0] open on swapped mid_wire[0]: {v_sw:#?}"
    );
    assert_eq!(fts_sw[0].signal, "u_src/q[0]");
    assert_eq!(fts_sw[0].width_bits, 1);

    // 3. Swapped Case A with repeater on mid_wire[1:0] -> 0 `ISO_FEEDTHROUGH`.
    let upf_mid10 = format!(
        "{base_upf}\nset_repeater r_all -domain PD_TOP -applies_to both -elements {{mid_wire[1:0]}} -repeater_supply SS_AON\n"
    );
    let v_sw_all = run_rtl_upf("ca_sw_all", &sv_swapped, &upf_mid10);
    assert!(
        !v_sw_all.iter().any(|v| v.rule_id == "ISO_FEEDTHROUGH"),
        "expected mid_wire[1:0] to cover swapped Case A: {v_sw_all:#?}"
    );

    // 4. Unsplit multi-bit (`assign mid_wire = wq;`) with repeater on mid_wire[0] -> only u_src/q[1] fires.
    let sv_unsplit = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module dst_blk(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module top(input logic clk, input logic [1:0] d, output logic [1:0] y);
  logic [1:0] wq, mid_wire;
  src_blk u_src(.clk(clk), .d(d), .q(wq));
  assign mid_wire = wq;
  dst_blk u_dst(.clk(clk), .in_d(mid_wire), .y(y));
endmodule
"#;
    let v_un = run_rtl_upf("ca_un_0", sv_unsplit, &upf_mid0);
    let fts_un: Vec<_> = v_un
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH")
        .collect();
    assert_eq!(fts_un.len(), 1, "expected only u_src/q[1] open: {v_un:#?}");
    assert_eq!(fts_un[0].signal, "u_src/q[1]");
    assert_eq!(fts_un[0].width_bits, 1);

    // 5. Partial intermediate net (`mid_wire[0] = wq[0]` while `wq[1]` bypasses `mid_wire`):
    // whole-net `-elements {mid_wire}` covers bit [0] and leaves u_src/q[1] open.
    let sv_partial = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module dst_blk(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module top(input logic clk, input logic [1:0] d, output logic [1:0] y);
  logic [1:0] wq;
  logic mid_w0;
  src_blk u_src(.clk(clk), .d(d), .q(wq));
  assign mid_w0 = wq[0];
  dst_blk u_dst(.clk(clk), .in_d({wq[1], mid_w0}), .y(y));
endmodule
"#;
    let upf_partial = format!(
        "{base_upf}\nset_repeater r_w0 -domain PD_TOP -applies_to both -elements {{mid_w0}} -repeater_supply SS_AON\n"
    );
    let v_part = run_rtl_upf("ca_partial", sv_partial, &upf_partial);
    let fts_part: Vec<_> = v_part
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH")
        .collect();
    assert_eq!(
        fts_part.len(),
        1,
        "expected only u_src/q[1] open on partial mid_w0: {v_part:#?}"
    );
    assert_eq!(fts_part[0].signal, "u_src/q[1]");
    assert_eq!(fts_part[0].width_bits, 1);
}

#[test]
fn r16_unsplit_comb_and_direct_pin_expr_non_projection_vs_pure_wire_slice() {
    let sv_assign_comb = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_dst(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module wrap_blk(input logic clk, input logic [1:0] in_w, output logic [1:0] y);
  logic [1:0] tmp;
  assign tmp = in_w + 2'b01;
  inner_dst u_dst(.clk(clk), .in_d(tmp), .y(y));
endmodule
module top(input logic clk, input logic [1:0] d, output logic [1:0] y);
  logic [1:0] q;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  wrap_blk u_wrap(.clk(clk), .in_w(q), .y(y));
endmodule
"#;
    let sv_direct_comb = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_dst(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module wrap_blk(input logic clk, input logic [1:0] in_w, output logic [1:0] y);
  inner_dst u_dst(.clk(clk), .in_d(in_w + 2'b01), .y(y));
endmodule
module top(input logic clk, input logic [1:0] d, output logic [1:0] y);
  logic [1:0] q;
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
    let upf_bit0 = format!(
        "{base_upf}\nset_repeater r0 -domain PD_MID -applies_to both -elements {{u_wrap/u_dst/in_d[0]}} -repeater_supply SS_AON\n"
    );
    let upf_full = format!(
        "{base_upf}\nset_repeater r_all -domain PD_MID -applies_to both -elements {{u_wrap/u_dst/in_d[1:0]}} -repeater_supply SS_AON\n"
    );

    for (tag, sv) in [
        ("r16_assign_comb", sv_assign_comb),
        ("r16_direct_comb", sv_direct_comb),
    ] {
        let v_bit0 = run_rtl_upf(tag, sv, &upf_bit0);
        let fts: Vec<_> = v_bit0
            .iter()
            .filter(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER")
            .collect();
        assert!(
            !v_bit0
                .iter()
                .any(|v| v.rule_id == "ISO_FEEDTHROUGH" && v.signal.starts_with("u_src/q")),
            "{tag}: must classify u_src/q as ISO_FEEDTHROUGH_BUFFER, not ISO_FEEDTHROUGH: {v_bit0:#?}"
        );
        assert!(
            fts.iter()
                .any(|v| v.signal == "u_src/q[1:0]" && v.width_bits == 2),
            "{tag}: single-bit in_d[0] repeater must NOT suppress u_src/q[0] on unsplit Comb: {v_bit0:#?}"
        );

        let v_full = run_rtl_upf(&format!("{tag}_full"), sv, &upf_full);
        assert!(
            !v_full.iter().any(|v| {
                matches!(
                    v.rule_id.as_str(),
                    "ISO_FEEDTHROUGH" | "ISO_FEEDTHROUGH_BUFFER"
                ) && v.signal.starts_with("u_src/q")
            }),
            "{tag}: full-bus in_d[1:0] repeater must suppress cleanly: {v_full:#?}"
        );
    }

    // Input-side repeater on `u_wrap/in_w[0]` (pre-Comb) covers bit 0 and flags bit 1 only.
    let upf_in0 = format!(
        "{base_upf}\nset_repeater r_in0 -domain PD_MID -applies_to both -elements {{u_wrap/in_w[0]}} -repeater_supply SS_AON\n"
    );
    let v_in0 = run_rtl_upf("r16_in0", sv_assign_comb, &upf_in0);
    let fts_in0: Vec<_> = v_in0
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER")
        .collect();
    assert_eq!(
        fts_in0.len(),
        1,
        "expected only u_src/q[1] open: {v_in0:#?}"
    );
    assert_eq!(fts_in0[0].signal, "u_src/q[1]");
    assert_eq!(fts_in0[0].width_bits, 1);

    // PureWire (`assign tmp = in_w;`) and boundary-inverted PureWire (`.in_w(~q)`) still slice at bit level.
    let sv_pure_wire = sv_assign_comb.replace("in_w + 2'b01", "in_w");
    let sv_bnd_inv = sv_pure_wire.replace(".in_w(q)", ".in_w(~q)");
    for (tag, sv) in [
        ("r16_pw", sv_pure_wire.as_str()),
        ("r16_pw_inv", sv_bnd_inv.as_str()),
    ] {
        let v_pw = run_rtl_upf(tag, sv, &upf_bit0);
        let fts_pw: Vec<_> = v_pw
            .iter()
            .filter(|v| v.rule_id == "ISO_FEEDTHROUGH" && v.signal.starts_with("u_src/q"))
            .collect();
        assert_eq!(
            fts_pw.len(),
            1,
            "{tag}: expected only bit 1 open: {v_pw:#?}"
        );
        assert_eq!(fts_pw[0].signal, "u_src/q[1]");
        assert_eq!(fts_pw[0].width_bits, 1);
    }
}

#[test]
fn r17_per_bit_iso_cuts_preserve_comb_non_slice_and_input_exclude_slice() {
    let sv = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_dst(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module wrap_blk(input logic clk, input logic [1:0] in_w, output logic [1:0] y);
  logic [1:0] tmp;
  assign tmp = in_w + 2'b01;
  inner_dst u_dst(.clk(clk), .in_d(tmp), .y(y));
endmodule
module top(input logic clk, input logic [1:0] d, output logic [1:0] y);
  logic [1:0] q;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  wrap_blk u_wrap(.clk(clk), .in_w(q), .y(y));
endmodule
"#;
    let per_bit_iso_hdr = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC -elements {{u_src}} -supply {{primary SS_SW1}}
create_power_domain PD_MID -elements {{u_wrap}} -exclude_elements {{u_wrap/u_dst}} -supply {{primary SS_SW2}}
create_power_domain PD_DST -elements {{u_wrap/u_dst}} -supply {{primary SS_AON}}
set_isolation iso_s0 -domain PD_SRC -applies_to outputs -elements {{u_src/q[0]}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_s1 -domain PD_SRC -applies_to outputs -elements {{u_src/q[1]}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_m -domain PD_MID -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
"#
    );

    // 1. Per-bit iso_cuts + Combinational feedthrough + downstream single-bit in_d[0] repeater:
    // must NOT suppress u_src/q[0] via retain_sub_ft_routes narrowing.
    let upf_iso_comb_d0 = format!(
        "{per_bit_iso_hdr}\nset_repeater r0 -domain PD_MID -applies_to both -elements {{u_wrap/u_dst/in_d[0]}} -repeater_supply SS_AON\n"
    );
    let v_comb_d0 = run_rtl_upf("r17_iso_cuts_comb_d0", sv, &upf_iso_comb_d0);
    let fts_d0: Vec<_> = v_comb_d0
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER" && v.signal.starts_with("u_src/q"))
        .collect();
    assert!(
        fts_d0.iter().any(|v| v.signal == "u_src/q[0]"),
        "per-bit iso_cuts must not suppress u_src/q[0] on Comb route: {v_comb_d0:#?}"
    );
    assert!(
        fts_d0.iter().any(|v| v.signal == "u_src/q[1]"),
        "expected u_src/q[1] also uncovered: {v_comb_d0:#?}"
    );

    // 2. Input-side `-elements {u_wrap/in_w[1:0]} -exclude_elements {u_wrap/in_w[0]}`
    // must report ONLY u_src/q[0] (1 bit), both without and with per-bit iso_cuts.
    let dom_iso_hdr = format!(
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
    for (tag, hdr) in [
        ("r17_in_excl0_dom", dom_iso_hdr.as_str()),
        ("r17_in_excl0_cuts", per_bit_iso_hdr.as_str()),
    ] {
        let upf_excl = format!(
            "{hdr}\nset_repeater r_in -domain PD_MID -applies_to both -elements {{u_wrap/in_w[1:0]}} -exclude_elements {{u_wrap/in_w[0]}} -repeater_supply SS_AON\n"
        );
        let v_excl = run_rtl_upf(tag, sv, &upf_excl);
        let fts_excl: Vec<_> = v_excl
            .iter()
            .filter(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER" && v.signal.starts_with("u_src/q"))
            .collect();
        assert_eq!(
            fts_excl.len(),
            1,
            "{tag}: expected only u_src/q[0] (1 bit), not over-excluding bit 1: {v_excl:#?}"
        );
        assert_eq!(fts_excl[0].signal, "u_src/q[0]");
        assert_eq!(fts_excl[0].width_bits, 1);
    }
}

#[test]
fn r17_mux_bitand_bitor_redand_emit_iso_feedthrough_buffer_only() {
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
    let upf_d0 = format!(
        "{base_upf}\nset_repeater r0 -domain PD_MID -applies_to both -elements {{u_wrap/u_dst/in_d[0]}} -repeater_supply SS_AON\n"
    );

    let wrap_2b = |body: &str| {
        format!(
            r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_dst(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module wrap_blk(input logic clk, input logic sel, input logic [1:0] mask, input logic [1:0] in_b, input logic [1:0] in_w, output logic [1:0] y);
{body}
endmodule
module top(input logic clk, input logic sel, input logic [1:0] d, input logic [1:0] mask, input logic [1:0] in_b, output logic [1:0] y);
  logic [1:0] q;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  wrap_blk u_wrap(.clk(clk), .sel(sel), .mask(mask), .in_b(in_b), .in_w(q), .y(y));
endmodule
"#
        )
    };
    let sv_redand = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_dst(input logic clk, input logic in_d, output logic y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module wrap_blk(input logic clk, input logic [1:0] in_w, output logic y);
  logic tmp;
  assign tmp = &in_w;
  inner_dst u_dst(.clk(clk), .in_d(tmp), .y(y));
endmodule
module top(input logic clk, input logic [1:0] d, output logic y);
  logic [1:0] q;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  wrap_blk u_wrap(.clk(clk), .in_w(q), .y(y));
endmodule
"#;

    let cases: Vec<(&str, String, &str)> = vec![
        (
            "mux_swap_no_rep",
            wrap_2b("  inner_dst u_dst(.clk(clk), .in_d(sel ? {in_w[0], in_w[1]} : in_w), .y(y));"),
            &base_upf,
        ),
        (
            "mux_swap_d0",
            wrap_2b("  inner_dst u_dst(.clk(clk), .in_d(sel ? {in_w[0], in_w[1]} : in_w), .y(y));"),
            &upf_d0,
        ),
        (
            "mux_const_dir",
            wrap_2b("  inner_dst u_dst(.clk(clk), .in_d(sel ? in_w : 2'b00), .y(y));"),
            &base_upf,
        ),
        (
            "mux_const_asg",
            wrap_2b("  logic [1:0] tmp;\n  assign tmp = sel ? in_w : 2'b00;\n  inner_dst u_dst(.clk(clk), .in_d(tmp), .y(y));"),
            &base_upf,
        ),
        (
            "mux_2data",
            wrap_2b("  inner_dst u_dst(.clk(clk), .in_d(sel ? in_w : in_b), .y(y));"),
            &base_upf,
        ),
        (
            "mux_self_cond_d0",
            wrap_2b("  inner_dst u_dst(.clk(clk), .in_d(in_w[0] ? 2'b10 : in_w), .y(y));"),
            &upf_d0,
        ),
        (
            "bitand_wire",
            wrap_2b("  inner_dst u_dst(.clk(clk), .in_d(in_w & mask), .y(y));"),
            &base_upf,
        ),
        (
            "bitand_const",
            wrap_2b("  logic [1:0] tmp;\n  assign tmp = in_w & 2'b01;\n  inner_dst u_dst(.clk(clk), .in_d(tmp), .y(y));"),
            &base_upf,
        ),
        (
            "bitor_const",
            wrap_2b("  logic [1:0] tmp;\n  assign tmp = in_w | 2'b10;\n  inner_dst u_dst(.clk(clk), .in_d(tmp), .y(y));"),
            &base_upf,
        ),
        ("redand_asg", sv_redand.to_string(), &base_upf),
    ];

    for (tag, sv, upf) in cases {
        let viols = run_rtl_upf(tag, &sv, upf);
        assert!(
            !viols
                .iter()
                .any(|v| v.rule_id == "ISO_FEEDTHROUGH" && v.signal.starts_with("u_src/q")),
            "{tag}: leaked spurious ISO_FEEDTHROUGH warning: {viols:#?}"
        );
        assert!(
            viols
                .iter()
                .any(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER" && v.signal.starts_with("u_src/q")),
            "{tag}: expected ISO_FEEDTHROUGH_BUFFER error on u_src/q: {viols:#?}"
        );
    }
}

#[test]
fn r18_offset_purewire_iso_cuts_nested_bitand_and_pre_comb_w_pre_exclude() {
    let iso_hdr = format!(
        r#"{SUPPLY_HDR}
create_power_domain PD_TOP -include_scope -supply {{primary SS_AON}}
create_power_domain PD_SRC -elements {{u_src}} -supply {{primary SS_SW1}}
create_power_domain PD_MID -elements {{u_wrap}} -exclude_elements {{u_wrap/u_dst}} -supply {{primary SS_SW2}}
create_power_domain PD_DST -elements {{u_wrap/u_dst}} -supply {{primary SS_AON}}
set_isolation iso_s0 -domain PD_SRC -applies_to outputs -elements {{u_src/q[0]}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_s1 -domain PD_SRC -applies_to outputs -elements {{u_src/q[1]}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_isolation iso_m -domain PD_MID -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent
set_retention r_src -domain PD_SRC -retention_supply_set SS_AON -save_signal {{clk high}}
"#
    );

    // 1. Probe 1: offset PureWire `in_w[3:2]` -> `u_dst/in_d[1:0]` + per-bit iso_cuts + `in_d[0]` repeater.
    let sv_offset = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_dst(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module wrap_blk(input logic clk, input logic [3:0] in_w, output logic [1:0] y);
  inner_dst u_dst(.clk(clk), .in_d(in_w[3:2]), .y(y));
endmodule
module top(input logic clk, input logic [1:0] d, output logic [1:0] y);
  logic [1:0] q;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  wrap_blk u_wrap(.clk(clk), .in_w({q, 2'b00}), .y(y));
endmodule
"#;
    let upf_d0 = format!(
        "{iso_hdr}\nset_repeater r0 -domain PD_MID -applies_to both -elements {{u_wrap/u_dst/in_d[0]}} -repeater_supply SS_AON\n"
    );
    let v_off = run_rtl_upf("r18_offset_pw_cuts", sv_offset, &upf_d0);
    let fts_off: Vec<_> = v_off
        .iter()
        .filter(|v| v.rule_id == "ISO_FEEDTHROUGH" && v.signal.starts_with("u_src/q"))
        .collect();
    assert_eq!(
        fts_off.len(),
        1,
        "expected only u_src/q[1] uncovered: {v_off:#?}"
    );
    assert_eq!(fts_off[0].signal, "u_src/q[1]");

    // 2. Nested bit-mixing inside BitAnd/BitOr/Mux/Shl must NOT be suppressed by `in_d[0]` repeater.
    let wrap_2b = |body: &str| {
        format!(
            r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_dst(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module wrap_blk(input logic clk, sel, input logic [1:0] mask, in_w, output logic [1:0] y);
{body}
endmodule
module top(input logic clk, sel, input logic [1:0] d, mask, output logic [1:0] y);
  logic [1:0] q;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  wrap_blk u_wrap(.clk(clk), .sel(sel), .mask(mask), .in_w(q), .y(y));
endmodule
"#
        )
    };
    for (tag, body) in [
        (
            "r18_add_and",
            "  inner_dst u_dst(.clk(clk), .in_d((in_w + 2'b01) & mask), .y(y));",
        ),
        (
            "r18_add_or",
            "  inner_dst u_dst(.clk(clk), .in_d((in_w + 2'b01) | mask), .y(y));",
        ),
        (
            "r18_shl_and",
            "  inner_dst u_dst(.clk(clk), .in_d((in_w << 1) & mask), .y(y));",
        ),
        (
            "r18_mux_and",
            "  inner_dst u_dst(.clk(clk), .in_d((sel ? in_w : 2'b00) & mask), .y(y));",
        ),
    ] {
        let sv = wrap_2b(body);
        let viols = run_rtl_upf(tag, &sv, &upf_d0);
        let fts: Vec<_> = viols
            .iter()
            .filter(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER" && v.signal.starts_with("u_src/q"))
            .collect();
        assert!(
            fts.iter().any(|v| v.signal == "u_src/q[0]"),
            "{tag}: in_d[0] repeater unsoundly suppressed u_src/q[0]: {viols:#?}"
        );
        assert!(
            fts.iter().any(|v| v.signal == "u_src/q[1]"),
            "{tag}: expected u_src/q[1] also uncovered: {viols:#?}"
        );
    }

    // 3. Pre-combinational alias `w_pre = in_w; tmp = w_pre + 2'b01;` with `-exclude_elements {u_wrap/w_pre[0]}`.
    let sv_wpre = wrap_2b(
        "  logic [1:0] w_pre, tmp;\n  assign w_pre = in_w;\n  assign tmp = w_pre + 2'b01;\n  inner_dst u_dst(.clk(clk), .in_d(tmp), .y(y));",
    );
    let dom_hdr = iso_hdr
        .replace(
            "set_isolation iso_s0 -domain PD_SRC -applies_to outputs -elements {u_src/q[0]} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent\nset_isolation iso_s1 -domain PD_SRC -applies_to outputs -elements {u_src/q[1]} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent",
            "set_isolation iso_s -domain PD_SRC -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal clk -isolation_sense high -location parent",
        );
    for (tag, hdr, elems) in [
        ("r18_wpre_in_no_cuts", dom_hdr.as_str(), "u_wrap/in_w[1:0]"),
        (
            "r18_wpre_self_no_cuts",
            dom_hdr.as_str(),
            "u_wrap/w_pre[1:0]",
        ),
        ("r18_wpre_in_cuts", iso_hdr.as_str(), "u_wrap/in_w[1:0]"),
        ("r18_wpre_self_cuts", iso_hdr.as_str(), "u_wrap/w_pre[1:0]"),
    ] {
        let upf = format!(
            "{hdr}\nset_repeater r_wpre -domain PD_MID -applies_to both -elements {{{elems}}} -exclude_elements {{u_wrap/w_pre[0]}} -repeater_supply SS_AON\n"
        );
        let viols = run_rtl_upf(tag, &sv_wpre, &upf);
        let fts: Vec<_> = viols
            .iter()
            .filter(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER" && v.signal.starts_with("u_src/q"))
            .collect();
        assert_eq!(
            fts.len(),
            1,
            "{tag}: expected only u_src/q[0] (1 bit): {viols:#?}"
        );
        assert_eq!(fts[0].signal, "u_src/q[0]");
        assert_eq!(fts[0].width_bits, 1);
    }

    // Post-combinational `-exclude_elements {u_wrap/tmp[0]}` still covers neither bit cleanly (2 bits open).
    let upf_tmp = format!(
        "{dom_hdr}\nset_repeater r_tmp -domain PD_MID -applies_to both -elements {{u_wrap/in_w[1:0]}} -exclude_elements {{u_wrap/tmp[0]}} -repeater_supply SS_AON\n"
    );
    let v_tmp = run_rtl_upf("r18_wpre_post_tmp", &sv_wpre, &upf_tmp);
    assert!(
        v_tmp.iter().any(|v| v.rule_id == "ISO_FEEDTHROUGH_BUFFER"
            && v.signal == "u_src/q[1:0]"
            && v.width_bits == 2),
        "post-comb tmp[0] exclude must not slice route: {v_tmp:#?}"
    );
}
