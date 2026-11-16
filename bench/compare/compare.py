#!/usr/bin/env python3
"""End-to-end comparison of InFormal with SymbiYosys and EBMC.

The same SystemVerilog sources, with the properties written in them, go
through each flow; this script records which designs and properties each
flow accepts, the verdict each property gets, and the time taken.

    compare.py tools            check (or install) the pinned comparison tools
    compare.py fetch            fetch the pinned third-party designs
    compare.py list             the cases, after expansion
    compare.py run [options]    run cases through the flows, append to the run file
    compare.py report           write the tables from the run file

Everything about one run is in bench/compare/README.md. Third-party RTL is
never copied into the repository: it is fetched to $INFORMAL_COMPARE_DIR
(default ~/.cache/informal/compare) at the commits in cases.toml.
"""
from __future__ import annotations

import argparse
import glob as globlib
import hashlib
import json
import os
import re
import resource
import shutil
import signal
import subprocess
import sys
import time
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
TOOLS = Path(os.environ.get("INFORMAL_TOOLS_DIR", Path.home() / "Silicon/DV/tools"))
WORK = Path(os.environ.get("INFORMAL_COMPARE_DIR", Path.home() / ".cache/informal/compare"))
SUITE = TOOLS / "oss-cad-suite"
EBMC = TOOLS / "ebmc/usr/bin/ebmc"
INFORMAL = Path(os.environ.get("INFORMAL_BIN", REPO / "target/release/informal"))
FLOWS = ("informal", "sby", "ebmc")


def die(msg: str) -> None:
    print(f"compare: {msg}", file=sys.stderr)
    sys.exit(2)


# --------------------------------------------------------------------------
# The pinned tools and designs


def lock() -> dict:
    return tomllib.loads((HERE / "tools.toml").read_text())


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def download(url: str, dest: Path, digest: str) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    if not dest.exists():
        print(f"compare: downloading {url}")
        subprocess.run(["curl", "-fsSL", "-o", str(dest), url], check=True)
    have = sha256(dest)
    if have != digest:
        die(f"{dest.name}: sha256 is {have}, tools.toml pins {digest}")


def cmd_tools(args) -> None:
    """Install the comparison tools from their pinned release files, or
    check the installed ones. They are run as outside programs only."""
    pins = lock()["tool"]
    dl = TOOLS / "compare-dl"
    suite, ebmc = pins["oss-cad-suite"], pins["ebmc"]
    if not (SUITE / "bin/sby").exists():
        archive = dl / suite["url"].rsplit("/", 1)[1]
        download(suite["url"], archive, suite["sha256"])
        subprocess.run(["tar", "xzf", str(archive), "-C", str(TOOLS)], check=True)
    if not EBMC.exists():
        archive = dl / ebmc["url"].rsplit("/", 1)[1]
        download(ebmc["url"], archive, ebmc["sha256"])
        subprocess.run(["dpkg-deb", "-x", str(archive), str(TOOLS / "ebmc")], check=True)
    bad = 0
    for name, want in versions_wanted().items():
        have = tool_versions().get(name, "missing")
        ok = want in have
        bad |= not ok
        print(f"{'ok ' if ok else 'BAD'} {name}: {have}" + ("" if ok else f" (tools.toml: {want})"))
    sys.exit(bad)


def versions_wanted() -> dict:
    pins = lock()["tool"]
    return {
        "oss-cad-suite": pins["oss-cad-suite"]["version"],
        "yosys": pins["oss-cad-suite"]["yosys"],
        "sby": pins["oss-cad-suite"]["sby"],
        "ebmc": pins["ebmc"]["version"],
    }


def first_line(cmd: list[str], env=None) -> str:
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=30)
        return (out.stdout + out.stderr).strip().splitlines()[0]
    except (OSError, IndexError, subprocess.TimeoutExpired):
        return "missing"


_versions: dict | None = None


def tool_versions() -> dict:
    global _versions
    if _versions is None:
        env = suite_env()
        suite_version = SUITE / "VERSION"
        _versions = {
            "oss-cad-suite": suite_version.read_text().strip() if suite_version.exists() else "missing",
            "yosys": first_line([str(SUITE / "bin/yosys"), "-V"], env),
            "sby": first_line([str(SUITE / "bin/sby"), "--version"], env),
            "ebmc": first_line([str(EBMC), "--version"]),
            "informal": git_head(),
        }
    return _versions


def git_head() -> str:
    """The commit of main that the InFormal binary was built from: the
    point where this checkout leaves origin/main, marked +local when the
    tool's own sources differ from it."""
    if os.environ.get("INFORMAL_COMMIT"):
        # The binary was built earlier and the checkout has moved on.
        return os.environ["INFORMAL_COMMIT"][:9]

    def git(*a):
        return subprocess.run(["git", "-C", str(REPO), *a], capture_output=True, text=True).stdout.strip()
    base = git("merge-base", "HEAD", "origin/main") or git("rev-parse", "HEAD")
    dirty = git("diff", "--name-only", base, "--", "crates", "Cargo.toml", "Cargo.lock", "config")
    return base[:9] + ("+local" if dirty else "")


def suite_env() -> dict:
    """The environment for SymbiYosys: the suite's own programs first. It is
    never used for InFormal, whose portfolio would otherwise pick up the
    suite's rIC3, ABC, Pono and BtorMC."""
    env = dict(os.environ)
    env["PATH"] = f"{SUITE}/bin:{SUITE}/py3bin:" + env.get("PATH", "")
    env.pop("PYTHONHOME", None)
    return env


def source_root(name: str) -> Path:
    if name == "local":
        return REPO
    if name == "sby-examples":
        return SUITE / "examples"
    return WORK / "src" / name


def cmd_fetch(args) -> None:
    """Fetch each third-party source at its pinned commit. Nothing from the
    fetched trees is run."""
    for name, src in lock()["source"].items():
        root = source_root(name)
        if "archive" in src:
            if root.exists():
                continue
            archive = WORK / "dl" / (name + ".tar.gz")
            download(src["archive"], archive, src["sha256"])
            root.mkdir(parents=True)
            subprocess.run(["tar", "xzf", str(archive), "-C", str(root), "--strip-components=1"], check=True)
        else:
            if not (root / ".git").exists():
                root.mkdir(parents=True, exist_ok=True)
                subprocess.run(["git", "init", "-q", str(root)], check=True)
                subprocess.run(["git", "-C", str(root), "remote", "add", "origin", src["git"]], check=True)
            have = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
            if have != src["commit"]:
                subprocess.run(["git", "-C", str(root), "fetch", "-q", "--depth", "1", "origin", src["commit"]], check=True)
                subprocess.run(["git", "-C", str(root), "-c", "advice.detachedHead=false", "checkout", "-q", src["commit"]], check=True)
        print(f"compare: {name} at {src.get('commit', src.get('sha256'))[:12]} in {root}")


