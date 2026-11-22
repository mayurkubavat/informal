#!/usr/bin/env python3
"""Opaque-box E2E test runner for InFormal Unified LP Static + Formal LP."""

from __future__ import annotations

import ast
import sys

# Must precede local imports so `python3 -I` (safe_path=1) resolves `lib`/`cases`
# and never writes `__pycache__` directories into the git working tree.
sys.dont_write_bytecode = True

from pathlib import Path  # noqa: E402

E2E_DIR = Path(__file__).resolve().parent
if str(E2E_DIR) not in sys.path:
    sys.path.insert(0, str(E2E_DIR))

import argparse  # noqa: E402
from collections.abc import Iterable, Mapping  # noqa: E402
import fnmatch  # noqa: E402
import importlib  # noqa: E402
import json  # noqa: E402
import math  # noqa: E402
import py_compile  # noqa: E402
import re  # noqa: E402
import tempfile  # noqa: E402
import time  # noqa: E402
import traceback  # noqa: E402
from typing import Any  # noqa: E402

from lib.harness import (  # noqa: E402
    E2E_ROOT,
    FIXTURES_ROOT,
    REPO_ROOT,
    E2EContext,
    TestCase,
    build_tool_env,
    normalize_features,
    resolve_engines_toml,
    resolve_informal_bin,
)
from lib.vcd_check import parse_vcd  # noqa: E402

_LOCAL_IMPORT_ROOTS: frozenset[str] = frozenset({"lib", "cases", "run_e2e"})
_SV_BLOCK_COMMENT_RE = re.compile(r"/\*.*?\*/", re.DOTALL)
_SV_LINE_COMMENT_RE = re.compile(r"//[^\n]*")
_SV_STRING_RE = re.compile(r'"(?:\\.|[^"\\])*"')
_NEGATIVE_MARKERS: tuple[str, ...] = ("invalid", "malformed", "bad_")

# Constructed dynamically so this file itself never triggers repo hygiene grep rules.
_FORBIDDEN_STRINGS: tuple[str, ...] = (
    "claude" + ".ai/",
    "noreply" + "@anthropic.com",
    "Co-Authored" + "-By:",
)
_FORBIDDEN_FILENAMES: frozenset[str] = frozenset({"CLAUDE.md", ".mcp.json"})


def _parse_tiers(raw_tiers: list[str] | None) -> set[int]:
    if not raw_tiers:
        return {1, 2, 3, 4}
    tiers: set[int] = set()
    for item in raw_tiers:
        for tok in item.split(","):
            val = tok.strip().lower()
            if not val:
                continue
            if val == "all":
                return {1, 2, 3, 4}
            if val in {"1", "2", "3", "4"}:
                tiers.add(int(val))
            else:
                raise argparse.ArgumentTypeError(
                    f"Invalid --tier value {tok!r}; expected 1, 2, 3, 4, or all"
                )
    return tiers or {1, 2, 3, 4}


def _matches_filter(case: TestCase, pattern: str | None) -> bool:
    if not pattern:
        return True
    pat = pattern.strip().lower()
    if not pat:
        return True
    fields = [case.id, case.title, *case.features, *case.tags]
    is_glob = any(ch in pat for ch in ("*", "?", "["))
    for field in fields:
        low = field.lower()
        if is_glob:
            if fnmatch.fnmatchcase(low, pat):
                return True
        elif pat in low:
            return True
    return False


def _discover_case_modules() -> list[Path]:
    cases_dir = E2E_DIR / "cases"
    if not cases_dir.is_dir():
        return []
    return sorted(cases_dir.glob("tier*.py"))


