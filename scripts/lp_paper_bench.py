#!/usr/bin/env python3
"""Automated benchmark driver for Experiment 1 (Symbolic PST SAT scaling) and WS1A cache."""

import argparse
import csv
import hashlib
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GEN_SCRIPT = ROOT / "tests/lp/paper_cases/exp1_pst_scaling/gen_exp1_upf.py"
WS1A_SV = ROOT / "tests/lp/paper_cases/exp1_pst_scaling/ws1a_soc.sv"
WS3_CASES = [
    ("soc_multi_domain", ROOT / "tests/lp/soc_multi_domain/soc_run.tcl"),
    ("exp2_axi_valid_data", ROOT / "tests/lp/paper_cases/exp2_axi_valid_data/stage2_static.tcl"),
]
NS_DEFAULT = [4, 8, 12, 16, 20, 32, 64, 128]


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def run_cmd(cmd: list[str], env: dict[str, str] | None = None, cwd: Path = ROOT) -> str:
    res = subprocess.run(
        cmd,
        cwd=cwd,
        env=env,
        check=True,
        text=True,
        capture_output=True,
    )
    return res.stdout.strip()


def host_cpu_ram() -> dict[str, str | int]:
    info: dict[str, str | int] = {
        "os": platform.platform(),
        "machine": platform.machine(),
        "cpu_count": os.cpu_count() or 0,
    }
    if sys.platform == "darwin":
        for key, sc in [("cpu_brand", "machdep.cpu.brand_string"), ("ram_bytes", "hw.memsize")]:
            try:
                val = run_cmd(["sysctl", "-n", sc])
                info[key] = int(val) if key == "ram_bytes" else val
            except Exception:
                pass
    return info


def collect_env(upf_paths: dict[int, Path]) -> dict:
    git_commit = run_cmd(["git", "rev-parse", "HEAD"])
    git_branch = run_cmd(["git", "rev-parse", "--abbrev-ref", "HEAD"])
    rustc_ver = run_cmd(["rustc", "--version"])
    tools_lock = ""
    lock_path = ROOT / "tools.lock"
    if lock_path.is_file():
        tools_lock = lock_path.read_text(encoding="utf-8")
    return {
        "timestamp_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "git_commit": git_commit,
        "git_branch": git_branch,
        "rustc": rustc_ver,
        "python": sys.version.split()[0],
        "host": host_cpu_ram(),
        "tools_lock": tools_lock,
        "ws1a_sv_sha256": sha256_file(WS1A_SV),
        "exp1_upf_sha256": {str(n): sha256_file(p) for n, p in sorted(upf_paths.items())},
    }


def verify_cadical_env() -> None:
    tools_dir = os.environ.get("INFORMAL_TOOLS_DIR")
    if tools_dir:
        if not Path(tools_dir).is_dir():
            raise SystemExit(
                f"error: INFORMAL_TOOLS_DIR={tools_dir!r} does not exist or is not a directory"
            )
        return
    if any(os.environ.get(k) for k in ("INFORMAL_CADICAL_DIR", "INFORMAL_BITWUZLA_DIR")):
        return
    if (ROOT / "tools").is_dir() or (Path.home() / "Silicon/DV/tools").is_dir():
        return
    raise SystemExit(
        "error: cadical backend requested, but INFORMAL_TOOLS_DIR "
        "(or INFORMAL_CADICAL_DIR / INFORMAL_BITWUZLA_DIR) is not set and tools/ is absent. "
        "Set INFORMAL_TOOLS_DIR=/path/to/informal/tools or pass --backend mini."
    )


def build_example(backend: str) -> Path:
    env = os.environ.copy()
    if backend == "cadical":
        verify_cadical_env()
        cmd = [
            "cargo",
            "build",
            "--release",
            "--locked",
            "-p",
            "informal-fpv",
            "--example",
            "lp_paper_exp1",
        ]
        run_cmd(cmd, env=env)
        return ROOT / "target/release/examples/lp_paper_exp1"
    if backend == "mini":
        for k in ["INFORMAL_CADICAL_DIR", "INFORMAL_BITWUZLA_DIR"]:
            env.pop(k, None)
        env["INFORMAL_TOOLS_DIR"] = "/nonexistent"
        mini_target = ROOT / "target/bench-mini"
        cmd = [
            "cargo",
            "build",
            "--release",
            "--locked",
            "-p",
            "informal-fpv",
            "--example",
            "lp_paper_exp1",
            "--target-dir",
            str(mini_target),
        ]
        run_cmd(cmd, env=env)
        return mini_target / "release/examples/lp_paper_exp1"
    raise ValueError(f"unknown backend {backend}")


