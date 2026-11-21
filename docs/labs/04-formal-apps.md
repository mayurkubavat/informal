# Lab 04 — Formal Apps Suite

**Level:** Intermediate–Expert · **Directory:** `examples/04-formal-apps/`

Seven self-contained mini-labs exercising every built-in Formal App. Each completes in under 2.5 seconds. See [Formal Apps Guide](../guide/formal-apps.md) for conceptual details.

| App | Source directory | Run command | What it verifies |
| --- | --- | --- | --- |
| **Coverage & UNR** | `examples/04-formal-apps/cov/` | `informal shell examples/04-formal-apps/cov/run.jg.tcl` | COI observability (`checked_q` covered, `shadow_unobs_q` waived), branch UNR (`mode == 2'b11`), toggle UNR (`stuck_zero_q`) |
| **Connectivity** | `examples/04-formal-apps/conn/` | `informal shell examples/04-formal-apps/conn/run.jg.tcl` | OpenTitan-style `conn.csv`: `mux_conn` (`proven`), `pipe_conn` with `LATENCY 1` (`proven`), `bad_conn` LSB bug (`cex`) |
| **X-Propagation** | `examples/04-formal-apps/xprop/` | `informal shell examples/04-formal-apps/xprop/run.jg.tcl` | Exact 2-copy X-prop: `clean_out` & reconvergent mux `reconv_out` (`proven`, no mux pessimism), reset-less `leaky_out` (`cex` → waived) |
| **Security Path (SPV)** | `examples/04-formal-apps/spv/` | `informal shell examples/04-formal-apps/spv/run.jg.tcl` | 2-safety non-interference: `iso_pub` & conditional `iso_dbg_locked` (`proven`), `leak_dbg_open` & 2-cycle covert storage channel `leak_covert_2cyc` (`cex`) |
| **Equivalence (SEC / LEC)** | `examples/04-formal-apps/sec/` | `informal shell examples/04-formal-apps/sec/run.jg.tcl` | Sequential SEC (`imp_ok`), combinational `-lec`, retimed `-latency 1` (`imp_pipe`), corner bug (`imp_bug` at `a == 8'hA5` → side-by-side VCD + `why_cex`) |
| **CSR / RAL** | `examples/04-formal-apps/csr/` | `informal shell examples/04-formal-apps/csr/run.jg.tcl` | 21 auto-synthesized assertions from OpenTitan `regs.hjson` (`CFG_REGWEN` RW0C, `CTRL` RW/RO + `regwen`, `IRQ_STATE` RW1C, `KEY_WO` WO) — all `proven` |
| **Bus Protocol VIP** | `examples/04-formal-apps/protocol/` | `informal shell examples/04-formal-apps/protocol/run.jg.tcl` | 1-command APB4 slave compliance suite (`check_protocol -create -protocol apb -role slave -prove`) |
