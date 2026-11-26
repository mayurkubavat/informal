# InFormal Documentation

Welcome to the InFormal documentation set. Like commercial formal verification suites (JasperGold, VC Formal), the documentation is organized into **four books** plus a **runnable lab curriculum**, with the Command Reference Manual generated directly from source code (`informal docgen`) so documentation never drifts from the tool.

---

## Choose Your Path

- **New to InFormal or FPV?** Start with [Getting Started](guide/getting-started.md) and run the [Hands-On Labs](labs/README.md) (`01-quickstart` → `02-fpv-fundamentals`).
- **Migrating existing JasperGold / VC Formal flows?** Jump to [Vendor Tcl Migration](integration/vendor-migration.md) and the [JG ↔ VCF Command Crosswalk](reference/jg-vcf-crosswalk.md).
- **Scaling proofs or running Formal Apps?** Read [Convergence & Engines](guide/convergence-and-engines.md) and [Formal Apps](guide/formal-apps.md).
- **Wiring CI or AI coding agents?** See [CI & Batch Flows](integration/ci-and-batch.md) and [AI & MCP Integration](integration/ai-and-mcp.md).

---

## Book 1 — User Guide

Methodology, workflows, and debugging for learners through expert formal verification architects.

| Guide | Topics |
| --- | --- |
| [Getting Started](guide/getting-started.md) | Prerequisites, building InFormal & optional solvers (CaDiCaL, Bitwuzla, rIC3, ABC), first proof, launching `InFormal.app` |
| [FPV Methodology](guide/fpv-methodology.md) | Clocks, reset modeling, RTL vs. Tcl SVA properties, tasks, witness covers, vacuity, `check_assumptions`, 2QBF dead-end checks, `assume -from_assert` |
| [Debugging Counterexamples](guide/debugging-cex.md) | `why_cex` causal slicing, waveforms (`visualize` & VCD), RTL cone schematics, source overlays, `SVA-FPV` lint & quick-fixes |
| [Convergence & Engines](guide/convergence-and-engines.md) | In-house & external portfolio (`bmc`, `kind`, `ic3`, `klive`, `wbmc`, `wkind`), `get_fv_complexity`, `blackbox_assistant`, `stopat`, `abstract -counter`, `informal split`, proof cache |
| [Formal Apps](guide/formal-apps.md) | `check_cov` (COI/UNR), `check_conn`, `check_xprop`, `check_spv`, `check_sec` (SEC/LEC), `check_csr` (HJSON/CSV/RAL), `check_protocol` (APB/AXI4-Lite/TL-UL) |
| [Certificates & Signoff](guide/certificates-and-signoff.md) | Independent proof/CEX checking (Certifaiger, `aigsim`, `btorsim`) and machine-verifiable verification plans (`fvp.spec.json` + `spec_audit`) |
| [Desktop Workbench UI](guide/workbench-ui.md) | Tauri 2 + React workbench layouts (`Setup`, `Prove`, `CEX Debug`, `Formal Apps`), cross-probing, Proposals tab, Ask mode |

---

## Book 2 — Tool Integration & Administration

Integrating InFormal into RTL build systems, regressions, vendor scripts, and AI agent workflows.

| Guide | Topics |
| --- | --- |
| [Vendor Tcl Migration](integration/vendor-migration.md) | Running JasperGold & VC Formal scripts unchanged, `informal shell --check [--json]`, filelists (`-f`, `+define+`, `+incdir+`), ignored vs. rejected options |
| [CI & Batch Flows](integration/ci-and-batch.md) | Exit codes, CSV/JSON reports, proof cache reuse across CI jobs, `--mem-gb` / cgroup limits, `--isolate`, Makefile & GitHub Actions recipes |
| [Engine & LLM Configuration](integration/engines-config.md) | `config/engines.toml`, `config/llm.toml`, `INFORMAL_*` environment variables, macOS vs. Linux rIC3 notes |
| [AI & MCP Integration](integration/ai-and-mcp.md) | `informal mcp`, Antigravity plugin, Claude Code & Cursor configs, live `informal mcp --connect`, MCP App side-pane, MCP Prompts, CEGIS workflow |

