#!/usr/bin/env bash
# `just tmp-leftovers` must not let the sccache server start inside the
# scratch TMPDIR it deletes (987).
#
# The script points TMPDIR at a fresh directory for the suite and removes it on
# exit. sccache's client starts the server on first use, with the client's
# environment, so a server born during the suite would make its per-compile
# temp dirs in that directory and fail every later compile on the host once it
# is gone (985). The script therefore starts the server first, under the host's
# own TMPDIR.
#
# Asserted against a stub `cargo` and a stub `sccache` on PATH, so no build
# runs and the test costs about a second. The stub cargo makes one wrapper
# call, as the first compile of a real suite would; the stub sccache records
# the TMPDIR of whichever call brought its "server" up. Three cases:
#
#   1. no server yet   -- the server comes up under the caller's TMPDIR,
#                         never under the script's /tmp/tl.* scratch
#   2. server running  -- the refused start is not a failure, and the
#                         running server is the one the suite uses
#   3. no sccache      -- RUSTC_WRAPPER unset, the stub is never called
#
# Positive control, i.e. proof the test can fail: point it at the script from
# before 987 and case 1 must go red, naming the scratch path.
#
#   git show a3d7958f:scripts/tmp-leftovers.sh > /tmp/old-tl.sh
#   TMP_LEFTOVERS_SCRIPT=/tmp/old-tl.sh bash scripts/test-tmp-leftovers.sh
#
# Usage: bash scripts/test-tmp-leftovers.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${TMP_LEFTOVERS_SCRIPT:-$ROOT/scripts/tmp-leftovers.sh}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin" "$WORK/host"

# The server is a state file: present means running. `--start-server` against
# a running server answers as sccache 0.10 does, "Address in use", exit 2.
cat > "$WORK/bin/sccache" <<'STUB'
#!/bin/sh
echo "call $*" >> "$STUB_DIR/calls"
if [ -e "$STUB_DIR/running" ]; then
    if [ "${1:-}" = --start-server ]; then
        echo "sccache: error: Server startup failed: Address in use" >&2
        exit 2
    fi
    exit 0
fi
echo "${TMPDIR:-unset}" > "$STUB_DIR/started-under"
touch "$STUB_DIR/running"
STUB

cat > "$WORK/bin/cargo" <<'STUB'
#!/bin/sh
if [ -n "${RUSTC_WRAPPER:-}" ]; then
    "$RUSTC_WRAPPER" rustc --version || exit 1
fi
echo "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"
STUB
chmod +x "$WORK/bin/sccache" "$WORK/bin/cargo"

failures=0
fail() {
    echo "FAIL: $*"
    failures=$((failures + 1))
}

# run <case> [env assignments...]: one fresh stub state, the script under
# PATH with the stubs first and TMPDIR at a directory the test owns.
run() {
    local case=$1
    shift
    export STUB_DIR="$WORK/$case"
    mkdir -p "$STUB_DIR"
    env "$@" PATH="$WORK/bin:$PATH" TMPDIR="$WORK/host" \
        bash "$SCRIPT" >"$STUB_DIR/out" 2>&1
}

# 1. no server yet
if ! run cold RUSTC_WRAPPER="$WORK/bin/sccache"; then
    fail "cold: the script failed: $(cat "$WORK/cold/out")"
fi
started=$(cat "$WORK/cold/started-under" 2>/dev/null || echo "never started")
if [ "$started" != "$WORK/host" ]; then
    fail "cold: the sccache server started under TMPDIR=$started, not the caller's $WORK/host"
fi

# 2. server running
mkdir -p "$WORK/warm"
touch "$WORK/warm/running"
if ! run warm RUSTC_WRAPPER="$WORK/bin/sccache"; then
    fail "warm: a refused --start-server failed the script: $(cat "$WORK/warm/out")"
fi
if [ -e "$WORK/warm/started-under" ]; then
    fail "warm: a second server came up under $(cat "$WORK/warm/started-under")"
fi
if ! grep -q -x 'call rustc --version' "$WORK/warm/calls" 2>/dev/null; then
    fail "warm: the suite's compile never reached the running server"
fi

# 3. no sccache
if ! run none -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER; then
    fail "none: the script failed: $(cat "$WORK/none/out")"
fi
if [ -e "$WORK/none/calls" ]; then
    fail "none: sccache was called without being the wrapper: $(cat "$WORK/none/calls")"
fi

if [ "$failures" -ne 0 ]; then
    echo "test-tmp-leftovers: $failures failure(s)"
    exit 1
fi
echo "test-tmp-leftovers: ok, the sccache server starts outside the scratch TMPDIR (3 cases)"
