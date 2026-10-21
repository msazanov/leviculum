# The ThinkNode M9

The Elecrow ThinkNode M9 is the project's first handheld and its first
ESP32 target to carry the full stack. Lew decided on 2026-10-21, a few
days before the unit arrived, what it is for. This page records that
decision, what exists today, the bring-up plan, what the T1000-E taught
that applies here, and what is still open. The messenger and the map
it is for have their own page ([A messenger whose core runs
anywhere](handheld-messenger.md)).

## The device

| Part | What the M9 carries |
|---|---|
| MCU | ESP32-S3R8: 512 KB SRAM, 8 MB PSRAM, 16 MB flash |
| Radio | Semtech LR1110, EU868 |
| Display | 2.4-inch 240x320 ST7789, no touch |
| Input | 37-key QWERTY keyboard |
| Sensors | GNSS, magnetometer, RTC |
| Other | buzzer, 2300 mAh battery, USB-C |

## What changed since August

Codeberg #178 has held an M9 note since 2026-08-13. Its pinout came from
MeshCore PR #2942. Its order was that the Heltec V4 pays for the ESP32
platform first, and that the LR1110 is the one open-ended item, because
our only radio driver was the SX1262's.

The radio driver is no longer open-ended. `leviculum-lr11xx` exists. Cedric
Rivard contributed it as a0f0c3a0 (#451), and it carries the SenseCAP
T1000-E today: the board file runs the LR1110 through it with the
LR1110MB1DXS shield's PA table (`LR1110MB1DXS_PA_TABLE`,
`leviculum-nrf/src/boards/t1000e.rs:145-147`).

The driver's README has a section headed "Worked example: ThinkNode M9
(incomplete)" (`leviculum-lr11xx/README.md:142-161`). Its values come from
Meshtastic's M9 variant, and some were probed on a bench:

- the TCXO at 3.3 V;
- the RF switch lines, taken from the Meshtastic variant;
- `RegMode::DcDc` with either low-frequency clock initialising with a
  clean `GetErrors`, so a 32.768 kHz crystal is fitted.

On one M9 the driver initialised, ran CAD, transmitted, and received
frames over the air (`leviculum-lr11xx/README.md:259-267`). There is still
no `Board` constant for the M9. The PA table and whether the DC-DC
inductor is fitted cannot be settled without the Elecrow schematic.

The ESP32 class itself is still a skeleton. `leviculum-esp` boots the
Heltec V4 and identifies itself, and nothing goes on the air
(`leviculum-esp/src/lib.rs:9-12`, [Supported
boards](../firmware/boards.md), "ESP32 class").

**A community port of our firmware to the M9 is in preparation.** That
is all this page says about it.

## Decided

- **The full stack, in the two modes of [Operating
  modes](operating-modes.md).** In Mesh mode the M9 is a propagation
  node, like every board ([An LXMF propagation node on a
  board](propagation-node-on-a-board.md)). A handheld is not an
  exception to that.
- **A messenger**, described on [A messenger whose core runs
  anywhere](handheld-messenger.md). If the measurements on that page
  allow it, the M9 also gets **a vector map**.
- **No one-button grammar.** [One button](one-button.md) is written for
  boards with one button. With a keyboard, the mode switch, the status
  and the position report are menu items, not press lengths.

## Bring-up plan

Nothing in this section is built yet.

1. **The schematic first.** The hardware reference is Elecrow's schematic.
   Pinouts from other firmware and values probed on a bench do not replace
   it (rule `feedback_schematic_is_the_hardware_reference`). The PA table
   and the DC-DC question above wait for it.
2. **Save the factory firmware** before anything is written to the
   flash.
3. **Open a bring-up issue with a boot proof** before any radio work: the
   `[FW_BUILD]` banner the rig already reads from every board.
4. **Give the rig an ESP32 side.** Nothing on the rig handles this chip
   family as an LNode today:
   - `lnflash` knows two transports, `uf2-msc` and `dfu-serial`
     (`Transport`, `lnflash/src/manifest.rs:128-138`). An ESP32-S3 has no
     UF2 and no DFU and is written with `espflash`.
   - periculum's LNode profile identifies a board by its USB vendor string
     and interface numbers (`periculum/periculum/devices/lnode.toml:24-32`).
     The one ESP32-S3 on the bench, the Heltec V4, is a `firmware = "rnode"`
     device (`periculum/rig.toml.example:112-115`).
   - The recipes exist: `check-lr11xx-esp32s3` (`Justfile:154`) and
     `build-esp32` (`Justfile:172`). Neither runs in a gate. The Xtensa
     compiler is optional per host, and `.woodpecker/nightly.yml` has no
     step for either.
   - The host toolchain pin stays at 1.97.1 until esp-rs ships a
     non-prerelease Xtensa toolchain of the same generation (decided
     2026-10-05, `scripts/install-ci.sh:376-380`, `rust-toolchain.toml:17`).
5. **Acceptance at the rig:**
   - the boot proof;
   - the display;
   - LoRa against the T1000-E (the same driver family);
   - LoRa against an RNode, with 915's T1000-E/RNode loss measurement as
     the comparison.

## What the T1000-E taught

The T1000-E was the first LR1110 board on the rig. An LR1110 board meets
these three things first:

- **A guessed PA table.** Seeed publishes no characterisation, so the
  T1000-E ships the Semtech shield's table. Every dBm it announces is that
  table's claim (#454). Two more board fields are guesses from
  Meshtastic's variant (#455).
- **`txp_honoured=no`.** The T1000-E drives 4 dBm when asked for 2. The
  status line says so in a word that can be grepped
  (`txp_honoured`, `leviculum-nrf/log-line/src/facts.rs:113`).
- **7 % loss against an SX1276 RNode at -27 dBm.** 915 read five runs of
  the T1000-E/RNode pair cell at a 93 % mean PDR
  (`periculum/hardware/t1000e_rnode_pair.toml:36-38`). It split the losses
  into half-duplex kills, which both firmwares share, and frames the
  T1000-E aired that the RNode never reported. The suspect for the second
  kind is the transmit signal from the uncharacterised PA. 917 added a
  silent witness board to that cell to tell the two apart (#466).

The M9's plan follows from those three. The PA table comes from the
schematic and a measurement, not from a shield, and the RNode run is
compared with the T1000-E's numbers.

## Open

- **Storage:** whether the M9 has a usable SD slot or only its 16 MB
  flash. #178 records the contradiction: Elecrow claims 16 GB, MeshCore
  configures 16 MB.
- **SRAM after WiFi and BLE:** how much of the 512 KB is left once both
  stacks are up.
- **WiFi as a TCP interface** without a host attached.
- **The keyboard:** its layout, and the input model on top of it. The
  controller sends key events over I2C, and its type, address and
  protocol can only be read off the device (#178).

## Not yet built

All of it, apart from the driver: the `Board` constant, a board file and
binary, the keyboard, the display UI, the menu, the rig's ESP32 flashing
and device profile, the gates for the ESP32 recipes, and the acceptance
runs.

## See also

- [A messenger whose core runs anywhere](handheld-messenger.md), for
  what the M9 is for.
- [Operating modes](operating-modes.md), for Mesh and Tracker.
- [One button](one-button.md), the grammar the M9 does not use.
- [How far one firmware build reaches](board-support-scope.md), for the
  ESP32 class and the LR11xx driver.
- [Supported boards](../firmware/boards.md), for the T1000-E and the
  Heltec V4.
