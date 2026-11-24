"""Tier 4 real-world SoC workload E2E test cases (`T4-W1` .. `T4-W7`).

Opaque-box end-to-end SoC signoff workloads across `F1..F10` exercised via
`informal lower`, `informal shell`, `informal mcp`, `informal docgen`,
`vcd_check`, and `ui/` verification gates. Self-contained (`stdlib` + `lib.*`).
"""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

from lib.cli_client import run_command
from lib.fixtures_gen import (
    generate_gate_pg_netlist,
    generate_gate_pg_upf,
    generate_mini_liberty,
    generate_n_domain_upf,
    generate_stdcell_verilog,
)
from lib.harness import (
    E2EContext,
    TestCase,
    assert_eq,
    assert_exit,
    assert_in,
    assert_json_schema_field,
)
from lib.vcd_check import (
    find_divergences,
    has_lp_domain_signals,
    parse_vcd_file,
    signals_diverge,
)


def _read_json(path: Path) -> Any:
    assert path.is_file(), f"Expected JSON output file missing: {path}"
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise AssertionError(f"Invalid JSON in {path}: {exc}") from exc


def _extract_violations(report: dict[str, Any]) -> list[dict[str, Any]]:
    for key in ("violations", "findings", "items"):
        val = report.get(key)
        if isinstance(val, list):
            return [v for v in val if isinstance(v, dict)]
    raise AssertionError(
        f"Expected violations/findings/items list in LP report, got keys {sorted(report.keys())}: {report!r}"
    )


def _norm_status(v: dict[str, Any]) -> str:
    raw = str(v.get("status", "")).strip().lower().replace("-", "_").replace(" ", "_")
    if raw in {"formalcex", "formal_cex"}:
        return "formal_cex"
    if raw in {"formallywaived", "formally_waived"}:
        return "formally_waived"
    return raw


def _rule_of(v: dict[str, Any]) -> str:
    return str(v.get("rule") or v.get("rule_id") or v.get("code") or "").strip()


def _sig_text(v: dict[str, Any]) -> str:
    parts = [
        str(v.get("signal", "")),
        str(v.get("net", "")),
        str(v.get("object", "")),
        str(v.get("crossing", "")),
        str(v.get("message", "")),
        str(v.get("detail", "")),
        json.dumps(v.get("signals", "")),
    ]
    return " ".join(parts)


def _trace_of(v: dict[str, Any]) -> str | None:
    for key in ("trace", "vcd", "trace_path", "vcd_path"):
        val = v.get(key)
        if isinstance(val, str) and val.strip():
            return val.strip()
    return None


def _resolve_workdir_path(ctx: E2EContext, raw_path: str) -> Path:
    p = Path(raw_path)
    if p.is_absolute():
        return p
    for base in (ctx.workdir, ctx.workdir / "shell_work", ctx.workdir / "mcp_work"):
        cand = base / p
        if cand.exists():
            return cand
    return ctx.workdir / p


def _find_violations(
    violations: list[dict[str, Any]],
    *,
    rule_prefix: str | None = None,
    status: str | None = None,
    needle: str | None = None,
) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for v in violations:
        if rule_prefix and not _rule_of(v).upper().startswith(rule_prefix.upper()):
            continue
        if status and _norm_status(v) != status:
            continue
        if needle and needle.lower() not in _sig_text(v).lower():
            continue
        out.append(v)
    return out


def _ui_env(ctx: E2EContext) -> dict[str, str]:
    env = dict(ctx.env)
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


def _extract_block(stdout: str, tag: str) -> str:
    begin = f"==={tag}_BEGIN==="
    end = f"==={tag}_END==="
    i = stdout.find(begin)
    j = stdout.find(end)
    assert i != -1 and j != -1 and j > i, f"Missing {begin}/{end} block in stdout:\n{stdout}"
    return stdout[i + len(begin) : j].strip()


def _is_compare_mismatch(
    combined: str,
    returncode: int,
    extra_kws: tuple[str, ...] = (),
) -> bool:
    if returncode != 0:
        return True
    cleaned = re.sub(r"\b(?:0|no)\s+differ(?:ence|ences)?\b", "", combined.lower())
    kws = ("mismatch", "not equivalent", "differ", "diverg", "missing") + extra_kws
    return any(kw in cleaned for kw in kws)


# ---------------------------------------------------------------------------
# T4-W1: Multi-Domain Mobile SoC Static LP Signoff
# ---------------------------------------------------------------------------


