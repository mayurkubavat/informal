"""Tier 1 opaque-box E2E test cases (happy paths) for Feature Areas F1-F6."""

from __future__ import annotations

import json
from pathlib import Path
import re
import time
from typing import Any

from lib.fixtures_gen import generate_n_domain_upf
from lib.harness import (
    E2EContext,
    TestCase,
    assert_contains,
    assert_eq,
    assert_exit,
    assert_has_keys,
    assert_in,
    assert_json_schema_field,
    assert_not_in,
)
from lib.vcd_check import find_divergences, parse_vcd_file


def _copy_f1_f6(ctx: E2EContext, *names: str) -> dict[str, Path]:
    return {name: ctx.copy_fixture(f"f1_f6/{name}", name) for name in names}


def _extract_lp_violations(report_obj: Any) -> list[dict[str, Any]]:
    if isinstance(report_obj, list):
        return [v for v in report_obj if isinstance(v, dict)]
    if isinstance(report_obj, dict):
        for key in ("violations", "items", "findings", "results"):
            val = report_obj.get(key)
            if isinstance(val, list):
                return [v for v in val if isinstance(v, dict)]
    raise AssertionError(f"Expected LP violations list in report JSON, got: {report_obj!r}")


def _parse_last_json_object(stdout: str) -> Any:
    decoder = json.JSONDecoder()
    last_obj: Any = None
    found = False
    pos = 0
    n = len(stdout)
    while pos < n:
        next_obj = stdout.find("{", pos)
        next_arr = stdout.find("[", pos)
        if next_obj < 0 and next_arr < 0:
            break
        if next_obj < 0:
            cand = next_arr
        elif next_arr < 0:
            cand = next_obj
        else:
            cand = min(next_obj, next_arr)
        if cand > 0 and (stdout[cand - 1].isalnum() or stdout[cand - 1] == "_"):
            pos = cand + 1
            continue
        try:
            obj, end = decoder.raw_decode(stdout, cand)
        except json.JSONDecodeError:
            pos = cand + 1
            continue
        if end < n and stdout[end] not in " \t\r\n":
            pos = cand + 1
            continue
        if isinstance(obj, dict) or (
            isinstance(obj, list) and all(isinstance(x, dict) for x in obj)
        ):
            last_obj = obj
            found = True
        pos = end
    if found:
        return last_obj
    raise AssertionError(f"Could not locate JSON payload in stdout:\n{stdout}")


def _find_xprop_item(items: list[dict[str, Any]], sig: str) -> dict[str, Any]:
    for it in items:
        name = str(it.get("name", ""))
        if name == sig or name.endswith(f".{sig}"):
            return it
    raise AssertionError(
        f"Missing xprop item {sig!r} in {[it.get('name') for it in items]!r}"
    )


def _assert_lp_clean(report_obj: Any, *, allow_rules: set[str] = frozenset()) -> None:
    viols = _extract_lp_violations(report_obj)
    open_errors = [
        v
        for v in viols
        if str(v.get("status", "open")).lower() == "open"
        and str(v.get("severity", "error")).lower() in {"error", "fatal", "err"}
        and str(v.get("rule_id", "")) not in allow_rules
    ]
    if open_errors:
        raise AssertionError(f"Expected 0 open LP errors, got: {open_errors!r}")


def _copy_soc_lp_clean(ctx: E2EContext) -> tuple[Path, Path]:
    paths = _copy_f1_f6(ctx, "soc_lp_clean.sv", "soc_lp_clean.upf")
    return paths["soc_lp_clean.sv"], paths["soc_lp_clean.upf"]


# ---------------------------------------------------------------------------
# Feature F1: Baseline Gate & fvp.spec.json Spec Audit (low_power gates)
# ---------------------------------------------------------------------------


def _t1_f1_01_baseline_repo_gates(ctx: E2EContext) -> None:
    hygiene = ctx.run_repo_script("scripts/hygiene.sh")
    assert_exit(hygiene, 0, "scripts/hygiene.sh")

    docgen = ctx.run_docgen_check()
    assert_exit(docgen, 0, "informal docgen --check")
    assert_in("up to date", docgen.combined)

    docs = ctx.run_repo_script(
        "scripts/check_docs.py", ["--bin", str(ctx.informal_bin)], timeout_s=180.0
    )
    assert_exit(docs, 0, "scripts/check_docs.py")
    assert_in("check_docs: OK", docs.combined)

    examples = ctx.run_repo_script(
        "scripts/test_examples.py",
        ["--bin", str(ctx.informal_bin), "--check-only"],
        timeout_s=180.0,
    )
    assert_exit(examples, 0, "scripts/test_examples.py --check-only")
    assert_in("test_examples: OK", examples.combined)


def _t1_f1_02_spec_audit_fifo_autodiscovery(ctx: E2EContext) -> None:
    with ctx.mcp(mode="auto", root=ctx.repo_root) as mcp:
        opened = mcp.call_tool("session_open", {"script": "tests/fpv/fifo/fifo.jg.tcl"})
        assert_json_schema_field(opened, "informal.open/1")
        assert_eq(opened.get("ok"), True, "session_open ok")

        audit = mcp.call_tool("spec_audit", {})
        assert_json_schema_field(audit, "informal.spec_audit/1")
        assert_eq(audit.get("signoff_ready"), True, "fifo signoff_ready")
        assert_in("tests/fpv/fifo/fvp.spec.json", str(audit.get("spec_source", "")))
        summary = audit["summary"]
        assert_eq(summary["verified"], 5, "fifo verified count")
        assert_in("a_wrong", summary["unmapped_assertions"])
        gates = audit["gates"]
        assert gates, "expected non-empty signoff gates"
        for g in gates:
            assert_eq(g.get("ok"), True, f"gate {g.get('gate')} ok")


