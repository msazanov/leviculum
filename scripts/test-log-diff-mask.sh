#!/usr/bin/env bash
#
# Test for scripts/log-diff-mask.py (Codeberg #469, firmware layers step 0).
#
# Three scripted captures of one scenario. AFTER is BEFORE as a relocation
# leaves it: every stamp, link figure, duration, counter and census number
# moved, every word, `len=` and the line order stayed. Masked, the two must
# diff to nothing. REORDERED is BEFORE with one line moved down two places;
# masked, it must diff to that line and nothing else. A mask that hides too
# much passes the first half and fails the second, one that hides too little
# fails the first.
#
# Usage: test-log-diff-mask.sh [-v]   (-v prints both diffs)

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MASK="$HERE/log-diff-mask.py"
VERBOSE="${1:-}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

cat > "$tmp/before" <<'EOF'
[INFO!] [FW_BUILD] git_sha=83e581fc1 dirty=false t=22919 host_ms=1792575799734
[HEAP_CENSUS] used=41234 free=57070 largest=31800 node_box=9120 links=2 n_links=2 t=23001 host_ms=1792575799816
ANN_TX occasion=transit dst=b06ca5fb hops=1 iface=1 t=23942 host_ms=1792575800756
[T114_LORA_LOOP] op=rx_timeout duration_ms=886 t=25626 host_ms=1792575802440
[LORA] RX 147 bytes rssi=-31 snr=14 flags=0x50 dst=b8e2845c ctx=0x00 t=37610 host_ms=1792575814426
[T114_SERIAL_TX] pkt_hash8=0001b8e2845c0a3a len=131 t=47627 host_ms=1792575824444
[SX_RX_ARM] site=jitter timeout_ms=336 dark_ms=0 t=49211 host_ms=1792575826026
[LORA_STATS] packets=12 rx_ok=9 rx_crc=1 tx_done=3 t=50100 host_ms=1792575826915
PKT_RELAY outcome=forwarded ph=11ee2fb25e7076d8 dst=855ce9a2 hops=1 iface_out=1 t=81806 host_ms=1792575858621
EOF

cat > "$tmp/after" <<'EOF'
[INFO!] [FW_BUILD] git_sha=83e581fc1 dirty=false t=22004 host_ms=1792579912001
[HEAP_CENSUS] used=41290 free=57014 largest=31744 node_box=9176 links=2 n_links=2 t=22088 host_ms=1792579912085
ANN_TX occasion=transit dst=b06ca5fb hops=1 iface=1 t=23011 host_ms=1792579913008
[T114_LORA_LOOP] op=rx_timeout duration_ms=902 t=24730 host_ms=1792579914727
[LORA] RX 147 bytes rssi=-44 snr=9 flags=0x50 dst=b8e2845c ctx=0x00 t=36588 host_ms=1792579926585
[T114_SERIAL_TX] pkt_hash8=0001b8e2845c0a3a len=131 t=46710 host_ms=1792579936707
[SX_RX_ARM] site=jitter timeout_ms=292 dark_ms=1 t=48302 host_ms=1792579938299
[LORA_STATS] packets=14 rx_ok=10 rx_crc=2 tx_done=4 t=49170 host_ms=1792579939167
PKT_RELAY outcome=forwarded ph=11ee2fb25e7076d8 dst=855ce9a2 hops=1 iface_out=1 t=80840 host_ms=1792579970837
EOF

# BEFORE with the ANN_TX line (3) moved below the LORA RX line (5).
{
    sed -n '1,2p' "$tmp/before"
    sed -n '4,5p' "$tmp/before"
    sed -n '3p' "$tmp/before"
    sed -n '6,$p' "$tmp/before"
} > "$tmp/reordered"

for f in before after reordered; do
    python3 "$MASK" "$tmp/$f" "$tmp/$f.masked"
done

rc=0

if diff -u "$tmp/before.masked" "$tmp/after.masked" > "$tmp/d1"; then
    echo "[log-diff-mask] ok   stamps and counters only: the masked diff is empty"
else
    echo "[log-diff-mask] FAIL stamps and counters only: the masked diff is not empty"
    cat "$tmp/d1"
    rc=1
fi

# The diff body minus the headers and the context lines: what changed.
diff -u "$tmp/before.masked" "$tmp/reordered.masked" > "$tmp/d2" || true
changed="$(grep -E '^[-+][^-+]' "$tmp/d2" | sed 's/^[-+]//' | sort -u)"
want="$(sed -n 3p "$tmp/before.masked")"
if [ "$changed" = "$want" ]; then
    echo "[log-diff-mask] ok   one line reordered: the masked diff is that line"
else
    echo "[log-diff-mask] FAIL one line reordered: expected the diff to be only"
    echo "    $want"
    echo "  got:"
    cat "$tmp/d2"
    rc=1
fi

# `len=` is behaviour and must survive the mask.
if grep -qF 'len=131' "$tmp/before.masked"; then
    echo "[log-diff-mask] ok   len= is not masked"
else
    echo "[log-diff-mask] FAIL len= was masked"
    rc=1
fi

if [ "$VERBOSE" = "-v" ]; then
    echo "--- diff 1 (relocation, masked) ---"; cat "$tmp/d1"
    echo "--- diff 2 (one line reordered, masked) ---"; cat "$tmp/d2"
fi

exit "$rc"