def _case_t4_w1(ctx: E2EContext) -> None:
    ctx.copy_fixture("workloads/t4_w1_mobile_soc/mobile_soc.sv")
    ctx.copy_fixture("workloads/t4_w1_mobile_soc/mobile_soc_clean.upf")
    ctx.copy_fixture("workloads/t4_w1_mobile_soc/mobile_soc_buggy.upf")
    ctx.copy_fixture("workloads/t4_w1_mobile_soc/mobile_soc_signoff.tcl")

    # 1. Verify RTL compiles cleanly via `informal lower` and signoff Tcl passes `--check --json`
    low_res = ctx.run_lower("mobile_soc.sv", top="mobile_soc_top")
    assert_exit(low_res, 0, "T4-W1 mobile_soc.sv lower failed")

    chk_res = ctx.run_shell_check("mobile_soc_signoff.tcl", json_out=True)
    assert_exit(chk_res, 0, "T4-W1 signoff Tcl --check failed")
    chk_json = chk_res.json()
    findings = chk_json.get("findings")
    assert isinstance(findings, list), f"Expected findings list in --check --json: {chk_json!r}"
    bad_findings = [f for f in findings if not isinstance(f, dict) or f.get("kind") != "ignored"]
    assert not bad_findings, f"Expected no error/unknown/tcl findings for mobile_soc_signoff.tcl: {chk_json!r}"
    commands = chk_json.get("commands")
    assert isinstance(commands, dict) and len(commands) > 0, (
        f"Expected non-empty commands summary in --check --json: {chk_json!r}"
    )

    # 2. Buggy UPF phase: bus grouping, LS, RET backup, ISO polarity/supply, pure feedthrough immunity
    bug_res = ctx.run_shell_commands(
        [
            "analyze -sv12 mobile_soc.sv",
            "elaborate -top mobile_soc_top",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf mobile_soc_buggy.upf",
            "check_lp -stage design",
            "set fh [open buggy.json w]; puts $fh [report_lp -json]; close $fh",
        ],
        workdir=ctx.path("sw_buggy"),
        timeout_s=90.0,
    )
    assert_exit(bug_res, 0, "T4-W1 buggy UPF check_lp failed")
    v_bug = _extract_violations(_read_json(ctx.path("buggy.json")))

    cpu_iso = _find_violations(v_bug, rule_prefix="ISO_MISSING", status="open", needle="cpu_data_bus")
    assert len(cpu_iso) == 1, f"Expected 1 grouped 64-bit ISO_MISSING on cpu_data_bus, got {len(cpu_iso)}: {cpu_iso!r}"
    bus_width = cpu_iso[0].get("width_bits") or cpu_iso[0].get("width")
    assert bus_width == 64, f"Expected grouped width 64 on cpu_data_bus, got {bus_width!r} in {cpu_iso[0]!r}"

    assert _find_violations(v_bug, rule_prefix="LS_MISSING", status="open"), (
        f"Expected open LS_MISSING on PD_CPU -> PD_IO: {v_bug!r}"
    )
    assert _find_violations(v_bug, rule_prefix="RET_BACKUP_OFF", status="open"), (
        f"Expected open RET_BACKUP_OFF on PD_SRAM: {v_bug!r}"
    )
    assert _find_violations(v_bug, rule_prefix="ISO_CTRL_", status="open") or _find_violations(
        v_bug, rule_prefix="ISO_SUPPLY_", status="open"
    ), f"Expected ISO_CTRL_* / ISO_SUPPLY_* on iso_sram: {v_bug!r}"
    assert not _find_violations(v_bug, rule_prefix="ISO_MISSING", needle="ft_o") and not _find_violations(
        v_bug, rule_prefix="ISO_MISSING", needle="u_ft"
    ), f"Pure feedthrough u_ft/ft_o falsely flagged for ISO_MISSING: {v_bug!r}"

    # 3. Clean UPF + debug_bus manual waiver lifecycle (add -> list -> verify -> remove -> open)
    clean_res = ctx.run_shell_commands(
        [
            "analyze -sv12 mobile_soc.sv",
            "elaborate -top mobile_soc_top",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf mobile_soc_clean.upf",
            "check_lp -stage design",
            "set fh0 [open pre_waive.json w]; puts $fh0 [report_lp -json]; close $fh0",
            'set wid [set_lp_waiver -add -rule ISO_MISSING -signal *debug_bus* -src_domain PD_CPU -dst_domain PD_TOP -comment "Debug bus unused in sleep"]',
            "set wlist [set_lp_waiver -list]",
            'puts "WAIVER_ID=$wid"',
            'puts "WAIVER_LIST=$wlist"',
            "set fh1 [open post_open.json w]; puts $fh1 [report_lp -status open -json]; close $fh1",
            "set fh2 [open post_waived.json w]; puts $fh2 [report_lp -status waived -json]; close $fh2",
            "set_lp_waiver -remove $wid",
            "check_lp -stage design",
            "set fh3 [open after_remove.json w]; puts $fh3 [report_lp -status open -json]; close $fh3",
        ],
        workdir=ctx.path("sw_clean"),
        timeout_s=90.0,
    )
    assert_exit(clean_res, 0, "T4-W1 clean/waiver lifecycle failed")

    v_pre = _extract_violations(_read_json(ctx.path("pre_waive.json")))
    open_pre = _find_violations(v_pre, status="open")
    assert len(open_pre) == 1 and "debug_bus" in _sig_text(open_pre[0]).lower(), (
        f"Expected ONLY debug_bus open before waiver, got: {open_pre!r}"
    )

    m_wid = re.search(r"WAIVER_ID=(\S+)", clean_res.combined)
    assert m_wid is not None, f"Missing WAIVER_ID in output:\n{clean_res.combined}"
    assert_in(m_wid.group(1), clean_res.combined, "Expected waiver ID in set_lp_waiver -list")

    v_open_post = _extract_violations(_read_json(ctx.path("post_open.json")))
    assert not v_open_post, f"Expected 0 open violations after debug_bus waiver: {v_open_post!r}"

    v_waived_post = _extract_violations(_read_json(ctx.path("post_waived.json")))
    assert _find_violations(v_waived_post, rule_prefix="ISO_MISSING", status="waived", needle="debug_bus"), (
        f"Expected waived debug_bus ISO_MISSING: {v_waived_post!r}"
    )

    v_after_rm = _extract_violations(_read_json(ctx.path("after_remove.json")))
    assert _find_violations(v_after_rm, rule_prefix="ISO_MISSING", status="open", needle="debug_bus"), (
        f"Expected debug_bus to return to open after set_lp_waiver -remove: {v_after_rm!r}"
    )


# ---------------------------------------------------------------------------
# T4-W2: Formal LPV PMU Sequencing & Partial-Retention Post-Wakeup X-Leakage
# ---------------------------------------------------------------------------