---

## Book 3 — Command Reference Manual

Auto-generated directly from `informal-cli`, `informal-fpv`, and `informal-mcp` via `informal docgen` (verified by `informal docgen --check` in `scripts/check.sh`).

| Reference | Scope |
| --- | --- |
| [CLI Reference Manual](reference/cli.md) | All `informal` subcommands, flags, defaults, exit codes, and environment variables |
| [Tcl Command Reference](reference/tcl-commands.md) | All 75 `informal shell` commands (`common`, `jg`, `vcf`) — identical to interactive `help <cmd>` |
| [JG ↔ VCF Command Crosswalk](reference/jg-vcf-crosswalk.md) | Side-by-side mapping across JasperGold, VC Formal, and flavor-independent commands |
| [SVA & Lowering Support](reference/sva-support.md) | Supported IEEE 1800-2023 SVA operators, liveness, local variables, and explicit lowering rules |
| [MCP Tools & Prompts Reference](reference/mcp-tools.md) | All 48 MCP tools (36 core + 12 `gui_*`), 4 live-hydrated prompts, and `informal.error/1` schema |
| [Formal Plan Spec Schema](reference/spec-schema.md) | `fvp.spec.json` (`informal.spec/1`) contract and `spec_audit` verdicts |
| [Machine-Readable Index](reference/index.json) | Unified JSON catalog of CLI, Tcl, MCP tools, and prompts (also summarized in [llms.txt](../llms.txt)) |

---

## Book 4 — Hands-On Labs (`examples/`)

Self-contained, gate-tested RTL + Tcl + spec designs documented in [Hands-On Labs](labs/README.md) (source under `examples/`):

1. **[01 — Quickstart Counter](labs/01-quickstart.md)** — Counter proof, CEX, cover, unreachable cover (both JasperGold & VC Formal scripts)
2. **[02 — FPV Fundamentals (FIFO)](labs/02-fpv-fundamentals.md)** — Synchronous FIFO: SVA temporal checks, witness covers, vacuity, `check_assumptions -dead_end`, `why_cex`, tasks
3. **[03 — Convergence & Abstractions](labs/03-convergence.md)** — Wide watchdog & multiplier: `get_fv_complexity`, `blackbox_assistant`, `stopat`, `abstract -counter`, `assume -from_assert`, proof cache
4. **[04 — Formal Apps Suite](labs/04-formal-apps.md)** — Seven dedicated mini-labs (`cov`, `conn`, `xprop`, `spv`, `sec`, `csr`, `protocol`)
5. **[05 — CI, Certificates & Signoff](labs/05-integration-and-signoff.md)** — `fvp.spec.json`, `Makefile` CI gating, `shell --check`, independent `--certificate` verification

---

## Deep Dives & Internals

Existing design specifications, research notes, and developer references kept at stable paths:

- **Core subsystem deep dives:** [FPV Shell](fpv-shell.md) · [AI Interface](ai-interface.md) · [Engines](engines.md) · [Vacuity](vacuity.md) · [Certificates](certificates.md) · [Third-Party Tools](third-party.md)
- **Desktop UI internals:** [Ask Mode](ui/ask-mode.md) · [Stop & Progress Contract](ui/stop-progress.md) · [Trace Format](ui/trace-format.md) · [UI Testing](ui/testing.md) · [EDA GUI References](ui/eda-gui-references.md)
- **Architecture & research logs:** [Design Decisions](decisions.md) · [IC3 Research](ic3-research.md) · [Engine Comparisons](compare.md) · [Vendor Script Tally](vendor-script-tally.md) · [Literature Notes](literature-2026-11.md) · [Research Prompt Decisions](research-prompt-decisions.md) · [Contributing](https://github.com/mayurkubavat/informal/blob/main/CONTRIBUTING.md)
