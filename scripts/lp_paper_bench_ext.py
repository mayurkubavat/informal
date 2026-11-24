#!/usr/bin/env python3
"""Automated benchmark driver for Paper Experiments 2, 3, and 4.

- Experiment 2: Closed-loop certified auto-waivers & assumption-invalidation soundness
- Experiment 3: Formal LPV bug-finding (AAG, 2-copy corruption, LP-SEC) & why_cex slice compression
- Experiment 4: Streaming Liberty .lib parsing + 1k..100k-cell gate/PG Stage 3-4 throughput + LP-LEC
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tests/lp_e2e"))

from lib.fixtures_gen import (  # noqa: E402
    _nldm_7x7_values,
    generate_gate_pg_netlist,
    generate_gate_pg_upf,
    generate_mini_liberty,
    generate_stdcell_verilog,
)


def run_cmd(
    cmd: list[str],
    *,
    env: dict[str, str] | None = None,
    cwd: Path = ROOT,
    timeout_s: float = 180.0,
) -> tuple[str, str, float, int]:
    t0 = time.perf_counter()
    if sys.platform == "darwin":
        full_cmd = ["/usr/bin/time", "-l", *cmd]
    else:
        full_cmd = cmd
    res = subprocess.run(
        full_cmd,
        cwd=cwd,
        env=env,
        check=True,
        text=True,
        capture_output=True,
        timeout=timeout_s,
    )
    wall_ms = (time.perf_counter() - t0) * 1000.0
    peak_rss_kb = 0
    if sys.platform == "darwin":
        m = re.search(r"(\d+)\s+maximum resident set size", res.stderr)
        if m:
            peak_rss_kb = int(m.group(1)) // 1024
    return res.stdout, res.stderr, wall_ms, peak_rss_kb


def run_shell(
    bin_path: Path,
    tcl_body: str,
    *,
    cwd: Path,
    workdir: Path,
    timeout_s: float = 180.0,
) -> tuple[str, str, float, int]:
    script = cwd / "bench.tcl"
    script.write_text(tcl_body, encoding="utf-8")
    engines = ROOT / "config/engines.toml"
    cmd = [
        str(bin_path),
        "shell",
        str(script),
        "--engines",
        str(engines),
        "--workdir",
        str(workdir),
    ]
    return run_cmd(cmd, env=os.environ.copy(), cwd=cwd, timeout_s=timeout_s)


def build_release_cli() -> Path:
    subprocess.run(
        ["cargo", "build", "--release", "--locked", "-p", "informal-cli"],
        cwd=ROOT,
        env=os.environ.copy(),
        check=True,
    )
    return ROOT / "target/release/informal"


def collect_env() -> dict[str, Any]:
    git_commit = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
    ).strip()
    git_branch = subprocess.check_output(
        ["git", "rev-parse", "--abbrev-ref", "HEAD"], cwd=ROOT, text=True
    ).strip()
    rustc_ver = subprocess.check_output(["rustc", "--version"], cwd=ROOT, text=True).strip()
    host: dict[str, Any] = {
        "os": platform.platform(),
        "machine": platform.machine(),
        "cpu_count": os.cpu_count() or 0,
    }
    if sys.platform == "darwin":
        for key, sc in [("cpu_brand", "machdep.cpu.brand_string"), ("ram_bytes", "hw.memsize")]:
            try:
                val = subprocess.check_output(["sysctl", "-n", sc], text=True).strip()
                host[key] = int(val) if key == "ram_bytes" else val
            except Exception:
                pass
    return {
        "timestamp_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "git_commit": git_commit,
        "git_branch": git_branch,
        "rustc": rustc_ver,
        "python": sys.version.split()[0],
        "host": host,
    }


def write_csv(rows: list[dict[str, Any]], path: Path) -> None:
    if not rows:
        return
    with path.open("w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=list(rows[0].keys()))
        w.writeheader()
        w.writerows(rows)


def extract_marker_ms(stdout: str, tag: str) -> float:
    m = re.search(rf"{re.escape(tag)}=(\d+(?:\.\d+)?)", stdout)
    return float(m.group(1)) if m else 0.0


def count_lp_statuses(rep: dict[str, Any]) -> tuple[int, int, int, int, int]:
    """Return (total, rtl_open_err, rtl_formally_waived, rtl_cex, extra_formal_cex)."""
    viols = rep.get("violations", [])
    rtl = [v for v in viols if str(v.get("stage", "rtl")).lower() == "rtl"]
    formal = [v for v in viols if str(v.get("stage", "")).lower() == "formal"]
    rtl_open_err = sum(
        1
        for v in rtl
        if v.get("status") == "open" and str(v.get("severity", "error")).lower() == "error"
    )
    rtl_fw = sum(1 for v in rtl if v.get("status") in ("formally_waived", "formallywaived"))
    rtl_cex = sum(1 for v in rtl if v.get("status") in ("cex", "formal_cex", "formalcex"))
    extra_formal_cex = sum(
        1 for v in formal if v.get("status") in ("cex", "formal_cex", "formalcex")
    )
    return len(viols), rtl_open_err, rtl_fw, rtl_cex, extra_formal_cex


# ---------------------------------------------------------------------------
# Experiment 2: Certified Auto-Waivers & Soundness
# ---------------------------------------------------------------------------


def run_exp2(bin_path: Path, repeats: int) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    raw_runs: list[dict[str, Any]] = []

    cases = [
        (
            "exp2_axi_valid_data",
            [
                ROOT / "tests/lp/paper_cases/exp2_axi_valid_data/axi_valid_data.sv",
                ROOT / "tests/lp/paper_cases/exp2_axi_valid_data/axi_valid_data.upf",
            ],
            """
