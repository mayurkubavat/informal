#!/usr/bin/env bash
# Move the svlang pin: the rev in Cargo.toml, both lock files (workspace and
# ui/src-tauri) and the lowering record, in one step.
#
#   scripts/repin.sh              pin svlang's current main
#   scripts/repin.sh <commit>     pin that commit (full 40-character id)
#
# It leaves the changes uncommitted and prints which models changed, so the
# commit can say why. Land it like any other change (scripts/land.sh): the
# gate then runs against the new pin.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

url=$(sed -n 's/^svlang *= *{ *git *= *"\([^"]*\)".*/\1/p' Cargo.toml)
old=$(sed -n 's/^svlang *= *{.*rev *= *"\([0-9a-f]\{40\}\)".*/\1/p' Cargo.toml)
[ -n "$url" ] && [ -n "$old" ] || { echo "repin.sh: no svlang git pin in Cargo.toml" >&2; exit 1; }

new=${1:-}
if [ -z "$new" ]; then
    new=$(git ls-remote "$url" refs/heads/main | cut -f1)
    [ -n "$new" ] || { echo "repin.sh: could not read svlang main from $url" >&2; exit 1; }
fi
case "$new" in
    *[!0-9a-f]*) echo "repin.sh: $new is not a commit id" >&2; exit 2 ;;
esac
[ ${#new} = 40 ] || { echo "repin.sh: give the full 40-character commit id" >&2; exit 2; }
if [ "$new" = "$old" ]; then
    echo "svlang is already pinned at ${old:0:7}"
    exit 0
fi
[ -z "$(git status --porcelain --untracked-files=no)" ] \
    || { echo "repin.sh: commit or stash the changes in this clone first" >&2; exit 1; }

echo "svlang ${old:0:7} -> ${new:0:7}"
sed -i "s/$old/$new/g" Cargo.toml Cargo.lock ui/src-tauri/Cargo.lock
# The lock files hold the new rev; if svlang's own dependencies changed
# between the two commits, let cargo redo that part of each lock.
for dir in . ui/src-tauri; do
    (cd "$dir" && { cargo metadata --locked --format-version 1 >/dev/null 2>&1 \
        || cargo metadata --format-version 1 >/dev/null; })
done
if grep -q "$old" Cargo.toml Cargo.lock ui/src-tauri/Cargo.lock; then
    echo "repin.sh: the old rev is still in a lock file" >&2
    exit 1
fi

echo "rewriting tests/sv/lowering.lock under the new pin"
INFORMAL_BLESS=1 cargo nextest run --locked -p informal-fpv --test sv_semantics \
    --status-level fail --final-status-level fail
changed=$(git diff --numstat -- tests/sv/lowering.lock | awk '{print $1}')
echo
if [ -z "$changed" ]; then
    echo "No model changed: tests/sv/lowering.lock is as it was."
else
    echo "$changed line(s) of tests/sv/lowering.lock changed; the designs:"
    git diff -U0 -- tests/sv/lowering.lock | sed -n 's/^+\([a-z][^ ]*\) .*/  \1/p' | sort | uniq -c
fi
echo "Not committed. Review with 'git diff', commit, and land with scripts/land.sh."
