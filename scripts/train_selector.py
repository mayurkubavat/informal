#!/usr/bin/env python3
"""Offline trainer & 5-fold cross-validator for InFormal's Phase 3 learned selector.

Reads `informal bench --each` JSONL records, fits an L2-regularized Ridge
regression utility scorer over 10 scale-invariant structural features (pure
Python stdlib; solves the 10x10 normal equations directly), reports 5-fold
out-of-fold portfolio PAR-2 against static baselines and the Virtual Best
Solver (VBS), and optionally writes the JSON model for `INFORMAL_SELECTOR_MODEL`
or embedding in `crates/informal-engine/src/learned.rs`.
"""

from __future__ import annotations

import argparse
import json
import math
from collections import defaultdict
from pathlib import Path
from typing import Dict, List, Sequence, Tuple

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

# Engine metadata mirroring `Capabilities` for complementarity-aware evaluation.
ENGINE_META: Dict[str, Tuple[str, bool, bool, int, int | None]] = {
    # name: (kind, proves, finds_bugs, slots, max_state_bits)
    "informal-ic3-mt": ("ic3", True, True, 4, None),
    "informal-ic3": ("ic3", True, True, 1, None),
    "informal-ic3-cadical": ("ic3", True, True, 1, None),
    "informal-ic3-inn": ("ic3", True, True, 1, None),
    "informal-bmc": ("bmc", False, True, 1, None),
    "informal-bmc-10": ("bmc", False, True, 1, None),
    "informal-bmc-65": ("bmc", False, True, 1, None),
    "informal-kind": ("kind", True, True, 1, None),
    "informal-kind-ic3": ("hybrid", True, True, 1, None),
    "informal-gla": ("hybrid", True, True, 1, None),
    "informal-car": ("car", True, True, 1, None),
    "informal-bcar": ("car", True, True, 1, None),
    "informal-imc": ("imc", True, True, 1, None),
    "informal-bdd": ("bdd", True, True, 1, 200),
    "informal-swarm": ("swarm", False, True, 1, None),
}


def raw_features(f: dict) -> List[float]:
    states = float(f.get("states", 0))
    state_bits = float(f.get("state_bits", states))
    nodes = float(f.get("nodes", 0))
    inputs = float(f.get("inputs", 0))
    constraints = float(f.get("constraints", 0))
    arith_ops = float(f.get("arith_ops", 0))
    max_width = float(f.get("max_width", 1))
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


def select_portfolio(
    scores: Dict[str, float],
    f: dict,
    slot_cap: int,
    redundancy_penalty: float = 0.28,
) -> List[str]:
    """Complementarity-aware greedy selection under a thread-slot budget."""
    state_bits = f.get("state_bits", f.get("states", 0))
    candidates = [
        e for e in scores
        if not (e == "informal-bdd" and state_bits > 200)
    ]
    picked: List[str] = []
    used_slots = 0
    kind_counts: Dict[str, int] = defaultdict(int)

    def can_fit(e: str) -> bool:
        slots = ENGINE_META.get(e, ("other", False, False, 1, None))[3]
        return (used_slots == 0) or (used_slots + slots <= slot_cap)

    def add(e: str) -> None:
        nonlocal used_slots
        kind, _, _, slots, _ = ENGINE_META.get(e, ("other", False, False, 1, None))
        picked.append(e)
        used_slots += slots
        kind_counts[kind] += 1

    # Step 1: best prover
    provers = [e for e in candidates if ENGINE_META.get(e, ("", False, False, 1, None))[1] and can_fit(e)]
    if provers:
        add(max(provers, key=lambda e: scores[e]))

    # Step 2: best bug-finder of a distinct kind (guarantees BMC/Swarm coverage)
    buggers = [
        e for e in candidates
        if e not in picked
        and ENGINE_META.get(e, ("", False, False, 1, None))[2]
        and not ENGINE_META.get(e, ("", False, False, 1, None))[1]
        and can_fit(e)
    ]
    if buggers:
        add(max(buggers, key=lambda e: scores[e]))

    # Step 3: greedily fill remaining slots with redundancy penalty per kind
    while True:
        rem = [e for e in candidates if e not in picked and can_fit(e)]
        if not rem:
            break
        best = max(
            rem,
            key=lambda e: scores[e]
            - redundancy_penalty * kind_counts[ENGINE_META.get(e, ("other", False, False, 1, None))[0]],
        )
        add(best)
    return picked


