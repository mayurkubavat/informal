#!/usr/bin/env python3
"""Fetch public JasperGold / VC Formal Tcl scripts and tally what
`informal shell --check` says about them.

  scripts/vendor_tcl_corpus.py fetch <dir>     download the scripts into <dir>
  scripts/vendor_tcl_corpus.py tally <dir>     run --check on each, print the tally
                                               (INFORMAL=path picks the binary)

The corpus stays outside the repo (like the HWMCC models). `fetch` records
each repository's commit in <dir>/SOURCES.txt. The tally ranks unknown
commands and rejected or ignored options by how many scripts use them: the
list of what the shell should learn next (docs/fpv-shell.md).
"""
import collections, json, os, re, subprocess, sys, urllib.request

# Repositories that keep vendor formal flows in the open.
REPOS = [
    "lowRISC/opentitan",
    "lowRISC/ibex",
    "openhwgroup/core-v-verif",
    "openhwgroup/cv32e40x",
    "openhwgroup/cv32e40s",
    "openhwgroup/cva6",
    "chipsalliance/caliptra-rtl",
    "pulp-platform/axi",
    "pulp-platform/common_cells",
    "black-parrot/black-parrot",
    "nvdla/hw",
    "ucb-bar/chipyard",
    "SymbioticEDA/riscv-formal",
    "westerndigitalcorporation/swerv_eh1",
    "chipsalliance/Cores-VeeR-EL2",
]
PATH = re.compile(r"(formal|fpv|jasper|jg|vcf|fv)", re.I)
VENDOR = re.compile(
    r"^\s*(analyze|elaborate|prove|check_fv|read_file|fvassert|fvassume|check_cov|"
    r"set_fml_appmode|get_design_info|jasper_scoreboard_3|check_conn)\b",
    re.M,
)


def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": "informal-corpus"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read()


def fetch(out):
    os.makedirs(out, exist_ok=True)
    sources = []
    for repo in REPOS:
        try:
            info = json.loads(get(f"https://api.github.com/repos/{repo}"))
            branch = info["default_branch"]
            tree = json.loads(
                get(f"https://api.github.com/repos/{repo}/git/trees/{branch}?recursive=1")
            )
        except Exception as e:  # rate limit, renamed repo: skip it
            print(f"{repo}: skipped ({e})", file=sys.stderr)
            continue
        kept = 0
        for node in tree.get("tree", []):
            p = node["path"]
            if node["type"] != "blob" or not p.endswith(".tcl") or not PATH.search(p):
                continue
            if node.get("size", 0) > 200_000:
                continue
            try:
                text = get(f"https://raw.githubusercontent.com/{repo}/{tree['sha']}/{p}")
            except Exception:
                continue
            if not VENDOR.search(text.decode("utf-8", "replace")):
                continue
            dest = os.path.join(out, repo.replace("/", "__"), p)
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            with open(dest, "wb") as f:
                f.write(text)
            kept += 1
        sources.append(f"{repo} {tree['sha']} {kept} scripts")
        print(sources[-1], file=sys.stderr)
    with open(os.path.join(out, "SOURCES.txt"), "w") as f:
        f.write("\n".join(sources) + "\n")


def option(message):
    """The option or sub-case a finding is about, for grouping."""
    m = re.search(r"(?:unsupported option|ignoring) (\S+)", message)
    if m:
        return m.group(1)
    m = re.search(r"(-\w+)", message)
    return m.group(1) if m else ""


def tally(root):
    exe = os.path.abspath(os.environ.get("INFORMAL", "target/release/informal"))
    scripts = sorted(
        os.path.join(d, f) for d, _, fs in os.walk(root) for f in fs if f.endswith(".tcl")
    )
    by = {k: collections.defaultdict(set) for k in ("unknown", "error", "ignored", "tcl")}
    honoured = collections.Counter()
    clean = failed = 0
    for s in scripts:
        r = subprocess.run(
            [exe, "shell", "--check", "--json", os.path.basename(s)],
            cwd=os.path.dirname(s), capture_output=True, text=True, timeout=120,
        )
        try:
            rep = json.loads(r.stdout)
        except ValueError:
            failed += 1
            print(f"{s}: no report ({r.stderr.strip()[:200]})", file=sys.stderr)
            continue
        clean += all(f["kind"] == "ignored" for f in rep["findings"])
        for name, u in rep["commands"].items():
            honoured[name] += u["honoured"]
        for f in rep["findings"]:
            key = f["command"]
            if key in ("task", "open", "exec", "file"):
                pass
            elif f["kind"] in ("error", "ignored") and option(f["message"]):
                key += " " + option(f["message"])
            if f["kind"] == "tcl":
                key = re.sub(r'"[^"]*"', '"..."', f["message"])[:70]
            by[f["kind"]][key].add(s)
    n = len(scripts)
    print(f"{n} scripts, {clean} would run to the end, {failed} not checked\n")
    titles = {
        "unknown": "Commands informal does not have",
        "error": "Commands or options informal rejects",
        "ignored": "Accepted and ignored",
        "tcl": "Other Tcl errors (environment, missing procs or files)",
    }
    for kind, title in titles.items():
        rows = sorted(by[kind].items(), key=lambda kv: (-len(kv[1]), kv[0]))
        print(f"## {title} ({len(rows)})\n")
        print("| scripts | what |\n| ---: | --- |")
        for key, where in rows[:40]:
            print(f"| {len(where)} | `{key}` |")
        if len(rows) > 40:
            print(f"| | ... {len(rows) - 40} more |")
        print()
    print("## Commands honoured (uses)\n")
    print(", ".join(f"`{k}` {v}" for k, v in honoured.most_common()))


if __name__ == "__main__":
    if len(sys.argv) != 3 or sys.argv[1] not in ("fetch", "tally"):
        sys.exit(__doc__)
    (fetch if sys.argv[1] == "fetch" else tally)(sys.argv[2])
