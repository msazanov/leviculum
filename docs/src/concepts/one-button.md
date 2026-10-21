# One button

Every board has one user button and at least one LED, and the gestures
on that button mean the same thing on every board (Lew's decision of
2026-10-20). The board answers through whatever it has: an LED on all of
them, a notification LED on the WisMesh Pocket V2 and the Solar Node, a
buzzer on the T1000-E, a display on the T114 and the Pocket. This page
records the gestures, the feedback grammar, what moves compared to
today, and what is open.

## The hardware

Every board file names a `UserButton` and a `LedPin`
(`leviculum-nrf/src/boards/t114.rs:47,162`,
`leviculum-nrf/src/boards/rak4631.rs:53,62`,
`leviculum-nrf/src/boards/solarnode.rs:92,98`,
`leviculum-nrf/src/boards/t1000e.rs:281,286`). The Pocket (`rak4631.rs`)
and the Solar Node add a `LedNotificationPin`
(`leviculum-nrf/src/boards/rak4631.rs:55`,
`leviculum-nrf/src/boards/solarnode.rs:94`), the T1000-E a `BuzzerPin`
(`leviculum-nrf/src/boards/t1000e.rs:291`). The Pocket drives an OLED
and the T114 an ST7789 (`leviculum-nrf/src/bin/t114.rs:496`).

Since 889 every board brings the button up with the same task
(`leviculum-nrf/src/bin/t114.rs:524`,
`leviculum-nrf/src/bin/solarnode.rs:577`,
`leviculum-nrf/src/bin/rak4631.rs:530`,
`leviculum-nrf/src/bin/t1000e.rs:526`). The T114 and the Solar Node used
to say `[BTN] none` only because nobody had wired the task: neither pin
is shared with anything the firmware drives, and the Solar Node's touch
pad is a second pin (`TouchButton`, `leviculum-nrf/src/boards/solarnode.rs:100`)
that stays unused. On the T1000-E the long press is also the product's
power button (`leviculum-nrf/src/boards/t1000e.rs:282-286`).

Before 892 the LEDs said nothing a user asked for. The T114, the Solar
Node and the T1000-E flashed theirs once at boot and never touched it
again (`leviculum-nrf/src/bin/t114.rs:597`,
`leviculum-nrf/src/bin/solarnode.rs:674`,
`leviculum-nrf/src/bin/t1000e.rs:601`). The Pocket V2's two LEDs pulsed
LoRa TX and RX activity (`leviculum-nrf/src/led.rs`), and a bare RAK4631
module without the baseboard blinks a 1 Hz heartbeat
(`leviculum-nrf/src/bin/rak4631.rs:1444`). The Solar Node's blue LED was
lit only by a HardFault. The T1000-E's buzzer had its enable held low
for the life of the board.

## The gestures

| gesture | does |
|---|---|
| short press | Status. The display wakes for one minute and goes dark by itself; there is no display switch. The LED blinks mode and battery, the buzzer joins where present. No announce. |
| double press | Announce, both identities (what the short press did before 889, 869), plus one immediate position report when a recipient is configured. Feedback when the proof comes back. |
| long press, 2 s | Mode switch Mesh/Tracker ([Operating modes](operating-modes.md)). The board signals at 2 s, the user releases, the board confirms the new mode. |
| very long press, 8 s | Power off. A press powers on. |
| any press in Tracker mode | Also opens the wake window: BLE on, listening. |
| button held at boot, 10 s | Factory reset, pairing cleared. |

The same table on every board means a user who knows one board knows all
of them, and a gesture never does something on one board that it does
not do on another.

## The feedback grammar

One grammar for the LED, the buzzer and the display text:

| signal | means |
|---|---|
| three short | Tracker |
| one long | Mesh |
| *n* short | battery, in quarters |
| fast flicker | error |
| notification LED, or one short buzz | a message for the user arrived |

Since 892 the board plays this grammar. The patterns are
`leviculum-feedback` (`leviculum-nrf/feedback/src/lib.rs`), host-tested
for each pattern's shape and total length. Every pattern is a list of
on/off steps built from these constants:

| constant | value | reason |
|---|---|---|
| `SHORT_ON_MS` (`leviculum-nrf/feedback/src/lib.rs:68`) | 150 ms | one counted blink; seen as deliberate, not as the 80 ms traffic pulse |
| `COUNT_GAP_MS` (`leviculum-nrf/feedback/src/lib.rs:72`) | 350 ms | over twice the blink, so the gaps are what the eye counts; two blinks a second |
| `LONG_ON_MS` (`leviculum-nrf/feedback/src/lib.rs:76`) | 1000 ms | the Mesh long, over six shorts |
| `FLICKER_MS` (`leviculum-nrf/feedback/src/lib.rs:80`) | 50 ms on, 50 ms off | 10 Hz, too fast to count, far below the fusion rate |
| `ERROR_FLASHES` (`leviculum-nrf/feedback/src/lib.rs:84`) | 20 | the error is two seconds of flicker, bounded |
| `RELEASE_BLIP_MS` (`leviculum-nrf/feedback/src/lib.rs:89`) | 60 ms | shorter than a counted blink; Meshtastic's 62 ms click |