analyze -sv12 axi_valid_data.sv
elaborate -top axi_valid_data
clock clk
reset -expression {!rst_n}
load_upf axi_valid_data.upf
set t0 [clock milliseconds]
check_lp -stage rtl
set t1 [clock milliseconds]
set f0 [open s2.json w]; puts $f0 [report_lp -json]; close $f0
check_lp -prove_waivers
set t2 [clock milliseconds]
set f1 [open aw.json w]; puts $f1 [report_lp -json]; close $f1
puts "STATIC_MS=[expr {$t1 - $t0}]"
puts "WAIVE_MS=[expr {$t2 - $t1}]"
""",
            "masked_reconv",
            None,
        ),
        (
            "exp2_axi_bad_clamp",
            [
                ROOT / "tests/lp/paper_cases/exp2_axi_valid_data/axi_valid_data.sv",
                ROOT / "tests/lp/paper_cases/exp2_axi_valid_data/axi_valid_data.upf",
            ],
            """
analyze -sv12 axi_valid_data.sv
elaborate -top axi_valid_data
clock clk
reset -expression {!rst_n}
load_upf axi_valid_data.upf
set t0 [clock milliseconds]
check_lp -stage rtl
set t1 [clock milliseconds]
set f0 [open s2.json w]; puts $f0 [report_lp -json]; close $f0
check_lp -prove_waivers
set t2 [clock milliseconds]
set f1 [open aw.json w]; puts $f1 [report_lp -json]; close $f1
puts "STATIC_MS=[expr {$t1 - $t0}]"
puts "WAIVE_MS=[expr {$t2 - $t1}]"
""",
            "leaky_mutated",
            ("axi_valid_data.sv", "parameter bit BAD_VALID_CLAMP = 1'b0", "parameter bit BAD_VALID_CLAMP = 1'b1"),
        ),
        (
            "autowaiver_masked",
            [
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w3_autowaiver/autowaiver_soc.sv",
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w3_autowaiver/autowaiver_soc.upf",
            ],
            """
analyze -sv12 autowaiver_soc.sv
elaborate -top autowaiver_soc
clock clk
reset -expression {!rst_n}
load_upf autowaiver_soc.upf
assume -name m_active_only_when_on {active_use |-> !sleep_en}
set t0 [clock milliseconds]
check_lp -stage rtl
set t1 [clock milliseconds]
set f0 [open s2.json w]; puts $f0 [report_lp -json]; close $f0
check_lp -prove_waivers
set t2 [clock milliseconds]
set f1 [open aw.json w]; puts $f1 [report_lp -json]; close $f1
puts "STATIC_MS=[expr {$t1 - $t0}]"
puts "WAIVE_MS=[expr {$t2 - $t1}]"
""",
            "masked_assumed",
            None,
        ),
        (
            "autowaiver_unmasked_leaky",
            [
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w3_autowaiver/autowaiver_soc.sv",
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w3_autowaiver/autowaiver_soc.upf",
            ],
            """
analyze -sv12 autowaiver_soc.sv
elaborate -top autowaiver_soc
clock clk
reset -expression {!rst_n}
load_upf autowaiver_soc.upf
set t0 [clock milliseconds]
check_lp -stage rtl
set t1 [clock milliseconds]
set f0 [open s2.json w]; puts $f0 [report_lp -json]; close $f0
check_lp -prove_waivers
set t2 [clock milliseconds]
set f1 [open aw.json w]; puts $f1 [report_lp -json]; close $f1
puts "STATIC_MS=[expr {$t1 - $t0}]"
puts "WAIVE_MS=[expr {$t2 - $t1}]"
""",
            "leaky_unassumed",
            None,
        ),
        (
            "t3_waiver_mix",
            [
                ROOT / "tests/lp_e2e/fixtures/workloads/t3_waiver_mix/t3_waiver_mix.sv",
                ROOT / "tests/lp_e2e/fixtures/workloads/t3_waiver_mix/t3_waiver_mix.upf",
            ],
            """
