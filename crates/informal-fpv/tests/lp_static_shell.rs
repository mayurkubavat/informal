use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use informal_fpv::compat::check;
use informal_fpv::{Flavor, Session, Shell};

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn new_temp(tag: &str) -> TempDir {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("informal_lp_sh_{}_{n}_{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    TempDir(dir)
}

#[test]
fn shell_jg_and_vcf_full_lp_workflow_and_waivers() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let exp2_sv = root.join("tests/lp/paper_cases/exp2_axi_valid_data/axi_valid_data.sv");
    let exp2_upf = root.join("tests/lp/paper_cases/exp2_axi_valid_data/axi_valid_data.upf");
    let dir = new_temp("shell_live");
    let json_out = dir.0.join("lp_report.json");

    // 1. Test Jg flavor: load_upf BEFORE elaborate works seamlessly too!
    let sh_jg = Shell::new(Session::new(Vec::new(), dir.0.join("w_jg")), Flavor::Jg);
    sh_jg
        .eval(&format!(
            "load_upf {{{}}}; analyze -sv {{{}}}; elaborate -top axi_valid_data; check_lp -stage rtl",
            exp2_upf.display(),
            exp2_sv.display()
        ))
        .expect("jg flow");
    let rep_js = sh_jg
        .eval(&format!(
            "report_lp -json -file {{{}}} -force",
            json_out.display()
        ))
        .expect("report_lp -json");
    let v: serde_json::Value = serde_json::from_str(&rep_js.to_string()).expect("valid json");
    assert_eq!(v["schema"], "informal.lp/1");
    assert_eq!(v["summary"]["open"], 1);
    assert_eq!(v["violations"][0]["rule_id"], "ISO_MISSING");
    assert_eq!(v["violations"][0]["width_bits"], 64);

    // Waiver add -> list -> status=waived -> remove -> status=open.
    let wid = sh_jg
        .eval("set_lp_waiver -add -rule ISO_MISSING -signal *m_data* -comment {axi valid gated}")
        .expect("waiver add")
        .to_string();
    let list = sh_jg.eval("set_lp_waiver -list").expect("waiver list");
    assert!(list.to_string().contains("ISO_MISSING"));
    let waived_js = sh_jg
        .eval("report_lp -status waived -json")
        .expect("waived json");
    let wv: serde_json::Value = serde_json::from_str(&waived_js.to_string()).unwrap();
    assert_eq!(wv["summary"]["waived"], 1);

    sh_jg
        .eval(&format!("set_lp_waiver -remove {wid}"))
        .expect("waiver remove");
    let open_js = sh_jg
        .eval("report_lp -status open -json")
        .expect("open json");
    let ov: serde_json::Value = serde_json::from_str(&open_js.to_string()).unwrap();
    assert_eq!(ov["summary"]["open"], 1);

    // 2. Test Vcf flavor + help + clear + check_lpv.
    let sh_vcf = Shell::new(Session::new(Vec::new(), dir.0.join("w_vcf")), Flavor::Vcf);
    let help_txt = sh_vcf.eval("help check_lp").expect("help check_lp");
    assert!(help_txt.to_string().contains("check_lp"));
    sh_vcf
        .eval(&format!(
            "read_file -format sverilog -top axi_valid_data {{{}}}; load_upf {{{}}}; check_lpv -stage rtl",
            exp2_sv.display(),
            exp2_upf.display()
        ))
        .expect("vcf flow");
    let vcf_js: serde_json::Value =
        serde_json::from_str(&sh_vcf.eval("report_lp -json").unwrap().to_string()).unwrap();
    assert_eq!(vcf_js["summary"]["errors"], 1);

    // 3. Re-elaboration invalidates cached results cleanly.
    sh_jg
        .eval("elaborate -top axi_valid_data")
        .expect("re-elab");
    assert!(sh_jg.session.borrow().lp.violations.is_empty());
}

