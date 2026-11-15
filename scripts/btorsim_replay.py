"""Replay an InFormal counterexample with BtorSim, a simulator that shares no
code with InFormal.

usage: python3 -I scripts/btorsim_replay.py <model.btor2> [<witness>]
           [--btorsim <path>] [--bad <k>] [--keep <file>]
       python3 -I scripts/btorsim_replay.py --designs tests/sv --models <dir>
           [--informal target/debug/informal]

The second form is the gate's step: for every property of tests/sv that is
expected to fail or to be covered, it takes the model the test kept in
<dir> (INFORMAL_KEEP_MODELS), has informal-bmc find the trace again and
replays it. It fails if a trace is not confirmed, a model is missing, or a
trace could not be checked for another reason than being a liveness trace
(those end in a loop, which the Btor2 witness of a bad state cannot say).

<witness> is the named witness InFormal prints (`informalw 1`: free initial
register values, then per-step input values, by bit name); read from the
standard input when left out. It is rewritten in the Btor2 witness format
and given to `btorsim -c`, which simulates the model and says whether the
bad state is reached. InFormal replays its counterexamples itself; this is
the same check by an independent reader and simulator of the model.
The model is first rewritten with every initial value defined before its
register, which the reference parser insists on and InFormal's models do
not do; nothing else about it changes.

BtorSim: github.com/Boolector/btor2tools (MIT), found as --btorsim, as
$BTORSIM, or in $INFORMAL_TOOLS_DIR/btor2tools/build/bin
(default ~/Silicon/DV/tools).

Exit status: 0 BtorSim reaches the bad state, 1 it does not, 2 not checked
(no BtorSim, a model with arrays, or a witness that cannot be read).
"""
import argparse
import os
import re
import subprocess
import sys
import tempfile


def variables(model):
    """(states, inputs): [(name, width, has_init)] in declaration order."""
    sorts, states, inputs, inits = {}, [], [], set()
    for raw in open(model):
        t = raw.split(";")[0].split()
        if len(t) < 2:
            continue
        nid, op = t[0], t[1]
        if op == "sort":
            sorts[nid] = int(t[3]) if t[2] == "bitvec" else None
        elif op == "state":
            states.append((t[3] if len(t) > 3 else f"s{nid}", sorts[t[2]], nid))
        elif op == "input":
            inputs.append((t[3] if len(t) > 3 else f"i{nid}", sorts[t[2]], nid))
        elif op == "init":
            inits.add(t[3])
    if any(w is None for _, w, _ in states + inputs):
        return None
    return [(n, w, nid in inits) for n, w, nid in states], [(n, w, False) for n, w, _ in inputs]


CONST = {"const", "constd", "consth", "zero", "one", "ones"}
TRAILING_INTS = {"slice": 2, "uext": 1, "sext": 1}


def node_args(op, rest):
    """Positions in `rest` that hold node ids (not sorts, numbers or the symbol)."""
    if op == "sort" or op in CONST:
        return []
    if op in ("input", "state"):
        return []
    if op in ("bad", "constraint", "fair", "output"):
        return [0]
    if op == "justice":
        return list(range(1, 1 + int(rest[0])))
    args = rest[1:]
    while args and not args[-1].lstrip("-").isdigit():
        args = args[:-1]
    return list(range(1, 1 + len(args) - TRAILING_INTS.get(op, 0)))


def strict_order(model, out):
    """Write the model so that every initial value is defined before its
    register, as the reference Btor2 parser requires (InFormal's models
    declare registers first). Sorts go first, then the constants the
    initial values are made of, then everything else in its order, so
    registers and inputs keep their positions among themselves. False when
    an initial value is not a constant expression."""
    lines = []
    for raw in open(model):
        t = raw.split(";")[0].split()
        if len(t) >= 2:
            lines.append((int(t[0]), t[1], t[2:]))
    by_id = {nid: (op, rest) for nid, op, rest in lines}
    cone, todo = set(), [int(rest[2]) for _, op, rest in lines if op == "init"]
    while todo:
        n = abs(todo.pop())
        if n in cone:
            continue
        op, rest = by_id[n]
        if op in ("state", "input"):
            return False
        cone.add(n)
        todo += [int(rest[i]) for i in node_args(op, rest)]
    order = [l for l in lines if l[1] == "sort"] + [l for l in lines if l[0] in cone] + \
        [l for l in lines if l[1] != "sort" and l[0] not in cone]
    new = {nid: k + 1 for k, (nid, _, _) in enumerate(order)}
    ref = lambda tok: str(new[abs(int(tok))] * (-1 if int(tok) < 0 else 1))
    with open(out, "w") as f:
        for nid, op, rest in order:
            rest = list(rest)
            if op == "sort":
                if rest[0] == "array":
                    rest[1:3] = [ref(rest[1]), ref(rest[2])]
            else:
                if op not in ("bad", "constraint", "fair", "output", "justice"):
                    rest[0] = ref(rest[0])
                for i in node_args(op, rest):
                    rest[i] = ref(rest[i])
            f.write(f"{new[nid]} {op} {' '.join(rest)}\n")
    return True


