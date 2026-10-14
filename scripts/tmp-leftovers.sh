#!/usr/bin/env bash
# `cargo test --workspace` leaves nothing behind in the temp directory (730).
#
# /tmp on the coder host is a tmpfs, so every scratch directory a test leaks
# is RAM, and the host runs the suite all day. On 2026-10-14 it held 40 201
# entries older than two days and stood at 100 %; the OOM killer took two
# passes beside a gate that week.
#
# Shape: run the suite in the ordinary way, but with TMPDIR pointed at a fresh
# directory of its own. `std::env::temp_dir()` and the `tempfile` crate both
# honour TMPDIR, and so do the daemons and Python peers a test spawns, so every
# scratch path the suite builds lands there and nothing else does. Whatever is
# still in it once `cargo test` has exited is a leak, attributed by name, and a
# concurrent gate on the same host cannot put anything in it. That is what
# makes the verdict deterministic where a before/after diff of /tmp itself is
# not: another lane's run creates and deletes entries there the whole time.
#
# The /tmp diff is still printed, as information: a test that hardcodes
# "/tmp" bypasses TMPDIR, and the diff is where such a path would show up.
# It is not part of the verdict for the reason above.
#
# Exit 0 = the suite passed and left nothing in its TMPDIR.
# Exit 1 = it left entries (named), or the suite itself failed.
set -euo pipefail

cd "$(dirname "$0")/.."

# Short on purpose: a test that puts a Unix socket under temp_dir() has 108
# bytes for the whole path.
scratch=$(mktemp -d /tmp/tl.XXXXXX)
log=$(mktemp /tmp/tl-log.XXXXXX)
trap 'rm -rf "$scratch" "$log"' EXIT

list_tmp() {
    find /tmp -mindepth 1 -maxdepth 1 -user "$(id -u)" -printf '%f\n' 2>/dev/null | sort
}

before=$(list_tmp)

status=0
TMPDIR="$scratch" cargo test --workspace >"$log" 2>&1 || status=$?

after=$(list_tmp)
outside=$(comm -13 <(echo "$before") <(echo "$after") | grep -v -x -e "${scratch#/tmp/}" -e "${log#/tmp/}" || true)
leftovers=$(find "$scratch" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort)

if [ "$status" -ne 0 ]; then
    echo "tmp-leftovers: cargo test --workspace failed (exit $status); its output:" >&2
    cat "$log" >&2
    echo "tmp-leftovers: FAIL, the suite is red, so the leftover count below is not a verdict" >&2
fi

if [ -n "$outside" ]; then
    echo "tmp-leftovers: new in /tmp during the run (information only, any user process may have made these):"
    while IFS= read -r entry; do
        printf '  %s\n' "$entry"
    done <<<"$outside"
fi

if [ -n "$leftovers" ]; then
    count=$(echo "$leftovers" | wc -l)
    echo "tmp-leftovers: FAIL, the suite left $count entries in its TMPDIR:" >&2
    # Digits collapsed to N, so one leaking test is one line with a count.
    echo "$leftovers" | sed -E 's/[0-9]+/N/g' | sort | uniq -c | sort -rn >&2
    echo "tmp-leftovers: by name, with what each holds:" >&2
    while IFS= read -r entry; do
        if [ -d "$scratch/$entry" ]; then
            echo "  $entry/: $(find "$scratch/$entry" -mindepth 1 -maxdepth 2 -printf '%P ' | head -c 400)"
        else
            echo "  $entry"
        fi
    done <<<"$leftovers" >&2
    exit 1
fi

[ "$status" -eq 0 ] || exit 1
passed=$(awk '/^test result: ok\./ { binaries++; passed += $4 } END { printf "%d tests passed in %d test binaries", passed, binaries }' "$log")
echo "tmp-leftovers: ok, cargo test --workspace ($passed) left nothing in its TMPDIR"
