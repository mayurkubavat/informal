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
engines=${engines:-informal-bmc,informal-kind,informal-ic3,informal-kind-ic3,abc-pdr-pp}
# informal stops its own engines at 90% of the cap; the scope kill is the backstop.
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
informal=${informal:-$PWD/target/release/informal}
# each=1 (default): every engine alone; each=0: all engines as one
# in-process portfolio per model (first definitive verdict wins).
each=$([ "${each:-1}" = 0 ] && echo "" || echo "--each")
# Work files live next to the db (not /tmp, which a reboot wipes), and
# each model's rows are appended to the db as soon as it finishes, so a
# crash or reboot loses at most the models in flight. Models already in
# the db are skipped: rerunning the same command resumes.
tmp="$PWD/bench/.work/$(basename "$db" .jsonl)"
mkdir -p "$tmp"
touch "$db"
todo="$tmp/todo.txt"
python3 -I - "$list" "$db" "$todo" <<'PY'
import json, sys
lst, db, out = sys.argv[1:]
done = set()
for l in open(db):
    try:
        done.add(json.loads(l)["model"])
    except Exception:
        pass
ms = [m.strip() for m in open(lst) if m.strip()]
left = [m for m in ms if m not in done]
open(out, "w").write("".join(m + "\n" for m in left))
print(f"{len(ms) - len(left)} of {len(ms)} models already in {db}; {len(left)} to run", file=sys.stderr)
PY
one() {
  m=$1; d=$tmp/$(echo "$m" | md5sum | cut -c1-12)
  mkdir -p "$d/m"; ln -s "$PWD/$m" "$d/m/$(basename "$m")"
  systemd-run --user --scope -q $slice -p MemoryMax="$mem" -p MemoryHigh="$high" \
    -p MemorySwapMax=0 \
    "$informal" --mem-gb "$budget" bench "$d/m" $each --only "$engines" \
    --timeout "$timeout" --jobs 1 --db "$d/runs.jsonl" >"$d/out.log" 2>&1 \
    || echo "  memout/error  $m"
  # Report the real model path, not the temp link.
  if [ -s "$d/runs.jsonl" ]; then
    sed "s#\"model\":\"[^\"]*\"#\"model\":\"$m\"#" "$d/runs.jsonl" > "$d/fixed.jsonl"
    flock "$db" cat "$d/fixed.jsonl" >> "$db"
  else
    # Killed before writing anything (cgroup OOM kill, signal): still one
    # row, so the model counts as attempted and a resume skips it.
    printf '{"model":"%s","mode":"%s","engine":"%s","verdict":"unknown","detail":"killed","wall_ms":0,"timeout_ms":%d}\n' \
      "$m" "$([ -n "$each" ] && echo single || echo portfolio)" \
      "$([ -n "$each" ] && echo "$engines" || echo portfolio)" "$((timeout * 1000))" \
      | flock "$db" tee -a "$db" >/dev/null
  fi
  grep "ms  " "$d/out.log" || true
  rm -rf "$d"
}
# Heavy steps wait in the dvq formal queue (~/linux-tuning/dvq): one at a time,
# CPU/memory capped. DVQ= runs them directly; dvq missing also runs directly.
DVQ="${DVQ-$HOME/.local/bin/dvq run formal --}"; [ -x "$HOME/.local/bin/dvq" ] || DVQ=
# The per-model scopes below leave the dvq scope, so dvq only queues this run;
# the per-model MemoryMax and the jobs fit check are the caps.
export -f one; export tmp timeout engines mem budget informal each slice high db
$DVQ xargs -a "$todo" -P "$jobs" -I{} bash -c 'one "$@"' _ {}
echo "== end $(date -Is)"
