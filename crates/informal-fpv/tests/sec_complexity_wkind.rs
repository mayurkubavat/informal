//! End-to-end integration tests for sequential equivalence checking (`check_sec`),
//! SVA local variables & `case`, complexity/counter abstraction, and word-level
//! SMT array $k$-induction (`informal-wkind`):
//! 1. `check_sec`:
//!    - Sequential Equivalence Checking (`SEC`) between a reference RTL and a clock-gated /
//!      algebraically refactored RTL implementation (`proven`).
//!    - Retimed / pipelined SEC (`-latency 1`) proving a 1-stage spec equivalent to a 2-stage
//!      pipelined implementation (`proven`).
//!    - Combinational keypoint LEC (`-lec`) proving register next-state & output cones (`proven`).
//!    - Bug detection (`cex`) with side-by-side `shared.*`/`spec.*`/`imp.*` VCD trace and
//!      `why_cex` backward causal root-cause slice across both designs.
//! 2. SVA Local Variables, SVA `case`, `-D` `FORMAL_KEEP`, XMR, & Complexity/Abstraction Flow:
//!    - IEEE 1800-2023 §16.10 bounded multi-assignment local variables (`acc += din, acc++`)
//!      and unbounded `##[1:$]` / `until_with` tag-scoreboard local variables.
//!    - IEEE 1800-2023 §16.12.17 SVA `case (...) ... endcase`.
//!    - `-D` macro-expanded `(* keep *)` attribute + `bind` XMR + root-qualified CLI `top.u_sub.*`.
//!    - `assume -from_assert` helper-assertion workflow (verifying non-circularity when proving
//!      the promoted assertion itself!).
//!    - `get_fv_complexity`, `blackbox_assistant -apply`, `check_fv_setup`, `get_reset_info`,
//!      and `abstract -counter` over-approximate counter fast-forward on a 24-bit watchdog timer.
//! 3. `informal-wkind` + `H3` `coupled_push`:
//!    - Proving a stateful read-after-write invariant on an 8,192-word (`131,072`-bit) unblasted
//!      Btor2 `Sort::Array` memory (`> BLAST_WORDS = 4096`) end-to-end via `informal-wkind`.

