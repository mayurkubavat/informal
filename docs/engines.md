# Engines

Phase 0 runs existing model checkers as child processes. Each one is an
`[[engine]]` entry in `config/engines.toml`.

| Engine | Formats | Proves | Finds bugs | Get it |
| --- | --- | --- | --- | --- |
| rIC3 (IC3/PDR, BMC, k-induction portfolio) | AIGER, Btor2 | yes | yes | `git clone --recurse-submodules github.com/gipsyh/rIC3 && cargo build --release` (stable Rust 1.99 works; binary `ric3`) |
| Pono (k-induction, IC3IA, interpolation, liveness) | Btor2 | yes | yes | github.com/stanford-centaur/pono |
| ABC `pdr`, `bmc3` | AIGER (binary) | pdr | yes | github.com/berkeley-abc/abc: `make ABC_USE_NO_READLINE=1 abc` (also ships as `yosys-abc` with Yosys) |
| BtorMC (BMC) | Btor2 | no | yes | github.com/Boolector/boolector |

Only rIC3 and ABC are built and checked (see `docs/third-party.md`); the
Pono and BtorMC entries are unchecked guesses and those tools are not
installed on the Linux box.
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
  entries (`ric3-ic3`, `ric3-bmc`) and comment out the `ric3` portfolio
  entry, which is enabled in `engines.toml` for Linux.
- **Counterexamples**: entries with `cex_witness = true` (rIC3 `--cex`,
  ABC `write_cex`) hand their witness to informal, which turns it into the
  trace; other engines' bugs get a BMC re-run for the trace.
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

## Memory caps

`mem_limit_mb` on an `[[engine]]` entry (or `INFORMAL_ENGINE_MEM_MB` for all)
runs the engine in a systemd user scope with `MemoryMax`, so the engine
and every process it forks share one cap and only they die when it is
exceeded (the run reads as unknown). The rIC3 portfolio is capped at
14 GB: its 16 workers reached 18 GB together on HWMCC'24 models here, and
the kernel then killed unrelated processes.

## Preprocessed models for external engines

An `[[engine]]` entry with `preprocess = true` gets informal's simplified model
instead of the original file: cone of influence, constant latches and
strash, written as binary AIGER into an anonymous in-memory file
(`memfd_create`) that the engine opens as `/proc/self/fd/N`. Nothing is
written to disk. `abc-pdr-pp` is ABC `pdr` this way; on
`2024/hku/x-epic/a10-p06` it drops from 2.3 s to 0.2 s (41,262 latches
become 89), though PDR's luck varies the other way on some models.

## In-house engines (phase 1)

`informal-bmc` and `informal-kind` run in-process on CaDiCaL (crate `informal-sat`,
C API through FFI). They load the model at bit level with `informal-aig`
(AIGER directly, Btor2 bit-blasted), simplify it (cone of influence,
sequential constant latches by ternary simulation, structural hashing),
and unroll it one frame at a time, declaring variables to CaDiCaL ahead
of use. `informal simp <model>` shows a model's size before and after.

| Engine | Proves | Finds bugs | Notes |
| --- | --- | --- | --- |
| `informal-bmc` | no | yes | Shortest counterexample first; every counterexample is replayed before it is reported |
| `informal-bmc-10`, `informal-bmc-65` | no | yes | Stepped BMC: 10 or 65 frames per SAT call (like rIC3 `bmc --step`), for deep bugs |
| `informal-kind` | yes | yes | k-induction with lazy simple-path constraints, BMC base case |
| `informal-ic3` | yes | yes | IC3 / PDR on informal's small CDCL solver (`informal_sat::Mini`): lifting, core + MIC generalization, propagation |
| `informal-ic3-cadical` | yes | yes | The same IC3 on CaDiCaL |
| `informal-ic3-mt` | yes | yes | 4 IC3 workers with different options sharing lemmas (imports re-checked against each worker's frames) |
| `informal-kind-ic3` | yes | yes | IC3 for part of the budget, Houdini on its lemmas, then k-induction strengthened by the proved invariant |
| `informal-wbmc` | no | yes | Word-level BMC on Bitwuzla: Btor2 unrolled into terms, arrays included (skips `.aig` models) |

The in-house portfolio used for HWMCC'24 runs is `informal-kind-ic3`,
`informal-ic3-mt`, `informal-kind`, `informal-bmc`, `informal-bmc-10` and `informal-bmc-65`.
The front end encodes the AIG to CNF with DAG-aware variable elimination
and compact ITE / XOR clauses, and `scorr` merges equivalent latches
before the engines start.

Not supported yet at bit level (they report unknown): arrays and
liveness; `informal-wbmc` covers arrays for bug finding. Bitwuzla is built
under `~/Silicon/DV/tools/bitwuzla` with its own patched CaDiCaL, and
informal links that one library for both (crate `informal-sat` decides).

Build CaDiCaL once (`./configure -fPIC && make` in
`~/Silicon/DV/tools/cadical`, or point `INFORMAL_CADICAL_DIR` at a checkout)
and rebuild informal; without it these engines are left out of the portfolio.
Tests run them on a small built-in DPLL solver instead.

## Adding an engine

1. Add an `[[engine]]` entry: `program`, `args` with `{model}`, `formats`.
2. Say how its verdict is read: `exit_codes` (`safe`, `unsafe`) and/or
   regex `patterns` (`safe`, `unsafe`, `depth`). Exit codes are checked
   first, then `unsafe`, then `safe`.
3. Run `informal bench tests/models --each --only <name>` and check every
   verdict against the expected answers in the model comments.

An in-process engine implements `informal_engine::Engine` directly instead.

## Verdict trust

Until phase 2 adds certificate checking (Certifaiger for invariants,
BtorSim for witnesses), a verdict is only as trustworthy as the engine.
The portfolio already reports engines that contradict each other as
`unknown (engine conflict)` rather than picking one.

## Shrinking a disagreement

`informal reduce` shrinks an AIGER model while two engines keep giving
the same conflicting verdicts on it (`--disagree A,B`), or while a shell
command keeps exiting 0 (`--test CMD`, with `{}` standing for the
candidate's path). It never runs informal's own preprocessing, since the
bug may be there: it only turns latches and AND gates into free inputs,
inputs and latches into constants, keeps one bad state and drops
constraints, in halving chunks (`crates/informal-aig/src/reduce.rs`).

Example, from the ABC note above: ABC `pdr` without `fold` against
`informal-ic3` on `2019/wolf/2019C/zipversa_composecrc_prf-p12` (safe;
ABC alone says "asserted in frame 0"). 236 tests and 20 s shrank 236
inputs, 315 latches and 2,764 ANDs to 2 inputs, no latches, no gates: a
constant-false bad and one constraint, which is the whole bug (ABC reads
the constraint output as a property).

```sh
informal reduce model.aig --disagree informal-ic3,ric3-ic3 --timeout 10 -o small.aag
informal reduce model.aig --test './still-bad.sh {}' -o small.aag
```