def aggregate_exp1(rows: list[dict], out_csv: Path) -> list[dict]:
    grouped: dict[tuple[str, int], list[dict]] = {}
    for r in rows:
        if r.get("mode") != "exp1":
            continue
        grouped.setdefault((r["backend_label"], int(r["n"])), []).append(r)

    agg: list[dict] = []
    for (backend, n), grp in sorted(grouped.items(), key=lambda kv: (kv[0][0], kv[0][1])):
        first = grp[0]
        rec = {
            "backend": backend,
            "solver_name": first["backend"],
            "n": n,
            "explicit_pst_rows_expr": first["explicit_pst_rows_expr"],
            "consistent": all(x["consistent"] for x in grp),
            "cnf_vars": first["cnf_vars"],
            "cnf_clauses": first["cnf_clauses"],
            "cnf_count_method": first["cnf_count_method"],
            "pairwise_queries": first["pairwise_queries"],
            "off_while_on_sat": first["off_while_on_sat"],
            "off_while_on_unsat": first["off_while_on_unsat"],
            "parse_resolve_ms_med": round(statistics.median(x["parse_resolve_ms"] for x in grp), 3),
            "sat_build_ms_med": round(statistics.median(x["sat_build_ms"] for x in grp), 3),
            "pairwise_query_ms_med": round(statistics.median(x["pairwise_query_ms"] for x in grp), 3),
            "total_ms_med": round(statistics.median(x["total_ms"] for x in grp), 3),
            "total_ms_min": round(min(x["total_ms"] for x in grp), 3),
            "total_ms_max": round(max(x["total_ms"] for x in grp), 3),
            "peak_rss_kb_med": int(statistics.median(x["peak_rss_kb"] for x in grp)),
            "repeats": len(grp),
        }
        agg.append(rec)

    if agg:
        with out_csv.open("w", newline="", encoding="utf-8") as f:
            writer = csv.DictWriter(f, fieldnames=list(agg[0].keys()))
            writer.writeheader()
            writer.writerows(agg)
    return agg


def parse_last_json(stdout: str) -> dict:
    # Handle both single-line JSON (exp1/ws1a) and multi-line pretty JSON (report_lp -json).
    lines = stdout.splitlines()
    for line in reversed(lines):
        s = line.strip()
        if s.startswith("{") and s.endswith("}"):
            return json.loads(s)
    idx = stdout.rfind("\n{")
    if idx != -1:
        return json.loads(stdout[idx + 1 :])
    idx0 = stdout.find("{")
    if idx0 != -1:
        return json.loads(stdout[idx0:])
    raise ValueError(f"no JSON object found in stdout:\n{stdout[:500]}")


def build_informal_cli() -> Path:
    run_cmd(["cargo", "build", "--release", "--locked", "-p", "informal-cli"], env=os.environ.copy())
    return ROOT / "target/release/informal"


def run_ws3_case(bin_path: Path, name: str, tcl_path: Path) -> dict:
    import re
    import time

    env = os.environ.copy()
    t0 = time.perf_counter()
    if sys.platform == "darwin":
        res = subprocess.run(
            ["/usr/bin/time", "-l", str(bin_path), "shell", str(tcl_path)],
            cwd=ROOT,
            env=env,
            check=True,
            text=True,
            capture_output=True,
        )
        m = re.search(r"(\d+)\s+maximum resident set size", res.stderr)
        peak_rss_kb = int(m.group(1)) // 1024 if m else 0
    else:
        res = subprocess.run(
            [str(bin_path), "shell", str(tcl_path)],
            cwd=ROOT,
            env=env,
            check=True,
            text=True,
            capture_output=True,
        )
        peak_rss_kb = 0
    wall_ms = (time.perf_counter() - t0) * 1000.0
    rep = parse_last_json(res.stdout)
    st = rep.get("stats") or {}
    summ = rep.get("summary") or {}
    return {
        "mode": "ws3",
        "case": name,
        "struct_graph_build_ms": st.get("struct_graph_build_ms", 0.0),
        "pst_sat_build_ms": st.get("pst_sat_build_ms", 0.0),
        "stage1_ms": st.get("stage1_ms", 0.0),
        "stage2_ms": st.get("stage2_ms", 0.0),
        "wall_ms": round(wall_ms, 3),
        "cnf_vars": st.get("cnf_vars", 0),
        "cnf_clauses": st.get("cnf_clauses", 0),
        "raw_crossings": st.get("raw_crossings", 0),
        "grouped_crossings": st.get("grouped_crossings", 0),
        "compression_ratio": st.get("compression_ratio", 1.0),
        "violations_total": summ.get("total", 0),
        "errors": summ.get("errors", 0),
        "warnings": summ.get("warnings", 0),
        "peak_rss_kb": peak_rss_kb,
    }


