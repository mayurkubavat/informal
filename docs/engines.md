# Engines

Phase 0 runs existing model checkers as child processes. Each one is an
`[[engine]]` entry in `config/engines.toml`.

| Engine | Formats | Proves | Finds bugs | Get it |
| --- | --- | --- | --- | --- |
| rIC3 (IC3/PDR, BMC, k-induction portfolio) | AIGER, Btor2 | yes | yes | `git clone --recurse-submodules github.com/gipsyh/rIC3 && cargo build --release` (stable Rust 1.99 works; binary `ric3`) |
| Pono (k-induction, IC3IA, interpolation, liveness) | Btor2 | yes | yes | github.com/stanford-centaur/pono |
| ABC `pdr`, `bmc3` | AIGER (binary) | pdr | yes | github.com/berkeley-abc/abc: `make ABC_USE_NO_READLINE=1 abc` (also ships as `yosys-abc` with Yosys) |
| BtorMC (BMC) | Btor2 | no | yes | github.com/Boolector/boolector |
| AVR, CAR, Avy | Btor2 / AIGER | yes | yes | to add (from the Gemini research) |

## Checked engine notes

Checked on macOS arm64, 2026-11-02, with rIC3 1.5.2 and ABC from git
(`b04e85589`), against `tests/models`, rIC3's `examples/counter/counter.aig`
and a sample of HWMCC'24 models with published answers.

- **Both tools exit 0** whatever the verdict, so verdicts come from the
  output: rIC3 prints `SAT` / `UNSAT` as its last line; ABC prints
  `Property proved.` or `Output N of miter ... was asserted in frame K.`
- **rIC3 portfolio is Linux-only.** `ric3 check <m> portfolio` forks its
  workers and collects results over `ipc-channel`, which uses Mach ports
  on macOS; those do not survive `fork()`, so the parent never hears back
  and every run hangs to the timeout. On macOS use the single-engine
  entries (`ric3-ic3`, `ric3-bmc`); the `ric3` portfolio entry is kept
  commented out in `engines.toml` for Linux.
- **rIC3 has no liveness support** for Btor2 `fair`/`justice`: it panics
  (`unsupport fair op`), which reads as unknown.
- **ABC needs a prefix for AIGER 1.9** (HWMCC) files:
  `logic; undc; strash; zero; fold`. Without `fold`, invariant
  constraints are checked as if they were properties, and ABC reports
  false bugs (frame 0 on `counter.aig`, which is really unsafe at depth
  64). Without `undc`, latches with don't-care init are forced to 0,
  which can hide bugs.
- **ABC cannot read ASCII `.aag` files with a bad-state section**
  (`read_aiger` refuses them, which reads as unknown). Its `&r` reader
  does accept them but misreads them, so do not switch to it.
- ABC writes `abc.history` into the working directory (git-ignored).

## Preprocessed models for external engines

An `[[engine]]` entry with `preprocess = true` gets fdv's simplified model
instead of the original file: cone of influence, constant latches and
strash, written as binary AIGER into an anonymous in-memory file
(`memfd_create`) that the engine opens as `/proc/self/fd/N`. Nothing is
written to disk. `abc-pdr-pp` is ABC `pdr` this way; on
`2024/hku/x-epic/a10-p06` it drops from 2.3 s to 0.2 s (41,262 latches
become 89), though PDR's luck varies the other way on some models.

## In-house engines (phase 1)

`fdv-bmc` and `fdv-kind` run in-process on CaDiCaL (crate `fdv-sat`,
C API through FFI). They load the model at bit level with `fdv-aig`
(AIGER directly, Btor2 bit-blasted), simplify it (cone of influence,
sequential constant latches by ternary simulation, structural hashing),
and unroll it one frame at a time, declaring variables to CaDiCaL ahead
of use. `fdv simp <model>` shows a model's size before and after.

| Engine | Proves | Finds bugs | Notes |
| --- | --- | --- | --- |
| `fdv-bmc` | no | yes | Shortest counterexample first |
| `fdv-kind` | yes | yes | k-induction with lazy simple-path constraints, BMC base case |
| `fdv-ic3` | yes | yes | IC3 / PDR: lifting, core + MIC generalization, propagation (first cut) |

Not supported yet (they report unknown): arrays and liveness. Next:
DAG-CNF variable elimination when encoding, and IC3 invariants fed into
k-induction once our IC3 exists.
Build CaDiCaL once (`./configure -fPIC && make` in
`~/Silicon/DV/tools/cadical`, or point `FDV_CADICAL_DIR` at a checkout)
and rebuild fdv; without it these engines are left out of the portfolio.
Tests run them on a small built-in DPLL solver instead.

## Adding an engine

1. Add an `[[engine]]` entry: `program`, `args` with `{model}`, `formats`.
2. Say how its verdict is read: `exit_codes` (`safe`, `unsafe`) and/or
   regex `patterns` (`safe`, `unsafe`, `depth`). Exit codes are checked
   first, then `unsafe`, then `safe`.
3. Run `fdv bench tests/models --each --only <name>` and check every
   verdict against the expected answers in the model comments.

An in-process engine implements `fdv_engine::Engine` directly instead.

## Verdict trust

Until phase 2 adds certificate checking (Certifaiger for invariants,
BtorSim for witnesses), a verdict is only as trustworthy as the engine.
The portfolio already reports engines that contradict each other as
`unknown (engine conflict)` rather than picking one.
