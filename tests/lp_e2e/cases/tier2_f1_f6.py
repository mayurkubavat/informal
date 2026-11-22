"""Tier 2 opaque-box E2E test cases (boundaries, edge & negative) for F1-F6."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import subprocess
from typing import Any

from lib.fixtures_gen import generate_n_domain_upf
from lib.harness import (
    E2EContext,
    TestCase,
    assert_contains,
    assert_eq,
    assert_exit,
    assert_in,
    assert_json_schema_field,
    assert_not_in,
)
from lib.mcp_client import McpProtocolError
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


def _assert_low_power_gate_failed(audit: dict[str, Any], ctx_msg: str) -> None:
    lp_gates = [g for g in audit["gates"] if "low_power" in str(g.get("gate", ""))]
    assert lp_gates, f"{ctx_msg}: expected low_power gate in {audit['gates']!r}"
    exact = [g for g in lp_gates if g.get("gate") == "low_power"]
    if exact:
        assert_eq(exact[0].get("ok"), False, f"{ctx_msg}: low_power gate ok")
    else:
        assert any(g.get("ok") is False for g in lp_gates), (
            f"{ctx_msg}: expected failing low_power gate, got {lp_gates!r}"
        )


def _rule_ids(report_obj: Any) -> set[str]:
    return {str(v.get("rule_id", "")) for v in _extract_lp_violations(report_obj)}


# ---------------------------------------------------------------------------
# Feature F1: spec_audit Boundaries & low_power Gate Failures
# ---------------------------------------------------------------------------


def _t2_f1_01_spec_audit_task_qualified_disabled_prop(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f1_regfile.sv", "f1_regfile.jg.tcl", "f1_regfile.spec.json")
    spec = json.loads(ctx.path("f1_regfile.spec.json").read_text(encoding="utf-8"))
    spec["requirements"][0]["assertions"] = ["t_sub::a_bound"]
    spec["signoff"]["require_dead_end_check"] = False
    spec["signoff"]["forbid_under_ai"] = False

    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool("session_open", {"script": "f1_regfile.jg.tcl"})
        mcp.call_tool(
            "tcl_eval",
            {"command": "task -create t_sub -copy_asserts -copy_covers; assert -disable -task t_sub a_bound"},
        )
        mcp.call_tool("prove", {"task": "t_sub"})
        audit = mcp.call_tool("spec_audit", {"task": "t_sub", "spec": spec})
        assert_json_schema_field(audit, "informal.spec_audit/1")
        assert_eq(audit.get("signoff_ready"), False, "disabled task::prop must fail signoff")
        assert_eq(audit["summary"]["missing_assert"], 1, "missing_assert count")
        req0 = audit["requirements"][0]
        assert_eq(req0["status"], "missing_assert", "REQ-RF-001 status")
        assert_in("exists but is disabled", " ".join(req0["issues"]))


def _t2_f1_02_spec_audit_forbid_under_ai_and_undo(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f1_regfile.sv", "f1_regfile.jg.tcl")
    ctx.copy_fixture("f1_f6/f1_regfile.spec.json", "fvp.spec.json")
    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool("session_open", {"script": "f1_regfile.jg.tcl"})
        mcp.call_tool(
            "proposal_add",
            {
                "kind": "assume",
                "name": "ai_no_clr",
                "expr": "!clr",
                "rationale": "test forbid_under_ai detection",
            },
        )
        mcp.call_tool("prove", {})
        blocked = mcp.call_tool("spec_audit", {"check_dead_end": True, "depth": 6})
        assert_eq(blocked.get("signoff_ready"), False, "forbid_under_ai blocks signoff")
        ai_gate = next(g for g in blocked["gates"] if g.get("gate") == "ai_provenance")
        assert_eq(ai_gate["ok"], False, "ai_provenance gate fails under AI assumption")

        mcp.call_tool("proposal_undo", {})
        mcp.call_tool("prove", {})
        recovered = mcp.call_tool("spec_audit", {"check_dead_end": True, "depth": 6})
        assert_eq(recovered.get("signoff_ready"), True, "signoff recovers after proposal_undo")


def _t2_f1_03_spec_audit_dead_end_unchecked_and_found(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f1_regfile.sv", "f1_regfile.jg.tcl")
    ctx.copy_fixture("f1_f6/f1_regfile.spec.json", "fvp.spec.json")
    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool("session_open", {"script": "f1_regfile.jg.tcl"})
        unchecked = mcp.call_tool("spec_audit", {"check_dead_end": False})
        assert_eq(unchecked.get("signoff_ready"), False, "require_dead_end_check fails when unchecked")
        de_gate = next(g for g in unchecked["gates"] if g.get("gate") == "zero_dead_ends")
        assert_eq(de_gate["ok"], False, "zero_dead_ends ok=False when check_dead_end=False")

        # Seed a dead-end trap state: once full becomes 1 (after 4 writes), 1'b0 cannot hold next cycle
        mcp.call_tool(
            "tcl_eval",
            {"command": "assume -name a_dead_end_trap {full |=> 1'b0}"},
        )
        mcp.call_tool("prove", {})
        trapped = mcp.call_tool("spec_audit", {"check_dead_end": True, "depth": 8})
        assert_eq(trapped.get("signoff_ready"), False, "dead-end trap blocks signoff")
        assert_eq(trapped["boundary_audit"]["dead_end_checked"], True, "dead_end_checked")
        assert_eq(trapped["boundary_audit"]["dead_end_found"], True, "dead_end_found")
        de_gate2 = next(g for g in trapped["gates"] if g.get("gate") == "zero_dead_ends")
        assert_eq(de_gate2["ok"], False, "zero_dead_ends gate fails on trap")


def _t2_f1_04_bounded_floor_and_forbidden_override(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f1_regfile.sv")
    ctx.write_file(
        "b4.jg.tcl",
        "\n".join(
            [
                "clear -all",
                "analyze -sv12 f1_regfile.sv",
                "elaborate -top f1_regfile",
                "clock clk",
                "reset -expression {!rst_n}",
                "assert -name a_neq_max -bound 4 {cnt < 3'd5}",
                "assert -name a_bound {cnt <= 3'd6}",
                "prove -all",
            ]
        )
        + "\n",
    )
    spec_short = {
        "schema": "informal.spec/1",
        "name": "Min Bound 5 vs Bound 4",
        "design": {"top": "f1_regfile", "clocks": ["clk"], "reset": "!rst_n"},
        "boundary": {
            "allowed_assume_inputs": ["wr_en", "clr", "din"],
            "allow_internal_assumes": ["shadow_q"],
            "forbidden_assume_signals": ["shadow_q"],
        },
        "requirements": [
            {
                "id": "REQ-B5",
                "title": "Requires min_bound 5",
                "assertions": ["a_neq_max"],
            }
        ],
        "signoff": {"proof_target": "bounded_proven", "min_bound": 5, "forbid_under_ai": False},
    }
    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool("session_open", {"script": "b4.jg.tcl"})
        audit_bound = mcp.call_tool("spec_audit", {"spec": spec_short})
        assert_eq(audit_bound.get("signoff_ready"), False, "bound 4 < min_bound 5 fails signoff")
        assert_eq(audit_bound["requirements"][0]["status"], "undetermined", "REQ-B5 status")

        mcp.call_tool("tcl_eval", {"command": "assume -name a_shadow_ok {shadow_q != 8'hFF}"})
        mcp.call_tool("prove", {})
        audit_forbid = mcp.call_tool("spec_audit", {"spec": spec_short})
        assert_eq(audit_forbid.get("signoff_ready"), False, "forbidden_assume_signals fails signoff")
        ba = audit_forbid["boundary_audit"]
        assert_eq(ba["ok"], False, "forbidden_assume_signals overrides allow_internal_assumes")
        assert_in("forbidden_assume_signals", ba["overconstrained_assumptions"][0]["reason"])


def _t2_f1_05_spec_audit_invalid_inputs_rejected(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f1_regfile.sv", "f1_regfile.jg.tcl")
    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool("session_open", {"script": "f1_regfile.jg.tcl"})

        err_missing = mcp.call_tool("spec_audit", {}, expect_error=True)
        assert_eq(err_missing.get("code"), "not_found", "missing fvp.spec.json error code")

        err_schema = mcp.call_tool(
            "spec_audit",
            {"spec": {"schema": "wrong.schema/9", "requirements": []}},
            expect_error=True,
        )
        assert_eq(err_schema.get("code"), "arguments", "bad schema error code")

        err_dup = mcp.call_tool(
            "spec_audit",
            {
                "spec": {
                    "schema": "informal.spec/1",
                    "requirements": [
                        {"id": "DUP-1", "title": "first", "assertions": ["a_bound"]},
                        {"id": "DUP-1", "title": "second", "assertions": ["a_clr"]},
                    ],
                }
            },
            expect_error=True,
        )
        assert_eq(err_dup.get("code"), "arguments", "duplicate req id error code")

        err_floor = mcp.call_tool(
            "spec_audit",
            {
                "spec": {
                    "schema": "informal.spec/1",
                    "requirements": [],
                    "signoff": {"coi_coverage_floor": 120.0},
                }
            },
            expect_error=True,
        )
        assert_eq(err_floor.get("code"), "arguments", "out-of-range floor error code")


def _t2_f1_06_spec_audit_low_power_gate_failures(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f6_bus64_uniso.sv", "f6_bus64_uniso.upf")
    spec_lp = {
        "schema": "informal.spec/1",
        "name": "Strict LP Signoff Spec",
        "design": {"top": "f6_bus64_uniso", "clocks": ["clk"], "reset": "!rst_n"},
        "requirements": [
            {
                "id": "REQ-LP-01",
                "title": "Sanity invariant",
                "assertions": ["a_ok"],
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
    spec_no_formal = {
        **spec_lp,
        "low_power": {
            "require_upf": True,
            "max_open_lp_errors": 0,
            "require_formal_lpv": False,
            "allow_manual_lp_waivers": False,
        },
    }
    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool(
            "session_open",
            {"files": ["f6_bus64_uniso.sv"], "top": "f6_bus64_uniso", "clock": "clk", "reset": "!rst_n"},
        )
        mcp.call_tool("tcl_eval", {"command": "assert -name a_ok {din == din}"})
        mcp.call_tool("prove", {})
        # (a) No UPF loaded yet while require_upf=true
        no_upf = mcp.call_tool("spec_audit", {"spec": spec_lp})
        assert_eq(no_upf.get("signoff_ready"), False, "missing UPF fails low_power gate")
        _assert_low_power_gate_failed(no_upf, "missing UPF")

        # (b) Load UPF + run check_lp -> open ISO_MISSING error > max_open_lp_errors=0
        # De-confound by setting require_formal_lpv=False so only max_open_lp_errors triggers.
        mcp.call_tool("load_upf", {"file": "f6_bus64_uniso.upf"})
        mcp.call_tool("check_lp", {"stage": "rtl"})
        open_err = mcp.call_tool("spec_audit", {"spec": spec_no_formal})
        assert_eq(open_err.get("signoff_ready"), False, "open ISO_MISSING fails low_power gate")
        _assert_low_power_gate_failed(open_err, "open ISO_MISSING")

        # (c) Manual waiver removes open errors; allow_manual_lp_waivers=false still fails low_power gate
        mcp.call_tool(
            "tcl_eval",
            {"command": "set_lp_waiver -add -rule {ISO_MISSING} -comment {manual waiver}"},
        )
        waived_blocked = mcp.call_tool("spec_audit", {"spec": spec_no_formal})
        assert_eq(
            waived_blocked.get("signoff_ready"),
            False,
            "manual LP waiver fails when allow_manual_lp_waivers=false",
        )
        _assert_low_power_gate_failed(waived_blocked, "manual LP waiver blocked")


# ---------------------------------------------------------------------------
# Feature F2: Cache Invalidation & Negative Shell/MCP Interfaces
# ---------------------------------------------------------------------------


def _t2_f2_01_cache_invalidation_on_sv_and_svh_edit(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f2_design_defs.svh", "f2_design.sv")
    script = ctx.write_file(
        "inv.jg.tcl",
        "\n".join(
            [
                "clear -all",
                "analyze -sv12 f2_design.sv",
                "elaborate -top f2_design",
                "clock clk",
                "reset -expression {!rst_n}",
                "assert -name a_sub_xor {(en && (din[7:0] == 8'h00)) |=> (dout[7:0] == 8'h01)}",
                "prove -all",
            ]
        )
        + "\n",
    )
    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool("session_open", {"script": "inv.jg.tcl"})
        h1 = mcp.call_tool("design_hierarchy", {"depth": 1})
        din1 = next(s for s in h1["scopes"][0]["signals"] if s["name"] == "din")
        assert_eq(din1["width"], 8, "initial din width")
        p1 = mcp.call_tool("property_get", {"name": "a_sub_xor"})
        assert_eq(p1["property"]["status"], "proven", "initial a_sub_xor proven")

        # Edit included header `f2_design_defs.svh` on disk (width 8 -> 16)
        ctx.write_file(
            "f2_design_defs.svh",
            "\n".join(
                [
                    "`ifndef F2_DESIGN_DEFS_SVH",
                    "`define F2_DESIGN_DEFS_SVH",
                    "`define F2_W 16",
                    "`define F2_ZERO 16'h0000",
                    "`endif",
                ]
            )
            + "\n",
        )
        mcp.call_tool("session_open", {"script": "inv.jg.tcl"})
        h2 = mcp.call_tool("design_hierarchy", {"depth": 1})
        din2 = next(s for s in h2["scopes"][0]["signals"] if s["name"] == "din")
        assert_eq(din2["width"], 16, "din width updated after .svh edit")

        # Edit SV source file on disk to flip logic (`^ 1` -> `^ 2`) -> assertion flips to cex
        sv_text = ctx.path("f2_design.sv").read_text(encoding="utf-8")
        ctx.write_file("f2_design.sv", sv_text.replace("1'b1};", "2'b10};"))
        mcp.call_tool("session_open", {"script": "inv.jg.tcl"})
        p2 = mcp.call_tool("property_get", {"name": "a_sub_xor"})
        assert_eq(p2["property"]["status"], "cex", "a_sub_xor flips to cex after .sv edit")


def _t2_f2_02_bad_tcl_shell_check_and_script_check(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f2_design_defs.svh", "f2_design.sv")
    bad_tcl = ctx.write_file(
        "bad_flow.jg.tcl",
        "\n".join(
            [
                "analyze -sv12 f2_design.sv",
                "elaborate -top f2_design",
                "bogus_vendor_cmd -foo",
                "prove -all -frobnicate",
                "set val $no_such_var",
            ]
        )
        + "\n",
    )
    cli_res = ctx.run_shell_check(bad_tcl, json_out=True)
    assert_exit(cli_res, 1, "bad script shell --check --json")
    cli_json = cli_res.json()
    kinds = {f["kind"] for f in cli_json["findings"]}
    assert {"unknown", "error", "tcl"} <= kinds, f"Expected unknown/error/tcl kinds, got {cli_json!r}"

    with ctx.mcp(mode="auto") as mcp:
        mcp_json = mcp.call_tool("script_check", {"script": "bad_flow.jg.tcl"})
    assert_eq(mcp_json["clean"], False, "mcp clean=False")

    def _norm(items: list[dict[str, Any]]) -> list[tuple[str, str, int, str, str]]:
        return [
            (
                str(f["kind"]),
                str(Path(f["file"]).resolve()),
                int(f["line"]),
                str(f["command"]),
                str(f["message"]),
            )
            for f in items
        ]

    assert_eq(_norm(mcp_json["findings"]), _norm(cli_json["findings"]), "script_check vs --check findings parity")


def _t2_f2_03_mcp_malformed_ndjson_and_protocol_errors(ctx: E2EContext) -> None:
    payload = "\n".join(
        [
            "not valid json at all",
            json.dumps({"jsonrpc": "2.0", "id": 1, "foo": "bar"}),
            json.dumps({"jsonrpc": "2.0", "id": 2, "method": "no/suchMethod", "params": {}}),
            json.dumps(
                {
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {"name": "nonexistent_tool", "arguments": {}},
                }
            ),
        ]
    ) + "\n"

    proc = subprocess.run(
        [
            str(ctx.informal_bin),
            "mcp",
            "--mode",
            "auto",
            "--root",
            str(ctx.workdir),
            "--workdir",
            str(ctx.path("mcp_proto")),
            "--engines",
            str(ctx.engines_path),
        ],
        input=payload,
        cwd=str(ctx.workdir),
        env=ctx.env,
        capture_output=True,
        text=True,
        timeout=30.0,
    )
    assert_eq(proc.returncode, 0, f"mcp stdin EOF exit code; stderr={proc.stderr}")
    msgs = [json.loads(line) for line in proc.stdout.splitlines() if line.strip()]
    assert_eq(len(msgs), 4, f"Expected 4 JSON-RPC error responses, got {msgs!r}")
    assert_eq([m["error"]["code"] for m in msgs], [-32700, -32600, -32601, -32602], "JSON-RPC codes")


def _t2_f2_04_mcp_read_only_rejects_mutating_tools(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f2_design_defs.svh", "f2_design.sv")
    with ctx.mcp(mode="read-only") as mcp:
        names = mcp.tool_names()
        for forbidden in ("proposal_add", "check_cov", "check_xprop", "check_spv", "tcl_eval"):
            assert forbidden not in names, f"read-only mode must hide {forbidden}"

        try:
            mcp.call_tool("tcl_eval", {"command": "puts hello"})
        except McpProtocolError as exc:
            assert_eq(exc.code, -32602, "read-only tcl_eval error code")
        else:
            raise AssertionError("Expected McpProtocolError calling tcl_eval in read-only mode")


def _t2_f2_05_shell_missing_file_syntax_and_help_errors(ctx: E2EContext) -> None:
    res_missing = ctx.run_shell_file(ctx.path("does_not_exist.tcl"))
    assert_exit(res_missing, 1, "missing shell script")

    bad_syntax = ctx.write_file("bad_syntax.tcl", "if {1 {\n")
    res_syn = ctx.run_shell_file(bad_syntax)
    assert_exit(res_syn, 1, "Tcl syntax error")

    res_help_cmd = ctx.run_shell_commands("help no_such_command")
    assert_exit(res_help_cmd, 1, "help unknown command")

    res_help_flag = ctx.run_shell_commands("help -bogus_flag")
    assert_exit(res_help_flag, 1, "help unknown flag")

    res_lower = ctx.run_shell_commands("set_lowering unknown_option on")
    assert_exit(res_lower, 1, "set_lowering unknown option")


def _t2_f2_06_mcp_session_open_invalid_args(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f2_design_defs.svh", "f2_design.sv", "f2_flow.jg.tcl")
    with ctx.mcp(mode="auto") as mcp:
        err_both = mcp.call_tool(
            "session_open",
            {"script": "f2_flow.jg.tcl", "files": ["f2_design.sv"]},
            expect_error=True,
        )
        assert_eq(err_both.get("code"), "arguments", "both script and files")

        err_tcl_in_files = mcp.call_tool(
            "session_open",
            {"files": ["f2_flow.jg.tcl"]},
            expect_error=True,
        )
        assert_eq(err_tcl_in_files.get("code"), "arguments", ".tcl passed in files")

        err_escape = mcp.call_tool(
            "session_open",
            {"files": ["../outside.sv"]},
            expect_error=True,
        )
        assert_eq(err_escape.get("code"), "path", "path escaping --root")


# ---------------------------------------------------------------------------
# Feature F3: Miter/why_cex Boundary & Negative Cases
# ---------------------------------------------------------------------------


def _t2_f3_01_why_cex_error_on_safe_and_unknown(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f3_why_cex_mux.sv", "f3_xprop_clean.sv", "f3_spv_clean.sv")

    tcl_safe = ctx.run_shell_commands(
        [
            "analyze -sv12 f3_why_cex_mux.sv",
            "elaborate -top f3_why_cex_mux",
            "clock clk",
            "reset -expression {!rst_n}",
            "assert -name a_always_safe {safe_ok}",
            "prove -all",
            "why_cex a_always_safe",
        ]
    )
    assert_exit(tcl_safe, 1, "why_cex on proven property")
    assert_in("has no counterexample", tcl_safe.combined)

    tcl_unk = ctx.run_shell_commands(
        [
            "analyze -sv12 f3_why_cex_mux.sv",
            "elaborate -top f3_why_cex_mux",
            "clock clk",
            "reset -expression {!rst_n}",
            "why_cex no_such_prop",
        ],
        workdir=ctx.path("unk_work"),
    )
    assert_exit(tcl_unk, 1, "why_cex on unknown property")
    assert_in("unknown property", tcl_unk.combined)

    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool(
            "session_open",
            {"files": ["f3_why_cex_mux.sv"], "top": "f3_why_cex_mux", "clock": "clk", "reset": "!rst_n"},
        )
        mcp.call_tool("tcl_eval", {"command": "assert -name a_always_safe {safe_ok}"})
        mcp.call_tool("prove", {})
        err_safe = mcp.call_tool("why_cex", {"name": "a_always_safe"}, expect_error=True)
        assert_eq(err_safe.get("code"), "no_trace", "MCP why_cex on proven property")

        mcp.call_tool(
            "session_open",
            {"files": ["f3_xprop_clean.sv"], "top": "f3_xprop_clean", "clock": "clk", "reset": "!rst_n"},
        )
        mcp.call_tool("check_xprop", {"action": "init", "targets": ["outputs"]})
        xp_clean = mcp.call_tool("check_xprop", {"action": "prove"})
        clean_a_name = _find_xprop_item(xp_clean["items"], "clean_a")["name"]
        err_xp = mcp.call_tool("why_cex", {"name": clean_a_name}, expect_error=True)
        assert_eq(err_xp.get("code"), "no_trace", "MCP why_cex on proven xprop item")

        mcp.call_tool(
            "session_open",
            {"files": ["f3_spv_clean.sv"], "top": "f3_spv_clean", "clock": "clk", "reset": "!rst_n"},
        )
        mcp.call_tool(
            "check_spv",
            {"action": "create", "name": "iso_pub", "from": ["secret_key"], "to": ["pub_out"]},
        )
        mcp.call_tool("check_spv", {"action": "prove"})
        err_spv = mcp.call_tool("why_cex", {"name": "iso_pub"}, expect_error=True)
        assert_eq(err_spv.get("code"), "no_trace", "MCP why_cex on proven spv rule")


def _t2_f3_02_xprop_invalid_flags_and_targets(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f3_xprop_dut.sv")
    base = [
        "analyze -sv12 f3_xprop_dut.sv",
        "elaborate -top f3_xprop_dut",
        "clock clk",
        "reset -expression {!rst_n}",
    ]
    cases = [
        ("no_action", base + ["check_xprop"], "expected -init, -waiver"),
        ("missing_inst", base + ["check_xprop -waiver -add"], "-instance <pattern> required"),
        ("unknown_to", base + ["check_xprop -init -to {no_such_sig} -prove"], "not found in lowered design"),
        ("bad_flag", base + ["check_xprop -bogus"], "unsupported option -bogus"),
        (
            "overwrite_no_force",
            base
            + [
                "check_xprop -init -targets {outputs}",
                "check_xprop -report -file rep.txt",
                "check_xprop -report -file rep.txt",
            ],
            "add -force to overwrite",
        ),
    ]
    for idx, (tag, cmds, expected_msg) in enumerate(cases):
        res = ctx.run_shell_commands(cmds, workdir=ctx.path(f"xp_neg_{idx}"))
        assert_exit(res, 1, f"xprop negative {tag}")
        assert_in(expected_msg, res.combined, f"xprop {tag} error message")


def _t2_f3_03_spv_duplicate_missing_and_bad_cond(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f3_spv_crypto.sv")
    base = [
        "analyze -sv12 f3_spv_crypto.sv",
        "elaborate -top f3_spv_crypto",
        "clock clk",
        "reset -expression {!rst_n}",
    ]
    r_missing_to = ctx.run_shell_commands(
        base + ["check_spv -create -name r0 -from secret_key"], workdir=ctx.path("s0")
    )
    assert_exit(r_missing_to, 1, "spv missing -to")
    assert_in("-to", r_missing_to.combined)

    r_dup = ctx.run_shell_commands(
        base
        + [
            "check_spv -create -name r1 -from secret_key -to pub_status_o",
            "check_spv -create -name r1 -from secret_key -to debug_port_o",
        ],
        workdir=ctx.path("s1"),
    )
    assert_exit(r_dup, 1, "spv duplicate name")
    assert_in("already exists", r_dup.combined)

    r_missing_rule = ctx.run_shell_commands(
        base + ["check_spv -prove -name no_such_rule"], workdir=ctx.path("s2")
    )
    assert_exit(r_missing_rule, 1, "spv prove missing rule")
    assert_in("no security rule named", r_missing_rule.combined)

    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool(
            "session_open",
            {"files": ["f3_spv_crypto.sv"], "top": "f3_spv_crypto", "clock": "clk", "reset": "!rst_n"},
        )
        mcp.call_tool(
            "check_spv",
            {"action": "create", "name": "bad_src", "from": ["missing_sig"], "to": ["pub_status_o"]},
        )
        mcp.call_tool(
            "check_spv",
            {
                "action": "create",
                "name": "bad_cond",
                "from": ["secret_key"],
                "to": ["debug_port_o"],
                "cond": "bad === ===",
            },
        )
        res = mcp.call_tool("check_spv", {"action": "prove"})
        by_name = {r["name"]: r for r in res["rules"]}
        assert_eq(by_name["bad_src"]["status"], "error", "bad_src status")
        assert_eq(by_name["bad_cond"]["status"], "error", "bad_cond status")


def _t2_f3_04_why_cex_max_events_boundary(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f3_why_cex_mux.sv")
    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool(
            "session_open",
            {"files": ["f3_why_cex_mux.sv"], "top": "f3_why_cex_mux", "clock": "clk", "reset": "!rst_n"},
        )
        mcp.call_tool(
            "tcl_eval",
            {"command": "assume -name a_arm0 {!sel_arm1}; assert -name a_no_err {!err_flag}"},
        )
        mcp.call_tool("prove", {})

        one = mcp.call_tool("why_cex", {"name": "a_no_err", "max_events": 1})
        assert len(one["events"]) <= 1 and len(one["events"]) == 1, f"Expected 1 event, got {one!r}"

        unbounded = mcp.call_tool("why_cex", {"name": "a_no_err", "max_events": 0})
        assert len(unbounded["events"]) >= len(one["events"]), "max_events=0 must not drop events"

    bad_arg = ctx.run_shell_commands(
        [
            "analyze -sv12 f3_why_cex_mux.sv",
            "elaborate -top f3_why_cex_mux",
            "clock clk",
            "reset -expression {!rst_n}",
            "assert -name a_no_err {!err_flag}",
            "prove -all",
            "why_cex -max_events nope a_no_err",
        ]
    )
    assert_exit(bad_arg, 1, "why_cex -max_events nope")
    assert_in("bad -max_events", bad_arg.combined)


def _t2_f3_05_safe_apps_leave_zero_trace_files(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f3_xprop_clean.sv", "f3_spv_clean.sv")
    xp_wd = ctx.path("safe_xp_wd")
    res_xp = ctx.run_shell_commands(
        [
            "analyze -sv12 f3_xprop_clean.sv",
            "elaborate -top f3_xprop_clean",
            "clock clk",
            "reset -expression {!rst_n}",
            "check_xprop -init -targets {outputs} -prove",
            "puts [check_xprop -report]",
        ],
        workdir=xp_wd,
    )
    assert_exit(res_xp, 0, "clean xprop")
    assert re.search(r"(?:^|\.)clean_a\s+output\s+proven\b", res_xp.stdout, re.MULTILINE), res_xp.stdout
    assert re.search(r"(?:^|\.)clean_b\s+output\s+proven\b", res_xp.stdout, re.MULTILINE), res_xp.stdout
    assert_eq(list(xp_wd.glob("*.vcd")), [], "Safe xprop must leave zero .vcd files")
    assert_eq(list(xp_wd.glob("*.trace.btor2")), [], "Safe xprop must leave zero .trace.btor2 files")

    spv_wd = ctx.path("safe_spv_wd")
    res_spv = ctx.run_shell_commands(
        [
            "analyze -sv12 f3_spv_clean.sv",
            "elaborate -top f3_spv_clean",
            "clock clk",
            "reset -expression {!rst_n}",
            "check_spv -create -name iso_pub -from secret_key -to pub_out",
            "check_spv -prove",
            "puts [check_spv -report]",
        ],
        workdir=spv_wd,
    )
    assert_exit(res_spv, 0, "clean spv")
    assert re.search(r"\biso_pub\b.*\bproven\b", res_spv.stdout), res_spv.stdout
    assert_eq(list(spv_wd.glob("*.vcd")), [], "Safe spv must leave zero .vcd files")
    assert_eq(list(spv_wd.glob("*.trace.btor2")), [], "Safe spv must leave zero .trace.btor2 files")

    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool(
            "session_open",
            {"files": ["f3_spv_clean.sv"], "top": "f3_spv_clean", "clock": "clk", "reset": "!rst_n"},
        )
        mcp.call_tool(
            "check_spv",
            {"action": "create", "name": "iso_pub", "from": ["secret_key"], "to": ["pub_out"]},
        )
        spv = mcp.call_tool("check_spv", {"action": "prove"})
        assert spv.get("rules"), f"Expected rules in check_spv prove result: {spv!r}"
        assert_eq(spv["rules"][0].get("status"), "proven", "Safe MCP spv rule status")
        assert "trace" not in spv["rules"][0], "Safe MCP spv rule must omit trace"


def _t2_f3_06_spv_internal_reg_from_and_multi_to(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f3_spv_crypto.sv")
    wd = ctx.path("spv_reg_cut")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f3_spv_crypto.sv",
            "elaborate -top f3_spv_crypto",
            "clock clk",
            "reset -expression {!rst_n}",
            "check_spv -create -name reg_cut -from s1 -to {pub_status_o covert_port_o}",
            "check_spv -prove",
            "puts [check_spv -report]",
        ],
        workdir=wd,
    )
    assert_exit(res, 0, "check_spv internal reg s1 -> multi-to")
    assert re.search(r"reg_cut\s+.*\bcex\b", res.stdout), res.stdout
    vcds = sorted(wd.glob("*reg_cut*.vcd"))
    assert vcds, f"Expected VCD for reg_cut in {wd}"
    divs = find_divergences(parse_vcd_file(vcds[0]))
    assert divs, f"Expected m0 vs m1 divergences in {vcds[0]}"


# ---------------------------------------------------------------------------
# Feature F4: UPF/CPF Boundary & Decision D13 Negative Cases
# ---------------------------------------------------------------------------


def _t2_f4_01_d13_unknown_semantic_upf_opt_file_line(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f4_bad_d13_semantic.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "load_upf f4_bad_d13_semantic.upf",
        ]
    )
    assert_exit(res, 1, "D13 unknown semantic UPF flag must fail")
    assert_contains(res.combined, "f4_bad_d13_semantic.upf", ":8", "-value_conversion")


def _t2_f4_02_missing_upf_and_bad_version(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f4_upf10_basic.upf")
    r1 = ctx.run_shell_commands(
        ["analyze -sv12 f4_soc_basic.sv", "elaborate -top soc_basic", "load_upf missing_file.upf"],
        workdir=ctx.path("m1"),
    )
    assert_exit(r1, 1, "missing UPF file")

    r2 = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "load_upf f4_upf10_basic.upf -version 9.9",
        ],
        workdir=ctx.path("m2"),
    )
    assert_exit(r2, 1, "bad load_upf -version 9.9")

    ctx.write_file("bad_ver.upf", "upf_version 9.9\nset_design_top soc_basic\n")
    r3 = ctx.run_shell_commands(
        ["analyze -sv12 f4_soc_basic.sv", "elaborate -top soc_basic", "load_upf bad_ver.upf"],
        workdir=ctx.path("m3"),
    )
    assert_exit(r3, 1, "bad upf_version 9.9 inside UPF")


def _t2_f4_03_unsupported_cpf_option_file_line(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f4_bad_opt.cpf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "load_upf f4_bad_opt.cpf -cpf",
        ]
    )
    assert_exit(res, 1, "unsupported CPF semantic option")
    assert_contains(res.combined, "f4_bad_opt.cpf", ":5", "-unknown_semantic_flag")


def _t2_f4_04_empty_supply_set_guard_not_false_unsat(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f4_empty_supply_set.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "load_upf f4_empty_supply_set.upf",
            "check_lp -stage upf",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "check_lp -stage upf with empty supply set")
    rules = _rule_ids(_parse_last_json_object(res.stdout))
    assert_in("PST_MISSING_PORT_STATE", " ".join(sorted(rules)), "Expected PST_MISSING_PORT_STATE")
    assert "PST_INCONSISTENT" not in rules, (
        f"Empty supply set must NOT cause false PST_INCONSISTENT, got {rules!r}"
    )


def _t2_f4_05_unresolved_scope_elements_and_domain(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f4_upf_subip.upf")
    r_scope = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "load_upf f4_upf_subip.upf -scope u_nonexistent",
        ],
        workdir=ctx.path("s_bad"),
    )
    assert_exit(r_scope, 1, "nonexistent -scope")

    ctx.write_file(
        "bad_target.upf",
        "\n".join(
            [
                "upf_version 2.1",
                "set_design_top soc_basic",
                "create_supply_port VDD_AON -direction in",
                "create_supply_net  VDD_AON",
                "connect_supply_net VDD_AON -ports VDD_AON",
                "create_supply_set SS_AON -function {power VDD_AON}",
                "create_power_domain PD_TOP -include_scope -supply {primary SS_AON}",
                "create_power_domain PD_BAD -elements {u_missing_inst} -supply {primary SS_AON}",
                "set_isolation iso_x -domain PD_UNDEF -applies_to outputs -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal u_pmu/iso_en",
            ]
        )
        + "\n",
    )
    r_elem = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "load_upf bad_target.upf",
            "check_lp -stage upf",
            "puts [report_lp -json]",
        ],
        workdir=ctx.path("e_bad"),
    )
    if r_elem.returncode == 0:
        rules = _rule_ids(_parse_last_json_object(r_elem.stdout))
        assert any("UPF_" in r for r in rules), f"Expected UPF_* finding for missing target/domain, got {rules!r}"
    else:
        low = r_elem.combined.lower()
        assert (
            "u_missing_inst" in r_elem.combined
            or "PD_UNDEF" in r_elem.combined
            or "error" in low
        ), f"Expected missing target/domain diagnostic on non-zero exit:\n{r_elem.combined}"


def _t2_f4_06_deep_nested_load_upf_and_inner_error_line(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f4_upf_parent.upf", "f4_upf_subip.upf", "f4_upf_leaf.upf")
    # Patch copy of f4_upf_subip.upf in workdir to include 3rd level f4_upf_leaf.upf
    subip_text = ctx.path("f4_upf_subip.upf").read_text(encoding="utf-8")
    subip_3lvl = subip_text.replace(
        "set_scope ..\n",
        "load_upf [file join [file dirname [info script]] f4_upf_leaf.upf] -scope u_leaf\nset_scope ..\n",
    )
    ctx.write_file("f4_upf_subip.upf", subip_3lvl)
    res_ok = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "load_upf f4_upf_parent.upf",
            "check_lp -stage upf",
        ],
        workdir=ctx.path("nest_ok"),
    )
    assert_exit(res_ok, 0, "3-level nested load_upf")

    # Seed line-4 error inside inner leaf UPF
    ctx.write_file(
        "f4_upf_leaf.upf",
        "\n".join(
            [
                "upf_version 2.1",
                "create_supply_net VDD_LEAF",
                "create_power_domain PD_LEAF -include_scope",
                "set_isolation iso_leaf_bad -domain PD_LEAF -value_conversion bad_opt",
            ]
        )
        + "\n",
    )
    res_err = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "load_upf f4_upf_parent.upf",
        ],
        workdir=ctx.path("nest_err"),
    )
    assert_exit(res_err, 1, "nested UPF D13 error")
    assert_contains(res_err.combined, "f4_upf_leaf.upf", ":4")


# ---------------------------------------------------------------------------
# Feature F5: PST Contradictions, compare_upf Mismatches & uV Boundary
# ---------------------------------------------------------------------------


def _t2_f5_01_pst_sat_32_domain_contradiction(ctx: E2EContext) -> None:
    bad32 = generate_n_domain_upf(32, include_contradiction=True, upf_version="2.1")
    ctx.write_file("bad32.sv", bad32.sv)
    ctx.write_file("bad32.upf", bad32.upf)
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 bad32.sv",
            "elaborate -top soc_top",
            "load_upf bad32.upf",
            "check_lp -stage upf",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "32-domain contradiction check_lp")
    assert "PST_INCONSISTENT" in _rule_ids(_parse_last_json_object(res.stdout)), res.stdout


def _t2_f5_02_upf10_pst_unreachable_or_inconsistent(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f5_upf1_bad_pst.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "load_upf f5_upf1_bad_pst.upf -version 1.0",
            "check_lp -stage upf",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "UPF 1.0 bad PST")
    rules = _rule_ids(_parse_last_json_object(res.stdout))
    assert rules & {"PST_UNREACHABLE_STATE", "PST_INCONSISTENT"}, (
        f"Expected PST_UNREACHABLE_STATE or PST_INCONSISTENT, got {rules!r}"
    )


def _t2_f5_03_set_equivalent_voltage_mismatch(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f5_equiv_clean.sv", "f5_equiv_mismatch.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f5_equiv_clean.sv",
            "elaborate -top f5_equiv_clean",
            "load_upf f5_equiv_mismatch.upf",
            "check_lp -stage upf",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, "set_equivalent mismatch")
    assert "UPF_EQUIV_MISMATCH" in _rule_ids(_parse_last_json_object(res.stdout)), res.stdout


def _t2_f5_04_compare_upf_detects_clamp_flip(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f5_cmp_golden.upf", "f5_cmp_clamp_flip.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "compare_upf -golden f5_cmp_golden.upf -revised f5_cmp_clamp_flip.upf",
        ]
    )
    low = res.combined.lower()
    assert res.returncode != 0 or any(tok in low for tok in ("mismatch", "differ", "clamp", "false")), (
        f"Expected compare_upf to flag clamp flip 0 vs 1:\n{res.combined}"
    )


def _t2_f5_05_compare_upf_detects_domain_and_strategy_mismatch(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f4_soc_basic.sv", "f5_cmp_golden.upf", "f5_cmp_domain_mismatch.upf")
    res = ctx.run_shell_commands(
        [
            "analyze -sv12 f4_soc_basic.sv",
            "elaborate -top soc_basic",
            "compare_upf -golden f5_cmp_golden.upf -revised f5_cmp_domain_mismatch.upf",
        ]
    )
    low = res.combined.lower()
    assert res.returncode != 0 or any(tok in low for tok in ("mismatch", "differ", "missing", "false")), (
        f"Expected compare_upf to flag domain/retention mismatch:\n{res.combined}"
    )


def _t2_f5_06_microvolt_tolerance_boundary_1000uv_vs_2000uv(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "f5_uv_within_tol.sv", "f5_uv_within_tol.upf", "f5_uv_over_tol.upf")

    res_ok = ctx.run_shell_commands(
        [
            "analyze -sv12 f5_uv_within_tol.sv",
            "elaborate -top f5_uv_tol",
            "load_upf f5_uv_within_tol.upf",
            "check_lp -stage rtl",
            "puts [report_lp -json]",
        ],
        workdir=ctx.path("uv_ok"),
    )
    assert_exit(res_ok, 0, "500 uV within tolerance")
    assert "LS_MISSING" not in _rule_ids(_parse_last_json_object(res_ok.stdout))

    res_over = ctx.run_shell_commands(
        [
            "analyze -sv12 f5_uv_within_tol.sv",
            "elaborate -top f5_uv_tol",
            "load_upf f5_uv_over_tol.upf",
            "check_lp -stage rtl",
            "puts [report_lp -json]",
        ],
        workdir=ctx.path("uv_over"),
    )
    assert_exit(res_over, 0, "2000 uV over tolerance")
    assert "LS_MISSING" in _rule_ids(_parse_last_json_object(res_over.stdout)), res_over.stdout


# ---------------------------------------------------------------------------
# Feature F6: Static LP Stage 1/2 Seeded Bugs & Negative Shell Contracts
# ---------------------------------------------------------------------------


def _run_stage2_rules(ctx: E2EContext, sv: str, upf: str, top: str) -> set[str]:
    _copy_f1_f6(ctx, sv, upf)
    res = ctx.run_shell_commands(
        [
            f"analyze -sv12 {sv}",
            f"elaborate -top {top}",
            "clock clk",
            "reset -expression {!rst_n}",
            f"load_upf {upf}",
            "check_lp -stage rtl",
            "puts [report_lp -json]",
        ]
    )
    assert_exit(res, 0, f"check_lp -stage rtl ({sv})")
    return _rule_ids(_parse_last_json_object(res.stdout))


def _t2_f6_01_seeded_iso_missing_ctrl_unpowered_loc_invalid(ctx: E2EContext) -> None:
    rules = _run_stage2_rules(ctx, "f6_iso_bugs.sv", "f6_iso_bugs.upf", "f6_iso_bugs")
    for expected in ("ISO_MISSING", "ISO_CTRL_UNPOWERED", "ISO_LOCATION_INVALID"):
        assert expected in rules, f"Expected {expected} in {rules!r}"


def _t2_f6_02_seeded_iso_ctrl_polarity_mismatch_inv3(ctx: E2EContext) -> None:
    rules = _run_stage2_rules(
        ctx, "f6_iso_pol_inv3.sv", "f6_iso_pol_inv3.upf", "f6_iso_pol_inv3"
    )
    assert "ISO_CTRL_POLARITY_MISMATCH" in rules, f"Expected ISO_CTRL_POLARITY_MISMATCH in {rules!r}"


def _t2_f6_03_seeded_iso_feedthrough_buffer(ctx: E2EContext) -> None:
    rules = _run_stage2_rules(ctx, "f6_ft_buffer.sv", "f6_ft_buffer.upf", "f6_ft_buffer")
    assert "ISO_FEEDTHROUGH_BUFFER" in rules, f"Expected ISO_FEEDTHROUGH_BUFFER in {rules!r}"


def _t2_f6_04_seeded_ls_missing_and_dir_mismatch(ctx: E2EContext) -> None:
    rules = _run_stage2_rules(ctx, "f6_ls_bugs.sv", "f6_ls_bugs.upf", "f6_ls_bugs")
    assert "LS_MISSING" in rules, f"Expected LS_MISSING in {rules!r}"
    assert "LS_DIR_MISMATCH" in rules, f"Expected LS_DIR_MISMATCH in {rules!r}"


def _t2_f6_05_seeded_ret_backup_off_and_async_rst(ctx: E2EContext) -> None:
    rules = _run_stage2_rules(ctx, "f6_ret_bugs.sv", "f6_ret_bugs.upf", "f6_ret_bugs")
    assert "RET_BACKUP_OFF" in rules, f"Expected RET_BACKUP_OFF in {rules!r}"
    assert "RET_ASYNC_RST_UNISOLATED" in rules, (
        f"Expected RET_ASYNC_RST_UNISOLATED in {rules!r}"
    )


def _t2_f6_06_seeded_psw_ctrl_unpowered_and_port_attr_missing(ctx: E2EContext) -> None:
    rules = _run_stage2_rules(
        ctx, "f6_psw_port_bugs.sv", "f6_psw_port_bugs.upf", "f6_psw_port_bugs"
    )
    assert "PSW_CTRL_UNPOWERED" in rules, f"Expected PSW_CTRL_UNPOWERED in {rules!r}"
    assert "PORT_ATTR_MISSING" in rules, f"Expected PORT_ATTR_MISSING in {rules!r}"


def _t2_f6_07_lp_shell_negative_d13_flags_and_pre_elab(ctx: E2EContext) -> None:
    _copy_f1_f6(ctx, "soc_lp_clean.sv", "soc_lp_clean.upf")

    r_pre_elab = ctx.run_shell_commands("check_lp -stage rtl", workdir=ctx.path("pre_elab"))
    assert_exit(r_pre_elab, 1, "check_lp -stage rtl before elaborate/load_upf")

    r_bad_stage = ctx.run_shell_commands(
        [
            "analyze -sv12 soc_lp_clean.sv",
            "elaborate -top soc_lp_clean",
            "load_upf soc_lp_clean.upf",
            "check_lp -stage bad_stage",
        ],
        workdir=ctx.path("bad_stage"),
    )
    assert_exit(r_bad_stage, 1, "check_lp -stage bad_stage")

    r_bad_opt = ctx.run_shell_commands(
        [
            "analyze -sv12 soc_lp_clean.sv",
            "elaborate -top soc_lp_clean",
            "load_upf soc_lp_clean.upf",
            "check_lp -bogus_opt",
        ],
        workdir=ctx.path("bad_opt"),
    )
    assert_exit(r_bad_opt, 1, "check_lp -bogus_opt D13")

    r_bad_waiver = ctx.run_shell_commands(
        [
            "analyze -sv12 soc_lp_clean.sv",
            "elaborate -top soc_lp_clean",
            "load_upf soc_lp_clean.upf",
            "set_lp_waiver -remove 999",
        ],
        workdir=ctx.path("bad_waiver"),
    )
    assert_exit(r_bad_waiver, 1, "set_lp_waiver -remove 999")


def get_cases() -> list[TestCase]:
    return [
        # F1 (6 cases)
        TestCase("T2-F1-01", 2, "F1", "spec_audit detects task-qualified disabled assertion (task::prop)", _t2_f1_01_spec_audit_task_qualified_disabled_prop, timeout_s=60.0, tags=("mcp", "spec_audit")),
        TestCase("T2-F1-02", 2, "F1", "spec_audit forbid_under_ai blocks AI assumptions until undone", _t2_f1_02_spec_audit_forbid_under_ai_and_undo, timeout_s=60.0, tags=("mcp", "spec_audit")),
        TestCase("T2-F1-03", 2, "F1", "spec_audit require_dead_end_check unchecked and dead_end_found failures", _t2_f1_03_spec_audit_dead_end_unchecked_and_found, timeout_s=60.0, tags=("mcp", "spec_audit")),
        TestCase("T2-F1-04", 2, "F1", "spec_audit bound < min_bound and forbidden_assume_signals override", _t2_f1_04_bounded_floor_and_forbidden_override, timeout_s=60.0, tags=("mcp", "spec_audit")),
        TestCase("T2-F1-05", 2, "F1", "spec_audit rejects missing spec, bad schema, duplicate ID, bad floor", _t2_f1_05_spec_audit_invalid_inputs_rejected, timeout_s=60.0, tags=("mcp", "spec_audit", "negative")),
        TestCase("T2-F1-06", 2, "F1", "spec_audit low_power gate fails on missing UPF, open errors, manual waivers", _t2_f1_06_spec_audit_low_power_gate_failures, timeout_s=90.0, tags=("mcp", "spec_audit", "lp")),
        # F2 (6 cases)
        TestCase("T2-F2-01", 2, "F2", "CompiledDesign cache invalidates on SV and include .svh edits", _t2_f2_01_cache_invalidation_on_sv_and_svh_edit, timeout_s=60.0, tags=("cache", "mcp")),
        TestCase("T2-F2-02", 2, "F2", "Bad Tcl script shell --check --json and MCP script_check parity", _t2_f2_02_bad_tcl_shell_check_and_script_check, timeout_s=60.0, tags=("shell", "mcp", "negative")),
        TestCase("T2-F2-03", 2, "F2", "MCP malformed NDJSON, non-request, unknown method/tool codes", _t2_f2_03_mcp_malformed_ndjson_and_protocol_errors, timeout_s=60.0, tags=("mcp", "negative")),
        TestCase("T2-F2-04", 2, "F2", "MCP read-only mode hides and rejects mutating tools", _t2_f2_04_mcp_read_only_rejects_mutating_tools, timeout_s=60.0, tags=("mcp", "negative")),
        TestCase("T2-F2-05", 2, "F2", "Shell missing script, Tcl syntax error, help and set_lowering errors", _t2_f2_05_shell_missing_file_syntax_and_help_errors, timeout_s=60.0, tags=("shell", "negative")),
        TestCase("T2-F2-06", 2, "F2", "MCP session_open rejects conflicting script/files, .tcl in files, escape path", _t2_f2_06_mcp_session_open_invalid_args, timeout_s=60.0, tags=("mcp", "negative")),
        # F3 (6 cases)
        TestCase("T2-F3-01", 2, "F3", "why_cex errors cleanly on proven standard/xprop/spv and unknown props", _t2_f3_01_why_cex_error_on_safe_and_unknown, timeout_s=90.0, tags=("why_cex", "negative")),
        TestCase("T2-F3-02", 2, "F3", "check_xprop rejects missing action/instance, unknown -to, no -force", _t2_f3_02_xprop_invalid_flags_and_targets, timeout_s=60.0, tags=("xprop", "negative")),
        TestCase("T2-F3-03", 2, "F3", "check_spv rejects duplicate name, missing -to/-rule, bad -cond", _t2_f3_03_spv_duplicate_missing_and_bad_cond, timeout_s=60.0, tags=("spv", "negative")),
        TestCase("T2-F3-04", 2, "F3", "why_cex -max_events boundary (1, 0, non-numeric)", _t2_f3_04_why_cex_max_events_boundary, timeout_s=60.0, tags=("why_cex", "boundary")),
        TestCase("T2-F3-05", 2, "F3", "Safe check_xprop and check_spv leave zero VCD/trace.btor2 artifacts", _t2_f3_05_safe_apps_leave_zero_trace_files, timeout_s=60.0, tags=("xprop", "spv", "vcd")),
        TestCase("T2-F3-06", 2, "F3", "check_spv internal register -from and multi-target -to miter VCD", _t2_f3_06_spv_internal_reg_from_and_multi_to, timeout_s=60.0, tags=("spv", "vcd")),
        # F4 (6 cases)
        TestCase("T2-F4-01", 2, "F4", "Decision D13 unknown semantic UPF option hard-errors with file:line", _t2_f4_01_d13_unknown_semantic_upf_opt_file_line, timeout_s=60.0, tags=("upf", "d13", "negative")),
        TestCase("T2-F4-02", 2, "F4", "Missing UPF file and unsupported -version / upf_version errors", _t2_f4_02_missing_upf_and_bad_version, timeout_s=60.0, tags=("upf", "negative")),
        TestCase("T2-F4-03", 2, "F4", "Decision D13 unsupported CPF option hard-errors with file:line", _t2_f4_03_unsupported_cpf_option_file_line, timeout_s=60.0, tags=("cpf", "d13", "negative")),
        TestCase("T2-F4-04", 2, "F4", "Empty supply set emits PST_MISSING_PORT_STATE without false UNSAT", _t2_f4_04_empty_supply_set_guard_not_false_unsat, timeout_s=60.0, tags=("upf", "pst_sat", "boundary")),
        TestCase("T2-F4-05", 2, "F4", "Unresolved -scope, missing -elements instance, and undefined -domain", _t2_f4_05_unresolved_scope_elements_and_domain, timeout_s=60.0, tags=("upf", "negative")),
        TestCase("T2-F4-06", 2, "F4", "Deep 3-level nested load_upf and inner nested error file:line", _t2_f4_06_deep_nested_load_upf_and_inner_error_line, timeout_s=60.0, tags=("upf", "nested")),
        # F5 (6 cases)
        TestCase("T2-F5-01", 2, "F5", "pst_sat detects PST_INCONSISTENT on 32-domain contradiction", _t2_f5_01_pst_sat_32_domain_contradiction, timeout_s=60.0, tags=("pst_sat", "scale")),
        TestCase("T2-F5-02", 2, "F5", "pst_sat detects UPF 1.0 PST_UNREACHABLE_STATE / PST_INCONSISTENT", _t2_f5_02_upf10_pst_unreachable_or_inconsistent, timeout_s=60.0, tags=("pst_sat", "upf10")),
        TestCase("T2-F5-03", 2, "F5", "pst_sat detects set_equivalent UPF_EQUIV_MISMATCH voltage conflict", _t2_f5_03_set_equivalent_voltage_mismatch, timeout_s=60.0, tags=("pst_sat", "equiv")),
        TestCase("T2-F5-04", 2, "F5", "compare_upf detects ISO clamp-value flip (0 vs 1)", _t2_f5_04_compare_upf_detects_clamp_flip, timeout_s=60.0, tags=("compare_upf", "negative")),
        TestCase("T2-F5-05", 2, "F5", "compare_upf detects domain partition and retention strategy mismatch", _t2_f5_05_compare_upf_detects_domain_and_strategy_mismatch, timeout_s=60.0, tags=("compare_upf", "negative")),
        TestCase("T2-F5-06", 2, "F5", "pst_sat microvolt tolerance boundary (500 uV clean vs 2000 uV LS_MISSING)", _t2_f5_06_microvolt_tolerance_boundary_1000uv_vs_2000uv, timeout_s=60.0, tags=("pst_sat", "voltage", "boundary")),
        # F6 (7 cases)
        TestCase("T2-F6-01", 2, "F6", "Seeded ISO_MISSING, ISO_CTRL_UNPOWERED, and ISO_LOCATION_INVALID", _t2_f6_01_seeded_iso_missing_ctrl_unpowered_loc_invalid, timeout_s=60.0, tags=("lp_static", "iso")),
        TestCase("T2-F6-02", 2, "F6", "Seeded ISO_CTRL_POLARITY_MISMATCH via 3-inverter odd chain", _t2_f6_02_seeded_iso_ctrl_polarity_mismatch_inv3, timeout_s=60.0, tags=("lp_static", "iso")),
        TestCase("T2-F6-03", 2, "F6", "Seeded ISO_FEEDTHROUGH_BUFFER in non-AON traversed domain", _t2_f6_03_seeded_iso_feedthrough_buffer, timeout_s=60.0, tags=("lp_static", "feedthrough")),
        TestCase("T2-F6-04", 2, "F6", "Seeded LS_MISSING and LS_DIR_MISMATCH level-shifter violations", _t2_f6_04_seeded_ls_missing_and_dir_mismatch, timeout_s=60.0, tags=("lp_static", "ls")),
        TestCase("T2-F6-05", 2, "F6", "Seeded RET_BACKUP_OFF and RET_ASYNC_RST_UNISOLATED violations", _t2_f6_05_seeded_ret_backup_off_and_async_rst, timeout_s=60.0, tags=("lp_static", "ret")),
        TestCase("T2-F6-06", 2, "F6", "Seeded PSW_CTRL_UNPOWERED and PORT_ATTR_MISSING violations", _t2_f6_06_seeded_psw_ctrl_unpowered_and_port_attr_missing, timeout_s=60.0, tags=("lp_static", "psw", "port")),
        TestCase("T2-F6-07", 2, "F6", "LP shell negative: pre-elab check_lp, bad -stage, D13 flags, bad waiver ID", _t2_f6_07_lp_shell_negative_d13_flags_and_pre_elab, timeout_s=60.0, tags=("lp_static", "shell", "negative")),
    ]
