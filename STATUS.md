# STATUS — informal

## Current milestone

Phase 2 FPV sign-off core completed and verified on `svlang` `5153d2304a5a2e9ce3421068953bc03355f220f4`: compiler-native SVA NFA/DFA operators (`[->m:n]`, `[=m:n]`, `##[m:$]`, `[*m:$]`, `strong(...)`, `s_eventually [m:$]`), IEEE 1800 `bind` directives, precondition witness vacuity detection, assumption-consistency UNSAT checking, sound multi-task proof isolation (`task`), and parameter override flags (`-param` / `-parameters`).

## Completed in Phase 2

- **Repinned `svlang` to `5153d2304a5a2e9ce3421068953bc03355f220f4`** across `Cargo.toml`, `Cargo.lock`, `ui/src-tauri/Cargo.lock`, and `tests/sv/lowering.lock` (52 untouched designs kept identical Btor2 hashes).
- **Compiler-native SVA NFA/DFA & `bind` (`informal-fpv`, `tests/sv`)**:
  - Removed `"[->"` and `"[="` from `NOT_COMPILED` in `crates/informal-fpv/src/session.rs` so goto/non-consecutive repetitions and unbounded delays/repetitions compile natively through `svlang::netlist`.
  - Updated `tests/sv/{command_ranges, rtl_unsupported, rtl_assume_unsupported}.sv` and added end-to-end known-answer suites `tests/sv/sva_automaton.sv` (command + RTL form) and `tests/sv/sva_bind.sv`.
- **Precondition vacuity detection (`informal-svlang`, `informal-fpv`, `informal-mcp`, `ui`)**:
  - Added sidecar precondition cover lowering (`Request::precondition_covers`, `Lowered::{preconditions, preconditions_btor2}`) in `crates/informal-svlang` so primary `prop_N.btor2` models keep zero extra gate logic.
  - Wired automatic vacuity detection on `Status::Proven` (`vacuous: bool`), reporting `[vacuous]` in `Session::report()`, `get_property_list -include {status vacuous}`, `PropProgress`, and MCP property detail.
- **Assumption-consistency UNSAT checking (`informal-fpv`)**:
  - Added post-reset assumption consistency verification during `Session::prove` (`assumption_conflict: bool`, marking assertions proved under contradictory assumptions as `vacuous`) and Tcl `check_assumptions [-conflict] [-live] [-task <name>]`.
- **Sound multi-task proof isolation & parameter flags (`informal-fpv`)**:
  - Implemented `Task` isolation in `Session` (`task -create` / `-copy` / `-source_task` / `-set` / `-list` / `-isolated`, plus `-task` scoping on `assert`, `assume`, `cover`, `stopat`, `prove`, `get_property_list`, `report`, and `check_assumptions`), verified by `tests/sv/task_isolation.sv` and `tests/sv/assume_task.sv`.
  - Added JG `elaborate -param` alias and wired VCF `elaborate`/`read_file -parameters {NAME=VALUE ...}`, verified in `crates/informal-fpv/tests/flow.rs`.

## Next actions (Subsequent roadmap phases)

1. IEEE 1800-2023 elaboration completion in `svlang` (`checker`, `primitive`/UDP, `config`, `nettype`, streaming concatenations `{<<{...}}` / `{>>{...}}`), followed by `informal` repin.
2. Additional formal apps on the shared `Session`/`Task` substrate (UNR / dead-code reachability, formal connectivity checking, X-propagation).