analyze -sv12 t3_waiver_mix.sv
elaborate -top t3_waiver_mix
clock clk
reset -expression {!rst_n}
load_upf t3_waiver_mix.upf
set t0 [clock milliseconds]
check_lp -stage rtl
set t1 [clock milliseconds]
set f0 [open s2.json w]; puts $f0 [report_lp -json]; close $f0
check_lp -prove_waivers
set t2 [clock milliseconds]
set f1 [open aw.json w]; puts $f1 [report_lp -json]; close $f1
puts "STATIC_MS=[expr {$t1 - $t0}]"
puts "WAIVE_MS=[expr {$t2 - $t1}]"
""",
            "mixed",
            None,
        ),
    ]

    for name, files, tcl, kind, patch in cases:
        for rep in range(1, repeats + 1):
            td = Path(tempfile.mkdtemp(prefix=f"exp2_{name}_"))
            try:
                for fp in files:
                    shutil.copy2(fp, td / fp.name)
                if patch is not None:
                    fname, old, new = patch
                    p = td / fname
                    txt = p.read_text(encoding="utf-8")
                    assert old in txt, f"patch target not found in {fname}"
                    p.write_text(txt.replace(old, new, 1), encoding="utf-8")
                stdout, _stderr, wall_ms, rss_kb = run_shell(
                    bin_path, tcl, cwd=td, workdir=td / "work"
                )
                s2 = json.loads((td / "s2.json").read_text(encoding="utf-8"))
                aw = json.loads((td / "aw.json").read_text(encoding="utf-8"))
                _, s2_open, _, _, _ = count_lp_statuses(s2)
                _, aw_open, aw_fw, aw_rtl_cex, aw_aag_cex = count_lp_statuses(aw)
                fp_red = round(100.0 * aw_fw / s2_open, 1) if s2_open > 0 else 0.0
                raw_runs.append(
                    {
                        "mode": "exp2",
                        "case": name,
                        "kind": kind,
                        "repeat": rep,
                        "static_open_errors": s2_open,
                        "formally_waived": aw_fw,
                        "post_rtl_open": aw_open,
                        "post_rtl_cex": aw_rtl_cex,
                        "aag_formal_cex": aw_aag_cex,
                        "fp_reduction_pct": fp_red,
                        "static_ms": extract_marker_ms(stdout, "STATIC_MS"),
                        "waive_ms": extract_marker_ms(stdout, "WAIVE_MS"),
                        "wall_ms": round(wall_ms, 2),
                        "peak_rss_kb": rss_kb,
                    }
                )
            finally:
                shutil.rmtree(td, ignore_errors=True)

    agg: list[dict[str, Any]] = []
    for name, _, _, kind, _ in cases:
        grp = [r for r in raw_runs if r["case"] == name]
        f0 = grp[0]
        agg.append(
            {
                "case": name,
                "kind": kind,
                "static_open_errors": f0["static_open_errors"],
                "formally_waived": f0["formally_waived"],
                "post_rtl_open": f0["post_rtl_open"],
                "post_rtl_cex": f0["post_rtl_cex"],
                "aag_formal_cex": f0["aag_formal_cex"],
                "fp_reduction_pct": f0["fp_reduction_pct"],
                "static_ms_med": round(statistics.median(x["static_ms"] for x in grp), 2),
                "waive_ms_med": round(statistics.median(x["waive_ms"] for x in grp), 2),
                "wall_ms_med": round(statistics.median(x["wall_ms"] for x in grp), 2),
                "peak_rss_kb_med": int(statistics.median(x["peak_rss_kb"] for x in grp)),
                "repeats": len(grp),
            }
        )
    return raw_runs, agg


# ---------------------------------------------------------------------------
# Experiment 3: Formal LPV Bug-Finding, LP-SEC, & why_cex Slice Compression
# ---------------------------------------------------------------------------


def parse_csv_props(csv_path: Path) -> tuple[int, int, int]:
    proven = covered = cex = 0
    for ln in csv_path.read_text(encoding="utf-8").splitlines()[1:]:
        parts = ln.split(",")
        if len(parts) < 3:
            continue
        st = parts[2].strip().lower()
        if st == "proven":
            proven += 1
        elif st == "covered":
            covered += 1
        elif st == "cex":
            cex += 1
    return proven, covered, cex


def run_exp3(
    bin_path: Path, repeats: int
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], list[dict[str, Any]]]:
    raw_runs: list[dict[str, Any]] = []

    lpv_cases = [
        (
            "lpv_pmu_clean",
            [
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w2_lpv_pmu_xleak/lpv_pmu_soc.sv",
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w2_lpv_pmu_xleak/lpv_pmu_soc_clean.upf",
            ],
            """
analyze -sv12 lpv_pmu_soc.sv
elaborate -top lpv_pmu_soc
clock clk
reset -expression {!rst_n}
load_upf lpv_pmu_soc_clean.upf
set t0 [clock milliseconds]
check_lp -stage formal -prove_corruption
set t1 [clock milliseconds]
report -task LowPower -csv -file lp_props.csv -force
set f0 [open lp.json w]; puts $f0 [report_lp -json]; close $f0
puts "FORMAL_MS=[expr {$t1 - $t0}]"
""",
            "formal_lpv",
        ),
        (
            "lpv_pmu_bad_seq",
            [
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w2_lpv_pmu_xleak/lpv_pmu_soc_bad_seq.sv",
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w2_lpv_pmu_xleak/lpv_pmu_soc_clean.upf",
            ],
            """
analyze -sv12 lpv_pmu_soc_bad_seq.sv
elaborate -top lpv_pmu_soc
clock clk
reset -expression {!rst_n}
load_upf lpv_pmu_soc_clean.upf
set t0 [clock milliseconds]
check_lp -stage formal -prove_corruption
set t1 [clock milliseconds]
report -task LowPower -csv -file lp_props.csv -force
set f0 [open lp.json w]; puts $f0 [report_lp -json]; close $f0
puts "FORMAL_MS=[expr {$t1 - $t0}]"
""",
            "formal_lpv",
        ),
        (
            "lpv_partial_ret_xleak",
            [
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w2_lpv_pmu_xleak/lpv_pmu_soc.sv",
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w2_lpv_pmu_xleak/lpv_pmu_soc_partial_ret.upf",
            ],
            """
analyze -sv12 lpv_pmu_soc.sv
elaborate -top lpv_pmu_soc
clock clk
reset -expression {!rst_n}
load_upf lpv_pmu_soc_partial_ret.upf
set t0 [clock milliseconds]
check_lp -stage formal -prove_corruption
set t1 [clock milliseconds]
report -task LowPower -csv -file lp_props.csv -force
set f0 [open lp.json w]; puts $f0 [report_lp -json]; close $f0
puts "FORMAL_MS=[expr {$t1 - $t0}]"
""",
            "formal_lpv",
        ),
        (
            "lp_sec_clean",
            [
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w4_lp_sec/golden_npg.sv",
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w4_lp_sec/pa_rtl.sv",
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w4_lp_sec/pa_rtl_clean.upf",
            ],
            """
analyze -sv12 pa_rtl.sv
elaborate -top pa_rtl
clock clk
reset -expression {!rst_n}
load_upf pa_rtl_clean.upf
check_sec -setup -spec golden_npg.sv -spec_top golden_npg -imp pa_rtl.sv -imp_top pa_rtl -clock clk -reset {!rst_n}
set t0 [clock milliseconds]
check_lp -sec
set t1 [clock milliseconds]
set f0 [open sec.json w]; puts $f0 [check_sec -report -json]; close $f0
puts "FORMAL_MS=[expr {$t1 - $t0}]"
""",
            "lp_sec",
        ),
        (
            "lp_sec_missing_ret_cex",
            [
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w4_lp_sec/golden_npg.sv",
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w4_lp_sec/pa_rtl.sv",
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w4_lp_sec/pa_rtl_buggy_ret.upf",
            ],
            """
