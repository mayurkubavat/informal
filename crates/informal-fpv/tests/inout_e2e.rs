//! End-to-end integration tests for SystemVerilog `inout` ports and tri-state buses across:
//! 1. Gate-PG (`LpStage::Netlist`): bidirectional `MACRO_PIN_SUPPLY_CROSSING` on Liberty `direction : inout`
//!    macro pins across leaf and hierarchical `inout` ports (`pg_stitch.rs`, `macro_pins.rs`, `cells_aon.rs`).
//! 2. `check_sec` & `check_sec -lec`: split `(pad$in, pad$oe, pad$out)` equivalence between behavioral RTL
//!    transceivers and gate-array `bufif1` implementations, including masked disabled-datapath immunity.
//! 3. `check_xprop`: split `(pad$oe, pad$out)` observation sinks, disabled-datapath X-masking, base-port waivers,
//!    and multi-driver internal bus contention `'x` detection.
//! 4. `check_lp` (`LpStage::Formal`): 2-copy power-aware corruption proofs (`corrupt_2copy.rs`) on top-level
//!    `inout` ports, plus opt-in `Request::{tristate_contention_checks, tristate_floating_checks}`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use informal_fpv::lp::{check_lp, load_upf, read_liberty, CheckLpOpts, LpStage, LpStatus};
use informal_fpv::sec::SecStatus;
use informal_fpv::session::{Clock, Kind, Session, Status};
use informal_fpv::xprop::XpropStatus;
use informal_fpv::{Flavor, Shell};

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
        "informal_inout_e2e_{tag}_{}_{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    TempDir(p)
}

fn all_engines() -> Option<Vec<std::sync::Arc<dyn informal_engine::Engine>>> {
    let mut e = informal_bmc::engines();
    e.extend(informal_bitwuzla::engines());
    if e.is_empty() {
        informal_engine::tool_missing("needs the in-house engines (CaDiCaL / Bitwuzla)");
        return None;
    }
    Some(e)
}

