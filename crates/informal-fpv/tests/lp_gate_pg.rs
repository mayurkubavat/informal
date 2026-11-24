//! Stage 3 (Post-Synthesis Gate Netlist + Liberty), Stage 4 (Physical PG),
//! 100k-cell fast-path elaboration, and LP-LEC integration tests (WS5).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use informal_fpv::lp::{check_lp, load_upf, read_liberty, CheckLpOpts, LpStage, LpStatus};
use informal_fpv::sec::{setup_sec, SecSetup, SecStatus};
use informal_fpv::session::{Clock, Session};

static SEQ: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn new_temp(tag: &str) -> TempDir {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "informal_lp_gate_pg_{tag}_{}_{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    TempDir(p)
}

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/lp_e2e/fixtures")
        .join(rel)
}

const MINI_LIB: &str = r#"
library (test_lp) {
    delay_model : table_lookup;
    voltage_map (VDD, 0.90);
    voltage_map (VDDH, 1.10);
    voltage_map (VDD_BK, 0.90);
    voltage_map (VSS, 0.00);

    cell (BUF_X1) {
        pg_pin (VDD) { pg_type : primary_power; voltage_name : VDD; }
        pg_pin (VSS) { pg_type : primary_ground; voltage_name : VSS; }
        pin (A) { direction : input; related_power_pin : "VDD"; related_ground_pin : "VSS"; }
        pin (Y) { direction : output; function : "A"; related_power_pin : "VDD"; related_ground_pin : "VSS"; }
    }
    cell (INV_X1) {
        pg_pin (VDD) { pg_type : primary_power; voltage_name : VDD; }
        pg_pin (VSS) { pg_type : primary_ground; voltage_name : VSS; }
        pin (A) { direction : input; related_power_pin : "VDD"; related_ground_pin : "VSS"; }
        pin (Y) { direction : output; function : "!A"; related_power_pin : "VDD"; related_ground_pin : "VSS"; }
    }
    cell (AON_BUF_X1) {
        always_on : true;
        pg_pin (VDD_BK) { pg_type : backup_power; voltage_name : VDD_BK; }
        pg_pin (VSS) { pg_type : primary_ground; voltage_name : VSS; }
        pin (A) { direction : input; related_power_pin : "VDD_BK"; related_ground_pin : "VSS"; }
        pin (Y) { direction : output; always_on : true; function : "A"; related_power_pin : "VDD_BK"; related_ground_pin : "VSS"; }
    }
    cell (ISO_LO_X1) {
        is_isolation_cell : true;
        isolation_enable_condition : "ISO";
        pg_pin (VDD_BK) { pg_type : backup_power; voltage_name : VDD_BK; }
        pg_pin (VSS) { pg_type : primary_ground; voltage_name : VSS; }
        pin (A) { direction : input; isolation_cell_data_pin : true; related_power_pin : "VDD_BK"; }
        pin (ISO) { direction : input; isolation_cell_enable_pin : true; related_power_pin : "VDD_BK"; }
        pin (Y) { direction : output; function : "A & !ISO"; related_power_pin : "VDD_BK"; }
    }
    cell (PSW_HDR_X1) {
        switch_cell_type : coarse_grain;
        pg_pin (VDD_IN) { pg_type : primary_power; voltage_name : VDD; }
        pg_pin (VDD_OUT) { pg_type : internal_power; voltage_name : VDD; switch_function : "!SLEEP"; pg_function : "VDD_IN"; }
        pg_pin (VSS) { pg_type : primary_ground; voltage_name : VSS; }
        pin (SLEEP) { direction : input; switch_pin : true; related_power_pin : "VDD_IN"; }
        pin (SLEEPOUT) { direction : output; function : "SLEEP"; related_power_pin : "VDD_IN"; }
    }
}
"#;

const MINI_UPF: &str = r#"
upf_version 2.1
set_design_top top_pg
create_supply_port VDD_AON -direction in
create_supply_port VDD_BK  -direction in
create_supply_port VSS     -direction in
create_supply_net VDD_AON
create_supply_net VDD_BK
create_supply_net VDD_SW
create_supply_net VSS
connect_supply_net VDD_AON -ports VDD_AON
connect_supply_net VDD_BK  -ports VDD_BK
connect_supply_net VSS     -ports VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_BK  -function {power VDD_BK}  -function {ground VSS}
create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
create_power_domain PD_SW  -elements {u_sw} -supply {primary SS_SW}
create_power_switch PSW_SW -domain PD_SW \
    -input_supply_port {VDD_IN VDD_AON} -output_supply_port {VDD_OUT VDD_SW} \
    -control_port {SLEEP slp} -ack_port {SLEEPOUT slp_ack} \
    -on_state {ON VDD_IN {!SLEEP}} -off_state {OFF {SLEEP}}
