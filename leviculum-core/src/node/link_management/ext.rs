//! Per-link cipher (CBC or GCM mode 7), cipher-aware proof binding, tunnel datagrams and bounded
//! LRRTT retries for GCM links.
//!
//! A child of `link_management`, so it reaches that module's private methods and the fields of
//! `NodeCore`; the shared file only calls into it.
use super::*;

/// The 32 bytes a data proof is bound to.
///
/// This used to be SHA-256 of the packet's hashable part, which meant a second
/// full SHA-256 pass over every payload byte at both ends of every data packet.
/// On a CPU without SHA extensions that pass was the largest single cost on the
/// receive path (measured: removing it alone took the bench from ~13.7 to
/// ~21.8 MiB/s). The token's HMAC tag is already computed during encrypt and
/// verify, is the same size, covers the same payload and is keyed with the link
/// key — so binding proofs to it drops the pass without weakening anything: the
/// proof is still signed with the peer's identity key, and an observer that
/// reads a tag off the wire still cannot forge a proof with it.
///
/// Packets that carry no token fall back to the hash, so an unexpected packet
/// degrades to the old behaviour instead of mis-binding a receipt.
///
/// `binding` selects the legacy mode explicitly; see [`crate::destination::ProofBinding`].
pub(super) fn proof_identifier(raw: &[u8], binding: ProofBinding) -> [u8; 32] {
    match binding {
        ProofBinding::TokenTag => {
            crate::packet::packet_token_tag(raw).unwrap_or_else(|| packet_hash(raw))
        }
        ProofBinding::PacketHash => packet_hash(raw),
    }
}

pub(super) fn cipher_proof_identifier(
    raw: &[u8],
    binding: ProofBinding,
    cipher: crate::LinkCipher,
) -> [u8; 32] {
    if binding == ProofBinding::TokenTag && cipher == crate::LinkCipher::Gcm {
        // A signed 32-byte identifier containing the nonce and full 128-bit
        // GCM tag. The authenticated tag binds the ciphertext, AAD and nonce.
        let header_len = if raw.first().is_some_and(|b| b & 0x40 != 0) {
            35
        } else {
            19
        };
        if raw.len() >= header_len + crate::crypto::link_gcm::OVERHEAD {
            return gcm_proof_id(&raw[header_len..]);
        }
    }
    proof_identifier(raw, binding)
}

/// Receive-side [`cipher_proof_identifier`]: the same value from the parsed packet, whose data is
/// the token, so the raw bytes are not needed; `raw_hash` is the hash the transport already took.
pub(super) fn link_proof_id(
    packet: &Packet,
    raw_hash: Option<&[u8; 32]>,
    binding: ProofBinding,
    cipher: crate::LinkCipher,
) -> [u8; 32] {
    if binding == ProofBinding::TokenTag {
        let token = packet.data.as_slice();
        if cipher == crate::LinkCipher::Gcm && token.len() >= crate::crypto::link_gcm::OVERHEAD {
            return gcm_proof_id(token);
        }
        if let Some(tag) = crate::packet::token_tag(packet.context, token) {
            return tag;
        }
    }
    raw_hash.copied().unwrap_or_else(|| packet_hash_from_packet(packet))
}

/// A signed 32-byte identifier containing the nonce and full 128-bit GCM tag. The
/// authenticated tag binds the ciphertext, AAD and nonce.
fn gcm_proof_id(token: &[u8]) -> [u8; 32] {
    let mut id = [0u8; 32];
    // Receipt lookup truncates to 16 bytes: keep the entire GCM tag
    // first, not a predictable counter plus a truncated tag.
    id[..16].copy_from_slice(&token[token.len() - 16..]);
    id[16..28].copy_from_slice(&token[..12]);
    id[28..].copy_from_slice(b"GCM1");
    id
}

impl ReceiptTracker {
    /// [`Self::register`] keyed by the proof identifier of the Link's cipher.
    pub(super) fn register_bound(
        &mut self,
        packet_data: &[u8],
        link_id: LinkId,
        sequence: u16,
        now_ms: u64,
        binding: ProofBinding,
        cipher: crate::LinkCipher,
    ) {
        let id = cipher_proof_identifier(packet_data, binding, cipher);
        self.entries
            .retain(|e| !(e.link_id == link_id && e.sequence == Some(sequence)) && e.full_hash != id);
        self.register(packet_data, link_id, sequence, now_ms);
        self.rebind_last(packet_data, id);
    }

