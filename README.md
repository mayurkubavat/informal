# InFormal

An open-source formal verification tool for digital designs: a Rust
orchestrator over a portfolio of model-checking engines, with an SVA front
end, verification apps and a debug UI on top. Formerly FormalDV: older
run databases and notes call the binary and in-house engines `fdv`.

The design decisions behind the code, and the issues still open, are in
`docs/decisions.md`.

## Status: phase 1 (FPV MVP) in progress

Phase 0 (wrapped engines reproduce published HWMCC'24 results) is done.
Phase 1 has the in-house bit-level engines (BMC, k-induction, IC3), the
svlang front end, the JasperGold / VC Formal Tcl shell and the desktop app.
On the 319 HWMCC'24 bit-level models (300 s per run), the in-house
engines have solved 204 across runs with no wrong verdicts, level with
rIC3's portfolio.

| Crate | What it does |
| --- | --- |
| `informal-ir` | Engine-side model IR. Lossless Btor2 reader and writer with validation; structural features for the run database and engine selector. |
| `informal-engine` | The engine plug-in API (`Engine` trait, capabilities, progress, cancellation) and the parallel portfolio runner, which takes the first definitive verdict, cancels the rest and flags engines that contradict each other. |
| `informal-engines-ext` | Adapters that run external model checkers (rIC3, Pono, ABC, BtorMC) as child processes, described in `config/engines.toml`. |
| `informal-bench` | Benchmark harness. Appends one JSON line per engine run to the run database and scores engines by solved count and PAR-2, including a virtual-best row. |
| `informal-sat` | SAT interface: CaDiCaL through its C API, plus a small DPLL solver for tests. |
| `informal-aig` | Bit-level model: AIG with structural hashing, AIGER reader and writer, Btor2 bit-blaster, preprocessing. |
| `informal-bmc` | In-house engines on the AIG: BMC (also stepped), k-induction, IC3 / PDR (single and multi-threaded with lemma sharing), and IC3 lemmas feeding k-induction. Engine list in `docs/engines.md`. |
| `informal-svlang` | SystemVerilog front end on svlang (pure Rust): diagnostics, top module, design hierarchy, and the word-level netlist written as Btor2 (`informal lower`). |
| `informal-slang` | Optional cross-check front end on slang (C++ shim through FFI), `INFORMAL_FRONTEND=slang`; also holds the shared request and design types. |
| `informal-bitwuzla` | Word-level engines on Bitwuzla (C++ shim through FFI): BMC over Btor2 with arrays. |
| `informal-tcl` | Embedded Tcl 8.6 (the C library through FFI) for informal scripts. |
| `informal-fpv` | FPV sessions: design setup, SVA subset compiler, prove over the portfolio, and the JasperGold / VC Formal Tcl compatibility layer (`docs/fpv-shell.md`). |
| `informal-llm` | Swappable LLM providers (OpenAI-compatible streaming client, scripted fake) for the app's Ask mode; defaults to local Gemma on Ollama (`config/llm.toml`). |
| `informal-cli` | The `informal` command. |

The desktop app (Tauri 2 with a React UI) lives in `ui/`, outside the
workspace; see `ui/README.md` and `docs/ui/`.

## Build and test

Needs a recent stable Rust toolchain (`rustup`; 1.87 or newer) and the
Tcl 8.6 development library (`tcl8.6-dev` on Ubuntu, `tcl-tk@8` from
Homebrew on macOS). Optional libraries are picked up when built under
`~/Silicon/DV/tools` (CaDiCaL, Bitwuzla, slang; see
`docs/third-party.md`); without them the matching engines are left out.
The SystemVerilog front end is svlang, a pure-Rust Cargo dependency.

```sh
cargo build --release
cargo test
scripts/check.sh        # fmt, clippy, all Rust and UI tests: run before merging
```

## Use

```sh
# Validate a Btor2 model and print its features
target/release/informal info tests/models/counter.btor2

# Canonical Btor2
target/release/informal fmt tests/models/memory.btor2

# Check one model with the engine portfolio (exit 10 unsafe, 20 safe, 0 unknown)
target/release/informal check tests/models/toggle_unsafe.aag --timeout 60

# Benchmark every engine on a model set and log to bench/runs.jsonl
target/release/informal bench path/to/hwmcc24 --each --timeout 300 --jobs 8

# Run a JasperGold or VC Formal style FPV script
target/release/informal shell tests/fpv/counter/counter.jg.tcl
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
through `informal bench`. That validates the adapters and verdict patterns
before any in-house engine is compared against them.

## License

Copyright 2026 Mayur Kubavat. Licensed under the Apache License,
Version 2.0 (`LICENSE`); see `NOTICE`.

InFormal does not include third-party source code. It links CaDiCaL,
Bitwuzla and slang (MIT) and the system Tcl, GMP and MPFR libraries, and
runs rIC3 and ABC as separate programs. `docs/third-party.md` lists each
one with its license and the exact commit used.

## Contributing

Contributions are welcome; see `CONTRIBUTING.md`. Contributors sign the
Contributor License Agreement in `CLA.md` once, before their first pull
request is merged.

## Trademarks

JasperGold and Xcelium are trademarks of Cadence Design Systems, Inc. VC
Formal and Verdi are trademarks of Synopsys, Inc. Other names are
trademarks of their owners. InFormal is an independent project, not
affiliated with or endorsed by either company. These names are used only
to say which tools' script commands InFormal accepts.