def _t1_f1_03_spec_audit_dead_end_check(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f1_regfile.sv", "f1_regfile.jg.tcl")
    ctx.copy_fixture("f1_f6/f1_regfile.spec.json", "fvp.spec.json")
    with ctx.mcp(mode="auto") as mcp:
        opened = mcp.call_tool("session_open", {"script": "f1_regfile.jg.tcl"})
        assert_eq(opened.get("ok"), True, "f1_regfile session_open")

        audit = mcp.call_tool("spec_audit", {"check_dead_end": True, "depth": 8})
        assert_json_schema_field(audit, "informal.spec_audit/1")
        assert_eq(audit.get("signoff_ready"), True, "f1_regfile signoff_ready")
        ba = audit["boundary_audit"]
        assert_eq(ba.get("dead_end_checked"), True, "dead_end_checked")
        assert_eq(ba.get("dead_end_found"), False, "dead_end_found")
        zero_de = next(
            (g for g in audit["gates"] if g.get("gate") == "zero_dead_ends"), None
        )
        assert zero_de is not None and zero_de.get("ok") is True, (
            f"Expected zero_dead_ends gate ok=True, got {audit['gates']!r}"
        )


def _t1_f1_04_spec_audit_bounded_proven_min_bound(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f1_regfile.sv")
    ctx.write_file(
        "bounded.jg.tcl",
        "\n".join(
            [
                "clear -all",
                "analyze -sv12 f1_regfile.sv",
                "elaborate -top f1_regfile",
                "clock clk",
                "reset -expression {!rst_n}",
                "assert -name a_neq_max -bound 4 {cnt < 3'd5}",
                "cover  -name c_full {full}",
                "prove -all",
            ]
        )
        + "\n",
    )
    spec = {
        "schema": "informal.spec/1",
        "name": "Bounded Regfile Spec",
        "design": {"top": "f1_regfile", "clocks": ["clk"], "reset": "!rst_n"},
        "boundary": {"allowed_assume_inputs": ["wr_en", "clr", "din"]},
        "requirements": [
            {
                "id": "REQ-B4",
                "title": "Bounded counter invariant up to 4 cycles",
                "assertions": ["a_neq_max"],
                "covers": ["c_full"],
            }
        ],
        "signoff": {
            "proof_target": "bounded_proven",
            "min_bound": 4,
            "forbid_vacuous": True,
        },
    }
    with ctx.mcp(mode="auto") as mcp:
        opened = mcp.call_tool("session_open", {"script": "bounded.jg.tcl"})
        assert_eq(opened.get("ok"), True, "bounded session_open")
        audit = mcp.call_tool("spec_audit", {"spec": spec})
        assert_json_schema_field(audit, "informal.spec_audit/1")
        assert_eq(audit.get("signoff_ready"), True, "bounded_proven signoff_ready")
        assert_eq(audit["requirements"][0]["status"], "verified", "REQ-B4 status")


def _t1_f1_05_spec_audit_allow_internal_assumes(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f1_regfile.sv", "f1_regfile.jg.tcl")
    spec = {
        "schema": "informal.spec/1",
        "name": "Waived Internal Assume Spec",
        "design": {"top": "f1_regfile", "clocks": ["clk"], "reset": "!rst_n"},
        "boundary": {
            "allowed_assume_inputs": ["wr_en", "clr", "din"],
            "allow_internal_assumes": ["shadow_q"],
        },
        "requirements": [
            {
                "id": "REQ-INT-01",
                "title": "Bound holds with waived internal shadow assumption",
                "assertions": ["a_bound"],
            }
        ],
        "signoff": {"proof_target": "proven", "forbid_under_ai": False},
    }
    with ctx.mcp(mode="auto") as mcp:
        opened = mcp.call_tool("session_open", {"script": "f1_regfile.jg.tcl"})
        assert_eq(opened.get("ok"), True, "session_open")
        mcp.call_tool(
            "tcl_eval",
            {"command": "assume -name a_shadow_ok {shadow_q != 8'hFF}"},
        )
        mcp.call_tool("prove", {})
        audit = mcp.call_tool("spec_audit", {"spec": spec})
        assert_json_schema_field(audit, "informal.spec_audit/1")
        assert_eq(audit["boundary_audit"]["ok"], True, "boundary_audit ok")
        assert_eq(audit.get("signoff_ready"), True, "signoff_ready")


def _t1_f1_06_spec_audit_low_power_gates_clean(ctx: E2EContext) -> None:
    _copy_soc_lp_clean(ctx)
    spec = {
        "schema": "informal.spec/1",
        "name": "Clean LP SoC Spec",
        "design": {"top": "soc_lp_clean", "clocks": ["clk"], "reset": "!rst_n"},
        "requirements": [
            {
                "id": "REQ-LP-01",
                "title": "Power acknowledge tracks PMU enable",
                "assertions": ["a_ack_pwr"],
            }
        ],
        "signoff": {"proof_target": "proven"},
        "low_power": {
            "require_upf": True,
            "max_open_lp_errors": 0,
            "require_formal_lpv": True,
            "allow_manual_lp_waivers": False,
        },
    }
    with ctx.mcp(mode="auto") as mcp:
        opened = mcp.call_tool(
            "session_open",
            {
                "files": ["soc_lp_clean.sv"],
                "top": "soc_lp_clean",
                "clock": "clk",
                "reset": "!rst_n",
            },
        )
        assert_eq(opened.get("ok"), True, "soc_lp_clean session_open")
        mcp.call_tool(
            "tcl_eval",
            {"command": "assert -name a_ack_pwr {pwr_ack == u_pmu.pwr_core_en}"},
        )
        mcp.call_tool("prove", {})
        mcp.call_tool("load_upf", {"file": "soc_lp_clean.upf"})
        lp = mcp.call_tool("check_lp", {"stage": "all"})
        assert_json_schema_field(lp, "informal.lp/1")

        audit = mcp.call_tool("spec_audit", {"spec": spec})
        assert_json_schema_field(audit, "informal.spec_audit/1")
        assert_eq(audit.get("signoff_ready"), True, "LP spec_audit signoff_ready")
        lp_gates = [g for g in audit["gates"] if "low_power" in str(g.get("gate", ""))]
        assert lp_gates, f"Expected low_power gate in gates: {audit['gates']!r}"
        for g in lp_gates:
            assert_eq(g.get("ok"), True, f"LP gate {g.get('gate')} ok")


# ---------------------------------------------------------------------------
# Feature F2: CompiledDesign Cache & Shell/MCP Modular Parity
# ---------------------------------------------------------------------------


def _t1_f2_01_docgen_help_mcp_catalog_parity(ctx: E2EContext) -> None:
    docgen = ctx.run_docgen_check()
    assert_exit(docgen, 0, "informal docgen --check")

    help_res = ctx.run_shell_commands("puts [help -json]")
    assert_exit(help_res, 0, "puts [help -json]")
    help_cmds = json.loads(help_res.stdout)
    assert isinstance(help_cmds, list) and help_cmds, "help -json must return a non-empty list"
    help_names = {entry["name"] for entry in help_cmds if isinstance(entry, dict) and "name" in entry}
    for req_cmd in ("analyze", "elaborate", "check_cov", "check_xprop", "check_spv", "help"):
        assert req_cmd in help_names, f"Missing {req_cmd!r} in help -json catalog"

    index_data = json.loads((ctx.repo_root / "docs/reference/index.json").read_text(encoding="utf-8"))
    expected_core_tools = {
        t["name"] for t in index_data["mcp_tools"] if not str(t["name"]).startswith("gui_")
    }
    with ctx.mcp(mode="auto", root=ctx.repo_root) as mcp:
        actual_tools = mcp.tool_names()
    assert_eq(actual_tools, expected_core_tools, "MCP tools/list vs docs/reference/index.json parity")


def _parse_csv_report_statuses(stdout: str) -> dict[str, str]:
    statuses: dict[str, str] = {}
    in_csv = False
    for line in stdout.splitlines():
        s = line.strip()
        if s == "property,type,status,engine,detail":
            in_csv = True
            continue
        if in_csv and s:
            parts = s.split(",")
            if len(parts) >= 3:
                statuses[parts[0]] = parts[2]
    assert statuses, f"Expected CSV report rows in stdout:\n{stdout}"
    return statuses


def _t1_f2_02_shell_check_and_mcp_script_check_parity(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f2_design_defs.svh", "f2_design.sv", "f2_flow.jg.tcl")

    cli_res = ctx.run_shell_check("f2_flow.jg.tcl", json_out=True)
    assert_exit(cli_res, 0, "shell --check --json")
    cli_json = cli_res.json()
    assert_eq(cli_json.get("findings"), [], "cli script_check findings")

    with ctx.mcp(mode="auto") as mcp:
        mcp_json = mcp.call_tool("script_check", {"script": "f2_flow.jg.tcl"})
    assert_json_schema_field(mcp_json, "informal.check/1")
    assert_eq(mcp_json.get("clean"), True, "mcp script_check clean")
    assert_eq(mcp_json.get("findings"), [], "mcp script_check findings")
    assert_eq(mcp_json.get("commands"), cli_json.get("commands"), "commands summary parity")


def _t1_f2_03_multistep_shell_mcp_session_parity(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f2_design_defs.svh", "f2_design.sv")
    base_cmds = [
        "clear -all",
        "analyze -sv12 f2_design.sv",
        "elaborate -top f2_design",
        "clock clk",
        "reset -expression {!rst_n}",
        "assert -name a_sub_xor {(en && (din == 8'h00)) |=> (dout == 8'h01)}",
        "cover  -name c_busy    {busy}",
    ]
    ctx.write_file("parity.jg.tcl", "\n".join(base_cmds) + "\n")

    shell_res = ctx.run_shell_commands(
        base_cmds
        + [
            "check_cov -init -model {coi branch}",
            "check_cov -measure",
            "check_xprop -init -targets {outputs} -prove",
            "prove -all",
            "puts [check_cov -report]",
            "puts [check_xprop -report]",
            "puts [report -csv]",
        ]
    )
    assert_exit(shell_res, 0, "shell multi-step workflow")
    shell_statuses = _parse_csv_report_statuses(shell_res.stdout)

    with ctx.mcp(mode="auto") as mcp:
        opened = mcp.call_tool("session_open", {"script": "parity.jg.tcl"})
        assert_eq(opened.get("ok"), True, "mcp session_open")
        dinfo = mcp.call_tool("design_info", {})
        assert_json_schema_field(dinfo, "informal.design_info/1")
        assert_eq(dinfo.get("top"), "f2_design", "design_info top")
        assert dinfo["design"]["flops"] > 0, f"Expected flops > 0, got {dinfo['design']}"

        cov = mcp.call_tool("check_cov", {"action": "init", "models": ["coi", "branch"]})
        assert_json_schema_field(cov, "informal.cov/1")
        cov_m = mcp.call_tool("check_cov", {"action": "measure"})
        assert_eq(cov_m.get("measured"), True, "mcp check_cov measured")

        xp_i = mcp.call_tool("check_xprop", {"action": "init", "targets": ["outputs"]})
        assert_eq(xp_i.get("initialized"), True, "mcp check_xprop initialized")
        xp_p = mcp.call_tool("check_xprop", {"action": "prove"})
        assert_json_schema_field(xp_p, "informal.xprop/1")
        assert xp_p["items"], "Expected non-empty check_xprop items"
        assert all(it["status"] == "proven" for it in xp_p["items"]), (
            f"Expected all outputs xprop-proven, got {xp_p['items']!r}"
        )

        proved = mcp.call_tool("prove", {})
        assert_json_schema_field(proved, "informal.prove/1")
        mcp_statuses = {p["name"]: p["status"] for p in proved.get("results", [])}

    assert_eq(mcp_statuses, shell_statuses, "Shell vs MCP proof verdict parity")
    assert_eq(mcp_statuses.get("a_sub_xor"), "proven", "a_sub_xor proven")
    assert_eq(mcp_statuses.get("c_busy"), "covered", "c_busy covered")


def _t1_f2_04_define_and_top_compile_sensitivity(ctx: E2EContext) -> None:
    paths = _copy_f1_f6(ctx, "f2_design_defs.svh", "f2_design.sv", "f2_alt_top.sv")

    low_narrow = ctx.run_lower(paths["f2_design.sv"], top="f2_design")
    assert_exit(low_narrow, 0, "lower narrow")
    low_wide = ctx.run_lower(paths["f2_design.sv"], top="f2_design", defines=["WIDE"])
    assert_exit(low_wide, 0, "lower -D WIDE")
    assert_in("bitvec 8", low_narrow.stdout)
    assert_in("bitvec 16", low_wide.stdout)

    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool(
            "session_open",
            {"files": ["f2_design.sv"], "top": "f2_design", "clock": "clk", "reset": "!rst_n"},
        )
        h_narrow = mcp.call_tool("design_hierarchy", {"depth": 1})
        din_narrow = next(s for s in h_narrow["scopes"][0]["signals"] if s["name"] == "din")
        assert_eq(din_narrow["width"], 8, "narrow din width")

        mcp.call_tool(
            "tcl_eval",
            {"command": "analyze -sv12 -D WIDE f2_design.sv; elaborate -top f2_design; clock clk; reset -expression {!rst_n}"},
        )
        h_wide = mcp.call_tool("design_hierarchy", {"depth": 1})
        din_wide = next(s for s in h_wide["scopes"][0]["signals"] if s["name"] == "din")
        assert_eq(din_wide["width"], 16, "WIDE din width")

        mcp.call_tool(
            "session_open",
            {"files": ["f2_alt_top.sv"], "top": "f2_alt_top", "clock": "clk", "reset": "!rst_n"},
        )
        d_alt = mcp.call_tool("design_info", {})
        assert_eq(d_alt.get("top"), "f2_alt_top", "-top f2_alt_top switch")


def _t1_f2_05_cut_point_and_bbox_per_task_isolation(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f2_design_defs.svh", "f2_design.sv")
    script = ctx.write_file(
        "cut_bbox.jg.tcl",
        "\n".join(
            [
                "clear -all",
                "analyze -sv12 f2_design.sv",
                "elaborate -top f2_design",
                "clock clk",
                "reset -expression {!rst_n}",
                "assert -name a_sub_xor {(en && (din == 8'h00)) |=> (dout == 8'h01)}",
                "task -create t_cut -copy_asserts",
                "stopat -task t_cut u_sub.y",
                "prove -all",
                "prove -task t_cut",
                "puts [report -csv]",
            ]
        )
        + "\n",
    )
    res = ctx.run_shell_file(script)
    assert_exit(res, 0, "stopat per-task isolation")
    by_task = _parse_csv_report_statuses(res.stdout)
    assert_eq(by_task.get("a_sub_xor"), "proven", "embedded un-cut proven")
    assert_eq(by_task.get("t_cut::a_sub_xor"), "cex", "t_cut cut_point cex")

    # Also verify -bbox_m sub_blk turns a_sub_xor into cex
    bbox_res = ctx.run_shell_commands(
        [
            "analyze -sv12 f2_design.sv",
            "elaborate -top f2_design -bbox_m sub_blk",
            "clock clk",
            "reset -expression {!rst_n}",
            "assert -name a_sub_xor {(en && (din == 8'h00)) |=> (dout == 8'h01)}",
            "prove -all",
            "puts [report -csv]",
        ]
    )
    assert_exit(bbox_res, 0, "elaborate -bbox_m sub_blk")
    bbox_status = _parse_csv_report_statuses(bbox_res.stdout)
    assert_eq(bbox_status.get("a_sub_xor"), "cex", "-bbox_m sub_blk makes a_sub_xor cex")


def _t1_f2_06_shell_flavor_jg_vcf_parity(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f2_design_defs.svh", "f2_design.sv")
    jg_script = ctx.write_file(
        "run_jg.tcl",
        "\n".join(
            [
                "analyze -sv12 f2_design.sv",
                "elaborate -top f2_design",
                "clock clk",
                "reset -expression {!rst_n}",
                "assert -name a_sub_xor {(en && (din == 8'h00)) |=> (dout == 8'h01)}",
                "prove -all",
                "puts [report -csv]",
            ]
        )
        + "\n",
    )
    vcf_script = ctx.write_file(
        "run_vcf.tcl",
        "\n".join(
            [
                "analyze -format sverilog f2_design.sv",
                "elaborate f2_design",
                "create_clock clk -period 10",
                "create_reset rst_n -sense low",
                "fvassert -name a_sub_xor -expr {(en && (din == 8'h00)) |=> (dout == 8'h01)}",
                "check_fv",
                "report_fv",
            ]
        )
        + "\n",
    )
    chk_jg = ctx.run_shell_check(jg_script, flavor="jg", json_out=True)
    chk_vcf = ctx.run_shell_check(vcf_script, flavor="vcf", json_out=True)
    assert_exit(chk_jg, 0, "jg --check --json")
    assert_exit(chk_vcf, 0, "vcf --check --json")
    assert_eq(chk_jg.json()["findings"], [], "jg clean")
    assert_eq(chk_vcf.json()["findings"], [], "vcf clean")

    run_jg = ctx.run_shell_file(jg_script, flavor="jg", workdir=ctx.path("w_jg"))
    run_vcf = ctx.run_shell_file(vcf_script, flavor="vcf", workdir=ctx.path("w_vcf"))
    assert_exit(run_jg, 0, "jg run")
    assert_exit(run_vcf, 0, "vcf run")
    st_jg = _parse_csv_report_statuses(run_jg.stdout)
    assert_eq(st_jg.get("a_sub_xor"), "proven", "jg a_sub_xor proven")
    assert re.search(r"a_sub_xor\s+assert\s+proven", run_vcf.stdout), (
        f"Expected a_sub_xor assert proven in VCF report_fv:\n{run_vcf.stdout}"
    )


# ---------------------------------------------------------------------------
# Feature F3: Miter VCD Replay, check_xprop, check_spv & why_cex
# ---------------------------------------------------------------------------


def _t1_f3_01_xprop_reconv_and_cex_vcd(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f3_xprop_dut.sv")
    shell_wd = ctx.path("xprop_work")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f3_xprop_dut.sv",
            "elaborate -top f3_xprop_dut",
            "clock clk",
            "reset -expression {!rst_n}",
            "check_xprop -init -targets {outputs} -prove",
            "puts [check_xprop -report]",
        ],
        workdir=shell_wd,
    )
    assert_exit(res, 0, "check_xprop outputs")
    assert re.search(r"(?:^|\.)clean_out\s+output\s+proven\b", res.stdout, re.MULTILINE), res.stdout
    assert re.search(r"(?:^|\.)reconv_out\s+output\s+proven\b", res.stdout, re.MULTILINE), res.stdout
    assert re.search(r"(?:^|\.)leaky_out\s+output\s+cex\b", res.stdout, re.MULTILINE), res.stdout

    vcds = sorted(
        p for p in shell_wd.glob("*leaky_out*.vcd") if "ctrl_leaky_out" not in p.name
    )
    assert vcds, f"Expected deterministic VCD for leaky_out CEX in {shell_wd}, got {list(shell_wd.iterdir())!r}"
    traces = sorted(
        p for p in shell_wd.glob("*leaky_out*.trace.btor2") if "ctrl_leaky_out" not in p.name
    )
    assert traces, f"Expected .trace.btor2 for leaky_out CEX in {shell_wd}"

    vcd = parse_vcd_file(vcds[0])
    divs = find_divergences(vcd)
    assert divs, f"Expected Copy 0 vs Copy 1 divergences in {vcds[0]}"
    clean_vcds = sorted(shell_wd.glob("*clean_out*.vcd")) + sorted(shell_wd.glob("*reconv_out*.vcd"))
    assert_eq(clean_vcds, [], "Safe xprop items must not emit VCD files")


def _t1_f3_02_xprop_sink_and_source_waivers(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f3_xprop_dut.sv")
    res1 = ctx.run_shell_commands(
        [
            "analyze -sv12 f3_xprop_dut.sv",
            "elaborate -top f3_xprop_dut",
            "clock clk",
            "reset -expression {!rst_n}",
            "check_xprop -init -targets {outputs} -prove",
            "puts \"BEFORE_WAIVER:\"",
            "puts [check_xprop -report]",
            "set wid [check_xprop -waiver -add -instance leaky_out -comment {ok}]",
            "puts \"WID=$wid\"",
            "puts [check_xprop -waiver -list]",
            "puts \"AFTER_WAIVER:\"",
            "puts [check_xprop -report]",
        ]
    )
    assert_exit(res1, 0, "xprop sink waiver")
    before_report, _, after_report = res1.stdout.partition("AFTER_WAIVER:")
    assert re.search(r"(?:^|\.)leaky_out\s+output\s+cex\b", before_report, re.MULTILINE), (
        f"Expected leaky_out cex before sink waiver:\n{res1.stdout}"
    )
    assert_contains(res1.stdout, "WID=1", "1: leaky_out")
    assert re.search(r"(?:^|\.)leaky_out\s+output\s+waived\b", after_report, re.MULTILINE), (
        f"Expected leaky_out waived after sink waiver:\n{res1.stdout}"
    )

    res2 = ctx.run_shell_commands(
        [
            "analyze -sv12 f3_xprop_dut.sv",
            "elaborate -top f3_xprop_dut",
            "clock clk",
            "reset -expression {!rst_n}",
            "check_xprop -init -targets {outputs}",
            "check_xprop -waiver -add -source -instance uninit_q -comment {initialized by firmware}",
            "check_xprop -prove",
            "puts [check_xprop -report]",
        ],
        workdir=ctx.path("src_waive_work"),
    )
    assert_exit(res2, 0, "xprop -source waiver")
    assert re.search(r"(?:^|\.)leaky_out\s+output\s+proven\b", res2.stdout, re.MULTILINE), (
        f"Expected leaky_out proven after -source waiver on uninit_q:\n{res2.stdout}"
    )
    assert re.search(r"(?:^|\.)ctrl_leaky_out\s+output\s+cex\b", res2.stdout, re.MULTILINE), (
        f"Expected ctrl_leaky_out cex when only uninit_q is source-waived:\n{res2.stdout}"
    )


def _t1_f3_03_spv_cond_and_covert_vcd(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f3_spv_crypto.sv")
    shell_wd = ctx.path("spv_work")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f3_spv_crypto.sv",
            "elaborate -top f3_spv_crypto",
            "clock clk",
            "reset -expression {!rst_n}",
            "check_spv -create -name iso_pub -from secret_key -to pub_status_o",
            "check_spv -create -name iso_dbg_locked -from secret_key -to debug_port_o -cond {!debug_unlock}",
            "check_spv -create -name leak_dbg_open -from secret_key -to debug_port_o",
            "check_spv -create -name leak_covert_2cyc -from secret_key -to covert_port_o",
            "check_spv -prove",
            "puts [check_spv -report]",
        ],
        workdir=shell_wd,
    )
    assert_exit(res, 0, "check_spv prove")
    assert_contains(res.stdout, "iso_pub", "iso_dbg_locked", "leak_dbg_open", "leak_covert_2cyc")
    assert re.search(r"iso_pub\s+.*proven", res.stdout), res.stdout
    assert re.search(r"iso_dbg_locked\s+.*proven", res.stdout), res.stdout
    assert re.search(r"leak_covert_2cyc\s+.*cex", res.stdout), res.stdout

    vcds = sorted(shell_wd.glob("*leak_covert_2cyc*.vcd"))
    assert vcds, f"Expected VCD for leak_covert_2cyc in {shell_wd}"
    traces = sorted(shell_wd.glob("*leak_covert_2cyc*.trace.btor2"))
    assert traces, f"Expected .trace.btor2 for leak_covert_2cyc in {shell_wd}"

    vcd = parse_vcd_file(vcds[0])
    divs = find_divergences(vcd)
    assert any("secret_key" in label for label, _, _, _ in divs), (
        f"Expected secret_key Copy0 vs Copy1 divergence in {vcds[0]}, got {divs!r}"
    )


def _t1_f3_04_mcp_xprop_spv_trace_field(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f3_xprop_dut.sv", "f3_spv_crypto.sv")
    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool(
            "session_open",
            {"files": ["f3_xprop_dut.sv"], "top": "f3_xprop_dut", "clock": "clk", "reset": "!rst_n"},
        )
        mcp.call_tool("check_xprop", {"action": "init", "targets": ["outputs"]})
        xp = mcp.call_tool("check_xprop", {"action": "prove"})
        assert_json_schema_field(xp, "informal.xprop/1")
        clean_item = _find_xprop_item(xp["items"], "clean_out")
        leaky_item = _find_xprop_item(xp["items"], "leaky_out")
        assert_eq(clean_item["status"], "proven", "clean_out proven")
        assert "trace" not in clean_item, "Safe item must omit trace"
        assert_eq(leaky_item["status"], "cex", "leaky_out cex")
        leaky_trace = ctx.path(leaky_item["trace"])
        assert leaky_trace.is_file(), f"Missing xprop trace file: {leaky_trace}"
        assert find_divergences(parse_vcd_file(leaky_trace)), "Expected VCD divergence in xprop trace"

        mcp.call_tool(
            "session_open",
            {"files": ["f3_spv_crypto.sv"], "top": "f3_spv_crypto", "clock": "clk", "reset": "!rst_n"},
        )
        mcp.call_tool(
            "check_spv",
            {"action": "create", "name": "iso_pub", "from": ["secret_key"], "to": ["pub_status_o"]},
        )
        mcp.call_tool(
            "check_spv",
            {"action": "create", "name": "leak_covert", "from": ["secret_key"], "to": ["covert_port_o"]},
        )
        spv = mcp.call_tool("check_spv", {"action": "prove"})
        assert_json_schema_field(spv, "informal.spv/1")
        rules = {r["name"]: r for r in spv["rules"]}
        assert_eq(rules["iso_pub"]["status"], "proven", "iso_pub proven")
        assert "trace" not in rules["iso_pub"], "Safe SPV rule must omit trace"
        assert_eq(rules["leak_covert"]["status"], "cex", "leak_covert cex")
        spv_trace = ctx.path(rules["leak_covert"]["trace"])
        assert spv_trace.is_file(), f"Missing SPV trace file: {spv_trace}"
        assert find_divergences(parse_vcd_file(spv_trace)), "Expected VCD divergence in SPV trace"


def _t1_f3_05_why_cex_std_and_miter_slices(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f3_why_cex_mux.sv", "f3_xprop_dut.sv", "f3_spv_crypto.sv")
    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool(
            "session_open",
            {"files": ["f3_why_cex_mux.sv"], "top": "f3_why_cex_mux", "clock": "clk", "reset": "!rst_n"},
        )
        mcp.call_tool(
            "tcl_eval",
            {
                "command": (
                    "assume -name a_arm0 {!sel_arm1}; "
                    "assert -name a_no_err {!err_flag}; "
                    "assert -name a_always_safe {safe_ok}"
                )
            },
        )
        mcp.call_tool("prove", {})
        wc_std = mcp.call_tool("why_cex", {"name": "a_no_err"})
        assert_json_schema_field(wc_std, "informal.why_cex/1")
        assert_eq(wc_std["status"], "cex", "a_no_err status")
        assert wc_std["events"], "Expected non-empty events for a_no_err"
        active = " ".join(wc_std["active_signals"])
        assert_in("bug_q", active)
        assert_not_in("unused_lane_q", active)
        assert wc_std["active_slice_signals"] <= wc_std["static_cone_signals"]

        # xprop CEX slice via why_cex
        mcp.call_tool(
            "session_open",
            {"files": ["f3_xprop_dut.sv"], "top": "f3_xprop_dut", "clock": "clk", "reset": "!rst_n"},
        )
        mcp.call_tool("check_xprop", {"action": "init", "targets": ["outputs"]})
        xp_res = mcp.call_tool("check_xprop", {"action": "prove"})
        leaky_name = _find_xprop_item(xp_res["items"], "leaky_out")["name"]
        wc_xp = mcp.call_tool("why_cex", {"name": leaky_name})
        assert_json_schema_field(wc_xp, "informal.why_cex/1")
        assert_eq(wc_xp["status"], "cex", "xprop why_cex status")
        assert wc_xp["events"] and wc_xp["active_signals"], "Expected xprop why_cex causal slice"

        # spv CEX slice via why_cex
        mcp.call_tool(
            "session_open",
            {"files": ["f3_spv_crypto.sv"], "top": "f3_spv_crypto", "clock": "clk", "reset": "!rst_n"},
        )
        mcp.call_tool(
            "check_spv",
            {"action": "create", "name": "leak_covert_2cyc", "from": ["secret_key"], "to": ["covert_port_o"]},
        )
        mcp.call_tool("check_spv", {"action": "prove"})
        wc_spv = mcp.call_tool("why_cex", {"name": "leak_covert_2cyc"})
        assert_json_schema_field(wc_spv, "informal.why_cex/1")
        assert_eq(wc_spv["status"], "cex", "spv why_cex status")
        assert wc_spv["events"] and wc_spv["active_signals"], "Expected spv why_cex causal slice"


def _t1_f3_06_xprop_control_and_memories_targets(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f3_xprop_dut.sv")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f3_xprop_dut.sv",
            "elaborate -top f3_xprop_dut",
            "clock clk",
            "reset -expression {!rst_n}",
            "check_xprop -init -targets {control memories} -prove",
            "puts [check_xprop -report]",
        ]
    )
    assert_exit(res, 0, "check_xprop control memories")
    assert re.search(r"\bcontrol\s+cex\b", res.stdout), (
        f"Expected control category cex in check_xprop -report:\n{res.stdout}"
    )
    assert re.search(r"\bmemory\s+cex\b", res.stdout), (
        f"Expected memory category cex in check_xprop -report:\n{res.stdout}"
    )

    res_to = ctx.run_shell_commands(
        [
            "analyze -sv12 f3_xprop_dut.sv",
            "elaborate -top f3_xprop_dut",
            "clock clk",
            "reset -expression {!rst_n}",
            "check_xprop -init -to {ctrl_leaky_out} -prove",
            "puts [check_xprop -report]",
        ],
        workdir=ctx.path("to_work"),
    )
    assert_exit(res_to, 0, "check_xprop -to ctrl_leaky_out")
    assert re.search(
        r"(?:^|\.)ctrl_leaky_out\s+(?:explicit|output)\s+cex\b",
        res_to.stdout,
        re.MULTILINE,
    ), res_to.stdout


# ---------------------------------------------------------------------------
# Feature F4: UPF 1.0-3.1 / CPF Parsing, ObjectFinder, Precedence & D13
# ---------------------------------------------------------------------------


def _t1_f4_01_upf_1_0_pst_and_supply_nets(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f4_upf10_basic.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf f4_upf10_basic.upf -version 1.0",
            "check_lp -stage upf",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "UPF 1.0 check_lp -stage upf")
    _assert_lp_clean(_parse_last_json_object(res.stdout))


def _t1_f4_02_upf_2_1_and_3_1_tcl_control_flow(ctx: E2EContext) -> None:
    gen = generate_n_domain_upf(4, include_contradiction=False, upf_version="2.1")
    ctx.write_file("gen4.sv", gen.sv)
    ctx.write_file("gen4.upf", gen.upf)
    res21 = ctx.run_shell_commands(
        [
            "analyze -sv12 gen4.sv",
            "elaborate -top soc_top",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf gen4.upf -version 2.1",
            "check_lp -stage upf",
            "puts [report_lp -json]",
        ],
        workdir=ctx.path("w21"),
    )
    assert_exit(res21, 0, "UPF 2.1 check_lp")
    _assert_lp_clean(_parse_last_json_object(res21.stdout))

    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f4_upf31_tcl_flow.upf")
    res31 = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf f4_upf31_tcl_flow.upf -version 3.1",
            "check_lp -stage upf",
            "puts [report_lp -json]",
        ],
        workdir=ctx.path("w31"),
    )
    assert_exit(res31, 0, "UPF 3.1 Tcl foreach/proc/set_scope")
    _assert_lp_clean(_parse_last_json_object(res31.stdout))


def _t1_f4_03_nested_load_upf_scope_and_object_finder(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f4_upf_parent.upf", "f4_upf_subip.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf f4_upf_parent.upf",
            "check_lp -stage upf",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "nested load_upf -scope u_sub + find_objects")
    assert_not_in("called from inside itself", res.combined)
    _assert_lp_clean(_parse_last_json_object(res.stdout))


def _t1_f4_04_cadence_cpf_frontend_parity(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f4_design_basic.cpf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf f4_design_basic.cpf -cpf",
            "check_lp -stage upf",
            "check_lp -stage rtl",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "load_upf -cpf")
    _assert_lp_clean(_parse_last_json_object(res.stdout))


def _t1_f4_05_two_phase_forward_ref_and_precedence(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f4_upf_precedence.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf f4_upf_precedence.upf",
            "check_lp -stage upf",
            "check_lp -stage rtl",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "two-phase forward refs & strategy precedence")
    _assert_lp_clean(_parse_last_json_object(res.stdout))


def _t1_f4_06_d13_benign_layout_hints_note_only(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f4_upf31_tcl_flow.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf f4_upf31_tcl_flow.upf",
            "check_lp -stage upf",
        ]
    )
    assert_exit(res, 0, "D13 benign layout hints (-name_prefix/-name_suffix)")
    assert_in("note", res.combined.lower(), "Expected benign layout hint note")


# ---------------------------------------------------------------------------
# Feature F5: Symbolic PST SAT Solver (pst_sat) & compare_upf
# ---------------------------------------------------------------------------


def _t1_f5_01_pst_sat_supply_and_logic_expr_clean(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f5_pst_clean.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf f5_pst_clean.upf",
            "check_lp -stage upf",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "pst_sat supply_expr & logic_expr clean")
    _assert_lp_clean(_parse_last_json_object(res.stdout))


def _t1_f5_02_pst_sat_32_domain_symbolic_scaling(ctx: E2EContext) -> None:
    gen32 = generate_n_domain_upf(32, include_contradiction=False, upf_version="2.1")
    ctx.write_file("soc32.sv", gen32.sv)
    ctx.write_file("soc32.upf", gen32.upf)
    t0 = time.monotonic()
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 soc32.sv",
            "elaborate -top soc_top",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf soc32.upf",
            "check_lp -stage upf",
            "puts [report_lp -json]",
        ],
        timeout_s=30.0,
    )
    elapsed = time.monotonic() - t0
    assert_exit(res, 0, "32-domain symbolic pst_sat")
    _assert_lp_clean(_parse_last_json_object(res.stdout))
    assert elapsed < 20.0, f"32-domain symbolic PST SAT took too long ({elapsed:.2f}s)"


def _t1_f5_03_pst_sat_illegal_blocks_false_crossing(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f5_illegal_blocks_crossing.sv", "f5_illegal_blocks_crossing.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f5_illegal_blocks_crossing.sv",
            "elaborate -top f5_illegal_blocks_crossing",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf f5_illegal_blocks_crossing.upf",
            "check_lp -stage upf",
            "check_lp -stage rtl",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "pst_sat -illegal pruning")
    _assert_lp_clean(_parse_last_json_object(res.stdout))


def _t1_f5_04_pst_sat_set_equivalent_coherence(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f5_equiv_clean.sv", "f5_equiv_clean.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f5_equiv_clean.sv",
            "elaborate -top f5_equiv_clean",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf f5_equiv_clean.upf",
            "check_lp -stage upf",
            "check_lp -stage rtl",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "set_equivalent coherent union-find")
    _assert_lp_clean(_parse_last_json_object(res.stdout))


def _t1_f5_05_compare_upf_equivalent_passes(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f5_cmp_golden.upf", "f5_cmp_refined_ok.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "compare_upf -golden f5_cmp_golden.upf -revised f5_cmp_refined_ok.upf",
        ]
    )
    assert_exit(res, 0, "compare_upf equivalent UPFs")
    assert_not_in("mismatch", res.combined.lower())


def _t1_f5_06_microvolt_tolerance_within_1000uv(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f5_uv_within_tol.sv", "f5_uv_within_tol.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f5_uv_within_tol.sv",
            "elaborate -top f5_uv_tol",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf f5_uv_within_tol.upf",
            "check_lp -stage rtl",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "500 uV within 1000 uV tolerance")
    _assert_lp_clean(_parse_last_json_object(res.stdout))


# ---------------------------------------------------------------------------
# Feature F6: Static LP Stages 1-2, Waivers, Reporting & LP Tcl Shell
# ---------------------------------------------------------------------------


def _t1_f6_01_clean_soc_stages_1_and_2(ctx: E2EContext) -> None:
    _copy_soc_lp_clean(ctx)
    for stage2, extra in (("rtl", ""), ("design", ""), ("all", " -timeout 30s")):
        res = ctx.run_shell_commands(
            [
                "analyze -sv12 soc_lp_clean.sv",
                "elaborate -top soc_lp_clean",
                "clock clk",
                "reset -expression {!rst_n}",
                "load_upf soc_lp_clean.upf",
                "check_lp -stage upf",
                f"check_lp -stage {stage2}{extra}",
                "puts [report_lp -json]",
            ],
            workdir=ctx.path(f"w_{stage2}"),
        )
        assert_exit(res, 0, f"soc_lp_clean -stage {stage2}{extra}")
        _assert_lp_clean(_parse_last_json_object(res.stdout))


def _t1_f6_02_report_lp_modes_and_json_fields(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f6_bus64_uniso.sv", "f6_bus64_uniso.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f6_bus64_uniso.sv",
            "elaborate -top f6_bus64_uniso",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf f6_bus64_uniso.upf",
            "check_lp -stage rtl",
            "puts [report_lp -summary]",
            "puts [report_lp -verbose]",
            "puts [report_lp -rule ISO_* -status open]",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "report_lp formatting modes")
    assert_contains(res.stdout, "ISO_MISSING")
    rep = _parse_last_json_object(res.stdout)
    viols = _extract_lp_violations(rep)
    iso = [v for v in viols if v.get("rule_id") == "ISO_MISSING"]
    assert iso, f"Expected ISO_MISSING in report_lp -json: {viols!r}"
    assert_has_keys(iso[0], "rule_id", "severity", "stage", "status", "message")


def _t1_f6_03_pure_feedthrough_single_crossing(ctx: E2EContext) -> None:
    _copy_soc_lp_clean(ctx)
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 soc_lp_clean.sv",
            "elaborate -top soc_lp_clean",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf soc_lp_clean.upf",
            "check_lp -stage rtl",
            "puts [report_lp -verbose]",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "pure feedthrough check_lp")
    assert_not_in("ISO_FEEDTHROUGH_BUFFER", res.stdout)
    _assert_lp_clean(_parse_last_json_object(res.stdout))