def _case_t4_w2(ctx: E2EContext) -> None:
    ctx.copy_fixture("workloads/t4_w2_lpv_pmu_xleak/lpv_pmu_soc.sv")
    ctx.copy_fixture("workloads/t4_w2_lpv_pmu_xleak/lpv_pmu_soc_bad_seq.sv")
    ctx.copy_fixture("workloads/t4_w2_lpv_pmu_xleak/lpv_pmu_soc_clean.upf")
    ctx.copy_fixture("workloads/t4_w2_lpv_pmu_xleak/lpv_pmu_soc_partial_ret.upf")

    # 1. Clean Formal LPV (AAG + 2-copy corruption proof)
    r_clean = ctx.run_shell_commands(
        [
            "analyze -sv12 lpv_pmu_soc.sv",
            "elaborate -top lpv_pmu_soc",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf lpv_pmu_soc_clean.upf",
            "check_lp -stage formal -prove_corruption",
            "report -task LowPower -csv -file lowpower_clean.csv -force",
            "set f1 [open stats_clean.json w]; puts $f1 [get_stats -task LowPower]; close $f1",
            "set f2 [open lp_clean.json w]; puts $f2 [report_lp -json]; close $f2",
        ],
        workdir=ctx.path("sw_clean"),
        timeout_s=120.0,
    )
    assert_exit(r_clean, 0, "T4-W2 clean Formal LPV failed")
    stats_clean = _read_json(ctx.path("stats_clean.json"))
    for pat, exp_status in (
        ("lp_iso_before_pwroff", "proven"),
        ("lp_iso_hold_after_pwron", "proven"),
        ("lp_ret_save_order", "proven"),
        ("lp_ret_restore_order", "proven"),
        ("lp_ret_save_restore_mutex", "proven"),
        ("lp_pst_no_illegal", "proven"),
        ("lp_pst_reach", "covered"),
    ):
        assert any(pat in str(r.get("name", "")) and r.get("status") == exp_status for r in stats_clean), (
            f"Expected {pat} -> {exp_status} in LowPower AAG stats: {stats_clean!r}"
        )
    lp_clean_v = _extract_violations(_read_json(ctx.path("lp_clean.json")))
    assert not _find_violations(lp_clean_v, status="formal_cex"), (
        f"Expected 0 corruption CEX in clean retention design: {lp_clean_v!r}"
    )

    # 2. Seeded PMU Sequencing Bug
    r_seq = ctx.run_shell_commands(
        [
            "analyze -sv12 lpv_pmu_soc_bad_seq.sv",
            "elaborate -top lpv_pmu_soc",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf lpv_pmu_soc_clean.upf",
            "check_lpv",
            "set f1 [open stats_seq.json w]; puts $f1 [get_stats -task LowPower]; close $f1",
        ],
        workdir=ctx.path("sw_bad_seq"),
        timeout_s=120.0,
    )
    assert_exit(r_seq, 0, "T4-W2 bad PMU sequence run failed")
    stats_seq = _read_json(ctx.path("stats_seq.json"))
    seq_cex = [r for r in stats_seq if "lp_iso_before_pwroff" in str(r.get("name", "")) and r.get("status") == "cex"]
    assert seq_cex, f"Expected lp_iso_before_pwroff CEX on bad PMU sequence: {stats_seq!r}"
    seq_prop = str(seq_cex[0]["name"])
    r_seq_why = ctx.run_shell_commands(
        [
            "analyze -sv12 lpv_pmu_soc_bad_seq.sv",
            "elaborate -top lpv_pmu_soc",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf lpv_pmu_soc_clean.upf",
            "check_lpv",
            f"set fw [open seq_why.json w]; puts $fw [why_cex -json {seq_prop}]; close $fw",
        ],
        workdir=ctx.path("sw_bad_seq"),
        timeout_s=120.0,
    )
    assert_exit(r_seq_why, 0, "T4-W2 why_cex on bad PMU sequence failed")
    seq_why = _read_json(ctx.path("seq_why.json"))
    assert_eq(seq_why.get("status"), "cex", "seq_why status")
    assert len(seq_why.get("events", [])) >= 1, f"Expected non-empty why_cex events: {seq_why!r}"
    assert any(
        k in json.dumps(seq_why).lower() for k in ("iso_en", "pwr_sw_en", "u_pmu")
    ), f"Expected PMU signals in why_cex slice: {seq_why!r}"

    # 3. Partial-Retention Post-Wakeup X-Leakage
    r_part = ctx.run_shell_commands(
        [
            "analyze -sv12 lpv_pmu_soc.sv",
            "elaborate -top lpv_pmu_soc",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf lpv_pmu_soc_partial_ret.upf",
            "check_lp -stage formal -prove_corruption",
            "set f1 [open partial_ret.json w]; puts $f1 [report_lp -json]; close $f1",
            "set f2 [open stats_partial.json w]; puts $f2 [get_stats -task LowPower]; close $f2",
        ],
        workdir=ctx.path("sw_partial"),
        timeout_s=120.0,
    )
    assert_exit(r_part, 0, "T4-W2 partial-retention run failed")
    stats_part = _read_json(ctx.path("stats_partial.json"))
    assert any("lp_iso_before_pwroff" in str(r.get("name", "")) and r.get("status") == "proven" for r in stats_part), (
        f"Expected PMU sequencing proven even with partial retention: {stats_part!r}"
    )
    v_part = _extract_violations(_read_json(ctx.path("partial_ret.json")))
    part_cex = _find_violations(v_part, status="formal_cex") or _find_violations(v_part, status="cex")
    assert part_cex, f"Expected partial-retention post-wakeup corruption CEX: {v_part!r}"

    trace_raw = _trace_of(part_cex[0])
    assert trace_raw, f"Expected trace path on partial-retention CEX: {part_cex[0]!r}"
    vcd_part = parse_vcd_file(_resolve_workdir_path(ctx, trace_raw))
    assert has_lp_domain_signals(vcd_part, "PD_CORE"), "Expected lp.pd.PD_CORE.* signals in partial-retention VCD"
    assert signals_diverge(vcd_part), "Expected m0/m1 divergence on partial-retention VCD"
    divs = find_divergences(vcd_part)
    dout_divs = [
        (t, a, b)
        for lbl, t, a, b in divs
        if lbl.rsplit(".", 1)[-1].lower() in {"dout", "lp_corrupt_dout"}
    ]
    assert dout_divs, f"Expected dout m0/m1 divergence post-wakeup, got: {divs!r}"

    # Verify isolation kept dout identical while iso_en == 1
    iso_sigs = vcd_part.find_signals("*iso_en*")
    if iso_sigs:
        first_div_t = min(t for t, _, _ in dout_divs)
        assert first_div_t > 0, "Expected post-wakeup divergence after reset/sleep"
        for t, _va, _vb in dout_divs:
            assert vcd_part.value_at(iso_sigs[0], t) != "1", (
                f"Unexpected dout divergence at t={t} while iso_en==1"
            )

    cex_props = [r["name"] for r in stats_part if isinstance(r, dict) and r.get("status") == "cex"]
    assert cex_props, f"Expected CEX property in stats_partial: {stats_part!r}"
    r_xwhy = ctx.run_shell_commands(
        [
            "analyze -sv12 lpv_pmu_soc.sv",
            "elaborate -top lpv_pmu_soc",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf lpv_pmu_soc_partial_ret.upf",
            "check_lp -stage formal -prove_corruption",
            f"set fw [open xleak_why.json w]; puts $fw [why_cex -json {cex_props[0]}]; close $fw",
        ],
        workdir=ctx.path("sw_partial"),
        timeout_s=120.0,
    )
    assert_exit(r_xwhy, 0, "T4-W2 why_cex on partial retention failed")
    xwhy = _read_json(ctx.path("xleak_why.json"))
    assert len(xwhy.get("events", [])) >= 1, f"Expected causal events in xleak_why: {xwhy!r}"
    assert any(k in json.dumps(xwhy).lower() for k in ("scratch_q", "use_scratch", "dout")), (
        f"Expected scratch_q/use_scratch/dout in why_cex slice: {xwhy!r}"
    )


