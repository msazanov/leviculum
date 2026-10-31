//! Per-link cipher state: GCM mode 7 next to the standard CBC token, the keepalive round
//! trip, bounded LRRTT retries for GCM initiators, and the Channel send window and pacing.
//!
//! A child of `link`, so it reaches the private fields of [`Link`]; the shared file keeps
//! one field and one-line hooks in `encrypt`/`decrypt`.
use super::*;

/// Fork-only state of a [`Link`].
pub(super) struct LinkExt {
    pub(super) cipher: LinkCipher,
    pub(super) gcm: Option<alloc::boxed::Box<crate::crypto::link_gcm::LinkGcm>>,
    pub(super) send_window_limit: usize,
    pub(super) send_pacing_enabled: bool,
    /// Round trip of the last acknowledged keepalive (initiator), ms.
    pub(super) keepalive_rtt_ms: Option<u64>,
    /// Last LRRTT transmit and retry state for mode-7 GCM initiators.
    /// CBC Links remain one-shot for Python-RNS interoperability.
    pub(super) rtt_sent_at_ms: Option<u64>,
    pub(super) rtt_send_count: u8,
    pub(super) rtt_confirmed: bool,
}

impl Default for LinkExt {
    fn default() -> Self {
        Self {
            cipher: LinkCipher::Cbc,
            gcm: None,
            send_window_limit: crate::constants::CHANNEL_WINDOW_MAX_FAST,
            send_pacing_enabled: true,
            keepalive_rtt_ms: None,
            rtt_sent_at_ms: None,
            rtt_send_count: 0,
            rtt_confirmed: false,
        }
    }
}

impl LinkExt {
    /// State of an incoming link: its cipher is the mode the request signals.
    pub(super) fn for_request(request_data: &[u8]) -> Self {
        let gcm = request_data.len() >= LINK_REQUEST_SIGNALING_SIZE
            && decode_signaling_bytes(&request_data[64..67].try_into().unwrap()).1
                == LinkCipher::Gcm as u8;
        Self {
            cipher: if gcm { LinkCipher::Gcm } else { LinkCipher::Cbc },
            ..Self::default()
        }
    }
}

impl Link {
    /// Round trip of the most recent acknowledged keepalive in milliseconds
    /// (initiator only), measured from the send time to the echo.
    pub fn keepalive_rtt_ms(&self) -> Option<u64> {
        self.ext.keepalive_rtt_ms
    }

    /// Record the round trip of an acknowledged keepalive (clock ms).
    pub(crate) fn record_keepalive_rtt(&mut self, now_ms: u64) {
        if self.last_keepalive_ms != 0 {
            self.ext.keepalive_rtt_ms = Some(now_ms.saturating_sub(self.last_keepalive_ms));
        }
    }

    pub(crate) fn rtt_sent_at_ms(&self) -> Option<u64> {
        self.ext.rtt_sent_at_ms
    }

    pub(crate) fn rtt_send_count(&self) -> u8 {
        self.ext.rtt_send_count
    }

    pub(crate) fn rtt_confirmed(&self) -> bool {
        self.ext.rtt_confirmed
    }

    pub(crate) fn rtt_retry_interval_ms(&self) -> u64 {
        self.rtt_ms()
            .saturating_mul(RTT_RETRY_INTERVAL_MULTIPLIER)
            .max(RTT_RETRY_MIN_INTERVAL_MS)
    }

    pub(crate) fn needs_rtt_retry(&self) -> bool {
        self.ext.cipher == LinkCipher::Gcm
            && self.initiator
            && self.state() == LinkState::Active
            && !self.ext.rtt_confirmed
            && self.ext.rtt_send_count > 0
            && self.ext.rtt_send_count <= RTT_RETRY_MAX_ATTEMPTS
    }

    pub(crate) fn record_rtt_sent(&mut self, now_ms: u64) {
        self.ext.rtt_sent_at_ms = Some(now_ms);
        self.ext.rtt_send_count = self.ext.rtt_send_count.saturating_add(1);
    }

    pub(crate) fn confirm_rtt(&mut self) {
        self.ext.rtt_confirmed = true;
        self.ext.rtt_sent_at_ms = None;
    }

