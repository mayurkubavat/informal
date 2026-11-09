# FormalDV

An internal formal verification tool for digital designs: a Rust
orchestrator over a portfolio of model-checking engines, with an SVA front
end, verification apps and a debug UI on top.

The plan this code follows is the research report and phased roadmap:
Formal Verification Tool: Research Report & Architecture Plan (kept outside this repository).
Background research (including the Gemini Deep Research reports) is kept
in `research/` on the Mac copy only; it is not in this repository.

## Status: phase 1 (FPV MVP) in progress

Phase 0 (wrapped engines reproduce published HWMCC'24 results) is done.
Phase 1 has the in-house bit-level engines (BMC, k-induction, IC3), the
slang front end, the JasperGold / VC Formal Tcl shell and the desktop app.
On the 319 HWMCC'24 bit-level models (300 s per run), the in-house
engines have solved 204 across runs with no wrong verdicts, level with
rIC3's portfolio.

| Crate | What it does |
| --- | --- |
| `fdv-ir` | Engine-side model IR. Lossless Btor2 reader and writer with validation; structural features for the run database and engine selector. |
| `fdv-engine` | The engine plug-in API (`Engine` trait, capabilities, progress, cancellation) and the parallel portfolio runner, which takes the first definitive verdict, cancels the rest and flags engines that contradict each other. |
| `fdv-engines-ext` | Adapters that run external model checkers (rIC3, Pono, ABC, BtorMC) as child processes, described in `config/engines.toml`. |
| `fdv-bench` | Benchmark harness. Appends one JSON line per engine run to the run database and scores engines by solved count and PAR-2, including a virtual-best row. |
| `fdv-sat` | SAT interface: CaDiCaL through its C API, plus a small DPLL solver for tests. |
| `fdv-aig` | Bit-level model: AIG with structural hashing, AIGER reader and writer, Btor2 bit-blaster, preprocessing. |
| `fdv-bmc` | In-house engines on the AIG: BMC (also stepped), k-induction, IC3 / PDR (single and multi-threaded with lemma sharing), and IC3 lemmas feeding k-induction. Engine list in `docs/engines.md`. |
| `fdv-slang` | SystemVerilog front end on slang (C++ shim through FFI): diagnostics, top module, design hierarchy, name checks, and lowering to Btor2 (`fdv lower`). |
| `fdv-bitwuzla` | Word-level engines on Bitwuzla (C++ shim through FFI): BMC over Btor2 with arrays. |
| `fdv-tcl` | Embedded Tcl 8.6 (the C library through FFI) for fdv scripts. |
| `fdv-fpv` | FPV sessions: design setup, SVA subset compiler, prove over the portfolio, and the JasperGold / VC Formal Tcl compatibility layer (`docs/fpv-shell.md`). |
| `fdv-llm` | Swappable LLM providers (OpenAI-compatible streaming client, scripted fake) for the app's Ask mode; defaults to local Gemma on Ollama (`config/llm.toml`). |
| `fdv-cli` | The `fdv` command. |

The desktop app (Tauri 2 with a React UI) lives in `ui/`, outside the
workspace; see `ui/README.md` and `docs/ui/`.

## Build and test

Needs a recent stable Rust toolchain (`rustup`; 1.87 or newer) and the
Tcl 8.6 development library (`tcl8.6-dev` on Ubuntu, `tcl-tk@8` from
Homebrew on macOS). Optional libraries are picked up when built under
`~/Silicon/DV/tools` (CaDiCaL, Bitwuzla, slang; see
`docs/third-party.md`); without them the matching engines or front end are
left out. Yosys is the fallback RTL front end for `fdv shell`.

```sh
cargo build --release
cargo test
scripts/check.sh        # fmt, clippy, all Rust and UI tests: run before merging
```

## Use

```sh
# Validate a Btor2 model and print its features
target/release/fdv info tests/models/counter.btor2

# Canonical Btor2
target/release/fdv fmt tests/models/memory.btor2

# Check one model with the engine portfolio (exit 10 unsafe, 20 safe, 0 unknown)
target/release/fdv check tests/models/toggle_unsafe.aag --timeout 60

# Benchmark every engine on a model set and log to bench/runs.jsonl
target/release/fdv bench path/to/hwmcc24 --each --timeout 300 --jobs 8

# Run a JasperGold or VC Formal style FPV script
target/release/fdv shell tests/fpv/counter/counter.jg.tcl
```

External engines must be on `PATH` under the names in
`config/engines.toml` (`ric3`, `abc`; the `pono` and `btormc` entries are
unchecked and those tools are not installed here). rIC3 builds on stable Rust; rIC3, Pono
and ABC all build from source (see `docs/engines.md`, which also covers
macOS limits such as rIC3's Linux-only portfolio).

## Layout

```
crates/        Rust workspace members
ui/            desktop app (Tauri + React), its tests and browser mock
config/        engine and LLM provider definitions
tests/models/  small Btor2 and AIGER models used by the tests
tests/fpv/     FPV designs and JasperGold / VC Formal style scripts
scripts/       benchmark drivers and result checkers (HWMCC'24)
bench/         model lists; run databases and benchmark sets are git-ignored
docs/          design notes (engines, FPV shell, UI, third-party tools)
refs/          reference library: papers and specs (see refs/README.md)
```

## Phase 0 gate

Wrapped engines must reproduce their published HWMCC solve counts when run
through `fdv bench`. That validates the adapters and verdict patterns
before any in-house engine is compared against them.