# ---------------------------------------------------------------------------
# T4-W3: Closed-Loop Certified Auto-Waiver Lifecycle & Invalidation
# ---------------------------------------------------------------------------


def _case_t4_w3(ctx: E2EContext) -> None:
    ctx.copy_fixture("workloads/t4_w3_autowaiver/autowaiver_soc.sv")
    ctx.copy_fixture("workloads/t4_w3_autowaiver/autowaiver_soc.upf")

    script = ctx.write_file(
        "run_t4_w3.tcl",
        """
proc dump_rep {path} {
    set fh [open $path w]
    puts $fh [report_lp -json]
    close $fh
}
analyze -sv12 autowaiver_soc.sv
elaborate -top autowaiver_soc
clock clk
reset -expression {!rst_n}
load_upf autowaiver_soc.upf

# Phase 1: static open ISO_MISSING
check_lp -stage rtl
dump_rep phase1.json

# Phase 2: mode assumption makes unisolated crossing unobservable -> FormallyWaived
assume -name m_active_only_when_on {active_use |-> !sleep_en}
check_lp -prove_waivers
dump_rep phase2.json

# Phase 3: removing assumption must immediately invalidate FormallyWaived -> open
assume -remove m_active_only_when_on
dump_rep phase3.json

# Phase 4: re-proving without assumption yields FormalCex with VCD trace
check_lp -prove_waivers
dump_rep phase4.json
""",
    )
    res = ctx.run_shell_file(script, timeout_s=120.0)
    assert_exit(res, 0, "T4-W3 auto-waiver lifecycle failed")

    v1 = _extract_violations(_read_json(ctx.path("phase1.json")))
    assert _find_violations(v1, rule_prefix="ISO_MISSING", status="open", needle="src_bus"), (
        f"Phase 1 expected open ISO_MISSING on src_bus: {v1!r}"
    )

    v2 = _extract_violations(_read_json(ctx.path("phase2.json")))
    fw = _find_violations(v2, rule_prefix="ISO_MISSING", status="formally_waived", needle="src_bus")
    assert fw, f"Phase 2 expected formally_waived on src_bus: {v2!r}"
    assert not _find_violations(v2, status="open"), f"Phase 2 expected 0 open violations: {v2!r}"
    meta = fw[0].get("auto_formal_meta")
    assert isinstance(meta, dict) and meta.get("engine"), (
        f"Expected non-empty auto_formal_meta with engine on formally_waived: {fw[0]!r}"
    )

    v3 = _extract_violations(_read_json(ctx.path("phase3.json")))
    assert not _find_violations(v3, status="formally_waived"), (
        f"Phase 3 expected assumption removal to invalidate formally_waived: {v3!r}"
    )
    assert _find_violations(v3, rule_prefix="ISO_MISSING", status="open", needle="src_bus"), (
        f"Phase 3 expected src_bus back to open after invalidation: {v3!r}"
    )

    v4 = _extract_violations(_read_json(ctx.path("phase4.json")))
    cex4 = _find_violations(v4, status="formal_cex", needle="src_bus") or _find_violations(
        v4, status="cex", needle="src_bus"
    )
    assert cex4, f"Phase 4 expected formal_cex on src_bus without assumption: {v4!r}"
    trace4 = _trace_of(cex4[0])
    assert trace4, f"Expected trace path on Phase 4 CEX: {cex4[0]!r}"
    vcd4 = parse_vcd_file(_resolve_workdir_path(ctx, trace4))
    assert signals_diverge(vcd4), "Phase 4 CEX VCD must show m0/m1 divergence on dout"


# ---------------------------------------------------------------------------
# T4-W4: Golden Non-PG RTL vs Power-Aware RTL+UPF LP-SEC Equivalence
# ---------------------------------------------------------------------------


