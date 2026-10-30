//! The Entropy seam's stratum-2 type (Codeberg #469, 959).
//!
//! [`Entropy`] is what the orchestration names wherever it draws randomness:
//! the node's RNG, the stamper's, the channel seed. It is a zero-sized type
//! that forwards every draw to one fill function the bind crate registers
//! with [`set_source`] at boot, before anything draws. The orchestration
//! therefore names no chip's RNG, and a bind crate supplies its own without
//! the orchestration changing: the same set-once shape as the interface
//! names ([`crate::iface_bytes::set_names`]).
//!
//! # An unseeded source panics
//!
//! A draw before [`set_source`] ran panics with `entropy drawn before
//! entropy::set_source registered a source`. It never returns zeros or any
//! other fixed bytes: an identity or a link key drawn from them would look
//! like a working node and be a guessable one. No draw runs that early (the
//! binaries register the source right after the interface names, before the
//! node builder draws the identity); if one ever did, the panic handler's
//! post-mortem names the line on the next boot.

use core::cell::Cell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use rand_core::{CryptoRng, RngCore};

/// What a bind crate registers: fill every byte of the slice from its
/// hardware source, or panic. A short fill is not an option the signature
/// leaves open.
pub type Fill = fn(&mut [u8]);

static SOURCE: Mutex<CriticalSectionRawMutex, Cell<Option<Fill>>> = Mutex::new(Cell::new(None));

/// Register the board's entropy source, once, at boot, before anything
/// draws.
///
/// The source must be a cryptographic one: [`Entropy`] is `CryptoRng`
/// because what it forwards to is, and the node's identity and link keys are
/// drawn through it. A second call is a programming error, as for
/// [`crate::iface_bytes::set_names`]: two sources would let one boot's
/// draws come from two places. Debug builds assert it; a release build
/// keeps the later source.
pub fn set_source(fill: Fill) {
    let previous = SOURCE.lock(|cell| cell.replace(Some(fill)));
    debug_assert!(previous.is_none(), "entropy::set_source called twice");
}

/// The draw every [`Entropy`] method ends in, over the source it was handed
/// rather than the static, so the unset case is testable on its own.
fn draw(source: Option<Fill>, dest: &mut [u8]) {
    match source {
        Some(fill) => fill(dest),
        None => panic!("entropy drawn before entropy::set_source registered a source"),
    }
}

/// The board's entropy, as the orchestration draws it: a zero-sized handle
/// over the source [`set_source`] registered. Panics on a draw before that
/// (the module documentation says why).
#[derive(Clone, Copy, Debug, Default)]
pub struct Entropy;

impl Entropy {
    fn fill(dest: &mut [u8]) {
        draw(SOURCE.lock(|cell| cell.get()), dest);
    }
}

impl RngCore for Entropy {
    fn next_u32(&mut self) -> u32 {
        let mut buf = [0u8; 4];
        Self::fill(&mut buf);
        u32::from_le_bytes(buf)
    }

    fn next_u64(&mut self) -> u64 {
        let mut buf = [0u8; 8];
        Self::fill(&mut buf);
        u64::from_le_bytes(buf)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        Self::fill(dest);
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        Self::fill(dest);
        Ok(())
    }
}

// As cryptographic as the source `set_source` requires it to be.
impl CryptoRng for Entropy {}

// The orchestration draws through the Entropy seam, which is
// `rand_core::RngCore`; this states that the type is one, as `rng.rs` does
// for the bind crate's implementer.
const _: () = {
    const fn is_entropy<T: crate::seams::Entropy>() {}
    is_entropy::<Entropy>();
};

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    /// A source whose every byte is its index plus one, so a forwarded draw
    /// is told apart from zeros and from a draw of the wrong length.
    fn counting(dest: &mut [u8]) {
        for (i, byte) in dest.iter_mut().enumerate() {
            *byte = i as u8 + 1;
        }
    }

    /// Unset draws panic, a registered source's bytes come back through
    /// every method, and a second registration is refused in a debug build.
    /// One test, because the source is one static: a second test touching
    /// it would race this one.
    #[test]
    fn draws_panic_unset_then_return_the_registered_sources_bytes() {
        let unset = std::panic::catch_unwind(|| Entropy.next_u32());
        assert!(unset.is_err());

        set_source(counting);
        let mut rng = Entropy;
        assert_eq!(rng.next_u32(), u32::from_le_bytes([1, 2, 3, 4]));
        assert_eq!(rng.next_u64(), u64::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8]));
        let mut buf = [0u8; 5];
        rng.fill_bytes(&mut buf);
        assert_eq!(buf, [1, 2, 3, 4, 5]);
        let mut buf = [0u8; 3];
        assert!(rng.try_fill_bytes(&mut buf).is_ok());
        assert_eq!(buf, [1, 2, 3]);

        let again = std::panic::catch_unwind(|| set_source(counting));
        assert_eq!(again.is_err(), cfg!(debug_assertions));
    }

    /// The unset draw itself, with the message an operator reads in the
    /// post-mortem.
    #[test]
    #[should_panic(expected = "entropy drawn before entropy::set_source registered a source")]
    fn a_draw_without_a_source_panics_and_says_so() {
        draw(None, &mut [0u8; 4]);
    }
}
