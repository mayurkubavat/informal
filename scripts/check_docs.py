#!/usr/bin/env python3
"""Verify InFormal documentation integrity:
1. Zero broken relative file links across README.md, llms.txt, docs/**/*.md, examples/**/*.md.
2. Every page listed in `zensical.toml` `nav` exists under `docs/`.
3. Optional `--bin <informal>` runs `informal docgen --check` to verify zero drift
   in generated reference manuals (`docs/reference/*` and `llms.txt`).
4. When `zensical` is installed, runs `zensical build --clean --strict` in an
   isolated temporary directory to verify zero site build warnings or broken links.
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
SKIP_PREFIXES = ("http://", "https://", "mailto:", "#")


def iter_markdown_files() -> list[Path]:
    files: list[Path] = []
    for root_file in ("README.md", "llms.txt"):
        p = REPO / root_file
        if p.exists():
            files.append(p)
    for folder in ("docs", "examples"):
        base = REPO / folder
        if base.exists():
            files.extend(sorted(base.rglob("*.md")))
    return files


def strip_fenced_code(text: str) -> str:
    out: list[str] = []
    in_fence = False
    for line in text.splitlines():
        stripped = line.lstrip()
        if stripped.startswith("```") or stripped.startswith("~~~"):
            in_fence = not in_fence
            out.append("")
            continue
        out.append("" if in_fence else line)
    return "\n".join(out)


def check_links() -> list[str]:
    errors: list[str] = []
    for md in iter_markdown_files():
        text = strip_fenced_code(md.read_text(encoding="utf-8"))
        for lineno, line in enumerate(text.splitlines(), 1):
            for match in LINK_RE.finditer(line):
                raw = match.group(1).strip()
                if not raw or raw.startswith(SKIP_PREFIXES):
                    continue
                target = raw.split()[0].split("#", 1)[0]
                if not target:
                    continue
                resolved = (md.parent / target).resolve()
                if not resolved.exists():
                    rel_md = md.relative_to(REPO)
                    errors.append(f"{rel_md}:{lineno}: broken link `{raw}` -> {resolved}")
    return errors


def _walk_nav(node: object, out: list[str]) -> None:
    if isinstance(node, str):
        out.append(node)
    elif isinstance(node, list):
        for item in node:
            _walk_nav(item, out)
    elif isinstance(node, dict):
        for val in node.values():
            _walk_nav(val, out)


def check_zensical_nav() -> list[str]:
    cfg_path = REPO / "zensical.toml"
    if not cfg_path.exists():
        return ["zensical.toml is missing"]
    cfg = tomllib.loads(cfg_path.read_text(encoding="utf-8"))
    project = cfg.get("project", {})
    docs_dir = REPO / project.get("docs_dir", "docs")
    paths: list[str] = []
    _walk_nav(project.get("nav", []), paths)
    errors: list[str] = []
    for rel in paths:
        if not (docs_dir / rel).exists():
            errors.append(f"zensical.toml: missing nav target `{docs_dir.name}/{rel}`")
    return errors


def find_zensical() -> Path | None:
    found = shutil.which("zensical")
    if found:
        return Path(found)
    candidate = Path.home() / ".local" / "bin" / "zensical"
    return candidate if candidate.exists() else None


def check_zensical_build(zensical_bin: Path) -> list[str]:
    cfg_text = (REPO / "zensical.toml").read_text(encoding="utf-8")
    with tempfile.TemporaryDirectory(prefix="informal-zensical-") as tmp:
        tmp_path = Path(tmp)
        shutil.copytree(REPO / "docs", tmp_path / "docs")
        tmp_cfg = tmp_path / "zensical.toml"
        tmp_cfg.write_text(cfg_text, encoding="utf-8")
        proc = subprocess.run(
            [str(zensical_bin), "build", "--clean", "--strict", "-f", str(tmp_cfg)],
            cwd=tmp_path,
            capture_output=True,
            text=True,
        )
        if proc.returncode != 0:
            msg = (proc.stdout + "\n" + proc.stderr).strip()
            return [f"zensical build --clean --strict failed:\n{msg}"]
    return []


def check_docgen(bin_path: Path) -> list[str]:
    proc = subprocess.run(
        [str(bin_path), "docgen", "--check"],
        cwd=REPO,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        msg = (proc.stderr or proc.stdout).strip()
        return [f"informal docgen --check failed:\n{msg}"]
    return []


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--bin",
        type=Path,
        default=None,
        help="Path to informal binary to run `informal docgen --check`",
    )
    args = ap.parse_args()

    errors: list[str] = []
    md_files = iter_markdown_files()
    errors.extend(check_links())
    errors.extend(check_zensical_nav())
    if args.bin is not None:
        errors.extend(check_docgen(args.bin.resolve()))
    zensical_bin = find_zensical()
    if zensical_bin is not None:
        errors.extend(check_zensical_build(zensical_bin))

    if errors:
        print("check_docs: FAILED", file=sys.stderr)
        for err in errors:
            print(f"  - {err}", file=sys.stderr)
        return 1

    extras: list[str] = []
    if args.bin is not None:
        extras.append("docgen --check")
    if zensical_bin is not None:
        extras.append("zensical --strict")
    suffix = (" + " + " + ".join(extras)) if extras else ""
    print(f"check_docs: OK ({len(md_files)} markdown files + zensical.toml{suffix})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