# --------------------------------------------------------------------------
# Cases


def load_cases() -> list[dict]:
    doc = tomllib.loads((HERE / "cases.toml").read_text())
    cases = []
    for c in doc["case"]:
        root = source_root(c.get("source", "local"))
        if "glob" in c:
            # One case per file; the top module is the first one in the file.
            for path in sorted(globlib.glob(str(root / c["glob"]))):
                rel = str(Path(path).relative_to(root))
                if rel in c.get("skip", []):
                    continue
                text = Path(path).read_text(errors="replace")
                d = dict(c)
                if c.get("informal_test"):
                    setup = informal_test_setup(text)
                    if setup is None:
                        continue
                    d.update(setup)
                m = re.search(r"^\s*module\s+(\w+)", text, re.M)
                if not m:
                    continue
                d["top"] = Path(path).stem if c.get("informal_test") else m.group(1)
                d["files"] = [rel]
                d["name"] = f"{c['name']}/{Path(path).stem}"
                cases.append(d)
        else:
            cases.append(dict(c))
    for c in cases:
        c.setdefault("source", "local")
        c.setdefault("group", "neutral")
        c.setdefault("incdirs", [])
        c.setdefault("defines", [])
        c.setdefault("clock", "clk")
        c.setdefault("reset", "")
        c["root"] = source_root(c["source"])
    return cases


def informal_test_setup(text: str) -> dict | None:
    """A tests/sv design is compared when it has properties in the RTL and
    its script only names one clock and a reset. Its command properties
    (the `//% assert ...` lines) exist in InFormal's script form only, so
    they are left out for every flow. Returns None for a design that needs
    more setup than that (cut points, black boxes, several clocks, ...),
    or whose script is expected to fail."""
    if re.search(r"^//!", text, re.M):
        return None
    if not re.search(r"\b(assert|cover|assume)\s+(property|final|\()|\b(assert|cover|assume)\s*\(", text):
        return None
    clock, reset = "", ""
    for line in re.findall(r"^//% *(.*)$", text, re.M):
        word = line.split()[0]
        if word == "assume":
            # An assumption given by command changes every answer, and the
            # other flows cannot be given it.
            return None
        if word in ("assert", "cover"):
            continue
        m = re.fullmatch(r"clock\s+(\w+)", line.strip())
        if m and not clock:
            clock = m.group(1)
            continue
        m = re.fullmatch(r"reset\s+-expression\s+\{(!?\w+)\}", line.strip())
        if m and not reset:
            reset = m.group(1)
            continue
        return None
    expect = {}
    for name, status in re.findall(r"^//= *(\S+) +(\S+)", text, re.M):
        expect[name] = status
    return {"clock": clock, "reset": reset, "expect": expect}


def cmd_list(args) -> None:
    for c in select(load_cases(), args):
        print(f"{c['name']:40} {c['group']:9} top={c['top']} clock={c['clock'] or '-'} reset={c['reset'] or '-'}")


def select(cases: list[dict], args) -> list[dict]:
    only = getattr(args, "only", None)
    if only:
        cases = [c for c in cases if any(re.search(o, c["name"]) for o in only)]
    return cases


# --------------------------------------------------------------------------
# Running one program


def run_program(cmd: list[str], cwd: Path, log: Path, timeout: float, env: dict | None, mem_gb: float) -> dict:
    """Run to the end or to the time limit, in a process group of its own so
    that a limit ends the helpers a tool started too. Records wall time and
    the CPU time of the whole process tree."""
    def setup():
        os.setsid()
        if mem_gb > 0:
            cap = int(mem_gb * (1 << 30))
            resource.setrlimit(resource.RLIMIT_AS, (cap, cap))
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    start = time.monotonic()
    timed_out = False
    with open(log, "w") as out:
        out.write("$ " + " ".join(cmd) + "\n")
        out.flush()
        proc = subprocess.Popen(cmd, cwd=cwd, stdout=out, stderr=subprocess.STDOUT, env=env, preexec_fn=setup)
        try:
            rc = proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(proc.pid, signal.SIGTERM)
            try:
                rc = proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                rc = proc.wait()
    wall = time.monotonic() - start
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = (after.ru_utime + after.ru_stime) - (before.ru_utime + before.ru_stime)
    return {"rc": rc, "timeout": timed_out, "wall_s": round(wall, 3), "cpu_s": round(cpu, 3)}


def paths(c: dict) -> list[str]:
    return [str(c["root"] / f) for f in c["files"]]


def prop_key(name: str, top: str) -> str:
    """One spelling for a property's name across the flows: the path below
    the top module, with unnamed assertions as _assert_N."""
    name = re.sub(r"^Verilog::(\$root\.)?", "", name)
    if name.startswith(top + "."):
        name = name[len(top) + 1:]
    name = re.sub(r"\b(assert|assume|cover)\.(\d+)$", r"_\1_\2", name)
    # EBMC numbers unnamed blocks (fifo.2.3.a_full); the others leave them out.
    name = ".".join(part for part in name.split(".") if not part.isdigit())
    return name


# --------------------------------------------------------------------------
# InFormal


INHOUSE = ["informal-bmc", "informal-bmc-10", "informal-bmc-65", "informal-kind", "informal-ic3",
           "informal-ic3-cadical", "informal-kind-ic3", "informal-ic3-mt", "informal-wbmc"]


