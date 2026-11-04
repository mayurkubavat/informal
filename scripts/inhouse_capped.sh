#!/usr/bin/env bash
# Score in-house engines one model per process, each under a memory cap,
# so a model that blows up memory loses only its own result.
# usage: scripts/inhouse_capped.sh <model-list> <db> [timeout_s] [jobs] [mem]
set -euo pipefail
cd "$(dirname "$0")/.."
list=$1; db=$2; timeout=${3:-60}; jobs=${4:-3}; mem=${5:-7G}
engines=${engines:-fdv-bmc,fdv-kind,fdv-ic3,fdv-kind-ic3,abc-pdr-pp}
tmp=$(mktemp -d)
one() {
  m=$1; d=$tmp/$(echo "$m" | md5sum | cut -c1-12)
  mkdir -p "$d/m"; ln -s "$PWD/$m" "$d/m/$(basename "$m")"
  systemd-run --user --scope -q -p MemoryMax="$mem" -p MemorySwapMax=0 \
    target/release/fdv bench "$d/m" --each --only "$engines" \
    --timeout "$timeout" --jobs 1 --db "$d/runs.jsonl" >"$d/out.log" 2>&1 \
    || echo "  memout/error  $m"
  # Report the real model path, not the temp link.
  [ -f "$d/runs.jsonl" ] && sed "s#\"model\":\"[^\"]*\"#\"model\":\"$m\"#" "$d/runs.jsonl" > "$d/fixed.jsonl"
  grep "ms  " "$d/out.log" || true
}
export -f one; export tmp timeout engines mem
xargs -a "$list" -P "$jobs" -I{} bash -c 'one "$@"' _ {}
cat "$tmp"/*/fixed.jsonl >> "$db" 2>/dev/null || true
echo "== end $(date -Is)"
