#!/usr/bin/env python3
"""Offline trainer & 5-fold cross-validator for InFormal's Phase 3 learned selector.

Reads `informal bench --each` JSONL records across bit-level AIGER and
word-level Btor2 (BV + array) corpora, fits an L2-regularized Ridge regression
utility scorer over 10 scale-invariant structural features (pure Python stdlib;
solves the 10x10 normal equations directly), reports 5-fold out-of-fold
portfolio PAR-2 against the Virtual Best Solver (VBS) overall and per format,
and optionally writes the JSON model for `INFORMAL_SELECTOR_MODEL` or embedding
in `crates/informal-engine/src/learned.rs`.
"""

from __future__ import annotations

import argparse
import json
import math
from collections import defaultdict
from pathlib import Path
from typing import Dict, List, Sequence, Set, Tuple

FEATURE_NAMES = [
    "bias",
    "log_states",
    "log_state_bits",
    "log_nodes",
    "log_inputs",
    "log_constraints",
    "and_per_latch",
    "input_per_latch",
    "arith_density",
    "log_width",
]

# Safety engine metadata mirroring `Capabilities` for format-aware conditional
# training and complementarity-aware portfolio evaluation.
# name: (domain, kind, sub_bucket, proves, finds_bugs, slots, max_state_bits, formats, arrays)
EngineMeta = Tuple[str, str, int, bool, bool, int, int | None, Set[str], bool]

ENGINE_META: Dict[str, EngineMeta] = {
    "informal-ic3-mt": ("bit", "ic3", 0, True, True, 4, None, {"aiger", "btor2"}, False),
    "informal-ic3": ("bit", "ic3", 0, True, True, 1, None, {"aiger", "btor2"}, False),
    "informal-ic3-cadical": ("bit", "ic3", 0, True, True, 1, None, {"aiger", "btor2"}, False),
    "informal-ic3-inn": ("bit", "ic3", 0, True, True, 1, None, {"aiger", "btor2"}, False),
    "informal-bmc": ("bit", "bmc", 0, False, True, 1, None, {"aiger", "btor2"}, False),
    "informal-bmc-10": ("bit", "bmc", 1, False, True, 1, None, {"aiger", "btor2"}, False),
    "informal-bmc-65": ("bit", "bmc", 1, False, True, 1, None, {"aiger", "btor2"}, False),
    "informal-kind": ("bit", "kind", 0, True, True, 1, None, {"aiger", "btor2"}, False),
    "informal-gla": ("bit", "hybrid", 0, True, True, 1, None, {"aiger", "btor2"}, False),
    "informal-kind-ic3": ("bit", "hybrid", 1, True, True, 1, None, {"aiger", "btor2"}, False),
    "informal-car": ("bit", "car", 0, True, True, 1, None, {"aiger", "btor2"}, False),
    "informal-bcar": ("bit", "car", 0, True, True, 1, None, {"aiger", "btor2"}, False),
    "informal-imc": ("bit", "imc", 0, True, True, 1, None, {"aiger", "btor2"}, False),
    "informal-bdd": ("bit", "bdd", 0, True, True, 1, 200, {"aiger", "btor2"}, False),
    "informal-swarm": ("bit", "swarm", 0, False, True, 1, None, {"aiger", "btor2"}, False),
    "informal-wbmc": ("word", "bmc", 0, False, True, 1, None, {"btor2"}, True),
    "informal-wkind": ("word", "kind", 0, True, True, 1, None, {"btor2"}, True),
    "informal-ic3sa": ("word", "ic3", 0, True, True, 1, None, {"btor2"}, True),
}


def raw_features(f: dict) -> List[float]:
    states = float(f.get("states", 0))
    state_bits = float(f.get("state_bits", 0) or states)
    nodes = float(f.get("nodes", 0))
    inputs = float(f.get("inputs", 0))
    constraints = float(f.get("constraints", 0))
    arith_ops = float(f.get("arith_ops", 0))
    max_width = float(max(1, f.get("max_width", 1)))
    return [
        1.0,
        math.log1p(states),
        math.log1p(state_bits),
        math.log1p(nodes),
        math.log1p(inputs),
        math.log1p(constraints),
        math.log1p(nodes / max(1.0, states)),
        math.log1p(inputs / max(1.0, states)),
        arith_ops / max(1.0, nodes),
        math.log1p(max_width),
    ]


