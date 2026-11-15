#!/usr/bin/env bash
# Score the in-house engines on the 60-model HWMCC'24 sample.
# usage: scripts/inhouse_sample.sh [timeout_s] [jobs] [tag]
set -euo pipefail
cd "$(dirname "$0")/.."
timeout=${1:-60}; jobs=${2:-3}; tag=${3:-$(date +%Y%m%d)}
# Heavy steps wait in the dvq formal queue (~/linux-tuning/dvq): one at a time,
# CPU/memory capped. DVQ= runs them directly; dvq missing also runs directly.
DVQ="${DVQ-$HOME/.local/bin/dvq run formal --}"; [ -x "$HOME/.local/bin/dvq" ] || DVQ=
db=bench/runs-inhouse-sample60-$tag.jsonl
log=bench/inhouse-sample60-$tag.log
{
  echo "== start $(date -Is) timeout=${timeout}s jobs=$jobs"
  $DVQ target/release/informal bench "${models:-bench/hwmcc24/sample60}" --each \
    --only informal-bmc,informal-kind,informal-ic3,informal-kind-ic3,abc-pdr-pp \
    --timeout "$timeout" --jobs "$jobs" --db "$db"
  python3 -I scripts/hwmcc_check.py bench/hwmcc24/expected.json "$db"
  echo "== end $(date -Is)"
} >>"$log" 2>&1 || {
  rc=$?
  echo "FAILED (exit $rc): see $log" >&2
  grep -A20 '^DISAGREEMENTS:' "$log" | tail -21 >&2 || true
  exit $rc
}
grep '^DISAGREEMENTS:' "$log" | tail -1
