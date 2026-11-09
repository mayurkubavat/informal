#!/usr/bin/env bash
# Full HWMCC'24 bit-level run (all 319 models) for the phase-0 gate.
#
# usage: scripts/hwmcc_full.sh [timeout_s] [jobs]
# Single engines run first, several models at once; the rIC3 portfolio
# runs after, one model at a time, since it already uses 16 threads.
# Logs go to bench/runs-linux-full.jsonl and bench/full.log.
set -euo pipefail
cd "$(dirname "$0")/.."
timeout=${1:-300}
jobs=${2:-8}
db=bench/runs-linux-full.jsonl
log=bench/full.log
models=bench/hwmcc24/extracted
informal=target/release/informal
{
  echo "== start $(date -Is) timeout=${timeout}s jobs=$jobs"
  $informal bench "$models" --each --only ric3-ic3,ric3-bmc,abc-pdr \
    --timeout "$timeout" --jobs "$jobs" --db "$db"
  echo "== singles done $(date -Is)"
  $informal bench "$models" --only ric3 --timeout "$timeout" --jobs 1 --db "$db"
  echo "== portfolio done $(date -Is)"
  python3 -I scripts/hwmcc_check.py bench/hwmcc24/expected.json "$db"
  echo "== end $(date -Is)"
} >>"$log" 2>&1