def _case_t4_w4(ctx: E2EContext) -> None:
    ctx.copy_fixture("workloads/t4_w4_lp_sec/golden_npg.sv")
    ctx.copy_fixture("workloads/t4_w4_lp_sec/pa_rtl.sv")
    ctx.copy_fixture("workloads/t4_w4_lp_sec/pa_rtl_clean.upf")
    ctx.copy_fixture("workloads/t4_w4_lp_sec/pa_rtl_buggy_ret.upf")

    script = ctx.write_file(
        "run_t4_w4.tcl",
        """
analyze -sv12 pa_rtl.sv
elaborate -top pa_rtl
clock clk
reset -expression {!rst_n}
load_upf pa_rtl_clean.upf
check_sec -setup -spec golden_npg.sv -spec_top golden_npg -imp pa_rtl.sv -imp_top pa_rtl -clock clk -reset {!rst_n}
check_lp -sec
set f1 [open sec_clean.json w]
puts $f1 [check_sec -report -json]
close $f1

load_upf pa_rtl_buggy_ret.upf
check_sec -setup -spec golden_npg.sv -spec_top golden_npg -imp pa_rtl.sv -imp_top pa_rtl -clock clk -reset {!rst_n}
check_lp -sec
set f2 [open sec_buggy.json w]
puts $f2 [check_sec -report -json]
close $f2
""",
    )
    res = ctx.run_shell_file(script, timeout_s=120.0)
    assert_exit(res, 0, "T4-W4 LP-SEC run failed")

    sec_clean = _read_json(ctx.path("sec_clean.json"))
    clean_checks = sec_clean.get("checks", [])
    assert clean_checks, f"Expected non-empty LP-SEC checks in sec_clean.json: {sec_clean!r}"
    for target in ("dout", "valid_out"):
        matched = [c for c in clean_checks if target in str(c.get("name", "")) or target in str(c.get("imp_sig", ""))]
        assert matched and all(c.get("status") == "proven" for c in matched), (
            f"Expected {target} proven in clean LP-SEC: {clean_checks!r}"
        )

    sec_buggy = _read_json(ctx.path("sec_buggy.json"))
    buggy_checks = sec_buggy.get("checks", [])
    dout_cex = [
        c
        for c in buggy_checks
        if ("dout" in str(c.get("name", "")) or "dout" in str(c.get("imp_sig", "")))
        and (c.get("status") == "cex" or isinstance(c.get("status"), dict) and "cex" in c.get("status", {}))
    ]
    assert dout_cex, f"Expected dout LP-SEC CEX when u_core/bias_q omitted from retention: {buggy_checks!r}"
    trace_raw = dout_cex[0].get("trace")
    assert trace_raw, f"Expected trace VCD on LP-SEC CEX: {dout_cex[0]!r}"
    vcd = parse_vcd_file(_resolve_workdir_path(ctx, str(trace_raw)))
    divs = find_divergences(vcd, prefix_a="spec.", prefix_b="imp.")
    assert any("dout" in lbl.lower() for lbl, _t, _a, _b in divs), (
        f"Expected spec.dout != imp.dout divergence in LP-SEC VCD, got {divs!r}"
    )


# ---------------------------------------------------------------------------
# T4-W5: 100k-Cell Gate & PG Netlist Stage 3/4 Signoff & LP-LEC
# ---------------------------------------------------------------------------


def _case_t4_w5(ctx: E2EContext) -> None:
    ctx.copy_fixture("workloads/t4_w5_gate_pg_lec/rtl_golden.sv")
    ctx.write_file("mini.lib", generate_mini_liberty())
    ctx.write_file("stdcells.sv", generate_stdcell_verilog())
    ctx.write_file("gate_pg.upf", generate_gate_pg_upf())
    ctx.write_file("gate_100k_clean.v", generate_gate_pg_netlist(num_cells=100000, include_bugs=False))
    ctx.write_file("gate_100k_buggy.v", generate_gate_pg_netlist(num_cells=100000, include_bugs=True))

    clean_100 = generate_gate_pg_netlist(num_cells=100, include_bugs=False)
    ctx.write_file("gate_100_clean.v", clean_100)
    bad_iso_100 = clean_100.replace(
        "ISO_LO_X1 u_iso (.A(core_sig), .ISO(aon_iso), .Y(iso_sig), .VDD_BK(VDD_BK), .VSS(VSS));",
        "INV_X1    u_iso (.A(core_sig), .Y(iso_sig), .VDD(VDD_AON), .VSS(VSS));",
    )
    assert bad_iso_100 != clean_100, "Failed to mutate ISO_LO_X1 in gate_100_bad_iso.v"
    ctx.write_file("gate_100_bad_iso.v", bad_iso_100)

    # 1. 100k Clean Stage 3/4 + Perf (< 10000 ms) and 100k Buggy Stage 3/4
    script_pg = ctx.write_file(
        "run_w5_pg.tcl",
        """
set t0 [clock milliseconds]
read_liberty mini.lib
analyze -sv gate_100k_clean.v
elaborate -top soc_pg_top
load_upf gate_pg.upf
check_lp -stage netlist
set fh1_s3 [open pg_clean_s3.json w]
puts $fh1_s3 [report_lp -json]
close $fh1_s3
check_lp -stage pg
set dt [expr {[clock milliseconds] - $t0}]
puts "W5_100K_MS=$dt"
set fh1_s4 [open pg_clean_s4.json w]
puts $fh1_s4 [report_lp -json]
close $fh1_s4

analyze -clear
analyze -sv gate_100k_buggy.v
elaborate -top soc_pg_top
load_upf gate_pg.upf
check_lp -stage netlist
set fh2_s3 [open pg_buggy_s3.json w]
puts $fh2_s3 [report_lp -json]
close $fh2_s3
check_lp -stage pg
set fh2_s4 [open pg_buggy_s4.json w]
puts $fh2_s4 [report_lp -json]
close $fh2_s4
""",
    )
    res_pg = ctx.run_shell_file(script_pg, workdir=ctx.path("sw_pg"), timeout_s=120.0)
    assert_exit(res_pg, 0, "T4-W5 100k-cell Stage 3/4 run failed")

    m_ms = re.search(r"W5_100K_MS=(\d+)", res_pg.combined)
    assert m_ms is not None, f"Missing W5_100K_MS timing marker in output:\n{res_pg.combined}"
    assert int(m_ms.group(1)) < 10000, f"100k-cell gate/PG Stage 3/4 exceeded budget: {m_ms.group(1)} ms"

    v_clean = _extract_violations(_read_json(ctx.path("pg_clean_s3.json"))) + _extract_violations(
        _read_json(ctx.path("pg_clean_s4.json"))
    )
    assert not _find_violations(v_clean, status="open"), f"Expected 0 open violations on 100k clean PG netlist: {v_clean!r}"

    v_bug = _extract_violations(_read_json(ctx.path("pg_buggy_s3.json"))) + _extract_violations(
        _read_json(ctx.path("pg_buggy_s4.json"))
    )
    rules_bug = {_rule_of(v).upper() for v in v_bug}
    for req in (
        "ISO_CELL_MISSING",
        "ISO_LOGIC_BEFORE_CELL",
        "AON_NON_AON_BUFFER",
        "PG_BACKUP_MISMATCH",
        "PG_SUPPLY_MISMATCH",
        "PG_PIN_UNCONNECTED",
    ):
        assert any(r.startswith(req) for r in rules_bug), (
            f"Expected {req} in 100k buggy Stage 3/4 report, got rules: {sorted(rules_bug)}"
        )

    # 2. LP-LEC: RTL+UPF vs Netlist+UPF (clean proven, mutated ISO fails lec_out_dout CEX)
    script_lec = ctx.write_file(
        "run_w5_lec.tcl",
        """
read_liberty mini.lib
analyze -sv12 rtl_golden.sv
elaborate -top soc_pg_top
clock clk
reset -expression {!rst_n}
load_upf gate_pg.upf
check_sec -setup -lec -spec rtl_golden.sv -spec_top soc_pg_top -imp {stdcells.sv gate_100_clean.v} -imp_top soc_pg_top -clock clk -reset {!rst_n}
check_lp -lec
set f1 [open lec_clean.json w]
puts $f1 [check_sec -report -json]
close $f1

check_sec -setup -lec -spec rtl_golden.sv -spec_top soc_pg_top -imp {stdcells.sv gate_100_bad_iso.v} -imp_top soc_pg_top -clock clk -reset {!rst_n}
check_lp -lec
set f2 [open lec_bad.json w]
puts $f2 [check_sec -report -json]
close $f2
""",
    )
    res_lec = ctx.run_shell_file(script_lec, workdir=ctx.path("sw_lec"), timeout_s=120.0)
    assert_exit(res_lec, 0, "T4-W5 LP-LEC run failed")

    lec_clean = _read_json(ctx.path("lec_clean.json")).get("checks", [])
    assert lec_clean and all(c.get("status") == "proven" for c in lec_clean), (
        f"Expected all clean LP-LEC checks proven: {lec_clean!r}"
    )

    lec_bad = _read_json(ctx.path("lec_bad.json")).get("checks", [])
    bad_dout = [
        c
        for c in lec_bad
        if "dout" in str(c.get("name", ""))
        and (c.get("status") == "cex" or isinstance(c.get("status"), dict) and "cex" in c.get("status", {}))
    ]
    assert bad_dout, f"Expected lec_out_dout CEX on mutated ISO netlist: {lec_bad!r}"


