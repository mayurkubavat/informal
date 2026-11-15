"""Are two Btor2 models of the same design the same model?

usage: python3 -I scripts/btor2_equiv.py <old.btor2> <new.btor2> [-o miter.btor2]
                                         [--informal target/debug/informal]

For a lowering change that is meant to keep behaviour (a new svlang pin, a
refactor): tests/sv/lowering.lock shows that a model changed, this shows
whether the change matters. It writes a miter: the inputs and the registers
of both models, matched by name, become shared free inputs, and its one
`bad` is true when, for some value of them, the two models disagree on any
next-state function, initial value, constraint, check, justice or fairness
term. No disagreement means the two models have the same transition system,
so every property has the same answer in both.

The check is on the functions, for all register values, reachable or not:
"same" is a proof, "differs" can also mean the models agree only on
reachable states.

Exit status: 0 the same (proved), 1 differs (a witness exists), 2 could not
be decided (interfaces differ, or the engines gave no answer).
"""
import argparse
import subprocess
import sys

DECL = {"sort", "input", "state", "init", "next", "bad", "constraint", "output", "justice", "fair"}
CONST = {"const", "constd", "consth", "zero", "one", "ones"}
# Operators whose last operands are plain numbers, not node ids.
TRAILING_INTS = {"slice": 2, "uext": 1, "sext": 1}


class Model:
    def __init__(self, path):
        self.sorts = {}     # id -> ("bitvec", w) | ("array", index sort, element sort), structural
        self.vars = {}      # symbol -> (id, sort id, "input" | "state")
        self.lines = []     # (id, op, fields after op, symbol)
        self.init, self.next = {}, {}   # state id -> value node
        self.bad, self.constraint, self.fair, self.justice = [], [], [], []
        anonymous = 0
        for raw in open(path):
            text = raw.split(";")[0].split()
            if not text:
                continue
            nid, op, rest = int(text[0]), text[1], text[2:]
            symbol = None
            if op == "sort":
                self.sorts[nid] = ("bitvec", int(rest[1])) if rest[0] == "bitvec" else \
                    ("array", self.sorts[int(rest[1])], self.sorts[int(rest[2])])
            elif op in ("input", "state"):
                if len(rest) > 1:
                    symbol = rest[1]
                else:
                    anonymous += 1
                    symbol = f"<unnamed {op} {anonymous}>"
                if symbol in self.vars:
                    raise Undecided(f"{path}: two variables named {symbol}")
                self.vars[symbol] = (nid, int(rest[0]), op)
            elif op == "init":
                self.init[int(rest[1])] = int(rest[2])
            elif op == "next":
                self.next[int(rest[1])] = int(rest[2])
            elif op in ("bad", "constraint", "fair"):
                getattr(self, op).append(int(rest[0]))
            elif op == "justice":
                self.justice.append([int(x) for x in rest[1:1 + int(rest[0])]])
            self.lines.append((nid, op, rest))
        self.top = max(l[0] for l in self.lines)


class Undecided(Exception):
    pass