def parse(text):
    """(depth, initial values, per-frame values), values as {bit name: '0' | '1'}."""
    lines = [l.strip() for l in text.splitlines()]
    start = lines.index("informalw 1")
    depth = int(lines[start + 1].split()[1])
    pairs = lambda words: dict(w.split("=", 1) for w in words)
    init = pairs(lines[start + 2].split()[1:])
    frames = []
    for l in lines[start + 3:start + 4 + depth]:
        w = l.split()
        if not w or w[0] != f"@{len(frames)}":
            raise ValueError(f"frame {len(frames)} is missing")
        frames.append(pairs(w[1:]))
    return depth, init, frames


def value(name, width, bits):
    """The variable's value, most significant bit first; unset bits are 0."""
    if width == 1:
        return bits.get(name, bits.get(f"{name}[0]", "0"))
    return "".join(bits.get(f"{name}[{i}]", "0") for i in reversed(range(width)))


def btor2_witness(states, inputs, init, frames, bad):
    out = ["sat", f"b{bad}", "#0"]
    out += [f"{i} {value(n, w, init)} {n}#0" for i, (n, w, has_init) in enumerate(states) if not has_init]
    for k, frame in enumerate(frames):
        out.append(f"@{k}")
        out += [f"{i} {value(n, w, frame)} {n}@{k}" for i, (n, w, _) in enumerate(inputs)]
    return "\n".join(out + ["."]) + "\n"


def find_btorsim(arg):
    tools = os.environ.get("INFORMAL_TOOLS_DIR", os.path.expanduser("~/Silicon/DV/tools"))
    for p in (arg, os.environ.get("BTORSIM"), os.path.join(tools, "btor2tools/build/bin/btorsim")):
        if p and os.access(p, os.X_OK):
            return p
    return None


def replay(model, text, sim, bad=0, keep=None):
    """(exit status, one line)."""
    v = variables(model)
    if v is None:
        return 2, "not checked: the model has array variables"
    try:
        depth, init, frames = parse(text)
    except (ValueError, IndexError):
        return 2, "not checked: no readable `informalw 1` witness"
    known = {n for n, _, _ in v[0] + v[1]}
    names = {re.sub(r"\[\d+\]$", "", k) for bits in [init] + frames for k in bits}
    if not names <= known:
        return 2, f"not checked: the witness names variables the model does not have: {sorted(names - known)[:5]}"
    witness = btor2_witness(v[0], v[1], init, frames, bad)
    if keep:
        open(keep, "w").write(witness)
    with tempfile.TemporaryDirectory() as tmp:
        strict = os.path.join(tmp, "model.btor2")
        if not strict_order(model, strict):
            return 2, "not checked: an initial value in the model is not a constant"
        wit = os.path.join(tmp, "witness")
        open(wit, "w").write(witness)
        r = subprocess.run([sim, "-c", strict, wit], capture_output=True, text=True)
    if r.returncode == 0:
        return 0, f"confirmed: BtorSim reaches b{bad} at step {depth}"
    return 1, f"NOT confirmed by BtorSim (exit {r.returncode}): {(r.stdout + r.stderr).strip().splitlines()[-1:]}"


def designs(sv_dir, models, informal, sim):
    confirmed, liveness, wrong = 0, 0, []
    for name in sorted(os.listdir(sv_dir)):
        if not name.endswith(".sv"):
            continue
        design = name[:-3]
        for line in open(os.path.join(sv_dir, name)):
            w = line.split()
            if len(w) != 3 or w[0] != "//=" or w[2] not in ("cex", "covered"):
                continue
            model = os.path.join(models, f"{design}.{w[1]}.btor2")
            if not os.path.isfile(model):
                wrong.append(f"{design} {w[1]}: no model in {models}")
                continue
            if any(l.split()[1:2] == ["justice"] for l in open(model)):
                liveness += 1
                continue
            out = subprocess.run([informal, "check", model, "--only", "informal-bmc", "--timeout", "60"],
                                 capture_output=True, text=True)
            status, text = replay(model, out.stdout + out.stderr, sim)
            if status == 0:
                confirmed += 1
            else:
                wrong.append(f"{design} {w[1]}: {text}")
    for l in wrong:
        print(l)
    print(f"witness replay: {confirmed} traces confirmed by BtorSim, {liveness} liveness traces not replayed, "
          f"{len(wrong)} wrong")
    return 1 if wrong or confirmed == 0 else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("model", nargs="?")
    ap.add_argument("--designs")
    ap.add_argument("--models")
    ap.add_argument("--informal", default="target/debug/informal")
    ap.add_argument("witness", nargs="?")
    ap.add_argument("--btorsim")
    ap.add_argument("--bad", type=int, default=0)
    ap.add_argument("--keep", help="also write the Btor2 witness here")
    args = ap.parse_args()
    sim = find_btorsim(args.btorsim)
    if not sim:
        print("not checked: no btorsim (see this script's header)")
        return 2
    if args.designs:
        return designs(args.designs, args.models, args.informal, sim)
    text = open(args.witness).read() if args.witness else sys.stdin.read()
    status, line = replay(args.model, text, sim, args.bad, args.keep)
    print(line)
    return status


if __name__ == "__main__":
    sys.exit(main())