def utility(verdict: str, wall_ms: float, timeout_ms: float) -> float:
    if verdict not in ("safe", "unsafe"):
        return 0.0
    t = max(0.0, min(wall_ms, timeout_ms))
    speed = 1.0 - math.log1p(t) / max(1.0, math.log1p(timeout_ms))
    return 0.5 + 0.5 * speed


def solve_linear(a: List[List[float]], b: List[float]) -> List[float]:
    """Gaussian elimination with partial pivoting for small dense systems."""
    n = len(b)
    m = [row[:] + [b[i]] for i, row in enumerate(a)]
    for col in range(n):
        piv = max(range(col, n), key=lambda r: abs(m[r][col]))
        m[col], m[piv] = m[piv], m[col]
        diag = m[col][col]
        if abs(diag) < 1e-12:
            continue
        inv = 1.0 / diag
        for j in range(col, n + 1):
            m[col][j] *= inv
        for r in range(n):
            if r != col and m[r][col] != 0.0:
                fac = m[r][col]
                for j in range(col, n + 1):
                    m[r][j] -= fac * m[col][j]
    return [m[i][n] for i in range(n)]


def standardize_fit(xs: Sequence[Sequence[float]]) -> Tuple[List[float], List[float]]:
    d = len(xs[0])
    n = max(1, len(xs))
    means = [0.0] + [sum(x[j] for x in xs) / n for j in range(1, d)]
    stds = [1.0]
    for j in range(1, d):
        var = sum((x[j] - means[j]) ** 2 for x in xs) / n
        s = math.sqrt(var)
        stds.append(s if s > 1e-9 else 1.0)
    return means, stds


def standardize_apply(x: Sequence[float], means: Sequence[float], stds: Sequence[float]) -> List[float]:
    return [1.0] + [(x[j] - means[j]) / stds[j] for j in range(1, len(x))]


def fit_ridge(xs: Sequence[Sequence[float]], ys: Sequence[float], lam: float) -> List[float]:
    d = len(xs[0])
    xtx = [[0.0] * d for _ in range(d)]
    xty = [0.0] * d
    for x, y in zip(xs, ys):
        for i in range(d):
            xi = x[i]
            xty[i] += xi * y
            row = xtx[i]
            for j in range(d):
                row[j] += xi * x[j]
    # Do not regularize bias term (index 0).
    for i in range(1, d):
        xtx[i][i] += lam
    return solve_linear(xtx, xty)


def dot(a: Sequence[float], b: Sequence[float]) -> float:
    return sum(x * y for x, y in zip(a, b))


def is_eligible(engine: str, fmt: str, f: dict) -> bool:
    meta = ENGINE_META.get(engine)
    if meta is None:
        return False
    _, _, _, _, _, _, max_bits, formats, arrays = meta
    if fmt not in formats:
        return False
    has_arr = bool(f.get("has_arrays")) or f.get("array_states", 0) > 0
    if has_arr and not arrays:
        return False
    state_bits = f.get("state_bits", 0) or f.get("states", 0)
    if max_bits is not None and state_bits > max_bits:
        return False
    return True


def size_aware_slot_cap(base_cap: int, f: dict) -> int:
    """Mirror `learned::size_aware_slot_cap` at `Effort::Standard`."""
    nodes = f.get("nodes", 0)
    state_bits = f.get("state_bits", 0) or f.get("states", 0)
    if nodes >= 250_000 or state_bits >= 20_000:
        size_cap = 6
    elif nodes >= 50_000 or state_bits >= 2_000:
        size_cap = 8
    else:
        size_cap = 10
    return min(base_cap, size_cap) if base_cap > 0 else 0