def _t1_f6_04_bus64_collapsed_single_crossing_group(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f6_bus64_uniso.sv", "f6_bus64_uniso.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f6_bus64_uniso.sv",
            "elaborate -top f6_bus64_uniso",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf f6_bus64_uniso.upf",
            "check_lp -stage rtl",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "64-bit bus CrossingGroup")
    viols = _extract_lp_violations(_parse_last_json_object(res.stdout))
    iso_missing = [v for v in viols if v.get("rule_id") == "ISO_MISSING"]
    assert_eq(len(iso_missing), 1, "64-bit bus must collapse into 1 ISO_MISSING CrossingGroup")
    assert_eq(iso_missing[0].get("width_bits"), 64, "CrossingGroup width_bits")


def _t1_f6_05_manual_waiver_lifecycle_and_status_filter(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f6_bus64_uniso.sv", "f6_bus64_uniso.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f6_bus64_uniso.sv",
            "elaborate -top f6_bus64_uniso",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf f6_bus64_uniso.upf",
            "check_lp -stage rtl",
            "set wid [set_lp_waiver -add -rule {ISO_MISSING} -src_domain PD_SW -dst_domain PD_DST -comment {ok}]",
            "puts \"WAIVER_ID=$wid\"",
            "puts \"LIST=[set_lp_waiver -list]\"",
            "puts \"OPEN_AFTER_ADD=[report_lp -status open -json]\"",
            "puts \"WAIVED_AFTER_ADD=[report_lp -status waived -json]\"",
            "set_lp_waiver -remove $wid",
            "puts \"OPEN_AFTER_RM=[report_lp -status open -json]\"",
        ]
    )
    assert_exit(res, 0, "set_lp_waiver add/list/remove")
    decoder = json.JSONDecoder()
    open_add_m = re.search(r"OPEN_AFTER_ADD=\s*", res.stdout)
    waived_add_m = re.search(r"WAIVED_AFTER_ADD=\s*", res.stdout)
    open_rm_m = re.search(r"OPEN_AFTER_RM=\s*", res.stdout)
    assert open_add_m and waived_add_m and open_rm_m, res.stdout

    open_after_add = _extract_lp_violations(decoder.raw_decode(res.stdout, open_add_m.end())[0])
    waived_after_add = _extract_lp_violations(decoder.raw_decode(res.stdout, waived_add_m.end())[0])
    open_after_rm = _extract_lp_violations(decoder.raw_decode(res.stdout, open_rm_m.end())[0])
    assert_eq(
        [v for v in open_after_add if v.get("rule_id") == "ISO_MISSING"],
        [],
        "waived ISO_MISSING excluded by -status open",
    )
    assert_eq(
        len([v for v in waived_after_add if v.get("rule_id") == "ISO_MISSING"]),
        1,
        "waived ISO_MISSING returned by -status waived",
    )
    assert_eq(
        len([v for v in open_after_rm if v.get("rule_id") == "ISO_MISSING"]),
        1,
        "removed waiver restores ISO_MISSING to open",
    )