#[test]
fn shell_dry_run_check_mode_validates_flags_and_errors() {
    let dir = new_temp("dry");
    let sh = Shell::new(Session::new(Vec::new(), dir.0.join("w")), Flavor::Jg);

    let ok_tcl = dir.0.join("ok.tcl");
    std::fs::write(
        &ok_tcl,
        "load_upf missing.upf\nread_liberty missing.lib\ncheck_lp -stage rtl\nreport_lp -verbose\nset wid [set_lp_waiver -add -rule ISO_*]\nset_lp_waiver -list\nset_lp_waiver -remove $wid\ncompare_upf -golden a.upf -revised b.upf\ncheck_lpv -stage formal\n",
    )
    .unwrap();
    let r_ok = sh.check(&ok_tcl);
    assert!(r_ok.clean(), "{:?}", r_ok.findings);

    let sh2 = Shell::new(Session::new(Vec::new(), dir.0.join("w2")), Flavor::Jg);
    let bad_tcl = dir.0.join("bad.tcl");
    std::fs::write(
        &bad_tcl,
        "check_lp -stage bogus_stage\nreport_lp -status bad_status\ncompare_upf -golden a.upf\n",
    )
    .unwrap();
    let r_bad = sh2.check(&bad_tcl);
    assert_eq!(r_bad.count(check::Kind::Error), 3, "{:?}", r_bad.findings);
}

#[test]
fn shell_multi_load_upf_scope_and_cpf_version_tol_and_waiver_invalidation() {
    let sv = r#"
module leaf(input logic clk, input logic d, output logic y);
  always_ff @(posedge clk) y <= d;
endmodule
module top(input logic clk, input logic d, input logic en, output logic y);
  leaf u_sw(.clk(clk), .d(d), .y(y));
endmodule
"#;
    let top_upf = r#"
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
"#;
    let leaf_upf = r#"
upf_version 2.1
create_power_domain PD_SW -include_scope -supply {primary /SS_SW}
set_isolation iso_sw -domain PD_SW -applies_to outputs -elements {y} \
  -clamp_value 0 -isolation_supply_set /SS_AON -isolation_signal /en -isolation_sense high -location parent
set_retention ret_sw -domain PD_SW -no_retention
"#;
    let dir = new_temp("multi_scope");
    let sv_path = dir.0.join("top.sv");
    let top_upf_path = dir.0.join("top.upf");
    let leaf_upf_path = dir.0.join("leaf.upf");
    std::fs::write(&sv_path, sv).unwrap();
    std::fs::write(&top_upf_path, top_upf).unwrap();
    std::fs::write(&leaf_upf_path, leaf_upf).unwrap();

    // 1. Pre-elaborate sequential `load_upf` + `load_upf -scope u_sw`.
    let sh_pre = Shell::new(Session::new(Vec::new(), dir.0.join("w_pre")), Flavor::Jg);
    sh_pre
        .eval(&format!(
            "load_upf {{{}}}; load_upf {{{}}} -scope u_sw; analyze -sv {{{}}}; elaborate -top top; check_lp -stage rtl",
            top_upf_path.display(),
            leaf_upf_path.display(),
            sv_path.display()
        ))
        .expect("pre-elab multi load_upf");
    assert_eq!(sh_pre.session.borrow().lp.upf_files.len(), 2);
    assert!(
        sh_pre.session.borrow().lp.violations.is_empty(),
        "{:#?}",
        sh_pre.session.borrow().lp.violations
    );

    // 2. Post-elaborate sequential `load_upf` + `load_upf -scope u_sw`.
    let sh_post = Shell::new(Session::new(Vec::new(), dir.0.join("w_post")), Flavor::Jg);
    sh_post
        .eval(&format!(
            "analyze -sv {{{}}}; elaborate -top top; load_upf {{{}}}; load_upf {{{}}} -scope u_sw; check_lp -stage rtl",
            sv_path.display(),
            top_upf_path.display(),
            leaf_upf_path.display()
        ))
        .expect("post-elab multi load_upf");
    assert_eq!(sh_post.session.borrow().lp.upf_files.len(), 2);
    assert!(
        sh_post.session.borrow().lp.violations.is_empty(),
        "{:#?}",
        sh_post.session.borrow().lp.violations
    );

    // 3. Invalid negative/NaN `-tol_mv` and negative `-tol_uv` are rejected.
    assert!(sh_post.eval("check_lp -stage upf -tol_mv -1.0").is_err());
    assert!(sh_post.eval("check_lp -stage upf -tol_mv NaN").is_err());
    assert!(sh_post
        .eval(&format!(
            "compare_upf -golden {{{}}} -revised {{{}}} -tol_uv -1",
            top_upf_path.display(),
            top_upf_path.display()
        ))
        .is_err());

    // 4. `vcf::create_reset` and `assume` mutations invalidate `FormallyWaived`.
    let sh_vcf = Shell::new(
        Session::new(Vec::new(), dir.0.join("w_vcf_inv")),
        Flavor::Vcf,
    );
    sh_vcf
        .eval(&format!(
            "read_file -format sverilog -top top {{{}}}; load_upf {{{}}}; check_lp -stage rtl",
            sv_path.display(),
            top_upf_path.display()
        ))
        .expect("vcf setup");
    {
        let mut sess = sh_vcf.session.borrow_mut();
        sess.lp.violations.push(informal_fpv::lp::LpViolation {
            id: 99,
            rule_id: "ISO_MISSING".to_string(),
            severity: informal_fpv::lp::LpSeverity::Error,
            stage: "rtl".to_string(),
            domain_src: None,
            domain_dst: None,
            signal: "u_sw/y".to_string(),
            width_bits: 1,
            raw_crossings: 1,
            message: "synthetic".to_string(),
            upf_loc: None,
            rtl_loc: None,
            pst_witness: None,
            status: informal_fpv::lp::LpStatus::FormallyWaived,
            waiver_id: None,
            formal_prop: None,
            trace: None,
            auto_formal_meta: None,
        });
    }
    sh_vcf.eval("create_reset en -high").expect("create_reset");
    assert_eq!(
        sh_vcf.session.borrow().lp.violations[0].status,
        informal_fpv::lp::LpStatus::Open
    );
}

