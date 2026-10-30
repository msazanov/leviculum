#!/usr/bin/env python3
"""The messenger is Loquax in prose; only the command is `loquax`.

Lew, 2026-10-30 (#232): the official name is Loquax with a capital L. What
stays lower case is what a machine reads: the binary, the package, the crate,
paths, config keys, environment variables, file names. In the docs those are
code (a code span or a code block), a path (`loquax/src`, `~/.local/share/
loquax`), a hyphenated name (`loquax-ui.md`), or, on a manual page, the bold
command of the man convention (`**loquax send**`). Everything else is the name
of the product and starts with a capital.

Nothing held that apart before: the crate was named lnmsg, renamed loquax, and
the docs followed the crate name in prose. This gate reads every
`docs/src/**/*.md` and refuses a bare lower-case `loquax` outside those forms,
so the next page written in a hurry is told at the guard step, not by Lew.

What counts as not prose, after fenced code blocks, code spans, link targets
and HTML comments are blanked out:
  * a neighbour that makes it part of a path or an identifier: `/`, `.`, `-`,
    `_`, `~`, `$`, `<`, `@` before; `/`, `-`, `_`, `=`, `::`, `(1)` or a `.`
    followed by a word character after;
  * on a page under `docs/src/man/`, a bold span (the command, by the man
    convention) and the NAME line `loquax -- ...`.

Exit 0 = no prose `loquax` left (and the self-test behaved).
Exit 1 = a finding, printed as path:line: text, or a failed self-test.

Usage:
  python3 scripts/check-loquax-name.py [--root DIR]
"""

import argparse
import re
import sys
from pathlib import Path

WORD = re.compile(r"loquax")
BEFORE_OK = set("/.-_~$<@")
FENCE = re.compile(r"^\s*(```+|~~~+)")
SPAN = re.compile(r"(`+)((?:(?!\n[ \t]*\n)[^`]|(?!\1)`)+?)\1")
COMMENT = re.compile(r"<!--[\s\S]*?-->")
LINK_TARGET = re.compile(r"\]\([^)]*\)")
URL = re.compile(r"https?://\S+")
BOLD = re.compile(r"\*\*[^*]+?\*\*")
MAN_NAME = re.compile(r"^loquax -- ")


def is_prose(line: str, start: int) -> bool:
    """Whether the `loquax` at `start` stands as a word of prose."""
    end = start + len("loquax")
    before = line[start - 1] if start > 0 else " "
    after = line[end:]
    if before.isalnum() or before in BEFORE_OK:
        return False
    if after[:1].isalnum() or after[:1] in ("/", "-", "_", "="):
        return False
    if after.startswith("::") or after.startswith("(1)"):
        return False
    if after[:1] == "." and after[1:2].isalnum():
        return False
    return True


def strip(text: str, man: bool) -> list[str]:
    """The page's lines with everything that is not prose blanked out.

    Fenced blocks go first, line by line; comments and code spans then over
    the whole remaining text, because both may break across a line (a code
    span only within its paragraph). Blanking keeps every offset, so a line
    number stays the page's line number.
    """
    lines = text.splitlines()
    fenced = None
    for i, raw in enumerate(lines):
        fence = FENCE.match(raw)
        if fenced is not None:
            if fence and fence.group(1)[0] == fenced[0] and len(fence.group(1)) >= len(fenced):
                fenced = None
            lines[i] = ""
        elif fence:
            fenced = fence.group(1)
            lines[i] = ""
    body = "\n".join(lines)
    keep_newlines = lambda m: re.sub(r"[^\n]", " ", m.group(0))
    body = COMMENT.sub(keep_newlines, body)
    body = SPAN.sub(keep_newlines, body)
    body = LINK_TARGET.sub(keep_newlines, body)
    body = URL.sub(keep_newlines, body)
    if man:
        body = BOLD.sub(keep_newlines, body)
    return body.split("\n")


def prose_hits(text: str, man: bool) -> list[tuple[int, int]]:
    """(line index, column) of every prose `loquax` on the page."""
    hits = []
    for i, line in enumerate(strip(text, man)):
        if man and MAN_NAME.match(line):
            continue
        hits.extend((i, m.start()) for m in WORD.finditer(line) if is_prose(line, m.start()))
    return hits


def findings(text: str, man: bool) -> list[tuple[int, str]]:
    raw = text.splitlines()
    lines = sorted({i for i, _ in prose_hits(text, man)})
    return [(i + 1, raw[i].strip()) for i in lines]


def self_test() -> list[str]:
    """The positive and negative controls, on text, before the real run."""
    cases = [
        ("loquax is the messenger.", False, 1),
        ("The window says loquax.", False, 1),
        ("loquax's store", False, 1),
        ("Run `loquax send` from cron.", False, 0),
        ("Run `loquax\nsend` from cron, then loquax is idle.", False, 1),
        ("A `` `loquax` `` span.", False, 0),
        ("```\nloquax send x\n```", False, 0),
        ("See [the page](concepts/loquax-ui.md).", False, 0),
        ("Under ~/.local/share/loquax/ and loquax-ui.md.", False, 0),
        ("Set LOQUAX_HOME or loquax_verification; read loquax(1).", False, 0),
        ("<!-- loquax is\nstill loquax -->", False, 0),
        ("Loquax is the messenger.", False, 0),
        ("**loquax send** queues one message.", True, 0),
        ("**loquax send** queues one message.", False, 1),
        ("loquax -- terminal LXMF messenger", True, 0),
        ("**send**: loquax queues it.", True, 1),
    ]
    failed = []
    for text, man, want in cases:
        got = len(findings(text, man))
        if got != want:
            failed.append(f"self-test: {text!r} (man={man}) gave {got} findings, want {want}")
    return failed


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=Path(__file__).resolve().parent.parent, type=Path)
    args = parser.parse_args()

    failed = self_test()
    for line in failed:
        print(line, file=sys.stderr)
    if failed:
        return 1

    docs = args.root / "docs" / "src"
    bad = 0
    for path in sorted(docs.rglob("*.md")):
        man = "man" in path.relative_to(docs).parts[:1]
        for number, text in findings(path.read_text(encoding="utf-8"), man):
            print(f"{path.relative_to(args.root)}:{number}: {text}")
            bad += 1
    if bad:
        print(
            f"loquax-name: {bad} line(s) name the messenger `loquax` in prose; "
            "the name is Loquax, the command `loquax` goes in a code span",
            file=sys.stderr,
        )
        return 1
    print("loquax-name: ok, Loquax in prose, `loquax` only as code, path or command")
    return 0


if __name__ == "__main__":
    sys.exit(main())