analyze -sv12 pa_rtl.sv
elaborate -top pa_rtl
clock clk
reset -expression {!rst_n}
load_upf pa_rtl_buggy_ret.upf
check_sec -setup -spec golden_npg.sv -spec_top golden_npg -imp pa_rtl.sv -imp_top pa_rtl -clock clk -reset {!rst_n}
set t0 [clock milliseconds]
check_lp -sec
set t1 [clock milliseconds]
set f0 [open sec.json w]; puts $f0 [check_sec -report -json]; close $f0
puts "FORMAL_MS=[expr {$t1 - $t0}]"
""",
            "lp_sec",
        ),
    ]

    for name, files, tcl, flow in lpv_cases:
        for rep in range(1, repeats + 1):
            td = Path(tempfile.mkdtemp(prefix=f"exp3_{name}_"))
            try:
                for fp in files:
                    shutil.copy2(fp, td / fp.name)
                stdout, _stderr, wall_ms, rss_kb = run_shell(
                    bin_path, tcl, cwd=td, workdir=td / "work"
                )
                if flow == "formal_lpv":
                    proven, covered, cex = parse_csv_props(td / "lp_props.csv")
                    lp_rep = json.loads((td / "lp.json").read_text(encoding="utf-8"))
                    cex_rules = sorted(
                        {
                            v["rule_id"]
                            for v in lp_rep.get("violations", [])
                            if v.get("status") in ("cex", "formal_cex")
                        }
                    )
                else:
                    sec_rep = json.loads((td / "sec.json").read_text(encoding="utf-8"))
                    checks = sec_rep.get("checks", [])
                    proven = sum(1 for c in checks if c.get("status") == "proven")
                    covered = 0
                    cex = sum(
                        1
                        for c in checks
                        if c.get("status") == "cex"
                        or (isinstance(c.get("status"), dict) and "cex" in c["status"])
                    )
                    cex_rules = ["LP_SEC_MISMATCH"] if cex > 0 else []
                raw_runs.append(
                    {
                        "mode": "exp3_lpv",
                        "case": name,
                        "flow": flow,
                        "repeat": rep,
                        "proven": proven,
                        "covered": covered,
                        "cex": cex,
                        "cex_rules": ";".join(cex_rules),
                        "formal_ms": extract_marker_ms(stdout, "FORMAL_MS"),
                        "wall_ms": round(wall_ms, 2),
                        "peak_rss_kb": rss_kb,
                    }
                )
            finally:
                shutil.rmtree(td, ignore_errors=True)

    lpv_agg: list[dict[str, Any]] = []
    for name, _, _, flow in lpv_cases:
        grp = [r for r in raw_runs if r.get("mode") == "exp3_lpv" and r["case"] == name]
        f0 = grp[0]
        lpv_agg.append(
            {
                "case": name,
                "flow": flow,
                "proven": f0["proven"],
                "covered": f0["covered"],
                "cex": f0["cex"],
                "cex_rules": f0["cex_rules"],
                "formal_ms_med": round(statistics.median(x["formal_ms"] for x in grp), 2),
                "wall_ms_med": round(statistics.median(x["wall_ms"] for x in grp), 2),
                "peak_rss_kb_med": int(statistics.median(x["peak_rss_kb"] for x in grp)),
                "repeats": len(grp),
            }
        )

    # Sub-table 3B: why_cex causal slice compression
    why_cases = [
        (
            "std_mux_cex",
            [ROOT / "tests/lp_e2e/fixtures/f1_f6/f3_why_cex_mux.sv"],
            """
analyze -sv12 f3_why_cex_mux.sv
elaborate -top f3_why_cex_mux
clock clk
reset -expression {!rst_n}
assume -name a_arm0 {!sel_arm1}
assert -name a_no_err {!err_flag}
prove -all
set t0 [clock milliseconds]
set w [why_cex -json a_no_err]
set t1 [clock milliseconds]
set fh [open why.json w]; puts $fh $w; close $fh
puts "WHY_MS=[expr {$t1 - $t0}]"
""",
        ),
        (
            "xprop_2copy_cex",
            [ROOT / "tests/lp_e2e/fixtures/f1_f6/f3_xprop_dut.sv"],
            """
analyze -sv12 f3_xprop_dut.sv
elaborate -top f3_xprop_dut
clock clk
reset -expression {!rst_n}
check_xprop -init -targets {outputs} -prove
set t0 [clock milliseconds]
set w [why_cex -json f3_xprop_dut.leaky_out]
set t1 [clock milliseconds]
set fh [open why.json w]; puts $fh $w; close $fh
puts "WHY_MS=[expr {$t1 - $t0}]"
""",
        ),
        (
            "spv_2copy_cex",
            [ROOT / "tests/lp_e2e/fixtures/f1_f6/f3_spv_crypto.sv"],
            """
analyze -sv12 f3_spv_crypto.sv
elaborate -top f3_spv_crypto
clock clk
reset -expression {!rst_n}
check_spv -create -name leak_covert_2cyc -from secret_key -to covert_port_o
check_spv -prove
set t0 [clock milliseconds]
set w [why_cex -json leak_covert_2cyc]
set t1 [clock milliseconds]
set fh [open why.json w]; puts $fh $w; close $fh
puts "WHY_MS=[expr {$t1 - $t0}]"
""",
        ),
        (
            "lp_corrupt_2copy_cex",
            [
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w2_lpv_pmu_xleak/lpv_pmu_soc.sv",
                ROOT / "tests/lp_e2e/fixtures/workloads/t4_w2_lpv_pmu_xleak/lpv_pmu_soc_partial_ret.upf",
            ],
            """