def _load_cases(errors: list[str] | None = None) -> list[TestCase]:
    cases: list[TestCase] = []
    for mod_path in _discover_case_modules():
        mod_name = f"cases.{mod_path.stem}"
        try:
            mod = importlib.import_module(mod_name)
        except Exception as exc:
            msg = f"Failed importing {mod_path.relative_to(REPO_ROOT)}: {exc}"
            if errors is not None:
                errors.append(msg)
                continue
            raise RuntimeError(msg) from exc

        get_cases = getattr(mod, "get_cases", None)
        if not callable(get_cases):
            msg = f"{mod_path.relative_to(REPO_ROOT)} must define callable get_cases()"
            if errors is not None:
                errors.append(msg)
                continue
            raise RuntimeError(msg)

        try:
            loaded = get_cases()
        except Exception as exc:
            msg = f"Failed calling {mod_path.relative_to(REPO_ROOT)}.get_cases(): {exc}"
            if errors is not None:
                errors.append(msg)
                continue
            raise RuntimeError(msg) from exc

        if not isinstance(loaded, list):
            msg = (
                f"{mod_path.relative_to(REPO_ROOT)}.get_cases() must return a list, "
                f"got {type(loaded).__name__}"
            )
            if errors is not None:
                errors.append(msg)
                continue
            raise RuntimeError(msg)

        for item in loaded:
            if not isinstance(item, TestCase):
                msg = (
                    f"{mod_path.relative_to(REPO_ROOT)}.get_cases() yielded non-TestCase "
                    f"object {type(item).__name__}"
                )
                if errors is not None:
                    errors.append(msg)
                    continue
                raise RuntimeError(msg)
            cases.append(item)
    return cases


def _validate_case_contracts(cases: list[TestCase], errors: list[str]) -> None:
    seen_ids: set[str] = set()
    for idx, case in enumerate(cases):
        label = f"case[{idx}] ({case.id!r})"
        if not isinstance(case.id, str) or not case.id.strip():
            errors.append(f"{label}: id must be a non-empty string")
        elif case.id in seen_ids:
            errors.append(f"Duplicate TestCase id: {case.id!r}")
        else:
            seen_ids.add(case.id)

        if (
            isinstance(case.tier, bool)
            or not isinstance(case.tier, int)
            or case.tier not in {1, 2, 3, 4}
        ):
            errors.append(f"{label}: tier must be in {{1, 2, 3, 4}}, got {case.tier!r}")

        try:
            normalize_features(case.feature)
        except (TypeError, ValueError) as exc:
            errors.append(f"{label}: invalid feature: {exc}")

        if not isinstance(case.title, str) or not case.title.strip():
            errors.append(f"{label}: title must be a non-empty string")

        if not callable(case.fn):
            errors.append(f"{label}: fn must be callable")

        if (
            isinstance(case.timeout_s, bool)
            or not isinstance(case.timeout_s, (int, float))
            or not math.isfinite(case.timeout_s)
            or case.timeout_s <= 0
        ):
            errors.append(
                f"{label}: timeout_s must be a finite number > 0, got {case.timeout_s!r}"
            )

        if isinstance(case.tags, (str, bytes, bytearray, Mapping)) or not isinstance(
            case.tags, Iterable
        ):
            errors.append(
                f"{label}: tags must be a sequence/iterable of str, got {type(case.tags).__name__}"
            )
        else:
            for tag_idx, tag in enumerate(case.tags):
                if not isinstance(tag, str):
                    errors.append(
                        f"{label}: tags[{tag_idx}] must be str, got {type(tag).__name__}"
                    )


def _audit_stdlib_imports(py_path: Path, errors: list[str]) -> None:
    rel = py_path.relative_to(REPO_ROOT)
    try:
        source = py_path.read_text(encoding="utf-8")
        tree = ast.parse(source, filename=str(py_path))
    except Exception as exc:
        errors.append(f"{rel}: AST parse error: {exc}")
        return

    stdlib = sys.stdlib_module_names
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                root = alias.name.split(".", 1)[0]
                if root not in stdlib and root not in _LOCAL_IMPORT_ROOTS:
                    errors.append(
                        f"{rel}:{node.lineno}: non-stdlib import {alias.name!r} forbidden"
                    )
        elif isinstance(node, ast.ImportFrom):
            if node.level > 0 or not node.module:
                continue
            root = node.module.split(".", 1)[0]
            if root not in stdlib and root not in _LOCAL_IMPORT_ROOTS:
                errors.append(
                    f"{rel}:{node.lineno}: non-stdlib import from {node.module!r} forbidden"
                )


def _strip_sv_comments_and_strings(text: str) -> str:
    no_block = _SV_BLOCK_COMMENT_RE.sub(" ", text)
    no_line = _SV_LINE_COMMENT_RE.sub(" ", no_block)
    return _SV_STRING_RE.sub('""', no_line)


