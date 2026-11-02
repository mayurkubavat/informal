# FormalDV

An internal formal verification tool for digital designs: a Rust
orchestrator over a portfolio of model-checking engines, with an SVA front
end, verification apps and a debug UI on top.

The plan this code follows is the research report and phased roadmap:
Formal Verification Tool: Research Report & Architecture Plan (kept outside this repository).
Background research lives in `research/`.

## Status: phase 0 (foundations)

| Crate | What it does |
| --- | --- |
| `fdv-ir` | Engine-side model IR. Lossless Btor2 reader and writer with validation; structural features for the run database and engine selector. |
| `fdv-engine` | The engine plug-in API (`Engine` trait, capabilities, progress, cancellation) and the parallel portfolio runner, which takes the first definitive verdict, cancels the rest and flags engines that contradict each other. |
| `fdv-engines-ext` | Adapters that run external model checkers (rIC3, Pono, ABC, BtorMC) as child processes, described in `config/engines.toml`. |
| `fdv-bench` | Benchmark harness. Appends one JSON line per engine run to the run database and scores engines by solved count and PAR-2, including a virtual-best row. |
| `fdv-cli` | The `fdv` command. |

## Build and test

Needs a stable Rust toolchain (`rustup`, Rust 1.80 or newer).

```sh
cargo build --release
cargo test
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
target/release/fdv bench path/to/hwmcc24 --each --timeout 300
```

Engines must be on `PATH` under the names in `config/engines.toml`
(`ric3`, `pono`, `abc`, `btormc`). rIC3 builds on stable Rust; rIC3, Pono
and ABC all build from source (see `docs/engines.md`, which also covers
macOS limits such as rIC3's Linux-only portfolio).

## Layout

```
crates/        Rust workspace members
config/        engine definitions
tests/models/  small Btor2 and AIGER models used by the tests
bench/         run database and downloaded benchmark sets (git-ignored)
docs/          design notes
refs/          reference library: papers and specs (see refs/README.md)
research/      research reports, including Gemini Deep Research
```

## Phase 0 gate

Wrapped engines must reproduce their published HWMCC solve counts when run
through `fdv bench`. That validates the adapters and verdict patterns
before any in-house engine is compared against them.