analyze -sv12 lpv_pmu_soc.sv
elaborate -top lpv_pmu_soc
clock clk
reset -expression {!rst_n}
load_upf lpv_pmu_soc_partial_ret.upf
check_lp -stage formal -prove_corruption
set t0 [clock milliseconds]
set w [why_cex -json LowPower::lp_corrupt_dout]
set t1 [clock milliseconds]
set fh [open why.json w]; puts $fh $w; close $fh
puts "WHY_MS=[expr {$t1 - $t0}]"
""",
        ),
    ]

    for name, files, tcl in why_cases:
        for rep in range(1, repeats + 1):
            td = Path(tempfile.mkdtemp(prefix=f"exp3_why_{name}_"))
            try:
                for fp in files:
                    shutil.copy2(fp, td / fp.name)
                stdout, _stderr, wall_ms, rss_kb = run_shell(
                    bin_path, tcl, cwd=td, workdir=td / "work"
                )
                why = json.loads((td / "why.json").read_text(encoding="utf-8"))
                st_cnt = int(why.get("static_cone_signals", 0))
                evs = why.get("events", [])
                act_io_reg = len(
                    {e["signal"] for e in evs if e.get("kind") in ("input", "register")}
                )
                act_wires = len({e["signal"] for e in evs if e.get("kind") == "wire"})
                ratio = round(st_cnt / act_io_reg, 2) if act_io_reg > 0 else 0.0
                pruned_pct = round(100.0 * (1.0 - act_io_reg / st_cnt), 1) if st_cnt > 0 else 0.0
                raw_runs.append(
                    {
                        "mode": "exp3_why",
                        "case": name,
                        "repeat": rep,
                        "static_io_reg": st_cnt,
                        "active_io_reg": act_io_reg,
                        "io_reg_compression": ratio,
                        "pruned_pct": pruned_pct,
                        "active_wire_aliases": act_wires,
                        "total_frames": int(why.get("total_frames", 0)),
                        "events": len(evs),
                        "why_ms": extract_marker_ms(stdout, "WHY_MS"),
                        "wall_ms": round(wall_ms, 2),
                        "peak_rss_kb": rss_kb,
                    }
                )
            finally:
                shutil.rmtree(td, ignore_errors=True)

    why_agg: list[dict[str, Any]] = []
    for name, _, _ in why_cases:
        grp = [r for r in raw_runs if r.get("mode") == "exp3_why" and r["case"] == name]
        f0 = grp[0]
        why_agg.append(
            {
                "case": name,
                "static_io_reg": f0["static_io_reg"],
                "active_io_reg": f0["active_io_reg"],
                "io_reg_compression": f0["io_reg_compression"],
                "pruned_pct": f0["pruned_pct"],
                "active_wire_aliases": f0["active_wire_aliases"],
                "total_frames": f0["total_frames"],
                "events": f0["events"],
                "why_ms_med": round(statistics.median(x["why_ms"] for x in grp), 2),
                "peak_rss_kb_med": int(statistics.median(x["peak_rss_kb"] for x in grp)),
                "repeats": len(grp),
            }
        )

    return raw_runs, lpv_agg, why_agg


# ---------------------------------------------------------------------------
# Experiment 4: Streaming Liberty .lib + 1k..100k Gate/PG + LP-LEC
# ---------------------------------------------------------------------------


def generate_scaled_liberty(num_cells: int) -> str:
    """Builds a realistic multi-MB Liberty library with 7x7 NLDM tables per cell."""
    base = generate_mini_liberty()
    idx = base.rfind("}")
    header = base[:idx]
    tbl_delay = _nldm_7x7_values(0.025, 0.008, 0.011)
    tbl_power = _nldm_7x7_values(0.004, 0.001, 0.002)
    extra: list[str] = []
    for i in range(num_cells):
        extra.append(
            f"""
    cell (BENCH_BUF_X{i}) {{
        area : 1.5;
        pg_pin (VDD) {{ pg_type : primary_power; voltage_name : VDD_CORE; }}
        pg_pin (VSS) {{ pg_type : primary_ground; voltage_name : VSS; }}
        pin (A) {{ direction : input; related_power_pin : VDD; related_ground_pin : VSS; capacitance : 0.002; }}
        pin (Y) {{
            direction : output;
            function : "A";
            related_power_pin : VDD;
            related_ground_pin : VSS;
            timing () {{
                related_pin : "A";
                cell_rise (delay_template_7x7) {{
                    values ( \\
{tbl_delay}
                    );
                }}
                cell_fall (delay_template_7x7) {{
                    values ( \\
{tbl_delay}
                    );
                }}
            }}
            internal_power () {{
                related_pin : "A";
                rise_power (power_template_7x7) {{
                    values ( \\
{tbl_power}
                    );
                }}
                fall_power (power_template_7x7) {{
                    values ( \\
{tbl_power}
                    );
                }}
            }}
        }}
    }}
"""
        )
    return header + "".join(extra) + "\n}\n"


def run_exp4(
    bin_path: Path, repeats: int
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], list[dict[str, Any]], list[dict[str, Any]]]:
    raw_runs: list[dict[str, Any]] = []

    # 4A: Streaming Liberty .lib throughput across cell counts (0.3 MB .. 12 MB)
    lib_scales = [100, 1000, 4000]
    td_lib = Path(tempfile.mkdtemp(prefix="exp4_lib_"))
    try:
        for n_cells in lib_scales:
            lib_path = td_lib / f"scale_{n_cells}.lib"
            lib_txt = generate_scaled_liberty(n_cells)
            lib_path.write_text(lib_txt, encoding="utf-8")
            size_bytes = lib_path.stat().st_size
            size_mb = size_bytes / (1024.0 * 1024.0)
            tcl = f"""
