# Firmware layers: one orchestration, many boards

The board firmware is meant to run on three chip families, the nRF52840,
the ESP32-S3 and the RP2350, without a second copy of anything above the
chip. Lew decided on 2026-10-23 that no code is duplicated and that the
best architecture wins; Cedric Rivard's fork is wanted for what it does,
and its code is not owed. This page measures where the duplication is
today, draws the three strata the firmware is cut into, names the seams
between them and their shape, and orders the extraction. The dated
decisions are at the end.

Line counts and `path:line` citations into this tree are against master
at the commit that adds this page. Citations into the fork are against
the branch `cedrivard/Leviculum` `rp2350-m9-net` at `7a699446`; they are
written as file plus line words, because that tree is not this one and
the citation guard cannot hold them.

The rule this page serves is `rule:interface-isolation` one level down
([Interface isolation](interface-isolation.md)): only the bind crate
knows its chip.

## The problem, measured

The board firmware is already two strata. Thirty-five host-tested policy
crates under `leviculum-nrf/*` (the `[workspace] members` of
`leviculum-nrf/Cargo.toml`: announce-policy, rx-arming, tx-spacing,
channel-access, pn-store, record-log, heap-budget, feedback, usb-policy,
telemetry-policy, gnss-time, gnss-presence, boot-count, boot-trace,
time-floor, mute-lease, queue-budget, drop-budget, log-line, node-name,
persist-ack, media-state, motion, battery-scale, gnss-init, screen, ...)
plus `leviculum-core` and `leviculum-lxmf` hold the behaviour. Five of
the thirty-five are consumed by the host daemons too: `leviculum-std`
lists ble-tx, channel-access, announce-policy, queue-budget and
heap-budget (`leviculum-std/Cargo.toml:87`, `:95`, `:99`, `:103`, `:164`),
and lnpnd takes announce-policy, one of the five
(`lnpnd/Cargo.toml:159`). `leviculum-core` itself depends on
telemetry-policy (`leviculum-core/Cargo.toml:55`). So "nRF" in their
path is a misstatement of ownership, not a description.

Above them, `leviculum-nrf/src` (34,948 lines with the binaries) glues
the policy to the nRF52840, and the glue has three shapes mixed in one
tree:

- **The node loop is not a module. It is four copies.** Each board
  binary carries its own `main` with the loop inside it:
  `leviculum-nrf/src/bin/t1000e.rs:55-1280` is a 1,226-line `main` with a
  514-line `loop { select ... dispatch ... settle }` at
  `leviculum-nrf/src/bin/t1000e.rs:766-1279` and 149 references into
  `leviculum_nrf::`; `bin/t114.rs`, `bin/rak4631.rs` and
  `bin/solarnode.rs` carry the same loop, and `t114` and `rak4631`
  differ by 500 lines. Beside the four board binaries sit two probes
  (`bin/sd-ram-probe.rs` 294, `bin/qspi-selftest.rs` 634).
- **Orchestration modules** that know no chip but call each other
  freely: `announce.rs` 587, `mgmt.rs` 259, `media.rs` 320, `name.rs`
  259, `events.rs` 261, `heap_census.rs` 395, `remote_status.rs` 171,
  `transport_stats.rs` 171, `iface_bytes.rs` 215, `dispatch.rs` 49,
  `chtm.rs` 71, `identity.rs` 119, `clock.rs` 12, `banner.rs` 53,
  `boot_trace.rs` 119, `log.rs` 366, `telemetry.rs` 2062, `pn.rs` 3758,
  `lora.rs` 2705.
