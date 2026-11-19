#!/usr/bin/env python3
"""Random AIGER models, checked by every engine, compared.

    python3 -I scripts/fuzz.py [--count 200] [--seed 1] [--timeout 10]
                               [--engines informal-bmc,informal-kind,informal-ic3,ric3,abc-pdr]

Each model comes from aigfuzz (AIGER tools, github.com/arminbiere/aiger;
small safety circuits, one property), seeded so that a run can be repeated
exactly. Every engine checks it alone (`informal check --only <engine>`).
The oracle is agreement: two engines that give opposite definitive verdicts
on one model have found a bug in one of them, and so has an in-house
engine whose own certificate check rejects its answer. A disagreement is
shrunk with `informal reduce --disagree` and kept, original and reduced,
in target/fuzz/failures/<seed>/. The in-house engines check every verdict
they give, so a wrong one usually shows as a rejected certificate; the
comparison also catches the external engines.

The last line is "FUZZ: <n> models, <d> with a definitive verdict, <k>
disagreements"; the exit status is 1 if there is a disagreement.
aigfuzz is looked for as $AIGFUZZ or in $INFORMAL_TOOLS_DIR/aiger/
(default ~/Silicon/DV/tools), the AIGER checkout pinned in tools.lock
(scripts/tools.sh build aiger).
"""
import argparse
import os
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VERDICT = {10: "unsafe", 20: "safe"}


def aigfuzz():
    repo_tools = os.path.join(REPO, "tools")
    default_tools = repo_tools if os.path.isdir(repo_tools) else os.path.expanduser("~/Silicon/DV/tools")
    tools = os.environ.get("INFORMAL_TOOLS_DIR", default_tools)
    path = os.environ.get("AIGFUZZ") or os.path.join(tools, "aiger/aigfuzz")
    if not os.access(path, os.X_OK):
        sys.exit(f"fuzz: no aigfuzz at {path}: scripts/tools.sh build aiger")
    return path


def check(informal, model, engine, timeout, config):
    p = subprocess.run([informal, "check", model, "--engines", config, "--only", engine, "--timeout", str(timeout)],
                       capture_output=True, text=True, cwd=REPO)
    out = p.stdout + p.stderr
    # An in-house engine whose certificate check fails withdraws its verdict
    # (informal-bmc, "verdict withdrawn, certificate ...").
    if "verdict withdrawn" in out:
        return "rejected", out
    return VERDICT.get(p.returncode, "unknown"), out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--count", type=int, default=200)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--timeout", type=int, default=10)
    ap.add_argument("--engines", default="informal-bmc,informal-kind,informal-ic3,ric3,abc-pdr")
    ap.add_argument("--informal", default=os.path.join(REPO, "target/release/informal"))
    ap.add_argument("--config", default="config/engines.toml", help="the external engines' configuration")
    ap.add_argument("--large", action="store_true", help="aigfuzz's large circuits instead of small ones")
    args = ap.parse_args()
    if not (0 <= args.seed and args.seed + args.count <= 2**31 - 1):
        sys.exit("fuzz: aigfuzz reads its seed as a C int: seeds must be between 0 and 2147483647")
    fuzz = aigfuzz()
    if not os.access(args.informal, os.X_OK):
        sys.exit(f"fuzz: no {args.informal} (cargo build --release -p informal-cli, or --informal)")
    engines = args.engines.split(",")
    work = os.path.join(REPO, "target/fuzz")
    os.makedirs(work, exist_ok=True)
    definitive = 0
    failures = []
    for seed in range(args.seed, args.seed + args.count):
        model = os.path.join(work, f"{seed}.aig")
        subprocess.run([fuzz, "-S", "-m", "-l" if args.large else "-s", "-o", model, str(seed)], check=True)
        got = {e: check(args.informal, model, e, args.timeout, args.config) for e in engines}
        verdicts = {e: v for e, (v, _) in got.items()}
        known = {e: v for e, v in verdicts.items() if v in ("safe", "unsafe")}
        definitive += bool(known)
        rejected = [e for e, v in verdicts.items() if v == "rejected"]
        opposite = len(set(known.values())) > 1
        if opposite or rejected:
            keep = os.path.join(work, "failures", str(seed))
            os.makedirs(keep, exist_ok=True)
            os.replace(model, os.path.join(keep, "original.aig"))
            with open(os.path.join(keep, "verdicts.txt"), "w") as f:
                for e, (v, out) in got.items():
                    f.write(f"== {e}: {v}\n{out}\n")
            line = f"seed {seed}: " + ", ".join(f"{e} {v}" for e, v in verdicts.items())
            if opposite:
                a = next(e for e, v in known.items() if v == "safe")
                b = next(e for e, v in known.items() if v == "unsafe")
                r = subprocess.run([args.informal, "reduce", os.path.join(keep, "original.aig"),
                                    "--disagree", f"{a},{b}", "--timeout", str(args.timeout),
                                    "--engines", args.config,
                                    "-o", os.path.join(keep, "reduced.aag"),
                                    "--workdir", os.path.join(keep, "reduce")],
                                   capture_output=True, text=True, cwd=REPO)
                line += f"; reduced to {keep}/reduced.aag" if r.returncode == 0 else "; reduce failed"
            failures.append(line)
            print("DISAGREE " + line, flush=True)
        else:
            os.remove(model)
    for l in failures:
        print(l)
    print(f"FUZZ: {args.count} models (seeds {args.seed}..{args.seed + args.count - 1}), "
          f"{definitive} with a definitive verdict, {len(failures)} disagreements")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
