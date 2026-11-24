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
    let dir =
        std::env::temp_dir().join(format!("informal_lp_iso_{}_{n}_{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    TempDir(dir)
}

fn run_rtl_upf(tag: &str, sv: &str, upf: &str, top: &str) -> (Vec<LpViolation>, usize, usize) {
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
    let st = s.lp.last_stats.clone().unwrap_or_default();
    (s.lp.violations, st.raw_crossings, st.grouped_crossings)
}

#[test]
fn soc_multi_domain_clean_fixture_has_zero_violations() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let sv_path = root.join("tests/lp/soc_multi_domain/soc_top.sv");
    let upf_path = root.join("tests/lp/soc_multi_domain/soc_clean.upf");
    let dir = new_temp("soc_clean");
    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    s.design.files.push(sv_path);
    s.elaborate(Some("soc_top".to_string())).expect("elaborate");
    load_upf(&mut s, &upf_path, None, None, false).expect("load_upf");
    check_lp(&mut s, &CheckLpOpts::default()).expect("check_lp");
    assert!(s.lp.violations.is_empty(), "{:#?}", s.lp.violations);
}

#[test]
fn iso_missing_64b_bus_compression_and_escaped_ident() {
    let sv = r#"
module src(output logic [63:0] m_data, output logic \esc_sig$0 );
  assign m_data = 64'hdead_beef;
  assign \esc_sig$0  = 1'b1;
endmodule
module dst(input logic [63:0] m_data, input logic \esc_sig$0 , output logic y);
  assign y = m_data[0] ^ \esc_sig$0 ;
endmodule
module top(output logic y);
  logic [63:0] bus;
  logic esc_w;
  src u_src(.m_data({bus[63:32], bus[31:0]}), .\esc_sig$0 (esc_w));
  dst u_dst(.m_data(bus), .\esc_sig$0 (esc_w), .y(y));
endmodule
"#;
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
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_AON}
create_power_domain PD_SRC -elements {u_src} -supply {primary SS_SW}
set_isolation iso_esc -domain PD_SRC -applies_to outputs -elements {u_src/\esc_sig$0 } \
  -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal esc_w -isolation_sense high -location parent
"#;
    let (viols, raw, grouped) = run_rtl_upf("iso_64b", sv, upf, "top");
    assert!(
        raw > grouped,
        "expected slice coalescing: raw={raw}, grouped={grouped}"
    );
    let missing: Vec<_> = viols
        .iter()
        .filter(|v| v.rule_id == "ISO_MISSING")
        .collect();
    assert_eq!(missing.len(), 1, "{viols:#?}");
    assert_eq!(missing[0].width_bits, 64);
    assert_eq!(missing[0].raw_crossings, 2);
}

#[test]
fn iso_redundant_location_supply_and_clamp_rules() {
    let sv = r#"
module pmu(output logic en);
  assign en = 1'b0;
endmodule
module sw_a(output logic sig_a, output logic sig_b);
  assign sig_a = 1'b1;
  assign sig_b = 1'b0;
endmodule
module aon_b(input logic sig_a, input logic sig_b, output logic y);
  assign y = sig_a ^ sig_b;
endmodule
module top(input logic en_in, output logic y);
  logic en, sa, sb, sb_iso;
  pmu u_pmu(.en(en));
  sw_a u_sw(.sig_a(sa), .sig_b(sb));
  assign sb_iso = (~en_in) ? 1'b1 : sb;
  aon_b u_aon(.sig_a(sa), .sig_b(sb_iso), .y(y));
endmodule
"#;
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
create_power_domain PD_PMU -elements {u_pmu} -supply {primary SS_AON}
create_power_domain PD_AON -elements {u_aon} -supply {primary SS_AON}
create_power_domain PD_SW  -elements {u_sw}  -supply {primary SS_SW}

set_isolation iso_red -domain PD_AON -applies_to outputs -clamp_value 0 \
  -isolation_supply_set SS_AON -isolation_signal en_in -isolation_sense high -location parent
set_isolation iso_bad_loc_sup -domain PD_SW -applies_to outputs -elements {u_sw/sig_a} \
  -isolation_supply_set SS_SW -isolation_signal en_in -isolation_sense high -location self
set_isolation iso_bad_clamp_pol -domain PD_SW -applies_to outputs -elements {u_sw/sig_b} \
  -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal en_in -isolation_sense high -location parent
"#;
    let (viols, _, _) = run_rtl_upf("iso_rules", sv, upf, "top");
    let ids: Vec<&str> = viols.iter().map(|v| v.rule_id.as_str()).collect();
    assert!(ids.contains(&"ISO_REDUNDANT"), "{ids:?}");
    assert!(ids.contains(&"ISO_LOCATION_INVALID"), "{ids:?}");
    assert!(ids.contains(&"ISO_SUPPLY_MISMATCH"), "{ids:?}");
    assert!(ids.contains(&"ISO_CLAMP_UNSPECIFIED"), "{ids:?}");
    assert!(ids.contains(&"ISO_CTRL_POLARITY_MISMATCH"), "{ids:?}");
    assert!(ids.contains(&"ISO_CLAMP_MISMATCH"), "{ids:?}");
}