def run_informal(c: dict, config: str, out: Path, args) -> dict:
    session = out / "session.json"
    script = out / "run.tcl"
    lines = ["clear -all"]
    opts = "".join(f" +define+{d}" for d in c["defines"]) + "".join(f" +incdir+{c['root'] / i}" for i in c["incdirs"])
    lines.append(f"analyze -sv12{opts} " + " ".join(paths(c)))
    lines.append(f"elaborate -top {c['top']}")
    if c["clock"]:
        lines.append(f"clock {c['clock']}")
    lines.append(f"reset -expression {{{c['reset']}}}" if c["reset"] else "reset -none")
    lines += c.get("informal_setup", [])
    lines.append(f"set_prove_time_limit {int(args.prop_timeout)}s")
    if config == "inhouse":
        lines.append("informal::set_engines " + " ".join(INHOUSE))
    # The session is written even when prove stops with an error, so a
    # refusal is recorded with its message.
    lines.append("set cmp_rc [catch {prove -all} cmp_msg]")
    lines.append(f"set f [open {{{session}}} w]; puts $f [informal::session]; close $f")
    lines.append('if {$cmp_rc} { puts stderr "prove: $cmp_msg"; exit 1 }')
    script.write_text("\n".join(lines) + "\n")
    cmd = [str(INFORMAL), "shell", str(script), "--flavor", "jg", "--workdir", str(out / "work"),
           "--engines", str(REPO / "config/engines.toml")]
    # InFormal's limit is per property, enforced by the tool; the limit on
    # the whole run is only there to end a hang. A design with several
    # undecided properties must not be cut off before it reports them.
    r = run_program(cmd, REPO, out / "log.txt", max(args.timeout, args.informal_timeout), None, 0)
    log = (out / "log.txt").read_text(errors="replace")
    props = []
    elaborated = False
    if session.exists():
        doc = json.loads(session.read_text())
        elaborated = bool(doc.get("elaborated"))
        for p in doc.get("props", []):
            if not p.get("rtl") or not p.get("enabled", True) or p["kind"] == "assume":
                continue
            st = p.get("status") or {}
            status = st.get("status", "unprocessed") if isinstance(st, dict) else str(st)
            verdict = {"proven": "proven", "cex": "cex", "covered": "covered", "unreachable": "unreachable",
                       "undetermined": "undetermined", "error": "unsupported"}.get(status, "undetermined")
            src = p.get("source") or {}
            entry = {"key": prop_key(p["name"], c["top"]), "name": p["name"], "kind": p["kind"],
                     "verdict": verdict, "raw": status, "line": src.get("line"),
                     "file": Path(src["file"]).name if src.get("file") else None}
            for k in ("engine", "depth", "ms", "message", "detail"):
                if isinstance(st, dict) and st.get(k) is not None:
                    entry[k] = st[k]
            props.append(entry)
    refusal = None
    if not props:
        errs = [l for l in log.splitlines() if re.search(r"\berror\b|^Error|prove:", l)]
        skipped = [l for l in log.splitlines() if "NOT CHECKED" in l or "NOT MODELLED" in l]
        if r["timeout"]:
            refusal = "time limit"
        elif skipped:
            refusal = re.sub(r"^informal: ", "", skipped[0])[:300]
        elif errs:
            refusal = errs[0][:300]
        elif elaborated:
            # The design was read and the tool said nothing was wrong, but
            # it holds no property the shell checks.
            refusal = "elaborated without an error, and no property of the design was found"
        else:
            refusal = (log.strip().splitlines() or ["no output"])[-1][:300]
    r.update(accepted=bool(props), refusal=refusal, props=props)
    return r


# --------------------------------------------------------------------------
# EBMC


EBMC_CONFIGS = {
    "bmc": lambda a: ["--bound", str(a.depth)],
    "kind": lambda a: ["--k-induction", "--bound", str(a.depth)],
    "bdd": lambda a: ["--bdd"],
    "ic3": lambda a: ["--ic3"],
    # EBMC's own translation of liveness properties to safety ones, with
    # its two engines that prove.
    "kind-l2s": lambda a: ["--k-induction", "--bound", str(a.depth), "--liveness-to-safety"],
    "bdd-l2s": lambda a: ["--bdd", "--liveness-to-safety"],
}


def run_ebmc(c: dict, config: str, out: Path, args) -> dict:
    result = out / "result.json"
    cmd = [str(EBMC), *paths(c), "--top", c["top"], "--systemverilog"]
    cmd += [f"-D{d}" for d in c["defines"]]
    for i in c["incdirs"]:
        cmd += ["-I", str(c["root"] / i)]
    if c["reset"]:
        neg = c["reset"].startswith("!")
        sig = c["reset"].lstrip("!")
        cmd += ["--reset", f"{'!' if neg else ''}{c['top']}.{sig}"]
    cmd += EBMC_CONFIGS[config](args) + ["--json-result", str(result)]
    r = run_program(cmd, out, out / "log.txt", args.timeout, None, args.mem_gb)
    log = (out / "log.txt").read_text(errors="replace")
    props = []
    if result.exists():
        try:
            doc = json.loads(result.read_text())
        except json.JSONDecodeError:
            doc = {}
        kinds = dict(re.findall(r"^\[(\S+)\] (always|cover|assume)?", log, re.M))
        for p in doc.get("properties", []):
            raw = p.get("status", "")
            name = re.sub(r"^Verilog::(\$root\.)?", "", p["identifier"])
            desc = re.search(r"^\[" + re.escape(name) + r"\] (.*?): [A-Z]{3,}", log, re.M)
            kind = "cover" if desc and desc.group(1).startswith("cover") else "assert"
            if kind == "cover" and config == "ic3":
                # EBMC's IC3 engine answers a cover as if it were an
                # assertion of the same expression (REFUTED where the
                # bounded check shows the cover trace), so its cover
                # statuses are kept raw and not turned into verdicts.
                props.append({"key": prop_key(name, c["top"]), "name": name, "kind": kind,
                              "verdict": "undetermined", "raw": raw})
                continue
            if kind == "cover":
                # EBMC says PROVED for a cover that is reached.
                verdict = ("covered" if raw == "PROVED" else
                           "unreachable" if raw == "REFUTED" else
                           "bounded" if raw.startswith("REFUTED up to") else None)
            else:
                verdict = ("proven" if raw == "PROVED" else
                           "cex" if raw == "REFUTED" else
                           "bounded" if raw.startswith("PROVED up to") else None)
            if verdict is None:
                verdict = ("unsupported" if raw.startswith(("UNSUPPORTED", "FAILURE")) else
                           "dropped" if raw.startswith("DROPPED") else
                           "assumed" if raw.startswith("ASSUMED") else "undetermined")
            if verdict == "assumed":
                continue
            props.append({"key": prop_key(name, c["top"]), "name": name, "kind": kind, "verdict": verdict, "raw": raw})
    refusal = None
    if not props:
        errs = [l for l in log.splitlines()[1:] if re.search(r"error|unknown|unsupported|Assertion|failed|not found", l, re.I)]
        refusal = "time limit" if r["timeout"] else (errs[0] if errs else (log.strip().splitlines() or ["no output"])[-1])[:300]
    r.update(accepted=bool(props), refusal=refusal, props=props)
    return r


