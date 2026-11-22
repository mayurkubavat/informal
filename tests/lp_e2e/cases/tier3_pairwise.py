"""Tier 3 pairwise cross-feature E2E test cases (`T3-01` .. `T3-14`).

Opaque-box tests exercising pairwise feature combinations across `F1..F10`
via `informal shell`, `informal mcp`, `informal docgen`, `vcd_check`, and `ui/`
checks. Completely self-contained (stdlib + `lib.*` only).
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
    assert_contains,
    assert_eq,
    assert_exit,
    assert_in,
    assert_json_schema_field,
)
from lib.vcd_check import (
    find_divergences,
    has_lp_domain_signals,
    parse_vcd_file,
    signal_changes_after,
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
# T3-01: F2 x F6 — CompiledDesign cache hit + `include/SV/UPF invalidation
# ---------------------------------------------------------------------------


def _case_t3_01(ctx: E2EContext) -> None:
    ctx.write_file("defs.svh", "// ADD_UNISO_PORT disabled\n")
    ctx.write_file(
        "soc_cache.sv",
        """`include "defs.svh"
module sw_blk (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] sw_bus
`ifdef ADD_UNISO_PORT
    , output logic [7:0] extra_bus
`endif
);
    logic [7:0] q_reg;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            q_reg <= 8'h00;
        end else begin
            q_reg <= din + 8'h01;
        end
    end
    assign sw_bus = q_reg;
`ifdef ADD_UNISO_PORT
    assign extra_bus = q_reg ^ 8'hFF;
`endif
endmodule

module soc_cache (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       VDD_AON,
    input  logic       VSS,
    input  logic       pwr_en,
    output logic       pwr_ack,
    input  logic       iso_en,
    input  logic [7:0] din,
    output logic [7:0] dout,
    output logic [7:0] aux_o
);
    logic [7:0] sw_bus_w;
`ifdef ADD_UNISO_PORT
    logic [7:0] extra_bus_w;
`endif
    assign pwr_ack = pwr_en;

    sw_blk u_sw (
        .clk    (clk),
        .rst_n  (rst_n),
        .din    (din),
        .sw_bus (sw_bus_w)
`ifdef ADD_UNISO_PORT
        , .extra_bus (extra_bus_w)
`endif
    );

    assign dout = sw_bus_w;
`ifdef ADD_UNISO_PORT
    assign aux_o = extra_bus_w;
`else
    assign aux_o = 8'h00;
`endif
endmodule
""",
    )
    upf_base = """upf_version 2.1
set_design_top soc_cache
create_supply_port VDD_AON -direction in
create_supply_port VSS     -direction in
create_supply_net  VDD_AON
create_supply_net  VDD_SW
create_supply_net  VSS
connect_supply_net VDD_AON -ports VDD_AON
connect_supply_net VSS     -ports VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
create_power_domain PD_SW  -elements {u_sw} -supply {primary SS_SW}
create_power_switch PSW_SW -domain PD_SW \\
    -input_supply_port {in VDD_AON} -output_supply_port {out VDD_SW} \\
    -control_port {ctrl pwr_en} -ack_port {ack pwr_ack} \\
    -on_state {ON_ST in {ctrl}} -off_state {OFF_ST {!ctrl}}
