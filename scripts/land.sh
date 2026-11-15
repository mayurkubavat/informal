#!/usr/bin/env bash
# Land a branch on main: one landing at a time on this machine, and main
# only moves to a commit that passed the gate in its final, rebased form.
#
#   scripts/land.sh                 land the current branch of this clone
#   scripts/land.sh <branch>        land that branch of this clone
#   scripts/land.sh --dry-run ...   everything except the push
#   scripts/land.sh --e2e ...       also the Playwright suites
#   scripts/land.sh --busy ...      run the gate with --quiet (low priority,
#                                   2 jobs) while timing runs have the machine
#
# What it does, under a lock shared by every clone on the machine:
#   1. fetches origin/main and the branch into a landing clone
#      ($INFORMAL_LAND_DIR, default ~/.cache/informal/land), which keeps
#      its own build so a landing does not rebuild from nothing;
#   2. rebases the branch's commits onto origin/main there; a conflict
#      stops the landing (rebase in your clone and try again);
#   3. runs scripts/check.sh on the result;
#   4. pushes it to main, fast-forward only.
# The last line is "LANDED <commit>" or "NOT LANDED: <why>", and the exit
# status says the same. The branch in your clone is not touched: after a
# landing, `git fetch origin && git rebase origin/main` brings it up to date.
#
# Nothing here stops a direct `git push origin main`; the point of landing
# through this script is that the commit on main is the one that was tested.
set -euo pipefail
src=$(cd "$(dirname "$0")/.." && pwd)
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

dry=0
gate_args=()
branch=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) dry=1 ;;
        --e2e) gate_args+=(--e2e) ;;
        --busy) gate_args+=(--quiet) ;;
        -*) echo "land.sh: unknown option $arg" >&2; exit 2 ;;
        *) branch=$arg ;;
    esac
done
[ -n "$branch" ] || branch=$(git -C "$src" symbolic-ref --short HEAD)

why="interrupted"
finish() {
    rc=$?
    [ "$rc" = 0 ] || printf '\nNOT LANDED: %s\n' "$why"
}
trap finish EXIT
fail() { why=$1; exit 1; }

[ -z "$(git -C "$src" status --porcelain --untracked-files=no)" ] \
    || fail "$src has uncommitted changes (only commits are landed)"
url=$(git -C "$src" remote get-url origin)
tip=$(git -C "$src" rev-parse --verify "$branch^{commit}") || fail "no branch $branch in $src"

land=${INFORMAL_LAND_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/informal/land}
mkdir -p "$(dirname "$land")"
exec 9>"$land.lock"
if ! flock -n 9; then
    echo "land.sh: another landing is running; waiting for it"
    flock 9
fi

if [ ! -d "$land/.git" ]; then
    echo "land.sh: creating the landing clone in $land (first use)"
    git clone -q "$url" "$land"
fi
cd "$land"
git remote set-url origin "$url"
git rebase --abort >/dev/null 2>&1 || true
git fetch -q origin main
git fetch -q "$src" "$tip"
base=$(git rev-parse origin/main)
if git merge-base --is-ancestor "$tip" "$base"; then
    why="nothing to land: $branch is already on main"
    printf '\n%s\n' "$why"
    trap - EXIT
    exit 0
fi
git checkout -q --detach "$tip"
git clean -qfd -e target -e node_modules
if ! git rebase -q "$base" >/dev/null 2>&1; then
    git rebase --abort >/dev/null 2>&1 || true
    fail "$branch does not rebase cleanly onto main ($(git rev-parse --short "$base")): rebase it in your clone and land again"
fi
head=$(git rev-parse HEAD)
echo "land.sh: $branch ($(git rev-parse --short "$tip")) on main $(git rev-parse --short "$base"): $(git rev-list --count "$base..$head") commit(s), testing $(git rev-parse --short "$head")"

# UI packages follow the lock file.
if [ ! -d ui/node_modules ] || [ ui/package-lock.json -nt ui/node_modules/.package-lock.json ]; then
    (cd ui && npm ci --no-audit --no-fund >/dev/null)
fi

scripts/check.sh "${gate_args[@]}" || fail "the gate failed on $(git rev-parse --short "$head") (see the GATE line above)"

if [ "$dry" = 1 ]; then
    why="dry run: $(git rev-parse --short "$head") passed the gate and was not pushed"
    printf '\n%s\n' "$why"
    trap - EXIT
    exit 0
fi
# Fast-forward only: if main moved while the gate ran (a direct push), the
# tested commit is no longer what would land.
git push -q origin "$head:refs/heads/main" \
    || fail "main moved while the gate ran (someone pushed directly): land again"
trap - EXIT
printf '\nLANDED %s\n' "$(git rev-parse --short "$head")"
