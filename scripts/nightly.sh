#!/usr/bin/env bash
# The nightly run, on this machine: what is too slow or too heavy for every
# landing. It checks main as it stands, in the landing clone and under the
# landing lock, so it never overlaps a landing.
#
#   scripts/nightly.sh               main: gate with the Playwright suites,
#                                    svlang drift, HWMCC'24 sample
#   scripts/nightly.sh --no-bench    without the HWMCC'24 sample
#   scripts/nightly.sh --ref <b>     branch <b> of this clone instead of main
#                                    (to try a change to the nightly itself)
#
#   1. scripts/check.sh --e2e on the commit.
#   2. svlang drift: what moving the pin to svlang's current main would do
#      (scripts/repin.sh, then thrown away): do the designs still give
#      their answers, which models change, and are those proved the same.
#   3. The in-house engines on the 60-model HWMCC'24 sample, 60 s each
#      (scripts/inhouse_sample.sh): a verdict that contradicts the
#      published answer fails the run. Needs the models, which are not in
#      git: $HWMCC_DIR, default ~/Silicon/DV/git/InFormal/bench/hwmcc24.
#   4. 200 random AIGER models (scripts/fuzz.py, seeded by the date), each
#      checked by every engine: two opposite verdicts, or an in-house
#      verdict whose certificate check fails, fail the run and are kept,
#      shrunk, in target/fuzz/failures.
#
# Every step runs even if an earlier one failed. The report goes to
# ~/.local/state/informal/nightly/<date>.log; its last line, also printed,
# is "NIGHTLY PASS ..." or "NIGHTLY FAIL ...", and the exit status agrees.
# To run it every night at 02:30 as a systemd user timer:
#   systemd-run --user --unit informal-nightly --on-calendar '*-*-* 02:30' \
#       --timer-property=Persistent=true <this clone>/scripts/nightly.sh
set -uo pipefail
src=$(cd "$(dirname "$0")/.." && pwd)
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

bench=1
ref=""
while [ $# -gt 0 ]; do
    case "$1" in
        --no-bench) bench=0 ;;
        --ref) ref=$2; shift ;;
        *) echo "nightly.sh: unknown option $1" >&2; exit 2 ;;
    esac
    shift
done

land=${INFORMAL_LAND_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/informal/land}
state=${XDG_STATE_HOME:-$HOME/.local/state}/informal/nightly
mkdir -p "$state" "$(dirname "$land")"
report=$state/$(date +%Y-%m-%d).log
exec 9>"$land.lock"
flock 9

url=$(git -C "$src" remote get-url origin)
[ -d "$land/.git" ] || git clone -q "$url" "$land" || exit 1
cd "$land" || exit 1
git rebase --abort >/dev/null 2>&1
git checkout -q -- . 2>/dev/null
if [ -n "$ref" ]; then
    git fetch -q "$src" "$ref" && git checkout -q --detach FETCH_HEAD || exit 1
else
    git fetch -q origin main && git checkout -q --detach origin/main || exit 1
fi
commit=$(git rev-parse --short HEAD)
if [ ! -d ui/node_modules ] || [ ui/package-lock.json -nt ui/node_modules/.package-lock.json ]; then
    (cd ui && npm ci --no-audit --no-fund >/dev/null)
fi

results=()
failed=0
{
    echo "== nightly $(date -Is) on ${ref:-main} $commit"

    echo; echo "== 1. gate with the Playwright suites"
    if scripts/check.sh --e2e; then
        results+=("gate pass")
    else
        results+=("gate FAIL"); failed=1
    fi

    echo; echo "== 2. svlang drift"
    if [ -x scripts/repin.sh ]; then
        if out=$(scripts/repin.sh 2>&1); then
            echo "$out" | tail -n 12
            drift=$(echo "$out" | grep -E "already pinned|No model changed|proved the same" | tail -n 1 | sed 's/^ *//')
            case "$drift" in
                *", 0 not"*|*"No model changed"*|*"already pinned"*) results+=("svlang main: ${drift%%:*}") ;;
                *) results+=("svlang main: ${drift:-no summary} (look before the next re-pin)") ;;
            esac
        else
            echo "$out" | tail -n 40
            results+=("svlang main FAILS the designs"); failed=1
        fi
        git checkout -q -- . 2>/dev/null
    else
        results+=("svlang drift not run (no scripts/repin.sh on this commit)"); failed=1
    fi

    echo; echo "== 3. HWMCC'24 sample, in-house engines"
    data=${HWMCC_DIR:-$HOME/Silicon/DV/git/InFormal/bench/hwmcc24}
    if [ "$bench" = 0 ]; then
        results+=("hwmcc sample not run (--no-bench)")
    elif [ ! -f "$data/expected.json" ]; then
        results+=("hwmcc sample NOT RUN (no models in $data)"); failed=1
    else
        [ -e bench/hwmcc24 ] || ln -s "$data" bench/hwmcc24
        tag=nightly-$(date +%Y%m%d)
        rm -f "bench/runs-inhouse-sample60-$tag.jsonl" "bench/inhouse-sample60-$tag.log"
        if cargo build --locked --release -q -p informal-cli && line=$(scripts/inhouse_sample.sh 60 3 "$tag"); then
            echo "$line"; results+=("hwmcc sample: ${line,,}")
        else
            results+=("hwmcc sample FAIL (bench/inhouse-sample60-$tag.log in $land)"); failed=1
        fi
    fi

    echo; echo "== 4. fuzzing: random AIGER models, every engine, compared"
    if cargo build --locked --release -q -p informal-cli \
        && line=$(python3 -I scripts/fuzz.py --count 200 --seed "$(date +%Y%m%d)000" | tail -n 1); then
        echo "$line"; results+=("${line,,}")
    else
        echo "$line"; results+=("fuzz FAIL (target/fuzz/failures in $land)"); failed=1
    fi
} >"$report" 2>&1

verdict=PASS
[ "$failed" = 0 ] || verdict=FAIL
summary="NIGHTLY $verdict $(date +%Y-%m-%d) ${ref:-main} $commit: $(IFS=';'; echo "${results[*]}" | sed 's/;/; /g')"
echo "$summary" | tee -a "$report"
echo "$summary" >>"$state/history.log"
exit "$failed"