# --------------------------------------------------------------------------
# SymbiYosys


SBY_CONFIGS = {
    # Bounded check and k-induction with the default solver; --keep-going
    # so that one failing assertion does not hide the status of the others.
    "smtbmc": ("prove", "smtbmc --keep-going"),
    "pdr": ("prove", "abc pdr"),
    "cover": ("cover", "smtbmc"),
}

RESET_BIND = """\
// Reset for the SymbiYosys runs: active in the first cycle, released after,
// which is what InFormal's `reset` command and EBMC's --reset do.
module cmp_reset(input logic clk, input logic rst_active);
  logic cmp_first = 1'b1;
  always_ff @(posedge clk) cmp_first <= 1'b0;
  always_comb assume (rst_active == cmp_first);
endmodule
bind {top} cmp_reset cmp_reset_i(.clk({clock}), .rst_active({reset}));
"""


def run_sby(c: dict, config: str, out: Path, args) -> dict:
    env = suite_env()
    work = out / "work"
    task = config.partition(":")[2]
    if config.startswith("shipped"):
        # An example that comes with SymbiYosys runs as its authors wrote it.
        # It is read where it lies (its file names are relative); only the
        # work directory is elsewhere.
        sby_file = c["sby_shipped"]
        # A copy of its directory (or of the tree named in `sby_tree`, for a
        # script that reaches outside its directory), so that the work
        # directory lands where the script expects it.
        tree = c.get("sby_tree", str(Path(sby_file).parent))
        shutil.copytree(c["root"] / tree, out / "tree", ignore=shutil.ignore_patterns(".git"))
        run_dir = (out / "tree" / Path(sby_file).relative_to(tree)).parent
        work = run_dir / (Path(sby_file).stem + (f"_{task}" if task else ""))
        where = []
        if c.get("sby_workdir"):
            # The design's own makefile passes -d.
            work = run_dir / (c["sby_workdir"] + (f"_{task}" if task else ""))
            work.parent.mkdir(parents=True, exist_ok=True)
            where = ["-d", str(work.relative_to(run_dir))]
        cmd = [str(SUITE / "bin/sby"), "-f", *where, Path(sby_file).name] + ([task] if task else [])
        mode = None
    else:
        mode, engine = SBY_CONFIGS[config]
        run_dir = out
        files = paths(c)
        if c["reset"]:
            (out / "cmp_reset.sv").write_text(RESET_BIND.format(top=c["top"], clock=c["clock"], reset=c["reset"]))
            files.append(str(out / "cmp_reset.sv"))
        opts = "".join(f" -D {d}" for d in c.get("sby_defines", c["defines"])) + "".join(f" -I {c['root'] / i}" for i in c["incdirs"])
        text = (f"[options]\nmode {mode}\ndepth {args.depth}\n\n[engines]\n{engine}\n\n[script]\n"
                f"plugin -i slang\nread_slang --formal{opts} --top {c['top']} {' '.join(files)}\n"
                f"prep -top {c['top']}\n" + "".join(l + "\n" for l in c.get("sby_script", [])))
        (out / "run.sby").write_text(text)
        cmd = [str(SUITE / "bin/sby"), "-f", "-d", str(work), "run.sby"]
    r = run_program(cmd, run_dir, out / "log.txt", args.timeout, env, args.mem_gb)
    log = (out / "log.txt").read_text(errors="replace")
    done = re.search(r"DONE \((\w+), rc=(\d+)\)", log)
    overall = done.group(1) if done else ("TIMEOUT" if r["timeout"] else "ERROR")
    if mode is None:
        m = re.search(r"^mode\s+(\w+)", (work / "config.sby").read_text() if (work / "config.sby").exists() else "", re.M)
        mode = m.group(1) if m else "bmc"
    props = []
    status = subprocess.run([str(SUITE / "bin/sby"), "--statusfmt", "jsonl", "--latest", "-d", str(work)],
                            capture_output=True, text=True, env=env, cwd=run_dir)
    for line in status.stdout.splitlines():
        try:
            p = json.loads(line)
        except json.JSONDecodeError:
            continue
        if p.get("kind") not in ("ASSERT", "COVER") or "name" not in p:
            continue
        raw = p.get("status", "UNKNOWN")
        cover = p["kind"] == "COVER"
        if cover and mode != "cover" or not cover and mode == "cover":
            continue
        if cover:
            verdict = "covered" if raw == "PASS" else "bounded" if overall in ("FAIL", "PASS") else "undetermined"
        elif raw == "FAIL":
            verdict = "cex"
        elif mode == "prove" and (raw == "PASS" or overall == "PASS"):
            verdict = "proven"
        elif mode == "bmc" and (raw == "PASS" or overall == "PASS") or mode == "prove" and overall == "UNKNOWN" and "smtbmc" in log:
            verdict = "bounded"
        else:
            verdict = "undetermined"
        loc = re.match(r"(.*?):(\d+)", p.get("location") or "")
        entry = {"key": prop_key(p["name"], c["top"]), "name": p["name"], "kind": "cover" if cover else "assert",
                 "verdict": verdict, "raw": raw, "file": Path(loc.group(1)).name if loc else None,
                 "line": int(loc.group(2)) if loc else None}
        if "depth" in p:
            entry["depth"] = p["depth"]
        props.append(entry)
    refusal = None
    if overall == "ERROR" or (not props and overall != "PASS"):
        errs = [re.sub(r"^SBY [\d:]+ \[[^\]]*\] ", "", l) for l in log.splitlines() if "ERROR" in l and "DONE" not in l]
        refusal = "time limit" if r["timeout"] else (errs[0] if errs else "no properties found")[:300]
    r.update(accepted=refusal is None, refusal=refusal, props=props if refusal is None else [], overall=overall)
    return r


# --------------------------------------------------------------------------
# run


