# InFormal

InFormal is an open-source formal verification tool for digital designs.
It reads SystemVerilog with assertions, runs a portfolio of model-checking
engines on each property, and reports a proof or a counterexample trace.
It accepts FPV setup scripts written in the command style of JasperGold
and VC Formal, and has a desktop workbench for results and waveforms.

**Status: pre-release (0.1).** The pieces below work and are tested, but
the SystemVerilog and SVA coverage is a subset, command and file formats
may change, and there are no binary releases yet. Do not rely on it for
sign-off.

![The InFormal workbench](docs/ui/workbench-light.png)

## What works today

- **Front end.** SystemVerilog through [svlang](https://github.com/mayurkubavat/svlang)
  (pure Rust), lowered to a word-level Btor2 model. Constructs it cannot
  model are rejected with a message, never guessed (decision D13).
- **Engines.** In-house BMC, k-induction and IC3 / PDR at bit level, and
  word-level BMC on Bitwuzla. External checkers (rIC3, ABC) join the same
  portfolio as child processes when they are installed. The first
  definitive verdict wins, and contradicting engines are flagged.
- **Checked results.** A proof's invariant is checked against the model as
  loaded, and every counterexample is replayed, before either is reported
  (`docs/engines.md`).
- **FPV shell.** `informal shell` runs Tcl setup scripts with `analyze`,
  `elaborate`, `clock`, `reset`, `assert`, `assume`, `cover`, `prove`,
  `stopat` and related commands in a real Tcl 8.6 interpreter
  (`docs/fpv-shell.md` lists what is and is not accepted).
- **Debugging aids.** `informal reduce` shrinks a model on which two engines
  disagree; `informal simp`, `info`, `fmt` and `lower` inspect models.
- **Desktop app.** A Tauri 2 + React workbench in `ui/`: property table,
  Tcl console, source view, counterexample waveforms, and an optional
  "Ask" mode backed by a local LLM (`ui/README.md`, `docs/ui/`).

Not there yet: liveness, arrays at bit level, VHDL, most of concurrent
SVA beyond the documented subset, coverage and other apps.

## Quick start

Tested on Ubuntu 26.04 (x86-64). macOS builds too (`docs/engines.md` has
the differences).

1. Install the prerequisites: a stable Rust toolchain from
   [rustup](https://rustup.rs) (1.87 or newer), a C++ compiler, and the
   Tcl 8.6 development library.

   ```sh
   sudo apt install build-essential tcl8.6-dev      # Ubuntu
   brew install tcl-tk@8                            # macOS
   ```

2. Build CaDiCaL, the SAT solver the in-house engines use. Any directory
   works; tell the build where it is with `INFORMAL_TOOLS_DIR`.

   ```sh
   export INFORMAL_TOOLS_DIR=$HOME/informal-tools
   mkdir -p $INFORMAL_TOOLS_DIR && cd $INFORMAL_TOOLS_DIR
   git clone https://github.com/arminbiere/cadical && cd cadical
   git checkout rel-3.0.1 && ./configure -fPIC && make -j4
   ```

3. Build InFormal and run the counter example.

   ```sh
   git clone https://github.com/mayurkubavat/informal && cd informal
   cargo build --release
   target/release/informal shell tests/fpv/counter/counter.jg.tcl
   ```

   The run ends with a table like this (engines and times vary):

   ```
   property                     type    status        engine     detail
   a_max                        assert  proven        informal-kind 0.06 s
   a_step                       assert  proven        informal-kind 0.10 s
   a_hold                       assert  proven        informal-kind 0.08 s
   a_never9                     assert  cex           informal-bmc depth 10 in 0.06 s
   a_no_wrap                    assert  proven        informal-kind-ic3 0.04 s
   c_done                       cover   covered       informal-bmc depth 10 in 0.07 s
   c_10                         cover   unreachable   informal-kind-ic3 0.06 s
   ```

Run commands from the repository root: `informal` reads
`config/engines.toml` from the current directory.

Optional, each picked up automatically when present (build steps and the
exact versions used are in `docs/third-party.md`):

| Add | Gives you | How it is found |
| --- | --- | --- |
| Bitwuzla | word-level BMC (`informal-wbmc`), arrays for bug finding | `$INFORMAL_TOOLS_DIR/bitwuzla/install` or `INFORMAL_BITWUZLA_DIR` |
| rIC3, ABC | more engines in the portfolio | `ric3` and `abc` on `PATH` |

Without CaDiCaL the build still succeeds, but the in-house engines are
left out of the portfolio and every property comes back undetermined.
External engines that are not installed are skipped.

## Use

```sh
# Run an FPV script (flavor detected from the script), or start a prompt
target/release/informal shell tests/fpv/counter/counter.jg.tcl
target/release/informal shell --flavor vcf

# List what a script uses that informal does not accept, without running it
target/release/informal shell --check path/to/setup.tcl

# Check one Btor2 or AIGER model (exit 10 unsafe, 20 safe, 0 unknown)
target/release/informal check tests/models/toggle_unsafe.aag --timeout 60

# Lower SystemVerilog to Btor2; inspect or canonicalize a model
target/release/informal lower tests/sv/counter_ok.sv
target/release/informal info tests/models/counter.btor2
target/release/informal fmt tests/models/memory.btor2

# Benchmark every engine on a model set and log to bench/runs.jsonl
target/release/informal bench path/to/models --each --timeout 300 --jobs 8
```

The desktop app is built separately; see `ui/README.md`.

## Benchmarks

Development is measured on the 319 bit-level models of HWMCC'24 (Zenodo
record 14775242; the models are not in this repository). Our own
measurements, on one 16-core, 26 GB Linux machine with a 300 s limit per
model, as of November 2026:

- The in-house engines, run together as one portfolio with a 14 GB memory
  cap, solved 191 of 319 (commit `af4e70e`).
- Counting every model that any in-house engine solved across separate
  per-engine runs gives 204 of 319, the same count the rIC3 portfolio
  reached on that machine.
- No engine has returned a verdict that contradicts the published answers
  in these runs.

These are local numbers, not competition results, and they move with
every engine change. To reproduce, download the models to
`bench/hwmcc24/`, then use `scripts/hwmcc_full.sh` or
`scripts/inhouse_sample.sh` and score with `scripts/hwmcc_check.py`
(`bench/README.md`).

## Code map

| Crate | What it does |
| --- | --- |
| `informal-ir` | Engine-side model IR. Lossless Btor2 reader and writer with validation; structural features for the run database and engine selector. |
| `informal-engine` | The engine plug-in API (`Engine` trait, capabilities, progress, cancellation) and the parallel portfolio runner, which takes the first definitive verdict, cancels the rest and flags engines that contradict each other. |
| `informal-engines-ext` | Adapters that run external model checkers (rIC3, Pono, ABC, BtorMC) as child processes, described in `config/engines.toml`. |
| `informal-bench` | Benchmark harness. Appends one JSON line per engine run to the run database and scores engines by solved count and PAR-2, including a virtual-best row. |
| `informal-sat` | SAT interface: CaDiCaL through its C API, plus a small DPLL solver for tests. |
| `informal-aig` | Bit-level model: AIG with structural hashing, AIGER reader and writer, Btor2 bit-blaster, preprocessing. |
| `informal-bmc` | In-house engines on the AIG: BMC (also stepped), k-induction, IC3 / PDR (single and multi-threaded with lemma sharing), and IC3 lemmas feeding k-induction. Engine list in `docs/engines.md`. |
| `informal-svlang` | SystemVerilog front end on svlang (pure Rust): diagnostics, top module, design hierarchy, the properties (written in the RTL or given by command) and the reset compiled into the model, and the word-level netlist written as Btor2 (`informal lower`). |
| `informal-bitwuzla` | Word-level engines on Bitwuzla (C++ shim through FFI): BMC over Btor2 with arrays. |
| `informal-tcl` | Embedded Tcl 8.6 (the C library through FFI) for informal scripts. |
| `informal-fpv` | FPV sessions: design setup, properties, prove over the portfolio, and the JasperGold / VC Formal Tcl compatibility layer (`docs/fpv-shell.md`). |
| `informal-llm` | Swappable LLM providers (OpenAI-compatible streaming client, scripted fake) for the app's Ask mode; defaults to local Gemma on Ollama (`config/llm.toml`). |
| `informal-cli` | The `informal` command. |

The desktop app lives in `ui/`, outside the Cargo workspace.

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

### Local data (git-ignored)

Working data stays out of git. `.gitignore` covers these places, and
`scripts/check.sh` (via `scripts/hygiene.sh`) fails if anything under
`research/` or `docs/review/` is ever tracked.

```
bench/hwmcc24/       HWMCC'24 benchmark models (multi-GB)
bench/bin/           frozen engine binaries used by long runs
bench/.work/         per-run scratch
bench/runs*.jsonl    run databases; bench/*.log are run logs
bench/seeds/, bench/store*/   seed sets and result stores
research/            research notes and reports
research/review/     review notes and feedback reports
informal_work/       FPV shell session output (btor2, vcd, logs)
refs/*.pdf           papers (copyrighted; the index in refs/ is tracked)
```

Model lists such as `bench/*.txt` and `bench/sample60.csv` are tracked.

## Documentation

| File | What |
| --- | --- |
| `docs/fpv-shell.md` | The FPV shell: commands, the SVA subset, how a prove runs |
| `docs/engines.md` | Engines, memory limits, result checking |
| `docs/third-party.md` | Third-party tools: licenses, versions, build steps |
| `docs/decisions.md` | Design decisions, with reasons and open issues |
| `docs/ic3-research.md` | IC3 experiments and their measured results |
| `docs/vendor-script-tally.md` | How publicly available vendor-style scripts fare in the shell |
| `docs/ui/` | Desktop app design notes and test strategy |

## Build and test

```sh
cargo build --release
cargo test
scripts/check.sh        # hygiene, fmt, clippy, all Rust and UI tests: run before a pull request
```

`scripts/check.sh` does not need the benchmark models. Its exit status is
the result, and its last line is `GATE: PASS` or `GATE: FAIL (<step>)`. It
needs CaDiCaL, Bitwuzla, `ric3` on `PATH` and BtorSim (it replays every
counterexample of the test designs; `scripts/btorsim_replay.py` says where
it is looked for): under the gate a test whose
tool is missing fails (plain `cargo test` prints why and skips it;
`scripts/check.sh --allow-missing-tools` does the same, but is not a valid
gate run). The gate clears `INFORMAL_*` tuning variables, checks the Rust
and Node versions against `rust-toolchain.toml` and `ui/.nvmrc`, and builds
with `--locked`. The UI steps need `npm ci` in `ui/` first, and the Rust
tests run under [cargo-nextest](https://nexte.st) (one process per test;
install the prebuilt binary with
`curl -LsSf https://get.nexte.st/latest/linux | tar zxf - -C ~/.cargo/bin`).
`scripts/check.sh --fast` is a quick check while working, and `--quiet`
runs the gate at low priority on a busy machine.

`scripts/land.sh` puts a branch on main: one landing at a time on the
machine, it rebases the branch onto main in a separate landing clone, runs
the gate on that result and pushes it only if the gate passed, so main is
always a commit that was tested as it is (`--dry-run` does all but the
push). `scripts/repin.sh` moves the svlang pin: `Cargo.toml`, both lock
files and `tests/sv/lowering.lock` together, and lists the designs whose
models changed. `scripts/nightly.sh` is the nightly run on the same machine:
the gate with the Playwright suites on main, what svlang's current main
would change, and the in-house engines on the HWMCC'24 sample; its report
ends with `NIGHTLY PASS` or `NIGHTLY FAIL`.

## License

Copyright 2026 Mayur Kubavat. Licensed under the Apache License,
Version 2.0 (`LICENSE`); see `NOTICE`.

InFormal does not include third-party source code. It links CaDiCaL and
Bitwuzla (MIT) and the system Tcl, GMP and MPFR libraries, and
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
