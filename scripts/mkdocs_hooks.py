"""MkDocs build hook (`mkdocs.yml: hooks:`) keeping canonical Markdown files
100% standard GitHub/IDE-relative while rendering `examples/**/*.md` directly
into the site and rewriting repo-root file links cleanly for `--strict` builds.
"""

from __future__ import annotations

import re
from pathlib import Path
from mkdocs.structure.files import File

REPO = Path(__file__).resolve().parent.parent
GITHUB_BLOB = "https://github.com/mayurkubavat/informal/blob/main/"
LINK_RE = re.compile(r"(!?\[[^\]]*\])\(([^)]+)\)")


def on_files(files, config):
    """Mount `examples/**/*.md` into the MkDocs virtual file tree under `examples/`."""
    examples_dir = REPO / "examples"
    if not examples_dir.exists():
        return files
    for md in sorted(examples_dir.rglob("*.md")):
        rel = md.relative_to(REPO).as_posix()
        files.append(
            File(
                path=rel,
                src_dir=str(REPO),
                dest_dir=config["site_dir"],
                use_directory_urls=config["use_directory_urls"],
            )
        )
    return files


def on_page_markdown(markdown, page, config, files):
    """Rewrite links that climb out of `docs/` so `--strict` builds cleanly:
    - Links targeting `examples/**/*.md` resolve to the mounted virtual pages.
    - Links targeting repo-root non-doc files (`config/*`, `plugin/*`, `ui/*`,
      `CONTRIBUTING.md`, `tools.lock`, `llms.txt`, `Makefile`, `scripts/*`)
      resolve to GitHub blob URLs in the HTML site.
    """
    src_path = Path(page.file.abs_src_path)
    docs_dir = (REPO / config["docs_dir"]).resolve()
    page_virtual = Path(page.file.src_path)

    def repl(match: re.Match[str]) -> str:
        label, raw = match.group(1), match.group(2).strip()
        if not raw or raw.startswith(("http://", "https://", "mailto:", "#")):
            return match.group(0)
        parts = raw.split(None, 1)
        url_part = parts[0]
        title_suffix = f" {parts[1]}" if len(parts) > 1 else ""
        if "#" in url_part:
            target_str, frag = url_part.split("#", 1)
            frag_suffix = f"#{frag}"
        else:
            target_str, frag_suffix = url_part, ""
        if not target_str:
            return match.group(0)

        resolved = (src_path.parent / target_str).resolve()
        # Already inside docs/ (and page itself is inside docs/) -> leave as-is
        try:
            rel_to_docs = resolved.relative_to(docs_dir)
            if src_path.is_relative_to(docs_dir):
                return match.group(0)
            # Page is mounted from examples/ linking into docs/
            rel = _relpath(Path(rel_to_docs.as_posix()), page_virtual.parent)
            return f"{label}({rel}{frag_suffix}{title_suffix})"
        except ValueError:
            pass

        # Target is inside examples/ and is a Markdown page -> link to mounted page
        try:
            rel_to_repo = resolved.relative_to(REPO)
        except ValueError:
            return match.group(0)

        if rel_to_repo.parts and rel_to_repo.parts[0] == "examples" and resolved.suffix == ".md":
            rel = _relpath(Path(rel_to_repo.as_posix()), page_virtual.parent)
            return f"{label}({rel}{frag_suffix}{title_suffix})"

        # Other repo files outside docs/ (config/, plugin/, ui/, CONTRIBUTING.md, Makefile, llms.txt, ...)
        return f"{label}({GITHUB_BLOB}{rel_to_repo.as_posix()}{frag_suffix}{title_suffix})"

    return LINK_RE.sub(repl, markdown)


def _relpath(target: Path, start: Path) -> str:
    import os

    return Path(os.path.relpath(target.as_posix(), start.as_posix())).as_posix()
