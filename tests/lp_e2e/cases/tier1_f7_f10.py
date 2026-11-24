"""Tier 1 (happy-path) opaque-box E2E test cases for Feature Areas F7-F10."""

from __future__ import annotations

import csv
import io
import json
from pathlib import Path
import re
import socket
import time
from typing import Any

from lib.cli_client import CliResult, run_command
from lib.fixtures_gen import (
    generate_gate_pg_netlist,
    generate_gate_pg_upf,
    generate_mini_liberty,
    generate_stdcell_verilog,
)
from lib.harness import E2EContext, TestCase, assert_eq, assert_exit, assert_in
from lib.vcd_check import (
    has_lp_domain_signals,
    lp_domain_names,
    parse_vcd_file,
    signals_diverge,
)


def _extract_block(text: str, tag: str) -> str:
    begin = f"==={tag}_BEGIN==="
    end = f"==={tag}_END==="
    start_idx = text.find(begin)
    if start_idx < 0:
        raise AssertionError(f"Missing start marker {begin!r} in output:\n{text}")
    body_start = start_idx + len(begin)
    end_idx = text.find(end, body_start)
    if end_idx < 0:
        raise AssertionError(f"Missing end marker {end!r} in output:\n{text}")
    return text[body_start:end_idx].strip()


def _parse_json_block(text: str, tag: str) -> Any:
    raw = _extract_block(text, tag)
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        raise AssertionError(f"Invalid JSON in {tag} block: {exc}\n{raw}") from exc


def _parse_csv_block(text: str, tag: str) -> list[dict[str, str]]:
    raw = _extract_block(text, tag)
    lines = [ln for ln in raw.splitlines() if ln.strip()]
    header: list[str] | None = None
    data_lines: list[str] = []
    for ln in lines:
        parsed = next(csv.reader([ln]))
        cols = [c.strip() for c in parsed]
        if header is None:
            if "property" in cols and "status" in cols:
                header = cols
            continue
        if cols == header:
            continue
        data_lines.append(ln)
    if header is None:
        raise AssertionError(
            f"Unexpected CSV header (missing 'property'/'status') in {tag} block:\n{raw}"
        )
    reader = csv.DictReader(io.StringIO("\n".join(data_lines)), fieldnames=header)
    rows: list[dict[str, str]] = []
    seen: set[tuple[tuple[str, str], ...]] = set()
    for row in reader:
        cleaned = {k: (v or "").strip() for k, v in row.items() if k is not None}
        key = tuple(sorted(cleaned.items()))
        if key not in seen:
            seen.add(key)
            rows.append(cleaned)
    return rows


def _lp_violations(payload: Any) -> list[dict[str, Any]]:
    if isinstance(payload, list):
        return [v for v in payload if isinstance(v, dict)]
    if isinstance(payload, dict):
        items = payload.get("violations")
        if items is None:
            items = payload.get("items")
        if isinstance(items, list):
            return [v for v in items if isinstance(v, dict)]
    raise AssertionError(f"Expected LP report payload with violations/items list, got {payload!r}")


def _norm_status(raw: Any) -> str:
    if isinstance(raw, dict):
        if len(raw) == 1:
            raw = next(iter(raw.keys()))
        elif "status" in raw:
            raw = raw["status"]
    s = str(raw or "").strip().lower().replace("-", "_").replace(" ", "_")
    aliases = {
        "formallywaived": "formally_waived",
        "formalcex": "cex",
        "formal_cex": "cex",
    }
    return aliases.get(s, s)


def _norm_rule(item: dict[str, Any]) -> str:
    return str(item.get("rule_id") or item.get("rule") or item.get("id") or "").strip()


def _assert_no_open_or_cex_lp(payload: Any, context: str) -> None:
    violations = _lp_violations(payload)
    bad = [
        v
        for v in violations
        if _norm_status(v.get("status")) in {"open", "cex", "error"}
    ]
    if bad:
        raise AssertionError(f"{context}: expected zero open/cex/error LP violations, got {bad!r}")


def _resolve_workdir_file(shell_workdir: Path, raw_path: str) -> Path:
    p = Path(raw_path)
    return p if p.is_absolute() else (shell_workdir / p)


def _find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def _ui_env(ctx: E2EContext, **extra: str) -> dict[str, str]:
    env = ctx.cli_env(**extra)
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    ver = run_command(
        ["node", "-p", "process.versions.node.split('.')[0]"],
        cwd=ctx.workdir,
        env=env,
        timeout_s=10.0,
    )
    if ver.returncode == 0 and ver.stdout.strip().isdigit() and int(ver.stdout.strip()) >= 25:
        ls_path = ctx.path("vitest_localstorage")
        existing = env.get("NODE_OPTIONS", "").strip()
        flag = f"--localstorage-file={ls_path}"
        env["NODE_OPTIONS"] = f"{existing} {flag}".strip()
    return env


def _stage3_4_error_count(violations: list[dict[str, Any]], rules: set[str]) -> int:
    count = 0
    for v in violations:
        rid = _norm_rule(v)
        st = _norm_status(v.get("status"))
        if rid in rules and st in {"open", "cex", "error"}:
            count += 1
    return count


# ---------------------------------------------------------------------------
# Feature Area F7: Power-Aware Formal LPV, LowPower AAG & check_lpv
# ---------------------------------------------------------------------------