#[test]
fn gate_pg_macro_inout_pin_bidirectional_crossings_and_hierarchy() {
    let dir = new_temp("gate_pg_io");
    let lib_p = dir.0.join("macro_io.lib");
    let upf_p = dir.0.join("top.upf");
    let buggy_v = dir.0.join("buggy.v");
    let clean_v = dir.0.join("clean.v");

    std::fs::write(
        &lib_p,
        r#"
library (macro_io_lib) {
    delay_model : table_lookup;
    voltage_map (VDD, 0.90);
    voltage_map (VDDH, 1.10);
    voltage_map (VSS, 0.00);

    cell (BUF_X1) {
        pg_pin (VDD) { pg_type : primary_power; voltage_name : VDD; }
        pg_pin (VSS) { pg_type : primary_ground; voltage_name : VSS; }
        pin (A) { direction : input; related_power_pin : "VDD"; related_ground_pin : "VSS"; }
        pin (Y) { direction : output; function : "A"; related_power_pin : "VDD"; related_ground_pin : "VSS"; }
    }
    cell (SRAM_IO_MACRO) {
        is_macro_cell : true;
        pg_pin (VDD_MEM) { pg_type : primary_power; voltage_name : VDD; }
        pg_pin (VSS) { pg_type : primary_ground; voltage_name : VSS; }
        pin (DQ) { direction : inout; related_power_pin : "VDD_MEM"; related_ground_pin : "VSS"; }
    }
}
"#,
    )
    .unwrap();

    // Two different voltage rails (`VDD_LO` = 0.90 V in `PD_MEM`, `VDD_HI` = 1.10 V in `PD_TOP`)
    // so a single `SRAM_IO_MACRO` (`VDD_MEM = VDD_LO`) connected through a hierarchical `inout`
    // wrapper to both a `PD_TOP` driver AND a `PD_TOP` load triggers BOTH directions on `DQ`!
    std::fs::write(
        &upf_p,
        r#"
upf_version 2.1
set_design_top top_io_pg
create_supply_port VDD_HI -direction in
create_supply_port VDD_LO -direction in
create_supply_port VSS    -direction in
create_supply_net VDD_HI
create_supply_net VDD_LO
create_supply_net VSS
connect_supply_net VDD_HI -ports VDD_HI
connect_supply_net VDD_LO -ports VDD_LO
connect_supply_net VSS    -ports VSS
create_supply_set SS_HI -function {power VDD_HI} -function {ground VSS}
create_supply_set SS_LO -function {power VDD_LO} -function {ground VSS}
create_power_domain PD_TOP -include_scope -supply {primary SS_HI}
create_power_domain PD_MEM -elements {u_wrap} -supply {primary SS_LO}
add_power_state SS_HI -state {HI_ON -supply_expr {power == {FULL_ON 1.10} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_LO -state {LO_ON -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -simstate NORMAL}
"#,
    )
    .unwrap();

    // Buggy netlist: `u_wrap` exposes a hierarchical `inout wire io_bus` connected inside to
    // `SRAM_IO_MACRO u_sram (.DQ(io_bus), .VDD_MEM(VDD_LO), .VSS(VSS))`, while `PD_TOP` (1.10 V)
    // BOTH drives `io_bus` (`BUF_X1 u_drv`) AND reads `io_bus` (`BUF_X1 u_rcv`) without level shifters!
    std::fs::write(
        &buggy_v,
        r#"
module mem_wrap (
    input wire VDD_LO,
    input wire VSS,
    inout wire io_bus
);
    SRAM_IO_MACRO u_sram (.DQ(io_bus), .VDD_MEM(VDD_LO), .VSS(VSS));
endmodule

module top_io_pg (
    input  wire VDD_HI,
    input  wire VDD_LO,
    input  wire VSS,
    input  wire tx_in,
    output wire rx_out
);
    wire pad_bus;
    BUF_X1   u_drv  (.A(tx_in),   .Y(pad_bus), .VDD(VDD_HI), .VSS(VSS));
    mem_wrap u_wrap (.VDD_LO(VDD_LO), .VSS(VSS), .io_bus(pad_bus));
    BUF_X1   u_rcv  (.A(pad_bus), .Y(rx_out),  .VDD(VDD_HI), .VSS(VSS));
endmodule
"#,
    )
    .unwrap();

    let mut sb = Session::new(informal_bmc::engines(), dir.0.join("work_bug"));
    read_liberty(&mut sb, std::slice::from_ref(&lib_p)).unwrap();
    sb.design.files.push(buggy_v);
    sb.elaborate(Some("top_io_pg".into())).unwrap();
    assert!(
        sb.fast_gate.is_some(),
        "expected Liberty fast_gate path when SRAM_IO_MACRO body is absent"
    );
    load_upf(&mut sb, &upf_p, None, None, false).unwrap();
    check_lp(
        &mut sb,
        &CheckLpOpts {
            stage: LpStage::Netlist,
            ..Default::default()
        },
    )
    .unwrap();

    let macro_viols: Vec<_> = sb
        .lp
        .violations
        .iter()
        .filter(|v| v.rule_id == "MACRO_PIN_SUPPLY_CROSSING" && v.status == LpStatus::Open)
        .collect();
    assert_eq!(
        macro_viols.len(),
        2,
        "expected both input-receive and output-drive MACRO_PIN_SUPPLY_CROSSING on u_wrap/u_sram/DQ, got: {:#?}",
        sb.lp.violations
    );
    assert!(macro_viols
        .iter()
        .any(|v| v.message.contains("inout pin `DQ`") && v.message.contains("receives")));
    assert!(macro_viols
        .iter()
        .any(|v| v.message.contains("inout pin `DQ`") && v.message.contains("drives")));

    // Clean netlist: driver and receiver also run on `VDD_LO` (`SS_LO`) -> 0 macro crossing violations.
    std::fs::write(
        &clean_v,
        r#"
module mem_wrap (
    input wire VDD_LO,
    input wire VSS,
    input wire tx_in,
    output wire rx_out
);
    wire io_bus;
    BUF_X1        u_drv  (.A(tx_in),  .Y(io_bus), .VDD(VDD_LO), .VSS(VSS));
    SRAM_IO_MACRO u_sram (.DQ(io_bus), .VDD_MEM(VDD_LO), .VSS(VSS));
    BUF_X1        u_rcv  (.A(io_bus), .Y(rx_out), .VDD(VDD_LO), .VSS(VSS));
endmodule

module top_io_pg (
    input  wire VDD_HI,
    input  wire VDD_LO,
    input  wire VSS,
    input  wire tx_in,
    output wire rx_out
);
    mem_wrap u_wrap (.VDD_LO(VDD_LO), .VSS(VSS), .tx_in(tx_in), .rx_out(rx_out));
endmodule
"#,
    )
    .unwrap();

    let mut sc = Session::new(informal_bmc::engines(), dir.0.join("work_clean"));
    read_liberty(&mut sc, &[lib_p]).unwrap();
    sc.design.files.push(clean_v);
    sc.elaborate(Some("top_io_pg".into())).unwrap();
    load_upf(&mut sc, &upf_p, None, None, false).unwrap();
    check_lp(
        &mut sc,
        &CheckLpOpts {
            stage: LpStage::Netlist,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        !sc.lp
            .violations
            .iter()
            .any(|v| v.rule_id == "MACRO_PIN_SUPPLY_CROSSING" && v.status == LpStatus::Open),
        "expected 0 MACRO_PIN_SUPPLY_CROSSING on same-rail inout bus, got: {:#?}",
        sc.lp.violations
    );
}

#[test]
fn check_sec_and_lec_on_inout_transceivers_and_masked_disabled_datapaths() {
    let Some(eng) = all_engines() else { return };
    let dir = new_temp("sec_io");

    let spec_sv = dir.0.join("spec.sv");
    std::fs::write(
        &spec_sv,
        r#"
module xcvr_spec (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       oe_i,
    input  logic [7:0] tx_i,
    inout  wire  [7:0] pad,
    output logic [7:0] rx_o
);
    logic       oe_q;
    logic [7:0] tx_q;
    always_ff @(posedge clk) begin
        if (!rst_n) begin
            oe_q <= 1'b0;
            tx_q <= 8'd0;
            rx_o <= 8'd0;
        end else begin
            oe_q <= oe_i;
            tx_q <= tx_i;
            rx_o <= pad;
        end
    end
    assign pad = oe_q ? tx_q : 8'bz;
endmodule
"#,
    )
    .unwrap();

    // Gate-level `bufif1` implementation whose internal pre-driver toggles `~tx_q` when `oe_q == 0`:
    // proves equivalent on both `pad$oe` and `pad$out` because `pad$out` masks disabled bits to 0!
    let imp_ok_sv = dir.0.join("imp_ok.sv");
    std::fs::write(
        &imp_ok_sv,
        r#"
module xcvr_imp (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       oe_i,
    input  logic [7:0] tx_i,
    inout  wire  [7:0] pad,
    output logic [7:0] rx_o
);
    logic       oe_q;
    logic [7:0] tx_q;
    always_ff @(posedge clk) begin
        if (!rst_n) begin
            oe_q <= 1'b0;
            tx_q <= 8'd0;
            rx_o <= 8'd0;
        end else begin
            oe_q <= oe_i;
            tx_q <= tx_i;
            rx_o <= pad;
        end
    end
    wire [7:0] raw_pre = oe_q ? tx_q : ~tx_q;
    bufif1 u_drv [7:0] (pad, raw_pre, oe_q);
endmodule
"#,
    )
    .unwrap();

    let imp_bug_sv = dir.0.join("imp_bug.sv");
    std::fs::write(
        &imp_bug_sv,
        r#"
module xcvr_bug (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       oe_i,
    input  logic [7:0] tx_i,
    inout  wire  [7:0] pad,
    output logic [7:0] rx_o
);
    logic       oe_q;
    logic [7:0] tx_q;
    always_ff @(posedge clk) begin
        if (!rst_n) begin
            oe_q <= 1'b0;
            tx_q <= 8'd0;
            rx_o <= 8'd0;
        end else begin
            oe_q <= oe_i;
            tx_q <= tx_i;
            rx_o <= pad;
        end
    end
    bufif1 u_drv [7:0] (pad, tx_q ^ 8'h01, oe_q);
endmodule
"#,
    )
    .unwrap();

    let sh = Shell::new(Session::new(eng, dir.0.join("work")), Flavor::Jg);

    // 1. Sequential SEC: proves `sec_out_pad_oe`, `sec_out_pad_out`, and `sec_out_rx_o`.
    sh.eval(&format!(
        "check_sec -setup -spec {{{}}} -imp {{{}}} -clock clk -reset {{!rst_n}} -prove",
        spec_sv.display(),
        imp_ok_sv.display()
    ))
    .unwrap();
    {
        let s = sh.session.borrow();
        let names: Vec<&str> = s.sec.checks.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"sec_out_pad_oe"), "{names:?}");
        assert!(names.contains(&"sec_out_pad_out"), "{names:?}");
        assert!(names.contains(&"sec_out_rx_o"), "{names:?}");
        assert!(
            s.sec.checks.iter().all(|c| c.status == SecStatus::Proven),
            "{:#?}",
            s.sec.checks
        );
    }

    // 2. Combinational keypoint LEC (`-lec`): proves all state + split `inout` output cones.
    sh.eval(&format!(
        "check_sec -setup -lec -spec {{{}}} -imp {{{}}} -clock clk -prove",
        spec_sv.display(),
        imp_ok_sv.display()
    ))
    .unwrap();
    {
        let s = sh.session.borrow();
        assert!(
            s.sec.checks.iter().all(|c| c.status == SecStatus::Proven),
            "{:#?}",
            s.sec.checks
        );
    }

    // 3. Buggy `imp_bug`: `sec_out_pad_oe` stays Proven while `sec_out_pad_out` and `sec_out_rx_o` catch CEX!
    sh.eval(&format!(
        "check_sec -setup -spec {{{}}} -imp {{{}}} -clock clk -reset {{!rst_n}} -prove",
        spec_sv.display(),
        imp_bug_sv.display()
    ))
    .unwrap();
    {
        let s = sh.session.borrow();
        let chk = |n: &str| s.sec.checks.iter().find(|c| c.name == n).unwrap();
        assert_eq!(chk("sec_out_pad_oe").status, SecStatus::Proven);
        assert!(matches!(
            chk("sec_out_pad_out").status,
            SecStatus::Cex { .. }
        ));
        assert!(matches!(chk("sec_out_rx_o").status, SecStatus::Cex { .. }));
    }
}

#[test]
fn check_xprop_on_split_inout_ports_and_contending_internal_tristate_bus() {
    let Some(eng) = all_engines() else { return };
    let dir = new_temp("xprop_io");
    let sv = dir.0.join("xprop_io.sv");
    std::fs::write(
        &sv,
        r#"
module xprop_io (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       load_uninit,
    input  logic       oe_clean_i,
    input  logic [3:0] din,
    inout  wire  [3:0] clean_pad,
    inout  wire  [3:0] leaky_pad,
    input  logic       en_a,
    input  logic       en_b,
    input  logic [3:0] da,
    input  logic [3:0] db,
    output logic [3:0] bus_out
);
    logic       oe_q;
    logic [3:0] clean_q;
    logic [3:0] uninit_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            oe_q    <= 1'b0;
            clean_q <= 4'd0;
        end else begin
            oe_q    <= oe_clean_i;
            clean_q <= din;
        end
    end
    always_ff @(posedge clk) begin
        if (load_uninit)
            uninit_q <= din;
    end

    // `raw_clean_tx` reads `uninit_q` ONLY when `oe_q == 0`, so `clean_pad$out` is 100% X-clean!
    wire [3:0] raw_clean_tx = oe_q ? clean_q : uninit_q;
    assign clean_pad = oe_q ? raw_clean_tx : 4'bz;

    // `leaky_pad` drives `uninit_q` when `oe_q == 1` -> X leaks to `leaky_pad$out`!
    assign leaky_pad = oe_q ? uninit_q : 4'bz;

    // Internal multi-driver tri-state bus: contends to `'x` when `en_a && en_b && da != db`
    // and floats to `'z` when `!en_a && !en_b`!
    wire [3:0] ibus;
    assign ibus = en_a ? da : 4'bz;
    assign ibus = en_b ? db : 4'bz;
    assign bus_out = ibus;
endmodule
"#,
    )
    .unwrap();

    let sh = Shell::new(Session::new(eng, dir.0.join("work")), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv {{{}}}; elaborate -top xprop_io; clock clk; reset {{!rst_n}}; set_prove_time_limit 20s",
        sv.display()
    ))
    .unwrap();
    sh.eval("check_xprop -init -targets {outputs} -prove")
        .unwrap();

    {
        let s = sh.session.borrow();
        let item = |n: &str| {
            s.xprop
                .items
                .iter()
                .find(|i| i.name == n)
                .unwrap_or_else(|| panic!("missing xprop item `{n}` in {:?}", s.xprop.items))
        };
        assert_eq!(item("xprop_io.clean_pad$oe").status, XpropStatus::Proven);
        assert_eq!(
            item("xprop_io.clean_pad$out").status,
            XpropStatus::Proven,
            "masked io$out must not report false X-leak when uninit_q is only selected while oe_q == 0"
        );
        assert_eq!(item("xprop_io.leaky_pad$oe").status, XpropStatus::Proven);
        assert!(matches!(
            item("xprop_io.leaky_pad$out").status,
            XpropStatus::Cex { .. }
        ));
        assert!(matches!(
            item("xprop_io.bus_out").status,
            XpropStatus::Cex { .. }
        ));
    }

    // Waiving base port `leaky_pad` waives `leaky_pad$out` (and `leaky_pad$oe`):
    sh.eval("check_xprop -waiver -add -instance leaky_pad -comment {external bus qualified}")
        .unwrap();
    {
        let s = sh.session.borrow();
        let leaky_out = s
            .xprop
            .items
            .iter()
            .find(|i| i.name == "xprop_io.leaky_pad$out")
            .unwrap();
        assert_eq!(leaky_out.status, XpropStatus::Waived);
    }
}

#[test]
fn formal_lp_2copy_corruption_on_top_inout_ports_and_tristate_checks() {
    let dir = new_temp("lp_io");
    let upf_p = dir.0.join("io_lp.upf");
    let clean_sv = dir.0.join("io_lp_clean.sv");
    let bug_sv = dir.0.join("io_lp_bug.sv");

    std::fs::write(
        &upf_p,
        r#"
upf_version 2.1
create_supply_port VDD_AON -direction in
create_supply_port VSS     -direction in
create_supply_net VDD_AON
create_supply_net VDD_SW
create_supply_net VSS
connect_supply_net VDD_AON -ports VDD_AON
connect_supply_net VSS     -ports VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
create_power_domain PD_SW  -elements {u_sw} -supply {primary SS_SW}
create_power_switch PSW_SW -domain PD_SW \
    -input_supply_port {VDD_IN VDD_AON} -output_supply_port {VDD_OUT VDD_SW} \
    -control_port {SLEEP pwr_off} \
    -on_state {ON VDD_IN {!SLEEP}} -off_state {OFF {SLEEP}}
add_power_state SS_AON -state {AON_ON -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW \
    -state {SW_ON  -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -logic_expr {!pwr_off} -simstate NORMAL} \
    -state {SW_OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {pwr_off} -simstate CORRUPT}
set_isolation ISO_SW -domain PD_SW -applies_to outputs -clamp_value 0 \
    -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent
"#,
    )
    .unwrap();

    // Clean SoC: PMU asserts `iso_en` whenever `pwr_off` is asserted, clamping BOTH `sw_oe` and `sw_tx`
    // before driving the top-level AON `inout` pad!
    let pmu_and_leaf = r#"
module sw_tx_blk (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       req_oe,
    input  logic [3:0] req_tx,
    output logic       sw_oe,
    output logic [3:0] sw_tx
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            sw_oe <= 1'b0;
            sw_tx <= 4'd0;
        end else begin
            sw_oe <= req_oe;
            sw_tx <= req_tx;
        end
    end
endmodule
"#;

    std::fs::write(
        &clean_sv,
        format!(
            r#"{pmu_and_leaf}
module io_lp_soc (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       sleep_req,
    input  logic       req_oe,
    input  logic [3:0] req_tx,
    inout  wire  [3:0] pad_io,
    output logic [3:0] rx_q
);
    logic [1:0] pmu_st;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) pmu_st <= 2'd0;
        else begin
            case (pmu_st)
                2'd0: if (sleep_req)  pmu_st <= 2'd1; // isolate first
                2'd1:                 pmu_st <= 2'd2; // then power off
                2'd2: if (!sleep_req) pmu_st <= 2'd1; // power on while isolated
                2'd3:                 pmu_st <= 2'd0;
            endcase
            if (pmu_st == 2'd1 && !sleep_req) pmu_st <= 2'd0;
        end
    end
    wire iso_en  = (pmu_st != 2'd0);
    wire pwr_off = (pmu_st == 2'd2);

    wire       sw_oe;
    wire [3:0] sw_tx;
    sw_tx_blk u_sw (.clk(clk), .rst_n(rst_n), .req_oe(req_oe), .req_tx(req_tx), .sw_oe(sw_oe), .sw_tx(sw_tx));

    wire       iso_oe = iso_en ? 1'b0 : sw_oe;
    wire [3:0] iso_tx = iso_en ? 4'd0 : sw_tx;
    assign pad_io = iso_oe ? iso_tx : 4'bz;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) rx_q <= 4'd0;
        else        rx_q <= pad_io;
    end
endmodule
"#
        ),
    )
    .unwrap();

    // Buggy SoC: PMU drops `iso_en` while `PD_SW` is powered off (`pmu_st == 2'd2`),
    // so power-off corruption of `sw_oe` leaks directly onto `pad_io$oe`!
    std::fs::write(
        &bug_sv,
        format!(
            r#"{pmu_and_leaf}
module io_lp_soc (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       sleep_req,
    input  logic       req_oe,
    input  logic [3:0] req_tx,
    inout  wire  [3:0] pad_io,
    output logic [3:0] rx_q
);
    logic [1:0] pmu_st;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) pmu_st <= 2'd0;
        else begin
            case (pmu_st)
                2'd0: if (sleep_req)  pmu_st <= 2'd1;
                2'd1:                 pmu_st <= 2'd2;
                2'd2: if (!sleep_req) pmu_st <= 2'd1;
                2'd3:                 pmu_st <= 2'd0;
            endcase
            if (pmu_st == 2'd1 && !sleep_req) pmu_st <= 2'd0;
        end
    end
    wire iso_en  = (pmu_st == 2'd1);
    wire pwr_off = (pmu_st == 2'd2);

    wire       sw_oe;
    wire [3:0] sw_tx;
    sw_tx_blk u_sw (.clk(clk), .rst_n(rst_n), .req_oe(req_oe), .req_tx(req_tx), .sw_oe(sw_oe), .sw_tx(sw_tx));

    assign pad_io = sw_oe ? sw_tx : 4'bz;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) rx_q <= 4'd0;
        else        rx_q <= pad_io;
    end
endmodule
"#
        ),
    )
    .unwrap();

    let run_lp_formal = |sv_path: PathBuf, tag: &str| -> Session {
        let mut s = Session::new(informal_bmc::engines(), dir.0.join(tag));
        s.design.files.push(sv_path);
        s.elaborate(Some("io_lp_soc".into())).unwrap();
        s.clocks.push(Clock {
            signal: "clk".into(),
            edge: "posedge",
            factor: 1,
            phase: 0,
        });
        s.reset = Some("!rst_n".into());
        load_upf(&mut s, &upf_p, None, None, false).unwrap();
        check_lp(
            &mut s,
            &CheckLpOpts {
                stage: LpStage::Formal,
                prove_corruption: true,
                ..Default::default()
            },
        )
        .unwrap();
        s
    };

    let s_clean = run_lp_formal(clean_sv.clone(), "work_lp_clean");
    let corrupt_props: Vec<_> = s_clean
        .props
        .iter()
        .filter(|p| {
            p.task == "LowPower" && p.kind == Kind::Assert && p.name.contains("lp_corrupt_")
        })
        .collect();
    assert!(
        corrupt_props.iter().any(|p| p.name.contains("pad_io_oe")),
        "{corrupt_props:#?}"
    );
    assert!(
        corrupt_props.iter().any(|p| p.name.contains("pad_io_out")),
        "{corrupt_props:#?}"
    );
    assert!(
        corrupt_props
            .iter()
            .all(|p| matches!(p.status, Status::Proven { .. })),
        "expected all 2-copy LP corruption checks on clean inout SoC to prove: {corrupt_props:#?}"
    );

    let s_bug = run_lp_formal(bug_sv, "work_lp_bug");
    let p_oe_bug = s_bug
        .props
        .iter()
        .find(|p| {
            p.task == "LowPower" && p.name.contains("lp_corrupt_") && p.name.contains("pad_io_oe")
        })
        .expect("lp_corrupt_pad_io_oe");
    assert!(
        matches!(p_oe_bug.status, Status::Cex { .. }),
        "unisolated sw_oe driving pad_io must fail 2-copy corruption check: {p_oe_bug:#?}"
    );

    // Also verify `informal_svlang::lower` wires `tristate_contention_checks` & `tristate_floating_checks`:
    let tri_sv = dir.0.join("tri_chk.sv");
    std::fs::write(
        &tri_sv,
        r#"
module tri_chk (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en0,
    input  logic       en1,
    input  logic [3:0] d0,
    input  logic [3:0] d1,
    output logic [3:0] y
);
    wire [3:0] bus;
    assign bus = en0 ? d0 : 4'bz;
    assign bus = en1 ? d1 : 4'bz;
    always_ff @(posedge clk) y <= rst_n ? bus : 4'd0;
endmodule
"#,
    )
    .unwrap();
    let lowered = informal_svlang::lower(&informal_svlang::Request {
        files: vec![tri_sv],
        top: Some("tri_chk".into()),
        reset: Some("!rst_n".into()),
        tristate_contention_checks: true,
        tristate_floating_checks: true,
        ..Default::default()
    });
    assert!(lowered.ok, "{:?}", lowered.errors);
    let pnames: Vec<&str> = lowered.properties.iter().map(|p| p.name.as_str()).collect();
    assert!(
        pnames.contains(&"tri_chk.bus._tri_contention"),
        "{pnames:?}"
    );
    assert!(pnames.contains(&"tri_chk.bus._tri_floating"), "{pnames:?}");
}
