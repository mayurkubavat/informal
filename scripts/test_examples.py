#!/usr/bin/env python3
"""Execute and verify every hands-on lab under `examples/`.

1. Runs `informal shell --check <script>` on every `.tcl` lab script.
2. Unless `--check-only` is set, runs each lab end-to-end in an isolated
   temporary workdir and asserts expected verification verdicts.
3. Validates `fvp.spec.json` files parse cleanly as `informal.spec/1`.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


@dataclass(frozen=True)
class LabCase:
    script: str
    expect_substrings: tuple[str, ...]


LABS: tuple[LabCase, ...] = (
    LabCase(
        "examples/01-quickstart/counter/run.jg.tcl",
        ("a_max", "proven", "a_never9", "cex", "c_done", "covered", "c_10", "unreachable"),
    ),
    LabCase(
        "examples/01-quickstart/counter/run.vcf.tcl",
        ("a_max", "proven", "a_never9", "cex", "c_done", "covered", "c_10", "unreachable"),
    ),
    LabCase(
        "examples/02-fpv-fundamentals/fifo/run.jg.tcl",
        ("a_count_max", "a_data", "CexDemo::a_wrong", "cex", "Environment check: alive"),
    ),
    LabCase(
        "examples/03-convergence/deep_counter/run.jg.tcl",
        ("a_wdog_bound", "proven", "c_wdog_expired", "covered", "u_mul"),
    ),
    LabCase(
        "examples/04-formal-apps/cov/run.jg.tcl",
        ("a_chk", "proven", "waived"),
    ),
    LabCase(
        "examples/04-formal-apps/conn/run.jg.tcl",
        ("Connectivity::mux_conn", "Connectivity::pipe_conn", "Connectivity::bad_conn"),
    ),
    LabCase(
        "examples/04-formal-apps/xprop/run.jg.tcl",
        ("xprop_dut.clean_out", "xprop_dut.reconv_out", "xprop_dut.leaky_out"),
    ),
    LabCase(
        "examples/04-formal-apps/spv/run.jg.tcl",
        ("iso_pub", "iso_dbg_locked", "leak_dbg_open", "leak_covert_2cyc"),
    ),
    LabCase(
        "examples/04-formal-apps/sec/run.jg.tcl",
        ("sec_out_acc_out", "lec_reg_acc_q"),
    ),
    LabCase(
        "examples/04-formal-apps/csr/run.jg.tcl",
        (
            "CSR::csr_cfg_regwen_wen_reset",
            "CSR::csr_ctrl_mode_lock",
            "CSR::csr_irq_state_pending_access",
            "CSR::csr_key_wo_secret_read",
        ),
    ),
    LabCase(
        "examples/04-formal-apps/protocol/run.jg.tcl",
        ("Protocol::", "apb"),
    ),
    LabCase(
        "examples/05-integration-and-signoff/signoff-spec/run.jg.tcl",
        ("Environment check: alive", "SIGNOFF GATE: PASS"),
    ),
)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--bin",
        type=Path,
        default=REPO / "target/debug/informal",
        help="Path to informal executable",
    )
    ap.add_argument(
        "--check-only",
        action="store_true",
        help="Only run `informal shell --check` and JSON validation (skip proving)",
    )
    args = ap.parse_args()

    bin_path = args.bin.resolve()
    if not bin_path.exists():
        print(f"test_examples: binary not found at {bin_path}", file=sys.stderr)
        return 2

    # Ensure every .tcl file under examples/ is listed in LABS.
    discovered = sorted(
        str(p.relative_to(REPO)) for p in (REPO / "examples").rglob("*.tcl")
    )
    listed = sorted(c.script for c in LABS)
    if discovered != listed:
        print(
            f"test_examples: LABS mismatch.\n  discovered={discovered}\n  listed={listed}",
            file=sys.stderr,
        )
        return 1

    # Validate all fvp.spec.json files under examples/.
    for spec_path in sorted((REPO / "examples").rglob("fvp.spec.json")):
        data = json.loads(spec_path.read_text(encoding="utf-8"))
        if data.get("schema") != "informal.spec/1":
            print(
                f"test_examples: {spec_path.relative_to(REPO)} has unexpected schema {data.get('schema')!r}",
                file=sys.stderr,
            )
            return 1

    failures: list[str] = []
    with tempfile.TemporaryDirectory(prefix="informal-examples-") as tmp:
        for idx, case in enumerate(LABS):
            script_path = REPO / case.script
            t0 = time.monotonic()

            # 1. Pre-flight --check must pass cleanly.
            chk = subprocess.run(
                [str(bin_path), "shell", "--check", str(script_path)],
                cwd=REPO,
                capture_output=True,
                text=True,
            )
            if chk.returncode != 0:
                failures.append(
                    f"{case.script} (--check failed, rc={chk.returncode}):\n{chk.stdout}{chk.stderr}"
                )
                continue

            if args.check_only:
                print(f"  [CHECK OK] {case.script} ({time.monotonic() - t0:.2f}s)")
                continue

            # 2. Full end-to-end prove in isolated workdir.
            workdir = Path(tmp) / f"case_{idx}"
            proc = subprocess.run(
                [
                    str(bin_path),
                    "shell",
                    str(script_path),
                    "--workdir",
                    str(workdir),
                ],
                cwd=REPO,
                capture_output=True,
                text=True,
            )
            combined = proc.stdout + "\n" + proc.stderr
            if proc.returncode != 0:
                failures.append(
                    f"{case.script} (run failed, rc={proc.returncode}):\n{combined}"
                )
                continue
            missing = [s for s in case.expect_substrings if s not in combined]
            if missing:
                failures.append(
                    f"{case.script} (missing expected output {missing}):\n{combined}"
                )
                continue
            print(f"  [PASS] {case.script} ({time.monotonic() - t0:.2f}s)")

    if failures:
        print("\ntest_examples: FAILED", file=sys.stderr)
        for f in failures:
            print(f"--- {f}", file=sys.stderr)
        return 1

    mode = "check-only" if args.check_only else "full prove"
    print(f"test_examples: OK ({len(LABS)} labs passed, mode={mode})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
