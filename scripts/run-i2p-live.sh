#!/usr/bin/env bash
# Run the two #[ignore]d live I2P tests against a private i2pd (T-0009).
#
# leviculum-std/src/interfaces/i2p/i2pd_live.rs holds the only coverage of
# I2PInterface against a real SAM bridge: that i2pd accepts our exact SESSION
# CREATE dialect, and that an HDLC frame crosses two live sessions over real
# tunnels. The mock-SAM tests in tier 0 reply with whatever they were told to,
# so a SAM reply i2pd changes is invisible to them. Both live tests wanted a
# router nobody started, and so ran nowhere.
#
# This starts one: a fresh i2pd in a temporary data directory, with its own
# SAM port and transport port and every other service off, so a system i2pd
# on the default ports is neither used nor disturbed and every run starts
# from the same cold state (reseed, then tunnel build). It waits for the SAM
# port, runs the two tests with LEVICULUM_I2P_SAM pointed at it, and stops
# the router on every exit path. setpriv --pdeathsig ties the router to this
# script, so a SIGKILLed run does not leave a router behind.
#
# Needs the network: a fresh router reseeds over HTTPS and builds tunnels
# through the live I2P network. Not a tier: the reviewer runs it weekly
# beside `emulated-corpus`.
#
# Usage: scripts/run-i2p-live.sh
# Exit status is cargo test's; 1 if the router could not be brought up.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Bounds. SAM_UP is how long i2pd may take to open its SAM port (it does so
# before reseeding, in about a second). TEST_BUDGET bounds the cargo run,
# whose slow test allows itself 240 s to connect once the sessions exist; a
# cold router spends most of its time before that, in SESSION CREATE, which
# answers only once the session's tunnels are built.
SAM_UP_S=60
TEST_BUDGET_S=900

command -v i2pd >/dev/null || { echo "[i2p-live] i2pd not installed (sudo apt install i2pd)" >&2; exit 1; }
command -v setpriv >/dev/null || { echo "[i2p-live] setpriv not found (util-linux)" >&2; exit 1; }

free_port() {
    python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'
}

say() { echo "[i2p-live] $*"; }

# Build first, so the router's clock starts with the tests and the measured
# wall is the router's, not the compiler's.
say "building the leviculum-std lib tests"
cargo test -p leviculum-std --lib --no-run

# Under ~/.i2pd and not under $TMPDIR: Debian's AppArmor profile for
# /usr/bin/i2pd (/etc/apparmor.d/usr.bin.i2pd) lets the router write only
# /var/lib/i2pd and the invoking user's ~/.i2pd, and a data directory in /tmp
# aborts it at startup with "cannot create directory: Permission denied".
mkdir -p "$HOME/.i2pd"
DATA="$(mktemp -d "$HOME/.i2pd/i2p-live.XXXXXX")"
SAM_PORT="$(free_port)"
NET_PORT="$(free_port)"
pid=""
# shellcheck disable=SC2317  # reached through the EXIT trap below
cleanup() {
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
        # SIGTERM stops i2pd at once; SIGINT would wait out its transit
        # tunnels for up to ten minutes.
        kill -TERM "$pid" 2>/dev/null || true
        for _ in $(seq 1 50); do kill -0 "$pid" 2>/dev/null || break; sleep 0.2; done
        kill -KILL "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
    rm -rf "$DATA"
}
trap cleanup EXIT

: >"$DATA/i2pd.conf"
: >"$DATA/tunnels.conf"
mkdir -p "$DATA/tunnels.d"
CERTS=()
[ -d /usr/share/i2pd/certificates ] && CERTS=(--certsdir=/usr/share/i2pd/certificates)

say "starting a private i2pd: SAM 127.0.0.1:${SAM_PORT}, transport port ${NET_PORT}, data ${DATA}"
start=$(date +%s)
setpriv --pdeathsig TERM -- i2pd \
    --datadir="$DATA" --conf="$DATA/i2pd.conf" \
    --tunconf="$DATA/tunnels.conf" --tunnelsdir="$DATA/tunnels.d" \
    --pidfile="$DATA/i2pd.pid" "${CERTS[@]}" \
    --log=file --logfile="$DATA/i2pd.log" --loglevel=warn \
    --port="$NET_PORT" --upnp.enabled=false \
    --http.enabled=false --httpproxy.enabled=false --socksproxy.enabled=false \
    --bob.enabled=false --i2cp.enabled=false --i2pcontrol.enabled=false \
    --sam.enabled=true --sam.address=127.0.0.1 --sam.port="$SAM_PORT" \
    >"$DATA/i2pd.stdout" 2>&1 &
pid=$!

up=0
for _ in $(seq 1 $((SAM_UP_S * 5))); do
    kill -0 "$pid" 2>/dev/null || break
    if (exec 3<>"/dev/tcp/127.0.0.1/${SAM_PORT}") 2>/dev/null; then up=1; break; fi
    sleep 0.2
done
if [ "$up" != 1 ]; then
    echo "[i2p-live] i2pd did not open SAM on ${SAM_PORT} within ${SAM_UP_S} s; its output:" >&2
    cat "$DATA/i2pd.stdout" "$DATA/i2pd.log" >&2 2>/dev/null || true
    exit 1
fi
say "SAM up after $(($(date +%s) - start)) s; running the live tests (budget ${TEST_BUDGET_S} s)"

rc=0
LEVICULUM_I2P_SAM="127.0.0.1:${SAM_PORT}" timeout "$TEST_BUDGET_S" \
    cargo test -p leviculum-std --lib interfaces::i2p::i2pd_live -- --ignored --nocapture \
    || rc=$?
say "router up to test end: $(($(date +%s) - start)) s, exit ${rc}"
if [ "$rc" != 0 ]; then
    echo "[i2p-live] the router's log:" >&2
    cat "$DATA/i2pd.log" >&2 2>/dev/null || true
fi
exit "$rc"