# ---------------------------------------------------------------------------
# T4-W6: UPF 1.0/2.1/3.1 + CPF Migration & 32-Domain Symbolic PST SAT
# ---------------------------------------------------------------------------


def _case_t4_w6(ctx: E2EContext) -> None:
    ctx.copy_fixture("workloads/t4_w6_migration_pst/mig_soc.sv")
    ctx.copy_fixture("workloads/t4_w6_migration_pst/mig_v10.upf")
    ctx.copy_fixture("workloads/t4_w6_migration_pst/mig_v21.upf")
    ctx.copy_fixture("workloads/t4_w6_migration_pst/mig_v31.upf")
    ctx.copy_fixture("workloads/t4_w6_migration_pst/mig_cadence.cpf")
    ctx.copy_fixture("workloads/t4_w6_migration_pst/mig_v21_flipped_clamp.upf")

    clean_32 = generate_n_domain_upf(32, include_contradiction=False, upf_version="2.1")
    bad_32 = generate_n_domain_upf(32, include_contradiction=True, upf_version="2.1")
    ctx.write_file("soc_32.sv", clean_32.sv)
    ctx.write_file("soc_32.upf", clean_32.upf)
    ctx.write_file("soc_32_bad.upf", bad_32.upf)

    # 1. Cross-version & cross-format UPF/CPF equivalence + 32-domain PST SAT
    script = ctx.write_file(
        "run_t4_w6.tcl",
        """
puts "===CMP_10_21_BEGIN==="
puts [compare_upf -golden mig_v10.upf -revised mig_v21.upf]
puts "===CMP_10_21_END==="
puts "===CMP_21_31_BEGIN==="
puts [compare_upf -golden mig_v21.upf -revised mig_v31.upf]
puts "===CMP_21_31_END==="
puts "===CMP_21_CPF_BEGIN==="
puts [compare_upf -golden mig_v21.upf -revised mig_cadence.cpf]
puts "===CMP_21_CPF_END==="

analyze -sv12 mig_soc.sv
elaborate -top mig_soc
clock clk
reset -expression {!rst_n}
load_upf mig_cadence.cpf -cpf
check_lp -stage design
set f_cpf [open cpf_design.json w]
puts $f_cpf [report_lp -json]
close $f_cpf

analyze -clear
analyze -sv12 soc_32.sv
elaborate -top soc_top
clock clk
reset -expression {!rst_n}
set t0 [clock milliseconds]
load_upf soc_32.upf
check_lp -stage upf
set dt_clean [expr {[clock milliseconds] - $t0}]
puts "PST32_CLEAN_MS=$dt_clean"
set f_32c [open pst32_clean.json w]
puts $f_32c [report_lp -json]
close $f_32c

set t1 [clock milliseconds]
load_upf soc_32_bad.upf
check_lp -stage upf
set dt_bad [expr {[clock milliseconds] - $t1}]
puts "PST32_BAD_MS=$dt_bad"
set f_32b [open pst32_bad.json w]
puts $f_32b [report_lp -json]
close $f_32b
""",
    )
    res = ctx.run_shell_file(script, timeout_s=90.0)
    assert_exit(res, 0, "T4-W6 migration & 32-domain PST run failed")

    for tag in ("CMP_10_21", "CMP_21_31", "CMP_21_CPF"):
        cmp_out = _extract_block(res.stdout, tag)
        assert cmp_out and not _is_compare_mismatch(cmp_out, 0), (
            f"Expected positive compare_upf equivalence for {tag}, got: {cmp_out!r}"
        )

    v_cpf = _extract_violations(_read_json(ctx.path("cpf_design.json")))
    assert not _find_violations(v_cpf, status="open"), f"Expected 0 open errors for mig_cadence.cpf: {v_cpf!r}"

    v_32c = _extract_violations(_read_json(ctx.path("pst32_clean.json")))
    assert not _find_violations(v_32c, rule_prefix="PST_"), (
        f"Expected 0 PST_* violations on 32-domain clean UPF: {v_32c!r}"
    )
    v_32b = _extract_violations(_read_json(ctx.path("pst32_bad.json")))
    assert _find_violations(v_32b, rule_prefix="PST_"), (
        f"Expected PST_INCONSISTENT / PST_UNREACHABLE_STATE on 32-domain contradictory UPF: {v_32b!r}"
    )

    m_c = re.search(r"PST32_CLEAN_MS=(\d+)", res.combined)
    m_b = re.search(r"PST32_BAD_MS=(\d+)", res.combined)
    assert m_c and m_b, f"Missing 32-domain PST timing lines:\n{res.combined}"
    assert int(m_c.group(1)) < 5000 and int(m_b.group(1)) < 5000, (
        f"32-domain symbolic PST SAT exceeded latency bound: clean={m_c.group(1)}ms, bad={m_b.group(1)}ms"
    )

    # 2. Negative compare_upf: flipped clamp must be detected
    r_flip = ctx.run_shell_commands(
        ["compare_upf -golden mig_v21.upf -revised mig_v21_flipped_clamp.upf"],
    )
    assert _is_compare_mismatch(r_flip.combined, r_flip.returncode, extra_kws=("clamp",)), (
        f"Expected compare_upf mismatch on flipped clamp UPF, got rc={r_flip.returncode}: {r_flip.combined}"
    )