    /// Must be selected before an outgoing request is built, never mid-link.
    pub fn set_cipher(&mut self, cipher: LinkCipher) -> Result<(), LinkError> {
        if self.state != LinkState::Pending || self.link_key.is_some() {
            return Err(LinkError::InvalidState);
        }
        self.ext.cipher = cipher;
        Ok(())
    }

    pub fn cipher(&self) -> LinkCipher {
        self.ext.cipher
    }

    pub fn set_send_window_limit(&mut self, limit: usize) {
        self.ext.send_window_limit = limit.clamp(1, crate::constants::CHANNEL_WINDOW_MAX_FAST);
        if let Some(channel) = self.channel.as_mut() {
            channel.set_window_ceiling(self.ext.send_window_limit);
        }
    }

    pub fn set_send_pacing_enabled(&mut self, enabled: bool) {
        self.ext.send_pacing_enabled = enabled;
        if let Some(channel) = self.channel.as_mut() {
            channel.set_pacing_enabled(enabled);
        }
    }

    pub(super) fn install_link_key(&mut self, key: [u8; 64]) {
        if self.ext.cipher == LinkCipher::Gcm {
            self.ext.gcm = Some(alloc::boxed::Box::new(
                crate::crypto::link_gcm::LinkGcm::new(&key, self.id.as_bytes(), self.initiator),
            ));
        }
        self.link_key = Some(key);
    }

    /// GCM branch of [`Self::encrypt`]: `None` on a CBC link.
    pub(super) fn gcm_encrypt(
        &self,
        plaintext: &[u8],
        output: &mut [u8],
        context: PacketContext,
    ) -> Option<Result<usize, LinkError>> {
        if self.ext.cipher != LinkCipher::Gcm {
            return None;
        }
        Some(
            self.ext
                .gcm
                .as_ref()
                .ok_or(LinkError::InvalidState)
                .and_then(|g| {
                    g.encrypt_context(plaintext, output, context.to_byte())
                        .map_err(|_| LinkError::InvalidState)
                }),
        )
    }

    /// GCM branch of [`Self::decrypt`]: `None` on a CBC link.
    pub(super) fn gcm_decrypt(
        &self,
        token: &[u8],
        output: &mut [u8],
        context: PacketContext,
    ) -> Option<Result<usize, LinkError>> {
        if self.ext.cipher != LinkCipher::Gcm {
            return None;
        }
        Some(
            self.ext
                .gcm
                .as_ref()
                .ok_or(LinkError::InvalidState)
                .and_then(|g| {
                    g.decrypt_context(token, output, context.to_byte())
                        .map_err(|_| LinkError::InvalidProof)
                }),
        )
    }

    /// [`Self::encrypt`] with the packet context authenticated by GCM; CBC ignores it.
    pub(crate) fn encrypt_for_context(
        &self,
        plaintext: &[u8],
        output: &mut [u8],
        rng: &mut impl CryptoRngCore,
        context: PacketContext,
    ) -> Result<usize, LinkError> {
        match self.gcm_encrypt(plaintext, output, context) {
            Some(r) => r,
            None => self.encrypt(plaintext, output, rng),
        }
    }

    /// [`Self::decrypt`] with the packet context authenticated by GCM; CBC ignores it.
    pub(crate) fn decrypt_for_context(
        &self,
        token: &[u8],
        output: &mut [u8],
        context: PacketContext,
    ) -> Result<usize, LinkError> {
        match self.gcm_decrypt(token, output, context) {
            Some(r) => r,
            None => self.decrypt(token, output),
        }
    }

    /// Resource bodies use the existing streamed CBC token construction,
    /// independently of the negotiated cipher of their Link control packets.
    /// This is a specified format, not a fallback after failed GCM verification.
    pub(crate) fn decrypt_resource_body(
        &self,
        token: &[u8],
        output: &mut [u8],
    ) -> Result<usize, LinkError> {
        let key = self.token_key().ok_or(LinkError::InvalidState)?;
        crate::crypto::decrypt_token(&key, token, output).map_err(|_| LinkError::InvalidProof)
    }
}
