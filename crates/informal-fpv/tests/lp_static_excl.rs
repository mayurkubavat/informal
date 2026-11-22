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
        std::env::temp_dir().join(format!("informal_lp_excl_{}_{n}_{tag}", std::process::id()));
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

const UPF_SW_AON_HEADER: &str = r#"
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

#[test]
fn exclude_elements_slice_and_base_isolation_all_seven_scenarios() {
    let sv_contig = r#"
module src(output logic [7:0] out_bus);
  assign out_bus = 8'h5a;
endmodule
module dst(input logic [7:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic iso_en, output logic y);
  logic [7:0] bus;
  src u_src(.out_bus(bus));
  dst u_dst(.d(bus), .y(y));
endmodule
"#;
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
    let sv_partial = r#"
module src(output logic [7:0] out_bus);
  assign out_bus = 8'h5a;
endmodule
module dst(input logic [3:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic iso_en, output logic y);
  logic [7:0] bus;
  src u_src(.out_bus(bus));
  dst u_dst(.d(bus[3:0]), .y(y));
endmodule
"#;

    // 1. `swap-concat-exclude-dst-slice` -> ISO_MISSING on `u_src/out_bus[7:4]` (4b)
    let upf1 = format!(
        "{UPF_SW_AON_HEADER}\nset_isolation iso_d -domain PD_DST -applies_to inputs -elements {{u_dst/d}} -exclude_elements {{u_dst/d[3:0]}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location self\n"
    );
    let v1 = run_rtl_upf("excl_swap_dst", sv_swap, &upf1, "top");
    let m1: Vec<_> = v1.iter().filter(|v| v.rule_id == "ISO_MISSING").collect();
    assert_eq!(m1.len(), 1, "{v1:#?}");
    assert_eq!(m1[0].signal, "u_src/out_bus[7:4]");
    assert_eq!(m1[0].width_bits, 4);

    // 2. `swap-concat-exclude-src-slice` -> ISO_MISSING on `u_src/out_bus[3:0]` (4b)
    let upf2 = format!(
        "{UPF_SW_AON_HEADER}\nset_isolation iso_s -domain PD_SRC -applies_to outputs -elements {{u_src/out_bus}} -exclude_elements {{u_src/out_bus[3:0]}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\n"
    );
    let v2 = run_rtl_upf("excl_swap_src", sv_swap, &upf2, "top");
    let m2: Vec<_> = v2.iter().filter(|v| v.rule_id == "ISO_MISSING").collect();
    assert_eq!(m2.len(), 1, "{v2:#?}");
    assert_eq!(m2[0].signal, "u_src/out_bus[3:0]");
    assert_eq!(m2[0].width_bits, 4);

    // 3. `contig-exclude-elem-slice` -> ISO_MISSING on `u_src/out_bus[3:0]` (4b)
    let v3 = run_rtl_upf("excl_contig_elem", sv_contig, &upf2, "top");
    let m3: Vec<_> = v3.iter().filter(|v| v.rule_id == "ISO_MISSING").collect();
    assert_eq!(m3.len(), 1, "{v3:#?}");
    assert_eq!(m3[0].signal, "u_src/out_bus[3:0]");
    assert_eq!(m3[0].width_bits, 4);

    // 4. `contig-exclude-dom-slice` -> ISO_MISSING on `u_src/out_bus[3:0]` (4b)
    let upf4 = format!(
        "{UPF_SW_AON_HEADER}\nset_isolation iso_dom -domain PD_SRC -applies_to outputs -exclude_elements {{u_src/out_bus[3:0]}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\n"
    );
    let v4 = run_rtl_upf("excl_contig_dom", sv_contig, &upf4, "top");
    let m4: Vec<_> = v4.iter().filter(|v| v.rule_id == "ISO_MISSING").collect();
    assert_eq!(m4.len(), 1, "{v4:#?}");
    assert_eq!(m4[0].signal, "u_src/out_bus[3:0]");
    assert_eq!(m4[0].width_bits, 4);

    // 5. `partial-slice-excl-same-slice` -> ISO_MISSING on `u_src/out_bus[3:0]` (4b)
    let v5 = run_rtl_upf("excl_partial_same", sv_partial, &upf4, "top");
    let m5: Vec<_> = v5.iter().filter(|v| v.rule_id == "ISO_MISSING").collect();
    assert_eq!(m5.len(), 1, "{v5:#?}");
    assert_eq!(m5[0].signal, "u_src/out_bus[3:0]");
    assert_eq!(m5[0].width_bits, 4);

    // 6. `partial-slice-excl-whole-bus` -> ISO_MISSING on `u_src/out_bus[3:0]` (4b)
    let upf6 = format!(
        "{UPF_SW_AON_HEADER}\nset_isolation iso_dom -domain PD_SRC -applies_to outputs -exclude_elements {{u_src/out_bus}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\n"
    );
    let v6 = run_rtl_upf("excl_partial_whole", sv_partial, &upf6, "top");
    let m6: Vec<_> = v6.iter().filter(|v| v.rule_id == "ISO_MISSING").collect();
    assert_eq!(m6.len(), 1, "{v6:#?}");
    assert_eq!(m6[0].signal, "u_src/out_bus[3:0]");
    assert_eq!(m6[0].width_bits, 4);

    // 7. `ls-exclude-elem-slice` -> LS_MISSING on `u_src/out_bus[3:0]` (4b)
    let sv_ls = r#"
module src(output logic [7:0] out_bus, output logic [7:0] a);
  assign out_bus = 8'h5a;
  assign a = 8'ha5;
endmodule
module dst(input logic [7:0] d, input logic [7:0] a, output logic y);
  assign y = (^d) ^ (^a);
endmodule
module top(output logic y);
  logic [7:0] bus, a;
  src u_src(.out_bus(bus), .a(a));
  dst u_dst(.d(bus), .a(a), .y(y));
endmodule
"#;
    let upf_ls = r#"
upf_version 2.1
create_supply_net VDD_LO
create_supply_net VDD_HI
create_supply_net VSS
create_supply_set SS_LO -function {power VDD_LO} -function {ground VSS}
create_supply_set SS_HI -function {power VDD_HI} -function {ground VSS}
add_power_state SS_LO -state {ON -supply_expr {power == {FULL_ON 0.8} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_HI -state {ON -supply_expr {power == {FULL_ON 1.2} && ground == {FULL_ON 0.0}} -simstate NORMAL}
create_power_domain PD_TOP -include_scope -supply {primary SS_HI}
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_HI}
create_power_domain PD_SRC -elements {u_src} -supply {primary SS_LO}
set_level_shifter ls_all -domain PD_SRC -applies_to outputs -elements {u_src/out_bus} -exclude_elements {u_src/out_bus[3:0]} -rule low_to_high -location parent
"#;
    let v7 = run_rtl_upf("excl_ls_slice", sv_ls, upf_ls, "top");
    let ls_miss: Vec<_> = v7.iter().filter(|v| v.rule_id == "LS_MISSING").collect();
    assert_eq!(ls_miss.len(), 2, "{v7:#?}");
    assert!(
        ls_miss
            .iter()
            .any(|v| v.signal == "u_src/out_bus[3:0]" && v.width_bits == 4),
        "{ls_miss:#?}"
    );
    assert!(
        !ls_miss.iter().any(|v| v.signal.contains("out_bus[7:4]")),
        "{ls_miss:#?}"
    );
}

#[test]
fn no_isolation_and_no_shift_honor_exclude_elements_and_slice_elements() {
    let sv = r#"
module src(output logic [7:0] out_bus);
  assign out_bus = 8'h5a;
endmodule
module dst(input logic [7:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic iso_en, output logic y);
  logic [7:0] bus;
  src u_src(.out_bus(bus));
  dst u_dst(.d(bus), .y(y));
endmodule
"#;

    // `-no_isolation` with `-exclude_elements {u_src/out_bus[3:0]}` -> `[7:4]` suppressed, `[3:0]` fires `ISO_MISSING`.
    let upf_no_iso_excl = format!(
        "{UPF_SW_AON_HEADER}\nset_isolation iso_no -domain PD_SRC -no_isolation -exclude_elements {{u_src/out_bus[3:0]}}\n"
    );
    let v_no = run_rtl_upf("no_iso_excl", sv, &upf_no_iso_excl, "top");
    let m_no: Vec<_> = v_no.iter().filter(|v| v.rule_id == "ISO_MISSING").collect();
    assert_eq!(m_no.len(), 1, "{v_no:#?}");
    assert_eq!(m_no[0].signal, "u_src/out_bus[3:0]");
    assert_eq!(m_no[0].width_bits, 4);

    // `-no_isolation -elements {u_src/out_bus} -exclude_elements {u_src/out_bus[3:0]}` above domain default ->
    // `[7:4]` suppressed, `[3:0]` falls back to domain default `iso_def` (no `ISO_MISSING`).
    let upf_no_over_def = format!(
        "{UPF_SW_AON_HEADER}\nset_isolation iso_def -domain PD_SRC -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\nset_isolation iso_no -domain PD_SRC -no_isolation -elements {{u_src/out_bus}} -exclude_elements {{u_src/out_bus[3:0]}}\n"
    );
    let v_def = run_rtl_upf("no_iso_fallback", sv, &upf_no_over_def, "top");
    assert!(
        !v_def.iter().any(|v| v.rule_id == "ISO_MISSING"),
        "expected [3:0] to fall back to domain default iso_def: {v_def:#?}"
    );

    // `-no_shift` with `-exclude_elements {u_src/out_bus[3:0]}` and slice `-elements {u_src/out_bus[3:0]}`.
    let upf_no_ls_hdr = r#"
upf_version 2.1
create_supply_net VDD_LO
create_supply_net VDD_HI
create_supply_net VSS
create_supply_set SS_LO -function {power VDD_LO} -function {ground VSS}
create_supply_set SS_HI -function {power VDD_HI} -function {ground VSS}
add_power_state SS_LO -state {ON -supply_expr {power == {FULL_ON 0.8} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_HI -state {ON -supply_expr {power == {FULL_ON 1.2} && ground == {FULL_ON 0.0}} -simstate NORMAL}
create_power_domain PD_TOP -include_scope -supply {primary SS_HI}
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_HI}
create_power_domain PD_SRC -elements {u_src} -supply {primary SS_LO}
"#;
    let upf_no_ls_excl = format!(
        "{upf_no_ls_hdr}\nset_level_shifter ls_no -domain PD_SRC -no_shift -exclude_elements {{u_src/out_bus[3:0]}}\n"
    );
    let v_ls_excl = run_rtl_upf("no_ls_excl", sv, &upf_no_ls_excl, "top");
    let m_ls_excl: Vec<_> = v_ls_excl
        .iter()
        .filter(|v| v.rule_id == "LS_MISSING")
        .collect();
    assert_eq!(m_ls_excl.len(), 1, "{v_ls_excl:#?}");
    assert_eq!(m_ls_excl[0].signal, "u_src/out_bus[3:0]");
    assert_eq!(m_ls_excl[0].width_bits, 4);

    let upf_no_ls_slice = format!(
        "{upf_no_ls_hdr}\nset_level_shifter ls_no -domain PD_SRC -no_shift -elements {{u_src/out_bus[3:0]}}\n"
    );
    let v_ls_slice = run_rtl_upf("no_ls_slice", sv, &upf_no_ls_slice, "top");
    let m_ls_slice: Vec<_> = v_ls_slice
        .iter()
        .filter(|v| v.rule_id == "LS_MISSING")
        .collect();
    assert_eq!(m_ls_slice.len(), 1, "{v_ls_slice:#?}");
    assert_eq!(m_ls_slice[0].signal, "u_src/out_bus[7:4]");
    assert_eq!(m_ls_slice[0].width_bits, 4);
}

#[test]
fn exclude_elements_ascending_wildcard_escaped_and_replicated_fanout() {
    // 1. Ascending [0:7] + `-exclude_elements {u_src/out_bus[0:3]}`
    let sv_asc = r#"
module src(output logic [0:7] out_bus);
  assign out_bus = 8'h5a;
endmodule
module dst(input logic [0:7] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic iso_en, output logic y);
  logic [0:7] bus;
  src u_src(.out_bus(bus));
  dst u_dst(.d(bus), .y(y));
endmodule
"#;
    let upf_asc = format!(
        "{UPF_SW_AON_HEADER}\nset_isolation iso_asc -domain PD_SRC -applies_to outputs -elements {{u_src/out_bus}} -exclude_elements {{u_src/out_bus[0:3]}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\n"
    );
    let v_asc = run_rtl_upf("excl_asc", sv_asc, &upf_asc, "top");
    let m_asc: Vec<_> = v_asc
        .iter()
        .filter(|v| v.rule_id == "ISO_MISSING")
        .collect();
    assert_eq!(m_asc.len(), 1, "{v_asc:#?}");
    assert_eq!(m_asc[0].signal, "u_src/out_bus[0:3]");
    assert_eq!(m_asc[0].width_bits, 4);

    // 2. Wildcard + escaped identifier `-exclude_elements`
    let sv_esc = r#"
module src(output logic [7:0] out_bus);
  assign out_bus = 8'h5a;
endmodule
module dst(input logic [7:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic iso_en, output logic y);
  logic [7:0] bus;
  src \u_blk$1 (.out_bus(bus));
  dst u_dst(.d(bus), .y(y));
endmodule
"#;
    let upf_esc = r#"
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
create_power_domain PD_SRC -elements {\u_blk$1 } -supply {primary SS_SW}
set_isolation iso_esc -domain PD_SRC -applies_to outputs -elements {\u_blk$1 /out_*} -exclude_elements {\u_blk$1 /out_*[3:0]} \
  -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent
"#;
    let v_esc = run_rtl_upf("excl_esc_wc", sv_esc, upf_esc, "top");
    let m_esc: Vec<_> = v_esc
        .iter()
        .filter(|v| v.rule_id == "ISO_MISSING")
        .collect();
    assert_eq!(m_esc.len(), 1, "{v_esc:#?}");
    assert_eq!(m_esc[0].signal, "u_blk$1/out_bus[3:0]");
    assert_eq!(m_esc[0].width_bits, 4);

    // 3. Same-port replicated fanout `{bus[3:0], bus[3:0]}` with per-`dst`-slice isolation strategies.
    let sv_rep = r#"
module src(output logic [3:0] out_bus);
  assign out_bus = 4'ha;
endmodule
module dst(input logic [7:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic iso_en, output logic y);
  logic [3:0] bus;
  src u_src(.out_bus(bus));
  dst u_dst(.d({bus[3:0], bus[3:0]}), .y(y));
endmodule
"#;
    let upf_rep_both = format!(
        "{UPF_SW_AON_HEADER}\nset_isolation iso_d0 -domain PD_DST -applies_to inputs -elements {{u_dst/d[3:0]}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location self\nset_isolation iso_d1 -domain PD_DST -applies_to inputs -elements {{u_dst/d[7:4]}} -clamp_value 1 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location self\n"
    );
    let v_rep_both = run_rtl_upf("rep_fanout_both", sv_rep, &upf_rep_both, "top");
    assert!(
        !v_rep_both.iter().any(|v| v.rule_id == "ISO_MISSING"),
        "expected both replicated dst lanes isolated: {v_rep_both:#?}"
    );

    let upf_rep_half = format!(
        "{UPF_SW_AON_HEADER}\nset_isolation iso_d -domain PD_DST -applies_to inputs -elements {{u_dst/d}} -exclude_elements {{u_dst/d[3:0]}} -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location self\n"
    );
    let v_rep_half = run_rtl_upf("rep_fanout_half", sv_rep, &upf_rep_half, "top");
    let m_rep: Vec<_> = v_rep_half
        .iter()
        .filter(|v| v.rule_id == "ISO_MISSING")
        .collect();
    assert_eq!(m_rep.len(), 1, "{v_rep_half:#?}");
    assert_eq!(m_rep[0].signal, "u_src/out_bus[3:0]");
    assert_eq!(m_rep[0].width_bits, 4);
}

#[test]
fn ls_unisolated_input_honors_split_and_excluded_slice_isolation() {
    let sv = r#"
module src(output logic [7:0] out_bus);
  assign out_bus = 8'h5a;
endmodule
module dst(input logic [7:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic iso_en, output logic y);
  logic [7:0] bus;
  src u_src(.out_bus(bus));
  dst u_dst(.d(bus), .y(y));
endmodule
"#;
    let hdr = r#"
upf_version 2.1
create_supply_net VDD_LO
create_supply_net VDD_HI
create_supply_net VSS
create_supply_set SS_LO -function {power VDD_LO} -function {ground VSS}
create_supply_set SS_HI -function {power VDD_HI} -function {ground VSS}
add_power_state SS_LO -state {ON  -supply_expr {power == {FULL_ON 0.8} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                      -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
add_power_state SS_HI -state {ON  -supply_expr {power == {FULL_ON 1.2} && ground == {FULL_ON 0.0}} -simstate NORMAL}
create_power_domain PD_TOP -include_scope -supply {primary SS_HI}
create_power_domain PD_DST -elements {u_dst} -supply {primary SS_HI}
create_power_domain PD_SRC -elements {u_src} -supply {primary SS_LO}
set_level_shifter ls_all -domain PD_SRC -applies_to outputs -elements {u_src/out_bus} -rule low_to_high -location parent
"#;

    // 1. Whole-bus LS + split [3:0] + [7:4] isolation -> 0 `ISO_MISSING`, 0 `LS_MISSING`, 0 `LS_UNISOLATED_INPUT`.
    let upf_split = format!(
        "{hdr}\nset_isolation iso_lo -domain PD_SRC -applies_to outputs -elements {{u_src/out_bus[3:0]}} -clamp_value 0 -isolation_supply_set SS_HI -isolation_signal iso_en -isolation_sense high -location parent\nset_isolation iso_hi -domain PD_SRC -applies_to outputs -elements {{u_src/out_bus[7:4]}} -clamp_value 0 -isolation_supply_set SS_HI -isolation_signal iso_en -isolation_sense high -location parent\n"
    );
    let v_split = run_rtl_upf("ls_uniso_split", sv, &upf_split, "top");
    assert!(
        !v_split.iter().any(|v| matches!(
            v.rule_id.as_str(),
            "ISO_MISSING" | "LS_MISSING" | "LS_UNISOLATED_INPUT"
        )),
        "expected 0 ISO/LS violations on 100%-split-isolated bus: {v_split:#?}"
    );

    // 2. Whole-bus LS + `-exclude_elements {u_src/out_bus[7:4]}` + `iso_hi` on `[7:4]` -> 0 `LS_UNISOLATED_INPUT`.
    let upf_excl_fill = format!(
        "{hdr}\nset_isolation iso_lo -domain PD_SRC -applies_to outputs -elements {{u_src/out_bus}} -exclude_elements {{u_src/out_bus[7:4]}} -clamp_value 0 -isolation_supply_set SS_HI -isolation_signal iso_en -isolation_sense high -location parent\nset_isolation iso_hi -domain PD_SRC -applies_to outputs -elements {{u_src/out_bus[7:4]}} -clamp_value 0 -isolation_supply_set SS_HI -isolation_signal iso_en -isolation_sense high -location parent\n"
    );
    let v_excl_fill = run_rtl_upf("ls_uniso_excl_fill", sv, &upf_excl_fill, "top");
    assert!(
        !v_excl_fill.iter().any(|v| matches!(
            v.rule_id.as_str(),
            "ISO_MISSING" | "LS_MISSING" | "LS_UNISOLATED_INPUT"
        )),
        "expected 0 ISO/LS violations on 100%-excluded+override-isolated bus: {v_excl_fill:#?}"
    );

    // 3. Partial unisolated slice (`[7:4]` isolated, `[3:0]` unisolated) -> `LS_UNISOLATED_INPUT` blames only `[3:0]` (4b).
    let upf_partial = format!(
        "{hdr}\nset_isolation iso_hi -domain PD_SRC -applies_to outputs -elements {{u_src/out_bus[7:4]}} -clamp_value 0 -isolation_supply_set SS_HI -isolation_signal iso_en -isolation_sense high -location parent\n"
    );
    let v_partial = run_rtl_upf("ls_uniso_partial", sv, &upf_partial, "top");
    let ls_uniso: Vec<_> = v_partial
        .iter()
        .filter(|v| v.rule_id == "LS_UNISOLATED_INPUT")
        .collect();
    assert_eq!(ls_uniso.len(), 1, "{v_partial:#?}");
    assert_eq!(ls_uniso[0].signal, "u_src/out_bus[3:0]");
    assert_eq!(ls_uniso[0].width_bits, 4);
}