# ---------------------------------------------------------------------------
# T4-W7: End-to-End AI Agent MCP + fvp.spec.json + docgen + UI Hooks
# ---------------------------------------------------------------------------


def _case_t4_w7(ctx: E2EContext) -> None:
    ctx.copy_fixture("workloads/t4_w7_mcp_spec_ui/lp_subsystem.sv")
    ctx.copy_fixture("workloads/t4_w7_mcp_spec_ui/lp_subsystem_buggy.upf")
    ctx.copy_fixture("workloads/t4_w7_mcp_spec_ui/lp_subsystem_clean.upf")
    ctx.copy_fixture("workloads/t4_w7_mcp_spec_ui/fvp.spec.json")

    with ctx.mcp(mode="auto") as mcp:
        tools = mcp.tool_names()
        for tname in ("load_upf", "check_lp", "lp_diagnose", "spec_audit", "open_workbench", "schematic_get"):
            assert_in(tname, tools, f"Missing required MCP tool {tname}")

        prompts = {str(p.get("name", "")) for p in mcp.prompts_list()}
        assert_in("audit_low_power", prompts, "Missing audit_low_power MCP prompt")
        p_res = mcp.prompts_get("audit_low_power")
        assert len(p_res.get("messages", [])) >= 1, f"Empty audit_low_power prompt: {p_res!r}"
        p_text = json.dumps(p_res)
        assert "load_upf" in p_text and "check_lp" in p_text, f"audit_low_power prompt missing LP workflow: {p_res!r}"

        mcp.call_tool(
            "session_open",
            {
                "files": ["lp_subsystem.sv"],
                "top": "lp_subsystem",
                "clock": "clk",
                "reset": "!rst_n",
            },
        )
        u_bug = mcp.call_tool("load_upf", {"file": "lp_subsystem_buggy.upf"})
        assert_json_schema_field(u_bug, "informal.upf/1")
        u_text = json.dumps(u_bug)
        assert "PD_TOP" in u_text and "PD_ENG" in u_text, f"Expected PD_TOP & PD_ENG in load_upf response: {u_bug!r}"

        lp_bug = mcp.call_tool("check_lp", {"stage": "all", "prove_corruption": True})
        assert_json_schema_field(lp_bug, "informal.lp/1")
        iso_bugs = _find_violations(_extract_violations(lp_bug), rule_prefix="ISO_MISSING")
        assert iso_bugs, f"Expected ISO_MISSING in buggy UPF check_lp: {lp_bug!r}"

        diag_args: dict[str, Any] = {}
        if "id" in iso_bugs[0]:
            diag_args["id"] = iso_bugs[0]["id"]
        elif "name" in iso_bugs[0]:
            diag_args["violation"] = iso_bugs[0]["name"]
        else:
            diag_args["rule"] = _rule_of(iso_bugs[0])
        diag = mcp.call_tool("lp_diagnose", diag_args)
        assert_json_schema_field(diag, "informal.lp_diagnose/1")
        assert "pst_witness" in diag, f"Expected pst_witness in lp_diagnose: {diag!r}"
        rtl_info = (
            diag.get("rtl_crossing_path")
            or diag.get("rtl_path")
            or diag.get("driver_snippets")
            or diag.get("rtl_loc")
        )
        assert rtl_info, f"Expected RTL crossing path/snippets/loc in lp_diagnose: {diag!r}"

        audit_fail = mcp.call_tool("spec_audit", {"spec_file": "fvp.spec.json"})
        assert_json_schema_field(audit_fail, "informal.spec_audit/1")
        assert_eq(audit_fail.get("signoff_ready"), False, "Expected signoff_ready=False before fixing LP + proving")

        # Fix UPF, prove formal LPV & embedded requirements, check schematic domain tags & open_workbench
        mcp.call_tool("load_upf", {"file": "lp_subsystem_clean.upf"})
        mcp.call_tool("check_lp", {"stage": "all", "prove_corruption": True})
        mcp.call_tool("prove", {})

        cone = mcp.call_tool("schematic_get", {"name": "lp_subsystem.a_iso_before_off"})
        nodes = cone.get("nodes", [])
        assert nodes and any("power_domain" in n for n in nodes if isinstance(n, dict)), (
            f"Expected power_domain metadata on schematic_get nodes when UPF is loaded: {cone!r}"
        )

        wb = mcp.call_tool("open_workbench", {"focus_property": "lp_subsystem.a_iso_before_off", "view": "apps"})
        assert_json_schema_field(wb, "informal.workbench/1")

        audit_pass = mcp.call_tool("spec_audit", {"spec_file": "fvp.spec.json"})
        assert_json_schema_field(audit_pass, "informal.spec_audit/1")
        assert_eq(audit_pass.get("signoff_ready"), True, f"Expected signoff_ready=True after LP signoff: {audit_pass!r}")

    # Docgen consistency gate (including gui_set_view "lp" tab doc verification)
    doc_res = ctx.run_docgen_check()
    assert_exit(doc_res, 0, "informal docgen --check failed")
    ref_idx = ctx.repo_root / "docs" / "reference" / "index.json"
    if ref_idx.is_file():
        idx_text = ref_idx.read_text(encoding="utf-8")
        for sym in ("load_upf", "check_lp", "lp_diagnose", "audit_low_power"):
            assert_in(sym, idx_text, f"{sym} missing from docs/reference/index.json")
    mcp_doc = ctx.repo_root / "docs" / "reference" / "mcp-tools.md"
    if mcp_doc.is_file():
        m_gui = re.search(r"### `gui_set_view`(.*?)(?:\n### |\Z)", mcp_doc.read_text(encoding="utf-8"), re.DOTALL)
        assert m_gui is not None and "`lp`" in m_gui.group(1), (
            "Expected gui_set_view documentation to include `lp` app_tab"
        )

    # UI TypeScript + Vitest (+ conditional Playwright when cached browsers exist)
    ui_dir = ctx.repo_root / "ui"
    if (ui_dir / "node_modules").exists():
        tsc_res = run_command(
            ["npx", "--no-install", "tsc", "--noEmit"],
            cwd=ui_dir,
            env=_ui_env(ctx),
            timeout_s=60.0,
        )
        assert_exit(tsc_res, 0, "UI tsc --noEmit failed")

        vitest_res = run_command(
            ["npx", "--no-install", "vitest", "run"],
            cwd=ui_dir,
            env=_ui_env(ctx),
            timeout_s=90.0,
        )
        assert_exit(vitest_res, 0, "UI vitest run failed")

        pw_cache = Path.home() / "Library" / "Caches" / "ms-playwright"
        if pw_cache.is_dir() and any(pw_cache.iterdir()):
            pw_res = run_command(
                ["npx", "--no-install", "playwright", "test", "tests/e2e/v2-upgrades-ux.spec.ts"],
                cwd=ui_dir,
                env=_ui_env(ctx),
                timeout_s=120.0,
            )
            assert_exit(pw_res, 0, "UI Playwright v2-upgrades-ux.spec.ts failed")