#[test]
fn iso_ctrl_unpowered_mixed_polarity_and_feedthroughs() {
    let sv = r#"
module sw_ctrl(input logic sel, input logic raw, output logic mix_en, output logic ft_wire, output logic ft_buf, input logic in_w, input logic in_b);
  assign mix_en = sel ? raw : ~raw;
  assign ft_wire = in_w;
  buf u_buf(ft_buf, in_b);
endmodule
module src(output logic a, output logic b, output logic c);
  assign a = 1'b1;
  assign b = 1'b0;
  assign c = 1'b1;
endmodule
module dst(input logic a, input logic b, input logic c, output logic y);
  assign y = a ^ b ^ c;
endmodule
module top(input logic sel, input logic raw, output logic y);
  logic mix_en, a, b, c, fw, fb;
  src u_src(.a(a), .b(b), .c(c));
  sw_ctrl u_sw(.sel(sel), .raw(raw), .mix_en(mix_en), .ft_wire(fw), .ft_buf(fb), .in_w(a), .in_b(b));
  dst u_dst(.a(fw), .b(fb), .c(c), .y(y));
endmodule
"#;
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
create_power_domain PD_SRC -elements {u_src} -supply {primary SS_AON}
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_AON}
create_power_domain PD_SW  -elements {u_sw}  -supply {primary SS_SW}
set_isolation iso_sw -domain PD_SW -applies_to outputs -clamp_value 0 \
  -isolation_supply_set SS_AON -isolation_signal u_sw/mix_en -isolation_sense high -location parent
"#;
    let (viols, _, _) = run_rtl_upf("iso_ft", sv, upf, "top");
    let ids: Vec<&str> = viols.iter().map(|v| v.rule_id.as_str()).collect();
    assert!(ids.contains(&"ISO_CTRL_UNPOWERED"), "{ids:?}");
    assert!(ids.contains(&"ISO_CTRL_MIXED_POLARITY"), "{ids:?}");
    assert!(ids.contains(&"ISO_FEEDTHROUGH"), "{ids:?}");
    assert!(ids.contains(&"ISO_FEEDTHROUGH_BUFFER"), "{ids:?}");
}

#[test]
fn regression_1c_blackbox_outgoing_crossing_and_width() {
    let sv = r#"
module dst(input logic [7:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic [7:0] a, output logic y);
  logic [7:0] w_bb;
  bb_ip u_bb(.in_bus(a), .out_bus(w_bb));
  dst u_dst(.d(w_bb), .y(y));
endmodule
"#;
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
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_AON}
create_power_domain PD_BB  -elements {u_bb}  -supply {primary SS_SW}
"#;
    let dir = new_temp("bb_stage2");
    let sv_path = dir.0.join("bb.sv");
    let upf_path = dir.0.join("bb.upf");
    std::fs::write(&sv_path, sv).unwrap();
    std::fs::write(&upf_path, upf).unwrap();
    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    s.design.bbox.push("bb_ip".to_string());
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
    let missing: Vec<_> =
        s.lp.violations
            .iter()
            .filter(|v| v.rule_id == "ISO_MISSING")
            .collect();
    assert_eq!(missing.len(), 1, "{:#?}", s.lp.violations);
    assert_eq!(missing[0].width_bits, 8);
}