def run_reference(c: dict, config: str, out: Path, args) -> dict:
    """What the design holds, from a parser none of the three flows wrote
    for itself: slang (the stand-alone program in the suite) elaborates the
    same files and its syntax tree is searched for assertion statements.
    This gives the number of properties a flow should have found. It says
    nothing about verdicts."""
    ast = out / "ast.json"
    cmd = [str(SUITE / "bin/slang"), "-q", *paths(c), "--top", c["top"], "--ast-json", str(ast)]
    cmd += [f"-D{d}" for d in c["defines"]]
    for i in c["incdirs"]:
        cmd += ["-I", str(c["root"] / i)]
    r = run_program(cmd, out, out / "log.txt", args.timeout, suite_env(), 0)
    log = (out / "log.txt").read_text(errors="replace")
    counts: dict = {}
    if ast.exists():
        def walk(x):
            if isinstance(x, dict):
                if x.get("kind") in ("ConcurrentAssertion", "ImmediateAssertion"):
                    what = {"Assert": "assert", "Assume": "assume", "CoverProperty": "cover", "CoverSequence": "cover",
                            "Restrict": "assume", "Expect": "assert"}.get(x.get("assertionKind"), "other")
                    form = "concurrent" if x["kind"] == "ConcurrentAssertion" else "immediate"
                    counts[f"{form} {what}"] = counts.get(f"{form} {what}", 0) + 1
                for v in x.values():
                    walk(v)
            elif isinstance(x, list):
                for v in x:
                    walk(v)
        try:
            walk(json.loads(ast.read_text()))
        except (json.JSONDecodeError, RecursionError):
            pass
        ast.unlink()
    errors = [l for l in log.splitlines() if ": error:" in l]
    r.update(accepted=not errors and r["rc"] == 0, refusal=errors[0][:300] if errors else None, props=[], counts=counts)
    return r


RUNNERS = {"informal": run_informal, "sby": run_sby, "ebmc": run_ebmc, "reference": run_reference}


def configs_for(flow: str, c: dict) -> list[str]:
    if flow == "reference":
        return ["slang"]
    if flow == "informal":
        return ["default", "inhouse"]
    if flow == "ebmc":
        return list(EBMC_CONFIGS)
    if c.get("sby_shipped"):
        return [f"shipped:{t}" if t else "shipped" for t in c.get("sby_tasks", [""])]
    return list(SBY_CONFIGS)


def cmd_run(args) -> None:
    if not INFORMAL.exists() and "informal" in args.flows:
        die(f"no {INFORMAL}: build it (cargo build --release -p informal) or set INFORMAL_BIN")
    cases = select(load_cases(), args)
    run_id = args.id or time.strftime("%Y%m%dT%H%M%S", time.gmtime())
    run_dir = WORK / "runs" / run_id
    run_dir.mkdir(parents=True, exist_ok=True)
    out_file = Path(args.out) if args.out else HERE / "results" / f"{run_id}.jsonl"
    out_file.parent.mkdir(parents=True, exist_ok=True)
    load = os.getloadavg()[0]
    meta = {"record": "run", "id": run_id, "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "purpose": args.purpose, "tools": tool_versions(), "timeout_s": args.timeout,
            "prop_timeout_s": args.prop_timeout, "informal_timeout_s": args.informal_timeout,
            "depth": args.depth, "mem_gb": args.mem_gb,
            "load_at_start": round(load, 1), "cpus": os.cpu_count(), "nice": os.nice(0),
            "machine": os.environ.get("COMPARE_MACHINE", "")}
    if args.purpose == "timing" and load > 1.0:
        die(f"load is {load:.1f}: a timing run needs a quiet machine (use --purpose verdicts)")
    done = set()
    if args.skip_done and out_file.exists():
        for line in out_file.read_text().splitlines():
            r = json.loads(line)
            if r["record"] == "result":
                done.add((r["case"], r["flow"], r["config"]))
    with open(out_file, "a") as out:
        out.write(json.dumps(meta) + "\n")
        for c in cases:
            for flow in args.flows:
                for config in configs_for(flow, c):
                    if args.configs and config not in args.configs or (c["name"], flow, config) in done:
                        continue
                    d = run_dir / c["name"].replace("/", "__") / f"{flow}-{config.replace(':', '_')}"
                    if d.exists():
                        shutil.rmtree(d)
                    d.mkdir(parents=True)
                    for rep in range(args.repeat):
                        r = RUNNERS[flow](c, config, d, args)
                        rec = {"record": "result", "case": c["name"], "group": c["group"], "flow": flow,
                               "config": config, "rep": rep, "load": round(os.getloadavg()[0], 1),
                               "machine": os.environ.get("COMPARE_MACHINE", ""), **r,
                               "log": str((d / "log.txt").relative_to(WORK))}
                        out.write(scrub_paths(json.dumps(rec)) + "\n")
                        out.flush()
                        got = ", ".join(f"{v} {k}" for k, v in sorted(tally(r["props"]).items())) or r["refusal"] \
                            or ", ".join(f"{v} {k}" for k, v in sorted(r.get("counts", {}).items())) or "no assertions"
                        print(f"{c['name']:36} {flow:8} {config:8} {r['wall_s']:8.2f}s  {got}", flush=True)
                        if r["timeout"]:
                            break  # a time limit is not a time: no point repeating it
    print(f"compare: results in {out_file}, logs in {run_dir}")


def tally(props: list[dict]) -> dict:
    t: dict = {}
    for p in props:
        t[p["verdict"]] = t.get(p["verdict"], 0) + 1
    return t


# --------------------------------------------------------------------------
# report

DEFINITE = ("proven", "cex", "covered", "unreachable")
ORDER = {"conflict": -1, "cex": 0, "covered": 0, "proven": 1, "unreachable": 1, "bounded": 2, "undetermined": 3,
         "unsupported": 4, "dropped": 4}


LIVENESS = re.compile(r"\bs_eventually\b(?!\s*\[)|\bs_until(_with)?\b|\beventually\b(?!\s*\[)")


def liveness_keys(c: dict) -> set:
    """The labels of the case's properties that are liveness properties
    (an unbounded s_eventually, eventually or strong until), from the
    source text."""
    found = set()
    for f in c["files"]:
        try:
            text = (c["root"] / f).read_text(errors="replace")
        except OSError:
            continue
        for m in re.finditer(r"\b(\w+)\s*:\s*assert\s+property\s*\(([^;]*)", text):
            if LIVENESS.search(m.group(2)):
                found.add(m.group(1))
    return found