def eval_models(
    models: Sequence[str],
    by_mod: Dict[str, Dict[str, Tuple[str, float, float]]],
    feats: Dict[str, dict],
    means: Sequence[float],
    stds: Sequence[float],
    weights: Dict[str, List[float]],
    slot_cap: int,
) -> Tuple[int, float]:
    solved = 0
    par2 = 0.0
    for m in models:
        x = standardize_apply(raw_features(feats[m]), means, stds)
        scores = {e: dot(w, x) for e, w in weights.items()}
        # Size-aware effective slot cap matching Phase 3 runtime policy.
        f = feats[m]
        eff_cap = slot_cap
        if f.get("nodes", 0) >= 250_000 or f.get("state_bits", 0) >= 20_000:
            eff_cap = min(eff_cap, 7)
        elif f.get("nodes", 0) >= 50_000 or f.get("state_bits", 0) >= 2_000:
            eff_cap = min(eff_cap, 9)
        picked = select_portfolio(scores, f, eff_cap)
        sols = [
            w_ms
            for e in picked
            if e in by_mod[m] and by_mod[m][e][0] in ("safe", "unsafe")
            for w_ms in [by_mod[m][e][1]]
        ]
        timeout_ms = next(iter(by_mod[m].values()))[2]
        if sols:
            solved += 1
            par2 += min(sols) / 1000.0
        else:
            par2 += (2.0 * timeout_ms) / 1000.0
    return solved, par2


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("db", nargs="+", type=Path, help="EachEngine runs.jsonl file(s)")
    ap.add_argument("--lam", type=float, default=12.0, help="Ridge L2 regularization lambda")
    ap.add_argument("--slots", type=int, default=8, help="Target Wave-0 slot cap for CV")
    ap.add_argument("--folds", type=int, default=5, help="Number of CV folds")
    ap.add_argument("--out", type=Path, help="Write trained SelectorModel JSON here")
    args = ap.parse_args()

    by_mod: Dict[str, Dict[str, Tuple[str, float, float]]] = defaultdict(dict)
    feats: Dict[str, dict] = {}
    engines_set = set()
    for p in args.db:
        for line in p.read_text().splitlines():
            if not line.strip():
                continue
            r = json.loads(line)
            if r.get("mode") != "single" or not r.get("features"):
                continue
            m = Path(r["model"]).name
            e = r["engine"]
            by_mod[m][e] = (r["verdict"], float(r["wall_ms"]), float(r["timeout_ms"]))
            feats[m] = r["features"]
            engines_set.add(e)

    models = sorted(by_mod.keys())
    engines = sorted(engines_set)

    # Virtual Best Solver ceiling.
    vbs_solved, vbs_par2 = 0, 0.0
    for m in models:
        sols = [w for v, w, _ in by_mod[m].values() if v in ("safe", "unsafe")]
        t_ms = next(iter(by_mod[m].values()))[2]
        if sols:
            vbs_solved += 1
            vbs_par2 += min(sols) / 1000.0
        else:
            vbs_par2 += (2.0 * t_ms) / 1000.0

    # 5-fold cross-validation across lambda candidates.
    print(f"Dataset: {len(models)} models, {len(engines)} engines | VBS: {vbs_solved}/{len(models)}, PAR-2={vbs_par2:.1f}s")
    for lam in [2.0, 6.0, 12.0, 25.0, 50.0]:
        cv_solved, cv_par2 = 0, 0.0
        for fold in range(args.folds):
            val = [m for i, m in enumerate(models) if i % args.folds == fold]
            trn = [m for i, m in enumerate(models) if i % args.folds != fold]
            trn_raw = [raw_features(feats[m]) for m in trn]
            means, stds = standardize_fit(trn_raw)
            trn_x = [standardize_apply(x, means, stds) for x in trn_raw]
            weights = {}
            for e in engines:
                cap = ENGINE_META.get(e, ("other", False, False, 1, None))[4]
                elig = [
                    (x, utility(*by_mod[m][e]) if e in by_mod[m] else 0.0)
                    for m, x in zip(trn, trn_x)
                    if cap is None or feats[m].get("state_bits", feats[m].get("states", 0)) <= cap
                ]
                if not elig:
                    weights[e] = [0.0] * len(FEATURE_NAMES)
                else:
                    weights[e] = fit_ridge([a for a, _ in elig], [b for _, b in elig], lam)
            s, p2 = eval_models(val, by_mod, feats, means, stds, weights, args.slots)
            cv_solved += s
            cv_par2 += p2
        print(f"  5-fold CV (lam={lam:4.1f}, slots={args.slots}): {cv_solved:2d}/{len(models)} | OOF PAR-2={cv_par2:6.1f}s (gap to VBS: +{cv_par2 - vbs_par2:.1f}s)")

    # Fit final model on all data at --lam.
    all_raw = [raw_features(feats[m]) for m in models]
    means, stds = standardize_fit(all_raw)
    all_x = [standardize_apply(x, means, stds) for x in all_raw]
    weights = {}
    for e in engines:
        cap = ENGINE_META.get(e, ("other", False, False, 1, None))[4]
        elig = [
            (x, utility(*by_mod[m][e]) if e in by_mod[m] else 0.0)
            for m, x in zip(models, all_x)
            if cap is None or feats[m].get("state_bits", feats[m].get("states", 0)) <= cap
        ]
        if not elig:
            weights[e] = [0.0] * len(FEATURE_NAMES)
        else:
            weights[e] = [round(v, 5) for v in fit_ridge([a for a, _ in elig], [b for _, b in elig], args.lam)]

    tr_s, tr_p2 = eval_models(models, by_mod, feats, means, stds, weights, args.slots)
    print(f"\nFull-fit (lam={args.lam}, slots={args.slots}): {tr_s}/{len(models)} | ideal PAR-2={tr_p2:.1f}s")
    print("\nExample Wave-0 selections on representative models:")
    for sample in [
        "yosyshq_appnote_123_cv32e40x-p128.aig",
        "ILA_Ridecore_ANDI_sanity.aig",
        "newton_1_5.aig",
        "cal178.aig",
        "qspiflash_dualflexpress_divfive-p040.aig",
    ]:
        if sample in feats:
            x = standardize_apply(raw_features(feats[sample]), means, stds)
            sc = {e: dot(w, x) for e, w in weights.items()}
            f = feats[sample]
            eff_cap = args.slots
            if f.get("nodes", 0) >= 250_000 or f.get("state_bits", 0) >= 20_000:
                eff_cap = min(eff_cap, 7)
            elif f.get("nodes", 0) >= 50_000 or f.get("state_bits", 0) >= 2_000:
                eff_cap = min(eff_cap, 9)
            picked = select_portfolio(sc, f, eff_cap)
            print(f"  {sample:42s} (cap={eff_cap}): {picked}")

    if args.out:
        payload = {
            "version": 1,
            "features": FEATURE_NAMES,
            "means": [round(v, 5) for v in means],
            "stds": [round(v, 5) for v in stds],
            "redundancy_penalty": 0.28,
            "weights": weights,
        }
        args.out.write_text(json.dumps(payload, indent=2) + "\n")
        print(f"\nWrote {args.out}")


if __name__ == "__main__":
    main()