def get_cases() -> list[TestCase]:
    """Return all 7 Tier 4 real-world SoC workload test cases."""
    return [
        TestCase(
            id="T4-W1",
            tier=4,
            feature=("F4", "F5", "F6"),
            title="Multi-domain mobile SoC static LP signoff with bus grouping, feedthrough, and waivers",
            fn=_case_t4_w1,
            timeout_s=90.0,
            tags=("workload", "lp_static", "static", "waiver", "feedthrough"),
        ),
        TestCase(
            id="T4-W2",
            tier=4,
            feature=("F3", "F7", "F8"),
            title="Formal LPV PMU sequencing & partial-retention post-wakeup X-leakage bug hunt",
            fn=_case_t4_w2,
            timeout_s=120.0,
            tags=("workload", "lpv", "aag", "corruption", "why_cex", "vcd"),
        ),
        TestCase(
            id="T4-W3",
            tier=4,
            feature=("F6", "F8"),
            title="Closed-loop certified auto-waiver lifecycle & assumption-invalidation regression",
            fn=_case_t4_w3,
            timeout_s=120.0,
            tags=("workload", "autowaiver", "formal", "invalidation", "vcd"),
        ),
        TestCase(
            id="T4-W4",
            tier=4,
            feature=("F2", "F7", "F8"),
            title="Non-power-gated golden RTL vs power-aware RTL+UPF LP-SEC sleep/wake equivalence",
            fn=_case_t4_w4,
            timeout_s=120.0,
            tags=("workload", "lp_sec", "cache", "retention", "vcd"),
        ),
        TestCase(
            id="T4-W5",
            tier=4,
            feature=("F5", "F9"),
            title="100k-cell post-synthesis gate & PG netlist Stage 3/4 signoff and LP-LEC",
            fn=_case_t4_w5,
            timeout_s=120.0,
            tags=("workload", "liberty", "gate_fast", "100k", "pg", "lp_lec"),
        ),
        TestCase(
            id="T4-W6",
            tier=4,
            feature=("F4", "F5"),
            title="UPF 1.0/2.1/3.1 + Cadence CPF migration & 32-domain symbolic PST verification",
            fn=_case_t4_w6,
            timeout_s=90.0,
            tags=("workload", "upf10", "upf21", "upf31", "cpf", "compare_upf", "pst_sat", "32domain"),
        ),
        TestCase(
            id="T4-W7",
            tier=4,
            feature=("F1", "F2", "F10"),
            title="End-to-end AI agent MCP + fvp.spec.json LP signoff + docgen + UI hooks",
            fn=_case_t4_w7,
            timeout_s=120.0,
            tags=("workload", "mcp", "spec_audit", "docgen", "ui", "vitest"),
        ),
    ]
