//! End-to-end integration tests for formal connectivity, coverage/UNR,
//! proof caching, and blackboxing:
//! 1. Source-less `-bbox_m` (on `analyze` and `elaborate`) + `-bbox_mul` / `-bbox_a`
//! 2. Content-addressed proof cache (`set_prove_cache`, `set_prove_cache_path`, `set_prove_cache_mode`)
//! 3. Formal Connectivity App (`check_conn` with OpenTitan `CONNECTION`/`CONDITION`/`LATENCY` CSVs)
//! 4. Formal Coverage & UNR App (`check_cov` COI observability, branch/toggle UNR, and waivers)

use informal_fpv::cov::{CovModel, CovStatus};
use informal_fpv::{Flavor, Session, Shell, Status};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("informal-conncov-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn engines() -> Option<Vec<std::sync::Arc<dyn informal_engine::Engine>>> {
    let e = informal_bmc::engines();
    if e.is_empty() {
        informal_engine::tool_missing("needs the in-house engines (CaDiCaL)");
        return None;
    }
    Some(e)
}

#[test]
fn sourceless_blackbox_analyze_and_auto_boxing_end_to_end() {
    let Some(eng) = engines() else { return };
    let dir = temp_dir("bbox");
    let sv = dir.join("top.sv");
    std::fs::write(
        &sv,
        "\
module top(
    input logic clk,
    input logic rst_n,
    input logic [15:0] a,
    input logic [15:0] b,
    input logic [5:0] addr,
    output logic [7:0] safe_cnt,
    output logic [7:0] sram_q,
    output logic [15:0] prod_q
);
    logic [7:0] big_arr [0:63];
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n)
            safe_cnt <= 8'd0;
        else if (safe_cnt < 8'd10)
            safe_cnt <= safe_cnt + 8'd1;
    end
    always_ff @(posedge clk) begin
        big_arr[addr] <= a[7:0];
    end
    // `prim_ram_1p` has NO module definition anywhere in the compilation!
    prim_ram_1p #(.Width(8), .Depth(64)) u_ram (
        .clk_i(clk),
        .req_i(1'b1),
        .addr_i(addr),
        .wdata_i(a[7:0]),
        .rdata_o(sram_q)
    );
    assign prod_q = (a * b) ^ {8'd0, big_arr[addr]};
endmodule
",
    )
    .unwrap();

    let sh = Shell::new(Session::new(eng, dir.join("work")), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv09 {{{}}} -bbox_m {{prim_ram_1p unused_macro_stub}}",
        sv.display()
    ))
    .unwrap();
    sh.eval("elaborate -top top -bbox_mul 16 -bbox_a 32; clock clk; reset {!rst_n}; set_prove_time_limit 30s")
        .unwrap();
    sh.eval("assert -name a_safe {safe_cnt <= 8'd10}").unwrap();
    sh.eval("assert -name a_sram_zero {sram_q == 8'd0}")
        .unwrap();
    sh.eval("assert -name a_prod_zero {prod_q == 16'd0}")
        .unwrap();
    sh.eval("prove -all").unwrap();

    assert_eq!(sh.eval("get_status a_safe").unwrap(), "proven");
    assert_eq!(sh.eval("get_status a_sram_zero").unwrap(), "cex");
    assert_eq!(sh.eval("get_status a_prod_zero").unwrap(), "cex");
}

