#!/usr/bin/env bash
# The pre-merge gate (it runs locally; there is no hosted CI): hygiene,
# formatting, clippy with warnings as errors, every Rust test, and the UI's
# type check and unit tests.
#
#   scripts/check.sh                        all of it
#   scripts/check.sh --fast                 a quick check while working: hygiene,
#                                           fmt, clippy, the Rust tests that do
#                                           not start the engines in the crates
#                                           the change touches and those that
#                                           depend on them (scripts/affected.sh),
#                                           and the UI type check (not a valid
#                                           gate run)
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
# --quiet sets CARGO_BUILD_JOBS, RUST_TEST_THREADS, VITEST_MAX_WORKERS and
# PW_WORKERS to 2 (a value already in the environment wins), lowers the
# priority of everything the gate starts, and skips the one UI test that
# asserts a wall-clock budget (the result line says so). A test that runs
# the engine portfolio still uses one thread per engine, at low priority.
#
# On a machine with the dvq job queue (~/.local/bin/dvq, the front end of
# rdev queue) the whole gate runs as one job of the build class, so a gate or
# a landing waits for CPUs and memory instead of fighting a timing run for
# them. The output, the GATE line and the exit status are passed through.
# DVQ= runs the gate directly; DVQ_CPU/DVQ_MEM change the reservation.
set -euo pipefail
DVQ="${DVQ-$HOME/.local/bin/dvq run build --}"; [ -x "$HOME/.local/bin/dvq" ] || DVQ=
if [ -n "$DVQ" ] && [ -z "${DVQ_CLASS:-}${RDEV_JOB_ID:-}" ]; then
    # More than the build class preset (4 CPUs, 6G): a full run builds and
    # tests the whole workspace in parallel; 12G leaves room for that.
    # --quiet (land.sh --busy) builds with 2 jobs, so 4 CPUs are enough, and
    # 7G: twelve queued --quiet gates peaked at 0.5-5.7G (median 1.5G), and
    # asking 12G kept them waiting behind other jobs for RAM they never used.
    case " $* " in
        *" --quiet "*) export DVQ_CPU="${DVQ_CPU:-400%}" DVQ_MEM="${DVQ_MEM:-7G}" ;;
        *) export DVQ_CPU="${DVQ_CPU:-800%}" ;;
    esac
    export DVQ_MEM="${DVQ_MEM:-12G}"
    exec $DVQ "$(cd "$(dirname "$0")" && pwd)/$(basename "$0")" "$@"
fi
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

e2e=0
strict=1
quiet=0
fast=0
for arg in "$@"; do
    case "$arg" in
        --e2e) e2e=1 ;;
        --fast) fast=1 ;;
        --allow-missing-tools) strict=0 ;;
        --quiet)
            export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
            export RUST_TEST_THREADS="${RUST_TEST_THREADS:-2}"
            export VITEST_MAX_WORKERS="${VITEST_MAX_WORKERS:-2}"
            export PW_WORKERS="${PW_WORKERS:-2}"
            export UI_SKIP_PERF=1
            quiet=1
            renice -n 19 -p $$ >/dev/null
            ;;
        *) echo "check.sh: unknown option $arg" >&2; exit 2 ;;
    esac
done
# Inside a queue job, build with as many jobs as the job has CPUs (--quiet
# has already set 2).
if [ -n "${RDEV_JOB_CPUS:-}" ]; then export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-$RDEV_JOB_CPUS}"; fi

current="setup"
finish() {
    rc=$?
    if [ "$rc" = 0 ] && [ "$fast" = 1 ]; then
        printf '\nGATE: FAST PASS (engine, app and UI tests were not run: not a valid gate run)\n'
    elif [ "$rc" = 0 ] && [ "$strict" = 1 ] && [ "$quiet" = 1 ] && [ "$e2e" = 1 ]; then
        printf '\nGATE: PASS (--quiet: the UI wall-clock budget test was not run)\n'
    elif [ "$rc" = 0 ] && [ "$strict" = 1 ]; then
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
        INFORMAL_TOOLS_DIR|INFORMAL_CADICAL_DIR|INFORMAL_BITWUZLA_DIR|INFORMAL_TCL_LIB|INFORMAL_TCL_LIB_DIR|INFORMAL_NO_SCCACHE) ;;
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
cargo nextest --version >/dev/null 2>&1 || { echo "cargo-nextest is missing: see README (Build and test)"; exit 1; }
echo "rustc $have_rust, node $have_node, $(cargo nextest --version | head -1)"
if [ "$strict" = 1 ]; then
    scripts/tools.sh check
