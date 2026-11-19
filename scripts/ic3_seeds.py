#!/usr/bin/env python3
"""Score an IC3 build over several seeds.

IC3's path (not its verdict) depends on which models its SAT solver
returns: moving one literal inside a clause once turned a 90 s proof into
a timeout. One run per model therefore says little about a change. This
runs `informal bench` once per seed (INFORMAL_IC3_SEED) and reports the
solved count per seed, the mean, and how often each model was solved.

usage: python3 -I scripts/ic3_seeds.py --models DIR|LIST [--bin PATH]
           [--engine informal-ic3] [--timeout 300] [--jobs 3]
           [--seeds 3] [--env KEY=VAL ...] [--tag NAME] [--out DIR]
       python3 -I scripts/ic3_seeds.py --compare A.json B.json

Run it on a quiet machine. Where the dvq job queue is installed
(~/.local/bin/dvq), each seed's bench waits in its formal class with 12
CPUs, the count the 300 s timeouts were measured with, and the --mem-gb cap
plus 2G (default --mem-gb 10, so 12G: a request near the queue's budget
would wait for ever, and one it can never start is refused); DVQ= in the environment runs the benches directly, and
DVQ_CPU/DVQ_MEM change the reservation. Compare two builds or settings by running it
twice with different --tag/--bin/--env and then --compare on the two
summaries: a model counts as gained or lost only when its solve count
differs by at least 2 seeds, or changes between none and all.
"""
import argparse
import json
import os
import subprocess
import sys


