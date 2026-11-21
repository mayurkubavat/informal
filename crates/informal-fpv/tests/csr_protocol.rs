//! End-to-end integration tests for CSR formal verification (`check_csr`),
//! bus protocol VIP generation (`check_protocol`), and `why_cex` causal slicing:
//! 1. `check_csr`:
//!    - Automated CSR formal verification from **OpenTitan HJSON**, **CSV**, and
//!      **UVM RAL SystemVerilog (`uvm_reg_block` / `uvm_reg`)** specifications.
//!    - Proves post-reset (`_reset`), read-back & `WO` zero-read (`_read`),
//!      access policies (`RW`, `RO`, `RW1C`, `RW0C`, `WO`, `_access`),
//!      `regwen` lock protection (`_lock`), and address non-aliasing (`_no_alias`).
//!    - Detects subtle CSR RTL bugs (`RW1C` implemented as `RW` + missing `regwen` lock).
//! 2. `check_protocol`:
//!    - 1-command formal bus VIP SVA synthesis & proof for **AMBA APB**,
//!      **AMBA AXI4-Lite**, and **OpenTitan TileLink-UL (`tlul`)** (`slave` & `master` roles).
//!    - Detects AXI4-Lite master handshake drop (`awvalid && !awready |=> awvalid`) and
//!      extracts causal root-cause slice via `why_cex -json`.

use informal_fpv::{Flavor, Kind, Session, Shell, Status};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "informal-csr-proto-{tag}-{}-{n}",
        std::process::id()
    ));
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
fn check_csr_hjson_csv_uvm_ral_and_bug_detection() {
    let Some(eng) = all_engines() else { return };
    let dir = temp_dir("csr");

    // Compliant CSR RTL block implementing CFG_REGWEN (RW0C), CTRL (RW + RO gated by CFG_REGWEN),
    // IRQ_STATE (RW1C), and KEY_WO (WO, reads 0).
    let csr_ok_sv = dir.join("csr_ok.sv");
    std::fs::write(
        &csr_ok_sv,
        r#"
module csr_block (
  input  logic        clk,
  input  logic        rst_n,
  input  logic        we,
  input  logic        re,
  input  logic [7:0]  addr,
  input  logic [31:0] wdata,
  output logic [31:0] rdata
);
  logic       cfg_regwen_q;
  logic [3:0] ctrl_mode_q;
  logic [3:0] ctrl_rev_id_q;
  logic [3:0] irq_state_pending_q;
  logic [7:0] key_wo_secret_q;

  always_ff @(posedge clk) begin
    if (!rst_n) begin
      cfg_regwen_q        <= 1'b1;
      ctrl_mode_q         <= 4'h5;
      ctrl_rev_id_q       <= 4'hA;
      irq_state_pending_q <= 4'hF;
      key_wo_secret_q     <= 8'h00;
    end else if (we) begin
      case (addr)
        8'h00: cfg_regwen_q        <= cfg_regwen_q & wdata[0];
        8'h04: if (cfg_regwen_q) ctrl_mode_q <= wdata[3:0];
        8'h08: irq_state_pending_q <= irq_state_pending_q & ~wdata[3:0];
        8'h0C: key_wo_secret_q     <= wdata[7:0];
        default: ;
      endcase
    end
  end

  always_comb begin
    rdata = 32'h0;
    if (re && !we) begin
      case (addr)
        8'h00: rdata = {31'd0, cfg_regwen_q};
        8'h04: rdata = {24'd0, ctrl_rev_id_q, ctrl_mode_q};
        8'h08: rdata = {28'd0, irq_state_pending_q};
        8'h0C: rdata = 32'h0; // WO reads 0
        default: rdata = 32'h0;
      endcase
    end
  end
