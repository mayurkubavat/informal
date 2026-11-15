"""Check InFormal's certificates with programs that share no code with it.

usage: python3 -I scripts/certificate_check.py <dir>... [--expect KINDS]
           [--report <file.json>] [--timeout S] [--quiet]
       python3 -I scripts/certificate_check.py --models tests/models
           [--informal target/debug/informal] [--out <dir>] [--expect KINDS]

The first form checks every `<model>.cert.json` in the directories given
(what `informal check --certificate <dir>` wrote; a model path in a summary
is read from the current directory):

- a counterexample (`<model>.trace`, AIGER witness format) is simulated by
  aigsim, which must say that the trace reaches the bad output;
- a proof (`<model>.cert.aig`, a witness circuit) goes to Certifaiger,
  which writes its conditions (reset, transition, safety, base, inductive)
  as circuits; each becomes a CNF with aigtocnf, and Kissat must find
  every one of them unsatisfiable.

A verdict whose summary names no file is counted as not checkable outside,
with the summary's reason; it is not a failure. A check that runs out of
time (--timeout, per checker call) is counted on its own, as not decided:
it says nothing about the certificate, and it does not fail the run unless
--strict is given.

The second form is the gate's step: it runs every in-house engine that
proves on each AIGER model in the directory, and BMC on those named
`*_unsafe`, with certificates, and checks them all. `--expect invariant,k-induction,two-step,trace` (the
gate's setting) fails the run when a kind was never confirmed, so the step
cannot pass by checking nothing.

Tools: Certifaiger (github.com/Froleyks/certifaiger, MIT), the AIGER tools
aigsim, aigsplit and aigtocnf (github.com/arminbiere/aiger, MIT), and
Kissat (github.com/arminbiere/kissat, MIT), the SAT solver Certifaiger's
own scripts use, found in $INFORMAL_TOOLS_DIR (default ~/Silicon/DV/tools)
as certifaiger/bin/certifaiger, aiger/<tool> and kissat/build/kissat, or by
the variables CERTIFAIGER, AIGSIM, AIGSPLIT, AIGTOCNF and KISSAT.

Exit status: 0 every certificate checked is valid (and every expected kind
was seen), 1 one is not, 2 not checked (a tool is missing). The --models
form is strict.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import tempfile

ENGINES = ["informal-bmc", "informal-kind", "informal-ic3", "informal-kind-ic3", "informal-ic3-mt"]


def tools():
    """{name: path}, and the names that are missing."""
    base = os.environ.get("INFORMAL_TOOLS_DIR") or os.path.expanduser("~/Silicon/DV/tools")
    places = {
        "certifaiger": "certifaiger/bin/certifaiger",
        "aigsim": "aiger/aigsim",
        "aigsplit": "aiger/aigsplit",
        "aigtocnf": "aiger/aigtocnf",
        "kissat": "kissat/build/kissat",
    }
    found, missing = {}, []
    for name, rel in places.items():
        p = os.environ.get(name.upper()) or os.path.join(base, rel)
        if os.path.isfile(p) and os.access(p, os.X_OK):
            found[name] = p
        else:
            missing.append(f"{name} ({p})")
    return found, missing


def run(cmd, timeout, cwd=None):
    """(exit status or None on a timeout, stdout + stderr)."""
    try:
        p = subprocess.run(cmd, cwd=cwd, timeout=timeout, capture_output=True, text=True, errors="replace")
    except subprocess.TimeoutExpired:
        return None, "timed out"
    return p.returncode, p.stdout + p.stderr


def check_trace(t, model, trace, timeout):
    """None when aigsim confirms the trace, else what went wrong."""
    with open(model, "rb") as f:
        head = f.readline().split()
    # AIGER 1.0 models give the property as an output: aigsim -m reads it as bad.
    old_style = len(head) < 7 or int(head[6]) == 0
    code, out = run([t["aigsim"]] + (["-m"] if old_style else []) + ["-w", "-2", model, trace], timeout)
    if code is None:
        return "timeout: aigsim"
    if code != 0 or not re.search(r"Trace is a witness for: \{ b[0-9]+( |\})", out):
        return "aigsim does not confirm the trace: " + (out.strip().splitlines() or ["no output"])[-1]
    return None


def check_circuit(t, model, witness, timeout):
    """None when every condition of Certifaiger is unsatisfiable, else what went wrong."""
    with tempfile.TemporaryDirectory(prefix="informal-cert-") as tmp:
        code, out = run([t["certifaiger"], os.path.abspath(model), os.path.abspath(witness), "check.aig"], timeout, tmp)
        if code is None:
            return "timeout: certifaiger"
        if code != 0:
            return "certifaiger refuses the witness circuit: " + (out.strip().splitlines() or ["no output"])[-1]
        code, out = run([t["aigsplit"], "-n", "check.aig", "split_"], timeout, tmp)
        if code != 0:
            return "aigsplit failed: " + out.strip()
        parts = sorted(f for f in os.listdir(tmp) if f.endswith(".aig") and f != "check.aig")
        if not parts:
            return "certifaiger wrote no conditions"
        for part in parts:
            code, out = run([t["aigtocnf"], part, part + ".cnf"], timeout, tmp)
            if code != 0:
                return f"aigtocnf failed on {part}: {out.strip()}"
            code, out = run([t["kissat"], "-q", "--unsat", part + ".cnf"], timeout, tmp)
            name = part[: -len(".aig")]
            if code is None:
                return f"timeout: the SAT solver on condition {name}"
            if code != 20:
                return f"condition {name} does not hold (SAT solver exit {code})"
    return None


def check_dir(t, d, timeout, log):
    """One record per summary in `d`: (summary, 'valid' | 'invalid' | 'timeout' | 'none', detail)."""
    out = []
    for name in sorted(os.listdir(d)):
        if not name.endswith(".cert.json"):
            continue
        with open(os.path.join(d, name)) as f:
            s = json.load(f)
        if not s.get("file"):
            out.append((s, "none", s.get("note", "no reason given")))
            continue
        model = os.path.join(d, s["bit_level_model"]) if s.get("bit_level_model") else s["model"]
        cert = os.path.join(d, s["file"])
        if not os.path.isfile(model) or not os.path.isfile(cert):
            out.append((s, "invalid", f"missing file: {model if not os.path.isfile(model) else cert}"))
            continue
        if s.get("checker") == "aigsim" and s["verdict"] == "unsafe":
            err = check_trace(t, model, cert, timeout)
        elif s.get("checker") == "certifaiger" and s["verdict"] == "safe":
            err = check_circuit(t, model, cert, timeout)
        else:
            err = f"checker {s.get('checker')} does not fit verdict {s['verdict']}"
        state = "valid" if not err else "timeout" if err.startswith("timeout: ") else "invalid"
        out.append((s, state, err or ""))
        log(f"  {'INVALID' if state == 'invalid' else state:<8} {s['verdict']:<7} {s['evidence']:<12} {s['engine']:<18} {s['model']}"
            + (f": {err}" if err else ""))
    return out


def generate(informal, models, out, timeout, log):
    """Run every engine on every AIGER model of `models`, certificates into `out`/<engine>."""
    dirs = []
    files = sorted(f for f in os.listdir(models) if f.endswith((".aag", ".aig")))
    for e in ENGINES:
        d = os.path.join(out, e)
        dirs.append(d)
        for f in files:
            # BMC never proves: on a safe model it would only use up its time.
            if e == "informal-bmc" and "_unsafe." not in f:
                continue
            p = subprocess.run(
                [informal, "check", os.path.join(models, f), "--only", e, "--timeout", str(timeout), "--certificate", d],
                capture_output=True, text=True)
            # 10 unsafe, 20 safe: these models are small, so no verdict is an error too.
            if p.returncode not in (10, 20):
                log(f"  informal check failed on {f} with {e} (exit {p.returncode}): {p.stderr.strip()[-300:]}")
                return None
    return [d for d in dirs if os.path.isdir(d)]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("dirs", nargs="*")
    ap.add_argument("--models")
    ap.add_argument("--informal", default="target/debug/informal")
    ap.add_argument("--out")
    ap.add_argument("--expect", default="")
    ap.add_argument("--report")
    ap.add_argument("--timeout", type=float, default=600.0, help="seconds per checker call")
    ap.add_argument("--strict", action="store_true", help="a check that ran out of time fails the run")
    ap.add_argument("--quiet", action="store_true")
    a = ap.parse_args()
    if bool(a.dirs) == bool(a.models):
        ap.error("give certificate directories, or --models <dir>")
    log = (lambda _m: None) if a.quiet else (lambda m: print(m, flush=True))
    t, missing = tools()
    if missing:
        print("certificate_check: not checked, missing " + ", ".join(missing))
        return 2
    keep = None
    dirs = a.dirs
    if a.models:
        if not os.path.isfile(a.informal):
            print(f"certificate_check: no {a.informal}")
            return 2
        if a.out:
            os.makedirs(a.out, exist_ok=True)
            out = a.out
        else:
            keep = tempfile.TemporaryDirectory(prefix="informal-cert-")
            out = keep.name
        dirs = generate(a.informal, a.models, out, 60, print)
        a.strict = True
        if dirs is None:
            return 1
    records = []
    for d in dirs:
        records += check_dir(t, d, a.timeout, log)
    valid = [r for r in records if r[1] == "valid"]
    invalid = [r for r in records if r[1] == "invalid"]
    none = [r for r in records if r[1] == "none"]
    late = [r for r in records if r[1] == "timeout"]
    by = {}
    for s, state, _ in records:
        k = (s["verdict"], s["evidence"])
        by.setdefault(k, {"valid": 0, "invalid": 0, "timeout": 0, "none": 0})[state] += 1
    print(f"certificates: {len(records)} verdicts, {len(valid)} confirmed outside, "
          f"{len(none)} not checkable outside, {len(late)} not decided in time, {len(invalid)} INVALID")
    for (verdict, evidence), c in sorted(by.items()):
        print(f"  {verdict:<7} {evidence:<12} confirmed {c['valid']:>4}  not checkable {c['none']:>4}"
              f"  out of time {c['timeout']:>4}  invalid {c['invalid']:>4}")
    for s, _, why in late:
        print(f"  out of time: {s['model']} ({s['verdict']} by {s['engine']}): {why}")
    reasons = {}
    for _, _, why in none:
        reasons[why] = reasons.get(why, 0) + 1
    for why, n in sorted(reasons.items(), key=lambda x: -x[1]):
        print(f"  not checkable ({n}): {why}")
    for s, _, why in invalid:
        print(f"  INVALID: {s['model']} ({s['verdict']} by {s['engine']}): {why}")
    if a.report:
        with open(a.report, "w") as f:
            json.dump([{"model": s["model"], "verdict": s["verdict"], "engine": s["engine"],
                        "evidence": s["evidence"], "result": state, "detail": why}
                       for s, state, why in records], f, indent=1)
            f.write("\n")
    seen = {s["evidence"] for s, _, _ in valid}
    lacking = [k for k in a.expect.split(",") if k and k not in seen]
    if lacking:
        print("certificate_check: no confirmed certificate of kind: " + ", ".join(lacking))
    if keep:
        keep.cleanup()
    return 1 if invalid or lacking or (a.strict and late) else 0


if __name__ == "__main__":
    sys.exit(main())
