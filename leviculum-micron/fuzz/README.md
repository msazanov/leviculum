# Fuzz harness for the micron markup parser

Coverage-guided fuzzing (cargo-fuzz / libFuzzer) for `leviculum_micron::parse`.
Companion to `leviculum-core/fuzz/` and `leviculum-std/fuzz/`; a separate crate
because the parser lives in its own crate.

Micron is what lnomad renders from a remote node's page: the bytes a peer
serves go through `String::from_utf8_lossy` and straight into `parse`
(`lnomad/src/browser.rs`). A parser that panics on crafted input is a remote
crash of the browser.

## Requirements

- Rust **nightly**: `rustup toolchain install nightly`
- **cargo-fuzz**: `cargo install cargo-fuzz`
- Build/run against the **glibc** host target, NOT the workspace musl default
  (`.cargo/config.toml` sets musl; ASan wants glibc):
  `--target x86_64-unknown-linux-gnu`

This crate is detached from the repo workspace (its own `[workspace]` table)
and is excluded from `just standard`; regression tests for any crash it finds
live in `leviculum-micron/tests/` instead.

## Target

| target         | parser                    | reachability |
|----------------|---------------------------|--------------|
| `micron_parse` | `leviculum_micron::parse` | every page lnomad fetches from a remote node |

Besides "never panics", every parsed document is checked for two properties
the renderer relies on:

- no string in the model carries a control character or a C1 code point (the
  terminal escape guarantee of Codeberg #281, the same rule as
  `tests/parser_tests.rs::assert_escape_free`);
- every block index in `anchors` and `header_rows` names a block that exists.

## Seeds

`seeds/micron_parse/` holds every distinct string literal the integration
tests in `tests/parser_tests.rs` hand to `parse` (files `test_<sha1 prefix>`)
plus `tests_data/reticulum_readme.mu`. They cover every construct the tests
name, including the hostile CSI/OSC/DCS/C1 inputs.

## Run it

```sh
just fuzz                # every target in every fuzz crate, 60 s each
just fuzz micron_parse   # this one
just fuzz-nightly        # the scheduled run, FUZZ_SECS (120 s) per target
just fuzz-regress        # replay the corpus and the seeds, no fuzzing
```

`scripts/run-fuzz.sh` is what those recipes call; see
`leviculum-core/fuzz/README.md` for the exit-code contract. The working corpus
and any crash input live OUTSIDE this checkout, under
`~/.local/state/leviculum-fuzz/{corpus,findings}/leviculum-micron/micron_parse/`.