def dvq_prefix(env, mem_gb):
    """The dvq command a bench runs under, or [] to run it directly."""
    dvq = env.get("DVQ")
    if dvq is None:
        exe = os.path.expanduser("~/.local/bin/dvq")
        dvq = f"{exe} run formal --" if os.access(exe, os.X_OK) else ""
    if not dvq or env.get("DVQ_CLASS") or env.get("RDEV_JOB_ID"):
        return []
    env.setdefault("DVQ_CPU", "1200%")
    want = -int(-(mem_gb + 2) // 1)
    # The queue hands out at most its budget (RDEV_QUEUE_BUDGET, 75% of RAM:
    # about 20G on the 26G box) and also needs that much free at the start,
    # so a request near the budget can wait for ever. Refuse one that
    # cannot fit instead of waiting.
    budget = queue_budget_gb()
    if budget and want > budget * 0.65:
        sys.exit(f"ic3_seeds: --mem-gb {mem_gb:g} asks the queue for {want}G, more than it can start "
                 f"on this machine (budget {budget:.0f}G); use --mem-gb {max(1, int(budget * 0.65) - 2)} or less")
    env.setdefault("DVQ_MEM", f"{want}G")
    return dvq.split()


def queue_budget_gb():
    """The job queue's memory budget in GB (rdev queue status --json), or None."""
    try:
        out = subprocess.run(["rdev", "queue", "status", "--json"], capture_output=True, text=True, timeout=20).stdout
        d = json.loads(out)
    except (OSError, subprocess.SubprocessError, ValueError):
        return None
    v = d.get("memory", {}).get("budget") if isinstance(d, dict) else None
    return v / (1 << 30) if isinstance(v, (int, float)) and v > 0 else None


def run(args):
    out = args.out
    os.makedirs(out, exist_ok=True)
    if os.path.isfile(args.models):
        # A list of model paths: link them into one directory. Names get
        # a number in front, since lists may hold equal file names.
        d = os.path.join(out, f"{args.tag}-models")
        os.makedirs(d, exist_ok=True)
        for f in os.listdir(d):
            os.remove(os.path.join(d, f))
        paths = [l.strip() for l in open(args.models) if l.strip()]
        for i, m in enumerate(paths):
            os.symlink(os.path.abspath(m), os.path.join(d, f"{i:03d}-{os.path.basename(m)}"))
        args.models = d
    env = dict(os.environ)
    for kv in args.env:
        k, _, v = kv.partition("=")
        env[k] = v
    per_seed = []
    solved = {}
    wrong = set()
    verdicts = {}
    for seed in range(1, args.seeds + 1):
        db = os.path.join(out, f"{args.tag}-seed{seed}.jsonl")
        if os.path.exists(db):
            os.remove(db)
        e = dict(env, INFORMAL_IC3_SEED=str(seed))
        subprocess.run(
            dvq_prefix(e, args.mem_gb) + [args.bin, "--mem-gb", str(args.mem_gb), "bench", args.models,
             "--each", "--only", args.engine, "--timeout", str(args.timeout),
             "--jobs", str(args.jobs), "--db", db],
            env=e, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        n = 0
        for line in open(db):
            r = json.loads(line)
            m = os.path.basename(r["model"])
            solved.setdefault(m, [])
            if r["verdict"] in ("safe", "unsafe"):
                n += 1
                solved[m].append(round(r["wall_ms"] / 1000, 1))
                if verdicts.setdefault(m, r["verdict"]) != r["verdict"]:
                    wrong.add(m)
        per_seed.append(n)
        print(f"{args.tag} seed {seed}: {n} solved", file=sys.stderr)
    summary = {
        "tag": args.tag, "engine": args.engine, "timeout": args.timeout,
        "seeds": args.seeds, "env": args.env, "bin": args.bin,
        "per_seed": per_seed, "mean": sum(per_seed) / len(per_seed),
        "models": solved, "verdicts": verdicts, "conflicting": sorted(wrong),
    }
    path = os.path.join(out, f"{args.tag}.json")
    json.dump(summary, open(path, "w"), indent=1)
    show(summary)
    print(f"summary: {path}")
    return 1 if wrong else 0


def show(s):
    k = s["seeds"]
    print(f"{s['tag']}: {s['engine']} at {s['timeout']} s, solved per seed "
          f"{s['per_seed']}, mean {s['mean']:.1f}")
    always = sum(1 for t in s["models"].values() if len(t) == k)
    ever = sum(1 for t in s["models"].values() if t)
    print(f"  solved on every seed: {always}; on at least one: {ever}; "
          f"models: {len(s['models'])}")
    if s["conflicting"]:
        print(f"  CONFLICTING VERDICTS between seeds: {s['conflicting']}")


def compare(a_path, b_path):
    a, b = json.load(open(a_path)), json.load(open(b_path))
    show(a)
    show(b)
    ka, kb = a["seeds"], b["seeds"]
    gained, lost = [], []
    for m in sorted(set(a["models"]) | set(b["models"])):
        na, nb = len(a["models"].get(m, [])), len(b["models"].get(m, []))
        clear = abs(nb / kb - na / ka) * min(ka, kb) >= 2 or \
            (na == 0 and nb == kb) or (nb == 0 and na == ka)
        if clear and nb / kb > na / ka:
            gained.append(f"{m} ({na}/{ka} -> {nb}/{kb})")
        elif clear and nb / kb < na / ka:
            lost.append(f"{m} ({na}/{ka} -> {nb}/{kb})")
    print(f"mean solved: {a['mean']:.1f} -> {b['mean']:.1f}")
    print(f"clearly gained ({len(gained)}):")
    for g in gained:
        print("  +", g)
    print(f"clearly lost ({len(lost)}):")
    for g in lost:
        print("  -", g)
    for m in sorted(set(a["verdicts"]) & set(b["verdicts"])):
        if a["verdicts"][m] != b["verdicts"][m]:
            print(f"VERDICTS DISAGREE on {m}: {a['verdicts'][m]} vs {b['verdicts'][m]}")
            return 1
    return 0


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--compare", nargs=2, metavar=("A", "B"))
    p.add_argument("--models")
    p.add_argument("--bin", default="target/release/informal")
    p.add_argument("--engine", default="informal-ic3")
    p.add_argument("--timeout", type=int, default=300)
    p.add_argument("--jobs", type=int, default=3)
    p.add_argument("--seeds", type=int, default=3)
    p.add_argument("--mem-gb", type=float, default=10)
    p.add_argument("--env", action="append", default=[])
    p.add_argument("--tag", default="run")
    p.add_argument("--out", default="bench/seeds")
    args = p.parse_args()
    if args.compare:
        return compare(*args.compare)
    if not args.models:
        p.error("--models is required")
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