    /// [`Self::register_raw`] keyed by the proof identifier of the Link's cipher.
    pub(super) fn register_raw_bound(
        &mut self,
        packet_data: &[u8],
        link_id: LinkId,
        deadline_ms: u64,
        binding: ProofBinding,
        cipher: crate::LinkCipher,
    ) -> [u8; 32] {
        let id = cipher_proof_identifier(packet_data, binding, cipher);
        self.entries.retain(|e| e.full_hash != id);
        self.register_raw(packet_data, link_id, deadline_ms);
        self.rebind_last(packet_data, id);
        id
    }

    /// `register*` appends its entry keyed by `packet_hash`; re-key it with the cipher's identifier.
    fn rebind_last(&mut self, packet_data: &[u8], id: [u8; 32]) {
        let e = self.entries.last_mut().expect("register just pushed an entry");
        debug_assert_eq!(e.full_hash, packet_hash(packet_data), "upstream register no longer appends its entry last");
        e.full_hash = id;
        e.truncated_hash.copy_from_slice(&id[..TRUNCATED_HASHBYTES]);
    }
}

impl<R: CryptoRngCore, C: Clock, S: Storage> NodeCore<R, C, S> {
    /// Whether a link request signalling `mode` is acceptable to this node as responder: its own cipher, or standard CBC when
    /// [`Self::set_accept_cbc_links`] is on (links that cross stock Reticulum transports; those only carry CBC).
    /// The initiator still rejects any mode other than the one it asked for, so this cannot be used to downgrade a link that asked for GCM.
    pub(super) fn link_mode_allowed(&self, mode: u8) -> bool {
        mode == self.ext.link_cipher as u8
            || (self.ext.accept_cbc_links && mode == crate::LinkCipher::Cbc as u8)
    }

    /// [`Self::connect_with_first_hop_timeout`] with an explicit cipher for this link only.
    ///
    /// # Errors
    /// As [`Self::connect`].
    pub fn connect_with_cipher(
        &mut self,
        dest_hash: DestinationHash,
        dest_signing_key: &[u8; 32],
        first_hop_timeout_ms: Option<u64>,
        cipher: crate::LinkCipher,
    ) -> Result<(LinkId, bool, crate::transport::TickOutput), LinkError> {
        self.ext.next_link_cipher = Some(cipher);
        let r = self.connect_with_first_hop_timeout(dest_hash, dest_signing_key, first_hop_timeout_ms);
        self.ext.next_link_cipher = None;
        r
    }

    /// Receipt for a retransmitted channel packet, bound by the Link's own cipher. `false` if the Link vanished.
    pub(super) fn register_retransmit_receipt(
        &mut self,
        packet: &[u8],
        link_id: LinkId,
        sequence: u16,
        now_ms: u64,
    ) -> bool {
        let Some(cipher) = self.links.get(&link_id).map(|l| l.cipher()) else {
            // The core is serialized, so this should only
            // be reachable after an invariant violation.
            // Never guess a node-global cipher/binding for
            // a receipt whose Link no longer exists.
            tracing::warn!(
                link = %HexShort(link_id.as_bytes()),
                sequence,
                "channel retransmit Link vanished before receipt registration"
            );
            return false;
        };
        self.receipt_tracker.register_bound(
            packet,
            link_id,
            sequence,
            now_ms,
            self.proof_binding_for_cipher(cipher),
            cipher,
        );
        true
    }

    /// Send one tunnelled datagram on an established link: sealed by the link cipher, sent once.
    ///
    /// Unlike [`Self::send_packet_on_link`] it keeps no receipt (a receipt per packet is state and a
    /// timer per packet, which an IP tunnel at tens of thousands of packets per second cannot afford),
    /// and unlike [`Self::send_on_link`] it has no window, no ordering and no retransmission: the
    /// transport underneath either delivers the packet or drops it, and the inner IP stack copes.
    pub fn send_link_tunnel(
        &mut self,
        link_id: &LinkId,
        data: &[u8],
    ) -> Result<crate::transport::TickOutput, send::SendError> {
        let link_id = &self.resolve_link_id(link_id);
        let link = self.links.get(link_id).ok_or(send::SendError::NoLink)?;
        if link.state() != LinkState::Active {
            return Err(send::SendError::LinkFailed);
        }
        if data.len() > link.mdu() {
            return Err(send::SendError::TooLarge);
        }
        let packet_bytes = link
            .build_data_packet_with_context(data, PacketContext::Tunnel, &mut self.rng)
            .map_err(|_| send::SendError::LinkFailed)?;
        self.route_link_packet(link_id, &packet_bytes);
        Ok(self.process_events_and_actions())
    }