set t0 [clock microseconds]
read_liberty {lib_path.name}
set t1 [clock microseconds]
puts "LIB_US=[expr {{$t1 - $t0}}]"
"""
            for rep in range(1, repeats + 1):
                stdout, _stderr, wall_ms, rss_kb = run_shell(
                    bin_path, tcl, cwd=td_lib, workdir=td_lib / f"w_{n_cells}_{rep}"
                )
                lib_ms = extract_marker_ms(stdout, "LIB_US") / 1000.0
                mb_s = (size_mb / (lib_ms / 1000.0)) if lib_ms > 0 else 0.0
                raw_runs.append(
                    {
                        "mode": "exp4_lib",
                        "cells": n_cells,
                        "repeat": rep,
                        "size_mb": round(size_mb, 3),
                        "parse_ms": round(lib_ms, 3),
                        "throughput_mb_s": round(mb_s, 2),
                        "wall_ms": round(wall_ms, 2),
                        "peak_rss_kb": rss_kb,
                    }
                )
    finally:
        shutil.rmtree(td_lib, ignore_errors=True)

    lib_agg: list[dict[str, Any]] = []
    for n_cells in lib_scales:
        grp = [r for r in raw_runs if r.get("mode") == "exp4_lib" and r["cells"] == n_cells]
        size_mb = grp[0]["size_mb"]
        p_med = round(statistics.median(x["parse_ms"] for x in grp), 3)
        mb_s = round(size_mb / (p_med / 1000.0), 2) if p_med > 0 else 0.0
        lib_agg.append(
            {
                "cells": n_cells,
                "size_mb": size_mb,
                "parse_ms_med": p_med,
                "throughput_mb_s": mb_s,
                "peak_rss_kb_med": int(statistics.median(x["peak_rss_kb"] for x in grp)),
                "repeats": len(grp),
            }
        )

    # 4B: Gate/PG Stage 3-4 scaling (1k, 10k, 50k, 100k clean + 100k buggy)
    pg_scales = [
        ("gate_1k_clean", 1_000, False),
        ("gate_10k_clean", 10_000, False),
        ("gate_50k_clean", 50_000, False),
        ("gate_100k_clean", 100_000, False),
        ("gate_100k_buggy", 100_000, True),
    ]
    td_pg = Path(tempfile.mkdtemp(prefix="exp4_pg_"))
    try:
        (td_pg / "mini.lib").write_text(generate_mini_liberty(), encoding="utf-8")
        (td_pg / "gate_pg.upf").write_text(generate_gate_pg_upf(), encoding="utf-8")
        for label, n_cells, bugs in pg_scales:
            v_path = td_pg / f"{label}.v"
            v_path.write_text(
                generate_gate_pg_netlist(num_cells=n_cells, include_bugs=bugs), encoding="utf-8"
            )
            v_mb = v_path.stat().st_size / (1024.0 * 1024.0)
            tcl = f"""
set t0 [clock milliseconds]
read_liberty mini.lib
analyze -sv {v_path.name}
elaborate -top soc_pg_top
set t1 [clock milliseconds]
load_upf gate_pg.upf
check_lp -stage netlist
set f3 [open s3.json w]; puts $f3 [report_lp -json]; close $f3
check_lp -stage pg
set t2 [clock milliseconds]
set f4 [open s4.json w]; puts $f4 [report_lp -json]; close $f4
puts "ELAB_MS=[expr {{$t1 - $t0}}]"
puts "CHECK_MS=[expr {{$t2 - $t1}}]"
puts "TOTAL_MS=[expr {{$t2 - $t0}}]"
"""
            for rep in range(1, repeats + 1):
                stdout, _stderr, wall_ms, rss_kb = run_shell(
                    bin_path, tcl, cwd=td_pg, workdir=td_pg / f"w_{label}_{rep}"
                )
                s3 = json.loads((td_pg / "s3.json").read_text(encoding="utf-8"))
                s4 = json.loads((td_pg / "s4.json").read_text(encoding="utf-8"))
                viols = s3.get("violations", []) + s4.get("violations", [])
                open_v = [v for v in viols if v.get("status") == "open"]
                tot_ms = extract_marker_ms(stdout, "TOTAL_MS")
                cps = round(n_cells / (tot_ms / 1000.0), 1) if tot_ms > 0 else 0.0
                raw_runs.append(
                    {
                        "mode": "exp4_pg",
                        "case": label,
                        "cells": n_cells,
                        "netlist_mb": round(v_mb, 2),
                        "repeat": rep,
                        "elab_ms": extract_marker_ms(stdout, "ELAB_MS"),
                        "check_ms": extract_marker_ms(stdout, "CHECK_MS"),
                        "total_ms": tot_ms,
                        "cells_per_sec": cps,
                        "wall_ms": round(wall_ms, 2),
                        "open_violations": len(open_v),
                        "peak_rss_kb": rss_kb,
                    }
                )
    finally:
        shutil.rmtree(td_pg, ignore_errors=True)

    pg_agg: list[dict[str, Any]] = []
    for label, n_cells, _ in pg_scales:
        grp = [r for r in raw_runs if r.get("mode") == "exp4_pg" and r["case"] == label]
        tot_med = round(statistics.median(x["total_ms"] for x in grp), 2)
        cps = round(n_cells / (tot_med / 1000.0), 1) if tot_med > 0 else 0.0
        pg_agg.append(
            {
                "case": label,
                "cells": n_cells,
                "netlist_mb": grp[0]["netlist_mb"],
                "elab_ms_med": round(statistics.median(x["elab_ms"] for x in grp), 2),
                "check_ms_med": round(statistics.median(x["check_ms"] for x in grp), 2),
                "total_ms_med": tot_med,
                "cells_per_sec": cps,
                "open_violations": grp[0]["open_violations"],
                "peak_rss_kb_med": int(statistics.median(x["peak_rss_kb"] for x in grp)),
                "repeats": len(grp),
            }
        )

    # 4C: LP-LEC (clean vs mutated ISO cell)
    td_lec = Path(tempfile.mkdtemp(prefix="exp4_lec_"))
    try:
        shutil.copy2(
            ROOT / "tests/lp_e2e/fixtures/workloads/t4_w5_gate_pg_lec/rtl_golden.sv",
            td_lec / "rtl_golden.sv",
        )
        (td_lec / "mini.lib").write_text(generate_mini_liberty(), encoding="utf-8")
        (td_lec / "stdcells.sv").write_text(generate_stdcell_verilog(), encoding="utf-8")
        (td_lec / "gate_pg.upf").write_text(generate_gate_pg_upf(), encoding="utf-8")
        clean_100 = generate_gate_pg_netlist(num_cells=100, include_bugs=False)
        (td_lec / "gate_100_clean.v").write_text(clean_100, encoding="utf-8")
        bad_iso_100 = clean_100.replace(
            "ISO_LO_X1 u_iso (.A(core_sig), .ISO(aon_iso), .Y(iso_sig), .VDD_BK(VDD_BK), .VSS(VSS));",
            "INV_X1    u_iso (.A(core_sig), .Y(iso_sig), .VDD(VDD_AON), .VSS(VSS));",
        )
        (td_lec / "gate_100_bad_iso.v").write_text(bad_iso_100, encoding="utf-8")
        for label, imp_v in [
            ("lp_lec_clean", "gate_100_clean.v"),
            ("lp_lec_mutated_iso", "gate_100_bad_iso.v"),
        ]:
            tcl = f"""
