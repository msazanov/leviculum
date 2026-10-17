# Third-party code in this repository

Leviculum is AGPL-3.0-or-later (see [LICENSE](LICENSE)). This file names
every piece of someone else's code the repository carries, under which
licence, and whether any of it reaches a binary we build or ship.

The Rust crates the binaries link from crates.io are listed, with their
licence texts, in [THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES), which is
generated from `Cargo.lock` and travels inside every artifact. This file
covers what that one does not: the git submodules, the crates vendored
into `vendor/`, and the one binary blob.

`scripts/check-third-party.py` (in `just guards`) refuses a tree with a
`.gitmodules` entry or a `[patch.crates-io]` path that is not named below.

## Submodules under `reference/`: read, never built, never shipped

`reference/` holds the Python and C++ stacks we interoperate with, pinned
so that `file:line` citations into them mean one thing. Nothing under it
is compiled into any Leviculum binary or copied into any package:

- `scripts/check-plain-clone.sh` (in `just guards`) refuses a compile-time
  `include_str!`/`include_bytes!` into `reference/`, so every crate builds
  from a clone without submodules.
- The forge pipelines clone without submodules (`scripts/ci-gate.sh`), and
  the published source tarball is `git archive`, which carries a submodule
  as a gitlink and none of its content.
- The packaging scripts (`scripts/build-deb.sh`, `packaging/`, the lnflash
  bundle) read no path under `reference/`.

What reads them is the test suite at runtime (the Python interop tests run
`python3` against `reference/Reticulum` and `reference/LXMF`) and the
citation checker (`leviculum-std/tests/doc_citations.rs`). A plain clone
initialises none of them; README asks for `reference/Reticulum` only, for
the interop tests.

| Path | Upstream | Pin | Licence | Role |
|---|---|---|---|---|
| `reference/Reticulum` | [reallew/Reticulum](https://github.com/reallew/Reticulum), branch `libreticulum-patches`, a fork of markqvist/Reticulum | `d5e62d4e` (1.3.5 plus one test-harness patch) | Reticulum License (Mark Qvist) | Reference and interop peer at test time |
| `reference/LXMF` | [markqvist/LXMF](https://github.com/markqvist/LXMF) | `795fdaa2` (1.1.0) | Reticulum License (Mark Qvist) | Reference and interop peer at test time |
| `reference/LXST` | [markqvist/LXST](https://github.com/markqvist/LXST) | `1194c901` (0.4.4-6) | CC BY-NC-ND 4.0 | Citation target only; nothing runs it |
| `reference/RNode_Firmware` | [markqvist/RNode_Firmware](https://github.com/markqvist/RNode_Firmware) | `d39339f8` (1.86-1) | GPL-3.0 | Citation target only; never built here |

The pin column is what the gitlink names at the time of writing;
`git submodule status` is authoritative, and `just check-submodules`
refuses a checkout that disagrees with it.

## Crates vendored under `vendor/`: linked into binaries

Each is a published crate with a small local patch, substituted for the
crates.io release through `[patch.crates-io]`. Their licences permit
modification and redistribution; each directory keeps the upstream
licence file, and each crate appears in THIRD-PARTY-NOTICES like any
other dependency.

| Path | Upstream | Version | Licence | Patch | Linked into |
|---|---|---|---|---|---|
| `vendor/crossterm` | [crossterm-rs/crossterm](https://github.com/crossterm-rs/crossterm) | 0.29.0 | MIT | Mouse side buttons 8/9 surface as `MouseButton::Back` / `Forward` instead of being dropped in `parse_cb` | lnomad (host) |
| `vendor/termbg` | [dalance/termbg](https://github.com/dalance/termbg) | 0.6.2 | MIT OR Apache-2.0 | One line: the crossterm requirement raised from 0.28 to 0.29, so lnomad links one crossterm | lnomad (host) |
| `vendor/nmea0183` | [nsforth/nmea0183](https://github.com/nsforth/nmea0183) | 0.6.0 | BSD-3-Clause | The float parse replaced by a fixed-point one (Codeberg #386) | nRF firmware |

The patches are described where they are wired in: the workspace
`Cargo.toml` (crossterm, termbg) and `leviculum-nrf/Cargo.toml`
(nmea0183). The vendored `Cargo.toml` files keep upstream's name, version
and repository URL; this table is where the difference is recorded.

## Binary blob

| Path | Origin | Licence | Role |
|---|---|---|---|
| `lnflash/payload/t114/s140_nrf52_7.3.0_softdevice.hex` | Nordic Semiconductor S140 SoftDevice 7.3.0 | Nordic licence, `s140_nrf52_7.3.0_license-agreement.txt` beside it | Shipped in the lnflash bundle, with its licence, for the board that needs it |
