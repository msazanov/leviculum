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
| MCU | ESP32-S3R8: 512 KB SRAM, 8 MB PSRAM, 16 MB NOR flash |
| Storage | microSD slot, a card with map tiles in the box |
| Radio | Semtech LR1110, EU868 |
| Display | 2.4-inch 240x320 ST7789, no touch |
| Input | 37-key QWERTY keyboard behind its own controller |
| Sensors | GNSS, RTC, magnetometer, IMU |
| Other | buzzer, 2300 mAh battery, USB-C through a USB-UART bridge |

## The hardware from the public record

No schematic is public. Elecrow's wiki quotes the V1.0 schematic, and
Elecrow's technical support is the address for it. Until it is here,
the facts below come from four public sources, and each one names the
source it rests on:

- **the wiki**: Elecrow's ThinkNode M9 wiki, Meshtastic edition,
  document version of 2026-08-26, describing "ThinkNode M9 V1.0";
- **the product page**: Elecrow's shop page and its PDF on formatting
  the microSD card;
- **the Meshtastic variant**: `variants/esp32s3/ELECROW-ThinkNode-M9/`
  in the Meshtastic firmware, develop branch;
- **the MeshCore variant**: `variants/thinknode_m9/` in MeshCore, main
  branch.

The two variants were read on 2026-10-21. None of these sources is the
hardware reference (rule `feedback_schematic_is_the_hardware_reference`),
so where they disagree this page keeps the disagreement and does not
pick a side. The device settles it.

### Storage

