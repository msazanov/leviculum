# The embassy pin rule

The board firmware runs on the embassy async runtime, and every member of
the `leviculum-nrf` workspace that names an embassy crate names the same
version of it. This page states the pins and the rule; it is the
dependency policy for the firmware workspace, and step 0 of [Firmware
layers](../concepts/firmware-layers.md) is where the rule was written
down (Codeberg #469).

## The pins today

Declared versions, read from the manifests; the resolved version is what
`leviculum-nrf/Cargo.lock` holds.

| crate | declared | resolved | declared in |
|---|---|---|---|
| `embassy-executor` | 0.9 | 0.9.1 | `leviculum-nrf/Cargo.toml`, `leviculum-nrf/firmware/Cargo.toml` |
| `embassy-sync` | 0.7 | 0.7.2 | `leviculum-nrf/Cargo.toml`, `leviculum-nrf/ble-tx/Cargo.toml`, `leviculum-nrf/firmware/Cargo.toml` |
| `embassy-time` | 0.5 | 0.5.1 | `leviculum-nrf/Cargo.toml`, `leviculum-nrf/firmware/Cargo.toml`, `leviculum-lr11xx/Cargo.toml` (dependency and dev-dependency) |
| `embassy-nrf` | 0.9 | 0.9.0 | `leviculum-nrf/Cargo.toml` |
| `embassy-usb` | 0.5 | 0.5.1 | `leviculum-nrf/Cargo.toml` |
| `embassy-futures` | 0.1 | 0.1.2 | `leviculum-nrf/Cargo.toml` |
| `embassy-embedded-hal` | 0.5 | 0.5.0 | `leviculum-nrf/Cargo.toml` |

`leviculum-firmware` (`leviculum-nrf/firmware/`), the orchestration
stratum, takes executor, sync and time at these pins and nothing else of
embassy. The LR11xx driver is its own workspace, but the
firmware pulls it by path, so its `embassy-time` resolves in this graph
and the rule covers it.

One second copy is in the lock and is not ours: `nrf-softdevice` at its
git pin brings `embassy-sync` 0.6.2 for its own internals. The rule is
about what a member declares; that copy goes when the SoftDevice binding
moves to 0.7, and it is the reason the guard reads manifests and not the
lock.

## The rule

1. **One version set per workspace.** Every manifest that names an
   embassy crate names it at one semver-compatible version (`0.7` and
   `0.7.2` are one; `0.7` and `0.8` are two). Two versions of
   `embassy-time` link two time drivers, two of `embassy-sync` give two
   channel types that do not unify, and either compiles.
2. **Bumped in one commit across all members.** A version bump of an
   embassy crate touches every manifest that names it, in the same
   commit, so no commit in history carries a split set.
3. **No git revisions in master.** No `[patch.crates-io]` entry and no
   embassy dependency comes from a git source. A git revision pins a
   build to a branch somebody else moves. The counterexample is the
   fork's `leviculum-fw/Cargo.toml`, lines 146 to 154 on
   `cedrivard/Leviculum` `rp2350-m9-net` at `7a699446`: embassy-executor
   0.10 and embassy-sync 0.8 with a `[patch.crates-io]` git revision on
   top. The second and third family join on a released set or not at
   all.

The existing `[patch.crates-io]` entry, `nmea0183` from `vendor/`, is a
path, and allowed. `nrf-softdevice` is a git dependency outside the
embassy name space, pinned for the reason its comment in
`leviculum-nrf/Cargo.toml` gives (the crates.io release is stale); it is
the one standing exception, and it is not a patch.

## The guard

`just embassy-pins` runs `scripts/check-embassy-pins.sh`: it reads every
`Cargo.toml` under `leviculum-nrf/` and the LR11xx driver's, and fails on
an embassy crate named at two versions, on an embassy crate from git, and
on any git entry in a `[patch]` table, naming the manifest. It runs its
own positive control first on a temporary copy of the real manifests (an
injected `embassy-time = "0.4"` and an injected git patch, each of which
must be refused) and is part of `just guards`.
