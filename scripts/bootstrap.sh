#!/usr/bin/env bash
# A fresh Linux machine, from nothing to a passing gate: everything InFormal
# is built and tested with, at the versions this commit pins.
#
#   scripts/bootstrap.sh            set up, then run scripts/check.sh
#   scripts/bootstrap.sh --apt      also install the system packages (needs
#                                   root or sudo; Debian or Ubuntu)
#   scripts/bootstrap.sh --no-gate  set up only
#   scripts/bootstrap.sh --verify   after the gate, the checks that need no
#                                   quiet machine: 200 fuzzed models and the
#                                   HWMCC'25 verdicts at 60 s per model
#
# It is how a second machine (a cloud session, a container, the artifact's
# Dockerfile) checks the same commit. Steps, each skipped when already done:
#   1. system packages (listed below; --apt installs them, else it says
#      which are missing and stops);
#   2. the Rust toolchain of rust-toolchain.toml (rustup) and cargo-nextest;
#   3. every tool of tools.lock, built at its commit in $INFORMAL_TOOLS_DIR
#      (default ~/Silicon/DV/tools), with ric3 and abc linked into
#      ~/.local/bin, which must be on PATH;
#   4. the UI packages (npm ci in ui/; Node 22, see ui/.nvmrc).
# Timing measurements do not belong on such a machine: they need the one
# quiet machine the published numbers name (bench/README.md).
set -euo pipefail
cd "$(dirname "$0")/.."

apt=0 gate=1 verify=0
for a in "$@"; do
    case "$a" in
        --apt) apt=1 ;;
        --no-gate) gate=0 ;;
        --verify) verify=1 ;;
        *) echo "usage: scripts/bootstrap.sh [--apt] [--no-gate] [--verify]" >&2; exit 2 ;;
    esac
done
step() { printf '\n== bootstrap: %s\n' "$*"; }

# Tools and libraries the builds and tests use, as Debian/Ubuntu packages.
pkgs="build-essential git curl ca-certificates pkg-config python3 cmake meson ninja-build
      libgmp-dev libmpfr-dev tcl8.6-dev zlib1g-dev nodejs npm
      libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev librsvg2-dev"

step "system packages"
if ! command -v dpkg >/dev/null; then
    echo "not Debian or Ubuntu: install the equivalents of these yourself:"
    echo $pkgs
    missing=""
else
    missing=$(for p in $pkgs; do dpkg -s "$p" >/dev/null 2>&1 || echo "$p"; done)
fi
if [ -n "$missing" ]; then
    if [ "$apt" = 1 ]; then
        sudo=""; [ "$(id -u)" = 0 ] || sudo=sudo
        $sudo apt-get update -q
        # shellcheck disable=SC2086
        DEBIAN_FRONTEND=noninteractive $sudo apt-get install -y -q $missing
    else
        echo "missing: $(echo $missing)"
        echo "install them (scripts/bootstrap.sh --apt does) and run again"
        exit 1
    fi
fi
echo "all present"

step "Rust toolchain and cargo-nextest"
if ! command -v rustup >/dev/null; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none --profile minimal
fi
. "$HOME/.cargo/env"
rustup toolchain install   # the version and components of rust-toolchain.toml
if ! cargo nextest --version >/dev/null 2>&1; then
    # The prebuilt binary, else from crates.io where get.nexte.st is blocked.
    curl -LsSf https://get.nexte.st/latest/linux | tar zxf - -C "$HOME/.cargo/bin" \
        || cargo install --locked cargo-nextest
fi
rustc --version
cargo nextest --version | head -1

step "tools of tools.lock"
mkdir -p "$HOME/.local/bin"
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) export PATH="$HOME/.local/bin:$PATH" ;; esac
tools=${INFORMAL_TOOLS_DIR:-$HOME/Silicon/DV/tools}
# In tools.lock order (aiger before certifaiger); a tool already at its
# commit and built is left alone.
while read -r name commit _ artifact; do
    dir=$tools/$name
    if [ -d "$dir/.git" ] && [ -e "$dir/$artifact" ] \
        && case "$(git -C "$dir" rev-parse HEAD)" in "$commit"*) true ;; *) false ;; esac; then
        echo "$name: at $commit, built"
    else
        scripts/tools.sh build "$name"
    fi
done < <(grep -v '^\s*\(#\|$\)' tools.lock)
scripts/tools.sh check

step "UI packages"
want=$(cat ui/.nvmrc)
have=$(node --version 2>/dev/null | sed 's/^v\([0-9]*\).*/\1/')
if [ "$have" != "$want" ]; then
    # The distribution's Node is often older (Ubuntu 24.04 has 18): take the
    # newest Node $want from nodejs.org into ~/.local/node-$want.
    nd="$HOME/.local/node-$want"
    if [ ! -x "$nd/bin/node" ]; then
        arch=$(uname -m | sed 's/x86_64/x64/; s/aarch64/arm64/')
        os=$(uname -s | tr 'A-Z' 'a-z')
        ver=$(curl -fsSL "https://nodejs.org/dist/latest-v$want.x/SHASUMS256.txt" 2>/dev/null \
            | sed -n "s/.*node-\(v$want\.[0-9.]*\)-$os-$arch\.tar\.gz$/\1/p" | head -n 1)
        if [ -n "$ver" ]; then
            mkdir -p "$nd"
            curl -fsSL "https://nodejs.org/dist/$ver/node-$ver-$os-$arch.tar.gz" | tar zxf - -C "$nd" --strip-components 1
        fi
    fi
    [ -x "$nd/bin/node" ] && export PATH="$nd/bin:$PATH"
    have=$(node --version 2>/dev/null | sed 's/^v\([0-9]*\).*/\1/')
    [ "$have" = "$want" ] || { echo "node is major ${have:-missing}, ui/.nvmrc pins $want and nodejs.org could not be reached: install Node $want and run again"; exit 1; }
    echo "using Node from $nd: put $nd/bin on PATH for later runs of the gate"
fi
(cd ui && npm ci --no-audit --no-fund)

if [ "$gate" = 1 ]; then
    step "the gate"
    scripts/check.sh
fi

if [ "$verify" = 1 ]; then
    step "verification that needs no quiet machine"
    cargo build --locked --release -q -p informal-cli
    python3 -I scripts/fuzz.py --count 200 --seed "$(date +%Y%m%d)000"
    python3 -I scripts/hwmcc25_fetch.py
    db=target/verify-hwmcc25-$(date +%Y%m%d).jsonl
    rm -f "$db"
    # The in-house portfolio, verdicts only: time limits are generous and
    # times are not reported.
    target/release/informal bench bench/hwmcc25/bitlevel/safety --timeout 60 --jobs "$(( $(nproc) / 4 > 0 ? $(nproc) / 4 : 1 ))" --db "$db"
    python3 -I scripts/hwmcc_check.py bench/hwmcc25/expected.json "$db"
fi
printf '\nBOOTSTRAP: DONE\n'
