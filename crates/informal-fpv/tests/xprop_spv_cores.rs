//! End-to-end integration tests for X-propagation, security path verification,
//! bounded proofs, dead-end assumption checks, and proof cores:
//! 1. `check_xprop` (exact 2-copy self-composition X-propagation, reconvergence precision, waivers)
//! 2. `check_spv` (2-safety non-interference, conditional isolation, multi-cycle covert channels)
//! 3. `assert -bound K` / `prove -bound K` (`bounded_proven` vs `cex`) & `check_assumptions -dead_end` (2QBF trap-state detection)
//! 4. `prove -with_proof_core` / `get_property_info -proof_core` (minimal needed vs redundant assumptions) + `informal-ic3-mt` (EXCTG/DynAMic/BVE)

use informal_fpv::spv::SpvStatus;
use informal_fpv::xprop::XpropStatus;
use informal_fpv::{Flavor, Session, Shell};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("informal-t35-{tag}-{}-{n}", std::process::id()));
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
fn check_xprop_catches_resetless_x_leak_avoids_mux_x_pessimism_and_honours_waivers() {
    let Some(eng) = engines() else { return };
    let dir = temp_dir("xprop");
    let sv = dir.join("xprop_dut.sv");
    std::fs::write(
        &sv,
        "\
module xprop_dut(
    input logic clk,
    input logic rst_n,
    input logic load_en,
    input logic [7:0] din,
    output logic [7:0] clean_out,
    output logic [7:0] reconv_out,
    output logic [7:0] leaky_out
);
    logic [7:0] uninit_pipe_q;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n)
            clean_out <= 8'd0;
        else
            clean_out <= din;
    end
    // Reset-less register: holds power-on X until `load_en` fires!
    always_ff @(posedge clk) begin
        if (load_en)
            uninit_pipe_q <= din;
    end
    // Reconvergent mux where both arms equal 8'hA5: ternary X-sim says 8'hxx,
    // whereas exact 2-copy self-composition proves it is 100% X-clean!
    assign reconv_out = uninit_pipe_q[0] ? 8'hA5 : 8'hA5;
    assign leaky_out = uninit_pipe_q;
endmodule
",
    )
    .unwrap();

    let sh = Shell::new(Session::new(eng, dir.join("work")), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv {{{}}}; elaborate -top xprop_dut; clock clk; reset {{!rst_n}}; set_prove_time_limit 20s",
        sv.display()
    ))
    .unwrap();
    sh.eval("check_xprop -init -targets {outputs} -prove")
        .unwrap();
    let rep = sh.eval("check_xprop -report").unwrap();
    assert!(rep.contains("xprop_dut.leaky_out"));
    {
        let s = sh.session.borrow();
        let find = |n: &str| s.xprop.items.iter().find(|i| i.name == n).unwrap().status;
        assert_eq!(find("xprop_dut.clean_out"), XpropStatus::Proven);
        assert_eq!(
            find("xprop_dut.reconv_out"),
            XpropStatus::Proven,
            "2-copy miter must not suffer ternary mux X-pessimism"
        );
        assert!(matches!(
            find("xprop_dut.leaky_out"),
            XpropStatus::Cex { .. }
        ));
    }

    sh.eval("check_xprop -waiver -add -instance leaky_out -comment {datapath qualified by valid}")
        .unwrap();
    {
        let s = sh.session.borrow();
        let leaky = s
            .xprop
            .items
            .iter()
            .find(|i| i.name == "xprop_dut.leaky_out")
            .unwrap();
        assert_eq!(leaky.status, XpropStatus::Waived);
    }
}

#[test]
fn check_spv_proves_key_isolation_and_catches_multi_cycle_covert_channel() {
    let Some(eng) = engines() else { return };
    let dir = temp_dir("spv");
    let sv = dir.join("crypto_wrap.sv");
    std::fs::write(
        &sv,
        "\
module crypto_wrap(
    input logic clk,
    input logic rst_n,
    input logic [7:0] secret_key,
    input logic [7:0] pub_data,
    input logic debug_unlock,
    output logic [7:0] pub_status_o,
    output logic [7:0] debug_port_o,
    output logic [7:0] covert_port_o
);
    logic s1, s2;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            pub_status_o <= 8'd0;
            s1 <= 1'b0;
            s2 <= 1'b0;
        end else begin
            pub_status_o <= pub_data + 8'd1;
            s1 <= secret_key[0];
            s2 <= s1;
        end
    end
    assign debug_port_o = debug_unlock ? secret_key : pub_data;
    assign covert_port_o = pub_data ^ {7'd0, s2};
endmodule
",
    )
    .unwrap();

    let sh = Shell::new(Session::new(eng, dir.join("work")), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv {{{}}}; elaborate -top crypto_wrap; clock clk; reset {{!rst_n}}; set_prove_time_limit 20s",
        sv.display()
    ))
    .unwrap();
    sh.eval("check_spv -create -name iso_pub -from secret_key -to pub_status_o")
        .unwrap();
    sh.eval("check_spv -create -name iso_dbg_locked -from secret_key -to debug_port_o -cond {!debug_unlock}")
        .unwrap();
    sh.eval("check_spv -create -name leak_dbg_open -from secret_key -to debug_port_o")
        .unwrap();
    sh.eval("check_spv -create -name leak_covert_2cyc -from secret_key -to covert_port_o")
        .unwrap();
    sh.eval("check_spv -prove").unwrap();
    let rep = sh.eval("check_spv -report").unwrap();
    assert!(rep.contains("iso_pub"));
    {
        let s = sh.session.borrow();
        let status_of = |n: &str| s.spv.iter().find(|r| r.name == n).unwrap().status.clone();
        assert_eq!(status_of("iso_pub"), SpvStatus::Proven);
        assert_eq!(status_of("iso_dbg_locked"), SpvStatus::Proven);
        assert!(matches!(status_of("leak_dbg_open"), SpvStatus::Cex { .. }));
        assert!(
            matches!(status_of("leak_covert_2cyc"), SpvStatus::Cex { depth: Some(d) } if d >= 2),
            "expected multi-cycle covert storage channel leak, got {:?}",
            status_of("leak_covert_2cyc")
        );
    }
}

