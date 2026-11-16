#!/usr/bin/env python3
"""Benchmark measurements that can be repeated and published: one harness for
the paper's numbers.

    python3 -I scripts/measure.py freeze
        Build this clone's commit (release, --locked) and keep the binary as
        bench/bin/informal-<commit>. Refuses a clone with uncommitted changes.

    python3 -I scripts/measure.py run --binary bench/bin/informal-<commit>
            --models bench/full319.txt --config portfolio --timeout 300
            --mem 16G --repeats 3 [--jobs 1] [--max-outside 0.10]
            [--out bench/measure/<name>]
        Run every model of the list, each in its own cgroup scope with a hard
        memory limit (MemoryMax, no swap), REPEATS times. Before each model it
        measures how busy the machine is outside the harness and waits until
        that is below --max-outside (a fraction of all CPUs); the value goes
        in the run's row. It refuses to start at all on a busy machine.
        --config is "portfolio" (all engines of config/engines.toml racing)
        or an engine name such as ric3 or informal-ic3 (that engine alone).
        Rerunning the same command resumes: finished (model, repeat) pairs
        are skipped. The run directory holds manifest.json (binary hash,
        tools.lock, machine, limits, model list hash) and runs.jsonl.

    python3 -I scripts/measure.py table bench/measure/<a> [bench/measure/<b> ...]
            [--expected bench/hwmcc24/expected.json] [--format md|csv|tex]
        The tables the paper prints, computed from the run directories: per
        configuration the number solved (median over repeats, with the
        lowest and highest), wrong verdicts, PAR-2, and the models solved by
        that configuration only. No number is typed by hand.

A verdict that contradicts the published answer is counted as wrong and
listed; a configuration with a wrong verdict is marked in every table.
"""
import argparse
import concurrent.futures
import datetime
import hashlib
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import threading
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# Each model runs in a scope of its own in rdev.slice, where the box's job
# queue counts it (~/remote-development/doc/queue.md); the harness itself
# waits in the queue as a formal job that reserves every CPU.
SLICE = "rdev.slice"
SCOPE_PREFIX = "informal-measure-"
_seen = {}


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def git(*args):
    return subprocess.run(["git", "-C", REPO, *args], capture_output=True, text=True, check=True).stdout.strip()


# How busy the machine is outside the harness -----------------------------

def busy_seconds():
    """CPU seconds the whole machine has spent busy since boot."""
    f = open("/proc/stat").readline().split()[1:]
    user, nice, system, idle, iowait, irq, softirq, steal = (int(x) for x in f[:8])
    return (user + nice + system + irq + softirq + steal) / os.sysconf("SC_CLK_TCK")


def own_seconds():
    """CPU seconds the harness's model scopes have used. A scope that has
    ended keeps the last value read, so the sum only grows."""
    uid = os.getuid()
    base = f"/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/{SLICE}"
    try:
        names = [n for n in os.listdir(base) if n.startswith(SCOPE_PREFIX) and n.endswith(".scope")]
    except OSError:
        names = []
    for n in names:
        try:
            for line in open(os.path.join(base, n, "cpu.stat")):
                if line.startswith("usage_usec "):
                    _seen[n] = int(line.split()[1]) / 1e6
        except OSError:
            pass
    return sum(_seen.values())


def outside(secs=3.0):
    """Fraction of all CPUs busy outside the harness over the next `secs`."""
    b0, o0 = busy_seconds(), own_seconds()
    time.sleep(secs)
    b1, o1 = busy_seconds(), own_seconds()
    return max(0.0, ((b1 - b0) - (o1 - o0)) / (secs * os.cpu_count()))


def wait_quiet(limit, stop):
    """Wait until the machine is quiet enough; return (outside, seconds waited)."""
    t0 = time.time()
    while not stop.is_set():
        o = outside()
        if o <= limit:
            return o, round(time.time() - t0)
        time.sleep(30)
    raise KeyboardInterrupt


# freeze ------------------------------------------------------------------

