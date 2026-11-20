//! End-to-end integration tests for Tier-4, Deep Tier-5, and Tier-6:
//! 1. Tier 4: `implies`, `iff`, `if..else`, `s_nexttime`, compound unbounded `always`,
//!    procedural concurrent assertions in `for` loops & `task`s, and pure RTL user
//!    functions in external CLI `assert` properties.
//! 2. Deep Tier 5: CTI-seeded 2-literal Houdini mining (`mine::cti_candidates`),
//!    parent-lemma CTP prediction (`parent_ctp`), and structural `select_for_btor`.
//! 3. Tier 6: Dynamic CEX root-cause backward slicing (`why_cex`) pruning untaken mux arms.

use informal_fpv::{Flavor, Session, Shell, Status};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("informal-t456-{tag}-{}-{n}", std::process::id()));
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
fn tier4_sva_operators_loop_task_unrolling_and_cli_user_functions() {
    let Some(eng) = engines() else { return };
    let dir = temp_dir("tier4");
    let sv = dir.join("tier4_dut.sv");
    std::fs::write(
        &sv,
        r#"
package math_pkg;
  function automatic logic [3:0] wrap_inc(input logic [3:0] x);
    return x + 4'd1;
  endfunction
endpackage

module tier4_dut (
  input  logic       clk,
  input  logic       rst_n,
  input  logic       start_cfg,
  input  logic       mode,
  input  logic [1:0] req,
  input  logic [3:0] din,
  output logic [1:0] gnt_q,
  output logic [3:0] inc_q
);
  import math_pkg::*;

  function automatic logic is_nonzero_inc(input logic [3:0] v);
    return wrap_inc(v) != 4'd0;
  endfunction

  task automatic check_lane(input int idx);
    t_lane: assert property (@(posedge clk) disable iff (!rst_n) req[idx] |=> gnt_q[idx]);
  endtask

  logic active_cfg_q;
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      gnt_q        <= 2'b00;
      inc_q        <= 4'd0;
      active_cfg_q <= 1'b0;
    end else begin
      gnt_q <= req;
      inc_q <= wrap_inc(din);
      if (start_cfg)
        active_cfg_q <= 1'b1;

      for (int i = 0; i < 2; i++) begin
        a_loop: assert property (disable iff (!rst_n) req[i] |=> gnt_q[i]);
      end
      check_lane(0);
      check_lane(1);
    end
  end

  // Compound unbounded `always`: once `start_cfg` fires, `active_cfg_q` stays 1 and `req[0] |=> gnt_q[0]` holds forever!
  a_comp_alw: assert property (@(posedge clk) disable iff (!rst_n)
    start_cfg |=> always (active_cfg_q and (req[0] |=> gnt_q[0])));

  // `implies`, `iff`, `if..else`, and `s_nexttime`:
  a_iff: assert property (@(posedge clk) disable iff (!rst_n)
    (req[0] |=> gnt_q[0]) iff 1'b1);
  a_ifelse: assert property (@(posedge clk) disable iff (!rst_n)
    if (mode) (req[0] |=> gnt_q[0]) else (req[1] |=> gnt_q[1]));
  a_snext: assert property (@(posedge clk) disable iff (!rst_n)
    s_nexttime [2] (req[0] |=> gnt_q[0]));
endmodule
"#,
    )
    .unwrap();

    let session = Session::new(eng, dir.join("work"));
    let sh = Shell::new(session, Flavor::Jg);
    sh.eval(&format!("analyze -sv {{{}}}", sv.display()))
        .unwrap();
    sh.eval("elaborate -top tier4_dut").unwrap();
    sh.eval("clock clk").unwrap();
    sh.eval("reset {!rst_n}").unwrap();
    // External CLI properties using `implies` AND calling module + package functions!
    sh.eval("assert -name ext_imp {(req == 2'b11) implies (req[0] && req[1])}")
        .unwrap();
    sh.eval("assert -name ext_fn {is_nonzero_inc(din) |=> (inc_q != 4'd0)}")
        .unwrap();
    sh.eval("assert -name ext_pkg_fn {(math_pkg::wrap_inc(din) == 4'd0) |=> (inc_q == 4'd0)}")
        .unwrap();
    assert!(sh
        .eval("assert -name bad_typo {no_such_fn(din) |=> (inc_q != 4'd0)}")
        .is_err());
    sh.eval("prove -all").unwrap();

    let s = sh.session.borrow();
    for want in [
        "tier4_dut.a_loop",
        "tier4_dut.a_loop_2",
        "tier4_dut.t_lane",
        "tier4_dut.t_lane_2",
        "tier4_dut.a_comp_alw",
        "tier4_dut.a_iff",
        "tier4_dut.a_ifelse",
        "tier4_dut.a_snext",
        "ext_imp",
        "ext_fn",
        "ext_pkg_fn",
    ] {
        let p = s.props.iter().find(|p| p.name == want).unwrap_or_else(|| {
            panic!(
                "missing property {want}; got {:?}",
                s.props.iter().map(|q| &q.name).collect::<Vec<_>>()
            )
        });
        assert!(
            matches!(p.status, Status::Proven { .. }),
            "expected {want} to be proven, got {:?}",
            p.status
        );
    }
}

#[test]
fn tier5_cti_mining_parent_ctp_and_structural_portfolio_selection() {
    let Some(eng) = engines() else { return };
    let dir = temp_dir("tier5");
    let sv = dir.join("mutex_pipe.sv");
    std::fs::write(
        &sv,
        r#"
module mutex_pipe (
  input  logic clk,
  input  logic rst_n,
  input  logic step_en,
  output logic err_o
);
  logic a_q, b_q, c_q, d_q;
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      a_q <= 1'b1;
      b_q <= 1'b0;
      c_q <= 1'b0;
      d_q <= 1'b0;
    end else if (step_en) begin
      a_q <= d_q;
      b_q <= a_q;
      c_q <= b_q;
      d_q <= c_q;
    end
  end
  // One-hot ring: `a_q && c_q` can never hold simultaneously.
  assign err_o = a_q & c_q;
  a_onehot: assert property (@(posedge clk) disable iff (!rst_n) !err_o);
endmodule
"#,
    )
    .unwrap();

    let session = Session::new(eng.clone(), dir.join("work"));
    let sh = Shell::new(session, Flavor::Jg);
    sh.eval(&format!("analyze -sv {{{}}}", sv.display()))
        .unwrap();
    sh.eval("elaborate -top mutex_pipe").unwrap();
    sh.eval("clock clk").unwrap();
    sh.eval("reset {!rst_n}").unwrap();
    sh.eval("informal::set_engines informal-ic3-mt").unwrap();
    sh.eval("prove -all").unwrap();

    let s = sh.session.borrow();
    let p = s
        .props
        .iter()
        .find(|p| p.name == "mutex_pipe.a_onehot")
        .unwrap();
    assert!(
        matches!(&p.status, Status::Proven { engine, .. } if engine == "informal-ic3-mt"),
        "{:?}",
        p.status
    );

    // Check structural portfolio pruning on an arithmetic-heavy Btor2 cone:
    let arith_btor = "\
1 sort bitvec 16
2 sort bitvec 1
3 input 1 a
4 input 1 b
5 state 1 q
6 zero 1
7 init 1 5 6
8 mul 1 3 4
9 add 1 8 3
10 sub 1 9 4
11 mul 1 10 3
12 add 1 11 4
13 sub 1 12 3
14 mul 1 13 4
15 add 1 14 5
16 next 1 5 15
17 eq 2 5 3
18 bad 17
";
    let sel = informal_bmc::select_for_btor(&eng, arith_btor, false);
    assert!(sel.iter().any(|e| e.name() == "informal-ic3-mt"));
    assert!(!sel.iter().any(|e| e.name() == "informal-ic3"));
    assert!(!sel.iter().any(|e| e.name() == "informal-bmc-s65"));
}

#[test]
fn tier6_why_cex_prunes_untaken_mux_arm_and_reports_minimal_causal_chain() {
    let Some(eng) = engines() else { return };
    let dir = temp_dir("why_cex");
    let sv = dir.join("mux_bug.sv");
    std::fs::write(
        &sv,
        r#"
module mux_bug (
  input  logic       clk,
  input  logic       rst_n,
  input  logic       sel_arm1,
  input  logic       trigger_bug,
  input  logic [7:0] heavy_unused_in,
  output logic       err_flag
);
  logic       bug_q;
  logic [7:0] unused_lane_q;

  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      bug_q         <= 1'b0;
      unused_lane_q <= 8'd0;
    end else begin
      if (trigger_bug)
        bug_q <= 1'b1;
      unused_lane_q <= unused_lane_q + heavy_unused_in;
    end
  end

  // Keep `sel_arm1 == 0` so the `unused_lane_q == 8'hFF` arm is NEVER taken in the CEX!
  m_sel0: assume property (@(posedge clk) disable iff (!rst_n) !sel_arm1);
  assign err_flag = sel_arm1 ? (unused_lane_q == 8'hFF) : bug_q;

  a_no_err: assert property (@(posedge clk) disable iff (!rst_n) !err_flag);
endmodule
"#,
    )
    .unwrap();

    let session = Session::new(eng, dir.join("work"));
    let sh = Shell::new(session, Flavor::Jg);
    sh.eval(&format!("analyze -sv {{{}}}", sv.display()))
        .unwrap();
    sh.eval("elaborate -top mux_bug").unwrap();
    sh.eval("clock clk").unwrap();
    sh.eval("reset {!rst_n}").unwrap();
    sh.eval("prove -all").unwrap();

    let json_str = sh.eval("why_cex -json mux_bug.a_no_err").unwrap();
    let rep: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    assert_eq!(rep["status"], "cex");
    let active: Vec<&str> = rep["active_signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        active.iter().any(|s| s.ends_with("bug_q")),
        "active slice must include bug_q: {active:?}"
    );
    assert!(
        active.iter().any(|s| s.ends_with("trigger_bug")),
        "active slice must include trigger_bug: {active:?}"
    );
    assert!(
        !active
            .iter()
            .any(|s| s.ends_with("unused_lane_q") || s.ends_with("heavy_unused_in")),
        "untaken mux arm signals must be pruned from dynamic slice: {active:?}"
    );
    assert!(
        rep["active_slice_signals"].as_u64().unwrap()
            < rep["static_cone_signals"].as_u64().unwrap(),
        "dynamic slice should be strictly smaller than static COI: {rep:#}"
    );
}