def _validate_fixtures(errors: list[str]) -> int:
    if not FIXTURES_ROOT.exists():
        return 0

    checked = 0
    for fpath in sorted(FIXTURES_ROOT.rglob("*")):
        if not fpath.is_file():
            continue
        checked += 1
        rel = fpath.relative_to(REPO_ROOT)
        try:
            text = fpath.read_text(encoding="utf-8")
        except Exception as exc:
            errors.append(f"{rel}: not valid UTF-8: {exc}")
            continue

        is_negative = any(marker in fpath.name.lower() for marker in _NEGATIVE_MARKERS)
        if is_negative:
            continue

        suffix = fpath.suffix.lower()
        if suffix == ".json":
            try:
                data = json.loads(text)
            except json.JSONDecodeError as exc:
                errors.append(f"{rel}: invalid JSON: {exc}")
                continue
            if isinstance(data, dict) and data.get("schema") == "informal.spec/1":
                reqs = data.get("requirements")
                if not isinstance(reqs, list):
                    errors.append(f"{rel}: informal.spec/1 missing 'requirements' list")
                else:
                    seen_req_ids: set[str] = set()
                    for r in reqs:
                        if isinstance(r, dict) and isinstance(r.get("id"), str):
                            rid = r["id"]
                            if rid in seen_req_ids:
                                errors.append(f"{rel}: duplicate requirement id {rid!r}")
                            seen_req_ids.add(rid)
        elif suffix in {".sv", ".v"}:
            cleaned = _strip_sv_comments_and_strings(text)
            if not cleaned.strip():
                errors.append(f"{rel}: empty SV/Verilog fixture")
                continue
            for kw_start, kw_end in (
                ("module", "endmodule"),
                ("package", "endpackage"),
                ("interface", "endinterface"),
            ):
                n_start = len(re.findall(rf"\b{kw_start}\b", cleaned))
                n_end = len(re.findall(rf"\b{kw_end}\b", cleaned))
                if n_start != n_end:
                    errors.append(
                        f"{rel}: unbalanced {kw_start}/{kw_end} ({n_start} vs {n_end})"
                    )
        elif suffix == ".vcd":
            try:
                parse_vcd(text)
            except Exception as exc:
                errors.append(f"{rel}: VCD parse error: {exc}")
    return checked


def _validate_hygiene(errors: list[str]) -> None:
    for path in sorted(E2E_ROOT.rglob("*")):
        if "__pycache__" in path.parts:
            errors.append(
                f"{path.relative_to(REPO_ROOT)}: __pycache__ artifact must not exist in working tree"
            )
            continue
        if not path.is_file():
            continue
        name = path.name
        rel = path.relative_to(REPO_ROOT)
        if (
            name in _FORBIDDEN_FILENAMES
            or name.startswith(".env")
            or path.suffix.lower() in {".key", ".pem", ".p12", ".pfx"}
        ):
            errors.append(f"{rel}: forbidden file name/extension for repo hygiene")
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        for marker in _FORBIDDEN_STRINGS:
            if marker in text:
                errors.append(f"{rel}: contains forbidden hygiene string {marker!r}")


def run_validation(emit_json: bool) -> int:
    errors: list[str] = []
    py_files = sorted(
        p for p in E2E_ROOT.rglob("*.py") if "__pycache__" not in p.parts
    )

    with tempfile.TemporaryDirectory(prefix="lp_e2e_pyc_") as pyc_dir:
        for idx, py_path in enumerate(py_files):
            rel = py_path.relative_to(REPO_ROOT)
            cfile = str(Path(pyc_dir) / f"mod_{idx}.pyc")
            try:
                py_compile.compile(str(py_path), cfile=cfile, doraise=True)
            except py_compile.PyCompileError as exc:
                errors.append(f"{rel}: py_compile error: {exc}")
            _audit_stdlib_imports(py_path, errors)

    cases = _load_cases(errors=errors)
    _validate_case_contracts(cases, errors)
    fixtures_checked = _validate_fixtures(errors)
    _validate_hygiene(errors)

    ok = len(errors) == 0
    if emit_json:
        payload = {
            "schema": "informal.lp_e2e.validate/1",
            "ok": ok,
            "python_files_checked": len(py_files),
            "cases_discovered": len(cases),
            "fixtures_checked": fixtures_checked,
            "errors": errors,
        }
        print(json.dumps(payload, indent=2))
    else:
        print(
            f"validate-only: checked {len(py_files)} Python files, "
            f"{len(cases)} cases discovered, {fixtures_checked} fixtures"
        )
        if errors:
            for err in errors:
                print(f"  ERROR: {err}", file=sys.stderr)
        else:
            print("validate-only: OK")

    return 0 if ok else 1