    /// Any non-LRRTT inbound Link packet confirms delivery of a mode-7 GCM
    /// RTT. CBC remains one-shot and never enters the retry state machine.
    pub(super) fn try_confirm_rtt(&mut self, link_id: &LinkId) {
        if let Some(link) = self.links.get_mut(link_id) {
            if link.cipher() == crate::LinkCipher::Gcm
                && link.is_initiator()
                && !link.rtt_confirmed()
            {
                link.confirm_rtt();
            }
        }
    }

    /// Regular data packet, decrypt, handle proof strategy, emit data event
    /// One tunnelled datagram: authenticate, decrypt, hand the plaintext up. Nothing else.
    ///
    /// No proof (an unreliable datagram is never acknowledged), no receipt, no ring, no hash: the
    /// cipher is the only integrity and replay check, and for GCM the record counter window rejects
    /// duplicates. A failure drops exactly this packet.
    pub(super) fn handle_tunnel_packet(&mut self, link_id: LinkId, packet: &Packet, now_ms: u64) {
        let Some(link) = self.links.get_mut(&link_id) else {
            return;
        };
        if link.state() != LinkState::Active {
            return;
        }
        link.record_inbound(now_ms);
        let sealed = packet.data.as_slice();
        let mut plaintext = alloc::vec![0u8; sealed.len()];
        match link.decrypt_for_context(sealed, &mut plaintext, packet.context) {
            Ok(len) => {
                plaintext.truncate(len);
                self.events.push(NodeEvent::LinkDataReceived {
                    link_id,
                    data: plaintext,
                });
            }
            Err(_) => {
                crate::tracing::trace!(
                    link = %HexShort(link_id.as_bytes()),
                    "Tunnel datagram refused by the link cipher"
                );
            }
        }
    }

    /// Retry only the explicitly signalled mode-7 GCM LRRTT. CBC remains a
    /// single-shot packet for Python-RNS callback/RTT parity. Retries are
    /// bounded and use the original mode-7 interval: max(3 × RTT, 10 s).
    pub(in crate::node) fn check_rtt_retry(&mut self, now_ms: u64) {
        let due: Vec<LinkId> = self
            .links
            .iter()
            .filter(|(_, link)| {
                link.needs_rtt_retry()
                    && link.rtt_sent_at_ms().is_some_and(|sent| {
                        now_ms.saturating_sub(sent) >= link.rtt_retry_interval_ms()
                    })
            })
            .map(|(link_id, _)| *link_id)
            .collect();

        for link_id in due {
            let packet = {
                let Some(link) = self.links.get_mut(&link_id) else {
                    continue;
                };
                let rtt_seconds = link
                    .rtt_secs()
                    .unwrap_or(CHANNEL_DEFAULT_RTT_MS as f64 / MS_PER_SECOND as f64);
                match link.build_rtt_packet(rtt_seconds, &mut self.rng) {
                    Ok(packet) => {
                        link.record_rtt_sent(now_ms);
                        crate::tracing::debug!(
                            link = %HexShort(link_id.as_bytes()),
                            attempt = link.rtt_send_count(),
                            retry_budget = RTT_RETRY_MAX_ATTEMPTS,
                            "Resending LRRTT on explicitly negotiated GCM Link"
                        );
                        packet
                    }
                    Err(error) => {
                        crate::tracing::warn!(
                            link = %HexShort(link_id.as_bytes()),
                            "GCM LRRTT retry packet build failed: {error:?}"
                        );
                        continue;
                    }
                }
            };
            self.route_link_packet(&link_id, &packet);
        }
    }
}