def select_portfolio(
    scores: Dict[str, float],
    fmt: str,
    f: dict,
    slot_cap: int,
    redundancy_penalty: float = 0.28,
) -> List[str]:
    """Complementarity-aware greedy selection under a thread-slot budget.

    Mirrors `learned::SelectorModel::rank_and_partition` using
    `(domain, kind, sub_bucket)` diversity buckets so bit-level vs word-level,
    shallow vs jump BMC, and CEGAR GLA vs k-ind+IC3 do not suppress each other.
    """
    has_arr = bool(f.get("has_arrays")) or f.get("array_states", 0) > 0
    candidates = [e for e in scores if is_eligible(e, fmt, f)]
    if has_arr:
        return sorted(candidates, key=lambda e: (-scores[e], e))

    # Mirror `plan_sequential_safety` deep-step shedding (`bmc-65` on tiny/arith-heavy).
    arith_heavy = f.get("arith_ops", 0) >= 8 and f.get("max_width", 1) >= 16
    tiny_state = (f.get("state_bits", 0) or f.get("states", 0)) <= 16
    if arith_heavy or tiny_state:
        trimmed = [e for e in candidates if e != "informal-bmc-65"]
        if trimmed:
            candidates = trimmed

    picked: List[str] = []
    used_slots = 0
    bucket_counts: Dict[Tuple[str, str, int], int] = defaultdict(int)

    def can_fit(e: str) -> bool:
        slots = ENGINE_META[e][5]
        return (used_slots == 0) or (slot_cap == 0) or (used_slots + slots <= slot_cap)

    def eff_score(e: str, apply_bucket: bool = True) -> float:
        s = scores[e]
        slots = ENGINE_META[e][5]
        # Half-budget slot opportunity cost: only penalize multi-worker engines
        # when a single engine would consume over half of Wave 0's slot budget.
        if slot_cap > 0 and slots > 1 and slots * 2 > slot_cap:
            s -= 0.08 * (slots - 1) / slot_cap
        if apply_bucket:
            s -= redundancy_penalty * bucket_counts[ENGINE_META[e][:3]]
        return s

    def add(e: str) -> None:
        nonlocal used_slots
        dom, kind, sub, _, _, slots, _, _, _ = ENGINE_META[e]
        picked.append(e)
        used_slots += slots
        bucket_counts[(dom, kind, sub)] += 1

    # Step 1: best prover
    provers = [e for e in candidates if ENGINE_META[e][3] and can_fit(e)]
    if provers:
        add(max(provers, key=lambda e: (eff_score(e, False), e)))

    # Step 2a: best dedicated bug-finder
    buggers = [
        e for e in candidates
        if e not in picked and ENGINE_META[e][4] and not ENGINE_META[e][3] and can_fit(e)
    ]
    if buggers:
        add(max(buggers, key=lambda e: (eff_score(e, True), e)))

    # Step 2b: second dedicated bug-finder from a distinct diversity bucket
    # (e.g., jump-step BMC alongside step-1 BMC, or bit-level BMC alongside wbmc)
    # when Wave 0 has >= 6 slots, counteracting prover-vs-bug-finder base-rate skew.
    if slot_cap == 0 or slot_cap >= 6:
        buggers2 = [
            e for e in candidates
            if e not in picked
            and ENGINE_META[e][4]
            and not ENGINE_META[e][3]
            and bucket_counts[ENGINE_META[e][:3]] == 0
            and can_fit(e)
        ]
        if buggers2:
            add(max(buggers2, key=lambda e: (eff_score(e, True), e)))

    # Step 3: greedily fill remaining slots with per-bucket redundancy penalty
    while True:
        rem = [e for e in candidates if e not in picked and can_fit(e)]
        if not rem:
            break
        add(max(rem, key=lambda e: (eff_score(e, True), e)))
    return picked


def eval_models(
    models: Sequence[str],
    by_mod: Dict[str, Dict[str, Tuple[str, float, float]]],
    feats: Dict[str, dict],
    fmts: Dict[str, str],
    means: Sequence[float],
    stds: Sequence[float],
    weights: Dict[str, List[float]],
    base_cap: int,
    redundancy_penalty: float,
) -> Tuple[int, float]:
    solved = 0
    par2 = 0.0
    for m in models:
        f = feats[m]
        fmt = fmts[m]
        x = standardize_apply(raw_features(f), means, stds)
        scores = {e: dot(w, x) for e, w in weights.items()}
        eff_cap = size_aware_slot_cap(base_cap, f)
        picked = select_portfolio(scores, fmt, f, eff_cap, redundancy_penalty)
        sols = [
            by_mod[m][e][1]
            for e in picked
            if e in by_mod[m] and by_mod[m][e][0] in ("safe", "unsafe")
        ]
        timeout_ms = next(iter(by_mod[m].values()))[2]
        if sols:
            solved += 1
            par2 += min(sols) / 1000.0
        else:
            par2 += (2.0 * timeout_ms) / 1000.0
    return solved, par2


