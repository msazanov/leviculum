//! The board firmware's orchestration stratum (Codeberg #469).
//!
//! Stratum 2 of `docs/src/concepts/firmware-layers.md` (Firmware layers):
//! the node loop, once, over seams that each chip family's bind crate
//! implements. Stratum 1 below it is policy (`leviculum-core`, the
//! host-tested member crates); stratum 3 above it is a bind crate with pins,
//! backends and `main` (`leviculum-nrf` today).
//!
//! [`seams`] declares every seam of the page's section 3. Every seam has a
//! host fake in `fakes` (test builds only), so the modules that arrive are
//! host-tested like the policy crates. Step 1 moves the leaf modules that
//! need no backend but a seam: [`log`].
//!
//! What the crate may depend on is held by two guards rather than by this
//! comment: `just firmware-no-hal` refuses a HAL or chip crate in its
//! `cargo tree`, `just firmware-no-chip-cfg` a BSP `cfg` or a chip name in
//! its sources outside doc comments.

#![no_std]

extern crate alloc;

pub mod log;
pub mod seams;

#[cfg(test)]
mod fakes;