#[test]
fn shell_cpf_stage2_e2e_clean_and_buggy() {
    let sv = r#"
module sw_core(input logic clk, input logic rst_n, input logic d, output logic q_out);
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) q_out <= 1'b0;
    else q_out <= d;
  end
endmodule
module aon_sink(input logic q_in, output logic y);
  assign y = q_in;
endmodule
module top(input logic clk, input logic rst_n, input logic iso_en, input logic pwr_off, input logic d, output logic y);
  logic q_sw;
  sw_core  u_sw(.clk(clk), .rst_n(rst_n), .d(d), .q_out(q_sw));
  aon_sink u_aon(.q_in(q_sw), .y(y));
endmodule
"#;
    let clean_cpf = r#"
set_cpf_version 2.0
set_design top
create_power_nets -nets VDD_AON -voltage 1.8
create_power_nets -nets VDD_SW  -voltage 0.9
create_ground_nets -nets VSS

create_power_domain -name PD_AON -default
create_power_domain -name PD_SW  -instances {u_sw} -shutoff_condition {pwr_off}
update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_SW  -primary_power_net VDD_SW  -primary_ground_net VSS

create_nominal_condition -name NC_AON -voltage 1.8 -state on
create_nominal_condition -name NC_SW  -voltage 0.9 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_mode -name PM_ON  -domain_conditions {PD_AON@NC_AON PD_SW@NC_SW}  -default
create_power_mode -name PM_OFF -domain_conditions {PD_AON@NC_AON PD_SW@NC_OFF}