| pattern | steps | total |
|---|---|---|
| `mode-tracker` | 3 short | 1150 ms |
| `mode-mesh` | 1 long | 1000 ms |
| `battery-1` to `battery-4` | 1 to 4 short (0 to 25 %, 26 to 50 %, 51 to 75 %, 76 to 100 %) | 150, 650, 1150, 1650 ms |
| `error` | 20 flicker | 1950 ms |
| `notify` | 1 short | 150 ms |
| `release-now` | 1 blip | 60 ms |

The players are `leviculum-nrf/src/feedback.rs`: one per LED
(`led_task`, `leviculum-nrf/src/feedback.rs:202`) and the T1000-E's buzzer
(`buzzer_task`, `leviculum-nrf/src/feedback.rs:266`). The buzzer sounds
`BUZZER_TONE_HZ` (247 Hz, `leviculum-nrf/src/feedback.rs:258`), the B3 of
Meshtastic's own beep on this board, while a step is on. A pattern
raised while another plays on the same sink replaces it. The Pocket V2's
LEDs keep their activity pulses between patterns. Three things play:

- **A short press** (`leviculum-nrf/src/button.rs:158`) plays the
  battery in quarters on the LED and the buzzer, after the display wake.
  A board with no percentage (on USB without a pack, or a pack outside
  the band its cell count implies) plays nothing, since nothing is wrong
  with it, and writes `FEEDBACK pattern=none reason=no-percentage`.
- **A held button reaching 2 s** (`leviculum-nrf/src/button.rs:130`)
  plays `release-now` on the LED and the buzzer.
- **A message for this board's own `lxmf.delivery` destination stored by
  the propagation role** (`leviculum-nrf/src/pn.rs:3315,3595`, a client
  upload and a peer sync; a duplicate is not news) plays `notify` on the
  notification LED where the board has one, else on the LED, and on the
  buzzer. The board has no inbox, so its propagation store is the one
  place a message for its user is kept.

Each play prints `FEEDBACK pattern=<name> sink=led|notify|buzzer`
([Structured event logs](../structured-event-logs.md)); the rig reads
that line, not the light.

Mute is a phone setting, not a gesture. "Find my device" is a Columba
function that sounds the buzzer.

## What moves compared to today

Two things the button did before 889 changed place, and both moves are
built:

- **Display wake moved to any press.** Before, a press of at least 2 s
  turned a dark display on and any press turned a lit one off. Now any
  press turns a dark display on and none turns it off
  (`press_target`, `leviculum-nrf/screen/src/power.rs:21`); the display
  goes dark by itself `DISPLAY_WAKE_MS` (60 s,
  `leviculum-nrf/screen/src/power.rs:16`) after boot or after the last
  press (`DisplayWake`, `leviculum-nrf/screen/src/power.rs:33`). The
  display task owns the state and the minute
  (`leviculum-nrf/src/display.rs:202`); the button task only reports the
  press (`leviculum-nrf/src/button.rs:149`).
- **Announce moved to the double press.** Before, a short press that
  passed the rate limit announced. Now the double press does, under the
  same 30 s limit (`leviculum-nrf/announce-policy/src/button.rs:489`),
  and the short press is status (`action=status`).

The classifier lives in `announce-policy`
(`ButtonGate`, `leviculum-nrf/announce-policy/src/button.rs:334`) and is
host-tested like the debounce. A short press is reported only once
`DOUBLE_PRESS_WINDOW_MS` (400 ms,
`leviculum-nrf/announce-policy/src/button.rs:63`) has passed after its
release without a second press starting, so the status answer costs that
much latency and a double is never first reported as a short. The long
press (2 s) and the very long press (8 s,
`leviculum-nrf/announce-policy/src/button.rs:74`) fire on release and do
nothing else yet; the moment a held button reaches each threshold is
logged as `BTN_HOLD ms=2000` and `BTN_HOLD ms=8000`.

The orders still to come follow this page: the mode switch once Tracker mode exists, then the power off once the
board can sleep.

## Open

- **The buzzer's tone.** Neither Seeed nor Meshtastic publishes the
  transducer's resonance; 247 Hz is the tone this part is known to have
  sounded. A louder one is a rig measurement.
- **Whether the double press's position report also goes out in Mesh
  mode when no recipient is configured.** The reviewer proposes no:
  there is nobody to send it to.

## Not yet built

The mode blink: `mode()` exists and nothing plays it, because there is
no mode yet. The 2 s mode switch and the 8 s power off (both are
recognised and logged, `action=long` and `action=hold8`, and do
nothing; the 8 s mark has no signal of its own), the boot-time factory
reset, the double press's position report and the feedback when its
proof comes back, and the T114's display wake: its ST7789 has no power
control yet and stays lit (`leviculum-nrf/src/st7789.rs:33-34`).

## See also

- [Operating modes](operating-modes.md), for what the long press
  switches between.
- [Announce cadence](announce-cadence.md), for the announces the double
  press adds to.
- [Supported boards](../firmware/boards.md), for each board's button,
  LEDs, buzzer and display.
