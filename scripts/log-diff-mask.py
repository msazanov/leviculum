#!/usr/bin/env python3
"""Mask the values of a board debug capture that a relocation may change.

Codeberg #469, the firmware layers' step 0: every extraction step proves it
changed no behaviour by diffing one scripted scenario's debug capture before
and after the move (docs/src/concepts/firmware-layers.md, "The extraction,
bottom-up"). Two captures of the same scenario never agree byte for byte:
the stamps differ, the radio's link figures differ, a relocated module
shifts a heap number. This writes a capture with exactly those values
replaced by a placeholder, so `diff` of two masked captures shows what is
left -- a line that appeared, vanished, changed its words or its order.

Masked, as `key=<*>` (a key is a run of word characters after a line start,
a space or a `[`, so `len=` is never part of `rx_len=`'s match and the other
way round):

  * the stamps, `t=` and `host_ms=`;
  * the link figures, `rssi=` and `snr=`;
  * every duration, a key ending in `_ms`;
  * the counters: `packets=`, every `rx_*=` and every `tx_*=`;
  * on a `[HEAP_CENSUS]` line, every number, keyed or not.

Not masked, and that is the point: `len=` (a frame length is behaviour), the
words, the keys, the line order and every other value.

Usage:
  log-diff-mask.py [CAPTURE [OUT]]   (stdin and stdout when omitted)

The page that documents it is docs/src/firmware/host-test-seams.md, "The
log-diff masking"; scripts/test-log-diff-mask.sh is its test.
"""

import re
import sys

PLACEHOLDER = "<*>"

# The keys whose value is masked wherever they appear.
MASKED_KEY = re.compile(
    r"(?:^|(?<=[\s\[]))"
    r"(t|host_ms|rssi|snr|packets|\w+_ms|rx_\w+|tx_\w+)"
    r"=(\S+)"
)
# Any number on a census line: the walk's figures move with the image.
CENSUS_NUMBER = re.compile(r"(?<![\w.])-?\d+(?:\.\d+)?(?![\w.])")


def mask_line(line: str) -> str:
    out = MASKED_KEY.sub(lambda m: f"{m.group(1)}={PLACEHOLDER}", line)
    if "[HEAP_CENSUS]" in out:
        out = CENSUS_NUMBER.sub(PLACEHOLDER, out)
    return out


def main(argv: list[str]) -> int:
    if len(argv) > 3:
        print(__doc__, file=sys.stderr)
        return 2
    # A capture is the board's bytes: a torn UTF-8 sequence after a reset
    # must not stop the mask, and must come out as it went in.
    src = open(argv[1], encoding="utf-8", errors="surrogateescape") if len(argv) > 1 \
        else open(sys.stdin.fileno(), encoding="utf-8", errors="surrogateescape", closefd=False)
    dst = open(argv[2], "w", encoding="utf-8", errors="surrogateescape") if len(argv) > 2 \
        else open(sys.stdout.fileno(), "w", encoding="utf-8", errors="surrogateescape",
                  closefd=False)
    with src, dst:
        for line in src:
            dst.write(mask_line(line))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
