#!/usr/bin/env bash
# Move the svlang pin: the rev or tag in Cargo.toml, both lock files
# (workspace and ui/src-tauri) and the lowering record, in one step.
#
#   scripts/repin.sh              pin svlang's current main
#   scripts/repin.sh <commit>     pin that commit (full 40-character id)
#   scripts/repin.sh <tag>        pin that release tag (for example v0.5.0)
#
# Either way Cargo.lock records the commit the pin resolves to.
#
# It leaves the changes uncommitted and prints which models changed, so the
# commit can say why. Every model that changed is compared with the one the
# old pin gave (scripts/btor2_equiv.py): "same" is a proof that the two have
# the same transition system, so a reshaped netlist that kept behaviour says
# so, and anything else is named for a look. Land it like any other change
# (scripts/land.sh): the gate then runs against the new pin.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
# The builds and test runs below wait in the dvq build queue
# (~/.local/bin/dvq, the front end of rdev queue); DVQ= runs them directly.
DVQ="${DVQ-$HOME/.local/bin/dvq run build --}"; [ -x "$HOME/.local/bin/dvq" ] || DVQ=

url=$(sed -n 's/^svlang *= *{ *git *= *"\([^"]*\)".*/\1/p' Cargo.toml)
old_ref=$(sed -nE 's/^svlang *= *\{.*[{ ,]((rev|tag) *= *"[^"]*").*/\1/p' Cargo.toml)
old=$(sed -nE 's/^source = "git\+[^"]*svlang[^"]*#([0-9a-f]{40})"$/\1/p' Cargo.lock | sort -u)
[ -n "$url" ] && [ -n "$old_ref" ] && [ ${#old} = 40 ] \
    || { echo "repin.sh: no svlang git pin in Cargo.toml and Cargo.lock" >&2; exit 1; }

new=${1:-}
if [ -z "$new" ]; then
    new=$(git ls-remote "$url" refs/heads/main | cut -f1)
    [ -n "$new" ] || { echo "repin.sh: could not read svlang main from $url" >&2; exit 1; }
fi
case "$new" in
    *[!0-9a-f]*)
        # A tag: pin it by name; an annotated tag's commit is its ^{} line.
        tag=$new
        new=$(git ls-remote "$url" "refs/tags/$tag" "refs/tags/$tag^{}" \
            | awk -v t="refs/tags/$tag" '$2 == t "^{}" { p = $1 } $2 == t { q = $1 } END { print (p ? p : q) }')
        [ -n "$new" ] || { echo "repin.sh: no tag $tag in $url" >&2; exit 2; }
        new_ref="tag = \"$tag\"" ;;
    *)
        [ ${#new} = 40 ] || { echo "repin.sh: give the full 40-character commit id or a tag" >&2; exit 2; }
        new_ref="rev = \"$new\"" ;;
esac
if [ "$new" = "$old" ] && [ "$new_ref" = "$old_ref" ]; then
    echo "svlang is already pinned at ${old:0:7}"
    exit 0
fi
[ -z "$(git status --porcelain --untracked-files=no)" ] \
    || { echo "repin.sh: commit or stash the changes in this clone first" >&2; exit 1; }

models=target/repin-models
rm -rf "$models"
run_designs() {
    INFORMAL_KEEP_MODELS="$PWD/$models/$1" INFORMAL_BLESS=$2 $DVQ cargo nextest run --locked \
        -p informal-fpv --test sv_semantics --status-level fail --final-status-level fail
}
echo "models under the current pin ${old:0:7}"
run_designs old ""
[ -z "$(git status --porcelain --untracked-files=no)" ] \
    || { echo "repin.sh: tests/sv/lowering.lock is not current for the pin in Cargo.toml" >&2; exit 1; }

echo "svlang ${old:0:7} ($old_ref) -> ${new:0:7} ($new_ref)"
sed -i -E "/^svlang(-lint|-model)? *=/s|$old_ref|$new_ref|" Cargo.toml
# Move only the svlang packages in each lock file (and whatever svlang's
# own dependencies need), nothing else.
for dir in . ui/src-tauri; do
    (cd "$dir" && cargo update -q -p svlang -p svlang-lint -p svlang-model)
done
if [ "$new" != "$old" ] && grep -q "$old" Cargo.toml Cargo.lock ui/src-tauri/Cargo.lock; then
    echo "repin.sh: the old rev is still in a lock file" >&2
    exit 1
fi

echo "rewriting tests/sv/lowering.lock under the new pin"
run_designs new 1
$DVQ cargo build --locked -q -p informal-cli
changed=$(git diff --numstat -- tests/sv/lowering.lock | awk '{print $1}')
echo
if [ -z "$changed" ]; then
    echo "No model changed: tests/sv/lowering.lock is as it was."
else
    echo "$changed line(s) of tests/sv/lowering.lock changed; the designs:"
    git diff -U0 -- tests/sv/lowering.lock | sed -n 's/^+\([a-z][^ ]*\) .*/  \1/p' | sort | uniq -c
    echo "old model against new model, for each one that changed:"
    same=0 other=0
    while read -r design prop; do
        f="$design.$prop.btor2"
        if [ ! -f "$models/old/$f" ]; then
            echo "  $design $prop: new (no model under the old pin)"
            continue
        fi
        verdict=$(python3 -I scripts/btor2_equiv.py "$models/old/$f" "$models/new/$f" \
            -o "$models/miter.$f" --informal target/debug/informal) || true
        case "$verdict" in
            same*) same=$((same + 1)) ;;
            *) other=$((other + 1)); echo "  $design $prop: $verdict" ;;
        esac
    done < <(git diff -U0 -- tests/sv/lowering.lock | sed -n 's/^+\([a-z][^ ]*\) \([^ ]*\) .*/\1 \2/p')
    echo "  $same proved the same as before, $other not (listed above)"
fi
echo "Not committed. Review with 'git diff', commit, and land with scripts/land.sh."