#[test]
fn bounded_proofs_and_2qbf_dead_end_environment_detection() {
    let Some(eng) = engines() else { return };
    let dir = temp_dir("bound_deadend");
    let sv = dir.join("proto.sv");
    std::fs::write(
        &sv,
        "\
module proto(
    input logic clk,
    input logic rst_n,
    input logic req,
    input logic ack,
    output logic [3:0] cnt,
    output logic busy
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            cnt <= 4'd0;
            busy <= 1'b0;
        end else begin
            cnt <= cnt + 4'd1;
            if (req)
                busy <= 1'b1;
        end
    end
endmodule
",
    )
    .unwrap();

    let sh = Shell::new(Session::new(eng, dir.join("work")), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv {{{}}}; elaborate -top proto; clock clk; reset {{!rst_n}}; set_prove_time_limit 20s",
        sv.display()
    ))
    .unwrap();

    // 1. Bounded assertions: `cnt != 4'd8` holds up to bound 5 (`bounded_proven`), fails within bound 12 (`cex`).
    sh.eval("assert -name a_short -bound 5 {cnt != 4'd8}")
        .unwrap();
    sh.eval("assert -name a_long -bound 12 {cnt != 4'd8}")
        .unwrap();
    sh.eval("prove -all").unwrap();
    assert_eq!(sh.eval("get_status a_short").unwrap(), "bounded_proven");
    assert_eq!(sh.eval("get_status a_long").unwrap(), "cex");

    // 2. Dead-end environment detection:
    // While `req == 0`, `busy` stays `0`, so a 6-step trace EXISTS (`check_assumptions -depth 6` == 6).
    // Yet if `req == 1`, `busy` becomes `1`, where `!(busy && ack)` and `!(busy && !ack)` trap ALL next inputs!
    sh.eval("assume -name m_no_ack_when_busy {!(busy && ack)}")
        .unwrap();
    sh.eval("assume -name m_must_ack_when_busy {!(busy && !ack)}")
        .unwrap();
    assert_eq!(sh.eval("check_assumptions -depth 6").unwrap(), "6");
    let de = sh.eval("check_assumptions -dead_end -depth 6").unwrap();
    assert!(
        de.starts_with("dead_end at step "),
        "expected 2QBF dead-end trap state when req=1 sets busy=1, got `{de}`"
    );
}

#[test]
fn proof_core_identifies_needed_vs_redundant_assumptions_and_ic3_mt_converges() {
    let Some(eng) = engines() else { return };
    let dir = temp_dir("pcore");
    let sv = dir.join("acc.sv");
    std::fs::write(
        &sv,
        "\
module acc(
    input logic clk,
    input logic rst_n,
    input logic [3:0] step_i,
    input logic bystander_i,
    output logic [3:0] q,
    output logic [3:0] mirror_q
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            q <= 4'd0;
            mirror_q <= 4'd0;
        end else if (q < 4'd10) begin
            q <= q + step_i;
            mirror_q <= mirror_q + step_i;
        end
    end
endmodule
",
    )
    .unwrap();

    let sh = Shell::new(Session::new(eng, dir.join("work")), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv {{{}}}; elaborate -top acc; clock clk; reset {{!rst_n}}; set_prove_time_limit 20s",
        sv.display()
    ))
    .unwrap();
    sh.eval("assume -name m_needed {step_i <= 4'd1}").unwrap();
    sh.eval("assume -name m_redundant_in_cone {step_i <= 4'd3}")
        .unwrap();
    sh.eval("assume -name m_disjoint {bystander_i == 1'b0}")
        .unwrap();
    sh.eval("assert -name a_bound {q <= 4'd10}").unwrap();
    sh.eval("assert -name a_eq {q == mirror_q}").unwrap();

    sh.eval("informal::set_engines informal-ic3-mt").unwrap();
    sh.eval("prove -all -with_proof_core").unwrap();
    assert_eq!(sh.eval("get_status a_bound").unwrap(), "proven");
    assert_eq!(sh.eval("get_status a_eq").unwrap(), "proven");

    let json_text = sh
        .eval("get_property_info a_bound -proof_core -json")
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&json_text).unwrap();
    let pc = &v["proof_core"];
    let needed: Vec<&str> = pc["needed_assumptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    let unused: Vec<&str> = pc["unused_assumptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(needed, ["m_needed"]);
    assert!(unused.contains(&"m_redundant_in_cone"));
    assert!(unused.contains(&"m_disjoint"));
}