def aggregate_ws3(rows: list[dict], out_csv: Path) -> list[dict]:
    grouped: dict[str, list[dict]] = {}
    for r in rows:
        if r.get("mode") != "ws3":
            continue
        grouped.setdefault(r["case"], []).append(r)

    agg: list[dict] = []
    for case, grp in sorted(grouped.items()):
        first = grp[0]
        rec = {
            "case": case,
            "struct_graph_build_ms_med": round(statistics.median(x["struct_graph_build_ms"] for x in grp), 3),
            "pst_sat_build_ms_med": round(statistics.median(x["pst_sat_build_ms"] for x in grp), 3),
            "stage1_ms_med": round(statistics.median(x["stage1_ms"] for x in grp), 3),
            "stage2_ms_med": round(statistics.median(x["stage2_ms"] for x in grp), 3),
            "wall_ms_med": round(statistics.median(x["wall_ms"] for x in grp), 3),
            "cnf_vars": first["cnf_vars"],
            "cnf_clauses": first["cnf_clauses"],
            "raw_crossings": first["raw_crossings"],
            "grouped_crossings": first["grouped_crossings"],
            "compression_ratio": round(first["compression_ratio"], 2),
            "violations_total": first["violations_total"],
            "errors": first["errors"],
            "warnings": first["warnings"],
            "peak_rss_kb_med": int(statistics.median(x["peak_rss_kb"] for x in grp)),
            "repeats": len(grp),
        }
        agg.append(rec)

    if agg:
        with out_csv.open("w", newline="", encoding="utf-8") as f:
            writer = csv.DictWriter(f, fieldnames=list(agg[0].keys()))
            writer.writeheader()
            writer.writerows(agg)
    return agg


def aggregate_ws1a(rows: list[dict], out_csv: Path) -> list[dict]:
    grouped: dict[str, list[dict]] = {}
    for r in rows:
        if r.get("mode") != "ws1a":
            continue
        grouped.setdefault(r["backend_label"], []).append(r)

    agg: list[dict] = []
    for backend, grp in sorted(grouped.items()):
        first = grp[0]
        fe_uncached = statistics.median(x["frontend_5op_uncached_ms"] for x in grp)
        fe_cached = statistics.median(x["frontend_5op_cached_ms"] for x in grp)
        sess_cached = statistics.median(x["session_5app_cached_ms"] for x in grp)
        rec = {
            "backend": backend,
            "has_engines": first["has_engines"],
            "cold_compile_elab_ms_med": round(statistics.median(x["cold_compile_elab_ms"] for x in grp), 3),
            "cold_lower_ms_med": round(statistics.median(x["cold_lower_ms"] for x in grp), 3),
            "warm_tier1_hit_ms_med": round(statistics.median(x["warm_tier1_hit_ms"] for x in grp), 3),
            "warm_tier2_hit_ms_med": round(statistics.median(x["warm_tier2_hit_ms"] for x in grp), 3),
            "frontend_5op_uncached_ms_med": round(fe_uncached, 3),
            "frontend_5op_cached_ms_med": round(fe_cached, 3),
            "frontend_5op_speedup": round(fe_uncached / fe_cached, 2) if fe_cached > 0 else 0.0,
            "session_5app_cached_ms_med": round(sess_cached, 3),
            "session_tier1_compiles": first["session_tier1_compiles"],
            "session_tier1_hits": first["session_tier1_hits"],
            "session_tier2_lowers": first["session_tier2_lowers"],
            "session_tier2_hits": first["session_tier2_hits"],
            "peak_rss_kb_med": int(statistics.median(x["peak_rss_kb"] for x in grp)),
            "repeats": len(grp),
        }
        agg.append(rec)

    if agg:
        with out_csv.open("w", newline="", encoding="utf-8") as f:
            writer = csv.DictWriter(f, fieldnames=list(agg[0].keys()))
            writer.writeheader()
            writer.writerows(agg)
    return agg