def fit_all_engines(
    trn: Sequence[str],
    trn_x: Sequence[List[float]],
    engines: Sequence[str],
    by_mod: Dict[str, Dict[str, Tuple[str, float, float]]],
    feats: Dict[str, dict],
    fmts: Dict[str, str],
    lam: float,
) -> Dict[str, List[float]]:
    weights: Dict[str, List[float]] = {}
    for e in engines:
        # Train each engine strictly on models where it is eligible (format,
        # array capability, and max_state_bits cap) and actually ran.
        elig = [
            (x, utility(*by_mod[m][e]))
            for m, x in zip(trn, trn_x)
            if is_eligible(e, fmts[m], feats[m]) and e in by_mod[m]
        ]
        if not elig:
            weights[e] = [0.0] * len(FEATURE_NAMES)
        else:
            weights[e] = fit_ridge([a for a, _ in elig], [b for _, b in elig], lam)
    return weights


def vbs_stats(models: Sequence[str], by_mod: Dict[str, Dict[str, Tuple[str, float, float]]]) -> Tuple[int, float]:
    solved, par2 = 0, 0.0
    for m in models:
        sols = [w for e, (v, w, _) in by_mod[m].items() if e in ENGINE_META and v in ("safe", "unsafe")]
        t_ms = next(iter(by_mod[m].values()))[2]
        if sols:
            solved += 1
            par2 += min(sols) / 1000.0
        else:
            par2 += (2.0 * t_ms) / 1000.0
    return solved, par2


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("db", nargs="+", type=Path, help="EachEngine runs.jsonl file(s)")
    ap.add_argument("--lam", type=float, default=50.0, help="Ridge L2 regularization lambda")
    ap.add_argument("--penalty", type=float, default=0.28, help="Per-bucket redundancy penalty")
    ap.add_argument("--slots", type=int, default=12, help="Host base slot cap for CV")
    ap.add_argument("--folds", type=int, default=5, help="Number of CV folds")
    ap.add_argument("--out", type=Path, help="Write trained SelectorModel JSON here")
    args = ap.parse_args()

    by_mod: Dict[str, Dict[str, Tuple[str, float, float]]] = defaultdict(dict)
    feats: Dict[str, dict] = {}
    fmts: Dict[str, str] = {}
    names: Dict[str, str] = {}
    engines_set: Set[str] = set()

    for p in args.db:
        for line in p.read_text().splitlines():
            if not line.strip():
                continue
            r = json.loads(line)
            if r.get("mode") != "single" or not r.get("features"):
                continue
            e = r["engine"]
            if e not in ENGINE_META:
                continue
            fmt = str(r.get("format", "aiger")).lower()
            sha = str(r.get("model_sha256", ""))[:12]
            bname = Path(r["model"]).name
            mid = f"{fmt}:{sha}:{bname}" if sha else f"{fmt}:{r['model']}"
            by_mod[mid][e] = (r["verdict"], float(r["wall_ms"]), float(r["timeout_ms"]))
            feats[mid] = r["features"]
            fmts[mid] = fmt
            names[mid] = bname
            engines_set.add(e)

    # Sort deterministically by (format, basename, mid) so modulo-fold splits
    # stratify both AIGER and Btor2 models evenly across all CV folds.
    models = sorted(by_mod.keys(), key=lambda m: (fmts[m], names[m], m))
    aiger_models = [m for m in models if fmts[m] == "aiger"]
    btor2_models = [m for m in models if fmts[m] == "btor2"]
    engines = sorted(engines_set)

    vbs_s, vbs_p2 = vbs_stats(models, by_mod)
    a_vbs_s, a_vbs_p2 = vbs_stats(aiger_models, by_mod)
    b_vbs_s, b_vbs_p2 = vbs_stats(btor2_models, by_mod)

    print(
        f"Dataset: {len(models)} models ({len(aiger_models)} AIGER, {len(btor2_models)} Btor2), "
        f"{len(engines)} safety engines"
    )
    print(
        f"  VBS overall: {vbs_s}/{len(models)} PAR-2={vbs_p2:.1f}s | "
        f"AIGER: {a_vbs_s}/{len(aiger_models)} PAR-2={a_vbs_p2:.1f}s | "
        f"Btor2: {b_vbs_s}/{len(btor2_models)} PAR-2={b_vbs_p2:.1f}s"
    )

    for lam in [6.0, 12.0, 25.0, 50.0, 100.0]:
        cv_s, cv_p2 = 0, 0.0
        cv_as, cv_ap2 = 0, 0.0
        cv_bs, cv_bp2 = 0, 0.0
        for fold in range(args.folds):
            val = [m for i, m in enumerate(models) if i % args.folds == fold]
            trn = [m for i, m in enumerate(models) if i % args.folds != fold]
            trn_raw = [raw_features(feats[m]) for m in trn]
            means, stds = standardize_fit(trn_raw)
            trn_x = [standardize_apply(x, means, stds) for x in trn_raw]
            weights = fit_all_engines(trn, trn_x, engines, by_mod, feats, fmts, lam)
            s, p2 = eval_models(val, by_mod, feats, fmts, means, stds, weights, args.slots, args.penalty)
            cv_s += s
            cv_p2 += p2
            val_a = [m for m in val if fmts[m] == "aiger"]
            val_b = [m for m in val if fmts[m] == "btor2"]
            sa, p2a = eval_models(val_a, by_mod, feats, fmts, means, stds, weights, args.slots, args.penalty)
            sb, p2b = eval_models(val_b, by_mod, feats, fmts, means, stds, weights, args.slots, args.penalty)
            cv_as += sa
            cv_ap2 += p2a
            cv_bs += sb
            cv_bp2 += p2b
        print(
            f"  5-fold CV (lam={lam:5.1f}, pen={args.penalty:.2f}): "
            f"{cv_s:3d}/{len(models)} PAR-2={cv_p2:7.1f}s (+{cv_p2 - vbs_p2:5.1f}s) | "
            f"AIGER {cv_as:3d}/{len(aiger_models)} (+{cv_ap2 - a_vbs_p2:4.1f}s) | "
            f"Btor2 {cv_bs:2d}/{len(btor2_models)} (+{cv_bp2 - b_vbs_p2:4.1f}s)"
        )

    # Fit final model on all data at --lam.
    all_raw = [raw_features(feats[m]) for m in models]
    means, stds = standardize_fit(all_raw)
    all_x = [standardize_apply(x, means, stds) for x in all_raw]
    raw_weights = fit_all_engines(models, all_x, engines, by_mod, feats, fmts, args.lam)
    weights = {e: [round(v, 5) for v in w] for e, w in raw_weights.items()}

    tr_s, tr_p2 = eval_models(models, by_mod, feats, fmts, means, stds, weights, args.slots, args.penalty)
    print(f"\nFull-fit (lam={args.lam}, pen={args.penalty}): {tr_s}/{len(models)} | ideal PAR-2={tr_p2:.1f}s (gap +{tr_p2 - vbs_p2:.1f}s)")

    print("\nExample Wave-0 selections on representative models:")
    by_bname = {names[m]: m for m in models}
    for sample in [
        "yosyshq_appnote_123_cv32e40x-p128.aig",
        "ILA_Ridecore_ANDI_sanity.aig",
        "newton_1_5.aig",
        "qspiflash_dualflexpress_divfive-p040.aig",
        "bv_wrap_cnt_64_safe.btor2",
        "bv_linear_pair_32_bug.btor2",
        "mix_credit_32_ch3_bug.btor2",
        "bit_ring_32_safe.btor2",
        "arr_guard_64x8_safe.btor2",
    ]:
        if sample in by_bname:
            m = by_bname[sample]
            f = feats[m]
            x = standardize_apply(raw_features(f), means, stds)
            sc = {e: dot(w, x) for e, w in weights.items()}
            cap = size_aware_slot_cap(args.slots, f)
            picked = select_portfolio(sc, fmts[m], f, cap, args.penalty)
            print(f"  {sample:42s} (cap={cap:2d}): {picked}")

    if args.out:
        payload = {
            "version": 1,
            "features": FEATURE_NAMES,
            "means": [round(v, 5) for v in means],
            "stds": [round(v, 5) for v in stds],
            "redundancy_penalty": args.penalty,
            "weights": weights,
        }
        args.out.write_text(json.dumps(payload, indent=2) + "\n")
        print(f"\nWrote {args.out}")


if __name__ == "__main__":
    main()