add_power_state SS_AON -state {AON_ON -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW \\
    -state {SW_ON  -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -logic_expr {pwr_en}  -simstate NORMAL} \\
    -state {SW_OFF -supply_expr {power == OFF            && ground == {FULL_ON 0.0}} -logic_expr {!pwr_en} -simstate CORRUPT}
"""
    ctx.write_file("soc_v1.upf", upf_base)
    ctx.write_file(
        "soc_v2.upf",
        upf_base
        + "set_isolation iso_sw -domain PD_SW -applies_to outputs -elements {u_sw/sw_bus} "
        + "-clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent\n",
    )

    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool(
            "session_open",
            {
                "files": ["soc_cache.sv"],
                "top": "soc_cache",
                "clock": "clk",
                "reset": "!rst_n",
            },
        )
        u1 = mcp.call_tool("load_upf", {"file": "soc_v1.upf"})
        assert_json_schema_field(u1, "informal.upf/1")

        r1 = mcp.call_tool("check_lp", {"stage": "rtl"})
        assert_json_schema_field(r1, "informal.lp/1")
        v1 = _extract_violations(r1)
        assert _find_violations(v1, rule_prefix="ISO_MISSING", status="open", needle="sw_bus"), (
            f"Expected open ISO_MISSING on sw_bus in r1, got: {v1!r}"
        )

        # Cache hit on unchanged SV + UPF
        r1_repeat = mcp.call_tool("check_lp", {"stage": "rtl"})
        assert_json_schema_field(r1_repeat, "informal.lp/1")
        assert_eq(_extract_violations(r1_repeat), v1, "Cached check_lp mismatch")

        # Load clean UPF isolating sw_bus while extra_bus is absent
        mcp.call_tool("load_upf", {"file": "soc_v2.upf"})
        r2 = mcp.call_tool("check_lp", {"stage": "rtl"})
        assert_json_schema_field(r2, "informal.lp/1")
        v2 = _extract_violations(r2)
        assert not _find_violations(v2, rule_prefix="ISO_MISSING", status="open", needle="sw_bus"), (
            f"Expected sw_bus ISO_MISSING cleared in r2, got: {v2!r}"
        )
        assert not _find_violations(v2, rule_prefix="ISO_MISSING", status="open", needle="extra_bus"), (
            f"Expected extra_bus absent in r2 before defs.svh mutation, got: {v2!r}"
        )

        # Mutate ONLY `include header defs.svh on disk so extra_bus becomes observable
        ctx.write_file("defs.svh", "`define ADD_UNISO_PORT 1\n")
        mcp.call_tool(
            "tcl_eval",
            {"command": "analyze -clear; analyze -sv12 soc_cache.sv; elaborate -top soc_cache"},
        )
        mcp.call_tool("load_upf", {"file": "soc_v2.upf"})
        r3 = mcp.call_tool("check_lp", {"stage": "rtl"})
        assert_json_schema_field(r3, "informal.lp/1")
        v3 = _extract_violations(r3)
        assert _find_violations(v3, rule_prefix="ISO_MISSING", status="open", needle="extra_bus"), (
            f"Expected `include mutation to invalidate cache and flag extra_bus ISO_MISSING, got: {v3!r}"
        )


# ---------------------------------------------------------------------------
# T3-02: F4 x F5 — UPF 2.1 vs Cadence CPF 2.0 equivalence & pst_sat matrix
# ---------------------------------------------------------------------------


def _case_t3_02(ctx: E2EContext) -> None:
    sv = ctx.copy_fixture("workloads/t3_equiv_soc/t3_equiv_soc.sv")
    upf_clean = ctx.copy_fixture("workloads/t3_equiv_soc/t3_equiv_soc.upf")
    cpf_clean = ctx.copy_fixture("workloads/t3_equiv_soc/t3_equiv_soc.cpf")
    upf_uniso = ctx.copy_fixture("workloads/t3_equiv_soc/t3_equiv_uniso.upf")
    cpf_uniso = ctx.copy_fixture("workloads/t3_equiv_soc/t3_equiv_uniso.cpf")

    script = ctx.write_file(
        "run_t3_02.tcl",
        f"""
proc write_rep {{path}} {{
    set fh [open $path w]
    puts $fh [report_lp -json]
    close $fh
}}
analyze -sv12 {sv.name}
elaborate -top soc_equiv_top
clock clk
reset -expression {{!rst_n}}

load_upf {upf_clean.name}
check_lp -stage rtl
write_rep upf_clean.json

load_upf {cpf_clean.name} -cpf
check_lp -stage rtl
write_rep cpf_clean.json

load_upf {upf_uniso.name}
check_lp -stage rtl
write_rep upf_uniso.json

load_upf {cpf_uniso.name} -cpf
check_lp -stage rtl
write_rep cpf_uniso.json

compare_upf -golden {upf_clean.name} -revised {cpf_clean.name}
""",
    )
    res = ctx.run_shell_file(script)
    assert_exit(res, 0, "T3-02 clean & compare_upf equivalent failed")

    upf_clean_rep = _read_json(ctx.path("upf_clean.json"))
    cpf_clean_rep = _read_json(ctx.path("cpf_clean.json"))
    assert not _find_violations(_extract_violations(upf_clean_rep), status="open"), (
        f"Expected 0 open violations in UPF clean report: {upf_clean_rep!r}"
    )
    assert not _find_violations(_extract_violations(cpf_clean_rep), status="open"), (
        f"Expected 0 open violations in CPF clean report: {cpf_clean_rep!r}"
    )

    upf_uniso_v = _extract_violations(_read_json(ctx.path("upf_uniso.json")))
    cpf_uniso_v = _extract_violations(_read_json(ctx.path("cpf_uniso.json")))
    upf_rules = {(_rule_of(v), str(v.get("src_domain", "")), str(v.get("dst_domain", ""))) for v in upf_uniso_v}
    cpf_rules = {(_rule_of(v), str(v.get("src_domain", "")), str(v.get("dst_domain", ""))) for v in cpf_uniso_v}
    assert any(r[0].startswith("ISO_MISSING") for r in upf_rules), f"Expected ISO_MISSING in UPF uniso: {upf_uniso_v!r}"
    assert any(r[0].startswith("LS_MISSING") for r in upf_rules), f"Expected LS_MISSING in UPF uniso: {upf_uniso_v!r}"
    assert_eq(cpf_rules, upf_rules, "CPF vs UPF pst_sat crossing violation matrix mismatch")

    # Negative compare_upf comparisons must detect divergence
    for bad_target in (upf_uniso.name, cpf_uniso.name):
        r_bad = ctx.run_shell_commands(
            [f"compare_upf -golden {upf_clean.name} -revised {bad_target}"],
        )
        assert _is_compare_mismatch(r_bad.combined, r_bad.returncode), (
            f"Expected compare_upf mismatch for {bad_target}, got rc={r_bad.returncode}: {r_bad.combined}"
        )


# ---------------------------------------------------------------------------
# T3-03: F5 x F6 — 32-domain symbolic PST SAT + Stage 2 grouped crossing
# ---------------------------------------------------------------------------


def _case_t3_03(ctx: E2EContext) -> None:
    clean_32 = generate_n_domain_upf(32, include_contradiction=False, upf_version="2.1")
    bad_32 = generate_n_domain_upf(32, include_contradiction=True, upf_version="2.1")
    ctx.write_file("soc_32.sv", clean_32.sv)
    ctx.write_file("soc_32.upf", clean_32.upf)
    ctx.write_file("soc_32_bad_pst.upf", bad_32.upf)

    uniso_upf = "\n".join(
        line for line in clean_32.upf.splitlines() if "set_isolation iso_d15 " not in line
    ) + "\n"
    ctx.write_file("soc_32_uniso.upf", uniso_upf)

    script = ctx.write_file(
        "run_t3_03.tcl",
        """
proc write_rep {path} {
    set fh [open $path w]
    puts $fh [report_lp -json]
    close $fh
}
analyze -sv12 soc_32.sv
elaborate -top soc_top
clock clk
reset -expression {!rst_n}

set t0 [clock milliseconds]
load_upf soc_32.upf
check_lp -stage rtl
set dt [expr {[clock milliseconds] - $t0}]
puts "LP_32_MS=$dt"
write_rep rep_32_clean.json

load_upf soc_32_uniso.upf
check_lp -stage rtl
write_rep rep_32_uniso.json

load_upf soc_32_bad_pst.upf
check_lp -stage rtl
write_rep rep_32_bad.json
""",
    )
    res = ctx.run_shell_file(script)
    assert_exit(res, 0, "T3-03 32-domain shell run failed")

    m = re.search(r"LP_32_MS=(\d+)", res.combined)
    assert m is not None, f"Missing LP_32_MS timing line in output:\n{res.combined}"
    elapsed_ms = int(m.group(1))
    assert elapsed_ms < 15000, f"32-domain symbolic PST + RTL check took {elapsed_ms} ms (>= 15000 ms)"

    v_clean = _extract_violations(_read_json(ctx.path("rep_32_clean.json")))
    assert not _find_violations(v_clean, status="open"), f"Expected 0 open errors in clean 32-domain SoC: {v_clean!r}"

    v_uniso = _extract_violations(_read_json(ctx.path("rep_32_uniso.json")))
    iso_d15 = _find_violations(v_uniso, rule_prefix="ISO_MISSING", status="open")
    assert len(iso_d15) == 1, f"Expected 1 grouped 8-bit ISO_MISSING violation for PD_D15, got {len(iso_d15)}: {iso_d15!r}"
    assert "PD_D15" in json.dumps(iso_d15[0]), f"Expected PD_D15 in violation: {iso_d15[0]!r}"

    v_bad = _extract_violations(_read_json(ctx.path("rep_32_bad.json")))
    pst_errs = _find_violations(v_bad, rule_prefix="PST_")
    assert pst_errs, f"Expected PST_INCONSISTENT / PST_UNREACHABLE_STATE in 32-domain contradictory UPF: {v_bad!r}"


# ---------------------------------------------------------------------------
# T3-04: F3 x F8 — 2-copy corruption proof + m0/m1 VCD + why_cex causal slice
# ---------------------------------------------------------------------------


def _case_t3_04(ctx: E2EContext) -> None:
    ctx.write_file(
        "soc_corrupt_leak.sv",
        """module sw_unit (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] safe_bus,
    output logic [7:0] leak_bus
);
    logic [7:0] q_data;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            q_data <= 8'h00;
        end else begin
            q_data <= din ^ 8'h5A;
        end
    end
    assign safe_bus = q_data;
    assign leak_bus = q_data + 8'h01;
endmodule

module soc_corrupt_leak (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       VDD_AON,
    input  logic       VSS,
    input  logic       pwr_en,
    output logic       pwr_ack,
    input  logic       iso_en,
    input  logic [7:0] din,
    input  logic [7:0] uncorr_din,
    output logic [7:0] safe_out,
    output logic [7:0] leak_out,
    output logic [7:0] aux_uncorr_out
);
    logic [7:0] safe_bus_w;
    logic [7:0] leak_bus_w;
    logic [7:0] uncorr_q;

    assign pwr_ack = pwr_en;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            uncorr_q <= 8'h00;
        end else begin
            uncorr_q <= uncorr_din;
        end
    end
    assign aux_uncorr_out = uncorr_q;

    sw_unit u_sw (
        .clk      (clk),
        .rst_n    (rst_n),
        .din      (din),
        .safe_bus (safe_bus_w),
        .leak_bus (leak_bus_w)
    );

    assign safe_out = iso_en ? 8'h00 : safe_bus_w;
    assign leak_out = leak_bus_w;