#[test]
fn regression_1e_shared_root_clamp_and_distinct_root_polarity() {
    let sv = r#"
module src(output logic d1, output logic d2);
  assign d1 = 1'b1;
  assign d2 = 1'b0;
endmodule
module dst(input logic d1, input logic d2, output logic y);
  assign y = d1 ^ d2;
endmodule
module top(input logic en_a, input logic en_b, input logic pmu_en, output logic y);
  logic d1, d2, d1_iso, iso_en, iso_en_n, en_comb, en_mix;
  assign en_comb = en_a & (~en_b);
  assign en_mix  = en_a & (~en_a);
  assign iso_en   = pmu_en;
  assign iso_en_n = ~pmu_en;
  src u_src(.d1(d1), .d2(d2));
  assign d1_iso = iso_en_n ? 1'b0 : d1;
  dst u_dst(.d1(d1_iso), .d2(d2), .y(y));
endmodule
"#;
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
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_AON}
create_power_domain PD_SRC -elements {u_src} -supply {primary SS_SW}
set_isolation iso_shared -domain PD_SRC -applies_to outputs -elements {u_src/d1} \
  -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent
set_isolation iso_comb_ok -domain PD_SRC -applies_to outputs -elements {u_src/d2} \
  -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal en_comb -isolation_sense high -location parent
"#;
    let (viols, _, _) = run_rtl_upf("iso_1e", sv, upf, "top");
    let ids: Vec<&str> = viols.iter().map(|v| v.rule_id.as_str()).collect();
    assert!(ids.contains(&"ISO_CTRL_POLARITY_MISMATCH"), "{ids:?}");
    assert!(!ids.contains(&"ISO_CTRL_MIXED_POLARITY"), "{ids:?}");
}

#[test]
fn regression_1a2_slice_elements_split_and_2a_escaped_parent() {
    let sv = r#"
module src(output logic [7:0] out_bus);
  assign out_bus = 8'hff;
endmodule
module blk(output logic y);
  assign y = 1'b1;
endmodule
module dst(input logic [7:0] d, input logic y, output logic o);
  assign o = (^d) ^ y;
endmodule
module top(input logic iso_en, output logic o);
  logic [7:0] bus;
  logic w;
  src u_src(.out_bus(bus));
  blk \u_blk$1 (.y(w));
  dst u_dst(.d(bus), .y(w), .o(o));
endmodule
"#;
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
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_AON}
create_power_domain PD_SRC -elements {u_src \u_blk$1 } -supply {primary SS_SW}
set_isolation iso_lo -domain PD_SRC -applies_to outputs -elements {u_src/out_bus[3:0]} \
  -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent
set_isolation iso_esc -domain PD_SRC -applies_to outputs -elements {\u_blk$1 /y} \
  -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent
"#;
    let (viols, _, _) = run_rtl_upf("iso_1a2_2a", sv, upf, "top");
    assert!(
        !viols.iter().any(|v| v.rule_id == "UPF_UNRESOLVED_TARGET"),
        "{viols:#?}"
    );
    let missing: Vec<_> = viols
        .iter()
        .filter(|v| v.rule_id == "ISO_MISSING")
        .collect();
    assert_eq!(missing.len(), 1, "{viols:#?}");
    assert_eq!(missing[0].width_bits, 4);
    assert!(missing[0].signal.contains("[7:4]"), "{missing:#?}");
}

#[test]
fn regression_3b_inputs_self_aon_supply_and_4a_if_else_clamp_and_defects_ab() {
    let sv = r#"
module src(output logic a, output logic b, output logic m_valid, output logic m_ready);
  assign a = 1'b1;
  assign b = 1'b0;
  assign m_valid = 1'b1;
  assign m_ready = 1'b1;
endmodule
module dst(input logic a, input logic b, input logic hs, output logic y);
  assign y = a ^ b ^ hs;
endmodule
module top(input logic iso_en, output logic y);
  logic a, b, b_iso, mv, mr, hs;
  src u_src(.a(a), .b(b), .m_valid(mv), .m_ready(mr));
  always_comb begin
    if (iso_en) b_iso = 1'b1;
    else b_iso = b;
  end
  assign hs = mv & mr;
  dst u_dst(.a(a), .b(b_iso), .hs(hs), .y(y));
endmodule
"#;
    let upf = r#"
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
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_SW2}