endmodule
"#,
    )
    .unwrap();

    // 1A. OpenTitan HJSON CSR spec (with comments, unquoted keys/hex, regwen, WO, RW0C, RW1C, RO).
    let regs_hjson = dir.join("regs.hjson");
    std::fs::write(
        &regs_hjson,
        r#"
// OpenTitan-style register specification
{
  name: "csr_block"
  regwidth: "32"
  registers: [
    {
      name: "CFG_REGWEN"
      swaccess: "rw0c"
      resval: "0x1"
      fields: [
        { bits: "0", name: "wen", resval: "0x1" }
      ]
    }
    {
      name: "CTRL"
      regwen: "CFG_REGWEN"
      fields: [
        { bits: "3:0", name: "mode", swaccess: "rw", resval: "0x5" }
        { bits: "7:4", name: "rev_id", swaccess: "ro", resval: "0xa" }
      ]
    }
    {
      name: "IRQ_STATE"
      swaccess: "rw1c"
      fields: [
        { bits: "3:0", name: "pending", resval: "0xf" }
      ]
    }
    {
      name: "KEY_WO"
      swaccess: "wo"
      fields: [
        { bits: "7:0", name: "secret", resval: "0x0" }
      ]
    }
  ]
}
"#,
    )
    .unwrap();

    {
        let sh = Shell::new(
            Session::new(eng.clone(), dir.join("work_hjson")),
            Flavor::Jg,
        );
        sh.eval(&format!(
            r#"
            analyze -sv09 {{{sv}}}
            elaborate -top csr_block
            clock clk
            reset !rst_n
            check_csr -load {{{spec}}} -prove
            "#,
            sv = csr_ok_sv.display(),
            spec = regs_hjson.display(),
        ))
        .unwrap();

        {
            let session = sh.session.borrow();
            let csr_props: Vec<_> = session
                .props
                .iter()
                .filter(|p| p.task == "CSR" && p.kind == Kind::Assert)
                .collect();
            assert!(
                csr_props.len() >= 18,
                "expected >= 18 synthesized CSR assertions, got {}",
                csr_props.len()
            );
            for p in &csr_props {
                assert!(
                    matches!(p.status, Status::Proven { .. }),
                    "HJSON CSR property `{}` ({}) did not prove: {:?}",
                    p.name,
                    p.expr,
                    p.status
                );
            }
        }

        let js_str = sh.eval("check_csr -report -json").unwrap();
        let js: serde_json::Value = serde_json::from_str(&js_str).unwrap();
        assert_eq!(js["schema"], "informal.csr/1");
    }

    // 1B. CSV CSR spec with explicit regwen column.
    let regs_csv = dir.join("regs.csv");
    std::fs::write(
        &regs_csv,
        "\
register,offset,field,bits,access,reset,rtl_path,regwen
CFG_REGWEN,0x00,cfg_regwen,0,RW0C,0x1,,
CTRL,0x04,mode,3:0,RW,0x5,,CFG_REGWEN
CTRL,0x04,rev_id,7:4,RO,0xA,,CFG_REGWEN
IRQ_STATE,0x08,pending,3:0,RW1C,0xF,,
KEY_WO,0x0C,secret,7:0,WO,0x0,,
",
    )
    .unwrap();

    {
        let sh = Shell::new(Session::new(eng.clone(), dir.join("work_csv")), Flavor::Jg);
        sh.eval(&format!(
            r#"
            analyze -sv09 {{{sv}}}
            elaborate -top csr_block
            clock clk
            reset !rst_n
            check_csr -load {{{spec}}} -prove
            "#,
            sv = csr_ok_sv.display(),
            spec = regs_csv.display(),
        ))
        .unwrap();

        let session = sh.session.borrow();
        let csr_props: Vec<_> = session
            .props
            .iter()
            .filter(|p| p.task == "CSR" && p.kind == Kind::Assert)
            .collect();
        assert!(!csr_props.is_empty());
        for p in &csr_props {
            assert!(
                matches!(p.status, Status::Proven { .. }),
                "CSV CSR property `{}` ({}) did not prove: {:?}",
                p.name,
                p.expr,
                p.status
            );
        }
    }

    // 1C. UVM RAL SystemVerilog (`uvm_reg_block` + `uvm_reg` + `uvm_reg_field::configure`).
    let regs_ral = dir.join("regs_ral.sv");
    std::fs::write(
        &regs_ral,
        r#"
class cfg_regwen_reg extends uvm_reg;
  rand uvm_reg_field cfg_regwen;
  function new(string name = "cfg_regwen_reg");
    super.new(name, 32, UVM_NO_COVERAGE);
  endfunction
  virtual function void build();
    cfg_regwen = uvm_reg_field::type_id::create("cfg_regwen");
    cfg_regwen.configure(this, 1, 0, "W0C", 0, 1'h1, 1, 1, 0);
  endfunction
endclass

class irq_state_reg extends uvm_reg;
  rand uvm_reg_field pending;
  function new(string name = "irq_state_reg");
    super.new(name, 32, UVM_NO_COVERAGE);
  endfunction
  virtual function void build();
    pending = uvm_reg_field::type_id::create("pending");
    pending.configure(this, 4, 0, "W1C", 0, 4'hF, 1, 1, 0);
  endfunction
endclass

class key_wo_reg extends uvm_reg;
  rand uvm_reg_field secret;
  function new(string name = "key_wo_reg");
    super.new(name, 32, UVM_NO_COVERAGE);
  endfunction
  virtual function void build();
    secret = uvm_reg_field::type_id::create("secret");
    secret.configure(this, 8, 0, "WO", 0, 8'h00, 1, 1, 0);
  endfunction
endclass

class csr_ral_block extends uvm_reg_block;
  rand cfg_regwen_reg cfg_regwen;
  rand irq_state_reg  irq_state;
  rand key_wo_reg     key_wo;

  virtual function void build();
    default_map = create_map("default_map", 'h0, 4, UVM_LITTLE_ENDIAN);
    cfg_regwen = cfg_regwen_reg::type_id::create("cfg_regwen");
    cfg_regwen.build();
    default_map.add_reg(cfg_regwen, 8'h00, "RW");

    irq_state = irq_state_reg::type_id::create("irq_state");
    irq_state.build();
    default_map.add_reg(irq_state, 8'h08, "RW");

    key_wo = key_wo_reg::type_id::create("key_wo");
    key_wo.build();
    default_map.add_reg(key_wo, 8'h0C, "WO");
  endfunction
endclass
"#,
    )
    .unwrap();

    {
        let sh = Shell::new(Session::new(eng.clone(), dir.join("work_ral")), Flavor::Jg);
        sh.eval(&format!(
            r#"
            analyze -sv09 {{{sv}}}
            elaborate -top csr_block
            clock clk
            reset !rst_n
            check_csr -load {{{spec}}} -prove
            "#,
            sv = csr_ok_sv.display(),
            spec = regs_ral.display(),
        ))
        .unwrap();

        let session = sh.session.borrow();
        let csr_props: Vec<_> = session
            .props
            .iter()
            .filter(|p| p.task == "CSR" && p.kind == Kind::Assert)
            .collect();
        assert_eq!(csr_props.len(), 12);
        for p in &csr_props {
            assert!(
                matches!(p.status, Status::Proven { .. }),
                "UVM RAL CSR property `{}` ({}) did not prove: {:?}",
                p.name,
                p.expr,
                p.status
            );
        }
    }

    // 1D. Buggy CSR RTL: CTRL.mode ignores CFG_REGWEN lock and IRQ_STATE.pending overwrites (RW) instead of W1C.
    let csr_bug_sv = dir.join("csr_bug.sv");
    std::fs::write(
        &csr_bug_sv,
        r#"
module csr_block (
  input  logic        clk,
  input  logic        rst_n,
  input  logic        we,
  input  logic        re,
  input  logic [7:0]  addr,
  input  logic [31:0] wdata,
  output logic [31:0] rdata
);
  logic       cfg_regwen_q;
  logic [3:0] ctrl_mode_q;
  logic [3:0] ctrl_rev_id_q;
  logic [3:0] irq_state_pending_q;
  logic [7:0] key_wo_secret_q;

  always_ff @(posedge clk) begin
    if (!rst_n) begin
      cfg_regwen_q        <= 1'b1;
      ctrl_mode_q         <= 4'h5;
      ctrl_rev_id_q       <= 4'hA;
      irq_state_pending_q <= 4'hF;
      key_wo_secret_q     <= 8'h00;
    end else if (we) begin
      case (addr)
        8'h00: cfg_regwen_q        <= cfg_regwen_q & wdata[0];
        8'h04: ctrl_mode_q         <= wdata[3:0]; // BUG 1: ignores cfg_regwen_q!
        8'h08: irq_state_pending_q <= wdata[3:0]; // BUG 2: RW overwrite instead of W1C!
        8'h0C: key_wo_secret_q     <= wdata[7:0];
        default: ;
      endcase
    end
  end

  always_comb begin
    rdata = 32'h0;
    if (re && !we) begin
      case (addr)
        8'h00: rdata = {31'd0, cfg_regwen_q};
        8'h04: rdata = {24'd0, ctrl_rev_id_q, ctrl_mode_q};
        8'h08: rdata = {28'd0, irq_state_pending_q};
        8'h0C: rdata = 32'h0;
        default: rdata = 32'h0;
      endcase
    end
  end
endmodule
"#,
    )
    .unwrap();

    {
        let sh = Shell::new(Session::new(eng, dir.join("work_bug")), Flavor::Jg);
        sh.eval(&format!(
            r#"
            analyze -sv09 {{{sv}}}
            elaborate -top csr_block
            clock clk
            reset !rst_n
            check_csr -load {{{spec}}} -prove
            "#,
            sv = csr_bug_sv.display(),
            spec = regs_hjson.display(),
        ))
        .unwrap();

        let session = sh.session.borrow();
        let find_status = |name: &str| {
            session
                .props
                .iter()
                .find(|p| p.name == name)
                .map(|p| &p.status)
        };
        assert!(
            matches!(
                find_status("CSR::csr_ctrl_mode_lock"),
                Some(Status::Cex { .. })
            ),
            "expected CEX on missing regwen lock, got {:?}",
            find_status("CSR::csr_ctrl_mode_lock")
        );
        assert!(
            matches!(
                find_status("CSR::csr_irq_state_pending_access"),
                Some(Status::Cex { .. })
            ),
            "expected CEX on RW-instead-of-W1C bug, got {:?}",
            find_status("CSR::csr_irq_state_pending_access")
        );
    }
}

#[test]
fn check_protocol_apb_axi4lite_tlul_and_why_cex() {
    let Some(eng) = all_engines() else { return };
    let dir = temp_dir("proto");

    // 2A. Compliant APB4 slave with 1 wait state and gated pslverr.
    let apb_sv = dir.join("apb_slave.sv");
    std::fs::write(
        &apb_sv,
        r#"
module apb_slave (
  input  logic        clk,
  input  logic        rst_n,
  input  logic        psel,
  input  logic        penable,
  input  logic        pwrite,
  input  logic [7:0]  paddr,
  input  logic [31:0] pwdata,
  output logic        pready,
  output logic        pslverr,
  output logic [31:0] prdata
);
  logic        wait_q;
  logic [31:0] reg0_q;

  always_ff @(posedge clk) begin
    if (!rst_n) begin
      wait_q <= 1'b0;
      reg0_q <= 32'd0;
    end else begin
      if (psel && !penable)
        wait_q <= 1'b1;
      else if (psel && penable && wait_q)
        wait_q <= 1'b0;

      if (psel && penable && pready && pwrite && (paddr == 8'h00))
        reg0_q <= pwdata;
    end
  end

  assign pready  = psel && penable && !wait_q;
  assign pslverr = pready && (paddr != 8'h00);
  assign prdata  = (paddr == 8'h00) ? reg0_q : 32'd0;
endmodule
"#,
    )
    .unwrap();

    {
        let sh = Shell::new(Session::new(eng.clone(), dir.join("work_apb")), Flavor::Jg);
        sh.eval(&format!(
            r#"
            analyze -sv09 {{{sv}}}
            elaborate -top apb_slave
            clock clk
            reset !rst_n
            check_protocol -create -protocol apb -role slave -prove
            "#,
            sv = apb_sv.display(),
        ))
        .unwrap();

        let session = sh.session.borrow();
        for p in session.props.iter().filter(|p| p.task == "Protocol") {
            match p.kind {
                Kind::Assert => assert!(
                    matches!(p.status, Status::Proven { .. }),
                    "{} failed: {:?}",
                    p.name,
                    p.status
                ),
                Kind::Cover => assert!(
                    matches!(p.status, Status::Covered { .. }),
                    "cover {} not hit: {:?}",
                    p.name,
                    p.status
                ),
                Kind::Assume => {}
            }
        }
    }

    // 2B. Compliant AXI4-Lite slave.
    let axil_sv = dir.join("axil_slave.sv");
    std::fs::write(
        &axil_sv,
        r#"
module axil_slave (
  input  logic        clk,
  input  logic        rst_n,
  input  logic        awvalid,
  output logic        awready,
  input  logic [7:0]  awaddr,
  input  logic        wvalid,
  output logic        wready,
  input  logic [31:0] wdata,
  output logic        bvalid,
  input  logic        bready,
  output logic [1:0]  bresp,
  input  logic        arvalid,
  output logic        arready,
  input  logic [7:0]  araddr,
  output logic        rvalid,
  input  logic        rready,
  output logic [31:0] rdata,
  output logic [1:0]  rresp
);
  logic [31:0] mem_q;
  logic        bvalid_q;
  logic [1:0]  bresp_q;
  logic        rvalid_q;
  logic [31:0] rdata_q;
  logic [1:0]  rresp_q;

  assign awready = !bvalid_q && awvalid && wvalid;
  assign wready  = !bvalid_q && awvalid && wvalid;
  assign arready = !rvalid_q;

  assign bvalid = bvalid_q;
  assign bresp  = bresp_q;
  assign rvalid = rvalid_q;
  assign rdata  = rdata_q;
  assign rresp  = rresp_q;

  always_ff @(posedge clk) begin
    if (!rst_n) begin
      mem_q    <= 32'd0;
      bvalid_q <= 1'b0;
      bresp_q  <= 2'b00;
      rvalid_q <= 1'b0;
      rdata_q  <= 32'd0;
      rresp_q  <= 2'b00;
    end else begin
      if (awready && awvalid) begin
        mem_q    <= wdata;
        bvalid_q <= 1'b1;
        bresp_q  <= (awaddr == 8'h00) ? 2'b00 : 2'b10;
      end else if (bvalid_q && bready) begin
        bvalid_q <= 1'b0;
      end

      if (arready && arvalid) begin
        rvalid_q <= 1'b1;
        rdata_q  <= mem_q;
        rresp_q  <= (araddr == 8'h00) ? 2'b00 : 2'b10;
      end else if (rvalid_q && rready) begin
        rvalid_q <= 1'b0;
      end
    end
  end
endmodule
"#,
    )
    .unwrap();

    {
        let sh = Shell::new(Session::new(eng.clone(), dir.join("work_axil")), Flavor::Jg);
        sh.eval(&format!(
            r#"
            analyze -sv09 {{{sv}}}
            elaborate -top axil_slave
            clock clk
            reset !rst_n
            check_protocol -create -protocol axi4lite -role slave -prove
            "#,
            sv = axil_sv.display(),
        ))
        .unwrap();

        let session = sh.session.borrow();
        for p in session.props.iter().filter(|p| p.task == "Protocol") {
            match p.kind {
                Kind::Assert => assert!(
                    matches!(p.status, Status::Proven { .. }),
                    "{} failed: {:?}",
                    p.name,
                    p.status
                ),
                Kind::Cover => assert!(
                    matches!(p.status, Status::Covered { .. }),
                    "cover {} not hit: {:?}",
                    p.name,
                    p.status
                ),
                Kind::Assume => {}
            }
        }
    }

    // 2C. Compliant OpenTitan TileLink-UL (`tlul`) device.
    let tlul_sv = dir.join("tlul_slave.sv");
    std::fs::write(
        &tlul_sv,
        r#"
module tlul_slave (
  input  logic        clk,
  input  logic        rst_n,
  input  logic        a_valid,
  output logic        a_ready,
  input  logic [2:0]  a_opcode,
  input  logic [7:0]  a_address,
  input  logic [31:0] a_data,
  input  logic [3:0]  a_mask,
  input  logic [1:0]  a_size,
  input  logic [3:0]  a_source,
  output logic        d_valid,
  input  logic        d_ready,
  output logic [2:0]  d_opcode,
  output logic [31:0] d_data,
  output logic [3:0]  d_source,
  output logic        d_error
);
  logic [31:0] reg_q;
  logic        d_valid_q;
  logic [2:0]  d_opcode_q;
  logic [31:0] d_data_q;
  logic [3:0]  d_source_q;
  logic        d_error_q;

  assign a_ready  = !d_valid_q;
  assign d_valid  = d_valid_q;
  assign d_opcode = d_opcode_q;
  assign d_data   = d_data_q;
  assign d_source = d_source_q;
  assign d_error  = d_error_q;

  always_ff @(posedge clk) begin
    if (!rst_n) begin
      reg_q      <= 32'd0;
      d_valid_q  <= 1'b0;
      d_opcode_q <= 3'd0;
      d_data_q   <= 32'd0;
      d_source_q <= 4'd0;
      d_error_q  <= 1'b0;
    end else begin
      if (a_valid && a_ready) begin
        d_valid_q  <= 1'b1;
        d_source_q <= a_source;
        d_error_q  <= (a_address != 8'h00);
        if (a_opcode == 3'd4) begin
          d_opcode_q <= 3'd1; // AccessAckData
          d_data_q   <= reg_q;
        end else begin
          d_opcode_q <= 3'd0; // AccessAck
          d_data_q   <= 32'd0;
          if (a_address == 8'h00)
            reg_q <= a_data;
        end
      end else if (d_valid_q && d_ready) begin
        d_valid_q <= 1'b0;
      end
    end
  end
endmodule
"#,
    )
    .unwrap();

    {
        let sh = Shell::new(Session::new(eng.clone(), dir.join("work_tlul")), Flavor::Jg);
        sh.eval(&format!(
            r#"
            analyze -sv09 {{{sv}}}
            elaborate -top tlul_slave
            clock clk
            reset !rst_n
            check_protocol -create -protocol tlul -role slave -prove
            "#,
            sv = tlul_sv.display(),
        ))
        .unwrap();

        let session = sh.session.borrow();
        for p in session.props.iter().filter(|p| p.task == "Protocol") {
            match p.kind {
                Kind::Assert => assert!(
                    matches!(p.status, Status::Proven { .. }),
                    "{} failed: {:?}",
                    p.name,
                    p.status
                ),
                Kind::Cover => assert!(
                    matches!(p.status, Status::Covered { .. }),
                    "cover {} not hit: {:?}",
                    p.name,
                    p.status
                ),
                Kind::Assume => {}
            }
        }
    }

    // 2D. Buggy AXI4-Lite master that times out and drops `awvalid` while `!awready` -> CEX + why_cex!
    let axil_master_bug_sv = dir.join("axil_master_bug.sv");
    std::fs::write(
        &axil_master_bug_sv,
        r#"
module axil_master_bug (
  input  logic        clk,
  input  logic        rst_n,
  input  logic        start_wr,
  output logic        awvalid,
  input  logic        awready,
  output logic [7:0]  awaddr,
  output logic        wvalid,
  input  logic        wready,
  output logic [31:0] wdata,
  input  logic        bvalid,
  output logic        bready,
  output logic        arvalid,
  input  logic        arready,
  output logic [7:0]  araddr,
  input  logic        rvalid,
  output logic        rready,
  input  logic [31:0] rdata
);
  logic aw_q;
  logic w_q;
  logic stall_timeout_q;

  assign awvalid = aw_q;
  assign awaddr  = 8'h10;
  assign wvalid  = w_q;
  assign wdata   = 32'hCAFE_BABE;
  assign bready  = 1'b1;
  assign arvalid = 1'b0;
  assign araddr  = 8'h00;
  assign rready  = 1'b1;

  always_ff @(posedge clk) begin
    if (!rst_n) begin
      aw_q            <= 1'b0;
      w_q             <= 1'b0;
      stall_timeout_q <= 1'b0;
    end else begin
      if (!aw_q && start_wr) begin
        aw_q            <= 1'b1;
        w_q             <= 1'b1;
        stall_timeout_q <= 1'b0;
      end else if (aw_q) begin
        if (awready) begin
          aw_q            <= 1'b0;
          stall_timeout_q <= 1'b0;
        end else if (stall_timeout_q) begin
          // PROTOCOL BUG: master aborts AWVALID before AWREADY!
          aw_q            <= 1'b0;
          stall_timeout_q <= 1'b0;
        end else begin
          stall_timeout_q <= 1'b1;
        end
      end

      if (w_q && wready)
        w_q <= 1'b0;
    end
  end
endmodule
"#,
    )
    .unwrap();

    {
        let sh = Shell::new(Session::new(eng, dir.join("work_master_bug")), Flavor::Jg);
        sh.eval(&format!(
            r#"
            analyze -sv09 {{{sv}}}
            elaborate -top axil_master_bug
            clock clk
            reset !rst_n
            check_protocol -create -protocol axi4lite -role master -prove
            "#,
            sv = axil_master_bug_sv.display(),
        ))
        .unwrap();

        {
            let session = sh.session.borrow();
            let hold = session
                .props
                .iter()
                .find(|p| p.name == "Protocol::axi4lite_aw_valid_hold")
                .expect("missing Protocol::axi4lite_aw_valid_hold");
            assert!(
                matches!(hold.status, Status::Cex { .. }),
                "expected CEX on aw_valid_hold, got {:?}",
                hold.status
            );
        }

        // Verify causal explanation backend (`why_cex -json`) on the protocol CEX!
        let why_raw = sh
            .eval("why_cex -json {Protocol::axi4lite_aw_valid_hold}")
            .unwrap();
        let why_json: serde_json::Value = serde_json::from_str(&why_raw).unwrap();
        assert_eq!(why_json["property"], "Protocol::axi4lite_aw_valid_hold");
        assert_eq!(why_json["status"], "cex");
        let proto_js: serde_json::Value =
            serde_json::from_str(&sh.eval("check_protocol -report -json").unwrap()).unwrap();
        assert_eq!(proto_js["schema"], "informal.protocol/1");
        let active: Vec<&str> = why_json["active_signals"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            active
                .iter()
                .any(|s| s.ends_with("stall_timeout_q") || s.ends_with("aw_q")),
            "expected stall_timeout_q / aw_q in causal slice, got {active:?}"
        );
    }
}