def ebmc_bdd_liveness(recs: list[dict], c: dict) -> list[dict]:
    """EBMC's plain BDD engine answers REFUTED, without a trace, on
    liveness properties that its own --liveness-to-safety engines prove
    (bench/compare/README.md, Differences). Its REFUTED on a liveness
    property is therefore not taken as a verdict; its PROVED is (it agrees
    with the liveness-to-safety engines wherever both decide)."""
    live = liveness_keys(c)
    out = []
    for r in recs:
        if r["flow"] == "ebmc" and r["config"] == "bdd" and live:
            props = []
            for p in r["props"]:
                if p["verdict"] == "cex" and p["key"].rsplit(".", 1)[-1] in live:
                    p = dict(p, verdict="undetermined", note="plain BDD REFUTED on a liveness property")
                props.append(p)
            r = dict(r, props=props)
        out.append(r)
    return out


def merged(recs: list[dict]) -> dict:
    """The verdict of one flow on one case, over its configurations: per
    property the definite verdict its configurations give, or `conflict`
    when two of them give different ones."""
    props: dict = {}
    for r in recs:
        for p in r["props"]:
            cur = props.get(p["key"])
            new = dict(p, config=r["config"])
            if cur is None:
                props[p["key"]] = new
            elif cur.get("conflict"):
                if p["verdict"] in DEFINITE:
                    cur["conflict"] += f", {r['config']}: {p['verdict']}"
            elif cur["verdict"] in DEFINITE and p["verdict"] in DEFINITE and cur["verdict"] != p["verdict"]:
                # The flow contradicts itself: it has no verdict on this
                # property, and the report lists what each configuration said.
                cur["conflict"] = f"{cur['config']}: {cur['verdict']}, {r['config']}: {p['verdict']}"
                cur["verdict"] = "conflict"
            elif ORDER[p["verdict"]] < ORDER[cur["verdict"]]:
                props[p["key"]] = new
    return props


def read_results(path: Path) -> tuple[dict, list[dict]]:
    meta, recs, metas = {}, [], []
    for line in path.read_text().splitlines():
        r = json.loads(line)
        if r["record"] == "run":
            meta = r
            metas.append(r)
        else:
            recs.append(r)
    # A later record for the same case, flow and configuration replaces an
    # earlier one (a rerun of one case appended to the same file).
    last: dict = {}
    for r in recs:
        if r.get("rep", 0) == 0:
            last[(r["case"], r["flow"], r["config"])] = dict(r, walls=[], cpus=[])
        cur = last.get((r["case"], r["flow"], r["config"]))
        if cur is not None:
            cur["walls"].append(r["wall_s"])
            cur["cpus"].append(r["cpu_s"])
    meta = dict(meta, all=metas)
    return meta, list(last.values())


UNNAMED = re.compile(r"^(?:(.*)\.)?(?:_(assert|cover)_(\d+)|_witness_\.\S+)$")


def align_unnamed(by_flow: dict) -> None:
    """One key for a property that has no label. InFormal and SymbiYosys
    give its source line, so it becomes `<scope>.<kind>@<line>`. EBMC gives
    only a running number over the statements of the design, so its
    unnamed properties are paired with those lines in source order, and
    only when it has exactly as many of that kind as the lines known."""
    lines: dict = {"assert": set(), "cover": set()}
    for flow in ("informal", "sby"):
        mine = by_flow.get(flow, {})
        for key in list(mine):
            p = mine[key]
            m = UNNAMED.match(key)
            if not m or not p.get("line"):
                continue
            scope = (m.group(1) + ".") if m.group(1) else ""
            new = f"{scope}{p['kind']}@{p['line']}"
            n = 2
            while new in mine:
                new = f"{scope}{p['kind']}@{p['line']}#{n}"
                n += 1
            mine[new] = mine.pop(key)
            lines[p["kind"]].add(new)
    ebmc = by_flow.get("ebmc", {})

    def where(key):
        scope, _, rest = key.rpartition(".")
        line, _, n = rest.partition("@")[2].partition("#")
        return (int(line), scope, int(n or 1))

    for kind in ("assert", "cover"):
        have = sorted(((int(UNNAMED.match(k).group(3)), k) for k, p in ebmc.items()
                       if p["kind"] == kind and UNNAMED.match(k) and UNNAMED.match(k).group(3)))
        want = sorted(lines[kind], key=where)
        if have and len(have) == len(want):
            for (_, old), new in zip(have, want):
                ebmc[new] = ebmc.pop(old)


