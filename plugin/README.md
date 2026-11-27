# InFormal Plugin for Antigravity

Provides the `informal` Model Context Protocol (MCP) server (52 tools, 5 live-hydrated prompts), the interactive **InFormal Workbench MCP App** (`ui://informal/workbench.html` in the side pane and global sidebar), and the `informal-fpv` formal verification skill.

## Included Components
- **MCP Server (`informal`)**: Pure-Rust IEEE 1800-2023 SystemVerilog FPV engine portfolio, single-call `cex_diagnose` and `convergence_diagnose`, zero-side-effect 2QBF CEGIS `proposal_validate`, `why_cex` dynamic backward causal slicer, `schematic_get` RTL cone extractor, `formal_lint` (`SVA-FPV-001..005` + `formal_fix_apply` quick-fixes), `signal_nav` driver/load lookup, `spec_audit`, and all eight Formal Apps (`check_cov`, `check_conn`, `check_xprop`, `check_spv`, `check_sec`, `check_csr`, `check_protocol`, plus `load_upf` / `check_lp` / `lp_diagnose`).
- **MCP App (`open_workbench`)**: Interactive HiDPI `<canvas>` waveform viewer, CodeMirror 6 RTL viewer with live `@C<k>` trace value annotations, and ELK.js causal schematic viewer directly inside Antigravity.
- **Skill (`informal-fpv`)**: Verification methodology and Decision D17 soundness workflow for AI agents.