#[cfg(test)]
#[cfg(test)]
mod proof_binding_tests {
    use super::proof_identifier;
    use crate::constants::{AES_BLOCK_SIZE, HMAC_SIZE};
    use crate::destination::{ProofBinding, ProofBindingPolicy};
    use crate::packet::packet_hash;

    fn token_packet() -> alloc::vec::Vec<u8> {
        let mut raw = alloc::vec::Vec::new();
        raw.extend_from_slice(&[0x00, 0x00]);
        raw.extend_from_slice(&[0x11u8; 16]);
        raw.push(crate::packet::PacketContext::Channel.to_byte());
        raw.extend_from_slice(&[0u8; AES_BLOCK_SIZE + AES_BLOCK_SIZE]);
        raw.extend_from_slice(&[0xABu8; HMAC_SIZE]);
        raw
    }

    #[test]
    fn gcm_receipt_lookup_uses_full_tag_even_for_short_records() {
        let mut raw = alloc::vec![0u8; 19];
        raw[18] = crate::packet::PacketContext::Channel.to_byte();
        raw.extend_from_slice(&[3u8; 12]);
        raw.extend_from_slice(b"echo");
        raw.extend_from_slice(&[7u8; 16]);
        let id =
            super::cipher_proof_identifier(&raw, ProofBinding::TokenTag, crate::LinkCipher::Gcm);
        assert_eq!(&id[..16], &[7u8; 16]);
        assert_eq!(&id[16..28], &[3u8; 12]);
        assert_eq!(&id[28..], b"GCM1");
        assert_eq!(
            super::cipher_proof_identifier(&raw, ProofBinding::PacketHash, crate::LinkCipher::Gcm),
            packet_hash(&raw)
        );
    }

    #[test]
    fn proof_binding_policy_resolves_per_cipher_and_preserves_explicit_override() {
        assert_eq!(
            ProofBindingPolicy::ByCipher.for_cipher(crate::LinkCipher::Cbc),
            ProofBinding::PacketHash
        );
        assert_eq!(
            ProofBindingPolicy::ByCipher.for_cipher(crate::LinkCipher::Gcm),
            ProofBinding::TokenTag
        );
        assert_eq!(
            ProofBindingPolicy::Explicit(ProofBinding::TokenTag).for_cipher(crate::LinkCipher::Cbc),
            ProofBinding::TokenTag
        );
        assert_eq!(
            ProofBindingPolicy::default(),
            ProofBindingPolicy::ByCipher,
            "by default CBC Links prove exactly as vanilla Reticulum"
        );
    }

    /// The two modes must disagree on a token packet - that is the whole point
    /// of the switch - while both stay internally consistent, which is what
    /// lets a legacy peer find our receipts at all.
    #[test]
    fn bindings_differ_and_each_matches_its_own_definition() {
        let raw = token_packet();
        let tag = proof_identifier(&raw, ProofBinding::TokenTag);
        let hash = proof_identifier(&raw, ProofBinding::PacketHash);

        assert_eq!(tag, crate::packet::packet_token_tag(&raw).unwrap());
        assert_eq!(hash, packet_hash(&raw));
        assert_ne!(tag, hash);
    }

    /// Without a token there is no tag to bind to, so both modes must land on
    /// the hash rather than one of them binding to something meaningless.
    #[test]
    fn packets_without_a_token_use_the_hash_in_both_modes() {
        let mut raw = token_packet();
        raw[2 + 16] = crate::packet::PacketContext::LinkClose.to_byte();
        let expected = packet_hash(&raw);
        assert_eq!(proof_identifier(&raw, ProofBinding::TokenTag), expected);
        assert_eq!(proof_identifier(&raw, ProofBinding::PacketHash), expected);
    }

    #[test]
    fn receive_side_identifier_matches_the_sender_for_every_binding_and_cipher() {
        use crate::packet::Packet;
        let raw = token_packet();
        let packet = Packet::unpack(&raw).unwrap();
        for binding in [ProofBinding::TokenTag, ProofBinding::PacketHash] {
            for cipher in [crate::LinkCipher::Cbc, crate::LinkCipher::Gcm] {
                assert_eq!(
                    super::link_proof_id(&packet, None, binding, cipher),
                    super::cipher_proof_identifier(&raw, binding, cipher),
                    "{binding:?}/{cipher:?}"
                );
            }
        }
    }
}