else
    scripts/tools.sh check || echo "(--allow-missing-tools: going on)"
fi

# A compile cache shared by every clone on the machine, when sccache is
# installed: a fresh clone, or the landing clone after a dependency change,
# gets the dependencies from the cache instead of compiling them again.
# Cache keys hold absolute paths, so every InFormal clone's root is listed
# in SCCACHE_BASEDIRS and stripped from them. The sccache server reads that
# list when it starts (it stops after 10 idle minutes): a clone made while
# it runs only misses until then. A cache hit is only ever the same input,
# so the cache can make a build faster, never different.
# INFORMAL_NO_SCCACHE=1 turns it off.
if [ -z "${INFORMAL_NO_SCCACHE:-}" ] && [ -z "${RUSTC_WRAPPER:-}" ] && command -v sccache >/dev/null; then
    roots="${SCCACHE_BASEDIRS:+$SCCACHE_BASEDIRS:}$PWD"
    for d in "$HOME"/Silicon/DV/git/*/ "${INFORMAL_LAND_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/informal/land}/"; do
        d=${d%/}
        [ -f "$d/scripts/check.sh" ] && [ -d "$d/crates/informal-fpv" ] && [ "$d" != "$PWD" ] && roots="$roots:$d"
    done
    export RUSTC_WRAPPER=sccache
    export SCCACHE_DIR="${SCCACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/sccache}"
    export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-30G}"
    export SCCACHE_BASEDIRS="$roots"
    echo "compile cache: sccache in $SCCACHE_DIR"
fi

step "hygiene (no assistant state, keys or attribution)"
scripts/hygiene.sh

step "cargo fmt (workspace, app)"
cargo fmt --all --check
(cd ui/src-tauri && cargo fmt --check)

step "clippy (workspace, app)"
cargo clippy --locked --workspace --all-targets -- -D warnings
(cd ui/src-tauri && cargo clippy --locked --all-targets -- -D warnings)

if [ "$fast" = 1 ]; then
    # Only the crates the change touches and the crates that depend on them
    # (scripts/affected.sh); the full gate runs everything.
    affected=$(scripts/affected.sh)
    step "rust tests of the changed crates, without the engine tests (nextest, profile fast)"
    case "$affected" in
        none) echo "no change reaches a Rust test" ;;
        all) echo "the change can reach every crate: all of them"
             cargo nextest run --locked --workspace --profile fast ;;
        *) echo "selected: $affected"
           cargo nextest run --locked --workspace --profile fast -E "$affected" ;;
    esac
    step "ui: tsc"
    (cd ui && npx --no-install tsc --noEmit)
    exit 0
fi

# One process per test, engine tests one at a time (.config/nextest.toml).
# nextest does not run doc tests, hence the second command.
step "rust tests (workspace: nextest and doc tests; app)"
# The tests/sv run keeps its models for the witness replay below.
rm -rf target/gate-models
export INFORMAL_KEEP_MODELS="$PWD/target/gate-models"
cargo nextest run --locked --workspace
unset INFORMAL_KEEP_MODELS
cargo test --locked --workspace --doc
(cd ui/src-tauri && cargo test --locked)

# Every counterexample and cover trace of tests/sv, replayed by BtorSim: a
# reader and simulator of the model that shares no code with InFormal.
step "witness replay (BtorSim)"
cargo build --locked -q -p informal-cli
replay=0
python3 -I scripts/btorsim_replay.py --designs tests/sv --models target/gate-models || replay=$?
if [ "$replay" = 2 ] && [ "$strict" = 0 ]; then
    echo "skipped: no BtorSim"
elif [ "$replay" != 0 ]; then
    exit 1
fi

# Every proof and counterexample of the in-house engines on tests/models,
# confirmed from its certificate by Certifaiger and aigsim, which share no
# code with InFormal (docs/certificates.md).
step "certificates (Certifaiger, aigsim)"
cert=0
python3 -I scripts/certificate_check.py --models tests/models --quiet \
    --expect invariant,k-induction,two-step,trace || cert=$?
if [ "$cert" = 2 ] && [ "$strict" = 0 ]; then
    echo "skipped: no certificate checkers"
elif [ "$cert" != 0 ]; then
    exit 1
fi

step "ui: tsc and vitest"
(cd ui && npx --no-install tsc --noEmit && npx --no-install vitest run)

if [ "$e2e" = 1 ]; then
    step "ui: playwright (e2e + visual)"
    (cd ui && npx --no-install playwright test ${PW_WORKERS:+--workers="$PW_WORKERS"})
fi
