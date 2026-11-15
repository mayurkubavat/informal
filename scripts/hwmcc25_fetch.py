#!/usr/bin/env python3
"""Fetch the HWMCC'25 bit-level safety models and their published answers.

    python3 -I scripts/hwmcc25_fetch.py [--data <dir>]

Downloads from the competition's Zenodo record (10.5281/zenodo.17428464,
CC-BY-4.0) into <dir> (default $INFORMAL_BENCH_DATA/hwmcc25, else
~/Silicon/DV/bench-data/hwmcc25, one copy for every clone on the machine),
extracts the 330 AIGER models, and writes <dir>/expected.json in the format
of bench/hwmcc24/expected.json: per model the answer the competition's
checkers gave (safe for unsat, unsafe for sat; unknown when none solved
it), whether a certificate for it was checked, and the fastest published
time. bench/hwmcc25 is made a link to <dir>, so the paths in
bench/hwmcc25-330.txt work in this clone. Run again to check the files.
"""
import argparse
import collections
import csv
import hashlib
import json
import os
import subprocess
import sys
import tarfile
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RECORD = "https://zenodo.org/api/records/17428464"
FILES = ["hwmcc25-benchmarks-bitlevel-safety.tar.gz", "hwmcc25-bitlevel-safety.csv",
         "hwmcc25-bitlevel-safety-cert.csv"]


def md5(path):
    h = hashlib.md5()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    default = os.path.join(os.environ.get("INFORMAL_BENCH_DATA", os.path.expanduser("~/Silicon/DV/bench-data")), "hwmcc25")
    ap.add_argument("--data", default=default)
    args = ap.parse_args()
    data = os.path.abspath(args.data)
    os.makedirs(data, exist_ok=True)

    record = json.load(urllib.request.urlopen(RECORD))
    sums = {f["key"]: f["checksum"].split(":", 1)[1] for f in record["files"]}
    for name in FILES:
        path = os.path.join(data, name)
        if not (os.path.exists(path) and md5(path) == sums[name]):
            print(f"downloading {name}")
            subprocess.run(["curl", "-LsSf", "-o", path, f"{RECORD}/files/{name}/content"], check=True)
        if md5(path) != sums[name]:
            sys.exit(f"hwmcc25: {name} does not match the record's checksum")
    models = os.path.join(data, "bitlevel/safety")
    if not os.path.isdir(models):
        with tarfile.open(os.path.join(data, FILES[0])) as t:
            t.extractall(data, filter="data")

    solved = collections.defaultdict(set)
    best = {}
    names = set()
    for r in csv.DictReader(open(os.path.join(data, FILES[1]))):
        names.add(r["benchmark"])
        if r["result"] in ("sat", "unsat"):
            solved[r["benchmark"]].add(r["result"])
            t = float(r["time_real"])
            best[r["benchmark"]] = min(t, best.get(r["benchmark"], t))
    certified = collections.defaultdict(set)
    for r in csv.DictReader(open(os.path.join(data, FILES[2]))):
        if r["result"] in ("sat", "unsat"):
            certified[r["benchmark"]].add(r["result"])
    expected, conflicts = {}, []
    for b in sorted(names):
        s = solved.get(b, set())
        if len(s) > 1:
            conflicts.append(b)
        answer = {"unsat": "safe", "sat": "unsafe"}[next(iter(s))] if len(s) == 1 else "unknown"
        expected[b] = {"expected": answer, "certified": bool(s) and s == certified.get(b, set()) | s
                       and bool(certified.get(b)), "best_s": best.get(b)}
    json.dump(expected, open(os.path.join(data, "expected.json"), "w"), indent=1, sort_keys=True)
    missing = [b for b in expected if not os.path.exists(os.path.join(models, b))]
    link = os.path.join(REPO, "bench/hwmcc25")
    if not os.path.lexists(link):
        os.symlink(data, link)
    counts = collections.Counter(v["expected"] for v in expected.values())
    print(f"hwmcc25: {len(expected)} models in {data}: {counts['safe']} safe, {counts['unsafe']} unsafe, "
          f"{counts['unknown']} unsolved at the competition; {sum(v['certified'] for v in expected.values())} "
          f"with a checked certificate; {len(conflicts)} with conflicting answers; {len(missing)} missing")
    return 1 if conflicts or missing else 0


if __name__ == "__main__":
    sys.exit(main())
