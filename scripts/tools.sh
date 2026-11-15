#!/usr/bin/env bash
# The external tools of tools.lock: check them, or build one.
#
#   scripts/tools.sh check          every tool at its commit and built (the
#                                   gate runs this); also that `ric3` and
#                                   `abc` on PATH are these builds
#   scripts/tools.sh build <name>   check out the pinned commit in
#                                   $INFORMAL_TOOLS_DIR/<name> (cloning it
#                                   first if needed) and build it
#   scripts/tools.sh list           the manifest, with what is installed
#
# Builds use the steps in docs/third-party.md; rebuild informal afterwards
# (`cargo build`), since its build scripts link whatever they find.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
tools=${INFORMAL_TOOLS_DIR:-$HOME/Silicon/DV/tools}
jobs=${CARGO_BUILD_JOBS:-$(nproc)}

manifest() { grep -v '^\s*\(#\|$\)' tools.lock; }

# The tool's artifact, or "" if it is not built. A path under a Debian
# multiarch directory (install/lib/x86_64-linux-gnu) is also looked for
# without it, where macOS and other systems install.
built() {
    local dir=$1 artifact=$2
    if [ -e "$dir/$artifact" ]; then echo "$dir/$artifact"
    elif [ -e "$dir/${artifact/\/x86_64-linux-gnu/}" ]; then echo "$dir/${artifact/\/x86_64-linux-gnu/}"
    fi
}

# The commit the checkout is at, or a word saying why there is none.
at() {
    if [ ! -d "$tools/$1/.git" ]; then echo "missing"; return; fi
    git -C "$tools/$1" rev-parse --short=12 HEAD
}

check() {
    local bad=0
    while read -r name commit _ artifact; do
        local dir=$tools/$name have
        have=$(at "$name")
        if [ "$have" = missing ]; then
            echo "tools: $name is not in $tools (scripts/tools.sh build $name)"; bad=1; continue
        fi
        case "$have" in
            "$commit"*) ;;
            *) echo "tools: $name is at ${have:0:9}, tools.lock pins $commit (scripts/tools.sh build $name)"; bad=1; continue ;;
        esac
        [ -n "$(built "$dir" "$artifact")" ] || { echo "tools: $name is not built: no $dir/$artifact (scripts/tools.sh build $name)"; bad=1; continue; }
        case "$name" in
            rIC3|abc)
                local prog=ric3; [ "$name" = abc ] && prog=abc
                local found; found=$(command -v "$prog" || true)
                if [ -z "$found" ] || [ "$(readlink -f "$found")" != "$(readlink -f "$dir/$artifact")" ]; then
                    echo "tools: '$prog' on PATH is ${found:-missing}, not $dir/$artifact (ln -sf $dir/$artifact ~/.local/bin/$prog)"; bad=1
                fi ;;
        esac
    done < <(manifest)
    [ "$bad" = 0 ] && echo "tools: all $(manifest | wc -l) at their tools.lock commits"
    return "$bad"
}

build() {
    local want=$1 line
    line=$(manifest | awk -v n="$want" '$1 == n') || true
    [ -n "$line" ] || { echo "tools: no $want in tools.lock" >&2; exit 2; }
    read -r name commit url artifact <<<"$line"
    local dir=$tools/$name
    mkdir -p "$tools"
    [ -d "$dir/.git" ] || git clone -q "$url" "$dir"
    git -C "$dir" fetch -q origin
    git -C "$dir" -c advice.detachedHead=false checkout -q "$commit"
    echo "tools: building $name at $commit in $dir"
    case "$name" in
        bitwuzla)
            # Its meson build downloads CaDiCaL and mimalloc archives; where
            # only git reaches GitHub, make them from git (hash-checked).
            python3 -I scripts/wrap_prefill.py "$dir" cadical mimalloc
            (cd "$dir" && ./configure.py --static --prefix "$dir/install" && cd build && meson compile -j "$jobs" && meson install) ;;
        cadical) (cd "$dir" && ./configure -fPIC && make -j "$jobs") ;;
        rIC3) (cd "$dir" && cargo build --release -j "$jobs") && mkdir -p "$HOME/.local/bin" && ln -sf "$dir/$artifact" "$HOME/.local/bin/ric3" ;;
        abc) make -C "$dir" -j "$jobs" ABC_USE_NO_READLINE=1 abc && mkdir -p "$HOME/.local/bin" && ln -sf "$dir/$artifact" "$HOME/.local/bin/abc" ;;
        btor2tools) (cd "$dir" && ./configure.sh && cd build && make -j "$jobs") ;;
        aiger) (cd "$dir" && ./configure.sh && make -j "$jobs" aigsim aigsplit aigtocnf aigfuzz) ;;
        certifaiger)
            # One source file and the AIGER library; its own `make` would
            # also download the tools around it.
            [ -f "$tools/aiger/aiger.c" ] || { echo "tools: certifaiger needs aiger first (scripts/tools.sh build aiger)" >&2; exit 1; }
            # aiger.c compiled on its own as C, and its directory searched only
            # for quoted includes: on a case-insensitive file system (macOS)
            # -I would make <version> find aiger's VERSION file.
            (cd "$dir" && mkdir -p bin && cc -O2 -DNDEBUG -c "$tools/aiger/aiger.c" -o bin/aiger.o \
                && c++ -std=c++23 -O2 -DNDEBUG -DVERSION='"10.4.1"' \
                -DGITID="\"$(git -C "$dir" rev-parse --short HEAD)\"" -iquote "$tools/aiger" \
                src/certifaiger.cpp bin/aiger.o -o bin/certifaiger) ;;
        kissat) (cd "$dir" && ./configure && make -j "$jobs") ;;
        *) echo "tools: no build steps for $name" >&2; exit 2 ;;
    esac
    [ -n "$(built "$dir" "$artifact")" ] || { echo "tools: built $name but $dir/$artifact is missing" >&2; exit 1; }
    echo "tools: $name built; rebuild informal (cargo build) if it links it"
    case "$name:$PATH:" in
        rIC3:*":$HOME/.local/bin:"*|abc:*":$HOME/.local/bin:"*) ;;
        rIC3:*|abc:*) echo "tools: put ~/.local/bin on PATH: the gate runs ric3 and abc from there" ;;
    esac
}

case "${1:-}" in
    check) check ;;
    build) [ -n "${2:-}" ] || { echo "usage: scripts/tools.sh build <name>" >&2; exit 2; }; build "$2" ;;
    list) while read -r name commit url artifact; do printf '%-11s %-10s %-10s %s\n' "$name" "$commit" "$(at "$name" | cut -c1-9)" "$url"; done < <(manifest) ;;
    *) echo "usage: scripts/tools.sh check | build <name> | list" >&2; exit 2 ;;
esac
