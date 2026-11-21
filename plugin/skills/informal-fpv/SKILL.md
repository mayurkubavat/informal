---
name: informal-fpv
description: Drive InFormal IEEE 1800 SystemVerilog formal property verification, why_cex causal root-cause slicing, RTL cone schematics, formal-readiness linting, Formal Apps (COV, X-Prop, SPV, SEC, CSR, Protocol VIP), and the embedded InFormal MCP App workbench.
---

# InFormal Formal Verification & Interactive Workbench

Use the `informal` MCP server tools whenever the user wants to elaborate SystemVerilog RTL, prove SVA assertions/covers, debug counterexample (CEX) waveforms, inspect cone-of-influence schematics, or run Formal Apps.

## Core Workflow

1. **Load & Elaborate (`session_open` + `formal_lint`)**
   - Open a JasperGold / VC Formal `.tcl` setup script or pass `files`, `top`, `clock`, and `reset` directly to `session_open`.
   - Run `formal_lint` (`SV-FPV-001..005`, SVA, CDC, FSM) early to catch over-constraining `assume` statements on outputs/state or missing `disable iff` reset guards before proving.

2. **Prove & Open Interactive Side-Pane UI (`prove`, `open_workbench`)**
   - Run `prove` on all or selected properties. Tools carrying `ui://informal/workbench.html` (`open_workbench`, `prove`, `why_cex`, `trace_get`, `schematic_get`, `check_csr`, `check_sec`, `check_cov`) automatically render or refresh the interactive InFormal MCP App side pane.
   - Respect **Decision D17**: never claim a property holds without an engine `proven` verdict. When proposing assumptions or cut points (`proposal_add`), propose the weakest legal environment constraint; AI-added assumptions mark dependent proofs `under_ai`.

3. **Debug Failing Counterexamples (`why_cex` + `schematic_get` + `gui_*`)**
   - Call `why_cex` on any `cex` or `covered` property to compute the dynamic backward causal root-cause slice from the failure frame back to cycle 0 (pruning untaken mux arms).
   - Call `schematic_get` to inspect the port-aware RTL cone and arithmetic complexity hotspots (`fv_complexity`).
   - When connected to a live desktop window (`gui_*` tools present), visually guide the engineer using `gui_open_trace`, `gui_toggle_why_cex`, `gui_annotate_trace`, `gui_present_walkthrough`, `gui_set_view` (`layout_preset: "cex-debug"`), and `gui_capture_view` (standalone SVG export).
