#!/usr/bin/env bash
# Fixture test for scripts/rig-cell.sh's reader restart.
#
# After a cell every bound board has been reset, and its by-id link is often
# missing for a few seconds. The runner it replaces skipped such a board
# silently and left it unrecorded until somebody noticed. Two claims, driven
# against a fake by-id directory and a fake rig.toml:
#
#   1. a link that appears while readers_back waits gets its reader, and the
#      log says after how long;
#   2. a link that never appears is named as NOT back, and no reader starts.
#
# The reader is a stub that records its argv: what is under test is the
# runner's decision, and the real reader has its own test
# (scripts/test-debug-witness.sh, --attach-now included).
#
# Positive control, built in: claim 1 is run a second time against a copy of
# rig-cell.sh whose wait is cut to zero, and has to FAIL there. A test that
# passed both ways would not be testing the wait.
#
# Usage: bash scripts/test-rig-cell.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RIG_CELL="$SCRIPT_DIR/rig-cell.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PASS=0
FAIL=0
ok() {
    PASS=$((PASS + 1))
    printf 'ok    %s\n' "$1"
}
bad() {
    FAIL=$((FAIL + 1))
    printf 'FAIL  %s\n' "$1"
}

# Two LNodes and an RNode; the RNode must never get a reader.
cat >"$WORK/rig.toml" <<'EOF'
[devices.board-late]
firmware = "lnode"
serial = "AAAA000000000001"

[devices.board-gone]
firmware = "lnode"
serial = "BBBB000000000002"

[devices.some-rnode]
firmware = "rnode"
serial = "5896000022"
EOF

cat >"$WORK/stub-reader.py" <<'PY'
import os, sys
with open(os.environ["STUB_CALLS"], "a") as fh:
    fh.write(" ".join(sys.argv[1:]) + "\n")
PY

# One readers_back pass against a fresh bench. Prints the runner log.
# Args: $1 = rig-cell.sh to source, $2 = case dir, $3 = wait (s)
run_readers_back() {
    local script="$1" dir="$2" wait="$3"
    mkdir -p "$dir/by-id" "$dir/rig-run"
    (
        export LEV_RIG_RUN="$dir/rig-run" LEV_RIG_TOML="$WORK/rig.toml" \
            LEV_RIG_BY_ID="$dir/by-id" LEV_RIG_RUNLOG="$dir/runner.log" \
            LEV_RIG_READER="$WORK/stub-reader.py" LEV_RIG_READER_WAIT="$wait" \
            STUB_CALLS="$dir/calls"
        # shellcheck source=scripts/rig-cell.sh
        . "$script"
        readers_back >/dev/null
    )
}

# Claim 1 against <script>: board-late's link appears two seconds into the
# wait. Returns 0 iff the reader started on it and the log says so.
# Args: $1 = script, $2 = case dir
late_link_case() {
    local script="$1" dir="$2" link
    link="$dir/by-id/usb-leviculum_leviculum_T114_AAAA000000000001-if00"
    mkdir -p "$dir/by-id"
    (sleep 2 && ln -s /dev/null "$link") &
    run_readers_back "$script" "$dir" 4
    wait
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        [ -s "$dir/calls" ] && break
        sleep 0.1
    done
    grep -qE 'reader board-late back after [2-4] s' "$dir/runner.log" || return 1
    grep -qF -- "--port $link" "$dir/calls" 2>/dev/null || return 1
    grep -qF -- "--attach-now --capture 31536000" "$dir/calls" || return 1
    return 0
}

if late_link_case "$RIG_CELL" "$WORK/late"; then
    ok "a link that comes back during the wait gets its reader"
else
    bad "a link that comes back during the wait gets its reader"
    cat "$WORK/late/runner.log" "$WORK/late/calls" 2>/dev/null | sed 's/^/        /'
fi

if grep -qF 'reader board-gone NOT back: link missing after 4 s' "$WORK/late/runner.log"; then
    ok "a link that never comes back is named NOT back"
else
    bad "a link that never comes back is named NOT back"
    sed 's/^/        /' "$WORK/late/runner.log"
fi

if grep -qF 'BBBB000000000002' "$WORK/late/calls" 2>/dev/null; then
    bad "...and gets no reader"
else
    ok "...and gets no reader"
fi

if grep -qE 'some-rnode|5896000022' "$WORK/late/runner.log" "$WORK/late/calls" 2>/dev/null; then
    bad "an RNode in rig.toml is never given a reader"
else
    ok "an RNode in rig.toml is never given a reader"
fi

# The control: the same case with the wait cut to zero must go red.
# shellcheck disable=SC2016  # the pattern is rig-cell.sh's literal text
sed 's/if \[ "\$waited" -ge "\$LEV_RIG_READER_WAIT" \]; then/if true; then/' \
    "$RIG_CELL" >"$WORK/rig-cell-nowait.sh"
if cmp -s "$RIG_CELL" "$WORK/rig-cell-nowait.sh"; then
    bad "the control could not cut the wait: rig-cell.sh's wait line moved"
elif late_link_case "$WORK/rig-cell-nowait.sh" "$WORK/control"; then
    bad "control: with the wait cut to zero the late-link case must fail"
else
    ok "control: with the wait cut to zero the late-link case fails"
fi

printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
