#!/usr/bin/env python3
"""Deterministic IEEE 1801-2018 (UPF 3.1) generator for Experiment 1 (Symbolic PST SAT scaling)."""

import argparse
from pathlib import Path


def generate_upf(n: int) -> str:
    if n < 2 or n % 2 != 0:
        raise ValueError(f"--n must be an even integer >= 2, got {n}")

    lines: list[str] = [
        f"# Auto-generated Experiment 1 UPF 3.1 (N={n} switchable domains, k=3 states/domain -> 3^{n} explicit PST rows)",
        "upf_version 3.1",
        "",
        "create_supply_net VSS",
        "create_supply_net VDD_AON",
        "create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}",
        "create_power_domain PD_AON -include_scope -supply {primary SS_AON}",
        "add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -simstate NORMAL}",
        "",
    ]

    for i in range(n):
        v_on = "0.90" if i % 2 == 0 else "0.75"
        lines.extend([
            f"create_supply_net VDD_D{i}",
            f"create_supply_set SS_D{i} -function {{power VDD_D{i}}} -function {{ground VSS}}",
            f"create_power_domain PD_D{i} -elements {{u_dom_{i}}} -supply {{primary SS_D{i}}}",
            (
                f"add_power_state SS_D{i} -state "
                f"{{ON -supply_expr {{power == {{FULL_ON {v_on}}} && ground == {{FULL_ON 0.0}}}} "
                f"-logic_expr {{pwr_ctrl[{i}] && !sleep_req[{i}]}} -simstate NORMAL}}"
            ),
            (
                f"add_power_state SS_D{i} -state "
                f"{{RET -supply_expr {{power == {{FULL_ON 0.65}} && ground == {{FULL_ON 0.0}}}} "
                f"-logic_expr {{!pwr_ctrl[{i}] && ret_en[{i}]}} -simstate CORRUPT_STATE_ON_ACTIVITY}}"
            ),
            (
                f"add_power_state SS_D{i} -state "
                f"{{OFF -supply_expr {{power == OFF && ground == {{FULL_ON 0.0}}}} "
                f"-logic_expr {{!pwr_ctrl[{i}] && !ret_en[{i}]}} -simstate CORRUPT}}"
            ),
        ])

    lines.append("")
    for i in range(0, n, 2):
        lines.append(
            f"add_power_state SS_D{i + 1} -state "
            f"{{ILL_DEP_{i} -logic_expr {{!pwr_ctrl[{i}] && pwr_ctrl[{i + 1}]}} -illegal}}"
        )

    supplies = " ".join(["SS_AON"] + [f"SS_D{i}" for i in range(n)])
    all_on = " ".join(["ON"] + ["ON"] * n)
    all_ret = " ".join(["ON"] + ["RET"] * n)
    all_off = " ".join(["ON"] + ["OFF"] * n)
    free_active = " ".join(["ON"] + ["*"] * n)

    lines.extend([
        "",
        f"create_pst PST_SOC -supplies {{{supplies}}}",
        f"add_pst_state ALL_ON -pst PST_SOC -state {{{all_on}}}",
        f"add_pst_state ALL_RET -pst PST_SOC -state {{{all_ret}}}",
        f"add_pst_state ALL_OFF -pst PST_SOC -state {{{all_off}}}",
        f"add_pst_state FREE_ACTIVE -pst PST_SOC -state {{{free_active}}}",
        "",
    ])
    return "\n".join(lines)


def main() -> None:
    ap = argparse.ArgumentParser(description="Generate Experiment 1 UPF 3.1 scaling benchmark")
    ap.add_argument("--n", type=int, required=True, help="Number of switchable domains (even >= 2)")
    ap.add_argument("--out", type=Path, default=None, help="Output UPF path (default: stdout)")
    args = ap.parse_args()
    text = generate_upf(args.n)
    if args.out is not None:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(text, encoding="utf-8")
    else:
        print(text, end="")


if __name__ == "__main__":
    main()
