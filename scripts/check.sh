#!/usr/bin/env bash
# The pre-merge gate (there is no CI): formatting, clippy with warnings as
# errors, every Rust test, and the UI's type check and unit tests.
#
#   scripts/check.sh            all of it (about 30 s on a warm build)
#   scripts/check.sh --e2e      also the Playwright e2e and visual suites
#
# Run from anywhere; exits non-zero on the first failing step.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

e2e=0
[ "${1:-}" = "--e2e" ] && e2e=1

step() { printf '\n== %s\n' "$*"; }

step "hygiene (no assistant state, keys or attribution)"
scripts/hygiene.sh

step "cargo fmt (workspace, app)"
cargo fmt --all --check
(cd ui/src-tauri && cargo fmt --check)

step "clippy (workspace, app)"
cargo clippy --workspace --all-targets -- -D warnings
(cd ui/src-tauri && cargo clippy --all-targets -- -D warnings)

step "cargo test (workspace, app)"
cargo test --workspace
(cd ui/src-tauri && cargo test)

step "ui: tsc and vitest"
(cd ui && npx tsc --noEmit && npx vitest run)

if [ "$e2e" = 1 ]; then
    step "ui: playwright (e2e + visual)"
    (cd ui && npx playwright test)
fi

printf '\nAll checks passed.\n'
