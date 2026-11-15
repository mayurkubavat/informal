#!/usr/bin/env python3
"""Fill a meson subproject's package cache from git when its download is blocked.

    python3 -I scripts/wrap_prefill.py <project dir> <wrap name> [<wrap name> ...]

For each [wrap-file] subproject whose archive is not in
subprojects/packagecache yet, it first tries the wrap's own source_url. If
that download fails (a network that allows git but not GitHub's archive
URLs), it clones the repository at the tag or commit the URL names and
writes the same archive with `git archive --prefix=<directory>/ | gzip -n`,
which for GitHub's archives gives the same bytes. Either way the archive's
SHA-256 must equal the wrap's source_hash, or nothing is kept: meson then
builds from exactly the source the project pinned. Used by
`scripts/tools.sh build bitwuzla` for CaDiCaL and mimalloc.
"""
import configparser
import hashlib
import os
import re
import subprocess
import sys
import tempfile


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def prefill(project, name):
    wrap = configparser.ConfigParser(interpolation=None)
    wrap.read(os.path.join(project, "subprojects", f"{name}.wrap"))
    w = wrap["wrap-file"]
    cache = os.path.join(project, "subprojects", "packagecache")
    os.makedirs(cache, exist_ok=True)
    dest = os.path.join(cache, w["source_filename"])
    want = w["source_hash"].strip()
    if os.path.exists(dest) and sha256(dest) == want:
        return f"{name}: in the package cache"
    tmp = dest + ".part"
    if subprocess.run(["curl", "-LsSf", "-o", tmp, w["source_url"]]).returncode == 0 and sha256(tmp) == want:
        os.replace(tmp, dest)
        return f"{name}: downloaded"
    if os.path.exists(tmp):
        os.remove(tmp)
    m = re.fullmatch(r"(https://github\.com/[^/]+/[^/]+)/archive/(?:refs/tags/)?(.+)\.tar\.gz", w["source_url"].strip())
    if not m:
        sys.exit(f"wrap_prefill: {name}: cannot download {w['source_url']} and it is not a GitHub archive")
    repo, ref = m.groups()
    with tempfile.TemporaryDirectory() as d:
        subprocess.run(["git", "init", "-q", d], check=True)
        subprocess.run(["git", "-C", d, "fetch", "-q", "--depth", "1", repo, ref], check=True)
        with open(tmp, "wb") as out:
            arch = subprocess.Popen(["git", "-C", d, "archive", "--format=tar",
                                     f"--prefix={w['directory'].strip()}/", "FETCH_HEAD"], stdout=subprocess.PIPE)
            subprocess.run(["gzip", "-n"], stdin=arch.stdout, stdout=out, check=True)
            arch.wait()
    if sha256(tmp) != want:
        os.remove(tmp)
        sys.exit(f"wrap_prefill: {name}: the archive made from {repo} at {ref} does not match the wrap's source_hash")
    os.replace(tmp, dest)
    return f"{name}: made from git ({repo} at {ref}), hash matches the wrap"


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__.split("\n\n")[1])
    for name in sys.argv[2:]:
        print("wrap_prefill:", prefill(sys.argv[1], name))


if __name__ == "__main__":
    main()
