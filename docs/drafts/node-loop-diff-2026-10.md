# The four node loops, diffed line by line (step 7 preparation, 1017)

Draft, 2026-10-26. Preparation for step 7 of
`docs/src/concepts/firmware-layers.md` ("the node loop, once"), Codeberg
#469. No code changed. Every line number below is against master at
`a3ac403b`; the four files are `leviculum-nrf/src/bin/t114.rs` (1415
lines), `rak4631.rs` (1469), `t1000e.rs` (1377) and `solarnode.rs`
(1505). The two probes (`sd-ram-probe.rs`, `qspi-selftest.rs`) are tools
and are left out.

Method. `diff -w` of each file against `t114.rs`, read hunk by hunk; then
a normalised comparison (whitespace removed, the board module's name
replaced by one token, so `t114::CONFIG` and `rak4631::CONFIG` compare
equal) of every phase across all four, once with comments and once code
only; then a line census: a code line is common when it is matched in the
longest common subsequence against each of the other three files, and
every code line that is not common was attributed by hand to the row of
the table below that explains it. No code line is unattributed. The
scripts were throwaway and are not kept; the method is enough to redo
them.

A note on feature sets, because several rows only exist under one. The
shipped builds are `bsp-t114` (pulls `gnss`, `battery` and the
orchestration crate's `display`), `bsp-solarnode` (`gnss`, `battery`),
`bsp-t1000e` (`gnss`, `battery`, `motion`) and `bsp-rak4631,rak-baseboard`
(`display`, `gnss`, `battery`; the lnflash bundle,
`scripts/lnflash-bundle.sh:81`). A bare `bsp-rak4631` build exists too,
and it is what `just flash-rak4631` flashes (`Justfile:2114`).

## 1 The common skeleton

Every binary runs the same 28 phases in the same order. Ranges are
`t114.rs`, `rak4631.rs`, `t1000e.rs`, `solarnode.rs`. "Identical" means
byte for byte after whitespace and the board module's name, comments
included; "code identical" means the same with comments left out.

1. Entry, HAL config, `embassy_nrf::init`, heap: `t114.rs:37-54`,
   `rak4631.rs:38-55`, `t1000e.rs:55-72`, `solarnode.rs:55-72`.
   Identical, 18 lines.
2. The previous boot's post-mortems and the persistent log tail, the
   stack paint: `t114.rs:55-69`, `rak4631.rs:56-70`, `t1000e.rs:73-87`,
   `solarnode.rs:73-87`. Code identical (one comment reworded on the RAK).
3. Panic LED, HardFault LED, IRQ priorities: `t114.rs:70-75`,
   `rak4631.rs:71-84`, `t1000e.rs:88-95`, `solarnode.rs:88-98`. Differs
   (B2).
4. Set-once registrations (interface names, entropy, feedback sinks) and
   the two pre-USB declarations (reporter, position sources):
   `t114.rs:76-94`, `rak4631.rs:85-101`, `t1000e.rs:96-110`,
   `solarnode.rs:99-117`. Differs (B15).
5. Pre-USB flash reads (media profile, name, management allow-list, boot
   count): `t114.rs:95-123`, `rak4631.rs:102-131`, `t1000e.rs:111-140`,
   `solarnode.rs:118-150`. Code identical.
6. USB up, boot line, `[FW_BUILD]`, `[TIME_SOURCE]`, boot diagnostics:
   `t114.rs:124-138`, `rak4631.rs:132-153`, `t1000e.rs:141-155`,
   `solarnode.rs:151-165`. Differs (B1, D2).
7. Board rails and the early pins (LED, buzzer, GNSS control):
   `t114.rs:139-147`, `rak4631.rs:154-175`, `t1000e.rs:156-169`,
   `solarnode.rs:166-172`. Differs (B3, B4).
8. Identity store, the link cap, the builder, identity load, `build_boxed`,
   heap budget, time floor: `t114.rs:148-244`, `rak4631.rs:176-272`,
   `t1000e.rs:170-266`, `solarnode.rs:173-269`. Identical, 97 lines.
9. Management banner, `boot_log_repeater`, `fw_build_banner`, diagnostics
   tasks, identity save: `t114.rs:245-265`, `rak4631.rs:273-293`,
   `t1000e.rs:267-287`, `solarnode.rs:270-290`. Identical, 21 lines.
10. Interface registration (names, MTUs, modes, link sparing, ingress
    control): `t114.rs:266-318`, `rak4631.rs:294-346`, `t1000e.rs:288-340`,
    `solarnode.rs:291-343`. Identical, 53 lines.
11. `LNode started` line: `t114.rs:319-337`, `rak4631.rs:347-365`,
    `t1000e.rs:341-359`, `solarnode.rs:344-362`. Identical, 19 lines.
12. QSPI: `t114.rs:338-348`, `rak4631.rs:366-377`, `t1000e.rs:360-368`,
    `solarnode.rs:363-413`. Differs (B5).
13. LoRa bring-up, radio profile load, LoRa spawn or held down:
    `t114.rs:349-409`, `rak4631.rs:378-438`, `t1000e.rs:369-428`,
    `solarnode.rs:414-481`. Differs (D1, B6, B7, B8); the profile load and
    the spawn decision are code identical.
14. BLE bring-up (`ble::init` with its twenty PPI channels) and the
    media and name banners: `t114.rs:410-456`, `rak4631.rs:439-483`,
    `t1000e.rs:429-475`, `solarnode.rs:482-528`. Code identical in three;
    the RAK lacks one line (D1).
15. Flash-backed store tasks (radio config, time floor, telemetry, record
    log, the store spike): `t114.rs:457-492`, `rak4631.rs:484-517`,
    `t1000e.rs:476-511`, `solarnode.rs:529-564`. Code identical.
16. Board peripheral tasks (display, button, GNSS, battery, motion):
    `t114.rs:493-582`, `rak4631.rs:518-593`, `t1000e.rs:512-583`,
    `solarnode.rs:565-659`. Differs (B9 to B13, D1, D7).
17. Heap line, stack line, interface adapters: `t114.rs:583-593`,
    `rak4631.rs:594-604`, `t1000e.rs:584-594`, `solarnode.rs:660-670`.
    Identical, 11 lines.
18. Boot blink, LED and buzzer tasks: `t114.rs:594-611`,
    `rak4631.rs:605-614`, `t1000e.rs:595-615`, `solarnode.rs:671-697`.
    Differs (D3, D4, D5, B14).
19. Telemetry delivery destination, boot identity, PN engine and miner:
    `t114.rs:612-640`, `rak4631.rs:631-659`, `t1000e.rs:616-644`,
    `solarnode.rs:698-726`. Identical, 29 lines.
20. Announce gate, reporter with its persisted target and position, the
    host receivers: `t114.rs:641-672`, `rak4631.rs:660-691`,
    `t1000e.rs:645-676`, `solarnode.rs:727-759`. Identical, 32 lines.
21. GNSS seed state: `t114.rs:673-688`, `rak4631.rs:615-630` (before
    phase 19, D8), `t1000e.rs:677-688`, `solarnode.rs:760-775`. Differs
    (B15, D8).
22. The two tickers, the announce cap, `[STG] main-loop`:
    `t114.rs:689-709`, `rak4631.rs:692-722`, `t1000e.rs:689-709`,
    `solarnode.rs:776-796`. Differs (D1).
23. The `pn_step!` macro: `t114.rs:710-748`, `rak4631.rs:723-761`,
    `t1000e.rs:710-748`, `solarnode.rs:797-835`. Identical, 39 lines.
24. Loop top: pollers, the deadline, the three wait futures, the
    `select4`: `t114.rs:749-850`, `rak4631.rs:762-852`, `t1000e.rs:749-843`,
    `solarnode.rs:836-937`. Code identical except the T1000-E's GNSS future
    (B15).
25. Post-wake mirrors (online, peer count, announce cap, time source,
    checkpoint): `t114.rs:851-884`, `rak4631.rs:853-886`,
    `t1000e.rs:844-877`, `solarnode.rs:938-971`. Identical, 34 lines.
26. The thirteen arms of `match wake`: `t114.rs:885-1310`,
    `rak4631.rs:887-1357`, `t1000e.rs:878-1298`, `solarnode.rs:972-1397`.
    Eleven arms are code identical in all four (host wall time, telemetry
    target, fixed position, host announce, telemetry tick, periodic
    announce, button, serial RX, LoRa RX, BLE RX, BLE peer). The GNSS arm
    differs in form on the T1000-E (B15), the timeout arm carries the
    RAK's `ACT` block (D6), and the RAK lists two arms in another order
    (D8).
27. `TELEMETRY_TICK_INTERVAL` and `collect_readings`: `t114.rs:1311-1398`,
    `rak4631.rs:1358-1441`, `t1000e.rs:1299-1360`, `solarnode.rs:1398-1488`.
    Code identical in three; the T1000-E's in form (B15).
28. Tasks `boot_log_repeater`, `fw_build_banner`, and on the bare RAK
    `led_heartbeat`: `t114.rs:1399-1415`, `rak4631.rs:1442-1469`,
    `t1000e.rs:1361-1377`, `solarnode.rs:1489-1505`. Differs (D5).

Ten of the 28 phases are identical with their comments (1, 8, 9, 10, 11,
17, 19, 20, 23, 25; 353 lines of `t114.rs`), three more are code identical
(2, 5, 15). The main loop proper (phases 23 to 26) is one copy with three
deviations.

## 2 The difference table

B rows are board facts, D rows behaviour decisions. "Lines" is the number
of code lines in each file (t114 / rak4631 / t1000e / solarnode) that are
not common to all four and are explained by the row.

| row | phase | what differs | t114 | rak4631 | t1000e | solarnode | class | lines | question for Lew |
|---|---|---|---|---|---|---|---|---|---|
| B1 | 6, 12, 16 | the board's name as a string: boot line, `[QSPI] NONE board=`, `ButtonWiring.board` | `T114`, `t114` (`t114.rs:128`) | `RAK4631`, `rak4631` (`rak4631.rs:136`) | `T1000-E`, `t1000e` (`t1000e.rs:145`) | `SolarNode`, `solarnode` (`solarnode.rs:155`) | board fact | 2/2/2/2 | |
| B2 | 3 | a second LED for HardFault | none (one LED) | `set_hardfault_led(1, 4, false)`, blue P1.04 (`rak4631.rs:79`) | none (one LED) | `set_hardfault_led(0, 15, false)`, blue P0.15 (`solarnode.rs:97`) | board fact | 0/1/0/1 | |
| B3 | 7 | board power rails and static control lines raised before the radio | `vext::raise(P0_21)`, TFT and L76K rail (`t114.rs:145`) | 3V3-S rail P1.02 and LoRa PA power P1.05 by raw `Output::new` (`rak4631.rs:161`, `:165`) | `power_sensors(P1_06)`, `gnss_control(4 pins)` (`t1000e.rs:162-163`) | none (`solarnode.rs:166-169`) | board fact | 1/2/2/0 | |
| B4 | 7 | which LED and buzzer handles exist | `led(P1_03)` | `led(P1_03)`, plus `led_notification(P1_04)` under `display` | `led(P0_24)`, buzzer enable P1.05 and PWM0 on P0.25 | `led(P0_19)`, `led_notification(P0_15)` | board fact | 1/6/3/2 | |
| B5 | 12 | the QSPI part | `[QSPI] NONE ... reason=not-fitted-...` | `... reason=no-onboard-flash-...` | `... reason=not-fitted-...` | `identify_and_mount_bounded` over six pins when `CONFIG.qspi_part` is `Some` (`solarnode.rs:391-412`) | board fact | 1/1/1/15 | |
| B6 | 13 | the seven LoRa pins passed to `lora::init` | P0.19, 22, 23, 24, 25, 17, 20 | P1.11, 12, 13, 10, 06, 14, 15 | P0.11, P1.09, P1.08, P0.12, P1.10, P0.07, P1.01 | P1.13, 15, 14, P0.04, 28, 29, 03 | board fact | 7/7/7/7 | |
| B7 | 13 | the radio front end | `FrontEnd { tcxo, rx_enable: None }` | same, `None` | `&t1000e::LR1110` (an LR11xx `Board`) | `FrontEnd { tcxo, rx_enable: Some(P0_05) }` | board fact | 4/5/1/5 | |
| B8 | 13 | the radio part in the ready line | `SX1262 ready` | `SX1262 ready` | `LR1110 ready` | `SX1262 ready` | board fact | 1/0/1/0 | |
| B9 | 16 | the display | ST7789 over SPI3 and seven pins, gated `bsp-t114` (`t114.rs:499-517`) | SSD1306 over TWISPI0 (`rak4631.rs:522`) under `display`; `[DISP] none` without | `[DISP] none` | `[DISP] none` | board fact | 18/7/1/1 | |
| B10 | 16 | the button | P1.10 active low | P0.09 active low, only under `display` (carrier); `[BTN] none` without | P0.06 active high | P1.01 active low | board fact | 3/8/3/3 | |
| B11 | 16 | GNSS wiring and module | L76K: RX P1.05, TX P1.07, PPS P1.04, standby P1.02 | ZOE-M8Q: RX P0.15, TX P0.16, no PPS, no standby | AG3335: RX P0.14, TX P0.13, standby P1.12, enable P1.11, wake | L76K: RX P1.12, TX P1.11, standby P0.02, enable P1.05 | board fact | 9/11/8/9 | |
| B12 | 16 | battery sense | AIN2 P0.04, enable P0.06, `for_board(4.916)` | P0.05, no enable, `for_board(1.73)` | P0.02, no enable, `for_unsized_divider(2.0)` | P0.31, enable P0.14 active low, `for_divider(BATTERY_DIVIDER)` | board fact | 7/4/3/7 | |
| B13 | 16 | the motion sensor | none | none | QMA6100P on TWISPI0, `motion::init` (`t1000e.rs:573-582`) | none | board fact | 0/0/10/0 | |
| B14 | 18 | LED polarity and the extra feedback sinks | active low, one `Led` sink | (see D4, D5) | active high, `Led` plus `spawn_buzzer` | active high, `Led` plus a `Notify` LED | board fact | 7/0/8/14 | |
| B15 | 4, 21, 24, 26, 27 | how a fitted capability is spelt | `cfg(feature = "gnss")`, `cfg!(...)`, `cfg(feature = "battery")` | same | no cfg, `gnss_available: true`; same image, since `bsp-t1000e` implies both | same as T114 | board fact (form only, no behaviour) | 41/32/13/41 | |
| D1 | 13, 14, 16, 22 | the `[STG]` staging lines | `lora-init`, `ble-init`, `display-spawn`, `main-loop` | none of them | `lora-init`, `ble-init`, `main-loop` | `lora-init`, `ble-init`, `main-loop` | decision | 4/0/3/3 | Do all boards print `[STG] lora-init`, `ble-init`, `main-loop` (RAK too), and is `display-spawn` kept as the display task's own line? |
| D2 | 6 | `[GNSS_PRESENCE] state=detecting baud=9600` boot anchor | absent | printed under `gnss` (`rak4631.rs:144-145`), baud a literal although `rak4631::GNSS_BAUD` exists | absent | absent | decision | 0/2/0/0 | Does every GNSS board print the detecting anchor (from `gnss.rs`, with the board's baud), or none? |
| D3 | 18 | the boot blink (a 12,000,000-iteration `nop` loop, blocking) | yes | no, in neither build | yes | yes | decision | 4/0/4/4 | Keep the blocking boot blink on every board, give it to the RAK, or drop it? |
| D4 | 18 | LoRa TX/RX activity pulses on the LEDs | no | yes: `led::init` hands `LORA_TX_FLASH`/`LORA_RX_FLASH` to the two sinks (`rak4631.rs:611`) | no | no | decision | 0/1/0/0 | Is activity blinking a feature of every board that has a free LED, of the RAK only, or of none? |
| D5 | 18, 28 | the bare RAK build: a 1 Hz `led_heartbeat` task instead of a feedback sink | n/a | `rak4631.rs:613`, `:1460-1469`, plus `Output` imports | n/a | n/a | decision | 0/14/0/0 | Is bare `bsp-rak4631` still a shipped target? If yes, does it run the feedback grammar like every other board; if no, should `just flash-rak4631` (`Justfile:2114`) stop flashing it onto Pocket V2s? |
| D6 | 26 | `ACT SendPacket` / `ACT Broadcast` per action in the timeout arm (#422 diagnostic) | no | yes (`rak4631.rs:1274-1318`) | no | no | decision | 0/37/0/0 | Drop it, or keep it on every board (in `events.rs`, beside the other core-event lines)? |
| D7 | 16 | info-line wording on task spawns | `display task spawned (ST7789 blind-drive)`, `gnss task spawned (L76K)` | `display task spawned`, `gnss task spawned`, and a `button task spawned` no other board prints (`rak4631.rs:535`) | `gnss task spawned (AG3335)`, `motion task spawned` | `gnss task spawned (L76K)` | decision | 0/1/0/0 | Unify on "task spawned (module)" with the module name from the board file, no RAK-only button line? |
| D8 | 21, 26 | order only: the GNSS seed receiver taken before phase 19, and the GNSS and telemetry-tick arms in other source positions | reference order | moved (`rak4631.rs:615-630`, `:1054`, `:1332`) | reference order | reference order | decision (no observable effect) | 21/24/19/21 | Take the T114 order (proposed: yes; `GNSS_FIX` has only these receivers, `leviculum-nrf/firmware/src/board_state.rs:26`, and match arms are exclusive) |

D8's line counts are lines that moved, not lines that differ: the moved
code is identical, and the census counts it in every file because the
alignment against the RAK loses it. The same alignment also displaces the
GNSS arm, whose 14 lines in three files are booked to B15.

## 3 The count

Rows: 15 board facts, 8 decisions. Of the decisions, D8 is an order with
no observable effect. The other seven are all the RAK against the other
three: D1 and D3 are lines and a blink the RAK lacks, D2 and D4 to D7 lines
or tasks only the RAK has. Beyond them the T114, the solar node and the
T1000-E have one decision line between them, the T114's
`[STG] display-spawn` (inside D1).

Lines, measured as described above:

| file | total | blank | comment | code | skeleton | board fact | decision | decision without D8 |
|---|---|---|---|---|---|---|---|---|
| `t114.rs` | 1415 | 55 | 519 | 841 | 710 | 102 | 29 | 8 |
| `rak4631.rs` | 1469 | 56 | 538 | 875 | 710 | 86 | 79 | 55 |
| `t1000e.rs` | 1377 | 56 | 521 | 800 | 711 | 63 | 26 | 7 |
| `solarnode.rs` | 1505 | 55 | 602 | 848 | 713 | 107 | 28 | 7 |

Skeleton is the 705 code lines common to all four plus the lines that
differ only by rustfmt's wrapping around the board name's length and by
the RAK's two extra imports (5, 5, 6, 8). Comments are not classified:
they follow their code, and where they differ they say the board's facts
in prose. Of the board-fact lines, B15 (41, 32, 13, 41) is spelling, not
substance.

## 4 What the board files already hold, and where the rows go

`leviculum-nrf/src/boards/*.rs` (t114 200, rak4631 215, t1000e 377,
solarnode 418, `mod.rs` 90) hold today:

- `CONFIG: BoardConfig` (`leviculum-nrf/src/boards/mod.rs:43`): USB ids
  and strings, three flash pages, TCXO select, SPI clock, maximum power,
  `qspi_part`, `log_prefix`.
- `IFACE_NAMES`, the `PANIC_LED_*` triple.
- A pin alias for every wired pin (`pub type LoRaSck = peripherals::P0_19`
  and so on), LED constructors (`led`, `led_notification`), the battery
  facts (`ADC_MULTIPLIER`, `ADC_CTRL_ACTIVE`, solar's `BATTERY_DIVIDER`),
  and on the T1000-E the whole LR1110 description (`LR1110`), the GNSS
  control set (`gnss_control`), `power_sensors` and the buzzer
  constructors.

Three findings about that content, none of them a row:

- The binaries pass raw `p.Px_y` handles, never the aliases: no `bin/`
  file names a `Lo*`, `Gnss*`, `Tft*` or `Battery*` alias. The two copies
  of the pin map are held equal only by `scripts/check-nrf-board-pins.sh`
  claim 2, which reads the `lora::init` and QSPI call sites (and the
  T1000-E's `GnssWiring` literal) out of the binaries.
- Dead board facts: `CONFIG.lora_spi_freq_hz`, `CONFIG.lora_max_power_dbm`
  and `CONFIG.log_prefix` have no reader outside `boards/`, and all four
  binaries hardcode `spim::Frequency::M4`. Likewise `LORA_SPI_FREQ_HZ`,
  `LORA_TCXO_VOLTAGE`, `LORA_MAX_POWER_DBM`, `LORA_DIO2_AS_RF_SWITCH`,
  `LORA_BUSY_TIMEOUT_MS`, `GNSS_BAUD` and `NEOPIXEL_COUNT`.
- Two shared modules name one board's file: `vext::raise` takes
  `crate::boards::t114::VextEnable` (`leviculum-nrf/src/vext.rs:57`) and
  `st7789::TftWiring` takes the T114's seven `Tft*` aliases
  (`leviculum-nrf/src/st7789.rs:253-259`).

Where each board-fact row goes:

| row | target | new seam needed |
|---|---|---|
| B1 | `CONFIG.log_prefix` already exists and is unread; one board name field read by the boot line, the QSPI line and `ButtonWiring` | no |
| B2 | an `Option` HardFault LED triple beside `PANIC_LED_*` | no |
| B3 | a `power_up(..)` per board, as the T1000-E's `power_sensors`/`gnss_control` already are; the RAK's two rails become constructors there; ordering constraint: before `lora::init` (the RAK's PA power) | no |
| B4, B14 | a per-board feedback spawn over the constructors that exist, on the Sinks seam (973) | no, cut |
| B5 | a per-board QSPI probe over `CONFIG.qspi_part` (solar's form); the pin gate's claim 4 then reads the board file, not the binary | no (Flash, step 1, is only needed when the record log moves onto it) |
| B6, B7, B8 | a per-board `lora` wiring value (pins plus front end plus part name); the front-end type stays per driver | no for the move; the loop leaving `leviculum_nrf::lora` needs the Transceiver seam (step 3) |
| B9 | per-board display spawn; the page is stratum 2's since 980 | no, cut |
| B10 | `ButtonWiring` as a board constant, `[BTN] none` where absent | no, cut (976) |
| B11, B13 | `GnssWiring` and `MotionWiring` built in the board file; the tasks stay in the bind crate, their state is `board_state` | no (Sensors: state cut in 959, tasks stay below) |
| B12 | battery wiring and `BatteryScale` in the board file | no, cut (991) |
| B15 | the cfg spelling, unified on the T114's; the capability set is the BSP's cargo features already | no |

So every board-fact row can move into its board file now, without a new
seam. What holds the loop itself in the bind crate is not a board fact:
the identical loop names `leviculum_nrf::lora` (`LoRaInterface`,
`channels`, `accepted_packets`, `AnnounceCap`), `media`, `telemetry`,
`announce`, `events`, `remote_status`, `heap_census`, `transport_stats`,
`banner`, `pn` (steps 3, 5 and 6, the cluster branch of plan B part 3),
the serial channels `usb::init` returns (step 2, the control port), and
`collect_readings` reads the die temperature through the SoftDevice
handle (`t114.rs:1361`), a Sensors seam item no step has cut yet.

## 5 Proposed order of the per-binary moves

Step 7 is one order per binary. Proposed:

1. **T114 first.** It is the reference the others were written from: the
   solar node's binary is "the T114's binary with the solar node's pin
   map" (`solarnode.rs:11-12`), the T1000-E's is "the solar node's binary
   with the T1000-E's pin map" (`t1000e.rs:10`). Its file has no decision
   row of its own beyond the `display-spawn` line, so its loop is the loop
   that moves. Best rig
   coverage: 58 hardware cells need `t114` in their profile (`rig.toml`),
   among them `boot_smoke_t114`, `lnode_remote_status`, `lora_pn_board_sync`,
   `ble_room_4`, `ble_pn_board_upload`, `lora_t114_lncp_bidir`,
   `lora_duty_lock_lawful_default` and the path discovery series.
2. **Solar node second.** No decision of its own; the largest board-fact
   set (the QSPI probe, two LEDs, the divider polarity, the RX-enable
   front end), which is exactly what tests the board-file shape. Cells:
   `boot_smoke_solarnode`, `reset_loop_solarnode_quiet`,
   `reset_loop_solarnode_links`, `lora_solarnode_lncp_bidir`,
   `ble_mesh_formation_solar`, `ble_room_4`.
3. **T1000-E third.** No decision of its own and B15 is spelling only; it
   is last of the three because its radio is the one `FrontEnd` of another
   type (B7) and its coverage is thinnest: `t1000e_lora_pair`,
   `t1000e_rnode_pair`, `lora_pn_board_sync_t1000e`, and no boot smoke
   cell (a `boot_smoke_t1000e` is the gap to fill before its date).
4. **RAK4631 last**, after Lew has answered D1 to D7. It carries six
   decisions of its own and the bare build (D5) is a second shape of the
   same binary. Its coverage is the second best (28 cells on `pocket-v2`,
   among them `boot_smoke_rak4631`, `ble_room_4`, `ble_pn_board_upload`,
   `ble_lora_transport`, the `ble_lxmf_delivery*` set and
   `lora_pn_board_offer_past_the_link`), so if the decisions come early it
   can go second.

Dates are the reviewer's.

## What could not be classified

- **Comments.** 519 to 602 comment lines a file are not classified. Two of
  them are wrong and should be fixed in the move: the RAK's module doc
  says the baseboard peripherals "land in Phase 3" (`rak4631.rs:8-9`),
  and they are all wired; the T1000-E's says the buzzer is "Not in it"
  (`t1000e.rs:24-25`), while `t1000e.rs:614` spawns it.
- **D8** is classed a decision because the instruction classes order as
  one, but no behaviour hangs on it; it needs no answer beyond "take the
  T114's order".
- **B15** is classed a board fact (GNSS and battery are fitted on every
  T1000-E), though no behaviour differs: the two spellings compile to the
  same image under `bsp-t1000e`.
- **The bare RAK build** (D5) is either a product decision or dead code;
  which one is Lew's question, not a fact the tree can answer.