create_power_switch_rule -name psw_sw -domain PD_SW -external_power_net VDD_AON -enable_condition_1 {!pwr_off}
create_isolation_rule -name iso_sw -from PD_SW -to PD_AON -pins {u_sw/q_out} \
  -isolation_condition {iso_en} -isolation_output low -isolation_target from
update_isolation_rules -names iso_sw -location parent
create_level_shifter_rule -name ls_sw -from PD_SW -to PD_AON -pins {u_sw/q_out}
update_level_shifter_rules -names ls_sw -location parent
create_level_shifter_rule -name ls_in -to PD_SW -pins {u_sw/clk u_sw/rst_n u_sw/d}
update_level_shifter_rules -names ls_in -location from
create_state_retention_rule -name ret_sw -domain PD_SW -secondary_power_net VDD_AON -restore_edge {!iso_en} -save_edge {iso_en}
end_design
"#;
    let bad_cpf = r#"
set_cpf_version 1.1
set_design top
create_power_nets -nets VDD_AON -voltage 1.8
create_power_nets -nets VDD_SW  -voltage 0.9
create_ground_nets -nets VSS

create_power_domain -name PD_AON -default
create_power_domain -name PD_SW  -instances {u_sw} -shutoff_condition {u_sw/q_out}
update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_SW  -primary_power_net VDD_SW  -primary_ground_net VSS

create_nominal_condition -name NC_AON -voltage 1.8 -state on
create_nominal_condition -name NC_SW  -voltage 0.9 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_mode -name PM_ON  -domain_conditions {PD_AON@NC_AON PD_SW@NC_SW}  -default
create_power_mode -name PM_OFF -domain_conditions {PD_AON@NC_AON PD_SW@NC_OFF}

create_power_switch_rule -name psw_bad -domain PD_SW -external_power_net VDD_AON -enable_condition_1 {!u_sw/q_out}
create_state_retention_rule -name ret_bad -domain PD_SW -restore_edge {iso_en} -save_edge {!iso_en}
end_design
"#;
    let dir = new_temp("cpf_e2e");
    let sv_path = dir.0.join("top.sv");
    let clean_path = dir.0.join("clean.cpf");
    let bad_path = dir.0.join("bad.cpf");
    std::fs::write(&sv_path, sv).unwrap();
    std::fs::write(&clean_path, clean_cpf).unwrap();
    std::fs::write(&bad_path, bad_cpf).unwrap();

    let sh_ok = Shell::new(Session::new(Vec::new(), dir.0.join("w_ok")), Flavor::Jg);
    sh_ok
        .eval(&format!(
            "analyze -sv {{{}}}; elaborate -top top; load_upf {{{}}} -cpf -version cpf2.0; check_lp -stage rtl",
            sv_path.display(),
            clean_path.display()
        ))
        .expect("cpf2.0 clean flow");
    let js_ok: serde_json::Value =
        serde_json::from_str(&sh_ok.eval("report_lp -json").unwrap().to_string()).unwrap();
    assert_eq!(js_ok["summary"]["errors"], 0, "{js_ok:#?}");
    assert_eq!(js_ok["summary"]["warnings"], 0, "{js_ok:#?}");

    let sh_bad = Shell::new(Session::new(Vec::new(), dir.0.join("w_bad")), Flavor::Jg);
    sh_bad
        .eval(&format!(
            "analyze -sv {{{}}}; elaborate -top top; load_upf {{{}}} -cpf -version cpf1.1; check_lp -stage rtl",
            sv_path.display(),
            bad_path.display()
        ))
        .expect("cpf1.1 bad flow");
    let ids: Vec<String> = sh_bad
        .session
        .borrow()
        .lp
        .violations
        .iter()
        .map(|v| v.rule_id.clone())
        .collect();
    assert!(ids.contains(&"ISO_MISSING".to_string()), "{ids:?}");
    assert!(ids.contains(&"LS_MISSING".to_string()), "{ids:?}");
    assert!(ids.contains(&"PSW_CTRL_UNPOWERED".to_string()), "{ids:?}");
}