- **A microSD slot (TF) exists.** The wiki shows it, and both variants
  drive it: chip select on GPIO48 (the Meshtastic variant's build flags,
  the MeshCore variant's `SDCARD_CS`).
- The card shares one SPI bus with the LR1110 and the LCD: SCK 40,
  MOSI 47, MISO 38 (both variants). Only one of the three chip selects
  may be low at a time; the Meshtastic variant drives all three high
  before anything else runs.
- The product page's "16 GB" is the microSD card in the box, which
  carries map tiles. The flash is a ZD25Q128, 16 MB of NOR (the wiki,
  from the schematic), which is what the MeshCore variant's 16 MB
  partition table configures. The "16 GB against 16 MB" question in
  #178 is therefore two different parts, not a contradiction.
- 2026-10-25, on our unit: the flash is confirmed as the ZD25Q128
  (see "What the unit on the rig confirmed"). The microSD card is not
  reachable from outside, so the vendor firmware's SD update path is not
  usable here.

### Display

- 2.4-inch TFT, 240x320, ST7789, no touch. Both variants configure the
  ST7789.
- Pins (both variants): CS 16, DC 15, RESET 14, TE 19, backlight 17.
  The backlight is on when the pin is low (`TFT_BACKLIGHT_ON LOW` in
  both).
- Peripheral power: GPIO18, on when low (`VEXT_ENABLE`/`VEXT_ON_VALUE`
  in both). MeshCore's board code switches it on to power the screen.

### Keyboard

- 37-key QWERTY plus navigation and function keys (the wiki), read
  through a controller on its own I2C bus, SDA 20 and SCL 21 (both
  variants).
- KB_INT on GPIO12 signals a key press and wakes the chip from light
  sleep; the Meshtastic variant says it rests low and rises on a press.
  The keyboard light is on GPIO46 (both variants).
- **Contradiction:** the wiki names an ESP32-S2 as the controller (U10,
  reading a 5x10 matrix); the Meshtastic variant's comments name an
  STC8H. Which chip it is decides nothing for a port as long as the I2C
  protocol is the same, but neither source documents that protocol; the
  Meshtastic variant writes one command (register 0x03, the long-press
  time).

### Two hardware revisions

The Meshtastic variant tells two revisions apart by the keyboard
controller's I2C address: **0x6C is V1.0, 0x6D is V1.1** (`m9Version`).
The GNSS enable line has the opposite polarity on the two: **active low
on V1.0, active high on V1.1**. The MeshCore variant knows only active
low, which is V1.0. A port has to probe the address at boot, as the
Meshtastic variant does, before it touches the GNSS enable line. The
wiki describes V1.0 only.

### GNSS

- **Contradiction, three ways:**
  - the wiki names an ATGM336H-5NR32 (and a BM72A);
  - the Meshtastic variant says "AG3352 without RTC_INT" at 115200 baud,
    and puts it to sleep with `$PAIR003` followed by EN low;
  - the MeshCore variant's header declares `GPS_L76K` at 9600 baud,
    while its build file sets 115200 baud, so MeshCore disagrees with
    itself as well.
- Pins (both variants): UART on GPIO2 and GPIO3, 1PPS 4, RESET 5,
  standby or wake line 10, EN 11. The variants name the two UART pins
  in opposite directions, so which one the chip transmits on is read
  off the device. Backup power comes from GPS_BAT (the wiki). The wiki
  lists GPS, BDS, Galileo and GLONASS.
- Our T1000-E GNSS work is for an AG3335 (#456). If the receiver is an
  AG3352, that work may transfer, since it is the same family; if it is
  an ATGM336H or an L76K, it does not. The receiver has to be
  identified on the device before any GNSS code is written for it.
- **Resolved by the bench, in favour of the Airoha family.** According
  to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), an AG3352 answered the Airoha `$PAIR` boot init on the bench
  unit. The three-way contradiction above stays as the history of what
  the public sources said; see "What the community port measured".

### Sensors

On a second I2C bus, SCL 6 and SDA 7 (both variants):

- a PCF8563 RTC at 0x51 (both variants), backed by its own RTC_3V3
  supply (the wiki);
- a QMC6309 magnetometer (the wiki);
- a QMI8658B IMU (the wiki).

One review article claims the M9 has no accelerometer. This page
follows the wiki, which names the part.

### Radio

A Semtech LR1110 with a 3.3 V TCXO on DIO3 (both variants), the RF
switch on DIO5 and DIO6 with the same table in both variants (standby
LL, RX HL, TX HH, TX_HP LH, DIO5 first), and NSS 39, BUSY 41, IRQ 42
(DIO9), NRESET 45. The MeshCore variant transmits at 22 dBm. The
`leviculum-lr11xx` README carries these values as a `Board` sketch,
with what was probed on a bench and what stays unknown without the
schematic: see its section "Worked example: ThinkNode M9 (incomplete)"
(`leviculum-lr11xx/README.md`).

### Power

- A 2300 mAh LiPo with protection (the wiki).
- An LGS4056HDA charger ending the charge at 4.35 V (the wiki).
- Battery voltage on GPIO13 through a 2:1 divider (the wiki; the
  Meshtastic variant's `BATTERY_PIN 13` with `ADC_MULTIPLIER 2.0`).
- USB power detected on GPIO1, low when present (the wiki's 5V_TEST;
  `EXT_PWR_DETECT 1` in both variants).
- A slide switch for on and off, a hidden RESET button, and BOOT on
  GPIO0 (the wiki). The slide switch is one more reason the one-button
  grammar does not apply (see "Decided").
  Since 2026-10-25 stage one does apply the grammar, on one key (see
  "Decided"); how its power-off row meets the slide switch is listed
  under "Open".

### USB and the rest

- USB-C reaches the ESP32-S3 through a CH340K USB-UART bridge at full
  speed (the wiki), not through the chip's own USB. There is no
  `uf2-msc` path: flashing goes over the ROM's UART bootloader with an
  esptool-class tool. Both variants upload with `esptool`.
- A buzzer behind a transistor, enabled on GPIO9 (the wiki; `PIN_BUZZER
  9` in both variants).
- 2026-10-24, on our unit: the DTR/RTS auto-reset through the CH340K
  works; esptool entered the ROM loader on the first try, three times,
  with no button pressed.

### What the unit on the rig confirmed

Read on 2026-10-24 and 2026-10-25 on schneckenschreck, without writing a
byte to the board (the reviewer's bench notes):

- **The chip:** ESP32-S3 (QFN56), revision v0.2, 8 MB embedded PSRAM.
  This confirms the device table.
- **The flash:** manufacturer 0xba, device 0x4018, the Zetta ZD25Q128,
  16 MB, read by `flash_id`. This confirms "Storage".
- **The eFuses are open:** no secure boot, no flash encryption
  (`SPI_BOOT_CRYPT_CNT` 0), download mode enabled. The whole flash reads
  in plain text and writes back unchanged.
- **The vendor firmware** is "Ripple UI / MeshOS" (meshcore.co.uk),
  MeshOS 1.3.1 by its own strings, derived from MeshCore.
- **The backup:** the full 16 MB image is
  `thinknode-m9_e8f60ab888f4_meshos-vendor_2026-10-25.bin` under
  `/home/lew/firmware-backups/` on schneckenschreck, with its partition
  table, checksums and restore command in that directory's README. Step 2
  of the bring-up plan is done.
- **The board revision** (V1.0 or V1.1) is not among these facts. The
  chip's revision is not the board's; that question stays under "Open".

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

**A community port of our firmware to the M9 exists.** The next section
takes the facts its bench measured; everything else about it stays with
the port.

## What the community port measured

The community port ran on one M9 unit. The facts below are what it
measured, not what we measured, and each one names that source. Its
board file and README are `leviculum-esp/src/boards/thinknode_m9.rs`
and `leviculum-esp/README.md` on that branch.

### Transmit power

- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), the M9's high-power PA delivers about 4 to 5 dB
  less than the LR1110 shield's at the same setting.
- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), the usable ceiling is about +17 dBm, reached at
  `power = 22`; MeshCore's "22 dBm" is that register value, not an
  output power.
- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), the PA table that fits keeps the shield's LP rows
  for -17 to +10 dBm and runs the HP PA with `power = dBm + 5` for +11
  to +17 dBm.