read_liberty mini.lib
analyze -sv12 rtl_golden.sv
elaborate -top soc_pg_top
clock clk
reset -expression {{!rst_n}}
load_upf gate_pg.upf
check_sec -setup -lec -spec rtl_golden.sv -spec_top soc_pg_top -imp {{stdcells.sv {imp_v}}} -imp_top soc_pg_top -clock clk -reset {{!rst_n}}
set t0 [clock milliseconds]
check_lp -lec
set t1 [clock milliseconds]
set f0 [open lec.json w]; puts $f0 [check_sec -report -json]; close $f0
puts "LEC_MS=[expr {{$t1 - $t0}}]"
"""
            for rep in range(1, repeats + 1):
                stdout, _stderr, wall_ms, rss_kb = run_shell(
                    bin_path, tcl, cwd=td_lec, workdir=td_lec / f"w_{label}_{rep}"
                )
                checks = json.loads((td_lec / "lec.json").read_text(encoding="utf-8")).get(
                    "checks", []
                )
                proven = sum(1 for c in checks if c.get("status") == "proven")
                cex = sum(
                    1
                    for c in checks
                    if c.get("status") == "cex"
                    or (isinstance(c.get("status"), dict) and "cex" in c["status"])
                )
                raw_runs.append(
                    {
                        "mode": "exp4_lec",
                        "case": label,
                        "repeat": rep,
                        "proven": proven,
                        "cex": cex,
                        "lec_ms": extract_marker_ms(stdout, "LEC_MS"),
                        "wall_ms": round(wall_ms, 2),
                        "peak_rss_kb": rss_kb,
                    }
                )
    finally:
        shutil.rmtree(td_lec, ignore_errors=True)

    lec_agg: list[dict[str, Any]] = []
    for label in ("lp_lec_clean", "lp_lec_mutated_iso"):
        grp = [r for r in raw_runs if r.get("mode") == "exp4_lec" and r["case"] == label]
        lec_agg.append(
            {
                "case": label,
                "proven": grp[0]["proven"],
                "cex": grp[0]["cex"],
                "lec_ms_med": round(statistics.median(x["lec_ms"] for x in grp), 2),
                "wall_ms_med": round(statistics.median(x["wall_ms"] for x in grp), 2),
                "peak_rss_kb_med": int(statistics.median(x["peak_rss_kb"] for x in grp)),
                "repeats": len(grp),
            }
        )

    return raw_runs, lib_agg, pg_agg, lec_agg


def write_summary_md(
    exp2: list[dict[str, Any]],
    exp3_lpv: list[dict[str, Any]],
    exp3_why: list[dict[str, Any]],
    exp4_lib: list[dict[str, Any]],
    exp4_pg: list[dict[str, Any]],
    exp4_lec: list[dict[str, Any]],
    out_md: Path,
) -> None:
    lines = ["# Paper Experiments 2–4 Summary Tables (Measured, Release Build)", ""]
    if exp2:
        lines.extend(
            [
                "## Experiment 2: Certified Auto-Waivers & Soundness",
                "",
                "| Case | Kind | Static Open | Formally Waived | Post RTL Open | Post RTL CEX | AAG CEX | FP Reduction | Static (ms) | Auto-Waive (ms) | Wall (ms) | Peak RSS (KiB) |",
                "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
            ]
        )
        for r in exp2:
            lines.append(
                f"| {r['case']} | {r['kind']} | {r['static_open_errors']} | {r['formally_waived']} | "
                f"{r['post_rtl_open']} | {r['post_rtl_cex']} | {r['aag_formal_cex']} | {r['fp_reduction_pct']:.1f}% | "
                f"{r['static_ms_med']:.1f} | {r['waive_ms_med']:.1f} | {r['wall_ms_med']:.1f} | {r['peak_rss_kb_med']} |"
            )
        lines.append("")
    if exp3_lpv:
        lines.extend(
            [
                "## Experiment 3A: Formal LPV Bug-Finding & LP-SEC",
                "",
                "| Case | Flow | Proven | Covered | CEX | CEX Rules | Formal (ms) | Wall (ms) | Peak RSS (KiB) |",
                "|---|---|---:|---:|---:|---|---:|---:|---:|",
            ]
        )
        for r in exp3_lpv:
            lines.append(
                f"| {r['case']} | {r['flow']} | {r['proven']} | {r['covered']} | {r['cex']} | "
                f"{r['cex_rules'] or '-'} | {r['formal_ms_med']:.1f} | {r['wall_ms_med']:.1f} | {r['peak_rss_kb_med']} |"
            )
        lines.append("")
    if exp3_why:
        lines.extend(
            [
                "## Experiment 3B: `why_cex` Causal Slice Compression (Inputs + Registers)",
                "",
                "| Case | Static COI (IO+Reg) | Active Slice (IO+Reg) | Compression | Pruned | Active Wire Aliases | Frames | Causal Events | `why_cex` (ms) | Peak RSS (KiB) |",
                "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
            ]
        )
        for r in exp3_why:
            lines.append(
                f"| {r['case']} | {r['static_io_reg']} | {r['active_io_reg']} | "
                f"{r['io_reg_compression']:.2f}x | {r['pruned_pct']:.1f}% | {r['active_wire_aliases']} | "
                f"{r['total_frames']} | {r['events']} | "
                f"{r['why_ms_med']:.1f} | {r['peak_rss_kb_med']} |"
            )
        lines.append("")
    if exp4_lib:
        lines.extend(
            [
                "## Experiment 4A: Streaming Liberty `.lib` Parsing",
                "",
                "| Liberty Cells | Size (MiB) | Parse (ms) | Throughput (MiB/s) | Peak RSS (KiB) |",
                "|---:|---:|---:|---:|---:|",
            ]
        )
        for r in exp4_lib:
            lines.append(
                f"| {r['cells']} | {r['size_mb']:.3f} | {r['parse_ms_med']:.2f} | "
                f"{r['throughput_mb_s']:.2f} | {r['peak_rss_kb_med']} |"
            )
        lines.append("")
    if exp4_pg:
        lines.extend(
            [
                "## Experiment 4B: Gate & PG Netlist Fast-Path Scaling (Stages 3–4)",
                "",
                "| Case | Cells | Netlist (MiB) | Elab (ms) | Stage 3–4 (ms) | Total (ms) | Throughput (cells/s) | Open Findings | Peak RSS (KiB) |",
                "|---|---:|---:|---:|---:|---:|---:|---:|---:|",
            ]
        )
        for r in exp4_pg:
            lines.append(
                f"| {r['case']} | {r['cells']} | {r['netlist_mb']:.2f} | {r['elab_ms_med']:.1f} | "
                f"{r['check_ms_med']:.1f} | {r['total_ms_med']:.1f} | {r['cells_per_sec']:.1f} | "
                f"{r['open_violations']} | {r['peak_rss_kb_med']} |"
            )
        lines.append("")
    if exp4_lec:
        lines.extend(
            [
                "## Experiment 4C: Post-Synthesis LP-LEC",
                "",
                "| Case | Proven | CEX | LEC (ms) | Wall (ms) | Peak RSS (KiB) |",
                "|---|---:|---:|---:|---:|---:|",
            ]
        )
        for r in exp4_lec:
            lines.append(
                f"| {r['case']} | {r['proven']} | {r['cex']} | {r['lec_ms_med']:.1f} | "
                f"{r['wall_ms_med']:.1f} | {r['peak_rss_kb_med']} |"
            )
        lines.append("")
    out_md.write_text("\n".join(lines), encoding="utf-8")


def main() -> None:
    ap = argparse.ArgumentParser(description="Run Paper Experiments 2, 3, and 4")
    ap.add_argument("--exp", choices=["2", "3", "4", "all"], default="all")
    ap.add_argument("--repeats", type=int, default=3)
    ap.add_argument("--out", type=Path, default=Path("research/lp-paper/measurements"))
    ap.add_argument("--bin", type=Path, default=None)
    args = ap.parse_args()

    out_dir = args.out if args.out.is_absolute() else (ROOT / args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "env_exp2_4.json").write_text(
        json.dumps(collect_env(), indent=2) + "\n", encoding="utf-8"
    )

    bin_path = args.bin.resolve() if args.bin else build_release_cli()
    all_runs: list[dict[str, Any]] = []
    exp2_agg: list[dict[str, Any]] = []
    exp3_lpv: list[dict[str, Any]] = []
    exp3_why: list[dict[str, Any]] = []
    exp4_lib: list[dict[str, Any]] = []
    exp4_pg: list[dict[str, Any]] = []
    exp4_lec: list[dict[str, Any]] = []

    if args.exp in ("2", "all"):
        r2, exp2_agg = run_exp2(bin_path, args.repeats)
        all_runs.extend(r2)
        write_csv(exp2_agg, out_dir / "exp2_autowaiver.csv")

    if args.exp in ("3", "all"):
        r3, exp3_lpv, exp3_why = run_exp3(bin_path, args.repeats)
        all_runs.extend(r3)
        write_csv(exp3_lpv, out_dir / "exp3_formal_lpv.csv")
        write_csv(exp3_why, out_dir / "exp3_why_cex.csv")

    if args.exp in ("4", "all"):
        r4, exp4_lib, exp4_pg, exp4_lec = run_exp4(bin_path, args.repeats)
        all_runs.extend(r4)
        write_csv(exp4_lib, out_dir / "exp4_liberty.csv")
        write_csv(exp4_pg, out_dir / "exp4_gate_pg.csv")
        write_csv(exp4_lec, out_dir / "exp4_lp_lec.csv")

    runs_path = out_dir / "exp2_4_runs.jsonl"
    runs_path.write_text("".join(json.dumps(r) + "\n" for r in all_runs), encoding="utf-8")
    summary_path = out_dir / "exp2_4_summary.md"
    write_summary_md(exp2_agg, exp3_lpv, exp3_why, exp4_lib, exp4_pg, exp4_lec, summary_path)
    print(f"Wrote {len(all_runs)} runs to {runs_path} and summary to {summary_path}")
    print(summary_path.read_text(encoding="utf-8"))


if __name__ == "__main__":
    main()