add_power_state SS_AON -state {AON_ON -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_BK  -state {BK_ON  -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW \
    -state {SW_ON  -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -logic_expr {!slp} -simstate NORMAL} \
    -state {SW_OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {slp} -simstate CORRUPT}
set_isolation ISO_SW -domain PD_SW -applies_to outputs -clamp_value 0 \
    -isolation_supply_set SS_BK -isolation_signal iso_en -isolation_sense high -location parent
"#;

#[test]
fn liberty_malformed_reports_exact_line_14() {
    let dir = new_temp("malformed");
    let mut s = Session::new(informal_bmc::engines(), dir.0.join("work"));
    let err = read_liberty(&mut s, &[fixture("f7_f10/f9/malformed_syntax.lib")]).unwrap_err();
    assert!(err.contains("14"), "expected line 14 in error, got: {err}");
}

#[test]
fn gate_fast_100k_cells_clean_and_buggy() {
    let dir = new_temp("scale100k");
    let lib_p = dir.0.join("mini.lib");
    let upf_p = dir.0.join("top.upf");
    let clean_v = dir.0.join("clean_100k.v");
    let buggy_v = dir.0.join("buggy_100k.v");
    std::fs::write(&lib_p, MINI_LIB).unwrap();
    std::fs::write(&upf_p, MINI_UPF).unwrap();

    let gen_netlist = |n: usize, buggy: bool| -> String {
        let mut s = String::with_capacity(n * 80);
        s.push_str("module sw_blk (input wire VDD_SW, input wire VSS, input wire a, output wire y");
        if buggy {
            s.push_str(", input wire VDD_AON, output wire bad_o, output wire flt_o");
        }
        s.push_str(");\n    wire n_0;\n    assign n_0 = a;\n");
        for chunk in (1..=n).step_by(16) {
            let end = (chunk + 16).min(n + 1);
            s.push_str("    wire ");
            for k in chunk..end {
                if k > chunk {
                    s.push_str(", ");
                }
                s.push_str(&format!("n_{k}"));
            }
            s.push_str(";\n");
        }
        for i in 0..n {
            let cell = if i % 2 == 0 { "BUF_X1" } else { "INV_X1" };
            s.push_str(&format!(
                "    {cell} u_c_{i} (.A(n_{i}), .Y(n_{}), .VDD(VDD_SW), .VSS(VSS));\n",
                i + 1
            ));
        }
        s.push_str(&format!("    assign y = n_{n};\n"));
        if buggy {
            s.push_str("    BUF_X1 u_pg_mis (.A(n_0), .Y(bad_o), .VDD(VDD_AON), .VSS(VSS));\n");
            s.push_str("    BUF_X1 u_pg_flt (.A(n_0), .Y(flt_o), .VSS(VSS));\n");
        }
        s.push_str("endmodule\nmodule top_pg (input wire VDD_AON, input wire VDD_BK, input wire VSS, input wire slp, input wire iso_en, input wire din, output wire dout");
        if buggy {
            s.push_str(", output wire bad_o, output wire flt_o, output wire uniso_o");
        }
        s.push_str(");\n    wire VDD_SW, slp_ack, aon_iso, sw_y;\n");
        s.push_str("    PSW_HDR_X1 u_psw (.SLEEP(slp), .SLEEPOUT(slp_ack), .VDD_IN(VDD_AON), .VDD_OUT(VDD_SW), .VSS(VSS));\n");
        s.push_str("    AON_BUF_X1 u_aon (.A(iso_en), .Y(aon_iso), .VDD_BK(VDD_BK), .VSS(VSS));\n");
        if buggy {
            s.push_str("    sw_blk u_sw (.VDD_SW(VDD_SW), .VSS(VSS), .a(din), .y(sw_y), .VDD_AON(VDD_AON), .bad_o(bad_o), .flt_o(flt_o));\n");
            s.push_str("    BUF_X1 u_uniso (.A(sw_y), .Y(uniso_o), .VDD(VDD_AON), .VSS(VSS));\n");
        } else {
            s.push_str("    sw_blk u_sw (.VDD_SW(VDD_SW), .VSS(VSS), .a(din), .y(sw_y));\n");
        }
        s.push_str("    ISO_LO_X1 u_iso (.A(sw_y), .ISO(aon_iso), .Y(dout), .VDD_BK(VDD_BK), .VSS(VSS));\nendmodule\n");
        s
    };

    std::fs::write(&clean_v, gen_netlist(100_000, false)).unwrap();
    std::fs::write(&buggy_v, gen_netlist(10_000, true)).unwrap();

    let t0 = Instant::now();
    let mut s = Session::new(informal_bmc::engines(), dir.0.join("work_clean"));
    read_liberty(&mut s, std::slice::from_ref(&lib_p)).unwrap();
    s.design.files.push(clean_v);
    s.elaborate(Some("top_pg".into())).unwrap();
    assert!(
        s.fast_gate.is_some(),
        "expected fast_gate path for 100k netlist"
    );
    load_upf(&mut s, &upf_p, None, None, false).unwrap();
    check_lp(
        &mut s,
        &CheckLpOpts {
            stage: LpStage::Netlist,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        s.lp.violations.iter().all(|v| v.status != LpStatus::Open),
        "expected 0 open Stage 3 violations on 100k clean: {:?}",
        s.lp.violations
    );
    check_lp(
        &mut s,
        &CheckLpOpts {
            stage: LpStage::Pg,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        s.lp.violations.iter().all(|v| v.status != LpStatus::Open),
        "expected 0 open Stage 4 violations on 100k clean: {:?}",
        s.lp.violations
    );
    let elapsed_ms = t0.elapsed().as_millis();
    assert!(
        elapsed_ms < 10_000,
        "100k-cell Stage 3+4 took {elapsed_ms} ms (>= 10000 ms)"
    );

    // Buggy netlist detects ISO_CELL_MISSING, PG_SUPPLY_MISMATCH, PG_PIN_UNCONNECTED.
    let mut sb = Session::new(informal_bmc::engines(), dir.0.join("work_buggy"));
    read_liberty(&mut sb, &[lib_p]).unwrap();
    sb.design.files.push(buggy_v);
    sb.elaborate(Some("top_pg".into())).unwrap();
    load_upf(&mut sb, &upf_p, None, None, false).unwrap();
    check_lp(
        &mut sb,
        &CheckLpOpts {
            stage: LpStage::Netlist,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(sb
        .lp
        .violations
        .iter()
        .any(|v| v.rule_id == "ISO_CELL_MISSING" && v.status == LpStatus::Open));
    check_lp(
        &mut sb,
        &CheckLpOpts {
            stage: LpStage::Pg,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(sb
        .lp
        .violations
        .iter()
        .any(|v| v.rule_id == "PG_SUPPLY_MISMATCH" && v.status == LpStatus::Open));
    assert!(sb
        .lp
        .violations
        .iter()
        .any(|v| v.rule_id == "PG_PIN_UNCONNECTED" && v.status == LpStatus::Open));
}

#[test]
fn lp_lec_proves_clean_and_catches_inverted_clamp() {
    let dir = new_temp("lec");
    let std_v = dir.0.join("stdcells.v");
    std::fs::write(
        &std_v,
        r#"
module INV_X1 (input wire A, output wire Y, input wire VDD, input wire VSS);
    assign Y = ~A;
endmodule
module AON_BUF_X1 (input wire A, output wire Y, input wire VDD_BK, input wire VSS);
    assign Y = A;
endmodule
module ISO_LO_X1 (input wire A, input wire ISO, output wire Y, input wire VDD_BK, input wire VSS);
    assign Y = ISO ? 1'b0 : A;
endmodule
module PSW_HDR_X1 (input wire SLEEP, output wire SLEEPOUT, input wire VDD_IN, output wire VDD_OUT, input wire VSS);
    assign SLEEPOUT = SLEEP;
    assign VDD_OUT = SLEEP ? 1'b0 : VDD_IN;
endmodule
"#,
    )
    .unwrap();

    let rtl = fixture("f7_f10/f9/lec_rtl.sv");
    let gate_ok = fixture("f7_f10/f9/lec_gate_ok.v");
    let gate_bad = fixture("f7_f10/f9/lec_gate_bad_clamp.v");
    let extra_v = fixture("f7_f10/f9/extra_stdcells.v");
    let upf = fixture("f7_f10/f9/lec.upf");

    let mut s = Session::new(informal_bmc::engines(), dir.0.join("work_lec"));
    s.design.files.push(rtl.clone());
    s.elaborate(Some("lec_top".into())).unwrap();
    s.clocks.push(Clock {
        signal: "clk".into(),
        edge: "posedge",
        factor: 1,
        phase: 0,
    });
    s.reset = Some("!rst_n".into());
    load_upf(&mut s, &upf, None, None, false).unwrap();

    setup_sec(
        &mut s,
        SecSetup {
            lec: true,
            spec_files: vec![rtl.clone()],
            spec_top: Some("lec_top".into()),
            imp_files: vec![std_v.clone(), gate_ok],
            imp_top: Some("lec_top".into()),
            clock: Some("clk".into()),
            reset: Some("!rst_n".into()),
            ..Default::default()
        },
    )
    .unwrap();
    check_lp(
        &mut s,
        &CheckLpOpts {
            lec: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!s.sec.checks.is_empty());
    assert!(s
        .sec
        .checks
        .iter()
        .all(|c| matches!(c.status, SecStatus::Proven)));

    setup_sec(
        &mut s,
        SecSetup {
            lec: true,
            spec_files: vec![rtl],
            spec_top: Some("lec_top".into()),
            imp_files: vec![std_v, extra_v, gate_bad],
            imp_top: Some("lec_top".into()),
            clock: Some("clk".into()),
            reset: Some("!rst_n".into()),
            ..Default::default()
        },
    )
    .unwrap();
    check_lp(
        &mut s,
        &CheckLpOpts {
            lec: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(s
        .sec
        .checks
        .iter()
        .any(|c| c.name.contains("dout") && matches!(c.status, SecStatus::Cex { .. })));
}