# [3B] `-applies_to inputs -location self -isolation_supply_set SS_AON` is valid even when PD_DST is switchable.
set_isolation iso_in_aon -domain PD_DST -applies_to inputs -elements {u_dst/a} \
  -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location self

# [4A] `always_comb if/else` clamps to 1 while UPF expects 0 -> `ISO_CLAMP_MISMATCH`.
set_isolation iso_if_bad -domain PD_SRC -applies_to outputs -elements {u_src/b} \
  -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent

# Defect A: `assign hs = mv & mr;` must NOT falsely fire `ISO_CLAMP_MISMATCH` against `-clamp_value 1`.
set_isolation iso_hs_ok -domain PD_SRC -applies_to outputs -elements {u_src/m_valid} \
  -clamp_value 1 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent

# Defect B: `-no_isolation` domain default overridden by `-elements {u_src/m_ready}` (Matched wins).
set_isolation iso_no_def -domain PD_SRC -no_isolation
set_isolation iso_mr_yes -domain PD_SRC -applies_to outputs -elements {u_src/m_ready} \
  -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent
"#;
    let (viols, _, _) = run_rtl_upf("iso_3b_4a", sv, upf, "top");
    assert!(
        !viols.iter().any(|v| v.rule_id == "ISO_LOCATION_INVALID"),
        "{viols:#?}"
    );
    let clamp_bad: Vec<_> = viols
        .iter()
        .filter(|v| v.rule_id == "ISO_CLAMP_MISMATCH")
        .collect();
    assert_eq!(clamp_bad.len(), 1, "{viols:#?}");
    assert!(clamp_bad[0].signal.contains("b"), "{clamp_bad:#?}");
    assert!(
        !viols
            .iter()
            .any(|v| v.rule_id == "ISO_MISSING" && v.signal.contains("m_ready")),
        "{viols:#?}"
    );
}

#[test]
fn regression_1a2_discontig_unsound_and_discontig_slice_split() {
    let sv = r#"
module src(output logic [7:0] out_bus);
  assign out_bus = 8'h5a;
endmodule
module dst(input logic [3:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic iso_en, output logic y);
  logic [7:0] bus;
  src u_src(.out_bus(bus));
  dst u_dst(.d({bus[7:6], bus[1:0]}), .y(y));
endmodule
"#;
    let upf_lo_iso = r#"
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
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_AON}
create_power_domain PD_SRC -elements {u_src} -supply {primary SS_SW}
set_isolation iso_lo -domain PD_SRC -applies_to outputs -elements {u_src/out_bus[1:0]} \
  -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent
"#;
    let (viols_lo, _, _) = run_rtl_upf("discontig_unsound", sv, upf_lo_iso, "top");
    let miss_lo: Vec<_> = viols_lo
        .iter()
        .filter(|v| v.rule_id == "ISO_MISSING")
        .collect();
    assert_eq!(miss_lo.len(), 1, "{viols_lo:#?}");
    assert_eq!(miss_lo[0].width_bits, 2);
    assert!(miss_lo[0].signal.contains("[7:6]"), "{miss_lo:#?}");

    let upf_hi_iso = r#"
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
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_AON}
create_power_domain PD_SRC -elements {u_src} -supply {primary SS_SW}
set_isolation iso_hi -domain PD_SRC -applies_to outputs -elements {u_src/out_bus[7:6]} \
  -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent
"#;
    let (viols_hi, _, _) = run_rtl_upf("discontig_hi", sv, upf_hi_iso, "top");
    let miss_hi: Vec<_> = viols_hi
        .iter()
        .filter(|v| v.rule_id == "ISO_MISSING")
        .collect();
    assert_eq!(miss_hi.len(), 1, "{viols_hi:#?}");
    assert_eq!(miss_hi[0].width_bits, 2);
    assert!(miss_hi[0].signal.contains("[1:0]"), "{miss_hi:#?}");
}

