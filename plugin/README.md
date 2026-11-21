# InFormal Plugin for Antigravity

Provides the `informal` Model Context Protocol (MCP) server, the interactive **InFormal Workbench MCP App** (`ui://informal/workbench.html` in the side pane and global sidebar), and the `informal-fpv` formal verification skill.

## Included Components
- **MCP Server (`informal`)**: Pure-Rust IEEE 1800 SystemVerilog FPV engine portfolio, `why_cex` dynamic backward causal slicer, `schematic_get` RTL cone extractor, `formal_lint` (`SV-FPV-001..005` + quick-fixes), `signal_nav` driver/load lookup, and Formal Apps (`check_cov`, `check_xprop`, `check_spv`, `check_sec`, `check_csr`, `check_protocol`).
- **MCP App (`open_workbench`)**: Interactive HiDPI `<canvas>` waveform viewer, CodeMirror 6 RTL viewer with live trace value annotations, and ELK.js causal schematic viewer directly inside Antigravity.
- **Skill (`informal-fpv`)**: Verification methodology and D17 soundness workflow for AI agents.