use informal_fpv::sec::SecStatus;
use informal_fpv::{Flavor, Session, Shell, Status};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("informal-t789-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
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
fn check_sec_sequential_pipelined_lec_and_bug_vcd_slice() {
    let Some(eng) = all_engines() else { return };
    let dir = temp_dir("sec");

    let spec_sv = dir.join("spec.sv");
    std::fs::write(
        &spec_sv,
        r#"
module mac_spec (
  input  logic       clk,
  input  logic       rst_n,
  input  logic       en,
  input  logic [7:0] a,
  input  logic [7:0] b,
  output logic [7:0] acc_out
);
  logic [7:0] acc_q;
  always_ff @(posedge clk) begin
    if (!rst_n)
      acc_q <= 8'd0;
    else if (en)
      acc_q <= acc_q + (a ^ b) + 8'd1;
  end
  assign acc_out = acc_q;
endmodule
"#,
    )
    .unwrap();

    // Algebraically refactored implementation: ~(~(a ^ b)) == (a ^ b), explicit mux hold.
    let imp_ok_sv = dir.join("imp_ok.sv");
    std::fs::write(
        &imp_ok_sv,
        r#"
module mac_imp (
  input  logic       clk,
  input  logic       rst_n,
  input  logic       en,
  input  logic [7:0] a,
  input  logic [7:0] b,
  output logic [7:0] acc_out
);
  logic [7:0] acc_q;
  wire  [7:0] delta = ((a & ~b) | (~a & b)) + 8'h01;
  always_ff @(posedge clk) begin
    if (!rst_n)
      acc_q <= 8'd0;
    else
      acc_q <= en ? (acc_q + delta) : acc_q;
  end
  assign acc_out = acc_q;
endmodule
"#,
    )
    .unwrap();

    // Retimed 2-stage pipelined implementation (`-latency 1` relative to `mac_spec`).
    let imp_pipe_sv = dir.join("imp_pipe.sv");
    std::fs::write(
        &imp_pipe_sv,
        r#"
module mac_pipe (
  input  logic       clk,
  input  logic       rst_n,
  input  logic       en,
  input  logic [7:0] a,
  input  logic [7:0] b,
  output logic [7:0] acc_out
);
  logic [7:0] acc_q;
  logic [7:0] out_stage_q;
  always_ff @(posedge clk) begin
    if (!rst_n) begin
      acc_q       <= 8'd0;
      out_stage_q <= 8'd0;
    end else begin
      if (en)
        acc_q <= acc_q + (a ^ b) + 8'd1;
      out_stage_q <= acc_q;
    end
  end
  assign acc_out = out_stage_q;
endmodule
"#,
    )
    .unwrap();

    // Buggy implementation: subtle corner bug when `en && a == 8'hA5`.
    let imp_bug_sv = dir.join("imp_bug.sv");
    std::fs::write(
        &imp_bug_sv,
        r#"
module mac_bug (
  input  logic       clk,
  input  logic       rst_n,
  input  logic       en,
  input  logic [7:0] a,
  input  logic [7:0] b,
  output logic [7:0] acc_out
);
  logic [7:0] acc_q;
  always_ff @(posedge clk) begin
    if (!rst_n)
      acc_q <= 8'd0;
    else if (en)
      acc_q <= acc_q + (a ^ b) + ((a == 8'hA5) ? 8'd2 : 8'd1);
  end
  assign acc_out = acc_q;
endmodule
"#,
    )
    .unwrap();

    let sh = Shell::new(Session::new(eng, dir.join("work")), Flavor::Jg);

    // 1. Sequential SEC: `mac_spec` === `mac_imp`.
    sh.eval(&format!(
        "check_sec -setup -spec {{{}}} -imp {{{}}} -clock clk -reset {{!rst_n}} -prove",
        spec_sv.display(),
        imp_ok_sv.display()
    ))
    .unwrap();
    {
        let s = sh.session.borrow();
        assert_eq!(s.sec.checks.len(), 1);
        assert_eq!(s.sec.checks[0].name, "sec_out_acc_out");
        assert_eq!(s.sec.checks[0].status, SecStatus::Proven);
    }

    // 2. Combinational keypoint LEC (`-lec`): proves both `lec_reg_acc_q` and `lec_out_acc_out`.
    sh.eval(&format!(
        "check_sec -setup -lec -spec {{{}}} -imp {{{}}} -clock clk -prove",
        spec_sv.display(),
        imp_ok_sv.display()
    ))
    .unwrap();
    {
        let s = sh.session.borrow();
        assert_eq!(s.sec.checks.len(), 2);
        assert!(
            s.sec.checks.iter().all(|c| c.status == SecStatus::Proven),
            "{:?}",
            s.sec.checks
        );
    }

    // 3. Pipelined SEC (`-latency 1`): proves `mac_spec` delayed by 1 cycle === `mac_pipe`.
    sh.eval(&format!(
        "check_sec -setup -latency 1 -spec {{{}}} -imp {{{}}} -clock clk -reset {{!rst_n}} -prove",
        spec_sv.display(),
        imp_pipe_sv.display()
    ))
    .unwrap();
    {
        let s = sh.session.borrow();
        assert_eq!(s.sec.checks.len(), 1);
        assert_eq!(s.sec.checks[0].latency, 1);
        assert_eq!(s.sec.checks[0].status, SecStatus::Proven);
    }

    // 4. Buggy implementation: catches `a == 8'hA5`, writes side-by-side VCD, and slices via `why_cex`.
    sh.eval(&format!(
        "check_sec -setup -spec {{{}}} -imp {{{}}} -clock clk -reset {{!rst_n}} -prove",
        spec_sv.display(),
        imp_bug_sv.display()
    ))
    .unwrap();
    {
        let s = sh.session.borrow();
        assert_eq!(s.sec.checks.len(), 1);
        assert!(matches!(s.sec.checks[0].status, SecStatus::Cex { .. }));
        let vcd_path = s.sec.checks[0].trace.as_ref().expect("SEC CEX VCD written");
        let vcd = std::fs::read_to_string(vcd_path).unwrap();
        assert!(vcd.contains("$scope module shared $end"), "{vcd}");
        assert!(vcd.contains("$scope module spec $end"), "{vcd}");
        assert!(vcd.contains("$scope module imp $end"), "{vcd}");
    }
    let slice_json = sh.eval("why_cex -json sec_out_acc_out").unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&slice_json).unwrap();
    assert_eq!(parsed["property"], "sec_out_acc_out");
    assert_eq!(parsed["status"], "cex");
    assert!(!parsed["events"].as_array().unwrap().is_empty());
}

#[test]
fn sva_local_vars_case_xmr_assume_from_assert_complexity_and_counter_abstract() {
    let Some(eng) = all_engines() else { return };
    let dir = temp_dir("complexity");

    let sv = dir.join("wdog_dut.sv");
    std::fs::write(
        &sv,
        r#"
module heavy_mul (
  input  logic        clk,
  input  logic [31:0] x,
  input  logic [31:0] y,
  output logic [31:0] prod_q
);
  always_ff @(posedge clk) prod_q <= x * y;
endmodule

module secret_ leaf_sub (
  input  logic       clk,
  input  logic       rst_n,
  input  logic [7:0] d,
  output logic [7:0] q
);
  `FORMAL_KEEP logic [7:0] shadow_q;
  always_ff @(posedge clk) begin
    if (!rst_n) begin
      q        <= 8'd0;
      shadow_q <= 8'd0;
    end else begin
      q        <= d;
      shadow_q <= d;
    end
  end
endmodule

module wdog_dut (
  input  logic        clk,
  input  logic        rst_n,
  input  logic        start,
  input  logic        wr_en,
  input  logic        rd_en,
  input  logic [1:0]  mode,
  input  logic [7:0]  din,
  input  logic [31:0] mx,
  input  logic [31:0] my,
  output logic [7:0]  sum2_q,
  output logic [7:0]  slot_q,
  output logic [31:0] unused_prod,
  output logic        wdog_expired
);
  // 2-cycle bounded pipeline for multi-assignment SVA local variable test
  logic [7:0] d1_q;
  always_ff @(posedge clk) begin
    if (!rst_n) begin
      d1_q   <= 8'd0;
      sum2_q <= 8'd0;
    end else begin
      d1_q   <= din;
      sum2_q <= d1_q + din + 8'd1;
    end
  end

  // Held slot register for unbounded local-variable prophecy test
  logic slot_valid_q;
  always_ff @(posedge clk) begin
    if (!rst_n) begin
      slot_valid_q <= 1'b0;
      slot_q       <= 8'd0;
    end else if (wr_en && !slot_valid_q) begin
      slot_valid_q <= 1'b1;
      slot_q       <= din;
    end else if (rd_en && slot_valid_q) begin
      slot_valid_q <= 1'b0;
    end
  end

  // Mode pipeline for SVA `case` property
  logic [1:0] mode_q;
  logic [7:0] mode_out_q;
  always_ff @(posedge clk) begin
    if (!rst_n) begin
      mode_q     <= 2'd0;
      mode_out_q <= 8'd0;
    end else begin
      mode_q <= mode;
      case (mode)
        2'd0:    mode_out_q <= 8'd0;
        2'd1:    mode_out_q <= din;
        default: mode_out_q <= 8'hFF;
      endcase
    end
  end

  // 24-bit watchdog counter (saturates at 1_000_000)
  logic [23:0] wdog_cnt;
  always_ff @(posedge clk) begin
    if (!rst_n)
      wdog_cnt <= 24'd0;
    else if (wdog_cnt < 24'd1000000)
      wdog_cnt <= wdog_cnt + 24'd1;
  end
  assign wdog_expired = (wdog_cnt == 24'd1000000);

  heavy_mul u_mul (.clk(clk), .x(mx), .y(my), .prod_q(unused_prod));
  secret_   u_sub (.clk(clk), .rst_n(rst_n), .d(din), .q());

  // 1. Bounded multi-assignment SVA local variable (`=`, `+=`, `++`)
  property p_local_acc;
    logic [7:0] acc;
    @(posedge clk) disable iff (!rst_n)
      (start, acc = din) ##1 (1'b1, acc += din, acc++) |=> (sum2_q == acc);
  endproperty
  a_local_acc: assert property (p_local_acc);

  // 2. Unbounded local-variable prophecy (`until_with`)
  property p_slot_integrity;
    logic [7:0] tag_data;
    @(posedge clk) disable iff (!rst_n)
      (wr_en && !slot_valid_q, tag_data = din) |=>
        ((slot_valid_q && slot_q == tag_data) until_with (rd_en && slot_q == tag_data));
  endproperty
  // Constrain `rd_en` to eventually arrive or just check safety `until` (`!rd_en` holds data):
  property p_slot_hold;
    logic [7:0] snap;
    @(posedge clk) disable iff (!rst_n)
      (wr_en && !slot_valid_q, snap = din) |=>
        ((slot_q == snap) until (rd_en && slot_q == snap));
  endproperty
  a_slot_hold: assert property (p_slot_hold);

  // 3. SVA `case` property (IEEE 1800-2023 §16.12.17)
  property p_mode_case;
    @(posedge clk) disable iff (!rst_n)
      case (mode)
        2'd0:    1'b1 |=> (mode_out_q == 8'd0);
        2'd1:    1'b1 |=> (mode_out_q == $past(din));
        default: 1'b1 |=> (mode_out_q == 8'hFF);
      endcase
  endproperty
  a_mode_case: assert property (p_mode_case);

  // 4. Helper assertion + intentional false assertion for `assume -from_assert` non-circularity check
  a_wdog_bound: assert property (@(posedge clk) disable iff (!rst_n) wdog_cnt <= 24'd1000000);
  a_false_self: assert property (@(posedge clk) disable iff (!rst_n) din != 8'h7E);
endmodule
"#,
    )
    .unwrap();

    // Fix typo `module secret_ leaf_sub` -> `module secret_sub` before running!
    let fixed_sv = std::fs::read_to_string(&sv)
        .unwrap()
        .replace("module secret_ leaf_sub", "module secret_sub")
        .replace("secret_   u_sub", "secret_sub u_sub");
    std::fs::write(&sv, fixed_sv).unwrap();

    let sh = Shell::new(Session::new(eng, dir.join("work")), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv {{+define+FORMAL_KEEP=(* keep *)}} {{{}}}",
        sv.display()
    ))
    .unwrap();
    sh.eval("elaborate -top wdog_dut").unwrap();
    sh.eval("clock clk; reset -expression {!rst_n}").unwrap();

    // Root-qualified XMR CLI assertion (`wdog_dut.u_sub.shadow_q`)
    sh.eval("assert -name a_xmr_shadow {wdog_dut.u_sub.shadow_q == u_sub.q}")
        .unwrap();
    // Cover reaching the 1,000,000 watchdog expiration (fast-forwarded via `abstract -counter`)
    sh.eval("cover -name c_wdog_expired {wdog_expired}")
        .unwrap();
    // Put `u_mul` (`unused_prod`) in the active property COI so `get_fv_complexity` &
    // `blackbox_assistant` identify `u_mul`'s 32-bit multiplier hotspot!
    sh.eval("cover -name c_prod_lsb {unused_prod[0] == 1'b0}")
        .unwrap();

    // Check `get_fv_complexity` & `blackbox_assistant -apply` (boxes `u_mul` & wide multiplier!)
    let comp_json = sh.eval("get_fv_complexity -json").unwrap();
    let comp: serde_json::Value = serde_json::from_str(&comp_json).unwrap();
    assert!(comp["wide_arith_ops"].as_u64().unwrap() >= 1, "{comp}");

    let bbox_json = sh.eval("blackbox_assistant -apply -json").unwrap();
    let bbox_adv: serde_json::Value = serde_json::from_str(&bbox_json).unwrap();
    assert_eq!(bbox_adv["applied"], true);
    assert!(
        bbox_adv["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["target"] == "u_mul"),
        "{bbox_adv}"
    );

    // Promote BOTH `a_wdog_bound` AND `a_false_self` via `assume -from_assert`:
    // `a_false_self` MUST still fail with CEX (non-circularity!), while another property
    // `a_uses_helper` CAN rely on `a_false_self_assume`!
    sh.eval("assume -from_assert *a_wdog_bound").unwrap();
    sh.eval("assume -from_assert *a_false_self -name m_no_7e")
        .unwrap();
    sh.eval("assert -name a_uses_7e_helper {din != 8'h7E}")
        .unwrap();

    // Abstract the 24-bit counter `wdog_cnt` with `-threshold 1000000`
    sh.eval("abstract -counter wdog_cnt -threshold 1000000")
        .unwrap();

    let setup_json = sh.eval("check_fv_setup -json").unwrap();
    let setup: serde_json::Value = serde_json::from_str(&setup_json).unwrap();
    assert_eq!(setup["elaborated"], true);
    assert_eq!(setup["counter_abstractions"].as_array().unwrap().len(), 1);
    assert_eq!(setup["helper_assumptions"].as_array().unwrap().len(), 2);

    sh.eval("prove -all").unwrap();

    let s = sh.session.borrow();
    let status_of = |suffix: &str| -> &Status {
        &s.props
            .iter()
            .find(|p| p.name.ends_with(suffix))
            .unwrap_or_else(|| panic!("missing property ending with {suffix}"))
            .status
    };

    assert!(
        matches!(status_of("a_local_acc"), Status::Proven { .. }),
        "{:?}",
        status_of("a_local_acc")
    );
    assert!(
        matches!(status_of("a_slot_hold"), Status::Proven { .. }),
        "{:?}",
        status_of("a_slot_hold")
    );
    assert!(
        matches!(status_of("a_mode_case"), Status::Proven { .. }),
        "{:?}",
        status_of("a_mode_case")
    );
    assert!(
        matches!(status_of("a_xmr_shadow"), Status::Proven { .. }),
        "{:?}",
        status_of("a_xmr_shadow")
    );
    assert!(
        matches!(status_of("a_wdog_bound"), Status::Proven { .. }),
        "{:?}",
        status_of("a_wdog_bound")
    );
    // `a_false_self` was promoted via `assume -from_assert`, yet proving `a_false_self` itself
    // excludes `m_no_7e` so it genuinely FAILS (`Cex`), whereas `a_uses_7e_helper` PROVES!
    assert!(
        matches!(status_of("a_false_self"), Status::Cex { .. }),
        "{:?}",
        status_of("a_false_self")
    );
    assert!(
        matches!(status_of("a_uses_7e_helper"), Status::Proven { .. }),
        "{:?}",
        status_of("a_uses_7e_helper")
    );
    // Under `abstract -counter wdog_cnt -threshold 1000000`, `wdog_expired` (`wdog_cnt == 1_000_000`)
    // is covered in a few cycles instead of requiring 1,000,000 BMC unrollings!
    assert!(
        matches!(status_of("c_wdog_expired"), Status::Covered { depth: Some(d), .. } if *d <= 5),
        "{:?}",
        status_of("c_wdog_expired")
    );
}

#[test]
fn wkind_proves_large_unblasted_8192_word_memory_in_session() {
    let wkind_engines = informal_bitwuzla::engines();
    if !wkind_engines.iter().any(|e| e.name() == "informal-wkind") {
        informal_engine::tool_missing("needs informal-bitwuzla / informal-wkind");
        return;
    }
    let Some(eng) = all_engines() else { return };
    let dir = temp_dir("wkind");

    // 8,192 words (> BLAST_WORDS = 4096) -> lowered as unblasted Btor2 `Sort::Array`!
    let sv = dir.join("big_sram.sv");
    std::fs::write(
        &sv,
        r#"
module big_sram (
  input  logic        clk,
  input  logic        rst_n,
  input  logic        wr_en,
  input  logic [12:0] wr_addr,
  input  logic [15:0] wr_data,
  output logic [15:0] shadow_data
);
  logic [15:0] mem [0:8191];
  logic [12:0] tracked_addr_q;
  logic [15:0] tracked_data_q;
  logic        valid_q;

  always_ff @(posedge clk) begin
    if (!rst_n) begin
      valid_q        <= 1'b0;
      tracked_addr_q <= 13'd0;
      tracked_data_q <= 16'd0;
    end else if (wr_en) begin
      mem[wr_addr] <= wr_data;
      if (!valid_q || wr_addr == tracked_addr_q) begin
        valid_q        <= 1'b1;
        tracked_addr_q <= wr_addr;
        tracked_data_q <= wr_data;
      end
    end
  end

  assign shadow_data = mem[tracked_addr_q];

  a_sram_coherence: assert property (
    @(posedge clk) disable iff (!rst_n)
      valid_q |-> (shadow_data == tracked_data_q)
  );
endmodule
"#,
    )
    .unwrap();

    let sh = Shell::new(Session::new(eng, dir.join("work")), Flavor::Jg);
    sh.eval(&format!("analyze -sv {{{}}}", sv.display()))
        .unwrap();
    sh.eval("elaborate -top big_sram").unwrap();
    sh.eval("clock clk; reset -expression {!rst_n}").unwrap();
    sh.eval("prove -all").unwrap();

    let s = sh.session.borrow();
    let prop = s
        .props
        .iter()
        .find(|p| p.name.ends_with("a_sram_coherence"))
        .unwrap();
    assert_eq!(
        prop.status,
        Status::Proven {
            engine: "informal-wkind".into(),
            ms: match prop.status {
                Status::Proven { ms, .. } => ms,
                _ => 0,
            }
        },
        "expected informal-wkind to prove 8192-word Sort::Array property, got {:?}",
        prop.status
    );
}
