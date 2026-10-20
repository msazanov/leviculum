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
`leviculum-nrf/src/boards/t1000e.rs:276,281`). The Pocket (`rak4631.rs`)
and the Solar Node add a `LedNotificationPin`
(`leviculum-nrf/src/boards/rak4631.rs:55`,
`leviculum-nrf/src/boards/solarnode.rs:94`), the T1000-E a `BuzzerPin`
(`leviculum-nrf/src/boards/t1000e.rs:286`). The Pocket drives an OLED
and the T114 an ST7789 (`leviculum-nrf/src/bin/t114.rs:496`).

Today only the Pocket and the T1000-E bring the button up; the T114 and
the Solar Node say `[BTN] none` (`leviculum-nrf/src/button.rs:16`). On
the T1000-E the long press is the power button and stays unassigned
(`leviculum-nrf/src/bin/t1000e.rs:521-522`).

## The gestures

| gesture | does |
|---|---|
| short press | Status. The display wakes for one minute and goes dark by itself; there is no display switch. The LED blinks mode and battery, the buzzer joins where present. No announce. |
| double press | Announce, both identities (what the short press does today, 869), plus one immediate position report when a recipient is configured. Feedback when the proof comes back. |
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

Two things the button does today change place:

- **Display wake moves to the short press.** Today a press of at least
  2 s turns a dark display on and any press turns a lit one off
  (`press_target`, `leviculum-nrf/screen/src/power.rs:17-25`; the 2 s
  threshold is `LONG_PRESS_MIN_MS`, `leviculum-nrf/screen/src/power.rs:12`).
  The 2 s press becomes the mode switch, and the display goes dark on its
  own after a minute instead of on a press.
- **Announce moves to the double press.** Today a short press that
  passes the rate limit announces (`PressAction::Announce`,
  `leviculum-nrf/announce-policy/src/button.rs:259`; the announce itself
  is 869, Refs #459). The short press becomes status only.

The implementation orders follow this page: the gesture classifier in
`announce-policy` first (double press, 2 s and 8 s thresholds,
host-tested), then the display rule in `leviculum-nrf/screen`, then the
feedback grammar, then the mode switch once Tracker mode exists.

## Open

- **The exact blink timings** of the grammar.
- **Whether the double press's position report also goes out in Mesh
  mode when no recipient is configured.** The reviewer proposes no:
  there is nobody to send it to.

## Not yet built

Everything except the single press that announces and the long press
that wakes the Pocket's display: the double press, the 2 s mode switch,
the 8 s power off, the boot-time factory reset, the feedback grammar,
the display's one minute timeout, and the button on the T114 and the
Solar Node.

## See also

- [Operating modes](operating-modes.md), for what the long press
  switches between.
- [Announce cadence](announce-cadence.md), for the announces the double
  press adds to.
- [Supported boards](../firmware/boards.md), for each board's button,
  LEDs, buzzer and display.