def _case_to_dict(case: TestCase) -> dict[str, Any]:
    return {
        "id": case.id,
        "tier": case.tier,
        "features": list(case.features),
        "title": case.title,
        "timeout_s": case.timeout_s,
        "tags": list(case.tags),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Opaque-box E2E test runner for InFormal Unified LP Static + Formal LP."
    )
    parser.add_argument(
        "--tier",
        action="append",
        help="Tier(s) to run: 1, 2, 3, 4, or all (default: all). Repeatable or comma-separated.",
    )
    parser.add_argument(
        "--bin",
        dest="bin_path",
        default=None,
        help="Path to compiled `informal` binary (default: $INFORMAL_BIN or target/{debug,release}/informal).",
    )
    parser.add_argument(
        "--engines",
        dest="engines_path",
        default=None,
        help="Path to engines.toml (default: <repo>/config/engines.toml).",
    )
    parser.add_argument(
        "--filter",
        dest="filter_pat",
        default=None,
        help="Case-insensitive substring or glob over id, feature, title, and tags.",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="List matching test cases and exit 0 without executing.",
    )
    parser.add_argument(
        "--validate-only",
        action="store_true",
        help="Validate Python compilation, stdlib-only imports, case contracts, fixtures, and hygiene.",
    )
    parser.add_argument(
        "--json",
        dest="emit_json",
        action="store_true",
        help="Emit machine-readable JSON output on stdout.",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=None,
        help="Override/scale per-case timeout budget in seconds.",
    )
    parser.add_argument(
        "--keep-workdir",
        action="store_true",
        help="Preserve per-case temporary working directories after completion.",
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="Print extra diagnostic output and tracebacks.",
    )

    args = parser.parse_args(argv)
    if args.timeout is not None and not (math.isfinite(args.timeout) and args.timeout > 0):
        parser.error("--timeout must be a finite positive number")

    if args.validate_only:
        return run_validation(args.emit_json)

    try:
        selected_tiers = _parse_tiers(args.tier)
    except argparse.ArgumentTypeError as exc:
        parser.error(str(exc))
    all_cases = _load_cases()
    validation_errors: list[str] = []
    _validate_case_contracts(all_cases, validation_errors)
    if validation_errors:
        for err in validation_errors:
            print(f"Validation error: {err}", file=sys.stderr)
        return 1

    matched = [
        c
        for c in all_cases
        if c.tier in selected_tiers and _matches_filter(c, args.filter_pat)
    ]

    if args.list:
        if args.emit_json:
            print(
                json.dumps(
                    {
                        "schema": "informal.lp_e2e.list/1",
                        "total": len(matched),
                        "cases": [_case_to_dict(c) for c in matched],
                    },
                    indent=2,
                )
            )
        else:
            if not matched:
                print("0 test cases found.")
            else:
                for c in matched:
                    feats = ",".join(c.features)
                    print(f"[T{c.tier}] {c.id:<14} ({feats}) {c.title}")
                print(f"Total: {len(matched)} test case(s).")
        return 0

    if not matched:
        if args.emit_json:
            print(
                json.dumps(
                    {
                        "schema": "informal.lp_e2e.report/1",
                        "bin": None,
                        "engines": None,
                        "total": 0,
                        "passed": 0,
                        "failed": 0,
                        "duration_s": 0.0,
                        "tiers": {},
                        "results": [],
                    },
                    indent=2,
                )
            )
        else:
            print("No test cases discovered (0 cases matched). Nothing to run.")
        return 0

    try:
        informal_bin = resolve_informal_bin(REPO_ROOT, args.bin_path)
        engines_path = resolve_engines_toml(REPO_ROOT, args.engines_path)
    except FileNotFoundError as exc:
        if args.emit_json:
            print(
                json.dumps(
                    {
                        "schema": "informal.lp_e2e.report/1",
                        "error": str(exc),
                        "total": len(matched),
                        "passed": 0,
                        "failed": len(matched),
                        "results": [],
                    },
                    indent=2,
                )
            )
        else:
            print(f"Error: {exc}", file=sys.stderr)
        return 1

    child_env = build_tool_env(REPO_ROOT)
    log_stream = sys.stderr if args.emit_json else sys.stdout

    suite_t0 = time.monotonic()
    results: list[dict[str, Any]] = []
    tier_stats: dict[int, dict[str, Any]] = {}
    passed = 0
    failed = 0

    for case in matched:
        safe_id = re.sub(r"[^A-Za-z0-9_.-]+", "_", case.id)
        scale = (
            float(args.timeout) / max(float(case.timeout_s), 1.0)
            if args.timeout is not None
            else 1.0
        )

        tmp_ctx: tempfile.TemporaryDirectory[str] | None = None
        if args.keep_workdir:
            workdir = Path(tempfile.mkdtemp(prefix=f"lp_e2e_{safe_id}_"))
        else:
            tmp_ctx = tempfile.TemporaryDirectory(prefix=f"lp_e2e_{safe_id}_")
            workdir = Path(tmp_ctx.name)

        case_t0 = time.monotonic()
        status = "PASS"
        error_msg: str | None = None
        tb_text: str | None = None

        try:
            ctx = E2EContext(
                repo_root=REPO_ROOT,
                informal_bin=informal_bin,
                engines_path=engines_path,
                workdir=workdir,
                env=dict(child_env),
                fixtures_dir=FIXTURES_ROOT,
                timeout_scale=scale,
            )
            case.fn(ctx)
            passed += 1
        except Exception as exc:
            status = "FAIL"
            failed += 1
            error_msg = str(exc) or exc.__class__.__name__
            tb_text = traceback.format_exc()
        finally:
            if tmp_ctx is not None:
                tmp_ctx.cleanup()

        dur = time.monotonic() - case_t0
        t_entry = tier_stats.setdefault(
            case.tier, {"passed": 0, "failed": 0, "total": 0, "duration_s": 0.0}
        )
        t_entry["total"] += 1
        t_entry["duration_s"] += dur
        if status == "PASS":
            t_entry["passed"] += 1
        else:
            t_entry["failed"] += 1

        rec: dict[str, Any] = {
            "id": case.id,
            "tier": case.tier,
            "features": list(case.features),
            "title": case.title,
            "status": status,
            "duration_s": round(dur, 4),
        }
        if error_msg is not None:
            rec["error"] = error_msg
        if args.keep_workdir:
            rec["workdir"] = str(workdir)
        results.append(rec)

        feats = ",".join(case.features)
        if not args.emit_json or args.verbose:
            wd_note = f" [workdir={workdir}]" if args.keep_workdir else ""
            print(
                f"[{status}] T{case.tier} {case.id:<14} ({feats}) {dur:6.2f}s  {case.title}{wd_note}",
                file=log_stream,
            )
            if error_msg is not None:
                print(f"       {error_msg}", file=log_stream)
                if args.verbose and tb_text:
                    for line in tb_text.rstrip().splitlines():
                        print(f"       | {line}", file=log_stream)

    total_dur = time.monotonic() - suite_t0

    if args.emit_json:
        payload = {
            "schema": "informal.lp_e2e.report/1",
            "bin": str(informal_bin),
            "engines": str(engines_path),
            "total": len(results),
            "passed": passed,
            "failed": failed,
            "duration_s": round(total_dur, 4),
            "tiers": {
                str(t): {
                    "total": st["total"],
                    "passed": st["passed"],
                    "failed": st["failed"],
                    "duration_s": round(st["duration_s"], 4),
                }
                for t, st in sorted(tier_stats.items())
            },
            "results": results,
        }
        print(json.dumps(payload, indent=2))
    else:
        print("\n--- Tier Summary ---")
        for t in sorted(tier_stats):
            st = tier_stats[t]
            print(
                f"Tier {t}: {st['passed']}/{st['total']} passed "
                f"({st['failed']} failed, {st['duration_s']:.2f}s)"
            )
        print(
            f"Total: {passed}/{len(results)} passed, {failed} failed ({total_dur:.2f}s)"
        )

    return 0 if failed == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
