#!/usr/bin/env python3
"""Group the red list of `just lnmsg-core-no-std` by cause.

The gate checks lnmsg's library without the `std` feature for the firmware
target and fails until the core holds no std use (the plan on
docs/src/concepts/lnmsg-architecture.md, "Plan: the lnmsg core without std").
Each pass of that plan is accepted on how the list shrinks, and rustc prints
the list as one error per use, so the passes compare a table of causes rather
than raw logs:

    just lnmsg-core-no-std > /tmp/nostd.log 2>&1
    python3 scripts/lnmsg-no-std-groups.py /tmp/nostd.log

Each group names its count, the files it is in and, except for the alloc
prelude (which pass 1 cleared), every site with its source line.
"""

import collections
import re
import sys

NAMED = re.compile(r"cannot find (?:macro|type|value|module or crate) `(\w+)`")
LOCATION = re.compile(r"\s*--> (\S+):(\d+):(\d+)")
SOURCE = re.compile(r"\s*(\d+)\s*\|\s?(.*)")
ALLOC_PRELUDE = ("format", "String", "Vec", "Box", "ToString", "ToOwned")


def cause(line, src):
    match = NAMED.search(line)
    name = match.group(1) if match else ("leviculum_std" if "leviculum_std" in line else line)
    if name in ALLOC_PRELUDE:
        return "alloc prelude (%s)" % name
    if name == "std":
        path = re.search(r"std::(\w+)", src or "")
        return "std::" + (path.group(1) if path else "?")
    if name == "eprintln":
        return "eprintln! (process stderr)"
    if name == "OsRng":
        return "rand_core::OsRng (getrandom)"
    return name


def errors(lines):
    for i, line in enumerate(lines):
        if not line.startswith("error") or "could not compile" in line or "Recipe" in line:
            continue
        loc = src = None
        for follow in lines[i + 1 : i + 8]:
            found = LOCATION.match(follow)
            if found:
                loc = found.groups()
            shown = SOURCE.match(follow)
            if shown and loc and shown.group(1) == loc[1]:
                src = shown.group(2).strip()
                break
        yield cause(line, src), loc, src, line


def main():
    if len(sys.argv) != 2:
        sys.exit("usage: lnmsg-no-std-groups.py <log of just lnmsg-core-no-std>")
    with open(sys.argv[1], encoding="utf-8") as log:
        found = list(errors(log.read().splitlines()))
    groups = collections.defaultdict(list)
    for entry in found:
        groups[entry[0]].append(entry)
    print("total %d errors\n" % len(found))
    for group, entries in sorted(groups.items(), key=lambda kv: -len(kv[1])):
        files = collections.Counter(e[1][0].split("/")[-1] if e[1] else "?" for e in entries)
        print("== %s: %d  (%s)" % (group, len(entries), ", ".join("%s %d" % kv for kv in files.most_common())))
        if not group.startswith("alloc"):
            for _, loc, src, line in entries:
                print("   %s:%s:%s  %s" % (loc[0], loc[1], loc[2], src) if loc else "   " + line)
        print()


if __name__ == "__main__":
    main()