def _t1_f6_06_lp_tcl_shell_jg_vcf_parity_and_check_dry_run(ctx: E2EContext) -> None:
    _copy_soc_lp_clean(ctx)
    _copy_f1_f6(ctx, "f6_lp_check_clean.tcl", "f6_lp_check_clean.vcf.tcl")
    scripts = {
        "jg": "f6_lp_check_clean.tcl",
        "vcf": "f6_lp_check_clean.vcf.tcl",
    }
    for flavor, script in scripts.items():
        chk = ctx.run_shell_check(script, flavor=flavor, json_out=True)
        assert_exit(chk, 0, f"LP script --check --json ({flavor})")
        data = chk.json()
        assert_eq(data.get("findings"), [], f"{flavor} LP script findings")
        for cmd in ("load_upf", "check_lp", "report_lp", "set_lp_waiver", "compare_upf", "check_lpv"):
            assert cmd in data.get("commands", {}), f"Expected {cmd} honoured in {flavor} --check"

        hlp = ctx.run_shell_commands(["puts [help check_lp]", "puts [help -apps]"], flavor=flavor)
        assert_exit(hlp, 0, f"help check_lp ({flavor})")
        assert_contains(hlp.stdout, "check_lp", "-stage")


def get_cases() -> list[TestCase]:
    return [
        # F1 (6 cases)
        TestCase("T1-F1-01", 1, "F1", "Baseline hygiene, docs, docgen, and examples --check-only gates", _t1_f1_01_baseline_repo_gates, timeout_s=240.0, tags=("baseline", "docs")),
        TestCase("T1-F1-02", 1, "F1", "spec_audit auto-discovery and signoff on tests/fpv/fifo", _t1_f1_02_spec_audit_fifo_autodiscovery, timeout_s=60.0, tags=("mcp", "spec_audit")),
        TestCase("T1-F1-03", 1, "F1", "spec_audit require_dead_end_check with live check_dead_end=true", _t1_f1_03_spec_audit_dead_end_check, timeout_s=60.0, tags=("mcp", "spec_audit")),
        TestCase("T1-F1-04", 1, "F1", "spec_audit bounded_proven target with min_bound satisfaction", _t1_f1_04_spec_audit_bounded_proven_min_bound, timeout_s=60.0, tags=("mcp", "spec_audit")),
        TestCase("T1-F1-05", 1, "F1", "spec_audit boundary.allow_internal_assumes waives internal assume", _t1_f1_05_spec_audit_allow_internal_assumes, timeout_s=60.0, tags=("mcp", "spec_audit")),
        TestCase("T1-F1-06", 1, "F1", "spec_audit low_power signoff gates pass on clean LP SoC", _t1_f1_06_spec_audit_low_power_gates_clean, timeout_s=90.0, tags=("mcp", "spec_audit", "lp")),
        # F2 (6 cases)
        TestCase("T1-F2-01", 1, "F2", "docgen --check, Shell help -json, and MCP tools/list catalog parity", _t1_f2_01_docgen_help_mcp_catalog_parity, timeout_s=60.0, tags=("docgen", "mcp", "shell")),
        TestCase("T1-F2-02", 1, "F2", "Clean script shell --check --json and MCP script_check parity", _t1_f2_02_shell_check_and_mcp_script_check_parity, timeout_s=60.0, tags=("shell", "mcp")),
        TestCase("T1-F2-03", 1, "F2", "Multi-step elaborate -> check_cov -> check_xprop -> prove parity", _t1_f2_03_multistep_shell_mcp_session_parity, timeout_s=90.0, tags=("cache", "shell", "mcp")),
        TestCase("T1-F2-04", 1, "F2", "Compile-option sensitivity for -D define and -top selection", _t1_f2_04_define_and_top_compile_sensitivity, timeout_s=60.0, tags=("cache", "lower", "mcp")),
        TestCase("T1-F2-05", 1, "F2", "Lowering-option sensitivity for stopat cut_point and -bbox_m", _t1_f2_05_cut_point_and_bbox_per_task_isolation, timeout_s=60.0, tags=("cache", "shell")),
        TestCase("T1-F2-06", 1, "F2", "Shell --flavor jg and --flavor vcf parity on proof flow", _t1_f2_06_shell_flavor_jg_vcf_parity, timeout_s=60.0, tags=("shell", "jg", "vcf")),
        # F3 (6 cases)
        TestCase("T1-F3-01", 1, "F3", "check_xprop reconvergent proof and leaky CEX deterministic VCD", _t1_f3_01_xprop_reconv_and_cex_vcd, timeout_s=60.0, tags=("xprop", "vcd")),
        TestCase("T1-F3-02", 1, "F3", "check_xprop sink waiver and -source tie-off waiver", _t1_f3_02_xprop_sink_and_source_waivers, timeout_s=60.0, tags=("xprop", "waiver")),
        TestCase("T1-F3-03", 1, "F3", "check_spv -cond isolation proof and covert leak VCD divergence", _t1_f3_03_spv_cond_and_covert_vcd, timeout_s=60.0, tags=("spv", "vcd")),
        TestCase("T1-F3-04", 1, "F3", "MCP check_xprop and check_spv trace field on CEX only", _t1_f3_04_mcp_xprop_spv_trace_field, timeout_s=90.0, tags=("mcp", "xprop", "spv", "vcd")),
        TestCase("T1-F3-05", 1, "F3", "why_cex causal slice on standard, xprop, and spv counterexamples", _t1_f3_05_why_cex_std_and_miter_slices, timeout_s=90.0, tags=("why_cex", "mcp", "xprop", "spv")),
        TestCase("T1-F3-06", 1, "F3", "check_xprop -targets {control memories} and explicit -to target", _t1_f3_06_xprop_control_and_memories_targets, timeout_s=60.0, tags=("xprop",)),
        # F4 (6 cases)
        TestCase("T1-F4-01", 1, "F4", "UPF 1.0 create_pst and supply nets clean elaboration", _t1_f4_01_upf_1_0_pst_and_supply_nets, timeout_s=60.0, tags=("upf", "upf10")),
        TestCase("T1-F4-02", 1, "F4", "UPF 2.1 and 3.1 Tcl foreach/proc/set_scope/add_power_state", _t1_f4_02_upf_2_1_and_3_1_tcl_control_flow, timeout_s=60.0, tags=("upf", "upf21", "upf31")),
        TestCase("T1-F4-03", 1, "F4", "Nested load_upf -scope re-entrancy and live find_objects", _t1_f4_03_nested_load_upf_scope_and_object_finder, timeout_s=60.0, tags=("upf", "nested")),
        TestCase("T1-F4-04", 1, "F4", "Cadence CPF 2.0 load_upf -cpf canonical PowerIntent parity", _t1_f4_04_cadence_cpf_frontend_parity, timeout_s=60.0, tags=("cpf",)),
        TestCase("T1-F4-05", 1, "F4", "Two-phase forward refs and strategy precedence (-update/-no_isolation)", _t1_f4_05_two_phase_forward_ref_and_precedence, timeout_s=60.0, tags=("upf", "precedence")),
        TestCase("T1-F4-06", 1, "F4", "Decision D13 benign layout hints accepted with Note only", _t1_f4_06_d13_benign_layout_hints_note_only, timeout_s=60.0, tags=("upf", "d13")),
        # F5 (6 cases)
        TestCase("T1-F5-01", 1, "F5", "pst_sat consistent multi-domain -supply_expr and -logic_expr", _t1_f5_01_pst_sat_supply_and_logic_expr_clean, timeout_s=60.0, tags=("pst_sat",)),
        TestCase("T1-F5-02", 1, "F5", "pst_sat 32-domain symbolic PST SAT scaling without blowup", _t1_f5_02_pst_sat_32_domain_symbolic_scaling, timeout_s=60.0, tags=("pst_sat", "scale")),
        TestCase("T1-F5-03", 1, "F5", "pst_sat -illegal state blocks false off-while-on crossing", _t1_f5_03_pst_sat_illegal_blocks_false_crossing, timeout_s=60.0, tags=("pst_sat",)),
        TestCase("T1-F5-04", 1, "F5", "pst_sat set_equivalent coherent union-find supplies", _t1_f5_04_pst_sat_set_equivalent_coherence, timeout_s=60.0, tags=("pst_sat", "equiv")),
        TestCase("T1-F5-05", 1, "F5", "compare_upf passes on semantically equivalent golden vs revised UPF", _t1_f5_05_compare_upf_equivalent_passes, timeout_s=60.0, tags=("compare_upf",)),
        TestCase("T1-F5-06", 1, "F5", "pst_sat microvolt tolerance within 1000 uV requires no LS", _t1_f5_06_microvolt_tolerance_within_1000uv, timeout_s=60.0, tags=("pst_sat", "voltage")),
        # F6 (6 cases)
        TestCase("T1-F6-01", 1, "F6", "Clean multi-domain SoC Stage 1 (upf) and Stage 2 (rtl/design)", _t1_f6_01_clean_soc_stages_1_and_2, timeout_s=60.0, tags=("lp_static",)),
        TestCase("T1-F6-02", 1, "F6", "report_lp -summary, -verbose, -rule, -status, and -json modes", _t1_f6_02_report_lp_modes_and_json_fields, timeout_s=60.0, tags=("lp_static", "report_lp")),
        TestCase("T1-F6-03", 1, "F6", "Pure feedthrough PD_CORE -> PD_MID -> PD_RET single crossing", _t1_f6_03_pure_feedthrough_single_crossing, timeout_s=60.0, tags=("lp_static", "feedthrough")),
        TestCase("T1-F6-04", 1, "F6", "64-bit bus collapsed into 1 CrossingGroup with width_bits=64", _t1_f6_04_bus64_collapsed_single_crossing_group, timeout_s=60.0, tags=("lp_static", "bus")),
        TestCase("T1-F6-05", 1, "F6", "set_lp_waiver -add/-list/-remove and -status open/waived filters", _t1_f6_05_manual_waiver_lifecycle_and_status_filter, timeout_s=60.0, tags=("lp_static", "waiver")),
        TestCase("T1-F6-06", 1, "F6", "LP Tcl shell jg/vcf parity, help check_lp, and --check --json", _t1_f6_06_lp_tcl_shell_jg_vcf_parity_and_check_dry_run, timeout_s=60.0, tags=("lp_static", "shell", "jg", "vcf")),
    ]
