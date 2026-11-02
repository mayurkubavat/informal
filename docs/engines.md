# Engines

Phase 0 runs existing model checkers as child processes. Each one is an
`[[engine]]` entry in `config/engines.toml`.

| Engine | Formats | Proves | Finds bugs | Get it |
| --- | --- | --- | --- | --- |
| rIC3 (IC3/PDR, BMC, k-induction portfolio) | AIGER, Btor2 | yes | yes | build from source with the nightly pinned in its repo; `cargo +nightly install rIC3` fails on the 2026-11 nightly |
| Pono (k-induction, IC3IA, interpolation, liveness) | Btor2 | yes | yes | github.com/stanford-centaur/pono |
| ABC `pdr`, `bmc3` | AIGER | pdr | yes | github.com/berkeley-abc/abc (ships as `yosys-abc` with Yosys) |
| BtorMC (BMC) | Btor2 | no | yes | github.com/Boolector/boolector |
| AVR, CAR, Avy | Btor2 / AIGER | yes | yes | to add (from the Gemini research) |

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
