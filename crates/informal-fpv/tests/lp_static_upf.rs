use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use informal_fpv::lp::{check_lp, load_upf, CheckLpOpts, LpStage};
use informal_fpv::{Flavor, Session, Shell};
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
    let dir =
        std::env::temp_dir().join(format!("informal_lp_upf_{}_{n}_{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    TempDir(dir)
}

fn run_stage1_upf(tag: &str, upf_src: &str) -> Vec<String> {
    let dir = new_temp(tag);
    let upf_path = dir.0.join(format!("{tag}.upf"));
    std::fs::write(&upf_path, upf_src).expect("write upf");
    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    load_upf(&mut s, &upf_path, None, None, false).expect("load_upf");
    check_lp(
        &mut s,
        &CheckLpOpts {
            stage: LpStage::Upf,
            tol_uv: DEFAULT_VOLTAGE_TOL_UV,
            ..Default::default()
        },
    )
    .expect("check_lp -stage upf");
    s.lp.violations.iter().map(|v| v.rule_id.clone()).collect()
}

#[test]
fn stage1_supply_cycle_dangling_and_domain_no_supply() {
    let upf = r#"
upf_version 2.1
create_supply_port VDD_MAIN
create_supply_port VDD_DANGLING
create_supply_net VDD_MAIN
create_supply_net VDD_SW1
create_supply_net VDD_SW2
create_supply_net VDD_HALF
create_supply_net VSS
connect_supply_net VDD_MAIN -ports VDD_MAIN
connect_supply_net VDD_HALF -ports VDD_MAIN

create_supply_set SS_MAIN -function {power VDD_MAIN} -function {ground VSS}
create_supply_set SS_SW1  -function {power VDD_SW1}  -function {ground VSS}
create_supply_set SS_NOPWR -function {ground VSS}

create_power_domain PD_TOP   -include_scope -supply {primary SS_MAIN}
create_power_domain PD_SW1   -supply {primary SS_SW1}
create_power_domain PD_NOPWR -supply {primary SS_NOPWR}
create_power_domain PD_EMPTY

create_power_switch sw_a -domain PD_SW1 \
  -input_supply_port {in VDD_SW2} -output_supply_port {out VDD_SW1} \
  -control_port {ctrl en_a} -on_state {ON in {ctrl}} -off_state {OFF {!ctrl}}
create_power_switch sw_b -domain PD_SW1 \
  -input_supply_port {in VDD_SW1} -output_supply_port {out VDD_SW2} \
  -control_port {ctrl en_b} -on_state {ON in {ctrl}} -off_state {OFF {!ctrl}}
create_power_switch sw_bad -domain PD_SW1 \
  -input_supply_port {in MISSING_NET} -output_supply_port {out VDD_SW1} \
  -control_port {ctrl en_c} -on_state {ON in {ctrl}} -off_state {OFF {!ctrl}}
"#;
    let rules = run_stage1_upf("s1_topo", upf);
    assert!(rules.iter().any(|r| r == "UPF_SUPPLY_CYCLE"), "{rules:?}");
    assert!(rules.iter().any(|r| r == "UPF_DANGLING_NET"), "{rules:?}");
    assert!(
        rules.iter().any(|r| r == "UPF_DOMAIN_NO_SUPPLY"),
        "{rules:?}"
    );
    assert!(
        rules.iter().any(|r| r == "PSW_SUPPLY_UNCONNECTED"),
        "{rules:?}"
    );
}

#[test]
fn stage1_pst_inconsistent_unreachable_missing_and_equiv_mismatch() {
    let upf_inconsistent = r#"
upf_version 2.1
create_supply_net VDD_A
create_supply_net VDD_B
create_supply_net VDD_NOPST
create_supply_net VSS

create_supply_set SS_A -function {power VDD_A} -function {ground VSS}
create_supply_set SS_B -function {power VDD_B} -function {ground VSS}
create_supply_set SS_NOPST -function {power VDD_NOPST} -function {ground VSS}

add_power_state SS_A -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_B -state {ON  -supply_expr {power == {FULL_ON 1.2} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_A -state {BAD -illegal -supply_expr {power == {FULL_ON 0.9}}}

create_power_domain PD_TOP -include_scope -supply {primary SS_A}
create_power_domain PD_B   -supply {primary SS_B}
create_power_domain PD_N   -supply {primary SS_NOPST}

create_pst pst_bad -supplies {VDD_A VDD_B}
add_pst_state S0 -pst pst_bad -state {NO_SUCH_STATE ON}
"#;
    let rules = run_stage1_upf("s1_pst", upf_inconsistent);
    assert!(rules.iter().any(|r| r == "PST_INCONSISTENT"), "{rules:?}");
    assert!(
        rules.iter().any(|r| r == "PST_UNREACHABLE_STATE"),
        "{rules:?}"
    );
    assert!(
        rules.iter().any(|r| r == "PST_MISSING_PORT_STATE"),
        "{rules:?}"
    );

    let upf_equiv = r#"
upf_version 2.1
create_supply_net VDD_A
create_supply_net VDD_B
create_supply_net VSS
create_supply_set SS_A -function {power VDD_A} -function {ground VSS}
create_supply_set SS_B -function {power VDD_B} -function {ground VSS}
add_power_state SS_A -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_B -state {ON -supply_expr {power == {FULL_ON 1.2} && ground == {FULL_ON 0.0}} -simstate NORMAL}
set_equivalent -sets {SS_A SS_B}
create_power_domain PD_TOP -include_scope -supply {primary SS_A}
create_power_domain PD_B   -supply {primary SS_B}
"#;
    let equiv_rules = run_stage1_upf("s1_equiv", upf_equiv);
    assert!(
        equiv_rules.iter().any(|r| r == "UPF_EQUIV_MISMATCH"),
        "{equiv_rules:?}"
    );
}

#[test]
fn stage1_strategy_overlap_clamp_unspecified_ret_empty_and_d13() {
    let upf = r#"
upf_version 2.1
create_supply_net VDD
create_supply_net VSS
create_supply_set SS_AON -function {power VDD} -function {ground VSS}
add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}

set_isolation iso_1 -domain PD_TOP -applies_to outputs -isolation_supply_set SS_AON
set_isolation iso_2 -domain PD_TOP -applies_to outputs -clamp_value 1 -isolation_supply_set SS_AON
set_retention ret_1 -domain PD_TOP -retention_supply_set SS_AON
set_retention_elements ret_empty -domain PD_TOP -elements {}
"#;
    let rules = run_stage1_upf("s1_strat", upf);
    assert!(
        rules.iter().any(|r| r == "UPF_STRATEGY_OVERLAP"),
        "{rules:?}"
    );
    assert!(
        rules.iter().any(|r| r == "ISO_CLAMP_UNSPECIFIED"),
        "{rules:?}"
    );
    assert!(rules.iter().any(|r| r == "RET_NO_ELEMENTS"), "{rules:?}");

    // Decision D13 unsupported option fails cleanly on load_upf.
    let dir = new_temp("d13");
    let bad_upf = dir.0.join("bad.upf");
    std::fs::write(
        &bad_upf,
        "upf_version 2.1\nset_isolation iso_x -domain PD_TOP -bogus_flag\n",
    )
    .unwrap();
    let mut s = Session::new(Vec::new(), dir.0.join("work"));
    let err = load_upf(&mut s, &bad_upf, None, None, false).unwrap_err();
    assert!(err.contains("bogus_flag"), "{err}");
}

#[test]
fn stage1_compare_upf_positive_and_negative() {
    let dir = new_temp("cmp");
    let g_path = dir.0.join("golden.upf");
    let r_ok_path = dir.0.join("rev_ok.upf");
    let r_bad_path = dir.0.join("rev_bad.upf");
    let base = r#"
upf_version 2.1
create_supply_net VDD
create_supply_net VSS
create_supply_set SS_AON -function {power VDD} -function {ground VSS}
add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
set_isolation iso_a -domain PD_TOP -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON
"#;
    let diff = base.replace("-clamp_value 0", "-clamp_value 1");
    std::fs::write(&g_path, base).unwrap();
    std::fs::write(&r_ok_path, base).unwrap();
    std::fs::write(&r_bad_path, diff).unwrap();

    let sh = Shell::new(Session::new(Vec::new(), dir.0.join("work")), Flavor::Jg);
    sh.eval(&format!(
        "compare_upf -golden {{{}}} -revised {{{}}}",
        g_path.display(),
        r_ok_path.display()
    ))
    .expect("equivalent UPFs should pass");

    let err = sh
        .eval(&format!(
            "compare_upf -golden {{{}}} -revised {{{}}}",
            g_path.display(),
            r_bad_path.display()
        ))
        .unwrap_err();
    assert!(err.message.contains("mismatch"), "{err}");
}

#[test]
fn stage2_root_dot_order_independent_with_default_and_include_scope() {
    let sv = r#"
module sw_blk(input logic clk, input logic d, output logic [7:0] out_bus);
  always_ff @(posedge clk) out_bus <= {8{d}};
endmodule
module leaf_blk(input logic [7:0] in_bus, output logic [7:0] y);
  assign y = in_bus;
endmodule
module top(input logic clk, input logic d, output logic [7:0] y);
  logic [7:0] w;
  sw_blk u_sw(.clk(clk), .d(d), .out_bus(w));
  leaf_blk u_leaf(.in_bus(w), .y(y));
endmodule
"#;
    let run_case = |tag: &str, intent: &str, is_cpf: bool| -> Vec<(String, String, u32)> {
        let dir = new_temp(tag);
        let sv_path = dir.0.join("top.sv");
        let intent_path = dir.0.join(if is_cpf { "p.cpf" } else { "p.upf" });
        std::fs::write(&sv_path, sv).unwrap();
        std::fs::write(&intent_path, intent).unwrap();
        let mut s = Session::new(Vec::new(), dir.0.join("work"));
        s.design.files.push(sv_path);
        s.elaborate(Some("top".to_string())).unwrap();
        load_upf(&mut s, &intent_path, None, None, is_cpf).unwrap();
        check_lp(
            &mut s,
            &CheckLpOpts {
                stage: LpStage::Rtl,
                ..Default::default()
            },
        )
        .unwrap();
        let mut out: Vec<_> =
            s.lp.violations
                .into_iter()
                .filter(|v| v.rule_id != "PORT_ATTR_MISSING")
                .map(|v| (v.rule_id, v.signal, v.width_bits))
                .collect();
        out.sort();
        out
    };

    let cpf_def_first = r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_SW} -voltage 0.90
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_AON -default
        create_power_domain -name PD_SW -instances {.} -exclude_instances {u_leaf} -shutoff_condition {!pwr_en}
        update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_SW  -primary_power_net VDD_SW  -primary_ground_net VSS
        create_nominal_condition -name NC_ON  -voltage 0.90 -state on
        create_nominal_condition -name NC_OFF -voltage 0.0  -state off
        create_power_mode -name PM_ON  -domain_conditions {PD_AON@NC_ON PD_SW@NC_ON} -default
        create_power_mode -name PM_OFF -domain_conditions {PD_AON@NC_ON PD_SW@NC_OFF}
        end_design
    "#;
    let cpf_dot_first = r#"
        set_cpf_version 2.0
        create_power_nets -nets {VDD_AON VDD_SW} -voltage 0.90
        create_ground_nets -nets {VSS}
        create_power_domain -name PD_SW -instances {.} -exclude_instances {u_leaf} -shutoff_condition {!pwr_en}
        create_power_domain -name PD_AON -default
        update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
        update_power_domain -name PD_SW  -primary_power_net VDD_SW  -primary_ground_net VSS
        create_nominal_condition -name NC_ON  -voltage 0.90 -state on
        create_nominal_condition -name NC_OFF -voltage 0.0  -state off
        create_power_mode -name PM_ON  -domain_conditions {PD_AON@NC_ON PD_SW@NC_ON} -default
        create_power_mode -name PM_OFF -domain_conditions {PD_AON@NC_ON PD_SW@NC_OFF}
        end_design
    "#;
    let v_cpf_1 = run_case("cpf_def_first", cpf_def_first, true);
    let v_cpf_2 = run_case("cpf_dot_first", cpf_dot_first, true);
    assert_eq!(v_cpf_1, v_cpf_2);
    assert!(
        v_cpf_1.contains(&(
            "ISO_MISSING".to_string(),
            "u_sw/out_bus[7:0]".to_string(),
            8
        )),
        "{v_cpf_1:?}"
    );
    assert!(
        v_cpf_1.contains(&("RET_NO_ELEMENTS".to_string(), "PD_SW".to_string(), 1)),
        "{v_cpf_1:?}"
    );

    let upf_inc_first = r#"
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
        create_power_domain PD_SW  -elements {.} -exclude_elements {u_leaf} -supply {primary SS_SW}
    "#;
    let upf_dot_first = r#"
        upf_version 2.1
        create_supply_net VDD_AON
        create_supply_net VDD_SW
        create_supply_net VSS
        create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
        create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
        add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
        add_power_state SS_SW  -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                               -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
        create_power_domain PD_SW  -elements {.} -exclude_elements {u_leaf} -supply {primary SS_SW}
        create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
    "#;
    let v_upf_1 = run_case("upf_inc_first", upf_inc_first, false);
    let v_upf_2 = run_case("upf_dot_first", upf_dot_first, false);
    assert_eq!(v_upf_1, v_upf_2);
    assert!(
        v_upf_1.contains(&(
            "ISO_MISSING".to_string(),
            "u_sw/out_bus[7:0]".to_string(),
            8
        )),
        "{v_upf_1:?}"
    );
    assert!(
        v_upf_1.contains(&("RET_NO_ELEMENTS".to_string(), "PD_SW".to_string(), 1)),
        "{v_upf_1:?}"
    );
}

#[test]
fn stage2_top_glue_root_dot_vs_include_scope_and_scoped_slash_isolation() {
    let sv_top_glue = r#"
module leaf_blk(input logic in_sig, output logic y);
  assign y = in_sig;
endmodule
module top(input logic clk, input logic d, output logic y);
  logic q_top;
  always_ff @(posedge clk) q_top <= d;
  leaf_blk u_leaf(.in_sig(q_top), .y(y));
endmodule
"#;
    let run_rules =
        |tag: &str, sv: &str, upf: &str| -> Vec<(String, Option<String>, Option<String>, String)> {
            let dir = new_temp(tag);
            let sv_path = dir.0.join("top.sv");
            let upf_path = dir.0.join("p.upf");
            std::fs::write(&sv_path, sv).unwrap();
            std::fs::write(&upf_path, upf).unwrap();
            let mut s = Session::new(Vec::new(), dir.0.join("work"));
            s.design.files.push(sv_path);
            s.elaborate(Some("top".to_string())).unwrap();
            load_upf(&mut s, &upf_path, None, None, false).unwrap();
            check_lp(
                &mut s,
                &CheckLpOpts {
                    stage: LpStage::Rtl,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut out: Vec<_> =
                s.lp.violations
                    .into_iter()
                    .filter(|v| v.rule_id != "PORT_ATTR_MISSING")
                    .map(|v| (v.rule_id, v.domain_src, v.domain_dst, v.signal))
                    .collect();
            out.sort();
            out
        };

    let upf_hdr = r#"
upf_version 2.1
create_supply_net VDD_AON
create_supply_net VDD_SW
create_supply_net VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
add_power_state SS_AON -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW  -state {ON  -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL} \
                       -state {OFF -supply_expr {power == OFF} -simstate CORRUPT}
"#;

    // 1. Top-level `always_ff` glue under `PD_SW -elements {.}` vs `PD_AON -include_scope` (both orders).
    let upf_inc_first = format!(
        "{upf_hdr}\ncreate_power_domain PD_AON -include_scope -supply {{primary SS_AON}}\ncreate_power_domain PD_SW -elements {{.}} -exclude_elements {{u_leaf}} -supply {{primary SS_SW}}\n"
    );
    let upf_dot_first = format!(
        "{upf_hdr}\ncreate_power_domain PD_SW -elements {{.}} -exclude_elements {{u_leaf}} -supply {{primary SS_SW}}\ncreate_power_domain PD_AON -include_scope -supply {{primary SS_AON}}\n"
    );
    let v1 = run_rules("top_glue_inc_first", sv_top_glue, &upf_inc_first);
    let v2 = run_rules("top_glue_dot_first", sv_top_glue, &upf_dot_first);
    assert_eq!(v1, v2);
    assert!(
        v1.iter().any(|(r, src, dst, _)| {
            r == "ISO_MISSING"
                && src.as_deref() == Some("PD_SW")
                && dst.as_deref() == Some("PD_AON")
        }),
        "top-level always_ff glue must belong to PD_SW (`-elements {{.}}`): {v1:?}"
    );
    assert!(
        v1.iter()
            .any(|(r, src, _, _)| r == "RET_NO_ELEMENTS" && src.as_deref() == Some("PD_SW")),
        "expected RET_NO_ELEMENTS on PD_SW for top-level always_ff: {v1:?}"
    );

    // 2. `-exclude_elements {.}` excludes `top` from `PD_SW`, leaving it in `PD_AON -include_scope`.
    let upf_excl_dot = format!(
        "{upf_hdr}\ncreate_power_domain PD_AON -include_scope -supply {{primary SS_AON}}\ncreate_power_domain PD_SW -elements {{.}} -exclude_elements {{.}} -supply {{primary SS_SW}}\n"
    );
    let v_excl = run_rules("top_glue_excl_dot", sv_top_glue, &upf_excl_dot);
    assert!(
        v_excl.is_empty(),
        "excluding `.` must leave top-level glue in PD_AON with zero violations: {v_excl:?}"
    );

    // 3. `set_scope u_sub; set_isolation iso_root -domain /PD_SW -elements {/}` isolates root-sibling `u_sw/q`.
    let sv_scoped_iso = r#"
module sw_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module sub_blk(input logic a, output logic b);
  assign b = a;
endmodule
module top(input logic clk, input logic d, input logic iso_en, output logic b);
  logic q_sw;
  sw_blk  u_sw(.clk(clk), .d(d), .q(q_sw));
  sub_blk u_sub(.a(q_sw), .b(b));
endmodule
"#;
    let upf_scoped_iso = format!(
        "{upf_hdr}\ncreate_power_domain PD_AON -include_scope -supply {{primary SS_AON}}\ncreate_power_domain PD_SW -elements {{u_sw}} -supply {{primary SS_SW}}\nset_retention ret_sw -domain PD_SW -retention_supply_set SS_AON -save_signal {{iso_en high}} -restore_signal {{iso_en low}}\nset_scope u_sub\nset_isolation iso_root -domain /PD_SW -applies_to outputs -elements {{/}} -clamp_value 0 -isolation_supply_set /SS_AON -isolation_signal /iso_en -isolation_sense high -location parent\nset_scope /\n"
    );
    let v_iso = run_rules("scoped_slash_iso", sv_scoped_iso, &upf_scoped_iso);
    assert!(
        !v_iso.iter().any(|(r, _, _, _)| r == "ISO_MISSING"),
        "scoped `-elements {{/}}` must isolate root-sibling u_sw/q: {v_iso:?}"
    );
}