#[test]
fn regression_cpf_hier_instances_transitive_domain_inheritance() {
    let sv = r#"
module leaf(input logic clk, input logic rst_n, input logic d, output logic y);
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) y <= 1'b0;
    else y <= d;
  end
endmodule
module mid(input logic clk, input logic rst_n, input logic d, output logic y);
  leaf u_leaf(.clk(clk), .rst_n(rst_n), .d(d), .y(y));
endmodule
module aon_sink(input logic y_in, output logic o);
  assign o = y_in;
endmodule
module top(input logic clk, input logic rst_n, input logic iso_en, input logic pwr_off, input logic d, output logic o);
  logic y_mid;
  mid      u_mid(.clk(clk), .rst_n(rst_n), .d(d), .y(y_mid));
  aon_sink u_aon(.y_in(y_mid), .o(o));
endmodule
"#;
    let cpf = r#"
set_cpf_version 2.0
set_design top
create_power_nets -nets VDD_AON -voltage 1.8
create_power_nets -nets VDD_SW  -voltage 0.9
create_ground_nets -nets VSS

create_power_domain -name PD_AON -default
create_power_domain -name PD_SW  -instances {u_mid} -shutoff_condition {pwr_off}
update_power_domain -name PD_AON -primary_power_net VDD_AON -primary_ground_net VSS
update_power_domain -name PD_SW  -primary_power_net VDD_SW  -primary_ground_net VSS

create_nominal_condition -name NC_AON -voltage 1.8 -state on
create_nominal_condition -name NC_SW  -voltage 0.9 -state on
create_nominal_condition -name NC_OFF -voltage 0.0 -state off
create_power_mode -name PM_ON  -domain_conditions {PD_AON@NC_AON PD_SW@NC_SW}  -default
create_power_mode -name PM_OFF -domain_conditions {PD_AON@NC_AON PD_SW@NC_OFF}

create_power_switch_rule -name psw_sw -domain PD_SW -external_power_net VDD_AON -enable_condition_1 {!pwr_off}
create_isolation_rule -name iso_sw -from PD_SW -to PD_AON -pins {u_mid/y} \
  -isolation_condition {iso_en} -isolation_output low -isolation_target from
update_isolation_rules -names iso_sw -location parent
create_level_shifter_rule -name ls_out -from PD_SW -to PD_AON -pins {u_mid/y}
update_level_shifter_rules -names ls_out -location parent
create_level_shifter_rule -name ls_in -to PD_SW -pins {u_mid/clk u_mid/rst_n u_mid/d}
update_level_shifter_rules -names ls_in -location from
create_state_retention_rule -name ret_sw -domain PD_SW -secondary_power_net VDD_AON -restore_edge {!iso_en} -save_edge {iso_en}
end_design
"#;
    let dir = new_temp("cpf_hier");
    let sv_path = dir.0.join("top.sv");
    let cpf_path = dir.0.join("hier.cpf");
    std::fs::write(&sv_path, sv).unwrap();
    std::fs::write(&cpf_path, cpf).unwrap();

    let sh = Shell::new(Session::new(Vec::new(), dir.0.join("w")), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv {{{}}}; elaborate -top top; load_upf {{{}}} -cpf -version cpf2.0; check_lp -stage rtl",
        sv_path.display(),
        cpf_path.display()
    ))
    .expect("cpf_hier flow");
    let js: serde_json::Value =
        serde_json::from_str(&sh.eval("report_lp -json").unwrap().to_string()).unwrap();
    assert_eq!(js["summary"]["errors"], 0, "{js:#?}");
    assert_eq!(js["summary"]["warnings"], 0, "{js:#?}");
}
