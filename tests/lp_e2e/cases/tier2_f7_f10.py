"""Tier 2 (boundary, corner, and negative) E2E test cases for Feature Areas F7-F10."""

from __future__ import annotations

import csv
import io
import json
from pathlib import Path
import shutil
from typing import Any

from lib.fixtures_gen import (
    generate_gate_pg_netlist,
    generate_gate_pg_upf,
    generate_mini_liberty,
    generate_stdcell_verilog,
)
from lib.harness import E2EContext, TestCase, assert_eq, assert_exit, assert_in
from lib.mcp_client import McpProtocolError
from lib.vcd_check import (
    has_lp_domain_signals,
    lp_domain_names,
    parse_vcd,
    parse_vcd_file,
    signal_changes_after,
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
    raise AssertionError(f"Expected LP payload with violations/items list, got {payload!r}")


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


def _resolve_workdir_file(shell_workdir: Path, raw_path: str) -> Path:
    p = Path(raw_path)
    return p if p.is_absolute() else (shell_workdir / p)


def _find_vcd_for_prop(shell_workdir: Path, prop_name: str) -> Path:
    direct = shell_workdir / f"{prop_name}.vcd"
    if direct.is_file():
        return direct
    matches = sorted(shell_workdir.rglob(f"*{prop_name}*.vcd"))
    if matches:
        return matches[0]
    all_vcds = sorted(shell_workdir.rglob("*.vcd"))
    if len(all_vcds) == 1:
        return all_vcds[0]
    raise AssertionError(
        f"Expected VCD trace for property {prop_name!r} in {shell_workdir}; found {all_vcds!r}"
    )


def _tcl_extract_cex_prop_block(prefix_globs: tuple[str, ...]) -> str:
    conds = " || ".join(f"[string match {g!r} $p]" for g in prefix_globs)
    conds = conds.replace("'", '"')
    return f"""
set target_prop ""
set csv_lines [split [report -csv -task LowPower] "\\n"]
foreach line $csv_lines {{
    set cols [split $line ","]
    if {{[llength $cols] >= 3}} {{
        set p [string trim [lindex $cols 0]]
        set st [string trim [lindex $cols 2]]
        if {{$st eq "cex" && ({conds})}} {{
            set target_prop $p
            break
        }}
    }}
}}
if {{$target_prop eq ""}} {{
    error "Expected CEX property matching {prefix_globs!r}"
}}
puts "===TARGET_PROP_BEGIN==="
puts $target_prop
puts "===TARGET_PROP_END==="
puts "===WHY_CEX_BEGIN==="
puts [why_cex -property $target_prop -json]
puts "===WHY_CEX_END==="
"""


# ---------------------------------------------------------------------------
# Feature Area F7 Tier 2: PMU Ordering, Async Reset, PST Illegal, GClk, COA
# ---------------------------------------------------------------------------


def _case_f7_t2_01(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f7/bug_iso_before_pwroff.sv")
    upf = ctx.copy_fixture("f7_f10/f7/bug_iso_before_pwroff.upf")
    shell_workdir = ctx.path("shell_work")
    tcl = ctx.write_file(
        "run.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top bug_iso_before_pwroff
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage formal
puts "===CSV_BEGIN==="
report -csv -task LowPower
puts "===CSV_END==="
"""
        + _tcl_extract_cex_prop_block(("*lp_iso_before_pwroff_*",)),
    )
    res = ctx.run_shell_file(tcl, workdir=shell_workdir, timeout_s=90.0)
    assert_exit(res, 0, "F7_T2_01 pwr_sw before iso_en")

    rows = _parse_csv_block(res.stdout, "CSV")
    iso_rows = [r for r in rows if "lp_iso_before_pwroff_" in r.get("property", "")]
    if not iso_rows:
        raise AssertionError(f"Expected lp_iso_before_pwroff_* row in {rows!r}")
    assert_eq(_norm_status(iso_rows[0].get("status")), "cex", "lp_iso_before_pwroff status")

    prop_name = _extract_block(res.stdout, "TARGET_PROP")
    why = _parse_json_block(res.stdout, "WHY_CEX")
    assert_eq(_norm_status(why.get("status")), "cex", "why_cex status")
    if not (why.get("events") or []):
        raise AssertionError(f"Expected non-empty why_cex events: {why!r}")

    vcd = parse_vcd_file(_find_vcd_for_prop(shell_workdir, prop_name))
    pwr_sigs = vcd.find_signals("*pwr_ctrl*")
    iso_sigs = vcd.find_signals("*iso_en*")
    if not pwr_sigs or not iso_sigs:
        raise AssertionError(f"Expected pwr_ctrl and iso_en in VCD signals={vcd.signal_paths()!r}")
    witnessed = any(
        t > 0
        and vcd.value_at(pwr_sigs[0], t) == "0"
        and vcd.value_at(iso_sigs[0], t) == "0"
        for t in vcd.times
    )
    if not witnessed:
        raise AssertionError(
            f"Expected post-reset cycle with pwr_ctrl==0 && iso_en==0 in VCD times={vcd.times!r}"
        )


def _case_f7_t2_02(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f7/bug_iso_drop_early.sv")
    upf = ctx.copy_fixture("f7_f10/f7/bug_iso_drop_early.upf")
    shell_workdir = ctx.path("shell_work")
    tcl = ctx.write_file(
        "run.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top bug_iso_drop_early
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage formal
puts "===CSV_BEGIN==="
report -csv -task LowPower
puts "===CSV_END==="
"""
        + _tcl_extract_cex_prop_block(("*lp_iso_hold_after_pwron_*",)),
    )
    res = ctx.run_shell_file(tcl, workdir=shell_workdir, timeout_s=90.0)
    assert_exit(res, 0, "F7_T2_02 iso_en drops before pwr_ack")

    rows = _parse_csv_block(res.stdout, "CSV")
    hold_rows = [r for r in rows if "lp_iso_hold_after_pwron_" in r.get("property", "")]
    if not hold_rows:
        raise AssertionError(f"Expected lp_iso_hold_after_pwron_* in {rows!r}")
    assert_eq(_norm_status(hold_rows[0].get("status")), "cex", "lp_iso_hold_after_pwron status")

    prop_name = _extract_block(res.stdout, "TARGET_PROP")
    why = _parse_json_block(res.stdout, "WHY_CEX")
    assert_eq(_norm_status(why.get("status")), "cex", "why_cex status")
    if not (why.get("events") or []):
        raise AssertionError(f"Expected non-empty why_cex events: {why!r}")

    vcd = parse_vcd_file(_find_vcd_for_prop(shell_workdir, prop_name))
    ack_sigs = vcd.find_signals("*pwr_ack*")
    iso_sigs = vcd.find_signals("*iso_en*")
    if not ack_sigs or not iso_sigs:
        raise AssertionError(f"Expected pwr_ack and iso_en in VCD signals={vcd.signal_paths()!r}")
    witnessed = any(
        t > 0
        and vcd.value_at(iso_sigs[0], t) == "0"
        and vcd.value_at(ack_sigs[0], t) == "0"
        for t in vcd.times
    )
    if not witnessed:
        raise AssertionError("Expected post-reset cycle with iso_en==0 while pwr_ack==0 in VCD")


def _case_f7_t2_03(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f7/bug_ret_async_rst.sv")
    upf = ctx.copy_fixture("f7_f10/f7/bug_ret_async_rst.upf")
    shell_workdir = ctx.path("shell_work")
    tcl = ctx.write_file(
        "run.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top bug_ret_async_rst
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage formal
puts "===CSV_BEGIN==="
report -csv -task LowPower
puts "===CSV_END==="
"""
        + _tcl_extract_cex_prop_block(("*lp_ret_async_rst_clean_*", "*lp_ret_value_preserved_*")),
    )
    res = ctx.run_shell_file(tcl, workdir=shell_workdir, timeout_s=90.0)
    assert_exit(res, 0, "F7_T2_03 unisolated async reset during retention")

    rows = _parse_csv_block(res.stdout, "CSV")
    rst_cex = [
        r
        for r in rows
        if (
            "lp_ret_async_rst_clean_" in r.get("property", "")
            or "lp_ret_value_preserved_" in r.get("property", "")
        )
        and _norm_status(r.get("status")) == "cex"
    ]
    if not rst_cex:
        raise AssertionError(f"Expected CEX on lp_ret_async_rst_clean_* or lp_ret_value_preserved_*, got {rows!r}")

    prop_name = _extract_block(res.stdout, "TARGET_PROP")
    vcd = parse_vcd_file(_find_vcd_for_prop(shell_workdir, prop_name))
    ext_rst = vcd.find_signals("*ext_soft_rst_n*")
    pwr_sigs = vcd.find_signals("*pwr_ctrl*")
    if not ext_rst or not pwr_sigs:
        raise AssertionError(f"Expected ext_soft_rst_n and pwr_ctrl in VCD: {vcd.signal_paths()!r}")
    witnessed = any(
        t > 0
        and vcd.value_at(ext_rst[0], t) == "0"
        and vcd.value_at(pwr_sigs[0], t) == "0"
        for t in vcd.times
    )
    if not witnessed:
        raise AssertionError("Expected ext_soft_rst_n==0 while pwr_ctrl==0 in retention async-reset VCD")


def _case_f7_t2_04(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f7/bug_pst_illegal.sv")
    upf = ctx.copy_fixture("f7_f10/f7/bug_pst_illegal.upf")
    shell_workdir = ctx.path("shell_work")
    tcl = ctx.write_file(
        "run.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top bug_pst_illegal
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage formal
puts "===CSV_BEGIN==="
report -csv -task LowPower
puts "===CSV_END==="
"""
        + _tcl_extract_cex_prop_block(("*lp_pst_no_illegal_*",)),
    )
    res = ctx.run_shell_file(tcl, workdir=shell_workdir, timeout_s=90.0)
    assert_exit(res, 0, "F7_T2_04 illegal PST state")

    rows = _parse_csv_block(res.stdout, "CSV")
    ill_rows = [r for r in rows if "lp_pst_no_illegal_" in r.get("property", "")]
    if not ill_rows:
        raise AssertionError(f"Expected lp_pst_no_illegal_* in {rows!r}")
    assert_eq(_norm_status(ill_rows[0].get("status")), "cex", "lp_pst_no_illegal status")

    prop_name = _extract_block(res.stdout, "TARGET_PROP")
    why = _parse_json_block(res.stdout, "WHY_CEX")
    assert_eq(_norm_status(why.get("status")), "cex", "why_cex status")
    active = " ".join(str(s) for s in (why.get("active_signals") or []))
    if not any(k in active for k in ("dbg_force_off", "pwr_ctrl", "iso_en")):
        raise AssertionError(f"Expected dbg_force_off or PMU signals in active_signals: {active!r}")

    vcd = parse_vcd_file(_find_vcd_for_prop(shell_workdir, prop_name))
    pwr_sigs = vcd.find_signals("*pwr_ctrl*")
    iso_sigs = vcd.find_signals("*iso_en*")
    if not pwr_sigs or not iso_sigs:
        raise AssertionError(f"Expected pwr_ctrl and iso_en in VCD: {vcd.signal_paths()!r}")
    witnessed = any(
        t > 0
        and vcd.value_at(pwr_sigs[0], t) == "0"
        and vcd.value_at(iso_sigs[0], t) == "0"
        for t in vcd.times
    )
    if not witnessed:
        raise AssertionError("Expected pwr_ctrl==0 && iso_en==0 post-reset in illegal PST VCD")


def _case_f7_t2_05(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f7/bug_gclk_corrupt.sv")
    upf = ctx.copy_fixture("f7_f10/f7/bug_gclk_corrupt.upf")
    shell_workdir = ctx.path("shell_work")
    tcl = ctx.write_file(
        "run.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top bug_gclk_corrupt
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage formal -prove_corruption
puts "===GCLK_CEX_BEGIN==="
puts [report_lp -status cex -json]
puts "===GCLK_CEX_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, workdir=shell_workdir, timeout_s=90.0)
    assert_exit(res, 0, "F7_T2_05 gated-clock corruption across sleep")

    cex_items = _lp_violations(_parse_json_block(res.stdout, "GCLK_CEX"))
    if not cex_items:
        raise AssertionError("Expected FormalCex/cex for unretained gated-clock flop across sleep")

    trace_raw = next((str(v.get("trace")) for v in cex_items if v.get("trace")), "")
    if not trace_raw:
        raise AssertionError(f"Expected VCD trace path on gated-clock CEX: {cex_items!r}")
    vcd = parse_vcd_file(_resolve_workdir_file(shell_workdir, trace_raw))
    if not signals_diverge(vcd):
        raise AssertionError(f"Expected m0/m1 divergence on gated-clock CEX VCD {trace_raw!r}")


def _case_f7_t2_06(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f7/bug_coa_active.sv")
    upf = ctx.copy_fixture("f7_f10/f7/bug_coa_active.upf")
    shell_workdir = ctx.path("shell_work")
    tcl = ctx.write_file(
        "run.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top bug_coa_active
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
check_lp -stage formal -prove_corruption
puts "===COA_CEX_BEGIN==="
puts [report_lp -status cex -json]
puts "===COA_CEX_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, workdir=shell_workdir, timeout_s=90.0)
    assert_exit(res, 0, "F7_T2_06 COA activity-triggered corruption")

    cex_items = _lp_violations(_parse_json_block(res.stdout, "COA_CEX"))
    if not cex_items:
        raise AssertionError("Expected FormalCex/cex on un-gated CORRUPT_ON_ACTIVITY domain")

    trace_raw = next((str(v.get("trace")) for v in cex_items if v.get("trace")), "")
    if not trace_raw:
        raise AssertionError(f"Expected VCD trace path on COA CEX: {cex_items!r}")
    vcd = parse_vcd_file(_resolve_workdir_file(shell_workdir, trace_raw))
    if not signals_diverge(vcd):
        raise AssertionError(f"Expected m0/m1 divergence in COA CEX VCD {trace_raw!r}")
    if not signal_changes_after(vcd, "*din*", after_time=0):
        raise AssertionError("Expected din activity in COA CEX VCD")


# ---------------------------------------------------------------------------
# Feature Area F8 Tier 2: Partial Retention, Circular Masking, Invalidation, SEC
# ---------------------------------------------------------------------------


def _case_f8_t2_01(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f8/bug_partial_ret_x.sv")
    upf = ctx.copy_fixture("f7_f10/f8/bug_partial_ret_x.upf")
    shell_workdir = ctx.path("shell_work")
    tcl = ctx.write_file(
        "run.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top bug_partial_ret_x
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
check_lp -stage rtl
puts "===RTL_OPEN_BEGIN==="
puts [report_lp -status open -json]
puts "===RTL_OPEN_END==="
set_prove_time_limit 30s
check_lp -stage formal -prove_corruption
puts "===FORMAL_CEX_BEGIN==="
puts [report_lp -status cex -json]
puts "===FORMAL_CEX_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, workdir=shell_workdir, timeout_s=90.0)
    assert_exit(res, 0, "F8_T2_01 partial-retention post-wakeup X-leak")

    rtl_open = _lp_violations(_parse_json_block(res.stdout, "RTL_OPEN"))
    if any("ISO_MISSING" in _norm_rule(v) for v in rtl_open):
        raise AssertionError(f"Expected 0 ISO_MISSING in Stage 2 RTL, got {rtl_open!r}")

    cex_items = _lp_violations(_parse_json_block(res.stdout, "FORMAL_CEX"))
    leaks = [
        v
        for v in cex_items
        if any(tag in _norm_rule(v) for tag in ("RET_POST_WAKEUP_X", "ISO_CORRUPTION_LEAK", "RET"))
    ]
    if not leaks:
        raise AssertionError(f"Expected RET_POST_WAKEUP_X / corruption CEX, got {cex_items!r}")

    trace_raw = next((str(v.get("trace")) for v in leaks if v.get("trace")), "")
    if not trace_raw:
        raise AssertionError(f"Expected VCD trace path on partial-retention CEX: {leaks!r}")
    vcd = parse_vcd_file(_resolve_workdir_file(shell_workdir, trace_raw))
    if not signals_diverge(vcd):
        raise AssertionError("Expected m0/m1 divergence on partial-retention post-wakeup VCD")


def _case_f8_t2_02(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f8/bug_circular_mask.sv")
    upf = ctx.copy_fixture("f7_f10/f8/bug_circular_mask.upf")
    shell_workdir = ctx.path("shell_work")
    tcl = ctx.write_file(
        "run.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top bug_circular_mask
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
check_lp -stage rtl
puts "===RTL_OPEN_BEGIN==="
puts [report_lp -status open -json]
puts "===RTL_OPEN_END==="
set_prove_time_limit 30s
check_lp -prove_waivers
puts "===WAIVED_BEGIN==="
puts [report_lp -status formally_waived -json]
puts "===WAIVED_END==="
puts "===CEX_BEGIN==="
puts [report_lp -status cex -json]
puts "===CEX_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, workdir=shell_workdir, timeout_s=90.0)
    assert_exit(res, 0, "F8_T2_02 circular mutual-masking not waived")

    rtl_open = _lp_violations(_parse_json_block(res.stdout, "RTL_OPEN"))
    iso_missing = [v for v in rtl_open if "ISO_MISSING" in _norm_rule(v)]
    if len(iso_missing) < 2:
        raise AssertionError(f"Expected >= 2 ISO_MISSING open violations, got {rtl_open!r}")

    waived = _lp_violations(_parse_json_block(res.stdout, "WAIVED"))
    if len(waived) != 0:
        raise AssertionError(
            f"Circular mutual-masking MUST NOT be auto-waived, got FormallyWaived={waived!r}"
        )

    cex_items = _lp_violations(_parse_json_block(res.stdout, "CEX"))
    if len(cex_items) < 2:
        raise AssertionError(f"Expected >= 2 CEX violations for circular masking, got {cex_items!r}")
    for v in cex_items:
        trace_raw = str(v.get("trace") or "")
        if not trace_raw:
            raise AssertionError(f"Expected VCD trace for circular-mask CEX item: {v!r}")
        vcd = parse_vcd_file(_resolve_workdir_file(shell_workdir, trace_raw))
        if not signals_diverge(vcd):
            raise AssertionError(f"Expected m0/m1 divergence in {trace_raw!r}")


def _case_f8_t2_03(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f8/waiver_inval.sv")
    upf = ctx.copy_fixture("f7_f10/f8/waiver_inval.upf")
    tcl = ctx.write_file(
        "run.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top waiver_inval
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
set_prove_time_limit 30s
assume -name m_no_bypass {{!bypass_uniso}}
check_lp -stage rtl
check_lp -prove_waivers
puts "===WAIVED_STEP1_BEGIN==="
puts [report_lp -status formally_waived -json]
puts "===WAIVED_STEP1_END==="
assume -remove m_no_bypass
puts "===OPEN_STEP2_BEGIN==="
puts [report_lp -status open -json]
puts "===OPEN_STEP2_END==="
puts "===WAIVED_STEP2_BEGIN==="
puts [report_lp -status formally_waived -json]
puts "===WAIVED_STEP2_END==="
check_lp -prove_waivers
puts "===CEX_STEP3_BEGIN==="
puts [report_lp -status cex -json]
puts "===CEX_STEP3_END==="
# Also verify stopat invalidates an active FormallyWaived status back to Open
assume -name m_no_bypass {{!bypass_uniso}}
check_lp -stage rtl
check_lp -prove_waivers
puts "===WAIVED_STEP4_BEGIN==="
puts [report_lp -status formally_waived -json]
puts "===WAIVED_STEP4_END==="
stopat u_src.src_data
puts "===OPEN_STEP5_BEGIN==="
puts [report_lp -status open -json]
puts "===OPEN_STEP5_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=120.0)
    assert_exit(res, 0, "F8_T2_03 auto-waiver invalidation on assume/stopat")

    w1 = _lp_violations(_parse_json_block(res.stdout, "WAIVED_STEP1"))
    if not w1:
        raise AssertionError("Step 1: expected FormallyWaived under m_no_bypass assumption")

    o2 = _lp_violations(_parse_json_block(res.stdout, "OPEN_STEP2"))
    w2 = _lp_violations(_parse_json_block(res.stdout, "WAIVED_STEP2"))
    if not o2 or w2:
        raise AssertionError(
            f"Step 2: expected assume -remove to invalidate FormallyWaived -> Open; open={o2!r}, waived={w2!r}"
        )

    c3 = _lp_violations(_parse_json_block(res.stdout, "CEX_STEP3"))
    if not c3:
        raise AssertionError("Step 3: expected FormalCex after re-proving without m_no_bypass")

    w4 = _lp_violations(_parse_json_block(res.stdout, "WAIVED_STEP4"))
    o5 = _lp_violations(_parse_json_block(res.stdout, "OPEN_STEP5"))
    if not w4 or not o5:
        raise AssertionError(
            f"Step 4-5: expected stopat to invalidate FormallyWaived -> Open; w4={w4!r}, o5={o5!r}"
        )


def _case_f8_t2_04(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("f7_f10/f8/unmasked_leak.sv")
    upf = ctx.copy_fixture("f7_f10/f8/unmasked_leak.upf")
    shell_workdir = ctx.path("shell_work")
    tcl = ctx.write_file(
        "run.tcl",
        f"""
clear -all
analyze -sv12 {sv.name}
elaborate -top unmasked_leak
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
check_lp -stage rtl
puts "===OPEN_BEFORE_BEGIN==="
puts [report_lp -status open -json]
puts "===OPEN_BEFORE_END==="
set_prove_time_limit 30s
check_lp -prove_waivers
puts "===WAIVED_AFTER_BEGIN==="
puts [report_lp -status formally_waived -json]
puts "===WAIVED_AFTER_END==="
puts "===CEX_AFTER_BEGIN==="
puts [report_lp -status cex -json]
puts "===CEX_AFTER_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, workdir=shell_workdir, timeout_s=90.0)
    assert_exit(res, 0, "F8_T2_04 genuine leak promoted to FormalCex")

    open_before = _lp_violations(_parse_json_block(res.stdout, "OPEN_BEFORE"))
    if not any("ISO_MISSING" in _norm_rule(v) for v in open_before):
        raise AssertionError(f"Expected initial ISO_MISSING open violation, got {open_before!r}")

    waived_after = _lp_violations(_parse_json_block(res.stdout, "WAIVED_AFTER"))
    if waived_after:
        raise AssertionError(f"Unmasked leak must NOT be FormallyWaived, got {waived_after!r}")

    cex_after = _lp_violations(_parse_json_block(res.stdout, "CEX_AFTER"))
    if not cex_after:
        raise AssertionError("Expected unmasked ISO_MISSING promoted to FormalCex")
    trace_raw = next((str(v.get("trace")) for v in cex_after if v.get("trace")), "")
    if not trace_raw:
        raise AssertionError(f"Expected VCD trace path on promoted CEX: {cex_after!r}")
    vcd = parse_vcd_file(_resolve_workdir_file(shell_workdir, trace_raw))
    if not signals_diverge(vcd):
        raise AssertionError("Expected m0/m1 divergence on promoted FormalCex VCD")


def _case_f8_t2_05(ctx: E2EContext) -> None:
    golden = ctx.copy_fixture("f7_f10/f8/golden_soc.sv")
    bug_sv = ctx.copy_fixture("f7_f10/f8/bug_pa_sec_ret.sv")
    bug_upf = ctx.copy_fixture("f7_f10/f8/bug_pa_sec_ret.upf")
    shell_workdir = ctx.path("shell_work")
    tcl = ctx.write_file(
        "run.tcl",
        f"""
clear -all
analyze -sv12 {bug_sv.name}
elaborate -top bug_pa_sec_ret
clock clk
reset -expression {{!rst_n}}
load_upf {bug_upf.name}
set_prove_time_limit 30s
check_sec -setup -spec {golden.name} -spec_top golden_soc -imp {bug_sv.name} -imp_top bug_pa_sec_ret -clock clk -reset {{!rst_n}}
check_lp -sec
puts "===SEC_JSON_BEGIN==="
puts [check_sec -report -json]
puts "===SEC_JSON_END==="
puts "===WHY_SEC_BEGIN==="
puts [why_cex -property sec_out_dout -json]
puts "===WHY_SEC_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, workdir=shell_workdir, timeout_s=120.0)
    assert_exit(res, 0, "F8_T2_05 check_lp -sec sleep/wake mismatch")

    sec_state = _parse_json_block(res.stdout, "SEC_JSON")
    checks = sec_state.get("checks") or []
    dout_chk = next((c for c in checks if c.get("name") == "sec_out_dout"), None)
    if dout_chk is None:
        raise AssertionError(f"Expected sec_out_dout in SEC checks: {checks!r}")
    assert_eq(_norm_status(dout_chk.get("status")), "cex", "sec_out_dout status")

    trace_raw = str(dout_chk.get("trace") or "")
    vcd_path = _resolve_workdir_file(shell_workdir, trace_raw) if trace_raw else _find_vcd_for_prop(shell_workdir, "sec_out_dout")
    vcd = parse_vcd_file(vcd_path)
    if not signals_diverge(vcd, prefix_a="spec.", prefix_b="imp."):
        raise AssertionError(f"Expected spec.*/imp.* divergence in SEC VCD {vcd_path}")

    why = _parse_json_block(res.stdout, "WHY_SEC")
    assert_eq(_norm_status(why.get("status")), "cex", "why_cex SEC status")
    if not (why.get("events") or []):
        raise AssertionError(f"Expected non-empty causal events in SEC why_cex: {why!r}")


# ---------------------------------------------------------------------------
# Feature Area F9 Tier 2: Liberty Errors, Stage 3-4 Rules, Inverted Clamp LEC
# ---------------------------------------------------------------------------


def _case_f9_t2_01(ctx: E2EContext) -> None:
    bad_lib = ctx.copy_fixture("f7_f10/f9/malformed_syntax.lib")
    res_syn = ctx.run_shell_commands(f"read_liberty {bad_lib.name}", timeout_s=30.0)
    if res_syn.returncode == 0:
        raise AssertionError("Expected non-zero exit code for malformed_syntax.lib")
    assert_in("14", res_syn.combined, "F9_T2_01 malformed_syntax.lib line number 14")

    res_missing = ctx.run_shell_commands("read_liberty nonexistent_missing.lib", timeout_s=30.0)
    if res_missing.returncode == 0:
        raise AssertionError("Expected non-zero exit code for nonexistent_missing.lib")
    assert_in("nonexistent_missing.lib", res_missing.combined, "F9_T2_01 missing liberty file")


def _case_f9_t2_02(ctx: E2EContext) -> None:
    ctx.write_file("mini.lib", generate_mini_liberty())
    ctx.write_file("stdcells.v", generate_stdcell_verilog())
    ctx.write_file("gate_bugs.v", generate_gate_pg_netlist(num_cells=50, include_bugs=True))
    ctx.write_file("gate.upf", generate_gate_pg_upf())
    extra_lib = ctx.copy_fixture("f7_f10/f9/extra_cells.lib")
    extra_v = ctx.copy_fixture("f7_f10/f9/extra_stdcells.v")
    s3_v = ctx.copy_fixture("f7_f10/f9/stage3_bugs_extra.v")
    s3_upf = ctx.copy_fixture("f7_f10/f9/stage3_bugs_extra.upf")

    tcl_a = ctx.write_file(
        "run_a.tcl",
        """
clear -all
read_liberty mini.lib
analyze -sv12 stdcells.v gate_bugs.v
elaborate -top soc_pg_top
load_upf gate.upf
check_lp -stage netlist
puts "===S3A_BEGIN==="
puts [report_lp -json]
puts "===S3A_END==="
""".strip()
        + "\n",
    )
    res_a = ctx.run_shell_file(tcl_a, workdir=ctx.path("work_a"), timeout_s=60.0)
    assert_exit(res_a, 0, "F9_T2_02 Stage 3 sub-check (a)")

    tcl_b = ctx.write_file(
        "run_b.tcl",
        f"""
clear -all
read_liberty mini.lib {extra_lib.name}
analyze -sv12 stdcells.v {extra_v.name} {s3_v.name}
elaborate -top stage3_bugs_extra
load_upf {s3_upf.name}
check_lp -stage netlist
puts "===S3B_BEGIN==="
puts [report_lp -json]
puts "===S3B_END==="
""".strip()
        + "\n",
    )
    res_b = ctx.run_shell_file(tcl_b, workdir=ctx.path("work_b"), timeout_s=60.0)
    assert_exit(res_b, 0, "F9_T2_02 Stage 3 sub-check (b)")

    v_all = _lp_violations(_parse_json_block(res_a.stdout, "S3A")) + _lp_violations(
        _parse_json_block(res_b.stdout, "S3B")
    )
    open_rules = {_norm_rule(v) for v in v_all if _norm_status(v.get("status")) == "open"}
    required = {
        "ISO_CELL_MISSING",
        "ISO_LOGIC_BEFORE_CELL",
        "AON_NON_AON_BUFFER",
        "LS_DIR_CELL_MISMATCH",
        "RET_CELL_UNMAPPED",
    }
    missing = sorted(required - open_rules)
    if missing:
        raise AssertionError(
            f"Missing required open Stage 3 rules {missing!r}; observed open_rules={sorted(open_rules)!r}"
        )


def _case_f9_t2_03(ctx: E2EContext) -> None:
    ctx.write_file("mini.lib", generate_mini_liberty())
    ctx.write_file("stdcells.v", generate_stdcell_verilog())
    extra_lib = ctx.copy_fixture("f7_f10/f9/extra_cells.lib")
    extra_v = ctx.copy_fixture("f7_f10/f9/extra_stdcells.v")
    macro_v = ctx.copy_fixture("f7_f10/f9/macro_pin_crossing.v")
    macro_upf = ctx.copy_fixture("f7_f10/f9/macro_pin_crossing.upf")

    tcl = ctx.write_file(
        "run_macro.tcl",
        f"""
clear -all
read_liberty mini.lib {extra_lib.name}
analyze -sv12 stdcells.v {extra_v.name} {macro_v.name}
elaborate -top macro_pin_crossing
load_upf {macro_upf.name}
check_lp -stage netlist
puts "===MACRO_BEGIN==="
puts [report_lp -json]
puts "===MACRO_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, timeout_s=60.0)
    assert_exit(res, 0, "F9_T2_03 MACRO_PIN_SUPPLY_CROSSING")

    violations = _lp_violations(_parse_json_block(res.stdout, "MACRO"))
    hits = [
        v
        for v in violations
        if _norm_rule(v) == "MACRO_PIN_SUPPLY_CROSSING" and _norm_status(v.get("status")) == "open"
    ]
    if not hits:
        raise AssertionError(f"Expected open MACRO_PIN_SUPPLY_CROSSING violation, got {violations!r}")
    blob = json.dumps(hits)
    if "u_sram" not in blob and "DOUT" not in blob and "DIN" not in blob:
        raise AssertionError(f"Expected macro instance/pin detail in {hits!r}")


def _case_f9_t2_04(ctx: E2EContext) -> None:
    ctx.write_file("mini.lib", generate_mini_liberty())
    ctx.write_file("stdcells.v", generate_stdcell_verilog())
    ctx.write_file("gate_bugs.v", generate_gate_pg_netlist(num_cells=50, include_bugs=True))
    ctx.write_file("gate.upf", generate_gate_pg_upf())
    short_v = ctx.copy_fixture("f7_f10/f9/pg_short.v")
    short_upf = ctx.copy_fixture("f7_f10/f9/pg_short.upf")

    tcl_a = ctx.write_file(
        "run_pg_a.tcl",
        """
clear -all
read_liberty mini.lib
analyze -sv12 stdcells.v gate_bugs.v
elaborate -top soc_pg_top
load_upf gate.upf
check_lp -stage pg
puts "===PGA_BEGIN==="
puts [report_lp -json]
puts "===PGA_END==="
""".strip()
        + "\n",
    )
    res_a = ctx.run_shell_file(tcl_a, workdir=ctx.path("work_pg_a"), timeout_s=60.0)
    assert_exit(res_a, 0, "F9_T2_04 Stage 4 sub-check (a)")

    tcl_b = ctx.write_file(
        "run_pg_b.tcl",
        f"""
clear -all
read_liberty mini.lib
analyze -sv12 stdcells.v {short_v.name}
elaborate -top pg_short
load_upf {short_upf.name}
check_lp -stage pg
puts "===PGB_BEGIN==="
puts [report_lp -json]
puts "===PGB_END==="
""".strip()
        + "\n",
    )
    res_b = ctx.run_shell_file(tcl_b, workdir=ctx.path("work_pg_b"), timeout_s=60.0)
    assert_exit(res_b, 0, "F9_T2_04 Stage 4 sub-check (b)")

    v_all = _lp_violations(_parse_json_block(res_a.stdout, "PGA")) + _lp_violations(
        _parse_json_block(res_b.stdout, "PGB")
    )
    open_rules = {_norm_rule(v) for v in v_all if _norm_status(v.get("status")) == "open"}
    required = {
        "PG_PIN_UNCONNECTED",
        "PG_SUPPLY_MISMATCH",
        "PG_BACKUP_MISMATCH",
        "PG_SHORT",
    }
    missing = sorted(required - open_rules)
    if missing:
        raise AssertionError(
            f"Missing required open Stage 4 PG rules {missing!r}; observed open_rules={sorted(open_rules)!r}"
        )


def _case_f9_t2_05(ctx: E2EContext) -> None:
    ctx.write_file("mini.lib", generate_mini_liberty())
    ctx.write_file("stdcells.v", generate_stdcell_verilog())
    extra_lib = ctx.copy_fixture("f7_f10/f9/extra_cells.lib")
    extra_v = ctx.copy_fixture("f7_f10/f9/extra_stdcells.v")
    rtl = ctx.copy_fixture("f7_f10/f9/lec_rtl.sv")
    gate_bad = ctx.copy_fixture("f7_f10/f9/lec_gate_bad_clamp.v")
    upf = ctx.copy_fixture("f7_f10/f9/lec.upf")
    shell_workdir = ctx.path("shell_work")

    tcl = ctx.write_file(
        "run_lec_bad.tcl",
        f"""
clear -all
read_liberty mini.lib {extra_lib.name}
analyze -sv12 {rtl.name}
elaborate -top lec_top
clock clk
reset -expression {{!rst_n}}
load_upf {upf.name}
check_sec -setup -lec -spec {rtl.name} -spec_top lec_top -imp {{stdcells.v {extra_v.name} {gate_bad.name}}} -imp_top lec_top -clock clk -reset {{!rst_n}}
check_lp -lec
puts "===LEC_BAD_SEC_BEGIN==="
puts [check_sec -report -json]
puts "===LEC_BAD_SEC_END==="
""".strip()
        + "\n",
    )
    res = ctx.run_shell_file(tcl, workdir=shell_workdir, timeout_s=90.0)
    assert_exit(res, 0, "F9_T2_05 check_lp -lec inverted clamp")

    sec_state = _parse_json_block(res.stdout, "LEC_BAD_SEC")
    checks = sec_state.get("checks") or []
    dout_chk = next((c for c in checks if "dout" in str(c.get("name", ""))), None)
    if dout_chk is None:
        raise AssertionError(f"Expected dout check in LEC report: {checks!r}")
    assert_eq(_norm_status(dout_chk.get("status")), "cex", "inverted clamp LEC dout status")

    trace_raw = str(dout_chk.get("trace") or "")
    vcd_path = _resolve_workdir_file(shell_workdir, trace_raw) if trace_raw else _find_vcd_for_prop(shell_workdir, str(dout_chk["name"]))
    vcd = parse_vcd_file(vcd_path)
    if not signals_diverge(vcd, prefix_a="spec.", prefix_b="imp."):
        raise AssertionError(f"Expected spec/imp divergence in inverted-clamp VCD {vcd_path}")

    spec_dout = vcd.get_signal("spec.dout")
    imp_dout = vcd.get_signal("imp.dout")
    iso_sigs = vcd.find_signals("*iso_en*")
    if spec_dout and imp_dout and iso_sigs:
        clamp_mismatch = any(
            vcd.value_at(iso_sigs[0], t) == "1"
            and vcd.value_at(spec_dout, t) == "0"
            and vcd.value_at(imp_dout, t) == "1"
            for t in vcd.times
        )
        if not clamp_mismatch:
            raise AssertionError("Expected spec.dout==0 vs imp.dout==1 while iso_en==1 in VCD")


def _case_f9_t2_06(ctx: E2EContext) -> None:
    ctx.write_file("stdcells.v", generate_stdcell_verilog())
    ctx.write_file("gate_clean.v", generate_gate_pg_netlist(num_cells=20, include_bugs=False))
    ctx.write_file("gate.upf", generate_gate_pg_upf())

    # (a) check_lp -stage netlist without read_liberty
    res_no_lib = ctx.run_shell_commands(
        [
            "clear -all",
            "analyze -sv12 stdcells.v gate_clean.v",
            "elaborate -top soc_pg_top",
            "load_upf gate.upf",
            "check_lp -stage netlist",
        ],
        timeout_s=45.0,
    )
    if res_no_lib.returncode == 0:
        raise AssertionError("Expected non-zero rc for check_lp -stage netlist without read_liberty")

    # (b) invalid stage
    res_bad_stage = ctx.run_shell_commands(
        [
            "clear -all",
            "analyze -sv12 stdcells.v gate_clean.v",
            "elaborate -top soc_pg_top",
            "load_upf gate.upf",
            "check_lp -stage bogus_stage",
        ],
        timeout_s=45.0,
    )
    if res_bad_stage.returncode == 0:
        raise AssertionError("Expected non-zero rc for check_lp -stage bogus_stage")

    # (c) read_liberty with no args
    res_empty_lib = ctx.run_shell_commands("read_liberty", timeout_s=30.0)
    if res_empty_lib.returncode == 0:
        raise AssertionError("Expected non-zero rc for read_liberty with no arguments")

    # (d) --check --json flags unknown check_lp option
    bad_tcl = ctx.write_file("bad_opt.tcl", "check_lp -bogus_opt\n")
    chk = ctx.run_shell_check(bad_tcl, json_out=True, timeout_s=30.0)
    if chk.returncode == 0:
        raise AssertionError("Expected --check to reject unknown check_lp -bogus_opt")
    chk_json = chk.json()
    findings = chk_json.get("findings") or []
    cmds = chk_json.get("commands") or {}
    rejected_cnt = (cmds.get("check_lp") or {}).get("rejected", 0) if isinstance(cmds, dict) else 0
    if not findings and rejected_cnt == 0:
        raise AssertionError(
            f"Expected non-empty findings or rejected check_lp for unknown option: {chk_json!r}"
        )


# ---------------------------------------------------------------------------
# Feature Area F10 Tier 2: Read-Only Mode, Paths, Invalid Args, VCD, Docgen, D13
# ---------------------------------------------------------------------------


def _case_f10_t2_01(ctx: E2EContext) -> None:
    ctx.copy_fixture("f7_f10/f10/soc_lp_clean.upf")
    with ctx.mcp(mode="read-only", root=ctx.workdir) as client:
        init_res = client.initialize(protocol_version="2025-06-18")
        assert_in("read-only mode", str(init_res.get("instructions") or ""), "read-only instructions")

        names = client.tool_names()
        if "load_upf" in names or "check_lp" in names:
            raise AssertionError(f"Expected load_upf and check_lp hidden in read-only mode, got {sorted(names)!r}")
        if "lp_diagnose" not in names:
            raise AssertionError(f"Expected lp_diagnose visible in read-only mode, got {sorted(names)!r}")

        for mut_tool, args in (("load_upf", {"file": "soc_lp_clean.upf"}), ("check_lp", {"stage": "rtl"})):
            try:
                err = client.call_tool(mut_tool, args, expect_error=True)
            except McpProtocolError as exc:
                if exc.code != -32602 or "unknown tool" not in str(exc).lower():
                    raise AssertionError(
                        f"Expected JSON-RPC -32602 'unknown tool' or informal.error/1 'mode' "
                        f"for {mut_tool} in read-only mode, got {exc.code}: {exc.message}"
                    ) from exc
            else:
                assert_eq(err.get("schema"), "informal.error/1", f"{mut_tool} error schema")
                assert_eq(err.get("code"), "mode", f"{mut_tool} read-only error code")

        diag_err = client.call_tool("lp_diagnose", {"id": "LP-0001"}, expect_error=True)
        assert_eq(diag_err.get("schema"), "informal.error/1", "lp_diagnose error schema")
        if diag_err.get("code") in {"mode"}:
            raise AssertionError(f"lp_diagnose must be allowed in read-only mode, got {diag_err!r}")


def _case_f10_t2_02(ctx: E2EContext) -> None:
    ctx.copy_fixture("f7_f10/f10/soc_lp_clean.sv")
    with ctx.mcp(mode="auto", root=ctx.workdir) as client:
        client.call_tool(
            "session_open",
            {"files": ["soc_lp_clean.sv"], "top": "soc_lp", "clock": "clk", "reset": "!rst_n"},
        )

        for outside in ("/etc/passwd", "../../../etc/passwd"):
            err = client.call_tool("load_upf", {"file": outside}, expect_error=True)
            assert_eq(err.get("schema"), "informal.error/1", f"outside root schema ({outside})")
            assert_eq(err.get("code"), "path", f"outside root error code ({outside})")

        missing_err = client.call_tool("load_upf", {"file": "missing_intent.upf"}, expect_error=True)
        assert_eq(missing_err.get("schema"), "informal.error/1", "missing UPF schema")
        if missing_err.get("code") not in {"not_found", "path"}:
            raise AssertionError(
                f"Expected error code in {{'not_found', 'path'}} for missing UPF, got {missing_err!r}"
            )
        assert_in(
            "missing_intent.upf",
            str(missing_err.get("message") or ""),
            "missing UPF error message",
        )


def _case_f10_t2_03(ctx: E2EContext) -> None:
    ctx.copy_fixture("f7_f10/f10/soc_lp_clean.sv")
    ctx.copy_fixture("f7_f10/f10/soc_lp_clean.upf")
    with ctx.mcp(mode="auto", root=ctx.workdir) as client:
        client.call_tool(
            "session_open",
            {"files": ["soc_lp_clean.sv"], "top": "soc_lp", "clock": "clk", "reset": "!rst_n"},
        )

        no_upf_err = client.call_tool("check_lp", {"stage": "rtl"}, expect_error=True)
        if no_upf_err.get("code") not in {"rejected", "not_found"}:
            raise AssertionError(
                f"Expected rejected/not_found before load_upf, got {no_upf_err!r}"
            )

        client.call_tool("load_upf", {"file": "soc_lp_clean.upf"})

        bad_stage_err = client.call_tool("check_lp", {"stage": "bogus_stage"}, expect_error=True)
        assert_eq(bad_stage_err.get("schema"), "informal.error/1", "invalid stage schema")
        assert_eq(bad_stage_err.get("code"), "arguments", "invalid stage error code")

        bad_type_err = client.call_tool("check_lp", {"stage": 42}, expect_error=True)
        assert_eq(bad_type_err.get("code"), "arguments", "wrong stage type error code")


def _case_f10_t2_04(ctx: E2EContext) -> None:
    lp_vcd = parse_vcd_file(ctx.fixture("f7_f10/f10/lp_swimlane_cex.vcd"))
    if not has_lp_domain_signals(lp_vcd):
        raise AssertionError("Expected has_lp_domain_signals(lp_vcd) == True")
    if not has_lp_domain_signals(lp_vcd, "PD_CORE"):
        raise AssertionError("Expected has_lp_domain_signals(lp_vcd, 'PD_CORE') == True")
    if has_lp_domain_signals(lp_vcd, "PD_UNKNOWN"):
        raise AssertionError("Expected has_lp_domain_signals(lp_vcd, 'PD_UNKNOWN') == False")
    assert_eq(lp_domain_names(lp_vcd), ["PD_CORE", "PD_RET"], "lp_domain_names")

    plain_vcd = parse_vcd_file(ctx.repo_root / "ui" / "tests" / "fixtures" / "a_never9.vcd")
    if has_lp_domain_signals(plain_vcd):
        raise AssertionError("Expected has_lp_domain_signals(a_never9.vcd) == False")
    assert_eq(lp_domain_names(plain_vcd), [], "plain VCD lp_domain_names")

    miter_no_lp = parse_vcd(
        """$timescale 1ns $end
$scope module m0 $end
$var wire 1 ! dout $end
$upscope $end
$scope module m1 $end
$var wire 1 " dout $end
$upscope $end
$enddefinitions $end
#0
0!
1"
#10
"""
    )
    if has_lp_domain_signals(miter_no_lp):
        raise AssertionError("Expected has_lp_domain_signals(miter_no_lp) == False")
    if not signals_diverge(miter_no_lp):
        raise AssertionError("Expected miter_no_lp m0/m1 divergence == True")


def _case_f10_t2_05(ctx: E2EContext) -> None:
    repo_chk = ctx.run_docgen_check()
    assert_exit(repo_chk, 0, "F10_T2_05 repo docgen --check")

    ai_doc = (ctx.repo_root / "docs" / "ai-interface.md").read_text(encoding="utf-8")
    mcp_doc = (ctx.repo_root / "docs" / "reference" / "mcp-tools.md").read_text(encoding="utf-8")
    spec_doc = (ctx.repo_root / "docs" / "reference" / "spec-schema.md").read_text(encoding="utf-8")

    for tok in ("load_upf", "check_lp", "lp_diagnose"):
        assert_in(f"`{tok}`", ai_doc, f"docs/ai-interface.md missing {tok}")
        assert_in(f"`{tok}`", mcp_doc, f"docs/reference/mcp-tools.md missing {tok}")
    assert_in("audit_low_power", mcp_doc, "mcp-tools.md missing audit_low_power")
    assert_in("`lp`", mcp_doc, "mcp-tools.md missing gui_set_view lp")
    assert_in("low_power", spec_doc, "spec-schema.md missing low_power gates")

    # Positive & negative drift check in isolated workdir copy
    doc_out = ctx.path("doc_ref")
    llms_out = ctx.path("llms.txt")
    doc_out.mkdir(parents=True, exist_ok=True)
    shutil.copytree(ctx.repo_root / "docs" / "reference", doc_out, dirs_exist_ok=True)
    shutil.copy2(ctx.repo_root / "llms.txt", llms_out)

    pos_chk = ctx.run_docgen_check(out=doc_out, llms_out=llms_out)
    assert_exit(pos_chk, 0, "F10_T2_05 clean workdir docgen --check")

    (doc_out / "spec-schema.md").write_text("# drifted spec-schema\n", encoding="utf-8")
    (doc_out / "mcp-tools.md").write_text("# drifted mcp-tools\n", encoding="utf-8")
    neg_chk = ctx.run_docgen_check(out=doc_out, llms_out=llms_out)
    if neg_chk.returncode == 0:
        raise AssertionError("Expected non-zero exit code when generated docs are drifted")
    assert_in("out of date", neg_chk.combined.lower(), "F10_T2_05 drift message")


def _case_f10_t2_06(ctx: E2EContext) -> None:
    ctx.copy_fixture("f7_f10/f10/soc_lp_clean.sv")
    ctx.copy_fixture("f7_f10/f10/bad_d13.upf")
    spec_path = ctx.write_json(
        "fvp.spec.json",
        {
            "schema": "informal.spec/1",
            "name": "LP Gate Failure Spec",
            "design": {"top": "soc_lp", "clocks": ["clk"], "reset": "!rst_n"},
            "requirements": [
                {
                    "id": "REQ-LP-GATE",
                    "title": "Low power gate check",
                    "assertions": [],
                    "covers": [],
                }
            ],
            "signoff": {"proof_target": "proven"},
            "low_power": {
                "require_upf": True,
                "max_open_lp_errors": 0,
            },
        },
    )

    with ctx.mcp(mode="auto", root=ctx.workdir) as client:
        client.call_tool(
            "session_open",
            {"files": ["soc_lp_clean.sv"], "top": "soc_lp", "clock": "clk", "reset": "!rst_n"},
        )

        # 1. Unknown violation ID in lp_diagnose -> not_found
        diag_err = client.call_tool("lp_diagnose", {"id": "LP-NO-SUCH-999"}, expect_error=True)
        assert_eq(diag_err.get("schema"), "informal.error/1", "lp_diagnose unknown id schema")
        assert_eq(diag_err.get("code"), "not_found", "lp_diagnose unknown id code")

        # 2. Decision D13 unsupported UPF semantic option -> rejected with file:line / option info
        d13_err = client.call_tool("load_upf", {"file": "bad_d13.upf"}, expect_error=True)
        assert_eq(d13_err.get("schema"), "informal.error/1", "bad_d13 load_upf schema")
        assert_eq(d13_err.get("code"), "rejected", "bad_d13 load_upf code")
        msg = str(d13_err.get("message") or "")
        if "value_conversion" not in msg and "bad_d13.upf" not in msg:
            raise AssertionError(f"Expected D13 error message mentioning option/file, got {d13_err!r}")

        # 3. spec_audit with low_power gate when no UPF is loaded -> signoff_ready == False
        audit = client.call_tool("spec_audit", {"spec_file": spec_path.name})
        assert_eq(audit.get("signoff_ready"), False, "spec_audit signoff_ready without UPF")
        gates = audit.get("gates") or []
        lp_gate = next(
            (g for g in gates if isinstance(g, dict) and g.get("gate") == "low_power"),
            None,
        )
        if lp_gate is None:
            raise AssertionError(f"Expected low_power gate entry in spec_audit gates: {audit!r}")
        assert_eq(lp_gate.get("ok"), False, "spec_audit low_power gate ok without UPF")


def get_cases() -> list[TestCase]:
    """Return all 23 Tier 2 E2E test cases for F7-F10."""
    return [
        # F7 (6)
        TestCase(
            id="F7_T2_01",
            tier=2,
            feature="F7",
            title="pwr_sw turns OFF before iso_en (lp_iso_before_pwroff CEX + VCD)",
            fn=_case_f7_t2_01,
            timeout_s=90.0,
            tags=("f7", "cex", "pmu", "vcd"),
        ),
        TestCase(
            id="F7_T2_02",
            tier=2,
            feature="F7",
            title="iso_en drops before pwr_ack settles (lp_iso_hold_after_pwron CEX + VCD)",
            fn=_case_f7_t2_02,
            timeout_s=90.0,
            tags=("f7", "cex", "pmu", "vcd"),
        ),
        TestCase(
            id="F7_T2_03",
            tier=2,
            feature="F7",
            title="Unisolated async reset firing during retention (lp_ret_async_rst_clean CEX)",
            fn=_case_f7_t2_03,
            timeout_s=90.0,
            tags=("f7", "cex", "retention", "async_reset"),
        ),
        TestCase(
            id="F7_T2_04",
            tier=2,
            feature="F7",
            title="PMU enters -illegal PST state (lp_pst_no_illegal CEX + why_cex)",
            fn=_case_f7_t2_04,
            timeout_s=90.0,
            tags=("f7", "cex", "pst", "why_cex"),
        ),
        TestCase(
            id="F7_T2_05",
            tier=2,
            feature="F7",
            title="Computed/gated-clock non-retained flop corruption across sleep",
            fn=_case_f7_t2_05,
            timeout_s=90.0,
            tags=("f7", "cex", "gated_clock", "computed_regs"),
        ),
        TestCase(
            id="F7_T2_06",
            tier=2,
            feature="F7",
            title="CORRUPT_ON_ACTIVITY corruption on un-gated toggle",
            fn=_case_f7_t2_06,
            timeout_s=90.0,
            tags=("f7", "cex", "coa", "simstate"),
        ),
        # F8 (5)
        TestCase(
            id="F8_T2_01",
            tier=2,
            feature="F8",
            title="Partial-retention post-wakeup X-leakage (RET_POST_WAKEUP_X + VCD)",
            fn=_case_f8_t2_01,
            timeout_s=90.0,
            tags=("f8", "cex", "partial_ret", "vcd"),
        ),
        TestCase(
            id="F8_T2_02",
            tier=2,
            feature="F8",
            title="Circular mutual-masking w1 ^ w2 not falsely auto-waived",
            fn=_case_f8_t2_02,
            timeout_s=90.0,
            tags=("f8", "waivers", "circular", "cex"),
        ),
        TestCase(
            id="F8_T2_03",
            tier=2,
            feature="F8",
            title="Auto-waiver invalidation on assume/stopat changes (FormallyWaived -> Open)",
            fn=_case_f8_t2_03,
            timeout_s=120.0,
            tags=("f8", "waivers", "invalidation"),
        ),
        TestCase(
            id="F8_T2_04",
            tier=2,
            feature="F8",
            title="Genuine leak on unmasked crossing promoted from Open to FormalCex with VCD",
            fn=_case_f8_t2_04,
            timeout_s=90.0,
            tags=("f8", "waivers", "cex", "vcd"),
        ),
        TestCase(
            id="F8_T2_05",
            tier=2,
            feature="F8",
            title="check_lp -sec catches golden vs power-aware sleep/wake state mismatch",
            fn=_case_f8_t2_05,
            timeout_s=120.0,
            tags=("f8", "sec", "lp_sec", "cex"),
        ),
        # F9 (6)
        TestCase(
            id="F9_T2_01",
            tier=2,
            feature="F9",
            title="read_liberty malformed .lib syntax error reports exact line 14 & missing file",
            fn=_case_f9_t2_01,
            timeout_s=45.0,
            tags=("f9", "liberty", "negative"),
        ),
        TestCase(
            id="F9_T2_02",
            tier=2,
            feature="F9",
            title="Stage 3 catches ISO_CELL_MISSING, ISO_LOGIC_BEFORE_CELL, AON_NON_AON_BUFFER, LS_DIR_CELL_MISMATCH, RET_CELL_UNMAPPED",
            fn=_case_f9_t2_02,
            timeout_s=60.0,
            tags=("f9", "netlist", "stage3"),
        ),
        TestCase(
            id="F9_T2_03",
            tier=2,
            feature="F9",
            title="Stage 3 catches MACRO_PIN_SUPPLY_CROSSING on dual-rail SRAM macro",
            fn=_case_f9_t2_03,
            timeout_s=60.0,
            tags=("f9", "netlist", "macro", "stage3"),
        ),
        TestCase(
            id="F9_T2_04",
            tier=2,
            feature="F9",
            title="Stage 4 catches PG_PIN_UNCONNECTED, PG_SUPPLY_MISMATCH, PG_BACKUP_MISMATCH, PG_SHORT",
            fn=_case_f9_t2_04,
            timeout_s=60.0,
            tags=("f9", "pg", "stage4"),
        ),
        TestCase(
            id="F9_T2_05",
            tier=2,
            feature="F9",
            title="check_lp -lec catches inverted ISO clamp (0 vs 1) between RTL+UPF and Netlist+UPF",
            fn=_case_f9_t2_05,
            timeout_s=90.0,
            tags=("f9", "lec", "lp_lec", "cex"),
        ),
        TestCase(
            id="F9_T2_06",
            tier=2,
            feature="F9",
            title="Stage 3-4 negative guards (missing read_liberty, bogus stage, empty args)",
            fn=_case_f9_t2_06,
            timeout_s=60.0,
            tags=("f9", "negative", "guards"),
        ),
        # F10 (6)
        TestCase(
            id="F10_T2_01",
            tier=2,
            feature="F10",
            title="MCP --mode read-only hides load_upf/check_lp, rejects with mode, allows lp_diagnose",
            fn=_case_f10_t2_01,
            timeout_s=45.0,
            tags=("f10", "mcp", "read_only"),
        ),
        TestCase(
            id="F10_T2_02",
            tier=2,
            feature="F10",
            title="MCP load_upf rejects outside-root (path) and missing file (not_found)",
            fn=_case_f10_t2_02,
            timeout_s=45.0,
            tags=("f10", "mcp", "path", "not_found"),
        ),
        TestCase(
            id="F10_T2_03",
            tier=2,
            feature="F10",
            title="MCP check_lp rejects invalid stage/types (arguments) and uninitialized LP (rejected)",
            fn=_case_f10_t2_03,
            timeout_s=45.0,
            tags=("f10", "mcp", "arguments"),
        ),
        TestCase(
            id="F10_T2_04",
            tier=2,
            feature="F10",
            title="VCD lp.pd.* vs non-LP VCD boundary (vcd_check.has_lp_domain_signals)",
            fn=_case_f10_t2_04,
            timeout_s=30.0,
            tags=("f10", "vcd", "swimlane"),
        ),
        TestCase(
            id="F10_T2_05",
            tier=2,
            feature="F10",
            title="docgen --check verifies docs sync & catches drifted mcp-tools/spec-schema",
            fn=_case_f10_t2_05,
            timeout_s=45.0,
            tags=("f10", "docgen", "docs"),
        ),
        TestCase(
            id="F10_T2_06",
            tier=2,
            feature="F10",
            title="MCP lp_diagnose unknown ID, D13 unsupported UPF flag & spec_audit low_power gate failure",
            fn=_case_f10_t2_06,
            timeout_s=45.0,
            tags=("f10", "mcp", "d13", "spec_audit"),
        ),
    ]