def miter(a, b):
    """The miter's Btor2 text."""
    out = []
    # Model A as it is, registers as free inputs, without its declarations.
    for nid, op, rest in a.lines:
        if op == "state":
            out.append(f"{nid} input {' '.join(rest)}")
        elif op not in DECL or op in ("sort", "input"):
            out.append(f"{nid} {op} {' '.join(rest)}")
    # Model B after it, its variables replaced by A's of the same name.
    if set(a.vars) != set(b.vars):
        only = sorted(set(a.vars) ^ set(b.vars))
        raise Undecided(f"the models do not have the same inputs and registers: {', '.join(only[:8])}")
    remap = {}
    for symbol, (bid, bsort, bkind) in b.vars.items():
        aid, asort, akind = a.vars[symbol]
        if akind != bkind or a.sorts[asort] != b.sorts[bsort]:
            raise Undecided(f"{symbol} is not the same kind or width in both models")
        remap[bid] = aid
    off = a.top

    def ref(tok):
        n = int(tok)
        m = remap.get(abs(n), abs(n) + off)
        return str(-m if n < 0 else m)

    for nid, op, rest in b.lines:
        if op in ("input", "state") or (op in DECL and op != "sort"):
            continue
        if op == "sort":
            body = rest[:1] + ([rest[1]] if rest[0] == "bitvec" else [ref(rest[1]), ref(rest[2])])
        elif op in CONST:
            body = [ref(rest[0])] + rest[1:]
        else:
            ints = TRAILING_INTS.get(op, 0)
            args = rest[1:]
            # A trailing symbol is a token that is not a number.
            while args and not args[-1].lstrip("-").isdigit():
                args = args[:-1]
            keep = len(args) - ints
            body = [ref(rest[0])] + [ref(x) for x in args[:keep]] + args[keep:]
        out.append(f"{nid + off} {op} {' '.join(body)}")

    nxt = [b.top + off]

    def emit(text):
        nxt[0] += 1
        out.append(f"{nxt[0]} {text}")
        return nxt[0]

    bit = emit("sort bitvec 1")
    diffs = []

    def differ(x, y):
        diffs.append(emit(f"neq {bit} {x} {y}"))

    def conj(ids):
        if not ids:
            return emit(f"one {bit}")
        acc = ids[0]
        for i in ids[1:]:
            acc = emit(f"and {bit} {acc} {i}")
        return acc

    for symbol, (aid, _, kind) in sorted(a.vars.items()):
        if kind != "state":
            continue
        bid = b.vars[symbol][0]
        for what, fa, fb in (("init", a.init, b.init), ("next", a.next, b.next)):
            if (aid in fa) != (bid in fb):
                raise Undecided(f"{symbol} has a {what} in one model only")
            if aid in fa:
                differ(fa[aid], int(ref(str(fb[bid]))))
    bref = lambda ids: [int(ref(str(i))) for i in ids]
    if len(a.bad) != len(b.bad) or len(a.fair) != len(b.fair) or \
            [len(j) for j in a.justice] != [len(j) for j in b.justice]:
        raise Undecided("the models do not have the same number of checks")
    for x, y in zip(a.bad, bref(b.bad)):
        differ(x, y)
    # Constraints and fairness terms count as a set: compare their conjunction.
    differ(conj(a.constraint), conj(bref(b.constraint)))
    differ(conj(a.fair), conj(bref(b.fair)))
    for ja, jb in zip(a.justice, b.justice):
        for x, y in zip(ja, bref(jb)):
            differ(x, y)
    acc = diffs[0]
    for d in diffs[1:]:
        acc = emit(f"or {bit} {acc} {d}")
    emit(f"bad {acc} models_differ")
    return "\n".join(out) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("old")
    ap.add_argument("new")
    ap.add_argument("-o", "--out", default=None, help="where to write the miter (default: <new>.miter.btor2)")
    ap.add_argument("--informal", default="target/debug/informal")
    ap.add_argument("--timeout", default="60")
    args = ap.parse_args()
    out = args.out or args.new + ".miter.btor2"
    try:
        text = miter(Model(args.old), Model(args.new))
    except Undecided as e:
        print(f"undecided: {e}")
        return 2
    open(out, "w").write(text)
    r = subprocess.run([args.informal, "check", out, "--timeout", args.timeout],
                       capture_output=True, text=True)
    verdict = (r.stdout + r.stderr).lower()
    if r.returncode == 20:
        print("same: no register or input value makes the two models disagree")
        return 0
    if r.returncode == 10:
        print(f"differs: a witness exists (run `{args.informal} check {out}`)")
        return 1
    print(f"undecided: the engines gave no answer (exit {r.returncode}): {verdict.strip().splitlines()[-1] if verdict.strip() else ''}")
    return 2


if __name__ == "__main__":
    sys.exit(main())
