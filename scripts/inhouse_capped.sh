#!/usr/bin/env bash
# Score in-house engines one model per process, each under a memory cap,
# so a model that blows up memory loses only its own result.
# usage: scripts/inhouse_capped.sh <model-list> <db> [timeout_s] [jobs] [mem]
# Scopes run in rdev.slice when that user unit exists (slice-wide cap,
# watched by rdev-guard), with MemoryHigh at 90% of mem so the kernel
# reclaims before the MemoryMax kill. jobs is cut so jobs x mem fits in
# 75% of MemTotal (at least 1).
set -euo pipefail
cd "$(dirname "$0")/.."
list=$1; db=$2; timeout=${3:-60}; jobs=${4:-3}; mem=${5:-7G}
engines=${engines:-fdv-bmc,fdv-kind,fdv-ic3,fdv-kind-ic3,abc-pdr-pp}
# fdv stops its own engines at 90% of the cap; the scope kill is the backstop.
budget=$(echo "${mem%G} * 0.9" | bc)
# mem in MiB (accepts K/M/G suffix or plain bytes), for MemoryHigh and the fit check.
mem_mib=$(awk -v m="$mem" 'BEGIN{n=m+0; u=toupper(substr(m,length(m)))
  if(u=="G")n*=1024; else if(u=="K")n/=1024; else if(u!="M")n/=1048576; printf "%d", n}')
high="$(( mem_mib * 9 / 10 ))M"
fit=$(( $(awk '/^MemTotal:/{printf "%d", $2/1024}' /proc/meminfo) * 3 / 4 / mem_mib ))
[ "$fit" -lt 1 ] && fit=1
if [ "$jobs" -gt "$fit" ]; then
  echo "warning: $jobs jobs x $mem exceeds 75% of RAM; using $fit jobs" >&2
  jobs=$fit
fi
slice=$(systemctl --user cat rdev.slice >/dev/null 2>&1 && echo "--slice=rdev.slice" || echo "")
fdv=${fdv:-$PWD/target/release/fdv}
# each=1 (default): every engine alone; each=0: all engines as one
# in-process portfolio per model (first definitive verdict wins).
each=$([ "${each:-1}" = 0 ] && echo "" || echo "--each")
tmp=$(mktemp -d)
one() {
  m=$1; d=$tmp/$(echo "$m" | md5sum | cut -c1-12)
  mkdir -p "$d/m"; ln -s "$PWD/$m" "$d/m/$(basename "$m")"
  systemd-run --user --scope -q $slice -p MemoryMax="$mem" -p MemoryHigh="$high" \
    -p MemorySwapMax=0 \
    "$fdv" --mem-gb "$budget" bench "$d/m" $each --only "$engines" \
    --timeout "$timeout" --jobs 1 --db "$d/runs.jsonl" >"$d/out.log" 2>&1 \
    || echo "  memout/error  $m"
  # Report the real model path, not the temp link.
  [ -f "$d/runs.jsonl" ] && sed "s#\"model\":\"[^\"]*\"#\"model\":\"$m\"#" "$d/runs.jsonl" > "$d/fixed.jsonl"
  grep "ms  " "$d/out.log" || true
}
export -f one; export tmp timeout engines mem budget fdv each slice high
xargs -a "$list" -P "$jobs" -I{} bash -c 'one "$@"' _ {}
cat "$tmp"/*/fixed.jsonl >> "$db" 2>/dev/null || true
echo "== end $(date -Is)"
