# Formal Apps Guide

InFormal ships **seven built-in Formal Apps** accessible in both JasperGold (`jg`) and VC Formal (`vcf`) Tcl flavors, through the MCP server (`check_*` tools), and in the Desktop Workbench's **`Formal Apps`** panel. Each app synthesizes formal properties automatically from a high-level specification or structural analysis so you do not have to hand-write boilerplate SVA.

| App | Tcl / MCP Command | Purpose | Runnable Lab |
| --- | --- | --- | --- |
| **Coverage & UNR** | `check_cov` | COI observability, branch/toggle dead-code (UNR), waivers, HTML reports | `examples/04-formal-apps/cov/` |
| **Connectivity** | `check_conn` | Pin-to-pin, muxed (`CONDITION`), and pipelined (`LATENCY`) CSV checks | `examples/04-formal-apps/conn/` |
| **X-Propagation** | `check_xprop` | Exact 2-copy self-composition X-reachability (no mux X-pessimism) | `examples/04-formal-apps/xprop/` |
| **Security Path (SPV)** | `check_spv` | 2-safety non-interference catching direct & multi-cycle covert leaks | `examples/04-formal-apps/spv/` |
| **Equivalence (SEC / LEC)** | `check_sec` | Sequential, pipelined (`-latency N`), and combinational (`-lec`) checks | `examples/04-formal-apps/sec/` |
| **CSR / RAL** | `check_csr` | Auto-generated register checks from OpenTitan HJSON, CSV, or UVM RAL `.sv` | `examples/04-formal-apps/csr/` |
| **Bus Protocol VIP** | `check_protocol` | 1-command formal assertions & assumptions for APB, AXI4-Lite, and TL-UL | `examples/04-formal-apps/protocol/` |

---

## 1. Formal Coverage & UNR (`check_cov`)

Measures how completely your assertions observe the design (**COI**), proves which RTL branches and flop bit toggles are mathematically **Unreachable (UNR)** under your environment assumptions, tracks functional covers, and supports waivers + HTML reports.

```tcl
check_cov -init -model {coi branch toggle}
prove -all
check_cov -measure
check_cov -waiver -add -type coi -instance shadow_unobs_q -comment {debug shadow}
check_cov -report
check_cov -report -html -report_file cov.html -no_return -force
```

---

## 2. Formal Connectivity (`check_conn`)

Verifies SoC-level muxing, tie-offs, and pipelined wiring from JasperGold / OpenTitan-style CSV specs into task `Connectivity`:

```csv
# OpenTitan-style connectivity spec
CONNECTION, mux_conn, soc_top.u_src, sig_o, soc_top.u_mux_dst, sig_i
CONDITION, , scan_en, 1'b1, 1'b0
CONDITION, soc_top, mode, 2'b01, 2'b10

CONNECTION, pipe_conn, u_src, sig_o, u_pipe_dst, sig_i
LATENCY, 1, 1
```

```tcl
check_conn -load conn.csv
check_conn -list
prove -task Connectivity
check_conn -report
```

---

## 3. Formal X-Propagation (`check_xprop`)

Uses **exact 2-copy self-composition** over reset-less flops, X literals, and undriven nets to prove whether an unknown power-on value can ever affect an output or flop. Unlike ternary 4-state simulation, 2-copy miter checking avoids false X-pessimism on reconvergent muxes (e.g., `x ? 8'hA5 : 8'hA5` proves X-clean).

```tcl
check_xprop -init -targets {outputs} -prove
check_xprop -report
check_xprop -waiver -add -instance leaky_out -comment {datapath qualified by valid}
```

---

## 4. Security Path Verification (`check_spv`)

Proves **2-safety non-interference** between a sensitive source (`-from`, e.g., `secret_key`) and an observable sink (`-to`, e.g., `debug_port_o`), optionally guarded by a security mode predicate (`-cond`). Catches both direct combinational leaks and multi-cycle covert storage channels.

```tcl
check_spv -create -name iso_pub        -from secret_key -to pub_status_o
check_spv -create -name iso_dbg_locked -from secret_key -to debug_port_o -cond {!debug_unlock}
check_spv -create -name leak_covert    -from secret_key -to covert_port_o
check_spv -prove
check_spv -report
```

---

## 5. Sequential & Combinational Equivalence (`check_sec`)

Compares a reference specification RTL (`-spec`) against an optimized, clock-gated, or retimed implementation (`-imp`). Supports:
- **Sequential equivalence (`SEC`):** proves outputs match cycle-for-cycle after reset.
- **Pipelined / retimed equivalence (`-latency N`):** proves `imp` matches `spec` delayed by `N` cycles.
- **Combinational keypoint equivalence (`-lec`):** proves state next-value and output cones match directly.
- **Side-by-side mismatch debug:** writes a VCD containing `shared.*`, `spec.*`, and `imp.*` scopes and supports `why_cex`.

```tcl
# Sequential equivalence
check_sec -setup -spec spec.sv -imp imp_ok.sv -clock clk -reset {!rst_n} -prove
check_sec -report

# Retimed 2-stage implementation (-latency 1)
check_sec -setup -latency 1 -spec spec.sv -imp imp_pipe.sv -clock clk -reset {!rst_n} -prove

# Combinational keypoint LEC
check_sec -setup -lec -spec spec.sv -imp imp_ok.sv -clock clk -prove
```

---

## 6. CSR / RAL Verification (`check_csr`)

Parses OpenTitan **HJSON**, **CSV**, or UVM **`uvm_reg_block` `.sv`** register descriptions and automatically generates and proves properties in task `CSR` covering:
- Reset values for every field
- Read/write semantics (`RW`, `RO`, `RW1C`, `RW1S`, `RW0C`, `WO`, `RC`)
- `regwen` write-enable lock protection
- Non-aliasing across register addresses

```tcl
check_csr -load regs.hjson -prove
check_csr -report
check_csr -report -json
```

If your CSR block uses non-default bus port names, pass `-prefix <P>` or explicit port flags (`-addr`, `-we`, `-re`, `-wdata`, `-rdata`, `-read_latency`).

---

## 7. Bus Protocol VIP (`check_protocol`)

Synthesizes protocol compliance assertions, witness covers, and matched legal peer assumptions in one command into task `Protocol`:

- **Protocols:** `apb` (APB4), `axi4lite` (AXI4-Lite), `tlul` (TileLink-UL)
- **Roles:** `slave` (asserts DUT responses, assumes legal master requests), `master` (asserts DUT requests, assumes legal slave responses), `monitor` (all checks as assertions)

```tcl
check_protocol -create -protocol apb -role slave -prove
check_protocol -report
```

Use `-prefix <P>` or `-map {proto_sig dut_sig ...}` when port names differ from standard bus conventions.

---

## Related References

- Exact options for every `check_*` command: [Tcl Command Reference](../reference/tcl-commands.md#formal-apps)
- MCP JSON schemas for driving Formal Apps via AI: [MCP Tools Reference](../reference/mcp-tools.md)
- Complete runnable mini-labs: [Lab 04 — Formal Apps Suite](../labs/04-formal-apps.md)