def write_summary_md(
    exp1_agg: list[dict], ws1a_agg: list[dict], ws3_agg: list[dict], out_md: Path
) -> None:
    lines: list[str] = ["# Paper Benchmark Summary Tables (Measured)", ""]
    if exp1_agg:
        lines.extend([
            "## Experiment 1: Symbolic PST SAT Scaling",
            "",
            "| Backend | N | Explicit Rows | CNF Vars | CNF Clauses | Parse+Resolve (ms) | SAT Build (ms) | Queries N(N-1) (ms) | Total (ms) | Peak RSS (KiB) | SAT / UNSAT |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ])
        for r in exp1_agg:
            lines.append(
                f"| {r['backend']} ({r['solver_name']}) | {r['n']} | {r['explicit_pst_rows_expr']} | "
                f"{r['cnf_vars']} | {r['cnf_clauses']} | {r['parse_resolve_ms_med']:.3f} | "
                f"{r['sat_build_ms_med']:.3f} | {r['pairwise_query_ms_med']:.3f} | "
                f"{r['total_ms_med']:.3f} | {r['peak_rss_kb_med']} | "
                f"{r['off_while_on_sat']}/{r['off_while_on_unsat']} |"
            )
        lines.append("")
    if ws1a_agg:
        lines.extend([
            "## WS1A: `CompiledDesign` Cold vs. Warm Cache",
            "",
            "| Backend | Engines | Cold Compile+Elab (ms) | Cold Lower (ms) | Warm Tier-1 Hit (ms) | Warm Tier-2 Hit (ms) | 5-Op FE Uncached (ms) | 5-Op FE Cached (ms) | FE Speedup | Full 5-App Session (ms) | Tier-1 Compiles / Hits | Peak RSS (KiB) |",
            "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ])
        for r in ws1a_agg:
            lines.append(
                f"| {r['backend']} | {r['has_engines']} | {r['cold_compile_elab_ms_med']:.3f} | "
                f"{r['cold_lower_ms_med']:.3f} | {r['warm_tier1_hit_ms_med']:.3f} | "
                f"{r['warm_tier2_hit_ms_med']:.3f} | {r['frontend_5op_uncached_ms_med']:.3f} | "
                f"{r['frontend_5op_cached_ms_med']:.3f} | {r['frontend_5op_speedup']:.2f}x | "
                f"{r['session_5app_cached_ms_med']:.3f} | {r['session_tier1_compiles']} / {r['session_tier1_hits']} | "
                f"{r['peak_rss_kb_med']} |"
            )
        lines.append("")
    if ws3_agg:
        lines.extend([
            "## WS3: `StructuralGraph` & Static Power-Intent/RTL Verification (Stages 1–2)",
            "",
            "| Case | StructGraph (ms) | PstSat (ms) | Stage 1 (ms) | Stage 2 (ms) | Wall (ms) | Raw -> Grouped | Compression | Errors / Warnings | Peak RSS (KiB) |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ])
        for r in ws3_agg:
            lines.append(
                f"| {r['case']} | {r['struct_graph_build_ms_med']:.3f} | "
                f"{r['pst_sat_build_ms_med']:.3f} | {r['stage1_ms_med']:.3f} | "
                f"{r['stage2_ms_med']:.3f} | {r['wall_ms_med']:.3f} | "
                f"{r['raw_crossings']} -> {r['grouped_crossings']} | {r['compression_ratio']:.2f}x | "
                f"{r['errors']} / {r['warnings']} | {r['peak_rss_kb_med']} |"
            )
        lines.append("")
    out_md.write_text("\n".join(lines), encoding="utf-8")


def main() -> None:
    ap = argparse.ArgumentParser(description="Run Experiment 1, WS1A, and WS3 benchmarks")
    ap.add_argument("--exp", choices=["1", "ws1a", "ws3", "all"], default="all")
    ap.add_argument("--repeats", type=int, default=3)
    ap.add_argument("--backend", choices=["cadical", "mini", "both"], default="cadical")
    ap.add_argument("--out", type=Path, default=Path("research/lp-paper/measurements"))
    ap.add_argument("--ns", type=str, default=",".join(str(x) for x in NS_DEFAULT))
    args = ap.parse_args()

    mode_map = {"1": {"exp1"}, "ws1a": {"ws1a"}, "ws3": {"ws3"}, "all": {"exp1", "ws1a", "ws3"}}
    active_modes = mode_map[args.exp]
    backends = ["cadical", "mini"] if args.backend == "both" else [args.backend]
    if "cadical" in backends and args.exp in ("1", "ws1a", "all"):
        verify_cadical_env()

    out_dir = args.out if args.out.is_absolute() else (ROOT / args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    upf_dir = out_dir / "upf_cases"
    upf_dir.mkdir(parents=True, exist_ok=True)
    work_root = out_dir / "ws1a_work"

    ns = [int(x.strip()) for x in args.ns.split(",") if x.strip()]
    upf_paths: dict[int, Path] = {}
    if args.exp in ("1", "all"):
        for n in ns:
            upf_path = upf_dir / f"soc_n{n}.upf"
            run_cmd([sys.executable, "-I", str(GEN_SCRIPT), "--n", str(n), "--out", str(upf_path)])
            upf_paths[n] = upf_path

    env_path = out_dir / "env.json"
    env_data = collect_env(upf_paths)
    if args.exp != "all" and env_path.is_file():
        prev_env = json.loads(env_path.read_text(encoding="utf-8"))
        if prev_env.get("git_commit") and prev_env.get("git_commit") != env_data["git_commit"]:
            print(
                f"warning: partial --exp {args.exp} commit {env_data['git_commit']} differs from prior env.json {prev_env.get('git_commit')}; recommend --exp all",
                file=sys.stderr,
            )
        if not upf_paths and prev_env.get("exp1_upf_sha256"):
            env_data["exp1_upf_sha256"] = prev_env["exp1_upf_sha256"]
    env_path.write_text(json.dumps(env_data, indent=2) + "\n", encoding="utf-8")

    bins = {b: build_example(b) for b in backends} if args.exp in ("1", "ws1a", "all") else {}
    cli_bin = build_informal_cli() if args.exp in ("ws3", "all") else None

    new_runs: list[dict] = []
    for b in backends:
        if b not in bins:
            continue
        bin_path = bins[b]
        if args.exp in ("1", "all"):
            for n in ns:
                upf_path = upf_paths[n]
                for rep in range(1, args.repeats + 1):
                    raw = run_cmd([str(bin_path), "--mode", "exp1", "--upf", str(upf_path), "--n", str(n)])
                    row = parse_last_json(raw)
                    actual_backend = row.get("backend")
                    if actual_backend != b:
                        raise RuntimeError(
                            f"requested backend {b!r}, but lp_paper_exp1 reported solver_name={actual_backend!r}. "
                            "Verify INFORMAL_TOOLS_DIR points to built CaDiCaL/Bitwuzla libraries and rebuild."
                        )
                    row["backend_label"] = b
                    row["repeat"] = rep
                    new_runs.append(row)
        if args.exp in ("ws1a", "all"):
            for rep in range(1, args.repeats + 1):
                wd = work_root / f"{b}_r{rep}"
                raw = run_cmd([
                    str(bin_path),
                    "--mode",
                    "ws1a",
                    "--sv",
                    str(WS1A_SV),
                    "--top",
                    "ws1a_soc",
                    "--workdir",
                    str(wd),
                ])
                row = parse_last_json(raw)
                actual_backend = row.get("backend")
                if actual_backend != b:
                    raise RuntimeError(
                        f"requested backend {b!r}, but lp_paper_exp1 reported solver_name={actual_backend!r}. "
                        "Verify INFORMAL_TOOLS_DIR points to built CaDiCaL/Bitwuzla libraries and rebuild."
                    )
                row["backend_label"] = b
                row["repeat"] = rep
                new_runs.append(row)
                shutil.rmtree(wd, ignore_errors=True)

    if args.exp in ("ws3", "all") and cli_bin is not None:
        for case_name, tcl_path in WS3_CASES:
            for rep in range(1, args.repeats + 1):
                row = run_ws3_case(cli_bin, case_name, tcl_path)
                row["repeat"] = rep
                new_runs.append(row)

    runs_path = out_dir / "runs.jsonl"
    if args.exp != "all" and runs_path.is_file():
        old_runs = [
            json.loads(ln)
            for ln in runs_path.read_text(encoding="utf-8").splitlines()
            if ln.strip()
        ]
        preserved = [r for r in old_runs if r.get("mode") not in active_modes]
    else:
        preserved = []
    all_runs = preserved + new_runs
    runs_path.write_text("".join(json.dumps(r) + "\n" for r in all_runs), encoding="utf-8")

    exp1_agg = aggregate_exp1(all_runs, out_dir / "exp1_pst_scaling.csv")
    ws1a_agg = aggregate_ws1a(all_runs, out_dir / "ws1a_cache.csv")
    ws3_agg = aggregate_ws3(all_runs, out_dir / "ws3_static.csv")
    write_summary_md(exp1_agg, ws1a_agg, ws3_agg, out_dir / "summary_tables.md")
    print(f"Wrote {len(all_runs)} runs ({len(new_runs)} new) to {runs_path}")


if __name__ == "__main__":
    main()