- **Backends** that know the chip: the SoftDevice BLE host
  (`ble/columba.rs` 3155, `ble/mod.rs` 1300, `ble/notify.rs` 238), USB
  CDC (`usb.rs` 1464, which also holds the control-frame executor, see
  [the seams](#the-seams-and-their-shape)), QSPI and internal flash
  (`qspi.rs` 1188, `record_store.rs` 788, `flash.rs` 79), the SX1262
  driver (`sx1262.rs` 1704), entropy from the SoftDevice (`rng.rs` 194),
  the allocator and the panic post-mortem (`lib.rs` 1160: the allocator
  at `leviculum-nrf/src/lib.rs:266-269`, the panic counter at
  `leviculum-nrf/src/lib.rs:315-363`, the post-mortem records, panic
  handler and HardFault at `leviculum-nrf/src/lib.rs:700-1160`),
  `boot_count.rs` (writes through `embassy_nrf::nvmc::Nvmc`,
  `leviculum-nrf/src/boot_count.rs:81`), the board files (`boards/*.rs`
  1264).

Cedric Rivard's fork needed the same glue for the RP2350 and the
ESP32-S3. It could not reuse ours, and wrote `leviculum-fw`
(`leviculum-fw/src`: 26,651 lines in 52 files, 36 modules declared in
its `lib.rs` plus the test module), `#![no_std]`, on the same policy
crates, imported from `../leviculum-nrf/` by path (25 of them). Twenty
of its module names are our file names: announce, chtm, clock,
dispatch, events, gnss, identity, iface_bytes, interface, log, lora,
media, mgmt, name, pn, radio_store, record_store, remote_status,
telemetry, transport_stats; `ble` is a directory module in both. The
fork did not extract a node loop either: its loop is per family too
(`leviculum-esp/src/node.rs` in the fork, 531 lines, and
`leviculum-rp`'s own `node::run`); the fork's `leviculum-fw/src/node.rs`
holds the interface registry (`Ifaces`, `register_interface`,
`on_link_wake`).

So the duplication is located, and so is its shape: two trees each
carry one copy of the orchestration modules and several copies of the
node loop, and neither carries the stratum this page draws. The module
boundaries agree across both trees, which is what makes the extraction
below a relocation rather than a design.

## The target: three strata

| stratum | what | crates | knows |
|---|---|---|---|
| 1 Policy | every protocol and behaviour decision, host-tested | `leviculum-core`, `leviculum-lxmf`, the thirty-five policy crates | no executor, no HAL, no board. One chip lives here already and stays: `leviculum-core/src/sx126x.rs` (2,906 lines) is the SX126x command set behind `RegisterBus` (`leviculum-core/src/sx126x.rs:1065`) and `CommandBus` (`leviculum-core/src/sx126x.rs:924`), implemented by our `Sx1262` (`leviculum-nrf/src/sx1262.rs:1680`, `leviculum-nrf/src/sx1262.rs:1700`) and by the fork's `leviculum-esp/src/sx1262.rs`; a chip's wire protocol is policy, its bus is a backend |
| 2 Orchestration | the node loop, once; every module of the orchestration list above; the control-frame executor | one new crate, working name `leviculum-firmware` | `embassy-executor`, `embassy-time`, `embassy-sync`, `alloc`, the seams below; no HAL crate, no chip name, no `cfg(feature = "bsp-*")` |
| 3 Bind | pins, backends, `main` | `leviculum-nrf`, `leviculum-esp`, `leviculum-rp` | one chip family each |

Stratum 2 is **extracted** from `leviculum-nrf/src` and from the four
binaries, with the behaviour it has today. It is not written anew and
not adopted from the fork. The reason is evidence: our glue carries
months of rig measurement (the `#438` collision floor, `#466` and the
rx-arming latch settle, the PN cells, the BLE room cells); a rewrite
would re-verify it on the rig, a relocation changes nothing the census
and the cells pin. The fork's `leviculum-fw` is the specification of
what stratum 2 must offer a second and a third family, and the source
of the ESP32-S3 and RP2350 backends; it is not the layer. Where the fork
cut a module better than we did (the control-frame executor, below),
its cut is taken.

## The seams, and their shape

Both trees today connect their modules the same way: free functions
over static state, and `embassy_sync` channels between tasks
(`ble_active` (`leviculum-nrf/src/media.rs:193`) and `note_duty_budget`
(`leviculum-nrf/src/announce.rs:152`) here; `lora::channels()`,
`BleChannels`, `LoRaChannels` in the fork). The seams take two shapes,
chosen by what crosses them:

- **Message seams** where a backend is a task: the LoRa part, the BLE
  controller, the host port. The orchestration owns the channels and
  the types on them (the fork already does this for LoRa configuration
  and BLE); the backend task consumes and produces. No generics climb
  into the orchestration modules; `announce.rs` keeps calling
  `crate::media::ble_active()` exactly as today, with the function now
  reading orchestration-owned state that the backend task feeds.
- **Trait seams** where a backend is a thing the orchestration calls
  synchronously or awaits once: flash, entropy, clock, sensors, sinks.
  Here an ecosystem trait is used when it exists and has two
  implementers: `embedded_storage_async::nor_flash::MultiwriteNorFlash`
  for flash (the fork's `leviculum-fw/src/record_store.rs` already binds
  it at lines 57, 268 and 323), `embedded_hal_async::spi::SpiDevice` for
  buses, `rand_core` for entropy. A project trait is written only where
  no ecosystem trait fits (the post-mortem sink, the one-button and
  keypad input, the display).

Rules for both: no `dyn`; no `cfg` per chip inside stratum 2
(`leviculum-nrf/src/radio.rs:8-10` already states this for the radio,
and selects by cargo feature; the seam makes the selection a type or a
task); every seam has a host fake, so stratum 2 is host-tested like
stratum 1 (precedents: the rx-arming fake radio harness, the fork's
`just test-fw`); a seam is cut only where a second implementer exists.

| seam | shape | covers | today | from the fork |
|---|---|---|---|---|
| Transceiver | message, plus the type set `Radio`/`RxStatus`/`Error`/`FrontEnd` of `radio.rs` as the vocabulary | bring-up, configure, CAD, TX, RX arming, IRQ facts | `sx1262.rs` over core's bus traits; `leviculum-lr11xx` (method set name for name, `leviculum-nrf/src/radio.rs:3-6`) | the fork has no transceiver seam: `leviculum-fw/src/lora.rs` names `leviculum_lr11xx::Lr11xx` (lines 21, 53, 742, 1151), its Heltec SX1262 path is a separate `leviculum-esp/src/{lora,radio,sx1262}.rs`. The seam is ours to cut |
| Control port | trait, the fork's `ControlPort` (`leviculum-fw/src/control.rs`, lines 166 to 188: two methods always, five more behind its `pn`, `bench` and `sd-storage` features) under a stratum-2 control-frame executor (the fork's `control.rs`, 795 lines) | the framed host link: bytes, DTR/RTS or XON/XOFF | `usb.rs` is today the executor AND the port: it reaches sixteen sibling modules and two `lib.rs` functions and runs `ControlAction` dispatch (`leviculum-nrf/src/usb.rs:24`, `leviculum-nrf/src/usb.rs:768`), only seventeen lines touch `embassy_usb` | `serial-mux` on the M9 (one UART, debug text and HDLC), RP2350 USB CDC. The fork's cut is adopted |
| BLE link | message: connection up/down with MTU and parameters, inbound writes, outbound notifies, advertising on/off | the Columba link as the orchestration sees it | SoftDevice: the GATT service is declared by `nrf_softdevice` macros (`leviculum-nrf/src/ble/columba.rs:133`, `leviculum-nrf/src/ble/columba.rs:149`, `leviculum-nrf/src/ble/columba.rs:1779`) and raw S140 structs (`leviculum-nrf/src/ble/columba.rs:729`, `leviculum-nrf/src/ble/columba.rs:2293`); the backend also reaches up today (`ble/mod.rs` calls `crate::media` at `leviculum-nrf/src/ble/mod.rs:442,752,757,1287`, `crate::boot_trace` at `leviculum-nrf/src/ble/mod.rs:1281,1291`, `crate::name` at `leviculum-nrf/src/ble/mod.rs:1139` and `crate::iface_bytes` at `leviculum-nrf/src/ble/mod.rs:696,769`; `columba.rs` calls `crate::name` at `leviculum-nrf/src/ble/columba.rs:407` and `crate::iface_bytes` at `leviculum-nrf/src/ble/columba.rs:1080,2908`), which the extraction turns into messages | TrouBLE over `bt_hci::Transport` (the fork's `leviculum-fw/src/ble/mod.rs`, lines 81 to 91) on CYW43439 and on the ESP32-S3 radio. The seam sits above GATT because the SoftDevice is not an HCI controller; the fork's `HciTransport` stays inside its backend |
| Flash | `MultiwriteNorFlash`, plus `pn-store::Region` (`leviculum-nrf/pn-store/src/lib.rs:160`) and `record-log` on top | the record log, the PN store, settings, identity, boot count, time floor (`time_floor.rs` writes through `crate::flash::SharedFlash`, `leviculum-nrf/src/time_floor.rs:141`, beside the regions of `boot_count` and `record_store`) | internal flash and QSPI (`record_store.rs`, `qspi.rs`, `flash.rs`), `boot_count.rs` over `Nvmc` | RP2350 flash with a PSRAM index, ESP32 flash, `leviculum-sd-store` on a card |
| Clock | trait | monotonic time, calendar sources and their rank | embassy time, GNSS time, host time | the same plus SNTP (`TimeSource::Ntp`, the fork's `leviculum-fw/src/lib.rs` line 120) and the PCF8563 |
| Entropy | `rand_core` | what core's crypto draws | `rng.rs` (SoftDevice `sd_rand_application_bytes_available_get`, `leviculum-nrf/src/rng.rs:109`) | the ESP32 and RP2350 hardware RNGs |
| Sensors | traits per sensor | GNSS UART, motion, battery, die temperature, later magnetometer | AG3335 (`gnss.rs`), QMA6100P (`motion.rs`), ADC divider (`battery.rs`), `nrf_softdevice::temperature_celsius` (`leviculum-nrf/src/telemetry.rs:946`) | AG3352 (same Airoha init), MAX17048 (`battery-scale`); the M9's QMI8658B IMU ([The ThinkNode M9](thinknode-m9.md)) has no driver in the fork yet |
| Sinks | traits | LED, buzzer, display, keypad or button, the debug log (`log.rs` 366, to CDC today), the post-mortem store (`leviculum-nrf/src/lib.rs:315-363`, `leviculum-nrf/src/lib.rs:700-1160`; the fork's `diag/postmortem.rs`, 398 lines) | `led.rs`, `feedback.rs`, SSD1306 and ST7789, `button.rs` | the M9's ST7789 and keypad (menus, [One button](one-button.md)) |
| Watchdog | trait | feed and timeout | none fed: the ten matches in `leviculum-nrf/src` are four comments about the outbound link watchdog in the board binaries and `qspi-selftest`'s read of the WDT's RUNSTATUS | per family |
| Net (optional) | the fork's `leviculum-net` as a stratum-3 crate | TCP, UDP, NTP over `embassy-net` | none on nRF52840 | shared by esp and rp |

Power management has zero references in today's firmware and gets no
seam until there is something to put behind it.

The protocol inside each seam stays below it: the Columba framing and
defragmenter in `leviculum-core/src/framing/ble.rs`, the notify pump in
`ble-tx` (the fork's `pump_fragments` addition to that crate is its
shared form), the LoRa channel discipline in `rx-arming`,
`channel-access`, `tx-spacing`. A fix to behaviour never touches a
backend; a fix to a chip never touches behaviour.

## The extraction, bottom-up

The orchestration modules import the backends (`announce.rs` imports
pn, telemetry, identity and baseboard; `mgmt.rs` telemetry; `media.rs`
ble and telemetry; `name.rs` telemetry; `events.rs` announce and pn;
`heap_census.rs` ble, pn, rng, clock and iface_bytes), so moving them
first does not compile. The order is therefore bottom-up: the seams
first, the modules that use them after, the node loop last.

Each step: cut the seam, let `leviculum-nrf` implement it, move the
files that now depend only on seams, prove zero behaviour change by the
existing tests, the compile-time census (`lint-nrf`, `nrf-stack-frames`
read the ELF and need no board), the runtime `[HEAP_CENSUS]` line on a
board, one rig cell per touched subsystem, and a debug-log diff over
one scripted scenario with these keys masked: `t=`, `host_ms=`, `rssi=`,
`snr=`, every counter and the `[HEAP_CENSUS]` numbers (the capture
exists in `scripts/debug-witness.sh`; the masking is step 0's
[`scripts/log-diff-mask.py`](../firmware/host-test-seams.md#the-log-diff-masking)). One coder pass per step.

| step | does | seams | pin |
|---|---|---|---|
| 0 | the embassy pin rule across the three families (ours: embassy-executor 0.9 / embassy-sync 0.7; the fork: 0.10 / 0.8 plus a `[patch.crates-io]` git rev in the fork's `leviculum-fw/Cargo.toml`, lines 146 to 154); the log-diff masking script; the crate skeleton in the `leviculum-nrf` workspace (excluded from the root, so the guards below live there) | none | builds; the guards below green on an empty crate |
| 1 | Flash, Entropy, Clock, Watchdog, Sinks (debug log, post-mortem, LED, buzzer, display, button) as traits with host fakes; `boot_count.rs`, `rng.rs`, `log.rs`, `lib.rs`'s post-mortem, `time_floor.rs`, `record_store.rs`'s policy half move | 5 | census, the boot-count and time-floor host tests, one boot on each board (the rig's `boot-proof-both.sh`, which lives outside this tree) |
| 2 | the control-frame executor out of `usb.rs` into stratum 2 (the fork's `control.rs` as the template), `usb.rs` shrinks to the port | Control port | lnflash round trips over every control frame, the serial-mux host tests |
| 3 | Transceiver as a message seam; `lora.rs` and `radio.rs` move; `sx1262.rs` and `leviculum-lr11xx` stay below | Transceiver | `t1000e_lora_pair`, `t1000e_rnode_pair` with the witness, the `#438` cells; `lora_pn_board_sync` only with its register standing stated (it is in periculum's `hardware/KNOWN-REDS.toml`, retired twice) |
| 4 | BLE link as a message seam; the upward calls of `ble/mod.rs` and `columba.rs` become messages; `ble/` stays the SoftDevice backend | BLE link | the BLE room cells, `ble_pn_board_upload`, a two-phone session |
| 5 | the pure modules move: `announce`, `mgmt`, `media`, `name`, `events`, `heap_census`, `remote_status`, `transport_stats`, `iface_bytes`, `dispatch`, `chtm`, `identity`, `banner`, `boot_trace`; `telemetry.rs` with the reporter moved into `leviculum-lxmf` behind a `reporter` feature, as the fork did | Sensors | the telemetry cells, 914's double press, `/status` round trips |
| 6 | `pn.rs` over Flash and the PN store | none new | `lora_pn_board_sync_t1000e`, the PN census, the stock-client cells |
| 7 | **the node loop, once**: the four binaries' loops are diffed line by line; every difference is either a board fact (moves to the board file) or a behaviour decision (named in the report, decided before it is unified); the loop moves to stratum 2 and `bin/*.rs` become wiring | none new | every rig cell of the four boards, before and after; this is the riskiest step and gets its own order per binary |
| 8 | the ESP32-S3 and RP2350 bind crates onto the extracted crate; `leviculum-fw` retires module by module, its backends adapted to the seams | none new | the fork's bench results re-run on the M9 and the RedBoard |

Out of scope for every step: a behaviour change under the cover of a
move. In scope as its own mechanical step, early: moving the thirty-five
policy crates out of the `leviculum-nrf/` path ([the problem,
measured](#the-problem-measured) says why the path is wrong; the fork
and the daemons both import them from there).

## What is taken from the fork now, independent of the extraction

As PRs by crate against master, each with its own gates (audit 927
lists the file spans):

1. Core hardening: resource parts bounded by the SDU and the remaining
   transfer, request-id exactly 16 bytes, an announce longer than the
   base MTU refused; each PR text carries the three-part deviation
   argument.
2. Bounded BLE reassembly (`framing/ble.rs`, `ble_reassembly_budget.rs`,
   the nRF and lnsd instantiations), with a BLE room cell before and
   after.
3. The boot-count torn-slot scan fix.
4. The host-tested member crates: `serial-mux`, `rtc-pcf8563`,
   `battery-scale` (MAX17048), `gnss-init` (Airoha),
   `record-log::legacy`, `ble-tx`'s `pump_fragments`.
5. `leviculum-sd-spi` and `leviculum-sd-store` as a Flash-seam backend
   after step 1; the core `sd-storage` control plane and the lnflash
   provisioning with them.
6. `leviculum-net` as the WiFi families' stratum-3 crate, with the
   `net_config` codec moved there out of core (audit 927, isolation
   finding).

Not taken as-is: `leviculum-fw` as a layer ([the target](#the-target-three-strata));
`leviculum-ble5` and the vendored TrouBLE copy (the CYW43439 could not
prove periodic advertising on the fork's own bench); the lnpnd owner
shortcut in `admit_control` without a stated decision; the 100-plus
non-test `expect` in the new crates.

## Gates the new stratum brings

- `firmware-no-hal`: `cargo tree` of `leviculum-firmware` carries no
  `embassy-nrf`, `nrf-softdevice`, `esp-hal`, `embassy-rp`, `cyw43`,
  `trouble-host`; precedent `scripts/check-nrf-evt-max-size.sh:42`;
  positive-controlled with an injected dependency.
- `firmware-no-chip-cfg`: no `cfg(feature = "bsp-` and no chip name in
  stratum 2 sources. The existing source-invariant tests are
  root-workspace targets (`scripts/source-invariant-targets.txt`) and
  `leviculum-nrf` is excluded from the root (`Cargo.toml:59`); the new
  crate lives in the `leviculum-nrf` workspace, so this guard is a
  script under `just guards`, not a root-workspace test.
- The census unchanged, in both its forms: the compile-time size
  asserts in `lint-nrf` and `nrf-stack-frames` (no board), and the
  runtime `[HEAP_CENSUS]` line (a board). A step's commit message states
  the before and after numbers; a difference is measured, never
  estimated.
- The log diff of [the extraction](#the-extraction-bottom-up), attached
  to every step's report.

## Risks and open questions

- Flash and stack on the nRF52840 (flip-link, the 96 KiB heap, `#50`)
  are the budget that says no first. The message seams add channels,
  the trait seams add nothing at runtime; the figures per step are
  measurements, none is given here.
- The SoftDevice is not a controller behind a thin trait; step 4 may
  end with a wider BLE link vocabulary than TrouBLE needs. Decide at
  step 4 with both implementers on the table, not before.
- The node loop (step 7): four copies, each pair differing by between
  about 370 and 650 lines; every difference is a decision. This is
  where "zero behaviour change" is hardest to keep and where the review
  time goes.
- Cedric Rivard: the plan asks the fork to retire its layer and keep
  its backends and leaf crates. Whether that happens is Cedric's call,
  and Lew's conversation.
- Time: steps 0 to 7 are weeks of coder passes beside the main lane.
  The M9 port needs exactly these seams, so the work is not a detour;
  it is still second-lane work under `rule:workflow-priorities`.

## Decision record

- 2026-10-23, Lew: no duplicated code, the best architecture; the
  fork's functionality is wanted, its code is not owed; take parts, be
  inspired, need not be immediate.
- 2026-10-23: extraction over adoption ([the
  target](#the-target-three-strata)); two seam shapes, message and
  trait, with ecosystem traits where they exist ([the
  seams](#the-seams-and-their-shape)); bottom-up order with the node
  loop last ([the extraction](#the-extraction-bottom-up)); the fork's
  control-frame cut adopted.
- 2026-10-23, review of the first draft: five facts corrected (crate
  count, line counts, binaries, the node loop's location, the fork's
  missing transceiver seam), the step order inverted, four seams added
  (entropy, watchdog, debug log, post-mortem), the embassy rule moved to
  step 0.
- 2026-10-23: this page is tracked by Codeberg #469 (firmware layers);
  the fork integration by Codeberg #470.
- Open: the order of steps 3 to 7 against the T1000-E and M9 work on
  the main lane; the BLE link vocabulary; whether the policy crates'
  path move comes before or after step 1.

## See also

- [Interface isolation](interface-isolation.md), the rule this page
  carries down to the chip.
- [How far one firmware build reaches](board-support-scope.md), for the
  board families a bind crate serves.
- [The firmware's host-test seam](firmware-host-test-seam.md), for how
  the policy crates are tested without a board.
- [The ThinkNode M9](thinknode-m9.md), the ESP32-S3 handheld that needs
  these seams first.
