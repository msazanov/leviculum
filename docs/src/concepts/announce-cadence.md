# Announce cadence

A board decides for itself how often it announces. There is no stationary
or mobile profile to pick at flash time: a board observes whether it moves
and announces every five minutes while it does and once an hour while it
does not, with an immediate announce when movement resumes or an unknown
neighbour is heard, and the whole set stretched when the PHY makes it
unaffordable (Codeberg #401, decided 2026-09-15). The rule and its
constants live in one place, `leviculum-nrf/announce-policy/src/cadence.rs`,
whose module doc is the reference for the five rules.

## Movement has to be proven

"Moving" is not a single observation. `MovementDetector` in that file
counts a board as moving only when three consecutive usable fixes lie past
the 50 m threshold (`MOVEMENT_MIN_CONSECUTIVE_FIXES`), a fix without a good
HDOP counts as no fix at all, and the fast state ends 15 minutes after the
last confirmation (`MOVEMENT_CONFIRM_TIMEOUT_MS`). The reason is the board
on a mast or a tree: its position wanders while it stands still, and a
single implausible fix must not be able to switch it to the fast cadence
and spend the channel.

## The accelerometer gates the proof, it does not replace it

The SenseCAP T1000-E carries a QMA6100P accelerometer, so the obvious
question is whether it should say "moving" directly (Codeberg #457). Lew's
decision of 2026-10-20: **accelerometer motion gates the GNSS movement
proof. It neither replaces the proof nor adds to it.**

- *Not a replacement:* a tilt, a gust of wind on a mast, a truck passing
  the post or a hand brushing the case all move an accelerometer. Letting
  any of them switch the cadence is exactly the failure the three-fix proof
  was built against.
- *Not an alternative path (OR):* for the same reason. Either source alone
  would then be enough, and the weaker one decides.
- *A gate (AND):* the displacement proof stays the only thing that decides
  "moving", exactly as #401 specifies. The accelerometer decides whether it
  is worth running at all: while the board is still, the GNSS receiver can
  sleep, and motion wakes it so the proof runs as it does today.

What that buys is energy, not a different cadence: a still tracker no
longer keeps a receiver awake to prove, every few seconds, that it has not
moved.

## In two steps

1. **The sensor reads and reports** (#457, step one). The T1000-E image
   drives the part and prints `MOTION state=moving|still hold_ms=<n>
   src=qma6100p` on each change, with `MOTION_INIT ok|absent` at boot
   ([Structured event logs](../structured-event-logs.md)). Moving is one
   sample (of four a second) differing from the last by more than 30 mg;
   still is 30 s without such a sample. Nothing reads the state: the
   cadence, the GNSS task and telemetry are untouched. The driver and the
   rule are `leviculum-nrf/motion`, host-tested.
2. **The receiver sleeps while still** (a later pass). The GNSS task puts
   the receiver to sleep on `still` and wakes it on `moving`. The proof's
   inputs and thresholds do not change; it simply gets no fixes while the
   board is still, which is what a still board would have produced anyway.

A board without an accelerometer, or one whose part does not answer
(`MOTION_INIT absent`), keeps its receiver awake and runs the proof as
today. The gate only ever removes work; it never adds a way to become
"moving".

## See also

- [Regulatory airtime](regulatory-airtime.md), for rule 5's duty budget.
- [Telemetry](telemetry.md), whose tracker policy supplies the distance and
  HDOP thresholds the proof reuses.