def cmd_report(args) -> None:
    results = Path(args.results) if args.results else sorted((HERE / "results").glob("*.jsonl"))[-1]
    meta, all_recs = read_results(results)
    recs = [r for r in all_recs if r["flow"] != "reference"]
    cases = {c["name"]: c for c in load_cases()}
    names = [n for n in cases if any(r["case"] == n for r in recs)]
    table: dict = {}
    for n in names:
        by_flow = {}
        for flow in FLOWS:
            mine = [r for r in recs if r["case"] == n and r["flow"] == flow]
            if flow == "ebmc":
                mine = ebmc_bdd_liveness(mine, cases[n])
            if mine:
                by_flow[flow] = merged(mine)
            # EBMC's bounded check marks the design's assumptions ASSUMED and
            # they are left out; its other engines list some of them as
            # unsupported properties. Not properties: dropped here too.
            bmc = [r for r in mine if flow == "ebmc" and r["config"] == "bmc" and r["accepted"]]
            if bmc:
                checked = {p["key"] for p in bmc[0]["props"]}
                by_flow[flow] = {k: v for k, v in by_flow[flow].items() if k in checked}
        align_unnamed(by_flow)
        table[n] = by_flow
    out = []
    w = out.append
    w("# InFormal, SymbiYosys and EBMC on the same SystemVerilog\n")
    w("Generated by `bench/compare/compare.py report` from "
      f"`bench/compare/results/{results.name}`; do not edit by hand. "
      "What is run and how is in `bench/compare/README.md`.\n")
    t = meta.get("tools", {})
    used = {(r.get("machine") or "box") for r in recs}
    parts = {}
    for m in meta.get("all", [meta]):
        k = m.get("machine") or "box"
        if k in used:
            parts.setdefault(k, []).append(m)
    w(f"- Run `{meta.get('id')}` ({meta.get('purpose')}), {len(meta.get('all', [meta]))} sittings on "
      f"{len(parts)} machine(s), the first started {min(m.get('started', '') for m in meta.get('all', [meta]))}:")
    for k, ms in sorted(parts.items()):
        n = sum(1 for r in recs if (r.get("machine") or "box") == k)
        loads = [m.get("load_at_start") for m in ms if m.get("load_at_start") is not None]
        w(f"  {k}: {n} tool runs, {ms[0].get('cpus')} CPUs, load at the start of a sitting "
          f"{min(loads)} to {max(loads)}, harness at `{', '.join(sorted({m['tools'].get('informal', '?') for m in ms}))}`.")
    w(f"- InFormal: the tool's sources of `98b4b81` on every machine (some binaries were built from the later "
      f"commits named above, which differ from it only in the LLM crate and one test file); {t.get('sby')}; {t.get('yosys')} "
      f"(OSS CAD Suite {t.get('oss-cad-suite')}); {t.get('ebmc')}.")
    w(f"- Limits: {meta.get('timeout_s')} s per SymbiYosys or EBMC run (one configuration, all properties), "
      f"{meta.get('prop_timeout_s')} s per property in InFormal, "
      f"depth {meta.get('depth')} for the bounded engines of SymbiYosys and EBMC.")
    if meta.get("purpose") != "timing":
        w("- **The times in this run are not measurements**: the machine was busy and the tools ran at low priority. "
          "They are kept in the run file and left out of the tables.")
    w("")

    groups = []
    for n in names:
        g = cases[n]["group"]
        if g not in groups:
            groups.append(g)
    label = {"informal": "InFormal's own tests", "sby": "SymbiYosys examples", "ebmc": "EBMC examples",
             "neutral": "Third-party designs"}

    w("## Summary\n")
    w("Properties are counted once per design. *Definite* is proven, counterexample, covered or unreachable "
      "(a property on which two configurations of one flow contradict each other counts as undetermined for that flow); "
      "*bounded* is no counterexample (or no cover trace) up to the depth; *refused* is a property in a design "
      "the flow did not read, or one its engines do not support.\n")
    w("| Set | Designs | Properties | Flow | Designs read | Definite | Bounded | Undetermined | Refused |")
    w("| --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: |")
    totals_rows = []
    for g in groups + ["all"]:
        ns = [n for n in names if g == "all" or cases[n]["group"] == g]
        allkeys = {n: set().union(*[set(table[n].get(f, {})) for f in FLOWS]) for n in ns}
        nprops = sum(len(k) for k in allkeys.values())
        for flow in FLOWS:
            read = sum(1 for n in ns if table[n].get(flow))
            cnt = {"definite": 0, "bounded": 0, "undetermined": 0, "refused": 0}
            for n in ns:
                mine = table[n].get(flow, {})
                for k in allkeys[n]:
                    v = mine.get(k, {}).get("verdict")
                    if v in DEFINITE:
                        cnt["definite"] += 1
                    elif v == "bounded":
                        cnt["bounded"] += 1
                    elif v in ("undetermined", "conflict"):
                        cnt["undetermined"] += 1
                    else:
                        cnt["refused"] += 1
            total = sum(1 for c in cases.values() if g == "all" or c["group"] == g)
            shown = f"{len(ns)}" if len(ns) == total else f"{len(ns)} of {total}"
            row = (f"| {label.get(g, 'All')} | {shown} | {nprops} | {flow} | {read} | {cnt['definite']} | "
                   f"{cnt['bounded']} | {cnt['undetermined']} | {cnt['refused']} |")
            w(row)
    w("")

    w("## Verdict agreement\n")
    agree = {}
    conflicts = []
    for n in names:
        keys = set().union(*[set(table[n].get(f, {})) for f in FLOWS])
        for k in sorted(keys):
            vs = {f: table[n].get(f, {}).get(k, {}).get("verdict") for f in FLOWS}
            for f in FLOWS:
                c = table[n].get(f, {}).get(k, {}).get("conflict")
                if c:
                    conflicts.append((n, k, f"within {f}: {c}"))
            for a in range(len(FLOWS)):
                for b in range(a + 1, len(FLOWS)):
                    fa, fb = FLOWS[a], FLOWS[b]
                    if vs[fa] in DEFINITE and vs[fb] in DEFINITE:
                        pair = agree.setdefault((fa, fb), [0, 0])
                        pair[0] += 1
                        if vs[fa] != vs[fb]:
                            pair[1] += 1
                            conflicts.append((n, k, f"{fa}: {vs[fa]}, {fb}: {vs[fb]}"))
            exp = cases[n].get("expect", {}).get(f"{cases[n]['top']}.{k}")
            if exp:
                for f in FLOWS:
                    if vs[f] in DEFINITE and vs[f] != exp:
                        conflicts.append((n, k, f"{f}: {vs[f]}, the test's recorded answer: {exp}"))
    w("| Pair | Properties both decide | Same verdict | Different |")
    w("| --- | ---: | ---: | ---: |")
    for (fa, fb), (both, diff) in agree.items():
        w(f"| {fa} and {fb} | {both} | {both - diff} | {diff} |")
    w("")
    if conflicts:
        w("Every difference, to be explained in `bench/compare/README.md` before it is quoted:\n")
        w("| Design | Property | Verdicts |")
        w("| --- | --- | --- |")
        for n, k, what in conflicts:
            w(f"| {n} | `{k}` | {what} |")
        w("")
    else:
        w("No property got two different definite verdicts.\n")

    w("## Designs a flow did not read\n")
    w("| Design | Flow | Configuration | First message |")
    w("| --- | --- | --- | --- |")
    for n in names:
        for flow in FLOWS:
            mine = [r for r in recs if r["case"] == n and r["flow"] == flow]
            if mine and not any(r["accepted"] for r in mine):
                r = mine[0]
                msg = scrub(r["refusal"] or "")
                w(f"| {n} | {flow} | {r['config']} | {msg} |")
    w("")

    w("## Per design\n")
    w("One cell per flow: definite / bounded / undetermined / refused, of the design's properties.\n")
    w("*In the source* is the number of assert and cover statements slang finds in the elaborated design "
      "(instances with the same body are counted once, so it can be below what a flow lists); "
      "`invalid` means slang, run to the letter of the standard, rejects the sources.\n")
    refs = {r["case"]: r for r in all_recs if r["flow"] == "reference"}
    w("| Design | In the source | Properties | " + " | ".join(FLOWS) + " |")
    w("| --- | ---: | ---: | " + " | ".join("---" for _ in FLOWS) + " |")
    for n in names:
        keys = set().union(*[set(table[n].get(f, {})) for f in FLOWS])
        cells = []
        for flow in FLOWS:
            mine = table[n].get(flow, {})
            if not mine:
                cells.append("not read")
                continue
            d = sum(1 for k in keys if mine.get(k, {}).get("verdict") in DEFINITE)
            b = sum(1 for k in keys if mine.get(k, {}).get("verdict") == "bounded")
            u = sum(1 for k in keys if mine.get(k, {}).get("verdict") in ("undetermined", "conflict"))
            cells.append(f"{d} / {b} / {u} / {len(keys) - d - b - u}")
        ref = refs.get(n)
        src = "" if ref is None else "invalid" if not ref["accepted"] else str(
            sum(v for k, v in ref.get("counts", {}).items() if not k.endswith("assume")))
        w(f"| {n} | {src} | {len(keys)} | " + " | ".join(cells) + " |")
    w("")

    timing = Path(args.timing) if args.timing else None
    if timing or meta.get("purpose") == "timing":
        import statistics
        tmeta, trecs = read_results(timing) if timing else (meta, recs)
        w("## Time\n")
        w(f"From `bench/compare/results/{(timing or results).name}`: run `{tmeta.get('id')}`, started {tmeta.get('started')}, "
          f"machine load at start {tmeta.get('load_at_start')} on {tmeta.get('cpus')} CPUs, InFormal `{tmeta.get('tools', {}).get('informal')}`, "
          f"the same limits and tools as above.\n")
        w("Seconds for one tool run on a design, all its properties: the median of the repeats, with the "
          "lowest and highest wall time. InFormal runs its engines in parallel on several cores; SymbiYosys "
          "and EBMC run one engine per configuration, so CPU seconds are given too. A run that reached the "
          "time limit is shown as such and was not repeated. SymbiYosys and EBMC need all of their listed "
          "configurations to give the verdicts of the tables above, so the last column adds them up; for "
          "InFormal it is the `default` run alone (`inhouse` is the same run without the external engines).\n")
        w("| Design | Flow | Configuration | Runs | Wall s (median) | Wall s (range) | CPU s (median) | Flow total, wall s |")
        w("| --- | --- | --- | ---: | ---: | --- | ---: | ---: |")
        for n in names:
            for flow in FLOWS:
                mine = [r for r in trecs if r["case"] == n and r["flow"] == flow and r["accepted"]]
                total = sum(statistics.median(r["walls"]) for r in mine
                            if not (flow == "informal" and r["config"] == "inhouse"))
                for k, r in enumerate(mine):
                    wall = statistics.median(r["walls"])
                    limit = " (time limit)" if r["timeout"] else ""
                    w(f"| {n} | {flow} | {r['config']} | {len(r['walls'])} | {wall:.2f}{limit} | "
                      f"{min(r['walls']):.2f} to {max(r['walls']):.2f} | {statistics.median(r['cpus']):.2f} | "
                      f"{total:.2f} |" if k == 0 else
                      f"| | | {r['config']} | {len(r['walls'])} | {wall:.2f}{limit} | "
                      f"{min(r['walls']):.2f} to {max(r['walls']):.2f} | {statistics.median(r['cpus']):.2f} | |")
        w("")
    dest = Path(args.out) if args.out else REPO / "docs/compare.md"
    dest.write_text("\n".join(out))
    print(f"compare: wrote {dest}")
    print("\n".join(out[: out.index("## Verdict agreement\n") if "## Verdict agreement\n" in out else 30]))
    if conflicts:
        print(f"compare: {len(conflicts)} differing verdicts")


