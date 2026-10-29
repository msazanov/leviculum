//! The home node and its clients (`docs/src/concepts/loquax-home-node.md`).
//!
//! One always-on loquax, the home node, holds the LXMF identity and keeps
//! every message it receives in an archive with a sequence. The user's other
//! devices are its clients: each has an RNS identity of its own, is admitted
//! once with a single-use token, and pulls the archive by cursor over a link
//! to the home node's own destination, `loquax.home`. Fetching never deletes.
//!
//! - [`code`]: the admission code `loquax home admit` prints.
//! - [`wire`]: the four requests, `/join`, `/sync`, `/send` and `/mark`,
//!   and their answers.
//! - [`outgoing`]: the home node's outbox policy, direct at once, a
//!   propagated copy on failure, direct retries for seven days.
//! - `server`: how the home node answers them, from the store.
//! - `client`: how a client asks, over the driver's link API.
//! - `daemon`: `loquax --daemon`, the home node itself.
//! - `lock`: one registrar of the delivery destination per home.

use alloc::string::String;

use leviculum_core::{Destination, DestinationType, Direction, Identity};

pub mod code;
pub mod outgoing;
pub mod wire;

#[cfg(feature = "std")]
pub mod client;
#[cfg(feature = "history")]
pub mod daemon;
#[cfg(feature = "std")]
pub mod lock;
#[cfg(feature = "tui")]
pub mod qr;
#[cfg(feature = "history")]
pub mod runner;
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
/// A message to send: the client's own id and the message in, the message
/// id out. The home node signs it, keeps it in its outbox and delivers.
pub const SEND_PATH: &str = "/send";

/// A client's word on messages, read or deleted: the flag and the message
/// ids in, a status out. The home node keeps it as an archive entry, which
/// every client applies on its next pull.
pub const MARK_PATH: &str = "/mark";

/// The archive entry kind of a received message, packed as it came.
pub const KIND_RECEIVED: u8 = 0;
/// A message a client handed in with `/send`, packed as the home node
/// signed it.
pub const KIND_SENT: u8 = 1;
/// A propagation node took a copy of a sent message. This and the kinds
/// below carry the message id as their bytes, nothing else.
pub const KIND_PROPAGATED: u8 = 2;
/// A direct delivery proof came for a sent message.
pub const KIND_DELIVERED: u8 = 3;
/// The retries of a sent message ended with no propagation node ever
/// taking it and no proof: it never left the home node.
pub const KIND_FAILED: u8 = 4;
/// The direct retries of a propagated message ended without a proof; the
/// copy on the propagation node may still reach the recipient.
pub const KIND_RETRIES_ENDED: u8 = 5;
/// A client's `/mark`: its bytes are the request as the client sent it, a
/// flag and the message ids ([`wire::Mark`]).
pub const KIND_MARK: u8 = 6;

/// What a client shows for a state entry of a message it sent through the
/// home node: the router state its bubble takes, `None` for an entry that
/// changes nothing on screen (direct retries that ended while a propagated
/// copy waits). The sent entry is `Sent`: the message left the device and
/// the home node has it, as a router's `Sent` says a hop took it.
pub fn shown_state(kind: u8) -> Option<leviculum_lxmf::router::MessageState> {
    use leviculum_lxmf::router::MessageState;
    match kind {
        KIND_SENT => Some(MessageState::Sent),
        KIND_PROPAGATED => Some(MessageState::AwaitingCollection),
        KIND_DELIVERED => Some(MessageState::Delivered),
        KIND_FAILED => Some(MessageState::Failed),
        _ => None,
    }
}

/// The word `loquax fetch` prints for an archive entry of a message sent
/// through the home node; `None` for a received message.
pub fn sent_word(kind: u8) -> Option<&'static str> {
    Some(match kind {
        KIND_SENT => "accepted",
        KIND_PROPAGATED => "propagated",
        KIND_DELIVERED => "delivered",
        KIND_FAILED => "failed",
        KIND_RETRIES_ENDED => "retries-ended",
        _ => return None,
    })
}

/// How long an admission token stays good, by default: a day, time to
/// carry the code to the other device, short enough that a code left in a
/// chat log or a screenshot is dead by the next. `home_token_hours` in the
/// config changes it.
pub const DEFAULT_TOKEN_HOURS: u64 = 24;

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

/// An admission token's life in seconds, as the config sets it.
pub fn token_ttl_secs(config: &crate::config::Config) -> i64 {
    let hours = config.home_token_hours.unwrap_or(DEFAULT_TOKEN_HOURS);
    i64::try_from(hours.saturating_mul(3600)).unwrap_or(i64::MAX)
}

/// The outbox's timing, as the config sets it.
pub fn policy(config: &crate::config::Config) -> outgoing::Policy {
    let secs = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
    outgoing::Policy {
        retry_secs: secs(
            config
                .outbox_retry_secs
                .unwrap_or(outgoing::DEFAULT_RETRY_SECS),
        ),
        horizon_secs: secs(
            config
                .outbox_days
                .unwrap_or(outgoing::DEFAULT_DAYS)
                .saturating_mul(24 * 3600),
        ),
    }
}

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