#[test]
fn prove_cache_hits_and_invalidates_on_cone_or_assumption_change() {
    let Some(eng) = engines() else { return };
    let dir = temp_dir("cache");
    let cache_dir = dir.join("shared_cache");
    let sv = dir.join("cnt.sv");
    std::fs::write(
        &sv,
        "\
module cnt(input logic clk, input logic rst_n, input logic en, output logic [3:0] q);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n)
            q <= 4'd0;
        else if (en && q < 4'd7)
            q <= q + 4'd1;
    end
endmodule
",
    )
    .unwrap();

    // Run 1: populate proof cache with both a Proven assertion and an Unreachable cover.
    let sh1 = Shell::new(Session::new(eng.clone(), dir.join("w1")), Flavor::Jg);
    sh1.eval(&format!(
        "set_prove_cache on; set_prove_cache_path {{{}}}; analyze -sv {{{}}}; elaborate -top cnt; clock clk; reset {{!rst_n}}",
        cache_dir.display(),
        sv.display()
    ))
    .unwrap();
    sh1.eval("assert -name a_bound {q <= 4'd7}").unwrap();
    sh1.eval("cover -name c_unr {q == 4'd12}").unwrap();
    sh1.eval("prove -all").unwrap();
    assert_eq!(sh1.eval("get_status a_bound").unwrap(), "proven");
    assert_eq!(sh1.eval("get_status c_unr").unwrap(), "unreachable");
    {
        let s1 = sh1.session.borrow();
        assert!(
            matches!(&s1.props[0].status, Status::Proven { engine, .. } if !engine.starts_with("cache("))
        );
    }

    // Run 2: fresh session pointing at the same cache directory -> instant cache hits!
    let sh2 = Shell::new(Session::new(eng.clone(), dir.join("w2")), Flavor::Jg);
    sh2.eval(&format!(
        "set_prove_cache on; set_prove_cache_mode read_only; set_prove_cache_path {{{}}}; analyze -sv {{{}}}; elaborate -top cnt; clock clk; reset {{!rst_n}}",
        cache_dir.display(),
        sv.display()
    ))
    .unwrap();
    sh2.eval("assert -name a_bound {q <= 4'd7}").unwrap();
    sh2.eval("cover -name c_unr {q == 4'd12}").unwrap();
    sh2.eval("assert -name a_diff {q <= 4'd8}").unwrap();
    sh2.eval("prove -all").unwrap();
    {
        let s2 = sh2.session.borrow();
        assert!(
            matches!(&s2.props[0].status, Status::Proven { engine, ms: 0 } if engine.starts_with("cache(")),
            "expected cache hit on a_bound, got {:?}",
            s2.props[0].status
        );
        assert!(
            matches!(&s2.props[1].status, Status::Unreachable { engine, ms: 0 } if engine.starts_with("cache(")),
            "expected cache hit on c_unr, got {:?}",
            s2.props[1].status
        );
        assert!(
            matches!(&s2.props[2].status, Status::Proven { engine, .. } if !engine.starts_with("cache(")),
            "different property cone must miss cache, got {:?}",
            s2.props[2].status
        );
    }
}

#[test]
fn check_conn_proves_and_catches_bugs_on_opentitan_matrix_and_pipeline_csv() {
    let Some(eng) = engines() else { return };
    let dir = temp_dir("conn");
    let sv = dir.join("soc_top.sv");
    std::fs::write(
        &sv,
        "\
module sub_src(input logic [3:0] raw_i, output logic [3:0] sig_o);
    assign sig_o = raw_i;
endmodule

module sub_dst(input logic [3:0] sig_i, output logic [3:0] obs_o);
    assign obs_o = sig_i;
endmodule

module soc_top(
    input logic clk,
    input logic rst_n,
    input logic scan_en,
    input logic [1:0] mode,
    input logic [3:0] raw_a,
    input logic [3:0] raw_b
);
    logic [3:0] a_wire, mux_wire, pipe_q, broken_wire;
    sub_src u_src(.raw_i(raw_a), .sig_o(a_wire));
    assign mux_wire = ((scan_en && mode == 2'b01) || (!scan_en && mode == 2'b10)) ? a_wire : raw_b;
    assign broken_wire = scan_en ? a_wire : (a_wire ^ 4'h1);
    sub_dst u_mux_dst(.sig_i(mux_wire), .obs_o());
    sub_dst u_pipe_dst(.sig_i(pipe_q), .obs_o());
    sub_dst u_bad_dst(.sig_i(broken_wire), .obs_o());

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n)
            pipe_q <= 4'd0;
        else
            pipe_q <= a_wire;
    end
endmodule
",
    )
    .unwrap();

    let csv = dir.join("conn.csv");
    std::fs::write(
        &csv,
        "\
# OpenTitan-format connectivity spec
CONNECTION, mux_conn, soc_top.u_src, sig_o, soc_top.u_mux_dst, sig_i
CONDITION, , scan_en, 1'b1, 1'b0
CONDITION, soc_top, mode, 2'b01, 2'b10

CONNECTION, pipe_conn, u_src, sig_o, u_pipe_dst, sig_i
LATENCY, 1, 1

CONNECTION, bad_conn, u_src, sig_o, u_bad_dst, sig_i
",
    )
    .unwrap();

    let sh = Shell::new(Session::new(eng, dir.join("work")), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv {{{}}}; elaborate -top soc_top; clock clk; reset {{!rst_n}}; set_prove_time_limit 30s",
        sv.display()
    ))
    .unwrap();
    sh.eval(&format!("check_conn -load {{{}}}", csv.display()))
        .unwrap();
    assert_eq!(
        sh.eval("check_conn -list").unwrap(),
        "Connectivity::mux_conn Connectivity::pipe_conn Connectivity::bad_conn"
    );
    sh.eval("prove -task Connectivity").unwrap();
    let rep = sh.eval("check_conn -report").unwrap();
    assert!(rep.contains("Connectivity::mux_conn"));
    assert_eq!(
        sh.eval("get_status Connectivity::mux_conn").unwrap(),
        "proven"
    );
    assert_eq!(
        sh.eval("get_status Connectivity::pipe_conn").unwrap(),
        "proven"
    );
    assert_eq!(sh.eval("get_status Connectivity::bad_conn").unwrap(), "cex");
}