def scrub(text: str) -> str:
    """No machine paths in a report."""
    return scrub_paths(text).replace("|", "\\|")


def scrub_paths(text: str) -> str:
    """No machine paths in a run file either: it is committed."""
    text = text.replace(str(WORK) + "/", "").replace(str(REPO) + "/", "").replace(str(TOOLS) + "/", "")
    # A tool that prints paths relative to its work directory leaves a run
    # of ../ and then the tail of one of the same directories.
    text = re.sub(r"(\.\./)+", "../", text)
    for base in (REPO, WORK, TOOLS):
        parts = base.parts[1:]
        for i in range(len(parts)):
            text = text.replace("../" + "/".join(parts[i:]) + "/", "")
    return text.replace(str(Path.home()), "~")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("tools").set_defaults(fn=cmd_tools)
    sub.add_parser("fetch").set_defaults(fn=cmd_fetch)
    p = sub.add_parser("list")
    p.add_argument("--only", action="append")
    p.set_defaults(fn=cmd_list)
    p = sub.add_parser("run")
    p.add_argument("--only", action="append", help="regular expression on the case name (repeatable)")
    p.add_argument("--flows", nargs="+", default=["reference", *FLOWS], choices=["reference", *FLOWS])
    p.add_argument("--configs", nargs="+")
    p.add_argument("--timeout", type=float, default=300, help="seconds per tool run")
    p.add_argument("--prop-timeout", type=float, default=60, help="seconds per property (InFormal)")
    p.add_argument("--informal-timeout", type=float, default=1800,
                   help="seconds for a whole InFormal run (all properties); only ends a hang")
    p.add_argument("--depth", type=int, default=20, help="bound for the bounded engines (SymbiYosys's default)")
    p.add_argument("--mem-gb", type=float, default=8, help="address-space cap for SymbiYosys and EBMC runs")
    p.add_argument("--repeat", type=int, default=1)
    p.add_argument("--purpose", choices=("verdicts", "timing"), default="verdicts")
    p.add_argument("--id")
    p.add_argument("--out")
    p.add_argument("--skip-done", action="store_true", help="leave out what the run file already has (to resume a run)")
    p.set_defaults(fn=cmd_run)
    p = sub.add_parser("report")
    p.add_argument("--results")
    p.add_argument("--timing", help="a run file made with --purpose timing, for the time table")
    p.add_argument("--out")
    p.set_defaults(fn=cmd_report)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
