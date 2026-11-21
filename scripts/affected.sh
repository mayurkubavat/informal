#!/usr/bin/env bash
# Which Rust tests a change can affect, as a cargo-nextest filter.
#
#   scripts/affected.sh [<base>]
#
# The change is everything between <base> (default: where this branch left
# origin/main) and the working tree, committed or not. A file in a crate
# selects that crate and every crate that depends on it (nextest's rdeps);
# tests/sv selects informal-fpv; files no test reads (docs, the UI, scripts
# other than this one) select nothing. Anything else (Cargo.toml or
# Cargo.lock at the root, the toolchain, nextest's settings, tests/models,
# config/) can reach any crate, so it selects all of them.
#
# Prints one line: "all", "none", or a filter such as
#   rdeps(informal-bmc) | rdeps(informal-fpv)
# scripts/check.sh --fast uses it; the full gate always runs every test.
set -euo pipefail
cd "$(dirname "$0")/.."

base=${1:-$(git merge-base HEAD origin/main 2>/dev/null || true)}
[ -n "$base" ] || { echo all; exit 0; }
files=$( { git diff --name-only "$base"; git ls-files --others --exclude-standard; } | sort -u)
[ -n "$files" ] || { echo none; exit 0; }

crates=()
add() { case " ${crates[*]-} " in *" $1 "*) ;; *) crates+=("$1") ;; esac; }
while read -r f; do
    case "$f" in
        crates/*/*) add "$(cut -d/ -f2 <<<"$f")" ;;
        tests/sv/*) add informal-fpv ;;
        docs/*|examples/*|*.md|zensical.toml|llms.txt|ui/src/*|ui/tests/*|ui/*.json|ui/*.ts|ui/.nvmrc|bench/*|LICENSE*|NOTICE*|.gitignore) ;;
        scripts/affected.sh) ;;
        scripts/*) ;;
        ui/src-tauri/*) ;;
        *) echo all; exit 0 ;;
    esac
done <<<"$files"

if [ ${#crates[@]} = 0 ]; then
    echo none
else
    printf 'rdeps(%s)\n' "${crates[@]}" | paste -sd'|' | sed 's/|/ | /g'
fi