#[test]
fn regression_p_hier_plain_uniso_and_escaped_transitive_inheritance() {
    let sv = r#"
module leaf(input logic clk, input logic d, output logic y);
  always_ff @(posedge clk) y <= d;
endmodule
module mid(input logic clk, input logic d, output logic y2);
  leaf u_leaf(.clk(clk), .d(d), .y(y2));
endmodule
module esc_mid(input logic clk, input logic d, output logic y_esc);
  leaf \u_leaf$0 (.clk(clk), .d(d), .y(y_esc));
endmodule
module dst(input logic y2, input logic y_esc, output logic o);
  assign o = y2 ^ y_esc;
endmodule
module top(input logic clk, input logic d, input logic iso_en, output logic o);
  logic y2, y_esc;
  mid     u_mid(.clk(clk), .d(d), .y2(y2));
  esc_mid \u_mid$0 (.clk(clk), .d(d), .y_esc(y_esc));
  dst     u_dst(.y2(y2), .y_esc(y_esc), .o(o));
endmodule
"#;
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
create_power_domain PD_SRC -elements {u_mid \u_mid$0 } -supply {primary SS_SW}
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_AON}
set_isolation iso_esc -domain PD_SRC -applies_to outputs -elements {\u_mid$0 /y_esc} \
  -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent
"#;
    let (viols, _, _) = run_rtl_upf("p_hier_uniso", sv, upf, "top");
    // Child instances `u_mid/u_leaf` and `u_mid$0/u_leaf$0` inherit `PD_SRC`, so there must be
    // ZERO false `ISO_FEEDTHROUGH` or `PD_SRC -> PD_TOP` violations on `u_mid/clk` or `u_mid/d`.
    assert!(
        !viols.iter().any(|v| v.rule_id == "ISO_FEEDTHROUGH"),
        "unexpected ISO_FEEDTHROUGH: {viols:#?}"
    );
    let miss: Vec<_> = viols
        .iter()
        .filter(|v| v.rule_id == "ISO_MISSING")
        .collect();
    assert_eq!(miss.len(), 1, "{viols:#?}");
    assert_eq!(miss[0].domain_src.as_deref(), Some("PD_SRC"));
    assert_eq!(miss[0].domain_dst.as_deref(), Some("PD_DST"));
    assert!(miss[0].signal.contains("u_mid/y2"), "{miss:#?}");
}

#[test]
fn regression_permuted_swapped_concat_dst_slices_and_parent_clamp_slice() {
    let upf_header = r#"
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
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_AON}
create_power_domain PD_SRC -elements {u_src} -supply {primary SS_SW}
"#;

    // 1. `1A2-dst-slice-rev`: `{bus[1:0], bus[7:6]}` + `-elements {u_dst/d[1:0]}` -> flags `u_src/out_bus[1:0]` (2b).
    let sv_rev = r#"
module src(output logic [7:0] out_bus);
  assign out_bus = 8'h5a;