#[test]
fn check_cov_measures_coi_observability_branch_toggle_unr_and_waivers() {
    let Some(eng) = engines() else { return };
    let dir = temp_dir("cov");
    let sv = dir.join("dut.sv");
    std::fs::write(
        &sv,
        "\
module dut(
    input logic clk,
    input logic rst_n,
    input logic [1:0] mode,
    output logic checked_q,
    output logic shadow_unobs_q,
    output logic stuck_zero_q,
    output logic dead_branch_y
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            checked_q <= 1'b0;
            shadow_unobs_q <= 1'b0;
            stuck_zero_q <= 1'b0;
        end else begin
            checked_q <= (mode == 2'b01);
            shadow_unobs_q <= (mode == 2'b10);
            stuck_zero_q <= 1'b0;
        end
    end

    always_comb begin
        if (mode == 2'b11)
            dead_branch_y = 1'b1;
        else
            dead_branch_y = checked_q;
    end
endmodule
",
    )
    .unwrap();

    let sh = Shell::new(Session::new(eng, dir.join("work")), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv {{{}}}; elaborate -top dut; clock clk; reset {{!rst_n}}; set_prove_time_limit 30s",
        sv.display()
    ))
    .unwrap();
    sh.eval("assume -name m_no_mode3 {mode != 2'b11}").unwrap();
    sh.eval("assert -name a_chk {checked_q |-> $past(mode == 2'b01) || !$past(rst_n)}")
        .unwrap();
    sh.eval("check_cov -init -model {coi branch toggle}")
        .unwrap();
    sh.eval("prove -all").unwrap();
    assert_eq!(sh.eval("get_status a_chk").unwrap(), "proven");

    sh.eval("check_cov -measure").unwrap();
    {
        let s = sh.session.borrow();
        // 1. COI: `dut.checked_q` is Covered; `dut.shadow_unobs_q` is Uncovered!
        let coi_checked = s
            .cov
            .items
            .iter()
            .find(|i| i.model == CovModel::Coi && i.name == "dut.checked_q")
            .unwrap();
        assert_eq!(coi_checked.status, CovStatus::Covered);
        let coi_shadow = s
            .cov
            .items
            .iter()
            .find(|i| i.model == CovModel::Coi && i.name == "dut.shadow_unobs_q")
            .unwrap();
        assert_eq!(coi_shadow.status, CovStatus::Uncovered);

        // 2. Branch UNR: `mode == 2'b11` true branch is Unreachable under `assume {mode != 2'b11}`!
        assert!(
            s.cov
                .items
                .iter()
                .any(|i| i.model == CovModel::Branch && i.status == CovStatus::Unreachable),
            "expected dead `mode == 2'b11` branch to be UNR: {:?}",
            s.cov.items
        );

        // 3. Toggle UNR: `stuck_zero_q` never toggles!
        assert!(
            s.cov.items.iter().any(|i| {
                i.model == CovModel::Toggle
                    && i.name.contains("stuck_zero_q")
                    && i.status == CovStatus::Unreachable
            }),
            "expected stuck_zero_q toggle to be UNR: {:?}",
            s.cov.items
        );
    }

    // Add a waiver on `shadow_unobs_q` and generate both text and HTML reports!
    sh.eval("check_cov -waiver -add -type coi -instance shadow_unobs_q -comment {debug shadow}")
        .unwrap();
    {
        let s = sh.session.borrow();
        let coi_shadow = s
            .cov
            .items
            .iter()
            .find(|i| i.model == CovModel::Coi && i.name == "dut.shadow_unobs_q")
            .unwrap();
        assert_eq!(coi_shadow.status, CovStatus::Waived);
    }
    let html_file = dir.join("cov.html");
    sh.eval(&format!(
        "check_cov -report -html -report_file {{{}}} -no_return -force",
        html_file.display()
    ))
    .unwrap();
    let html = std::fs::read_to_string(&html_file).unwrap();
    assert!(html.contains("InFormal Formal Coverage"));
    assert!(html.contains("waived"));
}
