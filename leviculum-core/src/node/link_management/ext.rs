//! Per-link cipher (CBC or GCM mode 7), tunnel datagrams and bounded LRRTT retries for GCM
//! links.
//!
//! A child of `link_management`, so it reaches that module's private methods and the fields of
//! `NodeCore`; the shared file only calls into it.
use super::*;

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