endmodule
""",
    )
    ctx.write_file(
        "soc_corrupt_leak.upf",
        """upf_version 2.1
set_design_top soc_corrupt_leak
create_supply_port VDD_AON -direction in
create_supply_port VSS     -direction in
create_supply_net  VDD_AON
create_supply_net  VDD_SW
create_supply_net  VSS
connect_supply_net VDD_AON -ports VDD_AON
connect_supply_net VSS     -ports VSS
create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}
create_supply_set SS_SW  -function {power VDD_SW}  -function {ground VSS}
create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
create_power_domain PD_SW  -elements {u_sw} -supply {primary SS_SW}
create_power_switch PSW_SW -domain PD_SW \\
    -input_supply_port {in VDD_AON} -output_supply_port {out VDD_SW} \\
    -control_port {ctrl pwr_en} -ack_port {ack pwr_ack} \\
    -on_state {ON_ST in {ctrl}} -off_state {OFF_ST {!ctrl}}
add_power_state SS_AON -state {AON_ON -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_SW \\
    -state {SW_ON  -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -logic_expr {pwr_en}  -simstate NORMAL} \\
    -state {SW_OFF -supply_expr {power == OFF            && ground == {FULL_ON 0.0}} -logic_expr {!pwr_en} -simstate CORRUPT}
set_isolation iso_safe -domain PD_SW -applies_to outputs -elements {u_sw/safe_bus} \\
    -clamp_value 0 -isolation_supply_set SS_AON -isolation_signal iso_en -isolation_sense high -location parent
""",
    )

    script = ctx.write_file(
        "run_t3_04.tcl",
        """
analyze -sv12 soc_corrupt_leak.sv
elaborate -top soc_corrupt_leak
clock clk
reset -expression {!rst_n}
load_upf soc_corrupt_leak.upf
check_lp -stage formal -prove_corruption
set fh [open lp_corrupt.json w]
puts $fh [report_lp -json]
close $fh
set fh2 [open lp_stats.json w]
puts $fh2 [get_stats -task LowPower]
close $fh2
""",
    )
    res = ctx.run_shell_file(script, timeout_s=90.0)
    assert_exit(res, 0, "T3-04 shell run failed")

    rep = _read_json(ctx.path("lp_corrupt.json"))
    violations = _extract_violations(rep)
    leak_cex = _find_violations(violations, status="formal_cex", needle="leak")
    if not leak_cex:
        leak_cex = _find_violations(violations, status="cex", needle="leak")
    assert leak_cex, f"Expected formal_cex/cex violation for leak_out/leak_bus, got: {violations!r}"

    trace_raw = _trace_of(leak_cex[0])
    assert trace_raw, f"Expected trace VCD path on CEX violation: {leak_cex[0]!r}"
    vcd_path = _resolve_workdir_path(ctx, trace_raw)
    vcd = parse_vcd_file(vcd_path)
    assert signals_diverge(vcd, after_time=0), f"Expected m0/m1 divergence in {vcd_path}"
    divs = find_divergences(vcd, after_time=0)
    assert any("leak" in label.lower() for label, _t, _a, _b in divs), (
        f"Expected leak signal divergence in VCD, got {divs!r}"
    )

    # Locate failing property name and run why_cex -json
    stats = _read_json(ctx.path("lp_stats.json"))
    cex_props = [r["name"] for r in stats if isinstance(r, dict) and r.get("status") == "cex"]
    prop_name = next((p for p in cex_props if "leak" in p.lower()), None) or (
        cex_props[0] if cex_props else str(leak_cex[0].get("property", ""))
    )
    assert prop_name, f"Could not identify failing corruption property in {stats!r} or {leak_cex[0]!r}"

    why_res = ctx.run_shell_commands(
        [
            "analyze -sv12 soc_corrupt_leak.sv",
            "elaborate -top soc_corrupt_leak",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf soc_corrupt_leak.upf",
            "check_lp -stage formal -prove_corruption",
            f"set fh [open why_corrupt.json w]; puts $fh [why_cex -json {prop_name}]; close $fh",
        ],
        timeout_s=90.0,
    )
    assert_exit(why_res, 0, "why_cex failed on corruption CEX")
    why_rep = _read_json(ctx.path("why_corrupt.json"))
    assert_eq(why_rep.get("status"), "cex", "why_cex status")
    assert why_rep.get("active_slice_signals", 0) >= 1, f"Empty active slice: {why_rep!r}"
    assert len(why_rep.get("events", [])) >= 1, f"Empty causal events: {why_rep!r}"
    active_joined = " ".join(str(s) for s in why_rep.get("active_signals", []))
    assert "uncorr_din" not in active_joined, f"Unrelated uncorr_din leaked into why_cex slice: {why_rep!r}"


# ---------------------------------------------------------------------------
# T3-05: F6 x F8 — Manual waiver vs certified -prove_waivers + invalidation
# ---------------------------------------------------------------------------


def _case_t3_05(ctx: E2EContext) -> None:
    ctx.copy_fixture("workloads/t3_waiver_mix/t3_waiver_mix.sv")
    ctx.copy_fixture("workloads/t3_waiver_mix/t3_waiver_mix.upf")

    script = ctx.write_file(
        "run_t3_05.tcl",
        """
proc dump_json {cmd path} {
    set fh [open $path w]
    puts $fh [eval $cmd]
    close $fh
}
analyze -sv12 t3_waiver_mix.sv
elaborate -top t3_waiver_mix
clock clk
reset -expression {!rst_n}
load_upf t3_waiver_mix.upf

set_lp_waiver -add -rule ISO_MISSING -signal *sw_manual* -comment "manual review"
check_lp -stage rtl
dump_json {report_lp -json} rep_static.json
dump_json {report_lp -status waived -json} rep_waived.json

check_lp -prove_waivers
dump_json {report_lp -json} rep_after.json
dump_json {report_lp -status formally_waived -json} rep_fw.json
dump_json {report_lp -status cex -json} rep_cex.json

assume -name m_extra {din != 8'hFF}
dump_json {report_lp -json} rep_invalidated.json
""",
    )
    res = ctx.run_shell_file(script, timeout_s=90.0)
    assert_exit(res, 0, "T3-05 waiver/auto-waiver lifecycle failed")

    v_static = _extract_violations(_read_json(ctx.path("rep_static.json")))
    assert _find_violations(v_static, status="waived", needle="sw_manual"), (
        f"Expected sw_manual waived in static stage: {v_static!r}"
    )
    assert _find_violations(v_static, status="open", needle="sw_masked"), (
        f"Expected sw_masked open in static stage: {v_static!r}"
    )
    assert _find_violations(v_static, status="open", needle="sw_leak"), (
        f"Expected sw_leak open in static stage: {v_static!r}"
    )

    v_fw = _extract_violations(_read_json(ctx.path("rep_fw.json")))
    fw_masked = _find_violations(v_fw, status="formally_waived", needle="sw_masked")
    assert fw_masked, f"Expected sw_masked formally_waived after -prove_waivers: {v_fw!r}"
    assert fw_masked[0].get("auto_formal_meta") is not None, (
        f"Expected auto_formal_meta on certified auto-waiver: {fw_masked[0]!r}"
    )

    v_waived = _extract_violations(_read_json(ctx.path("rep_waived.json")))
    assert _find_violations(v_waived, status="waived", needle="sw_manual"), (
        f"Expected manual waiver preserved in rep_waived: {v_waived!r}"
    )

    v_cex = _extract_violations(_read_json(ctx.path("rep_cex.json")))
    leak_cex = _find_violations(v_cex, needle="sw_leak")
    assert leak_cex, f"Expected sw_leak in cex report: {v_cex!r}"
    trace_raw = _trace_of(leak_cex[0])
    assert trace_raw, f"Expected trace on sw_leak CEX: {leak_cex[0]!r}"
    vcd = parse_vcd_file(_resolve_workdir_path(ctx, trace_raw))
    assert signals_diverge(vcd), "Expected diverging m0/m1 signals on sw_leak CEX VCD"

    v_inv = _extract_violations(_read_json(ctx.path("rep_invalidated.json")))
    assert not _find_violations(v_inv, status="formally_waived", needle="sw_masked"), (
        f"Expected adding assume to invalidate formally_waived status on sw_masked: {v_inv!r}"
    )


# ---------------------------------------------------------------------------
# T3-06: F7 x F8 — LowPower AAG PMU sequencing + partial-retention X-leakage
# ---------------------------------------------------------------------------


def _case_t3_06(ctx: E2EContext) -> None:
    ctx.copy_fixture("workloads/t3_partial_ret_soc/t3_partial_ret_soc.sv")
    ctx.copy_fixture("workloads/t3_partial_ret_soc/t3_partial_ret_soc.upf")

    # BUG_MODE=0: clean PMU + clean retention/clearing
    res0 = ctx.run_shell_commands(
        [
            "analyze -sv12 t3_partial_ret_soc.sv",
            "elaborate -top t3_partial_ret_soc -param BUG_MODE 0",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf t3_partial_ret_soc.upf",
            "check_lp -stage formal -prove_corruption",
            "set fh [open stats0.json w]; puts $fh [get_stats -task LowPower]; close $fh",
            "set fh2 [open lp0.json w]; puts $fh2 [report_lp -json]; close $fh2",
        ],
        workdir=ctx.path("w_mode0"),
        timeout_s=90.0,
    )
    assert_exit(res0, 0, "T3-06 BUG_MODE=0 failed")
    stats0 = _read_json(ctx.path("stats0.json"))
    lp0_v = _extract_violations(_read_json(ctx.path("lp0.json")))
    assert any(
        "lp_iso_before_pwroff" in str(r.get("name", "")) and r.get("status") == "proven"
        for r in stats0
    ), f"Expected proven lp_iso_before_pwroff in BUG_MODE=0: {stats0!r}"
    assert any(
        "lp_pst_reach" in str(r.get("name", "")) and r.get("status") == "covered"
        for r in stats0
    ), f"Expected covered lp_pst_reach in BUG_MODE=0: {stats0!r}"
    assert not _find_violations(lp0_v, status="formal_cex"), (
        f"Expected 0 formal_cex in clean BUG_MODE=0: {lp0_v!r}"
    )

    # BUG_MODE=1: clean PMU sequencing, partial-retention post-wakeup X-leakage
    res1 = ctx.run_shell_commands(
        [
            "analyze -sv12 t3_partial_ret_soc.sv",
            "elaborate -top t3_partial_ret_soc -param BUG_MODE 1",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf t3_partial_ret_soc.upf",
            "check_lp -stage formal -prove_corruption",
            "set fh [open stats1.json w]; puts $fh [get_stats -task LowPower]; close $fh",
            "set fh2 [open lp1.json w]; puts $fh2 [report_lp -json]; close $fh2",
        ],
        workdir=ctx.path("w_mode1"),
        timeout_s=90.0,
    )
    assert_exit(res1, 0, "T3-06 BUG_MODE=1 failed")
    stats1 = _read_json(ctx.path("stats1.json"))
    assert any(
        "lp_iso_before_pwroff" in str(r.get("name", "")) and r.get("status") == "proven"
        for r in stats1
    ), f"Expected PMU sequencing still proven in BUG_MODE=1: {stats1!r}"
    lp1_v = _extract_violations(_read_json(ctx.path("lp1.json")))
    cex1 = _find_violations(lp1_v, status="formal_cex") or _find_violations(lp1_v, status="cex")
    assert cex1, f"Expected post-wakeup corruption CEX in BUG_MODE=1: {lp1_v!r}"
    trace1 = _trace_of(cex1[0])
    assert trace1, f"Expected trace path on partial-retention CEX: {cex1[0]!r}"
    vcd1 = parse_vcd_file(_resolve_workdir_path(ctx, trace1))
    assert has_lp_domain_signals(vcd1, "PD_CORE"), "Expected lp.pd.PD_CORE.* signals in VCD"
    assert signals_diverge(vcd1), "Expected m0/m1 divergence after wakeup in BUG_MODE=1"

    # BUG_MODE=2: PMU drops pwr_en before iso_en
    res2 = ctx.run_shell_commands(
        [
            "analyze -sv12 t3_partial_ret_soc.sv",
            "elaborate -top t3_partial_ret_soc -param BUG_MODE 2",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf t3_partial_ret_soc.upf",
            "check_lp -stage formal",
            "set fh [open stats2.json w]; puts $fh [get_stats -task LowPower]; close $fh",
        ],
        workdir=ctx.path("w_mode2"),
        timeout_s=90.0,
    )
    assert_exit(res2, 0, "T3-06 BUG_MODE=2 failed")
    stats2 = _read_json(ctx.path("stats2.json"))
    assert any(
        "lp_iso_before_pwroff" in str(r.get("name", "")) and r.get("status") == "cex"
        for r in stats2
    ), f"Expected lp_iso_before_pwroff CEX in BUG_MODE=2: {stats2!r}"


# ---------------------------------------------------------------------------
# T3-07: F8 x F10 — check_lp -prove_waivers + MCP lp_diagnose dossier
# ---------------------------------------------------------------------------


def _case_t3_07(ctx: E2EContext) -> None:
    ctx.copy_fixture("workloads/t3_waiver_mix/t3_waiver_mix.sv")
    ctx.copy_fixture("workloads/t3_waiver_mix/t3_waiver_mix.upf")

    with ctx.mcp(mode="auto") as mcp:
        assert_in("lp_diagnose", mcp.tool_names(), "lp_diagnose missing from MCP tools/list")
        mcp.call_tool(
            "session_open",
            {
                "files": ["t3_waiver_mix.sv"],
                "top": "t3_waiver_mix",
                "clock": "clk",
                "reset": "!rst_n",
            },
        )
        u = mcp.call_tool("load_upf", {"file": "t3_waiver_mix.upf"})
        assert_json_schema_field(u, "informal.upf/1")

        lp = mcp.call_tool("check_lp", {"stage": "all", "prove_waivers": True})
        assert_json_schema_field(lp, "informal.lp/1")
        violations = _extract_violations(lp)
        assert _find_violations(violations, status="formally_waived", needle="sw_masked"), (
            f"Expected sw_masked formally_waived: {violations!r}"
        )
        leak_cex = _find_violations(violations, status="formal_cex", needle="sw_leak") or _find_violations(
            violations, status="cex", needle="sw_leak"
        )
        assert leak_cex, f"Expected formal_cex on sw_leak: {violations!r}"

        target_v = leak_cex[0]
        diag_args: dict[str, Any] = {}
        if "id" in target_v:
            diag_args["id"] = target_v["id"]
        elif "name" in target_v:
            diag_args["violation"] = target_v["name"]
        else:
            diag_args["rule"] = _rule_of(target_v)
            diag_args["signal"] = "sw_leak"

        diag = mcp.call_tool("lp_diagnose", diag_args)
        assert_json_schema_field(diag, "informal.lp_diagnose/1")
        assert "pst_witness" in diag and diag["pst_witness"], f"Expected non-empty pst_witness: {diag!r}"
        rtl_info = (
            diag.get("rtl_crossing_path")
            or diag.get("rtl_path")
            or diag.get("driver_snippets")
            or diag.get("rtl_loc")
        )
        assert rtl_info, f"Expected RTL crossing path/snippets/loc in lp_diagnose: {diag!r}"
        assert "PD_SW" in json.dumps(rtl_info), (
            f"Expected PD_SW in lp_diagnose RTL crossing info: {diag!r}"
        )
        assert "why_cex" in diag, f"Expected why_cex in lp_diagnose: {diag!r}"
        why = diag["why_cex"]
        assert isinstance(why, dict) and len(why.get("events", [])) >= 1, (
            f"Expected non-empty why_cex events in lp_diagnose: {diag!r}"
        )


# ---------------------------------------------------------------------------
# T3-08: F5 x F9 — Symbolic PST legal states + Stage 4 PG switch_function
# ---------------------------------------------------------------------------


def _case_t3_08(ctx: E2EContext) -> None:
    ctx.write_file("mini.lib", generate_mini_liberty())
    ctx.write_file("stdcells.sv", generate_stdcell_verilog())
    ctx.write_file("pg.upf", generate_gate_pg_upf())
    clean_v = generate_gate_pg_netlist(num_cells=64, include_bugs=False)
    ctx.write_file("clean_pg.v", clean_v)

    # Break PSW_HDR_X1 switch_function chain by tying .SLEEP(1'b1) on the header
    broken_v = clean_v.replace(
        "PSW_HDR_X1 u_psw_core (.SLEEP(sleep_core)",
        "PSW_HDR_X1 u_psw_core (.SLEEP(1'b1)",
    )
    assert broken_v != clean_v, "Failed to mutate PSW_HDR_X1 SLEEP pin in broken_psw_pg.v"
    ctx.write_file("broken_psw_pg.v", broken_v)

    script = ctx.write_file(
        "run_t3_08.tcl",
        """
read_liberty mini.lib
analyze -sv stdcells.sv clean_pg.v
elaborate -top soc_pg_top
load_upf pg.upf
check_lp -stage pg
set fh1 [open pg_clean.json w]
puts $fh1 [report_lp -json]
close $fh1

analyze -clear
analyze -sv stdcells.sv broken_psw_pg.v
elaborate -top soc_pg_top
load_upf pg.upf
check_lp -stage pg
set fh2 [open pg_broken.json w]
puts $fh2 [report_lp -json]
close $fh2
""",
    )
    res = ctx.run_shell_file(script)
    assert_exit(res, 0, "T3-08 PG switch_function run failed")

    v_clean = _extract_violations(_read_json(ctx.path("pg_clean.json")))
    assert not _find_violations(v_clean, rule_prefix="PG_", status="open"), (
        f"Expected 0 PG_* violations on clean_pg.v: {v_clean!r}"
    )

    v_broken = _extract_violations(_read_json(ctx.path("pg_broken.json")))
    pg_errs = _find_violations(v_broken, rule_prefix="PG_")
    assert pg_errs, f"Expected PG_SWITCH_CHAIN_BROKEN / PG_SUPPLY_MISMATCH on broken_psw_pg.v: {v_broken!r}"


# ---------------------------------------------------------------------------
# T3-09: F9 x F2 — read_liberty + 100k-cell gate/PG fast reader + cache reuse
# ---------------------------------------------------------------------------


def _case_t3_09(ctx: E2EContext) -> None:
    ctx.write_file("mini.lib", generate_mini_liberty())
    ctx.write_file("pg.upf", generate_gate_pg_upf())
    ctx.write_file("soc_pg_100k.v", generate_gate_pg_netlist(num_cells=100000, include_bugs=True))
    ctx.write_file("soc_pg_100k_clean.v", generate_gate_pg_netlist(num_cells=100000, include_bugs=False))

    script = ctx.write_file(
        "run_t3_09.tcl",
        """
proc dump_rep {path} {
    set fh [open $path w]
    puts $fh [report_lp -json]
    close $fh
}
read_liberty mini.lib
set t0 [clock milliseconds]
analyze -sv soc_pg_100k.v
elaborate -top soc_pg_top
load_upf pg.upf
check_lp -stage netlist
dump_rep rep1_s3.json
check_lp -stage pg
dump_rep rep1_s4.json
set dt1 [expr {[clock milliseconds] - $t0}]
puts "PG_100K_PASS1_MS=$dt1"

set t1 [clock milliseconds]
check_lp -stage netlist
dump_rep rep2_s3.json
check_lp -stage pg
dump_rep rep2_s4.json
set dt2 [expr {[clock milliseconds] - $t1}]
puts "PG_100K_CACHED_MS=$dt2"

analyze -clear
analyze -sv soc_pg_100k_clean.v
elaborate -top soc_pg_top
load_upf pg.upf
check_lp -stage netlist
dump_rep rep3_s3.json
check_lp -stage pg
dump_rep rep3_s4.json
""",
    )
    res = ctx.run_shell_file(script, timeout_s=90.0)
    assert_exit(res, 0, "T3-09 100k-cell gate/PG fast reader failed")

    m1 = re.search(r"PG_100K_PASS1_MS=(\d+)", res.combined)
    m2 = re.search(r"PG_100K_CACHED_MS=(\d+)", res.combined)
    assert m1 and m2, f"Missing timing markers in T3-09 output:\n{res.combined}"
    ms1 = int(m1.group(1))
    ms2 = int(m2.group(1))
    assert ms1 < 10000, f"100k-cell Stage 3+4 took {ms1} ms (too slow)"
    assert ms2 <= ms1, f"Cached re-check ({ms2} ms) was slower than pass 1 ({ms1} ms)"

    v1_s3 = _extract_violations(_read_json(ctx.path("rep1_s3.json")))
    v1_s4 = _extract_violations(_read_json(ctx.path("rep1_s4.json")))
    v2_s3 = _extract_violations(_read_json(ctx.path("rep2_s3.json")))
    v2_s4 = _extract_violations(_read_json(ctx.path("rep2_s4.json")))
    assert_eq(v2_s3, v1_s3, "Cached 100k Stage 3 report drifted")
    assert_eq(v2_s4, v1_s4, "Cached 100k Stage 4 report drifted")

    rules1 = {_rule_of(v).upper() for v in v1_s3 + v1_s4}
    for expected_prefix in ("PG_SUPPLY_MISMATCH", "PG_PIN_UNCONNECTED", "ISO_LOGIC_BEFORE_CELL", "AON_NON_AON_BUFFER"):
        assert any(r.startswith(expected_prefix) for r in rules1), (
            f"Expected {expected_prefix} in 100k buggy netlist, got rules: {sorted(rules1)}"
        )

    v3_s3 = _extract_violations(_read_json(ctx.path("rep3_s3.json")))
    v3_s4 = _extract_violations(_read_json(ctx.path("rep3_s4.json")))
    assert not _find_violations(v3_s3 + v3_s4, status="open"), (
        f"Expected 0 open Stage 3/4 errors after cache invalidation to clean 100k netlist: {v3_s3 + v3_s4!r}"
    )


# ---------------------------------------------------------------------------
# T3-10: F1 x F10 — MCP load_upf + check_lp + spec_audit low_power gates
# ---------------------------------------------------------------------------


def _case_t3_10(ctx: E2EContext) -> None:
    ctx.copy_fixture("workloads/t3_spec_lp/t3_spec_lp.sv")
    ctx.copy_fixture("workloads/t3_spec_lp/t3_spec_bad.upf")
    ctx.copy_fixture("workloads/t3_spec_lp/t3_spec_clean.upf")
    ctx.copy_fixture("workloads/t3_spec_lp/t3_fvp.spec.json")

    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool(
            "session_open",
            {
                "files": ["t3_spec_lp.sv"],
                "top": "t3_spec_lp",
                "clock": "clk",
                "reset": "!rst_n",
            },
        )
        # 1. No UPF loaded yet -> signoff_ready must be False
        a1 = mcp.call_tool("spec_audit", {"spec_file": "t3_fvp.spec.json"})
        assert_json_schema_field(a1, "informal.spec_audit/1")
        assert_eq(a1.get("signoff_ready"), False, "Expected signoff_ready=False without UPF")

        # 2. Bad UPF (missing ISO) -> signoff_ready must remain False
        mcp.call_tool("load_upf", {"file": "t3_spec_bad.upf"})
        mcp.call_tool("check_lp", {"stage": "all", "prove_corruption": True})
        a2 = mcp.call_tool("spec_audit", {"spec_file": "t3_fvp.spec.json"})
        assert_json_schema_field(a2, "informal.spec_audit/1")
        assert_eq(a2.get("signoff_ready"), False, "Expected signoff_ready=False with open LP errors")

        # 3. Clean UPF + formal LPV + corruption proof -> signoff_ready transitions True
        mcp.call_tool("load_upf", {"file": "t3_spec_clean.upf"})
        mcp.call_tool("check_lp", {"stage": "all", "prove_corruption": True})
        a3 = mcp.call_tool("spec_audit", {"spec_file": "t3_fvp.spec.json"})
        assert_json_schema_field(a3, "informal.spec_audit/1")
        assert_eq(a3.get("signoff_ready"), True, f"Expected signoff_ready=True after clean LP signoff: {a3!r}")
        assert all(g.get("ok") is True for g in a3.get("gates", [])), f"Unexpected failing gate in {a3!r}"


# ---------------------------------------------------------------------------
# T3-11: F3 x F10 — Power-aware VCD lp.pd.* + why_cex + UI TraceView
# ---------------------------------------------------------------------------


def _case_t3_11(ctx: E2EContext) -> None:
    ctx.copy_fixture("workloads/t3_partial_ret_soc/t3_partial_ret_soc.sv")
    ctx.copy_fixture("workloads/t3_partial_ret_soc/t3_partial_ret_soc.upf")
    ctx.write_file(
        "open_bad_seq.tcl",
        """analyze -sv12 t3_partial_ret_soc.sv
elaborate -top t3_partial_ret_soc -param BUG_MODE 2
clock clk
reset -expression {!rst_n}
""",
    )

    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool("session_open", {"script": "open_bad_seq.tcl"})
        mcp.call_tool("load_upf", {"file": "t3_partial_ret_soc.upf"})
        mcp.call_tool("check_lp", {"stage": "formal"})

        plist = mcp.call_tool("property_list", {"task": "LowPower", "status": "cex"})
        props = plist.get("properties", [])
        cex_names = [str(p["name"]) for p in props if "lp_iso_before_pwroff" in str(p.get("name", ""))]
        assert cex_names, f"Expected lp_iso_before_pwroff CEX in LowPower task, got: {props!r}"
        prop_name = cex_names[0]

        wb = mcp.call_tool("open_workbench", {"focus_property": prop_name, "view": "trace"})
        tr = mcp.call_tool("trace_get", {"name": prop_name, "helpers": True})
        wc = mcp.call_tool("why_cex", {"name": prop_name})

        assert_eq(tr.get("frames"), wc.get("total_frames"), "trace_get frames vs why_cex total_frames")
        assert wc.get("failure_frame") in tr.get("hit_at", []), (
            f"why_cex failure_frame {wc.get('failure_frame')} not in trace_get hit_at {tr.get('hit_at')}: {wb!r}"
        )
        assert len(wc.get("events", [])) >= 1, f"Expected non-empty why_cex events: {wc!r}"

        # Find and parse VCD matching prop_name / trace_get metadata before fallback
        raw_vcd = (
            tr.get("vcd_path")
            or tr.get("trace_path")
            or tr.get("trace")
            or ((tr.get("property") or {}).get("trace") if isinstance(tr.get("property"), dict) else None)
            or wb.get("vcd_path")
            or wb.get("trace")
            or next((p.get("trace") for p in props if isinstance(p, dict) and p.get("name") == prop_name), None)
        )
        if isinstance(raw_vcd, str) and raw_vcd.strip():
            vcd_path = _resolve_workdir_path(ctx, raw_vcd.strip())
        else:
            prop_short = prop_name.split("::")[-1]
            vcd_matches = sorted((ctx.workdir / "mcp_work").rglob(f"*{prop_short}*.vcd"))
            if not vcd_matches:
                vcd_matches = sorted((ctx.workdir / "mcp_work").rglob("*.vcd"))
            assert vcd_matches, f"No VCD file generated under {ctx.workdir / 'mcp_work'}"
            vcd_path = vcd_matches[0]
        vcd = parse_vcd_file(vcd_path)
        assert has_lp_domain_signals(vcd, "PD_CORE"), (
            f"Expected lp.pd.PD_CORE.* swimlane signals in VCD {vcd_path}: {vcd.signal_paths()[:30]}"
        )
        assert signal_changes_after(vcd, "lp.pd.PD_CORE.*", 0), (
            "Expected lp.pd.PD_CORE.* swimlane transitions after t=0"
        )
        assert vcd.timescale, "Expected non-empty $timescale in VCD for UI TraceView"
        assert vcd.end_time >= 10, f"Expected VCD end_time >= 10, got {vcd.end_time}"

    # Verify UI TraceView + vcd.ts Vitest unit suite passes when node_modules exists
    ui_dir = ctx.repo_root / "ui"
    if (ui_dir / "node_modules").exists():
        v_res = run_command(
            ["npx", "--no-install", "vitest", "run", "src/components/TraceView.test.tsx", "src/vcd.test.ts"],
            cwd=ui_dir,
            env=_ui_env(ctx),
            timeout_s=60.0,
        )
        assert_exit(v_res, 0, "UI TraceView / vcd.ts vitest run failed")


# ---------------------------------------------------------------------------
# T3-12: F4 x F9 — Cadence CPF + Liberty Stage 3 netlist check & D13 guard
# ---------------------------------------------------------------------------


def _case_t3_12(ctx: E2EContext) -> None:
    ctx.write_file("mini.lib", generate_mini_liberty())
    ctx.write_file("stdcells.sv", generate_stdcell_verilog())
    ctx.copy_fixture("workloads/t3_cpf_gate/t3_cpf_gate.v")
    ctx.copy_fixture("workloads/t3_cpf_gate/t3_cpf_gate_buggy.v")
    ctx.copy_fixture("workloads/t3_cpf_gate/t3_cpf_gate_clean.cpf")
    ctx.copy_fixture("workloads/t3_cpf_gate/t3_cpf_gate_mismatch.cpf")
    ctx.copy_fixture("workloads/t3_cpf_gate/bad_cpf_d13.cpf")

    script = ctx.write_file(
        "run_t3_12.tcl",
        """
read_liberty mini.lib
analyze -sv stdcells.sv t3_cpf_gate.v
elaborate -top soc_cpf_top
load_upf t3_cpf_gate_clean.cpf -cpf
check_lp -stage netlist
set fh1 [open cpf_s3_clean.json w]
puts $fh1 [report_lp -json]
close $fh1

analyze -clear
analyze -sv stdcells.sv t3_cpf_gate_buggy.v
elaborate -top soc_cpf_top
load_upf t3_cpf_gate_mismatch.cpf -cpf
check_lp -stage netlist
set fh2 [open cpf_s3_bug.json w]
puts $fh2 [report_lp -json]
close $fh2
""",
    )
    res = ctx.run_shell_file(script)
    assert_exit(res, 0, "T3-12 CPF + Liberty Stage 3 check failed")

    v_clean = _extract_violations(_read_json(ctx.path("cpf_s3_clean.json")))
    assert not _find_violations(v_clean, status="open"), (
        f"Expected 0 open Stage 3 violations on clean CPF netlist: {v_clean!r}"
    )

    v_bug = _extract_violations(_read_json(ctx.path("cpf_s3_bug.json")))
    rules_bug = {_rule_of(v).upper() for v in v_bug}
    for req in ("ISO_CELL_MISMATCH", "ISO_LOGIC_BEFORE_CELL", "MACRO_PIN_SUPPLY_CROSSING"):
        assert any(r.startswith(req) for r in rules_bug), (
            f"Expected {req} in buggy CPF Stage 3 check, got rules: {sorted(rules_bug)}"
        )

    # Decision D13 strict semantic option error with file:line
    res_d13 = ctx.run_shell_commands(
        [
            "analyze -sv stdcells.sv t3_cpf_gate.v",
            "elaborate -top soc_cpf_top",
            "load_upf bad_cpf_d13.cpf -cpf",
        ]
    )
    assert res_d13.returncode != 0, "Expected Decision D13 failure on unsupported CPF semantic option"
    assert_contains(res_d13.combined, "bad_cpf_d13.cpf", msg="D13 diagnostic must cite filename")


# ---------------------------------------------------------------------------
# T3-13: F1 x F8 — allow_manual_lp_waivers: false + prove_cache + -prove_waivers
# ---------------------------------------------------------------------------


def _case_t3_13(ctx: E2EContext) -> None:
    ctx.copy_fixture("workloads/t3_waiver_mix/t3_waiver_mix.sv")
    ctx.copy_fixture("workloads/t3_waiver_mix/t3_waiver_masked_only.upf")
    ctx.write_json(
        "fvp.spec.json",
        {
            "schema": "informal.spec/1",
            "name": "t3_13_manual_waiver_gate",
            "design": {"top": "t3_waiver_mix", "clocks": ["clk"], "reset": "!rst_n"},
            "requirements": [
                {
                    "id": "REQ-LP-MASK-01",
                    "title": "Masked output stays zero when switchable domain is off",
                    "assertions": ["t3_waiver_mix.a_masked_clamp_ok"],
                    "covers": ["t3_waiver_mix.c_pwr_off_seen"],
                }
            ],
            "low_power": {
                "require_upf": True,
                "max_open_lp_errors": 0,
                "require_formal_lpv": True,
                "allow_manual_lp_waivers": False,
            },
            "signoff": {"proof_target": "proven", "forbid_vacuous": True, "require_covers": True},
        },
    )

    with ctx.mcp(mode="auto") as mcp:
        mcp.call_tool(
            "session_open",
            {
                "files": ["t3_waiver_mix.sv"],
                "top": "t3_waiver_mix",
                "clock": "clk",
                "reset": "!rst_n",
            },
        )
        mcp.call_tool("load_upf", {"file": "t3_waiver_masked_only.upf"})
        mcp.call_tool("prove_cache", {"enabled": True, "path": "lp_cache", "mode": "rw"})
        mcp.call_tool("prove", {})

        # Manual waiver leaves 0 open errors, but allow_manual_lp_waivers=false must reject signoff
        add_res = mcp.call_tool(
            "tcl_eval",
            {"command": 'set_lp_waiver -add -rule ISO_MISSING -signal *sw_masked* -comment "manual"'},
        )
        assert_eq(add_res.get("ok"), True, f"set_lp_waiver -add failed: {add_res!r}")
        wid = str(add_res.get("result", "")).strip()
        assert wid, f"Expected non-empty waiver ID from set_lp_waiver -add: {add_res!r}"

        mcp.call_tool("check_lp", {"stage": "all", "prove_corruption": True})
        a_manual = mcp.call_tool("spec_audit", {"spec_file": "fvp.spec.json"})
        assert_json_schema_field(a_manual, "informal.spec_audit/1")
        assert_eq(
            a_manual.get("signoff_ready"),
            False,
            "Expected allow_manual_lp_waivers=false to reject manual waiver",
        )

        # Remove manual waiver by captured ID and run certified -prove_waivers -> signoff_ready=True
        rm_res = mcp.call_tool("tcl_eval", {"command": f"set_lp_waiver -remove {wid}"})
        assert_eq(rm_res.get("ok"), True, f"set_lp_waiver -remove {wid} failed: {rm_res!r}")
        lp_fw = mcp.call_tool(
            "check_lp",
            {"stage": "all", "prove_corruption": True, "prove_waivers": True},
        )
        assert _find_violations(_extract_violations(lp_fw), status="formally_waived", needle="sw_masked"), (
            f"Expected sw_masked formally_waived: {lp_fw!r}"
        )
        a_fw = mcp.call_tool("spec_audit", {"spec_file": "fvp.spec.json"})
        assert_eq(
            a_fw.get("signoff_ready"),
            True,
            f"Expected certified auto-waiver to pass allow_manual_lp_waivers=false gate: {a_fw!r}",
        )

    cache_dir = ctx.path("lp_cache")
    assert cache_dir.is_dir() and any(cache_dir.rglob("*")), (
        f"Expected populated prove_cache directory at {cache_dir}"
    )


# ---------------------------------------------------------------------------
# T3-14: F6 x F7 — LowPower AAG <embedded> assumption inheritance + check_lpv
# ---------------------------------------------------------------------------


def _case_t3_14(ctx: E2EContext) -> None:
    ctx.copy_fixture("workloads/t3_pmu_ext_req/t3_pmu_ext_req.sv")
    ctx.copy_fixture("workloads/t3_pmu_ext_req/t3_pmu_ext_req.upf")

    # 1. Without system assumption: lp_iso_before_pwroff fails CEX, Stage 2 catches dbg_uniso_o
    res_no_asm = ctx.run_shell_commands(
        [
            "analyze -sv12 t3_pmu_ext_req.sv",
            "elaborate -top t3_pmu_ext_req",
            "clock clk",
            "reset -expression {!rst_n}",
            "load_upf t3_pmu_ext_req.upf",
            "check_lp -stage rtl",
            "check_lpv",
            "set f1 [open lp_no_asm.json w]; puts $f1 [report_lp -json]; close $f1",
            "set f2 [open st_no_asm.json w]; puts $f2 [get_stats -task LowPower]; close $f2",
        ],
        workdir=ctx.path("w_no_asm"),
        timeout_s=90.0,
    )
    assert_exit(res_no_asm, 0, "T3-14 without assumption failed")
    st1 = _read_json(ctx.path("st_no_asm.json"))
    assert any(
        "lp_iso_before_pwroff" in str(r.get("name", "")) and r.get("status") == "cex"
        for r in st1
    ), f"Expected lp_iso_before_pwroff CEX without m_no_abort: {st1!r}"
    assert _find_violations(_extract_violations(_read_json(ctx.path("lp_no_asm.json"))), rule_prefix="ISO_MISSING", needle="dbg_uniso_o"), (
        "Expected ISO_MISSING on dbg_uniso_o"
    )

    # 2. Add <embedded> assumption -> LowPower AAG inherits it & proves, Stage 2 still flags dbg_uniso_o
    res_with_asm = ctx.run_shell_commands(
        [
            "analyze -sv12 t3_pmu_ext_req.sv",
            "elaborate -top t3_pmu_ext_req",
            "clock clk",
            "reset -expression {!rst_n}",
            "assume -name m_no_abort {!(sleep_req && wake_req)}",
            "load_upf t3_pmu_ext_req.upf",
            "check_lp -stage rtl",
            "check_lpv",
            "set f1 [open lp_with_asm.json w]; puts $f1 [report_lp -json]; close $f1",
            "set f2 [open st_with_asm.json w]; puts $f2 [get_stats -task LowPower]; close $f2",
        ],
        workdir=ctx.path("w_with_asm"),
        timeout_s=90.0,
    )
    assert_exit(res_with_asm, 0, "T3-14 with assumption failed")
    st2 = _read_json(ctx.path("st_with_asm.json"))
    assert any(
        "lp_iso_before_pwroff" in str(r.get("name", "")) and r.get("status") == "proven"
        for r in st2
    ), f"Expected LowPower AAG to inherit <embedded> m_no_abort and prove lp_iso_before_pwroff: {st2!r}"
    assert _find_violations(_extract_violations(_read_json(ctx.path("lp_with_asm.json"))), rule_prefix="ISO_MISSING", needle="dbg_uniso_o"), (
        "Expected Stage 2 static ISO_MISSING on dbg_uniso_o to remain open"
    )


def get_cases() -> list[TestCase]:
    """Return all 14 Tier 3 pairwise combinatorial test cases."""
    return [
        TestCase(
            id="T3-01",
            tier=3,
            feature=("F2", "F6"),
            title="CompiledDesign cache hit and SV/include/UPF invalidation with Stage 2 crossing check",
            fn=_case_t3_01,
            timeout_s=60.0,
            tags=("cache", "static_rtl", "mcp", "include"),
        ),
        TestCase(
            id="T3-02",
            tier=3,
            feature=("F4", "F5"),
            title="UPF 2.1 vs Cadence CPF 2.0 equivalence via compare_upf and pst_sat crossing matrix",
            fn=_case_t3_02,
            timeout_s=60.0,
            tags=("upf", "cpf", "compare_upf", "pst_sat"),
        ),
        TestCase(
            id="T3-03",
            tier=3,
            feature=("F5", "F6"),
            title="32-domain symbolic PST SAT scaling and Stage 2 RTL crossing evaluation",
            fn=_case_t3_03,
            timeout_s=60.0,
            tags=("pst_sat", "scale", "static_rtl", "bus_grouping"),
        ),
        TestCase(
            id="T3-04",
            tier=3,
            feature=("F3", "F8"),
            title="2-copy corruption proof CEX with m0/m1 VCD replay and why_cex backward slice",
            fn=_case_t3_04,
            timeout_s=90.0,
            tags=("formal_lpv", "miter", "vcd", "why_cex"),
        ),
        TestCase(
            id="T3-05",
            tier=3,
            feature=("F6", "F8"),
            title="Manual set_lp_waiver vs certified check_lp -prove_waivers and assumption invalidation",
            fn=_case_t3_05,
            timeout_s=90.0,
            tags=("waivers", "prove_waivers", "certified", "invalidation"),
        ),
        TestCase(
            id="T3-06",
            tier=3,
            feature=("F7", "F8"),
            title="LowPower AAG PMU sequencing and partial-retention post-wakeup X-leakage 2-copy proof",
            fn=_case_t3_06,
            timeout_s=90.0,
            tags=("aag", "retention", "partial_retention", "formal_lpv"),
        ),
        TestCase(
            id="T3-07",
            tier=3,
            feature=("F8", "F10"),
            title="Certified auto-waiver promotion paired with MCP lp_diagnose root-cause dossier",
            fn=_case_t3_07,
            timeout_s=90.0,
            tags=("mcp", "lp_diagnose", "prove_waivers", "why_cex"),
        ),
        TestCase(
            id="T3-08",
            tier=3,
            feature=("F5", "F9"),
            title="Symbolic PST legal states paired with Stage 4 PG PSW_HDR_X1 switch_function chain",
            fn=_case_t3_08,
            timeout_s=60.0,
            tags=("pst_sat", "liberty", "pg", "switch_function"),
        ),
        TestCase(
            id="T3-09",
            tier=3,
            feature=("F9", "F2"),
            title="read_liberty + 100k-cell gate/PG fast reader and CompiledDesign cache reuse/invalidation",
            fn=_case_t3_09,
            timeout_s=90.0,
            tags=("100k_cells", "liberty", "gate_fast", "cache", "performance"),
        ),
        TestCase(
            id="T3-10",
            tier=3,
            feature=("F1", "F10"),
            title="MCP load_upf + check_lp + spec_audit low_power gates signoff_ready transition",
            fn=_case_t3_10,
            timeout_s=90.0,
            tags=("spec_audit", "low_power_gates", "mcp", "signoff"),
        ),
        TestCase(
            id="T3-11",
            tier=3,
            feature=("F3", "F10"),
            title="Power-aware VCD lp.pd.* swimlane signals + why_cex + UI TraceView compatibility",
            fn=_case_t3_11,
            timeout_s=90.0,
            tags=("vcd", "swimlanes", "why_cex", "ui", "traceview"),
        ),
        TestCase(
            id="T3-12",
            tier=3,
            feature=("F4", "F9"),
            title="Cadence CPF (load_upf -cpf) + Liberty .lib Stage 3 netlist cell verification & D13 guard",
            fn=_case_t3_12,
            timeout_s=60.0,
            tags=("cpf", "liberty", "stage3", "netlist", "d13"),
        ),
        TestCase(
            id="T3-13",
            tier=3,
            feature=("F1", "F8"),
            title="fvp.spec.json allow_manual_lp_waivers gate vs certified -prove_waivers + prove_cache reuse",
            fn=_case_t3_13,
            timeout_s=90.0,
            tags=("spec_audit", "prove_waivers", "prove_cache", "manual_waiver_gate"),
        ),
        TestCase(
            id="T3-14",
            tier=3,
            feature=("F6", "F7"),
            title="LowPower AAG inheritance of embedded assumptions + check_lpv alias + Stage 2 RTL check",
            fn=_case_t3_14,
            timeout_s=90.0,
            tags=("aag", "assumptions", "check_lpv", "static_rtl"),
        ),
    ]
