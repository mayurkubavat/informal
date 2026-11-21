#!/usr/bin/env python3
"""Verify InFormal documentation integrity:
1. Zero broken relative file links across README.md, llms.txt, docs/**/*.md, examples/**/*.md.
2. Every file listed under `nav:` in mkdocs.yml exists under docs/.
3. Optional `--bin <informal>` runs `informal docgen --check` to verify zero drift
   in generated reference manuals (`docs/reference/*` and `llms.txt`).
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
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
                # Strip optional title (`path "title"`) and `#anchor`
                target = raw.split()[0].split("#", 1)[0]
                if not target:
                    continue
                resolved = (md.parent / target).resolve()
                if not resolved.exists():
                    rel_md = md.relative_to(REPO)
                    errors.append(f"{rel_md}:{lineno}: broken link `{raw}` -> {resolved}")
    return errors


def check_mkdocs_nav() -> list[str]:
    mkdocs = REPO / "mkdocs.yml"
    if not mkdocs.exists():
        return ["mkdocs.yml is missing"]
    errors: list[str] = []
    in_nav = False
    for lineno, line in enumerate(mkdocs.read_text(encoding="utf-8").splitlines(), 1):
        if line.startswith("nav:"):
            in_nav = True
            continue
        if in_nav and line and not line[0].isspace():
            in_nav = False
        if not in_nav:
            continue
        m = re.search(r":\s+([A-Za-z0-9_./-]+\.md)\s*$", line)
        if m:
            rel = m.group(1)
            target = (REPO / rel) if rel.startswith("examples/") else (REPO / "docs" / rel)
            if not target.exists():
                errors.append(f"mkdocs.yml:{lineno}: missing nav target `{rel}`")
    return errors


def find_mkdocs() -> Path | None:
    found = shutil.which("mkdocs")
    if found:
        return Path(found)
    candidate = Path.home() / ".local" / "bin" / "mkdocs"
    return candidate if candidate.exists() else None


def check_mkdocs_build(mkdocs_bin: Path) -> list[str]:
    env = os.environ.copy()
    env["NO_MKDOCS_2_WARNING"] = "1"
    with tempfile.TemporaryDirectory(prefix="informal-mkdocs-") as tmp:
        proc = subprocess.run(
            [str(mkdocs_bin), "build", "--strict", "-d", tmp],
            cwd=REPO,
            env=env,
            capture_output=True,
            text=True,
        )
        if proc.returncode != 0:
            msg = (proc.stderr or proc.stdout).strip()
            return [f"mkdocs build --strict failed:\n{msg}"]
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
    errors.extend(check_mkdocs_nav())
    if args.bin is not None:
        errors.extend(check_docgen(args.bin.resolve()))
    mkdocs_bin = find_mkdocs()
    if mkdocs_bin is not None:
        errors.extend(check_mkdocs_build(mkdocs_bin))

    if errors:
        print("check_docs: FAILED", file=sys.stderr)
        for err in errors:
            print(f"  - {err}", file=sys.stderr)
        return 1

    extras: list[str] = []
    if args.bin is not None:
        extras.append("docgen --check")
    if mkdocs_bin is not None:
        extras.append("mkdocs --strict")
    suffix = (" + " + " + ".join(extras)) if extras else ""
    print(f"check_docs: OK ({len(md_files)} markdown files + mkdocs.yml{suffix})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
