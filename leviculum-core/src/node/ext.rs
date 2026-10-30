//! Link cipher selection and path recovery for [`NodeCore`].
//!
//! Kept in its own module so that the shared node files carry only one-line call sites.
//! As a child of `node` it reaches the private fields it extends.
use super::*;

/// Link cipher settings shared by [`NodeCore`] and its builder.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NodeExt {
    pub(crate) link_cipher: crate::LinkCipher,
    /// Accept standard-CBC link requests besides `link_cipher` (default off).
    pub(crate) accept_cbc_links: bool,
    /// Cipher for the next `connect` only (see `connect_with_cipher`); `link_cipher` otherwise.
    pub(crate) next_link_cipher: Option<crate::LinkCipher>,
}

impl NodeExt {
    pub(crate) fn outgoing_cipher(&self) -> crate::LinkCipher {
        self.next_link_cipher.unwrap_or(self.link_cipher)
    }
}

impl NodeCoreBuilder {
    pub fn link_cipher(mut self, cipher: crate::LinkCipher) -> Self {
        self.ext.link_cipher = cipher;
        self
    }

    /// Also accept inbound links that ask for standard CBC (see [`Node::set_accept_cbc_links`]).
    pub fn accept_cbc_links(mut self, accept: bool) -> Self {
        self.ext.accept_cbc_links = accept;
        self
    }
}

impl<R: CryptoRngCore, C: Clock, S: Storage> NodeCore<R, C, S> {
    /// As responder also accept links that ask for standard CBC (links that cross stock Reticulum transports can only be CBC).
    /// Does not change the cipher this node asks for on its own links, and never lets an initiator's GCM request fall back to CBC.
    pub fn set_accept_cbc_links(&mut self, accept: bool) {
        self.ext.accept_cbc_links = accept;
    }

    /// Explicit cipher policy, set before establishing links. No automatic downgrade.
    pub fn set_link_cipher(&mut self, cipher: crate::LinkCipher) -> Result<(), crate::LinkError> {
        if !self.links.is_empty() {
            return Err(crate::LinkError::InvalidState);
        }
        self.ext.link_cipher = cipher;
        Ok(())
    }

    /// Request a path on the given interfaces only, ignoring the per-destination request limiter.
    ///
    /// Used after a route failed: the interface that carried the dead route is left out, so a
    /// neighbour that still lists the dead route cannot answer first and put it back.
    pub fn request_path_on(
        &mut self,
        dest_hash: &DestinationHash,
        interfaces: &[usize],
    ) -> crate::transport::TickOutput {
        let mut tag = [0u8; TRUNCATED_HASHBYTES];
        self.rng.fill_bytes(&mut tag);
        for &idx in interfaces {
            if let Err(e) =
                self.transport
                    .request_path_unthrottled(dest_hash.as_bytes(), Some(idx), &tag)
            {
                crate::tracing::debug!(
                    "Failed to send path request for <{}> on interface {}: {}",
                    HexShort(dest_hash.as_bytes()),
                    idx,
                    e
                );
            }
        }
        self.process_events_and_actions()
    }
}