def _case_f7_t1_01(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f7/soc_lp_clean.sv")
    upf = ctx.copy_fixture("f7_f10/f7/soc_lp_clean.upf")
    tcl = ctx.write_file(
        "run.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top soc_lp_clean
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage formal
puts "===LOWPOWER_CSV_BEGIN==="
report -csv -task LowPower
puts "===LOWPOWER_CSV_END==="
puts "===LP_REPORT_BEGIN==="
puts [report_lp -json]
puts "===LP_REPORT_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=90.0)
    assert_exit(res, 0, "F7_T1_01 check_lp -stage formal")

    rows = _parse_csv_block(res.stdout, "LOWPOWER_CSV")
    asserts = [r for r in rows if r.get("type", "").lower() == "assert"]
    if len(asserts) < 5:
        raise AssertionError(f"Expected >= 5 LowPower assertions, got {len(asserts)}: {rows!r}")

    names = [r["property"] for r in asserts]
    for prefix in (
        "lp_iso_before_pwroff_",
        "lp_iso_hold_after_pwron_",
        "lp_ret_save_order_",
        "lp_ret_restore_order_",
        "lp_ret_save_restore_mutex_",
        "lp_ret_async_rst_clean_",
    ):
        if not any(prefix in n for n in names):
            raise AssertionError(f"Missing expected AAG assertion prefix {prefix!r} in {names!r}")

    not_proven = [r for r in asserts if _norm_status(r.get("status")) != "proven"]
    if not_proven:
        raise AssertionError(f"Expected all LowPower assertions proven, got {not_proven!r}")

    lp_json = _parse_json_block(res.stdout, "LP_REPORT")
    _assert_no_open_or_cex_lp(lp_json, "F7_T1_01")


def _case_f7_t1_02(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f7/soc_lp_clean.sv")
    upf = ctx.copy_fixture("f7_f10/f7/soc_lp_clean.upf")
    tcl = ctx.write_file(
        "run_lpv.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top soc_lp_clean
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
check_lpv -timeout 25s
puts "===LPV_CSV_BEGIN==="
report -csv -task LowPower
puts "===LPV_CSV_END==="
""".strip()
        + "\n",
    )
    chk = ctx.run_shell_check(tcl, json_out=True, timeout_s=30.0)
    assert_exit(chk, 0, "F7_T1_02 check_lpv --check")
    chk_json = chk.json()
    if chk_json.get("findings") != []:
        raise AssertionError(f"Expected clean --check audit (findings == []) for check_lpv script, got {chk_json!r}")
    cmds = chk_json.get("commands")
    if not isinstance(cmds, dict) or not cmds:
        raise AssertionError(f"Expected non-empty commands map in --check output, got {chk_json!r}")
    rejected = {k: v for k, v in cmds.items() if isinstance(v, dict) and v.get("rejected", 0) != 0}
    if rejected:
        raise AssertionError(f"Expected zero rejected commands in --check output, got {rejected!r}")
    if "check_lpv" not in cmds or cmds["check_lpv"].get("honoured", 0) < 1:
        raise AssertionError(f"Expected check_lpv honoured >= 1, got {cmds!r}")

    res = ctx.run_shell_file(tcl, timeout_s=90.0)
    assert_exit(res, 0, "F7_T1_02 check_lpv execution")
    rows = _parse_csv_block(res.stdout, "LPV_CSV")
    asserts = [r for r in rows if r.get("type", "").lower() == "assert"]
    if len(asserts) < 5:
        raise AssertionError(f"Expected >= 5 LowPower assertions via check_lpv, got {rows!r}")
    bad = [r for r in asserts if _norm_status(r.get("status")) != "proven"]
    if bad:
        raise AssertionError(f"Expected all check_lpv assertions proven, got {bad!r}")


def _case_f7_t1_03(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f7/pst_reach_soc.sv")
    upf = ctx.copy_fixture("f7_f10/f7/pst_reach_soc.upf")
    tcl = ctx.write_file(
        "run_pst.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top pst_reach_soc
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage formal
puts "===PST_CSV_BEGIN==="
report -csv -task LowPower
puts "===PST_CSV_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=90.0)
    assert_exit(res, 0, "F7_T1_03 PST cover & safety")
    rows = _parse_csv_block(res.stdout, "PST_CSV")

    pst_covers = [r for r in rows if "lp_pst_reach_" in r.get("property", "")]
    if len(pst_covers) < 2:
        raise AssertionError(f"Expected >= 2 lp_pst_reach_* cover properties, got {rows!r}")
    uncovered = [r for r in pst_covers if _norm_status(r.get("status")) != "covered"]
    if uncovered:
        raise AssertionError(f"Expected all lp_pst_reach_* covers covered, got {uncovered!r}")

    illegal_asserts = [r for r in rows if "lp_pst_no_illegal_" in r.get("property", "")]
    if not illegal_asserts:
        raise AssertionError(f"Expected lp_pst_no_illegal_* assertion in {rows!r}")
    bad_illegal = [r for r in illegal_asserts if _norm_status(r.get("status")) != "proven"]
    if bad_illegal:
        raise AssertionError(f"Expected lp_pst_no_illegal_* proven, got {bad_illegal!r}")


def _case_f7_t1_04(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f7/iso_clamp_modes.sv")
    upf = ctx.copy_fixture("f7_f10/f7/iso_clamp_modes.upf")
    tcl = ctx.write_file(
        "run_iso_modes.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top iso_clamp_modes
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage formal -prove_corruption
puts "===ISO_CSV_BEGIN==="
report -csv -task LowPower
puts "===ISO_CSV_END==="
puts "===ISO_LP_BEGIN==="
puts [report_lp -json]
puts "===ISO_LP_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=90.0)
    assert_exit(res, 0, "F7_T1_04 ISO clamp 0/1/latch")

    rows = _parse_csv_block(res.stdout, "ISO_CSV")
    asserts = [r for r in rows if r.get("type", "").lower() == "assert"]
    if not asserts:
        raise AssertionError(f"Expected LowPower assertions for iso_clamp_modes, got {rows!r}")
    bad = [r for r in asserts if _norm_status(r.get("status")) != "proven"]
    if bad:
        raise AssertionError(f"Expected all ISO clamp 0/1/latch assertions proven, got {bad!r}")

    lp_json = _parse_json_block(res.stdout, "ISO_LP")
    _assert_no_open_or_cex_lp(lp_json, "F7_T1_04")


def _case_f7_t1_05(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f7/soc_lp_clean.sv")
    upf = ctx.copy_fixture("f7_f10/f7/soc_lp_clean.upf")
    tcl = ctx.write_file(
        "run_ret.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top soc_lp_clean
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage formal
puts "===RET_CSV_BEGIN==="
report -csv -task LowPower
puts "===RET_CSV_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=90.0)
    assert_exit(res, 0, "F7_T1_05 retention preservation")

    rows = _parse_csv_block(res.stdout, "RET_CSV")
    ret_pres = [
        r
        for r in rows
        if "lp_ret_value_preserved_" in r.get("property", "") and "cfg_q" in r.get("property", "")
    ]
    if not ret_pres:
        raise AssertionError(f"Expected lp_ret_value_preserved_*cfg_q* in {rows!r}")
    for row in ret_pres:
        assert_eq(_norm_status(row.get("status")), "proven", f"retention prop {row['property']}")

    for prefix in ("lp_ret_save_order_", "lp_ret_restore_order_"):
        matching = [r for r in rows if prefix in r.get("property", "")]
        if not matching:
            raise AssertionError(f"Missing {prefix}* in {rows!r}")
        for row in matching:
            assert_eq(_norm_status(row.get("status")), "proven", f"order prop {row['property']}")


def _case_f7_t1_06(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f7/simstate_coa_clean.sv")
    upf = ctx.copy_fixture("f7_f10/f7/simstate_coa_clean.upf")
    tcl = ctx.write_file(
        "run_coa.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top simstate_coa_clean
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage formal -prove_corruption
puts "===COA_CSV_BEGIN==="
report -csv -task LowPower
puts "===COA_CSV_END==="
puts "===COA_LP_BEGIN==="
puts [report_lp -json]
puts "===COA_LP_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=90.0)
    assert_exit(res, 0, "F7_T1_06 COA/CSOA idle clean")

    rows = _parse_csv_block(res.stdout, "COA_CSV")
    bad = [
        r
        for r in rows
        if r.get("type", "").lower() == "assert" and _norm_status(r.get("status")) != "proven"
    ]
    if bad:
        raise AssertionError(f"Expected all COA/CSOA assertions proven, got {bad!r}")

    lp_json = _parse_json_block(res.stdout, "COA_LP")
    _assert_no_open_or_cex_lp(lp_json, "F7_T1_06")


# ---------------------------------------------------------------------------
# Feature Area F8: 2-Copy Corruption Proofs, Certified Auto-Waivers & LP-SEC
# ---------------------------------------------------------------------------


def _case_f8_t1_01(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f8/pa_clean_soc.sv")
    upf = ctx.copy_fixture("f7_f10/f8/pa_clean_soc.upf")
    tcl = ctx.write_file(
        "run_f8_clean.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top pa_clean_soc
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage formal -prove_corruption
puts "===F8_CLEAN_CSV_BEGIN==="
report -csv -task LowPower
puts "===F8_CLEAN_CSV_END==="
puts "===F8_CLEAN_LP_BEGIN==="
puts [report_lp -json]
puts "===F8_CLEAN_LP_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=90.0)
    assert_exit(res, 0, "F8_T1_01 -prove_corruption clean")

    rows = _parse_csv_block(res.stdout, "F8_CLEAN_CSV")
    asserts = [r for r in rows if r.get("type", "").lower() == "assert"]
    if not asserts:
        raise AssertionError(f"Expected LowPower assertions in pa_clean_soc, got {rows!r}")
    bad = [r for r in asserts if _norm_status(r.get("status")) != "proven"]
    if bad:
        raise AssertionError(f"Expected all corruption/LP assertions proven, got {bad!r}")

    lp_json = _parse_json_block(res.stdout, "F8_CLEAN_LP")
    _assert_no_open_or_cex_lp(lp_json, "F8_T1_01")


def _case_f8_t1_02(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f8/masked_crossing.sv")
    upf = ctx.copy_fixture("f7_f10/f8/masked_crossing.upf")
    tcl = ctx.write_file(
        "run_waive.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top masked_crossing
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage rtl
puts "===OPEN_BEFORE_BEGIN==="
puts [report_lp -status open -json]
puts "===OPEN_BEFORE_END==="
check_lp -prove_waivers
puts "===WAIVED_AFTER_BEGIN==="
puts [report_lp -status formally_waived -json]
puts "===WAIVED_AFTER_END==="
puts "===OPEN_AFTER_BEGIN==="
puts [report_lp -status open -json]
puts "===OPEN_AFTER_END==="
puts "===SUMMARY_BEGIN==="
puts [report_lp -summary]
puts "===SUMMARY_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=90.0)
    assert_exit(res, 0, "F8_T1_02 -prove_waivers")

    open_before = _lp_violations(_parse_json_block(res.stdout, "OPEN_BEFORE"))
    if not any("ISO_MISSING" in _norm_rule(v) for v in open_before):
        raise AssertionError(f"Expected initial ISO_MISSING in open violations, got {open_before!r}")

    waived_after = _lp_violations(_parse_json_block(res.stdout, "WAIVED_AFTER"))
    if not any(
        "ISO_MISSING" in _norm_rule(v) and _norm_status(v.get("status")) == "formally_waived"
        for v in waived_after
    ):
        raise AssertionError(f"Expected ISO_MISSING FormallyWaived after -prove_waivers, got {waived_after!r}")

    open_after = _lp_violations(_parse_json_block(res.stdout, "OPEN_AFTER"))
    if open_after:
        raise AssertionError(f"Expected 0 open violations after auto-waiving masked crossing, got {open_after!r}")

    summary = _extract_block(res.stdout, "SUMMARY")
    if "Formally Waived" not in summary and "formally_waived" not in summary.lower():
        raise AssertionError(f"Expected summary to report Formally Waived count, got:\n{summary}")


def _case_f8_t1_03(ctx: E2EContext) -> None:
    golden = ctx.copy_fixture("f7_f10/f8/golden_soc.sv")
    pa_sv = ctx.copy_fixture("f7_f10/f8/pa_soc.sv")
    pa_upf = ctx.copy_fixture("f7_f10/f8/pa_soc.upf")
    tcl = ctx.write_file(
        "run_lp_sec.tcl",
        f"""
clear -all
analyze -sv12 {pa_sv.name}
elaborate -top pa_soc
clock clk
reset -expression {{!rst_n}}
load_upf {pa_upf.name}
set_prove_time_limit 30s
check_sec -setup -spec {golden.name} -spec_top golden_soc -imp {pa_sv.name} -imp_top pa_soc -clock clk -reset {{!rst_n}}
check_lp -sec
puts "===SEC_JSON_BEGIN==="
puts [check_sec -report -json]
puts "===SEC_JSON_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=120.0)
    assert_exit(res, 0, "F8_T1_03 check_lp -sec")

    sec_state = _parse_json_block(res.stdout, "SEC_JSON")
    checks = sec_state.get("checks") or []
    if not checks:
        raise AssertionError(f"Expected non-empty checks[] from check_sec -report -json, got {sec_state!r}")

    by_name = {c.get("name"): c for c in checks if isinstance(c, dict)}
    for req_check in ("sec_out_dout", "sec_out_valid_o"):
        if req_check not in by_name:
            raise AssertionError(f"Missing {req_check!r} in SEC checks: {list(by_name.keys())!r}")
    for c in checks:
        assert_eq(_norm_status(c.get("status")), "proven", f"LP-SEC check {c.get('name')}")


def _case_f8_t1_04(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f8/unmasked_leak.sv")
    upf = ctx.copy_fixture("f7_f10/f8/unmasked_leak.upf")
    shell_workdir = ctx.path("shell_work")
    tcl = ctx.write_file(
        "run_leak_vcd.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top unmasked_leak
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage formal -prove_corruption
puts "===CEX_LP_BEGIN==="
puts [report_lp -status cex -json]
puts "===CEX_LP_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, workdir=shell_workdir, timeout_s=90.0)
    assert_exit(res, 0, "F8_T1_04 unmasked leak VCD")

    cex_items = _lp_violations(_parse_json_block(res.stdout, "CEX_LP"))
    if not cex_items:
        raise AssertionError("Expected >= 1 CEX violation on unmasked_leak")

    leaks = [
        v
        for v in cex_items
        if "ISO_CORRUPTION_LEAK" in _norm_rule(v) or "ISO_MISSING" in _norm_rule(v)
    ]
    if not leaks:
        raise AssertionError(f"Expected ISO_CORRUPTION_LEAK CEX violation, got {cex_items!r}")

    trace_raw = next((str(v.get("trace")) for v in leaks if v.get("trace")), "")
    if not trace_raw:
        raise AssertionError(f"Expected non-empty trace path on CEX violation: {leaks!r}")
    vcd_path = _resolve_workdir_file(shell_workdir, trace_raw)
    if not vcd_path.is_file():
        raise AssertionError(f"Expected VCD trace file at {vcd_path}, does not exist")

    vcd = parse_vcd_file(vcd_path)
    if not signals_diverge(vcd):
        raise AssertionError(
            f"Expected m0.*/m1.* (or *__m0/*__m1) divergence in VCD {vcd_path}; signals={vcd.signal_paths()!r}"
        )


def _case_f8_t1_05(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f8/unmasked_leak.sv")
    upf = ctx.copy_fixture("f7_f10/f8/unmasked_leak.upf")
    tcl = ctx.write_file(
        "run_why_cex.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top unmasked_leak
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage formal -prove_corruption
set cex_json [report_lp -status cex -json]
puts "===CEX_LIST_BEGIN==="
puts $cex_json
puts "===CEX_LIST_END==="
set prop_name ""
foreach tok [split $cex_json "\\"\\n\\r\\t ,:\\[\\]\\{{\\}}"] {{
    if {{[string match "*lp_*" $tok] || [string match "*corrupt*" $tok] || [string match "*iso*" $tok]}} {{
        if {{![catch {{get_status $tok}}]}} {{
            if {{[get_status $tok] eq "cex"}} {{
                set prop_name $tok
                break
            }}
        }}
    }}
}}
if {{$prop_name eq ""}} {{
    set csv_lines [split [report -csv -task LowPower] "\\n"]
    foreach line $csv_lines {{
        set cols [split $line ","]
        if {{[llength $cols] >= 3}} {{
            set p [string trim [lindex $cols 0]]
            set st [string trim [lindex $cols 2]]
            if {{$st eq "cex" && ([string match "*lp_*" $p] || [string match "*corrupt*" $p] || [string match "*iso*" $p] || [string match "*leak*" $p])}} {{
                set prop_name $p
                break
            }}
        }}
    }}
}}
if {{$prop_name eq ""}} {{
    error "No CEX formal property found in report_lp -status cex -json or report -csv -task LowPower"
}}
puts "===WHY_CEX_BEGIN==="
puts [why_cex -property $prop_name -json]
puts "===WHY_CEX_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=90.0)
    assert_exit(res, 0, "F8_T1_05 why_cex on ISO_CORRUPTION_LEAK")

    why = _parse_json_block(res.stdout, "WHY_CEX")
    assert_eq(_norm_status(why.get("status")), "cex", "why_cex status")
    events = why.get("events") or []
    if not events:
        raise AssertionError(f"Expected non-empty events in why_cex report: {why!r}")
    active_sigs = [str(s) for s in (why.get("active_signals") or [])]
    if not active_sigs:
        raise AssertionError(f"Expected non-empty active_signals in why_cex report: {why!r}")

    joined = " ".join(active_sigs)
    if not any(tok in joined for tok in ("leak_sel", "src_data", "top_out", "pwr_ctrl")):
        raise AssertionError(f"Expected leak/PMU signals in active_signals, got {active_sigs!r}")
    if any("bystander_in" in s for s in active_sigs):
        raise AssertionError(f"Expected untaken bystander_in pruned from active_signals, got {active_sigs!r}")


# ---------------------------------------------------------------------------
# Feature Area F9: Liberty .lib Reader, Gate/PG Netlist Stages 3-4 & LP-LEC
# ---------------------------------------------------------------------------


def _case_f9_t1_01(ctx: E2EContext) -> None:
    lib_path = ctx.write_file("mini.lib", generate_mini_liberty())
    tcl = ctx.write_file(
        "run_lib.tcl",
        f"""
clear -all
read_liberty {lib_path.name}
puts "===HELP_LIB_BEGIN==="
puts [help read_liberty]
puts "===HELP_LIB_END==="
""".strip()
        + "\n",
    )
    chk = ctx.run_shell_check(tcl, json_out=True, timeout_s=30.0)
    assert_exit(chk, 0, "F9_T1_01 read_liberty --check")
    chk_json = chk.json()
    if chk_json.get("findings") != []:
        raise AssertionError(f"Expected clean --check (findings == []) for read_liberty script, got {chk_json!r}")
    cmds = chk_json.get("commands")
    if not isinstance(cmds, dict) or not cmds:
        raise AssertionError(f"Expected non-empty commands map in --check output, got {chk_json!r}")
    rejected = {k: v for k, v in cmds.items() if isinstance(v, dict) and v.get("rejected", 0) != 0}
    if rejected:
        raise AssertionError(f"Expected zero rejected commands in --check output, got {rejected!r}")
    if "read_liberty" not in cmds or cmds["read_liberty"].get("honoured", 0) < 1:
        raise AssertionError(f"Expected read_liberty honoured >= 1, got {cmds!r}")

    t0 = time.monotonic()
    res = ctx.run_shell_file(tcl, timeout_s=30.0)
    dur = time.monotonic() - t0
    assert_exit(res, 0, "F9_T1_01 read_liberty")
    if dur >= 5.0:
        raise AssertionError(f"Expected fast Liberty parse (<5s), took {dur:.2f}s")
    low = res.combined.lower()
    if "unsupported" in low or "syntax error" in low:
        raise AssertionError(f"Unexpected warning/error during read_liberty:\n{res.combined}")
    help_text = _extract_block(res.stdout, "HELP_LIB")
    assert_in("read_liberty", help_text, "F9_T1_01 help read_liberty")


def _case_f9_t1_02(ctx: E2EContext) -> None:
    ctx.write_file("mini.lib", generate_mini_liberty())
    ctx.write_file("stdcells.v", generate_stdcell_verilog())
    ctx.write_file("gate_clean.v", generate_gate_pg_netlist(num_cells=50, include_bugs=False))
    ctx.write_file("gate.upf", generate_gate_pg_upf())
    tcl = ctx.write_file(
        "run_stage3.tcl",
        """
clear -all
read_liberty mini.lib
analyze -sv12 stdcells.v gate_clean.v
elaborate -top soc_pg_top
load_upf gate.upf
check_lp -stage netlist
puts "===STAGE3_JSON_BEGIN==="
puts [report_lp -json]
puts "===STAGE3_JSON_END==="
puts "===STAGE3_SUM_BEGIN==="
puts [report_lp -summary]
puts "===STAGE3_SUM_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=60.0)
    assert_exit(res, 0, "F9_T1_02 Stage 3 clean")

    lp_json = _parse_json_block(res.stdout, "STAGE3_JSON")
    violations = _lp_violations(lp_json)
    stage3_rules = {
        "ISO_CELL_MISSING",
        "ISO_CELL_MISMATCH",
        "ISO_LOGIC_BEFORE_CELL",
        "LS_CELL_MISSING",
        "LS_DIR_CELL_MISMATCH",
        "LS_VRANGE_CELL_MISMATCH",
        "LS_ORDER_VIOLATION",
        "AON_NON_AON_BUFFER",
        "RET_CELL_UNMAPPED",
        "RET_PIN_UNCONNECTED",
        "MACRO_PIN_SUPPLY_CROSSING",
    }
    if _stage3_4_error_count(violations, stage3_rules) != 0:
        raise AssertionError(f"Expected 0 open Stage 3 errors on clean netlist, got {violations!r}")
    _assert_no_open_or_cex_lp(lp_json, "F9_T1_02")


def _case_f9_t1_03(ctx: E2EContext) -> None:
    ctx.write_file("mini.lib", generate_mini_liberty())
    ctx.write_file("stdcells.v", generate_stdcell_verilog())
    ctx.write_file("gate_clean.v", generate_gate_pg_netlist(num_cells=50, include_bugs=False))
    ctx.write_file("gate.upf", generate_gate_pg_upf())
    tcl = ctx.write_file(
        "run_stage4.tcl",
        """
clear -all
read_liberty mini.lib
analyze -sv12 stdcells.v gate_clean.v
elaborate -top soc_pg_top
load_upf gate.upf
check_lp -stage pg
puts "===STAGE4_JSON_BEGIN==="
puts [report_lp -json]
puts "===STAGE4_JSON_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=60.0)
    assert_exit(res, 0, "F9_T1_03 Stage 4 clean")

    lp_json = _parse_json_block(res.stdout, "STAGE4_JSON")
    violations = _lp_violations(lp_json)
    stage4_rules = {
        "PG_PIN_UNCONNECTED",
        "PG_SUPPLY_MISMATCH",
        "PG_BACKUP_MISMATCH",
        "PG_SHORT",
        "PG_BIAS_MISMATCH",
        "PG_SWITCH_CHAIN_BROKEN",
    }
    if _stage3_4_error_count(violations, stage4_rules) != 0:
        raise AssertionError(f"Expected 0 Stage 4 PG errors on clean netlist, got {violations!r}")
    _assert_no_open_or_cex_lp(lp_json, "F9_T1_03")


def _case_f9_t1_04(ctx: E2EContext) -> None:
    ctx.write_file("mini.lib", generate_mini_liberty())
    stdcells = ctx.write_file("stdcells.v", generate_stdcell_verilog())
    net_50 = ctx.write_file("net_50.v", generate_gate_pg_netlist(num_cells=50, include_bugs=False))
    ctx.write_file("gate_100k.v", generate_gate_pg_netlist(num_cells=100000, include_bugs=False))
    ctx.write_file("gate.upf", generate_gate_pg_upf())

    low_res = ctx.run_lower([stdcells, net_50], top="soc_pg_top", timeout_s=30.0)
    assert_exit(low_res, 0, "F9_T1_04 lower 50-cell PG netlist")
    assert_in("soc_pg_top.din", low_res.stdout, "F9_T1_04 Btor2 input symbol")
    assert_in("soc_pg_top.u_core.u_ret.Q", low_res.stdout, "F9_T1_04 Btor2 state symbol")

    tcl = ctx.write_file(
        "run_100k.tcl",
        """
clear -all
read_liberty mini.lib
analyze -sv12 gate_100k.v
elaborate -top soc_pg_top
load_upf gate.upf
check_lp -stage netlist
check_lp -stage pg
puts "===SCALE_JSON_BEGIN==="
puts [report_lp -json]
puts "===SCALE_JSON_END==="
""".strip()
        + "\n",
    )
    t0 = time.monotonic()
    res = ctx.run_shell_file(tcl, timeout_s=60.0)
    elapsed = time.monotonic() - t0
    assert_exit(res, 0, "F9_T1_04 100k-cell Stage 3+4")
    if elapsed >= 20.0:
        raise AssertionError(f"Expected 100k-cell Stage 3+4 < 20s, took {elapsed:.2f}s")
    lp_json = _parse_json_block(res.stdout, "SCALE_JSON")
    _assert_no_open_or_cex_lp(lp_json, "F9_T1_04 100k")


def _case_f9_t1_05(ctx: E2EContext) -> None:
    ctx.write_file("mini.lib", generate_mini_liberty())
    ctx.write_file("stdcells.v", generate_stdcell_verilog())
    rtl = ctx.copy_fixture("f7_f10/f9/lec_rtl.sv")
    gate_ok = ctx.copy_fixture("f7_f10/f9/lec_gate_ok.v")
    upf = ctx.copy_fixture("f7_f10/f9/lec.upf")
    tcl = ctx.write_file(
        "run_lp_lec.tcl",
        f"""
clear -all
read_liberty mini.lib
analyze -sv12 {rtl.name}
elaborate -top lec_top
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
check_sec -setup -lec -spec {rtl.name} -spec_top lec_top -imp {{stdcells.v {gate_ok.name}}} -imp_top lec_top -clock clk -reset {{!rst_n}}
check_lp -lec
puts "===LEC_SEC_BEGIN==="
puts [check_sec -report -json]
puts "===LEC_SEC_END==="
puts "===LEC_LP_BEGIN==="
puts [report_lp -json]
puts "===LEC_LP_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=90.0)
    assert_exit(res, 0, "F9_T1_05 check_lp -lec pass")

    sec_state = _parse_json_block(res.stdout, "LEC_SEC")
    checks = sec_state.get("checks") or []
    if not checks:
        raise AssertionError(f"Expected non-empty LEC checks, got {sec_state!r}")
    if not any("dout" in str(c.get("name", "")) for c in checks):
        raise AssertionError(f"Expected dout LEC check in {checks!r}")
    for c in checks:
        assert_eq(_norm_status(c.get("status")), "proven", f"LP-LEC check {c.get('name')}")

    lp_json = _parse_json_block(res.stdout, "LEC_LP")
    _assert_no_open_or_cex_lp(lp_json, "F9_T1_05")


def _case_f9_t1_06(ctx: E2EContext) -> None:
    ctx.write_file("mini.lib", generate_mini_liberty())
    extra_lib = ctx.copy_fixture("f7_f10/f9/extra_cells.lib")
    ctx.write_file("stdcells.v", generate_stdcell_verilog())
    ctx.write_file("gate_clean.v", generate_gate_pg_netlist(num_cells=50, include_bugs=False))
    ctx.write_file("gate.upf", generate_gate_pg_upf())
    tcl = ctx.write_file(
        "run_design_stage.tcl",
        f"""
clear -all
read_liberty mini.lib {extra_lib.name}
analyze -sv12 stdcells.v gate_clean.v
elaborate -top soc_pg_top
load_upf gate.upf
check_lp -stage design
puts "===DESIGN_LP_BEGIN==="
puts [report_lp -json]
puts "===DESIGN_LP_END==="
puts "===DESIGN_SUM_BEGIN==="
puts [report_lp -summary]
puts "===DESIGN_SUM_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=60.0)
    assert_exit(res, 0, "F9_T1_06 -stage design + multi-file read_liberty")

    lp_json = _parse_json_block(res.stdout, "DESIGN_LP")
    _assert_no_open_or_cex_lp(lp_json, "F9_T1_06")
    summary = _extract_block(res.stdout, "DESIGN_SUM")
    if not summary.strip():
        raise AssertionError("Expected non-empty report_lp -summary output")


# ---------------------------------------------------------------------------
# Feature Area F10: LP MCP Tools, audit_low_power Prompt & Workbench UI
# ---------------------------------------------------------------------------


def _case_f10_t1_01(ctx: E2EContext) -> None:
    with ctx.mcp(mode="auto", root=ctx.workdir) as client:
        init_res = client.initialize(protocol_version="2025-06-18")
        assert_eq(init_res.get("protocolVersion"), "2025-06-18", "MCP protocolVersion")
        assert_eq((init_res.get("serverInfo") or {}).get("name"), "informal", "serverInfo.name")
        caps = init_res.get("capabilities") or {}
        if "tools" not in caps or "prompts" not in caps:
            raise AssertionError(f"Expected tools & prompts capabilities, got {caps!r}")

        tools = client.tools_list()
        by_name = {str(t["name"]): t for t in tools if isinstance(t, dict) and "name" in t}
        for req in ("load_upf", "check_lp", "lp_diagnose"):
            if req not in by_name:
                raise AssertionError(f"Missing LP MCP tool {req!r}; got {sorted(by_name.keys())!r}")

        assert_eq(
            (by_name["load_upf"].get("annotations") or {}).get("readOnlyHint"),
            False,
            "load_upf readOnlyHint",
        )
        assert_eq(
            (by_name["check_lp"].get("annotations") or {}).get("readOnlyHint"),
            False,
            "check_lp readOnlyHint",
        )
        assert_eq(
            (by_name["lp_diagnose"].get("annotations") or {}).get("readOnlyHint"),
            True,
            "lp_diagnose readOnlyHint",
        )

        expected_schemas = {
            "load_upf": "informal.upf/1",
            "check_lp": "informal.lp/1",
            "lp_diagnose": "informal.lp_diagnose/1",
        }
        for tool_name, schema_const in expected_schemas.items():
            out_schema = by_name[tool_name].get("outputSchema") or {}
            actual_const = ((out_schema.get("properties") or {}).get("schema") or {}).get("const")
            assert_eq(actual_const, schema_const, f"{tool_name} outputSchema.schema")

        stage_enum = (
            ((by_name["check_lp"].get("inputSchema") or {}).get("properties") or {})
            .get("stage", {})
            .get("enum")
            or []
        )
        for req_stage in ("upf", "rtl", "netlist", "pg", "formal", "all"):
            if req_stage not in stage_enum:
                raise AssertionError(f"check_lp stage enum missing {req_stage!r}: {stage_enum!r}")

    doc_res = ctx.run_docgen_check()
    assert_exit(doc_res, 0, "F10_T1_01 docgen --check")

    mcp_doc = (ctx.repo_root / "docs" / "reference" / "mcp-tools.md").read_text(encoding="utf-8")
    m = re.search(r"### `gui_set_view`(.*?)(?:\n### |\Z)", mcp_doc, re.DOTALL)
    if not m:
        raise AssertionError("Missing ### `gui_set_view` section in docs/reference/mcp-tools.md")
    if "`lp`" not in m.group(1):
        raise AssertionError(f"Expected `lp` in gui_set_view doc section:\n{m.group(1)}")

    tools_gui_path = ctx.repo_root / "crates" / "informal-mcp" / "tests" / "golden" / "tools_gui.json"
    gui_tools = json.loads(tools_gui_path.read_text(encoding="utf-8"))
    gui_by_name = {str(t["name"]): t for t in gui_tools if isinstance(t, dict) and "name" in t}
    gui_set_view = gui_by_name.get("gui_set_view")
    if gui_set_view is None:
        raise AssertionError(f"Missing gui_set_view in {tools_gui_path}")
    app_tab_enum = (
        ((gui_set_view.get("input") or {}).get("properties") or {})
        .get("app_tab", {})
        .get("enum")
        or []
    )
    if "lp" not in app_tab_enum:
        raise AssertionError(
            f"Expected 'lp' in gui_set_view app_tab enum in {tools_gui_path}: {app_tab_enum!r}"
        )

    idx_json = json.loads(
        (ctx.repo_root / "docs" / "reference" / "index.json").read_text(encoding="utf-8")
    )
    if "lp" not in json.dumps(idx_json):
        raise AssertionError("Expected 'lp' in docs/reference/index.json")


def _case_f10_t1_02(ctx: E2EContext) -> None:
    ctx.copy_fixture("f7_f10/f10/soc_lp_clean.sv")
    ctx.copy_fixture("f7_f10/f10/soc_lp_clean.upf")

    with ctx.mcp(mode="auto", root=ctx.workdir) as client:
        client.call_tool(
            "session_open",
            {"files": ["soc_lp_clean.sv"], "top": "soc_lp", "clock": "clk", "reset": "!rst_n"},
        )
        raw = client.call_tool_raw("load_upf", {"file": "soc_lp_clean.upf"})
        assert_eq(raw.get("isError"), False, "load_upf isError")
        structured = raw.get("structuredContent")
        text_obj = json.loads(raw["content"][0]["text"])
        assert_eq(text_obj, structured, "load_upf text JSON vs structuredContent")
        assert_eq(structured.get("schema"), "informal.upf/1", "load_upf schema")

        dom_blob = json.dumps(structured.get("domains"))
        for dom in ("PD_AON", "PD_CORE", "PD_RET"):
            if dom not in dom_blob:
                raise AssertionError(f"Expected domain {dom!r} in load_upf domains: {dom_blob}")

        if not structured.get("pst") and not structured.get("supply_sets") and not structured.get("supply_nets"):
            raise AssertionError(f"Expected supply/PST summary in load_upf reply: {structured!r}")

        diags = structured.get("diagnostics") or []
        hard_errs = [
            d
            for d in diags
            if isinstance(d, dict) and str(d.get("severity", "")).lower() == "error"
        ]
        if hard_errs:
            raise AssertionError(f"Expected zero UPF error diagnostics, got {hard_errs!r}")


def _case_f10_t1_03(ctx: E2EContext) -> None:
    ctx.copy_fixture("f7_f10/f10/soc_lp_buggy.sv")
    ctx.copy_fixture("f7_f10/f10/soc_lp_buggy.upf")

    with ctx.mcp(mode="auto", root=ctx.workdir) as client:
        tools = client.tools_list()
        diag_tool = next((t for t in tools if t.get("name") == "lp_diagnose"), None)
        if diag_tool is None:
            raise AssertionError("Missing lp_diagnose in tools/list")
        diag_props = (diag_tool.get("inputSchema") or {}).get("properties") or {}
        selector_key = next(
            (k for k in ("id", "violation", "name", "violation_id") if k in diag_props),
            "id",
        )

        client.call_tool(
            "session_open",
            {"files": ["soc_lp_buggy.sv"], "top": "soc_lp", "clock": "clk", "reset": "!rst_n"},
        )
        client.call_tool("load_upf", {"file": "soc_lp_buggy.upf"})
        lp_res = client.call_tool("check_lp", {"stage": "rtl"})
        assert_eq(lp_res.get("schema"), "informal.lp/1", "check_lp schema")

        violations = lp_res.get("violations") or []
        if not violations:
            raise AssertionError(f"Expected non-empty violations on soc_lp_buggy, got {lp_res!r}")

        iso_v = next(
            (
                v
                for v in violations
                if "ISO_MISSING" in _norm_rule(v)
                and v.get("domain_src") == "PD_CORE"
                and v.get("domain_dst") == "PD_AON"
            ),
            None,
        )
        if iso_v is None:
            raise AssertionError(
                f"Expected ISO_MISSING violation PD_CORE -> PD_AON, got {violations!r}"
            )

        vid = iso_v.get("id") or iso_v.get("name") or iso_v.get("rule_id")
        diag = client.call_tool("lp_diagnose", {selector_key: vid})
        assert_eq(diag.get("schema"), "informal.lp_diagnose/1", "lp_diagnose schema")
        if not diag.get("violation"):
            raise AssertionError(f"Expected violation metadata in lp_diagnose: {diag!r}")
        if "pst_witness" not in diag:
            raise AssertionError(f"Expected pst_witness in lp_diagnose: {diag!r}")
        if not (diag.get("upf_snippet") or diag.get("upf_source") or diag.get("upf_loc")):
            raise AssertionError(f"Expected UPF source/loc in lp_diagnose: {diag!r}")
        if not (diag.get("rtl_path") or diag.get("driver_snippets") or diag.get("rtl_loc")):
            raise AssertionError(f"Expected RTL path/snippets in lp_diagnose: {diag!r}")
        if not (diag.get("hint") or diag.get("remediation") or diag.get("summary")):
            raise AssertionError(f"Expected remediation/triage hint in lp_diagnose: {diag!r}")


def _case_f10_t1_04(ctx: E2EContext) -> None:
    ctx.copy_fixture("f7_f10/f10/soc_lp_clean.sv")
    ctx.copy_fixture("f7_f10/f10/soc_lp_clean.upf")

    with ctx.mcp(mode="auto", root=ctx.workdir) as client:
        client.call_tool(
            "session_open",
            {"files": ["soc_lp_clean.sv"], "top": "soc_lp", "clock": "clk", "reset": "!rst_n"},
        )
        client.call_tool("load_upf", {"file": "soc_lp_clean.upf"})

        prompts = client.prompts_list()
        names = {p.get("name") for p in prompts if isinstance(p, dict)}
        if "audit_low_power" not in names:
            raise AssertionError(f"Missing audit_low_power in prompts/list: {sorted(names)!r}")

        prompt_res = client.prompts_get("audit_low_power", {})
        messages = prompt_res.get("messages") or []
        if not messages:
            raise AssertionError(f"Expected non-empty messages from prompts/get: {prompt_res!r}")
        assert_eq(messages[0].get("role"), "user", "prompt message role")
        content = messages[0].get("content") or {}
        assert_eq(content.get("type"), "text", "prompt content type")
        text = str(content.get("text") or "")
        for expected in ("soc_lp", "clk", "!rst_n", "PD_AON", "PD_CORE", "check_lp", "lp_diagnose"):
            assert_in(expected, text, "F10_T1_04 audit_low_power hydrated context")


def _case_f10_t1_05(ctx: E2EContext) -> None:
    ui_dir = ctx.repo_root / "ui"
    if not (ui_dir / "node_modules").exists():
        raise AssertionError(f"Missing ui/node_modules at {ui_dir / 'node_modules'}")

    ui_env = _ui_env(ctx)
    tsc_res: CliResult = run_command(
        ["npx", "--no-install", "tsc", "--noEmit"],
        cwd=ui_dir,
        env=ui_env,
        timeout_s=60.0,
    )
    assert_exit(tsc_res, 0, "F10_T1_05 ui tsc --noEmit")

    vitest_res: CliResult = run_command(
        [
            "npx",
            "--no-install",
            "vitest",
            "run",
            "src/components/FormalApps.test.tsx",
            "src/components/SchematicView.test.ts",
            "src/components/TraceView.test.tsx",
        ],
        cwd=ui_dir,
        env=ui_env,
        timeout_s=60.0,
    )
    assert_exit(vitest_res, 0, "F10_T1_05 ui vitest")

    pw_caches = (
        Path.home() / "Library" / "Caches" / "ms-playwright",
        Path.home() / ".cache" / "ms-playwright",
    )
    if any(p.is_dir() and any(p.iterdir()) for p in pw_caches):
        pw_out = ctx.path("pw_out")
        pw_out.mkdir(parents=True, exist_ok=True)
        ui_port = _find_free_port()
        pw_res: CliResult = run_command(
            [
                "npx",
                "--no-install",
                "playwright",
                "test",
                "--project=e2e",
                "tests/e2e/v2-upgrades-ux.spec.ts",
                "--output",
                str(pw_out),
            ],
            cwd=ui_dir,
            env=_ui_env(ctx, UI_TEST_PORT=str(ui_port)),
            timeout_s=120.0,
        )
        assert_exit(pw_res, 0, "F10_T1_05 ui playwright e2e")

    vcd_path = ctx.fixture("f7_f10/f10/lp_swimlane_cex.vcd")
    vcd = parse_vcd_file(vcd_path)
    if not has_lp_domain_signals(vcd):
        raise AssertionError("Expected lp.pd.* signals in lp_swimlane_cex.vcd")
    assert_eq(lp_domain_names(vcd), ["PD_CORE", "PD_RET"], "lp_domain_names")

    # Verify ON -> ISO -> RET -> OFF -> ON state timeline for PD_CORE
    assert_eq(vcd.value_at("lp.pd.PD_CORE.on", 10), "1", "PD_CORE ON @10 on")
    assert_eq(vcd.value_at("lp.pd.PD_CORE.iso", 10), "0", "PD_CORE ON @10 iso")
    assert_eq(vcd.value_at("lp.pd.PD_CORE.iso", 20), "1", "PD_CORE ISO @20 iso")
    assert_eq(vcd.value_at("lp.pd.PD_CORE.on", 30), "0", "PD_CORE RET @30 on")
    assert_eq(vcd.value_at("lp.pd.PD_CORE.ret", 30), "1", "PD_CORE RET @30 ret")
    assert_eq(vcd.value_at("lp.pd.PD_CORE.corrupt", 30), "1", "PD_CORE RET @30 corrupt")
    assert_eq(vcd.value_at("lp.pd.PD_CORE.ret", 40), "0", "PD_CORE OFF @40 ret")
    assert_eq(vcd.value_at("lp.pd.PD_CORE.on", 50), "1", "PD_CORE wakeup ON @50 on")
    assert_eq(vcd.value_at("lp.pd.PD_CORE.iso", 50), "0", "PD_CORE wakeup ON @50 iso")

    # Verify WS6 UI LP components ("lp" FormalApps tab, domain tags, lp.pd.* swimlanes)
    formal_apps_path = ui_dir / "src" / "components" / "FormalApps.tsx"
    if not formal_apps_path.exists():
        formal_apps_path = ui_dir / "src" / "components" / "FormalApps" / "index.tsx"
    formal_apps_src = formal_apps_path.read_text(encoding="utf-8")
    schematic_src = (ui_dir / "src" / "components" / "SchematicView.tsx").read_text(encoding="utf-8")
    trace_src = (ui_dir / "src" / "components" / "TraceView.tsx").read_text(encoding="utf-8")
    if '"lp"' not in formal_apps_src:
        raise AssertionError(f"Expected 'lp' tab in {formal_apps_path}")
    if "power_domain" not in schematic_src:
        raise AssertionError("Expected power_domain tag support in ui/src/components/SchematicView.tsx")
    if "lp.pd." not in trace_src:
        raise AssertionError("Expected lp.pd.* swimlane handling in ui/src/components/TraceView.tsx")


def get_cases() -> list[TestCase]:
    """Return all 22 Tier 1 E2E test cases for F7-F10."""
    return [
        # F7 (6)
        TestCase(
            id="F7_T1_01",
            tier=1,
            feature="F7",
            title="check_lp -stage formal on clean PMU + ISO + Retention SoC",
            fn=_case_f7_t1_01,
            timeout_s=90.0,
            tags=("f7", "formal", "aag", "pmu"),
        ),
        TestCase(
            id="F7_T1_02",
            tier=1,
            feature="F7",
            title="check_lpv lp_formal compatibility alias",
            fn=_case_f7_t1_02,
            timeout_s=90.0,
            tags=("f7", "check_lpv", "compat"),
        ),
        TestCase(
            id="F7_T1_03",
            tier=1,
            feature="F7",
            title="PST legal-state cover (lp_pst_reach_*) & illegal-state safety",
            fn=_case_f7_t1_03,
            timeout_s=90.0,
            tags=("f7", "pst", "cover"),
        ),
        TestCase(
            id="F7_T1_04",
            tier=1,
            feature="F7",
            title="SIMSTATE ISO 0 / 1 / latch behavioral verification",
            fn=_case_f7_t1_04,
            timeout_s=90.0,
            tags=("f7", "simstate", "iso"),
        ),
        TestCase(
            id="F7_T1_05",
            tier=1,
            feature="F7",
            title="Retention value preservation across sleep/wake (lp_ret_value_preserved_*)",
            fn=_case_f7_t1_05,
            timeout_s=90.0,
            tags=("f7", "retention", "aag"),
        ),
        TestCase(
            id="F7_T1_06",
            tier=1,
            feature="F7",
            title="SIMSTATE CORRUPT_ON_ACTIVITY & CORRUPT_STATE_ON_ACTIVITY clean when idle",
            fn=_case_f7_t1_06,
            timeout_s=90.0,
            tags=("f7", "simstate", "coa", "csoa"),
        ),
        # F8 (5)
        TestCase(
            id="F8_T1_01",
            tier=1,
            feature="F8",
            title="check_lp -prove_corruption proven clean on isolated & retained SoC",
            fn=_case_f8_t1_01,
            timeout_s=90.0,
            tags=("f8", "corruption", "miter"),
        ),
        TestCase(
            id="F8_T1_02",
            tier=1,
            feature="F8",
            title="check_lp -prove_waivers marks functionally-masked crossing FormallyWaived",
            fn=_case_f8_t1_02,
            timeout_s=90.0,
            tags=("f8", "waivers", "miter"),
        ),
        TestCase(
            id="F8_T1_03",
            tier=1,
            feature="F8",
            title="check_lp -sec golden RTL vs power-aware RTL+UPF proven equivalent",
            fn=_case_f8_t1_03,
            timeout_s=120.0,
            tags=("f8", "sec", "lp_sec"),
        ),
        TestCase(
            id="F8_T1_04",
            tier=1,
            feature="F8",
            title="FormalCex VCD trace generated on genuine leak (m0.*/m1.* via vcd_check)",
            fn=_case_f8_t1_04,
            timeout_s=90.0,
            tags=("f8", "vcd", "cex"),
        ),
        TestCase(
            id="F8_T1_05",
            tier=1,
            feature="F8",
            title="why_cex explains ISO_CORRUPTION_LEAK CEX and prunes bystander signals",
            fn=_case_f8_t1_05,
            timeout_s=90.0,
            tags=("f8", "why_cex", "cex"),
        ),
        # F9 (6)
        TestCase(
            id="F9_T1_01",
            tier=1,
            feature="F9",
            title="read_liberty parses Liberty .lib with 7x7 NLDM tables cleanly",
            fn=_case_f9_t1_01,
            timeout_s=45.0,
            tags=("f9", "liberty"),
        ),
        TestCase(
            id="F9_T1_02",
            tier=1,
            feature="F9",
            title="Stage 3 (check_lp -stage netlist) passes clean gate netlist",
            fn=_case_f9_t1_02,
            timeout_s=60.0,
            tags=("f9", "netlist", "stage3"),
        ),
        TestCase(
            id="F9_T1_03",
            tier=1,
            feature="F9",
            title="Stage 4 (check_lp -stage pg) passes clean PG netlist",
            fn=_case_f9_t1_03,
            timeout_s=60.0,
            tags=("f9", "pg", "stage4"),
        ),
        TestCase(
            id="F9_T1_04",
            tier=1,
            feature="F9",
            title="Structural gate/PG 100k-cell scaling & informal lower stdcell check",
            fn=_case_f9_t1_04,
            timeout_s=90.0,
            tags=("f9", "gate_fast", "scaling", "lower"),
        ),
        TestCase(
            id="F9_T1_05",
            tier=1,
            feature="F9",
            title="check_lp -lec RTL+UPF vs matching Post-Synth Netlist+UPF passes",
            fn=_case_f9_t1_05,
            timeout_s=90.0,
            tags=("f9", "lec", "lp_lec"),
        ),
        TestCase(
            id="F9_T1_06",
            tier=1,
            feature="F9",
            title="check_lp -stage design alias & multi-file read_liberty",
            fn=_case_f9_t1_06,
            timeout_s=60.0,
            tags=("f9", "design", "liberty"),
        ),
        # F10 (5)
        TestCase(
            id="F10_T1_01",
            tier=1,
            feature="F10",
            title="MCP initialize -> tools/list LP tools + gui_set_view lp via docgen",
            fn=_case_f10_t1_01,
            timeout_s=45.0,
            tags=("f10", "mcp", "docgen"),
        ),
        TestCase(
            id="F10_T1_02",
            tier=1,
            feature="F10",
            title="MCP load_upf (informal.upf/1) on multi-domain SoC",
            fn=_case_f10_t1_02,
            timeout_s=45.0,
            tags=("f10", "mcp", "load_upf"),
        ),
        TestCase(
            id="F10_T1_03",
            tier=1,
            feature="F10",
            title="MCP check_lp (informal.lp/1) + lp_diagnose (informal.lp_diagnose/1)",
            fn=_case_f10_t1_03,
            timeout_s=60.0,
            tags=("f10", "mcp", "check_lp", "lp_diagnose"),
        ),
        TestCase(
            id="F10_T1_04",
            tier=1,
            feature="F10",
            title="MCP prompts/list & prompts/get(audit_low_power) hydrated with domains",
            fn=_case_f10_t1_04,
            timeout_s=45.0,
            tags=("f10", "mcp", "prompts"),
        ),
        TestCase(
            id="F10_T1_05",
            tier=1,
            feature="F10",
            title="UI Vitest, tsc, Playwright aiGui & lp.pd.* VCD check without touching ui/",
            fn=_case_f10_t1_05,
            timeout_s=150.0,
            tags=("f10", "ui", "playwright", "vitest", "vcd"),
        ),
    ]
