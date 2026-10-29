//! The home node and its clients (`docs/src/concepts/loquax-home-node.md`).
//!
//! One always-on loquax, the home node, holds the LXMF identity and keeps
//! every message it receives in an archive with a sequence. The user's other
//! devices are its clients: each has an RNS identity of its own, is admitted
//! once with a single-use token, and pulls the archive by cursor over a link
//! to the home node's own destination, `loquax.home`. Fetching never deletes.
//!
//! - [`code`]: the admission code `loquax home admit` prints.
//! - [`wire`]: the two requests, `/join` and `/sync`, and their answers.
//! - `server`: how the home node answers them, from the store.
//! - `client`: how a client asks, over the driver's link API.
//! - `daemon`: `loquax --daemon`, the home node itself.

use alloc::string::String;

use leviculum_core::{Destination, DestinationType, Direction, Identity};

pub mod code;
pub mod wire;

#[cfg(feature = "std")]
pub mod client;
#[cfg(feature = "history")]
pub mod daemon;
#[cfg(feature = "history")]
pub mod server;

/// The home destination's app name and aspect: `loquax.home`, a SINGLE
/// destination of the home identity. It is never announced; a client learns
/// its key from the admission code and asks for its path.
pub const APP_NAME: &str = "loquax";
/// See [`APP_NAME`].
pub const ASPECT: &str = "home";

/// The admission request: token and device name in, a status out.
pub const JOIN_PATH: &str = "/join";
/// The pull: the client's cursor in, the archive entries after it out.
pub const SYNC_PATH: &str = "/sync";

/// The archive entry kind of a received message. Later kinds (sent,
/// delivered, read) are the later slices' state changes.
pub const KIND_RECEIVED: u8 = 0;

/// The archive's default cap, in megabytes: 1 GiB. Twice the 500 MB a stock
/// lxmd propagation node keeps by default
/// (`reference/LXMF/LXMF/Utilities/lxmd.py:159-163`), which stores everyone's
/// mail; this archive is one person's. A text message packs to well under a
/// kilobyte, so the default is about a million messages, and an always-on
/// home host (a small board, a VPS) has the gigabyte to spare. A machine
/// with less sets `archive_cap_mb` in loquax's config.
pub const DEFAULT_ARCHIVE_CAP_MB: u64 = 1024;

/// The most packed bytes one `/sync` answer carries before it says that more
/// follow. One entry larger than this still goes, alone. 64 KiB is a
/// resource of a few hundred parts at the TCP MDU, quick on a LAN or the
/// internet, which are the media a home node's clients use in this slice.
pub const SYNC_PAGE_BYTES: usize = 64 * 1024;

/// The home destination of `identity`: `loquax.home`, inbound when the
/// identity holds its private key (the home node), outbound otherwise.
pub fn destination(identity: Identity) -> Result<Destination, String> {
    let direction = match identity.has_private_keys() {
        true => Direction::In,
        false => Direction::Out,
    };
    Destination::new(
        Some(identity),
        direction,
        DestinationType::Single,
        APP_NAME,
        &[ASPECT],
    )
    .map_err(|e| alloc::format!("home destination: {e:?}"))
}

/// The LXMF delivery address of the identity whose public key is
/// `public_key`: what the client shows as its own address, and what every
/// message in the archive is addressed to.
pub fn lxmf_address(public_key: &[u8; 64]) -> Result<[u8; 16], String> {
    let identity = Identity::from_public_key_bytes(public_key)
        .map_err(|e| alloc::format!("home identity: {e:?}"))?;
    Destination::new(
        Some(identity),
        Direction::Out,
        DestinationType::Single,
        leviculum_lxmf::node::APP_NAME,
        &[leviculum_lxmf::node::DELIVERY_ASPECT],
    )
    .map(|destination| *destination.hash().as_bytes())
    .map_err(|e| alloc::format!("delivery destination: {e:?}"))
}

/// The hash of the home destination of the identity whose public key is
/// `public_key`.
pub fn destination_hash(public_key: &[u8; 64]) -> Result<[u8; 16], String> {
    let identity = Identity::from_public_key_bytes(public_key)
        .map_err(|e| alloc::format!("home identity: {e:?}"))?;
    destination(identity).map(|destination| *destination.hash().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lxmf_address_matches_the_one_the_engine_registers() {
        let identity = Identity::generate(&mut rand_core::OsRng);
        let registered = crate::engine::delivery_address(&identity).expect("address");
        assert_eq!(
            lxmf_address(&identity.public_key_bytes()).expect("address"),
            registered
        );
    }

    #[test]
    fn the_home_destination_is_the_same_from_either_side() {
        let identity = Identity::generate(&mut rand_core::OsRng);
        let public = identity.public_key_bytes();
        let inbound = destination(identity).expect("in");
        assert_eq!(
            *inbound.hash().as_bytes(),
            destination_hash(&public).expect("out")
        );
    }
}