### The keypad

- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), INT (GPIO12) is a 15 ms pulse that the controller
  does not raise for every key, so the port polls the controller while
  the panel is lit.
- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), `KB_LED` (GPIO46) lights nothing at either level
  on a version-2 board.

### GNSS

- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), an AG3352 answered the Airoha `$PAIR` boot init on
  the bench unit. That settles the three-way contradiction under "GNSS"
  in favour of the Airoha family, so our AG3335 work transfers.

### Executors

- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), running the LoRa task on an esp-rtos interrupt
  executor crashed the board, with stack-guard panics and watchdog
  resets within minutes.
- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), the port therefore keeps LoRa on core 0's thread
  executor, at a cost in re-arm latency after a reception: p50 about
  2 ms on the interrupt executor against 142 ms on the thread executor.

### The host link

- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), the single CH340K UART carries both the debug text
  and the HDLC transport, multiplexed on one line with XON/XOFF flow
  control. The multiplexing rules live in `serial-mux`, a member crate
  of ours since 943 (`leviculum-nrf/serial-mux`).

### GPIO10

- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), the board file types GPIO10 as the GNSS idle
  interrupt (an input), while the README says it is "held high" (an
  output). This page keeps the question open until a schematic or a
  probe decides it.

## Decided

- **The full stack, in the two modes of [Operating
  modes](operating-modes.md).** In Mesh mode the M9 is a propagation
  node, like every board ([An LXMF propagation node on a
  board](propagation-node-on-a-board.md)). A handheld is not an
  exception to that.
- **A messenger**, described on [A messenger whose core runs
  anywhere](handheld-messenger.md). If the measurements on that page
  allow it, the M9 also gets **a vector map**.
- **The one-button grammar in stage one, the menu grammar in stage
  two.** [One button](one-button.md) applies in stage one, on one
  designated key (2026-10-25, below). The menu grammar, in which the mode
  switch, the status and the position report are menu items, is stage
  two, with the keyboard UI. The position of 2026-10-21 was: "No
  one-button grammar. One button is written for boards with one button.
  With a keyboard, the mode switch, the status and the position report
  are menu items, not press lengths."
- **2026-10-25, Lew: stage one is a board like the others:** the same
  status screen, one-button operation on one key, Mesh mode, propagation
  node. Only when that is stable does the keyboard UI follow. The M9 also
  carries the NomadNet browser, after the messenger. Concept work runs in
  parallel: a messenger GUI concept for the desktop and for a 240x320
  display.

## Bring-up plan

Nothing in this section is built yet.

1. **The schematic first.** The hardware reference is Elecrow's schematic.
   Pinouts from other firmware and values probed on a bench do not replace
   it (rule `feedback_schematic_is_the_hardware_reference`). The PA table
   and the DC-DC question above wait for it.
2. **Save the factory firmware** before anything is written to the
   flash. Done 2026-10-25 (see "What the unit on the rig confirmed").
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
     2026-10-05, `scripts/install-ci.sh:378-382`, `rust-toolchain.toml:17`).
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

- **The schematic**, asked of Elecrow's technical support: the PA table,
  the DC-DC inductor, and every contradiction under "The hardware from
  the public record" wait for it or for the device.
- **Which revision the unit is**, read off the keyboard controller's
  I2C address at the first boot.
- **Which key carries the one-button grammar in stage one**, and how its
  8 s power-off row meets the slide switch (2026-10-25, "Decided").
- **The GNSS receiver:** AG3352, ATGM336H or L76K, identified on the
  device. Resolved by the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10) in favour of the AG3352 (see "What the
  community port measured").
- **GPIO10's direction:** the GNSS idle interrupt (input) or a line held
  high (output); in the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10) the board file and the README disagree.
- **SRAM after WiFi and BLE:** how much of the 512 KB is left once both
  stacks are up.
- **WiFi as a TCP interface** without a host attached.
- **The keyboard:** which controller it is (ESP32-S2 or STC8H), its
  I2C protocol beyond the one command the Meshtastic variant writes,
  and the input model on top of it.

## Not yet built

All of it, apart from the driver: the `Board` constant, a board file and
binary, the keyboard, the display UI, the menu, the rig's ESP32 flashing
and device profile, the gates for the ESP32 recipes, and the acceptance
runs.

## See also

- [A messenger whose core runs anywhere](handheld-messenger.md), for
  what the M9 is for.
- [Operating modes](operating-modes.md), for Mesh and Tracker.
- [One button](one-button.md), the grammar the M9 uses on one key in
  stage one.
- [How far one firmware build reaches](board-support-scope.md), for the
  ESP32 class and the LR11xx driver.
- [Supported boards](../firmware/boards.md), for the T1000-E and the
  Heltec V4.
- [Firmware layers](firmware-layers.md), for the shared orchestration
  the M9's bind crate is meant to sit under.
