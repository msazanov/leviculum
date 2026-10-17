#!/usr/bin/env python3
"""THIRD-PARTY.md names every submodule and every vendored crate.

The repository carries other people's code under five licences (finding
PUB-0034): four submodules under `reference/`, among them a CC BY-NC-ND and
a GPL-3.0 tree, and patched crates under `vendor/`. THIRD-PARTY.md is the
one place that says which licence each carries and whether it reaches a
binary. It is written by hand, so it rots the day someone adds a submodule
or a `[patch.crates-io]` path and does not touch it. This gate turns that
day into a red guard.

What it checks: every `path = ...` in `.gitmodules`, and every `path` of a
`[patch.crates-io]` entry in any tracked Cargo.toml outside `reference/`
and `vendor/` (resolved to a repository-relative path), appears in
THIRD-PARTY.md as a backticked literal. What it cannot check: that the
licence and role written beside the path are true. That stays a review.

Exit 0 = every entry is named. Exit 1 = one is not, or the checker's own
fixtures did not behave.

Usage:
  python3 scripts/check-third-party.py
"""

import os
import posixpath
import re
import subprocess
import sys
import tomllib

REPO_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOC = "THIRD-PARTY.md"

GITMODULES_PATH_RE = re.compile(r"^\s*path\s*=\s*(\S+)\s*$", re.MULTILINE)


def gitmodule_paths(text):
    return GITMODULES_PATH_RE.findall(text)


def patch_paths(manifest_rel, text):
    """Repository-relative paths of the `[patch.crates-io]` entries of one
    manifest. A registry or git patch has no path and names no vendored
    tree, so it is skipped."""
    data = tomllib.loads(text)
    entries = data.get("patch", {}).get("crates-io", {})
    base = posixpath.dirname(manifest_rel)
    out = []
    for spec in entries.values():
        if isinstance(spec, dict) and "path" in spec:
            out.append(posixpath.normpath(posixpath.join(base, spec["path"])))
    return out


def unnamed(required, doc_text):
    return [p for p in required if f"`{p}`" not in doc_text]


def canary():
    """Fixtures first: a checker that stops seeing entries is green forever."""
    mods = gitmodule_paths('[submodule "x"]\n\tpath = reference/X\n\turl = u\n')
    if mods != ["reference/X"]:
        return f"gitmodules parse returned {mods!r}"
    patches = patch_paths(
        "fw/Cargo.toml",
        '[patch.crates-io]\na = { path = "../vendor/a" }\nb = { git = "g" }\n',
    )
    if patches != ["vendor/a"]:
        return f"patch parse returned {patches!r}"
    if unnamed(["vendor/a"], "| `vendor/a` | MIT |") != []:
        return "a named path was reported missing"
    if unnamed(["vendor/a"], "| vendor/a | MIT |") != ["vendor/a"]:
        return "an unnamed path was not reported"
    return None


def main():
    err = canary()
    if err:
        print(f"check-third-party: CANARY FAILED -- {err}", file=sys.stderr)
        return 1

    os.chdir(REPO_DIR)
    try:
        with open(DOC, encoding="utf-8") as f:
            doc_text = f.read()
    except OSError as e:
        print(f"check-third-party: cannot read {DOC}: {e}", file=sys.stderr)
        return 1

    required = []
    with open(".gitmodules", encoding="utf-8") as f:
        required += [(p, ".gitmodules") for p in gitmodule_paths(f.read())]

    tracked = subprocess.run(
        ["git", "ls-files", "--", "*Cargo.toml"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.split()
    for manifest in tracked:
        if manifest.startswith(("reference/", "vendor/")):
            continue
        with open(manifest, "rb") as f:
            text = f.read().decode("utf-8")
        required += [(p, manifest) for p in patch_paths(manifest, text)]

    if not required:
        print("check-third-party: found no submodule and no patch entry; "
              "the scan is broken", file=sys.stderr)
        return 1

    missing = [(p, src) for p, src in required
               if unnamed([p], doc_text)]
    if missing:
        print("check-third-party: FAILED", file=sys.stderr)
        for p, src in missing:
            print(f"  {p} (from {src}) is not named in {DOC}.", file=sys.stderr)
        print(f"  Add a row with its licence, role and pin to {DOC}.",
              file=sys.stderr)
        return 1

    print(f"check-third-party: OK -- {len(required)} submodule and vendored "
          f"paths named in {DOC}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
