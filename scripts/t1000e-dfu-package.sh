#!/usr/bin/env bash
#
# Wrap the release t1000e ELF as an Adafruit serial-DFU package.
#
# The SenseCAP T1000-E's bootloader takes images over serial DFU only on the
# unit we have (no UF2 volume), and serial DFU takes a zip of the image plus
# an init packet. Two constants go into that packet, and both are checked by
# the bootloader before it writes a byte:
#
#   --dev-type 0x0052  ADAFRUIT_DEVICE_TYPE, refused otherwise
#                      (Adafruit_nRF52_Bootloader, dfu_init.c in its src/,
#                      lines 84 and 158)
#   --sd-req 0x0123    the firmware id of S140 7.3.0, the SoftDevice the unit
#                      reports; the bootloader compares it with the one it
#                      runs (the same file, line 178). Value from Meshtastic's
#                      boards/tracker-t1000-e.json, "sd_fwid": "0x0123".
#
# Run by `just t1000e-dfu-package` after the release build. Writes
# <nrf target>/thumbv7em-none-eabihf/release/t1000e-dfu.zip and prints the
# line that flashes it. Flashing is a deliberate act and not done here.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source-path=SCRIPTDIR/..
# shellcheck source=scripts/cargo-target-dir.sh
source "$ROOT/scripts/cargo-target-dir.sh"
OUT="$(cargo_target_dir "$ROOT/leviculum-nrf")/thumbv7em-none-eabihf/release"
ELF="$OUT/t1000e"
[ -f "$ELF" ] || { echo "no t1000e ELF at $ELF; build it first" >&2; exit 1; }

NRFUTIL="$(command -v adafruit-nrfutil || true)"
[ -n "$NRFUTIL" ] || [ ! -x "$HOME/.local/bin/adafruit-nrfutil" ] || NRFUTIL="$HOME/.local/bin/adafruit-nrfutil"
[ -n "$NRFUTIL" ] || { echo "adafruit-nrfutil not found (pip install --user adafruit-nrfutil)" >&2; exit 1; }

OBJCOPY="$(command -v arm-none-eabi-objcopy || command -v llvm-objcopy || true)"
[ -n "$OBJCOPY" ] || { echo "no objcopy found (arm-none-eabi-binutils or llvm-tools)" >&2; exit 1; }

# -R .bss -R .uninit for the reason scripts/lnflash-bundle.sh gives: NOBITS
# sections must not reach the image.
"$OBJCOPY" -O ihex -R .bss -R .uninit "$ELF" "$OUT/t1000e.hex"
"$NRFUTIL" dfu genpkg --dev-type 0x0052 --sd-req 0x0123 \
    --application "$OUT/t1000e.hex" "$OUT/t1000e-dfu.zip"

echo "[t1000e-dfu] package: $OUT/t1000e-dfu.zip"
echo "[t1000e-dfu] flash:   $NRFUTIL --verbose dfu serial --package $OUT/t1000e-dfu.zip -p /dev/ttyACMn -b 115200 --singlebank --touch 1200"
