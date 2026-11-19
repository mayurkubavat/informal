# STATUS — informal

## Current milestone

Workspace stabilized, unsound D13/D14 WIP patches removed, macOS link/symlink portability fixed, and `svlang` repinned to `e7ee563` (including Phase 1 IEEE 1800-2023 & Census fixes `B1`–`B12`; `tests/sv/lowering.lock` verified unchanged).

## Completed in Phase 0.2

- Removed unsound workarounds in `crates/informal-fpv/src/{session.rs, compat.rs}` (no-op `expand_sva`, stdout `DEBUG:` prints, silent no-op `task`/`bind` handlers, and aggressive `check_reset_and_clocks` rejection on combinational/RTL-clocked setups).
- Restored `[->` and `[=` to `NOT_COMPILED` fail-closed handling until the compiler-native `sv-netlist` NFA property compiler lands in Phase 2.
- Replaced blanket `#[allow(dead_code)]`/`#[allow(unused_imports)]` in `crates/informal-fpv/tests/stop.rs` with `#[cfg(target_os = "linux")]`.
- Restored `ui/.nvmrc` to `22` and removed stray WIP artifacts (`mac-c.jsonl`, invalid `tests/sv/sva_ranges.sv`, `tests/fpv/ranges.jg.tcl`, `tests/fpv/no_clocks.jg.tcl`).
- Added macOS Homebrew library search paths (`/opt/homebrew/lib`, `/usr/local/lib`) to `crates/informal-sat/build.rs` and `crates/informal-bitwuzla/build.rs`, and canonicalized temp path resolution in `crates/informal-tcl/src/lib.rs` (`caller_location_from_sourced_files`).
- Cleaned duplicate `svlang` / `[patch.crates-io]` entries in `Cargo.toml` and repinned `svlang` to `e7ee563150ce5c8b706040db4dd5e38595948ba9` across `Cargo.toml`, `Cargo.lock`, `ui/src-tauri/Cargo.lock`, and `tests/sv/lowering.lock`.

## Next actions (Phase 2)

1. Land compiler-native SVA NFA lowering (`[->m:n]`, `[=m:n]`, `##[m:$]`, `[*m:$]`) and `bind` instantiation in `svlang`, then repin `informal` and enable native lowering without Tcl string hacks.
2. Wire precondition witness covers (vacuity detection) and assumption-consistency UNSAT checks into `informal-fpv`.
3. Implement sound multi-task proof isolation (`task -create` / `-copy` / `-set`), `elaborate -param`, and `clock -both_edges` in `informal-fpv`.
