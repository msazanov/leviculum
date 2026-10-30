//! Link cipher selection, proof binding and per-link tuning for [`NodeCore`].
//!
//! Kept in its own module so that the shared node files carry only one-line call sites.
//! As a child of `node` it reaches the private fields it extends.
use super::*;
use crate::destination::{ProofBinding, ProofBindingPolicy};

/// Link cipher and proof-binding settings shared by [`NodeCore`] and its builder.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NodeExt {
    /// How each Link binds data proofs; see [`ProofBindingPolicy`].
    pub(crate) proof_binding_policy: ProofBindingPolicy,
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

/// [`LinkStats`] fields read from the `Link` itself: attachment, cipher and keepalive telemetry.
#[derive(Clone, Debug)]
pub(crate) struct LinkStatsExt {
    pub(crate) attached_interface: Option<usize>,
    pub(crate) cipher: crate::LinkCipher,
    // — keepalive telemetry —
    pub(crate) keepalive_ms: u64,
    pub(crate) stale_time_ms: u64,
    pub(crate) keepalives_sent: u64,
    pub(crate) keepalives_acked: u64,
    pub(crate) keepalive_rtt_ms: Option<u64>,
}

impl LinkStatsExt {
    pub(crate) fn of(link: &Link) -> Self {
        Self {
            attached_interface: link.attached_interface(),
            cipher: link.cipher(),
            keepalive_ms: link.keepalive_ms(),
            stale_time_ms: link.stale_time_ms(),
            keepalives_sent: link.keepalives_sent(),
            keepalives_acked: link.keepalives_acked(),
            keepalive_rtt_ms: link.keepalive_rtt_ms(),
        }
    }
}

impl LinkStats {
    /// Interface the link was established on (Python `Link.attached_interface`). The path table
    /// can flip to another interface later (every newer announce may), so this, not the path,
    /// says which neighbour carries the link.
    pub fn attached_interface(&self) -> Option<usize> {
        self.ext.attached_interface
    }

    /// Cipher this link was established with. An application that only wants GCM links on some paths checks it here.
    pub fn cipher(&self) -> crate::LinkCipher {
        self.ext.cipher
    }

    /// Keepalive interval in force for this link (milliseconds)
    pub fn keepalive_ms(&self) -> u64 {
        self.ext.keepalive_ms
    }

    /// Inbound silence after which the link goes stale (milliseconds)
    pub fn stale_time_ms(&self) -> u64 {
        self.ext.stale_time_ms
    }

    /// Proactive keepalives sent since establishment (initiator)
    pub fn keepalives_sent(&self) -> u64 {
        self.ext.keepalives_sent
    }

    /// Keepalive echoes received since establishment (initiator)
    pub fn keepalives_acked(&self) -> u64 {
        self.ext.keepalives_acked
    }

    /// Round trip of the last acknowledged keepalive (initiator), ms.
    /// Unlike [`rtt_ms`](Self::rtt_ms) this is refreshed during the link's
    /// life.
    pub fn keepalive_rtt_ms(&self) -> Option<u64> {
        self.ext.keepalive_rtt_ms
    }
}

impl NodeCoreBuilder {
    /// Apply one explicit proof binding to every Link. This preserves the
    /// historical generic-node behavior; mixed CBC/vanilla profiles should
    /// prefer [`Self::proof_binding_policy`] with `ByCipher`.
    pub fn proof_binding(mut self, binding: ProofBinding) -> Self {
        self.ext.proof_binding_policy = ProofBindingPolicy::Explicit(binding);
        self
    }

    /// Select proof binding per Link cipher (CBC → PacketHash, GCM mode 7 →
    /// TokenTag), or explicitly override that policy. The default remains the
    /// legacy node-wide TokenTag setting for generic API compatibility.
    pub fn proof_binding_policy(mut self, policy: ProofBindingPolicy) -> Self {
        self.ext.proof_binding_policy = policy;
        self
    }

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
    /// Choose what data proofs are bound to on this node.
    ///
    /// Both ends of a link must use the same binding: the sender keys its
    /// receipts by it and the receiver signs it. `TokenTag` (the default) is
    /// cheaper because the value already exists; `PacketHash` is the only mode
    /// that interoperates with vanilla Reticulum.
    pub fn set_proof_binding(&mut self, binding: ProofBinding) {
        self.ext.proof_binding_policy = ProofBindingPolicy::Explicit(binding);
    }

    /// Set the Link proof-binding policy. The legacy `set_proof_binding()`
    /// remains an explicit node-wide override.
    pub fn set_proof_binding_policy(
        &mut self,
        policy: ProofBindingPolicy,
    ) -> Result<(), crate::LinkError> {
        if !self.links.is_empty() {
            return Err(crate::LinkError::InvalidState);
        }
        self.ext.proof_binding_policy = policy;
        Ok(())
    }

    /// Effective proof binding for the node's default outgoing cipher.
    /// Inspect [`proof_binding_policy`](Self::proof_binding_policy) when the
    /// policy is cipher-derived and links may use different ciphers.
    pub fn proof_binding(&self) -> ProofBinding {
        self.ext.proof_binding_policy.for_cipher(self.ext.link_cipher)
    }

    /// Configured policy used to resolve the binding for each Link.
    pub fn proof_binding_policy(&self) -> ProofBindingPolicy {
        self.ext.proof_binding_policy
    }

    pub(crate) fn proof_binding_for_cipher(&self, cipher: crate::LinkCipher) -> ProofBinding {
        self.ext.proof_binding_policy.for_cipher(cipher)
    }

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

    pub fn set_link_send_window_limit(
        &mut self,
        id: &crate::LinkId,
        limit: usize,
    ) -> Result<(), crate::LinkError> {
        let id = self.resolve_link_id(id);
        self.links
            .get_mut(&id)
            .ok_or(crate::LinkError::NotFound)?
            .set_send_window_limit(limit);
        Ok(())
    }

    pub fn set_link_send_pacing_enabled(
        &mut self,
        id: &crate::LinkId,
        enabled: bool,
    ) -> Result<(), crate::LinkError> {
        let id = self.resolve_link_id(id);
        self.links
            .get_mut(&id)
            .ok_or(crate::LinkError::NotFound)?
            .set_send_pacing_enabled(enabled);
        Ok(())
    }

    /// Override this link's keepalive interval (seconds, clamped to the
    /// protocol minimum), or restore the RTT-derived value with `None`.
    ///
    /// Both ends of a link must agree on the interval, otherwise the side
    /// with the shorter stale window closes a quiet link. Returns `false`
    /// for an unknown link.
    pub fn set_link_keepalive(&mut self, link_id: &LinkId, secs: Option<u64>) -> bool {
        let link_id = self.resolve_link_id(link_id);
        let Some(link) = self.links.get_mut(&link_id) else {
            return false;
        };
        link.set_keepalive_override(secs);
        if secs.is_none() {
            // Re-derive from the handshake RTT (0 when the sample is absent,
            // which yields the protocol floor); establishment recomputes it.
            let rtt_us = link.rtt_us().unwrap_or(0);
            link.update_keepalive_from_rtt(rtt_us as f64 / 1_000_000.0);
        }
        true
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