endmodule
module dst(input logic [3:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic iso_en, output logic y);
  logic [7:0] bus;
  src u_src(.out_bus(bus));
  dst u_dst(.d({bus[1:0], bus[7:6]}), .y(y));
endmodule
"#;
    let upf_rev = format!(
        "{upf_header}\nset_isolation iso_d -domain PD_DST -applies_to inputs -elements {{u_dst/d[1:0]}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location self\n"
    );
    let (v_rev, _, _) = run_rtl_upf("1a2_dst_rev", sv_rev, &upf_rev, "top");
    let m_rev: Vec<_> = v_rev
        .iter()
        .filter(|v| v.rule_id == "ISO_MISSING")
        .collect();
    assert_eq!(m_rev.len(), 1, "{v_rev:#?}");
    assert_eq!(m_rev[0].signal, "u_src/out_bus[1:0]");
    assert_eq!(m_rev[0].width_bits, 2);

    // 2. `swap-concat-dst-slice`: `{bus[3:0], bus[7:4]}` + `-elements {u_dst/d[3:0]}` -> flags `u_src/out_bus[3:0]` (4b).
    let sv_swap = r#"
module src(output logic [7:0] out_bus);
  assign out_bus = 8'h5a;
endmodule
module dst(input logic [7:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic iso_en, output logic y);
  logic [7:0] bus;
  src u_src(.out_bus(bus));
  dst u_dst(.d({bus[3:0], bus[7:4]}), .y(y));
endmodule
"#;
    let upf_swap = format!(
        "{upf_header}\nset_isolation iso_d -domain PD_DST -applies_to inputs -elements {{u_dst/d[3:0]}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location self\n"
    );
    let (v_swap, _, _) = run_rtl_upf("swap_dst_slice", sv_swap, &upf_swap, "top");
    let m_swap: Vec<_> = v_swap
        .iter()
        .filter(|v| v.rule_id == "ISO_MISSING")
        .collect();
    assert_eq!(m_swap.len(), 1, "{v_swap:#?}");
    assert_eq!(m_swap[0].signal, "u_src/out_bus[3:0]");
    assert_eq!(m_swap[0].width_bits, 4);

    // 3. `1A2-asc-dst-slice-rev`: ascending `[0:7]` & `[0:3]`, `.d({bus[6:7], bus[0:1]})` + `-elements {u_dst/d[0:1]}` -> flags `u_src/out_bus[0:1]` (2b).
    let sv_asc = r#"
module src(output logic [0:7] out_bus);
  assign out_bus = 8'h5a;
endmodule
module dst(input logic [0:3] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic iso_en, output logic y);
  logic [0:7] bus;
  src u_src(.out_bus(bus));
  dst u_dst(.d({bus[6:7], bus[0:1]}), .y(y));
endmodule
"#;
    let upf_asc = format!(
        "{upf_header}\nset_isolation iso_d -domain PD_DST -applies_to inputs -elements {{u_dst/d[0:1]}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location self\n"
    );
    let (v_asc, _, _) = run_rtl_upf("asc_dst_rev", sv_asc, &upf_asc, "top");
    let m_asc: Vec<_> = v_asc
        .iter()
        .filter(|v| v.rule_id == "ISO_MISSING")
        .collect();
    assert_eq!(m_asc.len(), 1, "{v_asc:#?}");
    assert_eq!(m_asc[0].signal, "u_src/out_bus[0:1]");
    assert_eq!(m_asc[0].width_bits, 2);

    // 4. `swap-concat-clamp`: `bus_hi_iso = iso_en ? 4'b1111 : bus[7:4]` must NOT trigger `ISO_CLAMP_MISMATCH` on `u_src/out_bus[3:0]`.
    let sv_clamp = r#"
module src(output logic [7:0] out_bus);
  assign out_bus = 8'h5a;
endmodule
module dst(input logic [7:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic iso_en, output logic y);
  logic [7:0] bus;
  logic [3:0] bus_hi_iso;
  src u_src(.out_bus(bus));
  assign bus_hi_iso = iso_en ? 4'b1111 : bus[7:4];
  dst u_dst(.d({bus[3:0], bus_hi_iso}), .y(y));
endmodule
"#;
    let upf_clamp = format!(
        "{upf_header}\nset_isolation iso_lo -domain PD_SRC -applies_to outputs -elements {{u_src/out_bus[3:0]}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\nset_isolation iso_hi -domain PD_SRC -applies_to outputs -elements {{u_src/out_bus[7:4]}} -clamp_value 1 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\n"
    );
    let (v_clamp, _, _) = run_rtl_upf("swap_clamp", sv_clamp, &upf_clamp, "top");
    assert!(
        !v_clamp.iter().any(|v| v.rule_id == "ISO_CLAMP_MISMATCH"),
        "unexpected ISO_CLAMP_MISMATCH: {v_clamp:#?}"
    );

    // 5. `parent -> child` slice-shifted input `.d(bus[7:2])` with `-elements {u_dst/d[5:4]}` -> single `ISO_MISSING` on `bus[5:2]` (4b).
    let sv_p2c = r#"
module dst(input logic [5:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic [7:0] bus, input logic iso_en, output logic y);
  dst u_dst(.d(bus[7:2]), .y(y));
endmodule
"#;
    let upf_p2c = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_SW
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW  -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
create_power_domain PD_TOP -include_scope -supply {primary SS_SW}
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_AON}
set_isolation iso_d -domain PD_DST -applies_to inputs -elements {u_dst/d[5:4]} \
  -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location self
"#;
    let (v_p2c, _, _) = run_rtl_upf("p2c_slice_shift", sv_p2c, upf_p2c, "top");
    let m_p2c: Vec<_> = v_p2c
        .iter()
        .filter(|v| v.rule_id == "ISO_MISSING")
        .collect();
    assert_eq!(m_p2c.len(), 1, "{v_p2c:#?}");
    assert_eq!(m_p2c[0].signal, "bus[5:2]");
    assert_eq!(m_p2c[0].width_bits, 4);
}
