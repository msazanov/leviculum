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
`leviculum-nrf/src/bin/solarnode.rs:576`,
`leviculum-nrf/src/bin/rak4631.rs:530`,
`leviculum-nrf/src/bin/t1000e.rs:525`). The T114 and the Solar Node used
to say `[BTN] none` only because nobody had wired the task: neither pin
is shared with anything the firmware drives, and the Solar Node's touch
pad is a second pin (`TouchButton`, `leviculum-nrf/src/boards/solarnode.rs:100`)
that stays unused. On the T1000-E the long press is also the product's
power button (`leviculum-nrf/src/boards/t1000e.rs:282-286`).

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
  press (`leviculum-nrf/src/button.rs:156`).
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

The orders still to come follow this page: the feedback grammar, then
the mode switch once Tracker mode exists, then the power off once the
board can sleep.

## Open

- **The exact blink timings** of the grammar.
- **Whether the double press's position report also goes out in Mesh
  mode when no recipient is configured.** The reviewer proposes no:
  there is nobody to send it to.

## Not yet built

The 2 s mode switch and the 8 s power off (both are recognised and
logged, `action=long` and `action=hold8`, and do nothing), the boot-time
factory reset, the feedback grammar (so the short press's LED and buzzer
status, and the "release now" signal at 2 s), the double press's
position report, and the T114's display wake: its ST7789 has no power
control yet and stays lit (`leviculum-nrf/src/st7789.rs:33-34`).

## See also

- [Operating modes](operating-modes.md), for what the long press
  switches between.
- [Announce cadence](announce-cadence.md), for the announces the double
  press adds to.
- [Supported boards](../firmware/boards.md), for each board's button,
  LEDs, buzzer and display.
