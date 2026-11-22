#!/usr/bin/env bash
# Repository hygiene: fails if assistant state, credentials or assistant
# attribution got into tracked files or into commits not yet on origin/main.
set -euo pipefail
cd "$(dirname "$0")/.."
bad=0
paths=$(git ls-files | grep -E '^(research|docs/review)/|(^|/)(\.claude/|CLAUDE(\.local)?\.md$|\.mcp\.json$|\.env(\..*)?$|.*\.(pem|key|p12|pfx)$|id_(rsa|ed25519))' || true)
[ -n "$paths" ] && { echo "hygiene: tracked files that must not be in git:"; echo "$paths"; bad=1; }
text=$(git grep -nIE 'claude\.ai/|noreply@anthropic\.com|Co-Authored-By:' -- . ':!scripts/hygiene.sh' ':!scripts/hooks/' || true)
[ -n "$text" ] && { echo "hygiene: assistant links or trailers in tracked files:"; echo "$text"; bad=1; }
lp_names=$(git grep -nIiE '(^|[^[:alnum:]_])(vclp|vc[_ -]?lp|conformal|questa|spyglass|formality|powerartist|jasper[[:alpha:] ]*lpv|slec|clp)([^[:alnum:]_]|$)' -- . ':!scripts/hygiene.sh' ':!crates/informal-mcp/assets/' || true)
[ -n "$lp_names" ] && { echo "hygiene: forbidden third-party LP EDA tool name in tracked files:"; echo "$lp_names"; bad=1; }
base=$(git merge-base HEAD origin/main 2>/dev/null || true)
if [ -n "$base" ]; then
    msgs=$(git log --format='%h %an <%ae>%n%B' "$base"..HEAD | grep -iE '^(co-authored-by|generated[ -]with|generated-by):|noreply@anthropic\.com|claude\.ai/'  || true)
    [ -n "$msgs" ] && { echo "hygiene: unpushed commits carry assistant attribution:"; echo "$msgs"; bad=1; }
else
    echo "hygiene: no origin/main here, so unpushed commit messages were NOT checked"
fi

# Hard per-file LOC ceiling: <= 850 for .rs in crates/, <= 600 for .ts/.tsx in ui/src/.
# Frozen legacy exemptions ONLY (ZERO exemptions for informal-upf, informal-liberty,
# informal-svlang, informal-session/{session,miter,lp}, informal-debug/why_cex, informal-compat/compat/apps,
# informal-mcp/src, FormalApps).
is_exempt_rs() {
    case "$1" in
        crates/informal-bmc/src/lib.rs|\
        crates/informal-bmc/src/ic3.rs|\
        crates/informal-bmc/src/certify.rs|\
        crates/informal-bmc/src/klive.rs|\
        crates/informal-bmc/src/certificate.rs|\
        crates/informal-sat/src/mini.rs|\
        crates/informal-cli/src/main.rs|\
        crates/informal-session/src/complexity.rs|\
        crates/informal-app-csr/src/csr.rs|\
        crates/informal-session/src/sec.rs|\
        crates/informal-compat/src/compat/help.rs|\
        crates/informal-compat/src/compat/mod.rs|\
        crates/informal-mcp/tests/session.rs) return 0 ;;
        *) return 1 ;;
    esac
}

is_exempt_ts() {
    case "$1" in
        ui/src/App.tsx|\
        ui/src/backend.ts|\
        ui/src/mock.ts|\
        ui/src/components/TraceView.tsx|\
        ui/src/components/SchematicView.tsx|\
        ui/src/components/SourceView.tsx) return 0 ;;
        *) return 1 ;;
    esac
}

while IFS= read -r -d '' f; do
    is_exempt_rs "$f" && continue
    lines=$(wc -l < "$f" | tr -d ' ')
    if [ "$lines" -gt 850 ]; then
        echo "hygiene: Rust file exceeds 850 LOC limit ($lines): $f"
        bad=1
    fi
done < <(find crates -type f -name '*.rs' -not -path '*/target/*' -print0)

if [ -d ui/src ]; then
    while IFS= read -r -d '' f; do
        is_exempt_ts "$f" && continue
        lines=$(wc -l < "$f" | tr -d ' ')
        if [ "$lines" -gt 600 ]; then
            echo "hygiene: UI TS/TSX file exceeds 600 LOC limit ($lines): $f"
            bad=1
        fi
    done < <(find ui/src -type f \( -name '*.ts' -o -name '*.tsx' \) -print0)
fi

exit $bad