def freeze(_args):
    if git("status", "--porcelain", "--untracked-files=no"):
        sys.exit("measure: this clone has uncommitted changes; a frozen binary must be a commit")
    commit = git("rev-parse", "--short=7", "HEAD")
    env = dict(os.environ)
    subprocess.run(["cargo", "build", "--release", "--locked", "-p", "informal-cli"], cwd=REPO, check=True, env=env)
    os.makedirs(os.path.join(REPO, "bench/bin"), exist_ok=True)
    dest = os.path.join(REPO, "bench/bin", f"informal-{commit}")
    shutil.copy2(os.path.join(REPO, "target/release/informal"), dest)
    print(f"frozen: {os.path.relpath(dest, REPO)} (sha256 {sha256(dest)[:16]})")


# run ---------------------------------------------------------------------

def mem_bytes(text):
    m = re.fullmatch(r"(\d+(?:\.\d+)?)([KMG]?)", text.upper())
    if not m:
        sys.exit(f"measure: --mem {text}: give a size such as 16G")
    return int(float(m.group(1)) * {"": 1, "K": 1 << 10, "M": 1 << 20, "G": 1 << 30}[m.group(2)])


def machine():
    cpu = next((l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")), "?")
    mem = next(int(l.split()[1]) * 1024 for l in open("/proc/meminfo") if l.startswith("MemTotal"))
    return {"cpu": cpu, "cpus": os.cpu_count(), "mem_bytes": mem, "kernel": platform.release(), "host": platform.node()}


def manifest_for(args, models):
    tools = open(os.path.join(REPO, "tools.lock")).read() if os.path.exists(os.path.join(REPO, "tools.lock")) else ""
    return {
        "binary": os.path.relpath(os.path.abspath(args.binary), REPO),
        "binary_sha256": sha256(args.binary),
        "config": args.config,
        "timeout_s": args.timeout,
        "mem_limit": args.mem,
        "repeats": args.repeats,
        "jobs": args.jobs,
        "max_outside": args.max_outside,
        "models": os.path.relpath(os.path.abspath(args.models), REPO),
        "models_sha256": hashlib.sha256("\n".join(models).encode()).hexdigest(),
        "model_count": len(models),
        "tools_lock": [l for l in tools.splitlines() if l and not l.startswith("#")],
        "harness_commit": git("rev-parse", "HEAD"),
        "machine": machine(),
    }


def run(args):
    models = [l.strip() for l in open(args.models) if l.strip() and not l.startswith("#")]
    missing = [m for m in models if not os.path.exists(os.path.join(REPO, m))]
    if missing:
        sys.exit(f"measure: {len(missing)} models of {args.models} are missing, e.g. {missing[0]}")
    binary = os.path.abspath(args.binary)
    if not os.access(binary, os.X_OK):
        sys.exit(f"measure: {args.binary} is not an executable")
    name = args.out or os.path.join(
        "bench/measure", f"{os.path.basename(binary)}-{args.config}-{args.timeout}s")
    out = os.path.join(REPO, name)
    os.makedirs(out, exist_ok=True)
    man = manifest_for(args, models)
    mpath = os.path.join(out, "manifest.json")
    if os.path.exists(mpath):
        old = json.load(open(mpath))
        keep = ("binary_sha256", "config", "timeout_s", "mem_limit", "models_sha256", "repeats")
        changed = [k for k in keep if old.get(k) != man[k]]
        if changed:
            sys.exit(f"measure: {name} holds a run with other settings ({', '.join(changed)}); use another --out")
    else:
        man["started"] = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
        json.dump(man, open(mpath, "w"), indent=2)
    db = os.path.join(out, "runs.jsonl")
    done = set()
    if os.path.exists(db):
        for l in open(db):
            r = json.loads(l)
            done.add((r["model"], r["repeat"]))
    todo = [(m, k) for k in range(1, args.repeats + 1) for m in models if (m, k) not in done]
    print(f"measure: {name}: {len(done)} runs done, {len(todo)} to go "
          f"({len(models)} models x {args.repeats} repeats, {args.timeout} s, {args.mem})")
    if not todo:
        return
    o = outside()
    if o > args.max_outside and not args.wait:
        sys.exit(f"measure: the machine is busy: {o:.0%} of its CPUs are in use outside this harness "
                 f"(limit {args.max_outside:.0%}). Timing runs need a quiet machine; --wait waits for one.")

    budget = round(mem_bytes(args.mem) * 0.9 / (1 << 30), 2)
    high = str(int(mem_bytes(args.mem) * 0.9))
    each = [] if args.config == "portfolio" else ["--each", "--only", args.config]
    lock = threading.Lock()
    stop = threading.Event()
    work = os.path.join(out, ".work")

    def one(item):
        model, rep = item
        quiet, waited = wait_quiet(args.max_outside, stop)
        d = os.path.join(work, hashlib.sha256(f"{model}#{rep}".encode()).hexdigest()[:12])
        shutil.rmtree(d, ignore_errors=True)
        os.makedirs(os.path.join(d, "m"))
        os.symlink(os.path.join(REPO, model), os.path.join(d, "m", os.path.basename(model)))
        load1 = os.getloadavg()[0]
        unit = f"{SCOPE_PREFIX}{os.getpid()}-{os.path.basename(d)}-{rep}"
        cmd = ["systemd-run", "--user", "--scope", "-q", f"--slice={SLICE}", f"--unit={unit}",
               "-p", f"MemoryMax={mem_bytes(args.mem)}", "-p", f"MemoryHigh={high}", "-p", "MemorySwapMax=0",
               binary, "--mem-gb", str(budget), "bench", os.path.join(d, "m"), *each,
               "--timeout", str(args.timeout), "--jobs", "1", "--db", os.path.join(d, "runs.jsonl")]
        t0 = time.time()
        p = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True)
        wall = round((time.time() - t0) * 1000)
        rows = []
        if os.path.exists(os.path.join(d, "runs.jsonl")):
            rows = [json.loads(l) for l in open(os.path.join(d, "runs.jsonl")) if l.strip()]
        if not rows:
            rows = [{"verdict": "unknown", "detail": "killed (memory limit or signal)" if p.returncode else "no result",
                     "engine": args.config, "wall_ms": wall, "timeout_ms": args.timeout * 1000}]
        for r in rows:
            r.update({"model": model, "repeat": rep, "config": args.config, "outside_cpu": round(quiet, 3),
                      "waited_s": waited, "load1": round(load1, 2), "exit": p.returncode})
        with lock:
            with open(db, "a") as f:
                for r in rows:
                    f.write(json.dumps(r, sort_keys=True) + "\n")
        shutil.rmtree(d, ignore_errors=True)
        v = rows[0]
        print(f"  {rep}/{args.repeats} {v['verdict']:8} {v.get('wall_ms', 0) / 1000:8.1f} s  {model}", flush=True)

    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        try:
            for f in concurrent.futures.as_completed([pool.submit(one, t) for t in todo]):
                f.result()
        except KeyboardInterrupt:
            stop.set()
            pool.shutdown(cancel_futures=True)
            sys.exit("measure: stopped; rerun the same command to resume")
    with open(mpath) as f:
        man = json.load(f)
    man["finished"] = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
    json.dump(man, open(mpath, "w"), indent=2)
    print(f"measure: done; tables with: python3 -I scripts/measure.py table {name}")


# table -------------------------------------------------------------------

def model_key(path):
    return re.sub(r"^.*?/(sample60|extracted|bitlevel/safety)/", "", path)


def summarize(run_dir, expected):
    man = json.load(open(os.path.join(run_dir, "manifest.json")))
    rows = [json.loads(l) for l in open(os.path.join(run_dir, "runs.jsonl")) if l.strip()]
    timeout = man["timeout_s"]
    reps = sorted({r["repeat"] for r in rows})
    by = {}  # (model, repeat) -> row (the portfolio writes one row per model)
    for r in rows:
        by[(r["model"], r["repeat"])] = r
    models = sorted({m for m, _ in by})
    solved, par2, wrong = {}, {}, []
    per_model = {}
    for k in reps:
        s, p = 0, 0.0
        for m in models:
            r = by.get((m, k))
            ok = False
            if r and r["verdict"] in ("safe", "unsafe"):
                want = expected.get(model_key(m), {}).get("expected") if expected else None
                if want in ("safe", "unsafe") and want != r["verdict"]:
                    wrong.append((m, k, r["verdict"], want))
                else:
                    ok = True
            t = r["wall_ms"] / 1000 if r else 2 * timeout
            s += ok
            p += t if ok else 2 * timeout
            per_model.setdefault(m, []).append(t if ok else None)
        solved[k], par2[k] = s, p
    # A model counts as solved when most repeats solved it.
    majority = {m for m, ts in per_model.items() if sum(t is not None for t in ts) * 2 > len(ts)}
    return {
        "name": os.path.basename(os.path.normpath(run_dir)),
        "config": man["config"], "binary": os.path.basename(man["binary"]), "timeout": timeout,
        "mem": man["mem_limit"], "models": len(models), "repeats": len(reps),
        "complete": len(by) == man["model_count"] * man["repeats"],
        "solved_med": statistics.median(solved.values()), "solved_min": min(solved.values()),
        "solved_max": max(solved.values()), "par2_med": statistics.median(par2.values()),
        "wrong": wrong, "majority": majority,
        "outside_max": max((r.get("outside_cpu", 0) for r in rows), default=0),
    }


def table(args):
    expected = json.load(open(args.expected)) if args.expected else {}
    runs = [summarize(d, expected) for d in args.runs]
    for r in runs:
        others = set().union(*(o["majority"] for o in runs if o is not r)) if len(runs) > 1 else set()
        r["unique"] = len(r["majority"] - others)
    head = ["configuration", "binary", "time limit", "memory", "models", "repeats", "solved (median)",
            "solved (min-max)", "PAR-2 (median, s)", "unique", "wrong runs", "max outside CPU"]
    body = [[r["config"], r["binary"], f'{r["timeout"]} s', r["mem"], str(r["models"]), str(r["repeats"]),
             f'{r["solved_med"]:g}', f'{r["solved_min"]}-{r["solved_max"]}', f'{r["par2_med"]:.0f}',
             str(r["unique"]) if len(runs) > 1 else "-", str(len(r["wrong"])), f'{r["outside_max"]:.0%}']
            for r in runs]
    if args.format == "csv":
        print(",".join(head))
        for row in body:
            print(",".join(row))
    elif args.format == "tex":
        print("\\begin{tabular}{" + "l" * 2 + "r" * (len(head) - 2) + "}")
        print(" & ".join(head) + " \\\\ \\hline")
        for row in body:
            print(" & ".join(c.replace("_", "\\_").replace("%", "\\%") for c in row) + " \\\\")
        print("\\end{tabular}")
    else:
        print("| " + " | ".join(head) + " |")
        print("|" + "---|" * len(head))
        for row in body:
            print("| " + " | ".join(row) + " |")
    for r in runs:
        if not r["complete"]:
            print(f"\nnote: {r['name']} is not complete", file=sys.stderr)
        for m, k, got, want in r["wrong"]:
            print(f"WRONG: {r['name']} repeat {k}: {m}: {got}, published {want}", file=sys.stderr)
    return 1 if any(r["wrong"] for r in runs) else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("freeze")
    r = sub.add_parser("run")
    r.add_argument("--binary", required=True)
    r.add_argument("--models", required=True)
    r.add_argument("--config", required=True)
    r.add_argument("--timeout", type=int, required=True)
    r.add_argument("--mem", required=True)
    r.add_argument("--repeats", type=int, default=3)
    r.add_argument("--jobs", type=int, default=1)
    r.add_argument("--max-outside", type=float, default=0.10)
    r.add_argument("--wait", action="store_true", help="wait for a quiet machine instead of refusing")
    r.add_argument("--out")
    t = sub.add_parser("table")
    t.add_argument("runs", nargs="+")
    t.add_argument("--expected")
    t.add_argument("--format", choices=["md", "csv", "tex"], default="md")
    args = ap.parse_args()
    os.chdir(REPO)
    # A run waits in the box's job queue (dvq) when it has one, as a formal
    # job that reserves every CPU: other queued work does not start beside a
    # timing run. The memory sits in the per-model scopes, which the queue
    # counts on their own. DVQ= runs it directly.
    dvq = os.environ.get("DVQ", os.path.expanduser("~/.local/bin/dvq") + " run formal --").split()
    if (args.cmd == "run" and dvq and os.access(dvq[0], os.X_OK)
            and not (os.environ.get("DVQ_CLASS") or os.environ.get("RDEV_JOB_ID"))):
        env = dict(os.environ)
        env.setdefault("DVQ_CPU", f"{os.cpu_count() * 100}%")
        env.setdefault("DVQ_MEM", "1G")
        os.execvpe(dvq[0], [*dvq, sys.executable, "-I", os.path.abspath(__file__), *sys.argv[1:]], env)
    return {"freeze": freeze, "run": run, "table": table}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
