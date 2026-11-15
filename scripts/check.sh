#!/usr/bin/env bash
# The pre-merge gate (it runs locally; there is no hosted CI): hygiene,
# formatting, clippy with warnings as errors, every Rust test, and the UI's
# type check and unit tests.
#
#   scripts/check.sh                        all of it
#   scripts/check.sh --e2e                  also the Playwright e2e and visual suites
#   scripts/check.sh --allow-missing-tools  let tests skip when CaDiCaL, Bitwuzla
#                                           or rIC3 is missing (not a valid gate run)
#   scripts/check.sh --quiet                for a machine that is busy with timing
#                                           runs: nice 19, 2 build jobs, 2 test
#                                           threads, 2 UI test workers
#
# Run from anywhere. The exit status is the result: 0 only if every step
# passed. The last line printed is "GATE: PASS" or "GATE: FAIL (<step>)", so
# a log tail cannot be misread. Tests that need a tool fail when the tool is
# missing (INFORMAL_REQUIRE_TOOLS); every other INFORMAL_* variable that
# changes how the tool runs is cleared first, so a tuning or debugging
# setting in the calling shell cannot change what the gate tests.
#
# --quiet sets CARGO_BUILD_JOBS, RUST_TEST_THREADS and VITEST_MAX_WORKERS to
# 2 (a value already in the environment wins) and lowers the priority of
# everything the gate starts. A test that runs the engine portfolio still
# uses one thread per engine, at that low priority.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

e2e=0
strict=1
for arg in "$@"; do
    case "$arg" in
        --e2e) e2e=1 ;;
        --allow-missing-tools) strict=0 ;;
        --quiet)
            export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
            export RUST_TEST_THREADS="${RUST_TEST_THREADS:-2}"
            export VITEST_MAX_WORKERS="${VITEST_MAX_WORKERS:-2}"
            renice -n 19 -p $$ >/dev/null
            ;;
        *) echo "check.sh: unknown option $arg" >&2; exit 2 ;;
    esac
done

current="setup"
finish() {
    rc=$?
    if [ "$rc" = 0 ] && [ "$strict" = 1 ]; then
        printf '\nGATE: PASS\n'
    elif [ "$rc" = 0 ]; then
        printf '\nGATE: PASS (tool-dependent tests were allowed to skip: not a valid gate run)\n'
    else
        printf '\nGATE: FAIL (%s, exit %s)\n' "$current" "$rc"
    fi
}
trap finish EXIT
step() { current="$*"; printf '\n== %s\n' "$*"; }

# Only where the native libraries are found survives from the caller's
# INFORMAL_* environment.
for v in $(compgen -e | grep '^INFORMAL_' || true); do
    case "$v" in
        INFORMAL_TOOLS_DIR|INFORMAL_CADICAL_DIR|INFORMAL_BITWUZLA_DIR|INFORMAL_TCL_LIB|INFORMAL_TCL_LIB_DIR) ;;
        *) echo "check.sh: clearing $v"; unset "$v" ;;
    esac
done
[ "$strict" = 1 ] && export INFORMAL_REQUIRE_TOOLS=1

step "toolchain versions"
want_rust=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
have_rust=$(rustc --version | awk '{print $2}')
[ "$have_rust" = "$want_rust" ] || { echo "rustc is $have_rust, rust-toolchain.toml pins $want_rust"; exit 1; }
want_node=$(cat ui/.nvmrc)
have_node=$(node --version | sed 's/^v\([0-9]*\).*/\1/')
[ "$have_node" = "$want_node" ] || { echo "node is major $have_node, ui/.nvmrc pins $want_node"; exit 1; }
[ -d ui/node_modules ] || { echo "ui/node_modules is missing: run 'npm ci' in ui/"; exit 1; }
echo "rustc $have_rust, node $have_node"

step "hygiene (no assistant state, keys or attribution)"
scripts/hygiene.sh

step "cargo fmt (workspace, app)"
cargo fmt --all --check
(cd ui/src-tauri && cargo fmt --check)

step "clippy (workspace, app)"
cargo clippy --locked --workspace --all-targets -- -D warnings
(cd ui/src-tauri && cargo clippy --locked --all-targets -- -D warnings)

step "cargo test (workspace, app)"
cargo test --locked --workspace
(cd ui/src-tauri && cargo test --locked)

step "ui: tsc and vitest"
(cd ui && npx --no-install tsc --noEmit && npx --no-install vitest run)

if [ "$e2e" = 1 ]; then
    step "ui: playwright (e2e + visual)"
    (cd ui && npx --no-install playwright test)
fi
