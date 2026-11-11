//! Driver additions kept out of the shared file: direct Link access, the packet-channel
//! interface, interface-scoped path requests, the dropped data event counter, and the
//! hash-free receive class for tunnel datagrams.
use super::*;
use leviculum_core::link::{Link, LinkId};

/// Data-plane events (received packets, link data) discarded because the application did not drain its
/// channel fast enough. Process-wide and monotonic. A drop is the designed backpressure answer for lossy
/// traffic, but an IP tunnel must be able to SEE it: it is the receive-side half of the packet loss
/// budget and, unlike the wire, nothing else counts it.
pub static DATA_EVENTS_DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many data events have been dropped so far because the application channel was full.
pub fn data_events_dropped() -> u64 {
    DATA_EVENTS_DROPPED.load(std::sync::atomic::Ordering::Relaxed)
}

impl ReticulumNode {
    /// Run `f` on link `id`, or `None` if the node has no such link.
    ///
    /// `f` runs under the node's core lock, so keep it short and do not call back into the node.
    pub fn with_link<T>(&self, id: &LinkId, f: impl FnOnce(&Link) -> T) -> Option<T> {
        self.inner.lock_recover().link(id).map(f)
    }

    /// As [`Self::with_link`], with the link mutable (for example
    /// [`Link::set_keepalive_override`]).
    pub fn with_link_mut<T>(&self, id: &LinkId, f: impl FnOnce(&mut Link) -> T) -> Option<T> {
        self.inner.lock_recover().link_mut(id).map(f)
    }

    /// Attach a **packet-channel** interface to the running node: one Reticulum packet per datagram of a
    /// caller-supplied carrier (QUIC DATAGRAM frames, a UDP socket), with no framing.
    ///
    /// The right interface under an IP tunnel: it loses exactly the packet the carrier loses, where the
    /// byte channel's stream would stall everything behind it. See
    /// [`PacketSource`](crate::interfaces::PacketSource) / [`PacketSink`](crate::interfaces::PacketSink)
    /// for the carrier contract. Hold the returned handle to keep the interface attached.
    pub fn spawn_packet_channel<R, W>(
        &self,
        name: &str,
        source: R,
        sink: W,
        options: crate::interfaces::PacketChannelOptions,
    ) -> Result<crate::interfaces::PacketChannelHandle, Error>
    where
        R: crate::interfaces::PacketSource,
        W: crate::interfaces::PacketSink,
    {
        use std::sync::atomic::Ordering;

        let runtime = self.runtime.as_ref().ok_or(Error::NotRunning)?;
        let new_iface_tx = self.new_iface_tx.as_ref().ok_or(Error::NotRunning)?;
        let next_id = self.iface_id_counter.as_ref().ok_or(Error::NotRunning)?;

        let id = InterfaceId(next_id.fetch_add(1, Ordering::Relaxed));
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let handle = {
            let _enter = runtime.enter();
            crate::interfaces::packet_channel::spawn_packet_channel_interface(
                crate::interfaces::packet_channel::PacketChannelConfig {
                    id,
                    name: name.to_string(),
                    buffer_size: options.buffer_size,
                    hw_mtu: options.hw_mtu,
                    shutdown: Some(shutdown_rx),
                },
                source,
                sink,
            )
        };
        new_iface_tx
            .try_send(handle)
            .map_err(|_| Error::NotRunning)?;
        Ok(crate::interfaces::ByteChannelHandle::new(id, shutdown_tx))
    }

    /// Request a path on the listed interfaces only, ignoring the request limiter. See
    /// `NodeCore::request_path_on`.
    pub async fn request_path_on(
        &self,
        dest_hash: &leviculum_core::DestinationHash,
        interfaces: &[usize],
    ) -> Result<(), Error> {
        let output = {
            let mut inner = self.inner.lock_recover();
            inner.request_path_on(dest_hash, interfaces)
        };
        self.action_dispatch_tx
            .send(output)
            .await
            .map_err(|_| Error::NotRunning)?;
        Ok(())
    }

    /// [`Self::connect`] with an explicit cipher for this link only.
    pub async fn connect_with_cipher(
        &self,
        dest_hash: &leviculum_core::DestinationHash,
        dest_signing_key: &[u8; 32],
        cipher: leviculum_core::LinkCipher,
    ) -> Result<LinkHandle, Error> {
        let first_hop_timeout_ms = self.shared_instance_first_hop_timeout_ms(dest_hash).await;
        let link_id = super::dispatch_committed(&self.action_dispatch_tx, || {
            let mut inner = self.inner.lock_recover();
            let (link_id, _was_routed, output) = inner.connect_with_cipher(
                *dest_hash,
                dest_signing_key,
                first_hop_timeout_ms,
                cipher,
            )?;
            Ok((link_id, output))
        })
        .await?;

        Ok(LinkHandle::new(
            link_id,
            Arc::clone(&self.inner),
            self.action_dispatch_tx.clone(),
            Arc::clone(&self.completions),
        ))
    }

    /// Like [`spawn_byte_channel`](Self::spawn_byte_channel) with explicit
    /// [`ByteChannelOptions`] (fork addition: HW_MTU, channel capacity and read
    /// buffer size are tunables instead of pipe-sized constants).
    pub fn spawn_byte_channel_with<S>(
        &self,
        name: &str,
        stream: S,
        options: crate::interfaces::ByteChannelOptions,
    ) -> Result<crate::interfaces::ByteChannelHandle, Error>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    {
        use std::sync::atomic::Ordering;

        let runtime = self.runtime.as_ref().ok_or(Error::NotRunning)?;
        let new_iface_tx = self.new_iface_tx.as_ref().ok_or(Error::NotRunning)?;
        let next_id = self.iface_id_counter.as_ref().ok_or(Error::NotRunning)?;

        let id = InterfaceId(next_id.fetch_add(1, Ordering::Relaxed));
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let handle = {
            let _enter = runtime.enter();
            crate::interfaces::byte_channel::spawn_byte_channel_interface(
                crate::interfaces::byte_channel::ByteChannelConfig {
                    id,
                    name: name.to_string(),
                    buffer_size: options.buffer_size,
                    hw_mtu: options.hw_mtu,
                    read_buf_size: options.read_buf_size,
                    shutdown: Some(shutdown_rx),
                },
                stream,
            )
        };
        new_iface_tx
            .try_send(handle)
            .map_err(|_| Error::NotRunning)?;
        Ok(crate::interfaces::ByteChannelHandle::new(id, shutdown_tx))
    }
}

impl ReticulumNodeBuilder {
    pub fn link_cipher(mut self, cipher: leviculum_core::LinkCipher) -> Self {
        self.core_builder = self.core_builder.link_cipher(cipher);
        self
    }

    /// Also accept inbound links that ask for standard CBC (links that cross stock Reticulum transports).
    pub fn accept_cbc_links(mut self, accept: bool) -> Self {
        self.core_builder = self.core_builder.accept_cbc_links(accept);
        self
    }
}

/// Link tunnel datagrams: nothing to precompute. In particular no payload hash,
/// which on a bulk link was the single largest cost of the receive thread; core
/// treats the missing hash as "not needed" for exactly this packet class.
pub(super) fn prepare_hash_free(iface: InterfaceId, data: Vec<u8>) -> PreparedRx {
    PreparedRx {
        iface,
        peer: None,
        data,
        pre: leviculum_core::node::PrecomputedRx::default(),
        evict_decryptor_for: None,
    }
}

#[cfg(test)]
mod rx_class_tests {
    use super::*;
    use leviculum_core::packet::{
        HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
    };
    use leviculum_core::DestinationType;

    fn wire(packet_type: PacketType, dest_type: DestinationType, context: PacketContext) -> Vec<u8> {
        let packet = Packet {
            flags: PacketFlags {
                ifac_flag: false,
                header_type: HeaderType::Type1,
                context_flag: false,
                transport_type: TransportType::Broadcast,
                dest_type,
                packet_type,
            },
            hops: 0,
            transport_id: None,
            destination_hash: [0x31; 16],
            context,
            data: PacketData::Owned(vec![0xAB; 300]),
        };
        let mut buf = [0u8; 600];
        let len = packet.pack(&mut buf).unwrap();
        buf[..len].to_vec()
    }

    /// Only link tunnel data skips the payload hash in the driver; every other
    /// class keeps the precomputed hash the dedup path expects.
    #[test]
    fn only_link_tunnel_data_is_prepared_without_a_hash() {
        let tunnel = wire(PacketType::Data, DestinationType::Link, PacketContext::Tunnel);
        assert!(matches!(classify_rx(&tunnel), RxClass::HashFreeLinkData));
        let prepared = prepare_hash_free(InterfaceId(0), tunnel.clone());
        assert_eq!(prepared.pre.packet_hash, None);
        assert_eq!(prepared.data, tunnel);

        for (ty, dest, ctx) in [
            (PacketType::Data, DestinationType::Link, PacketContext::Channel),
            (PacketType::Data, DestinationType::Link, PacketContext::Resource),
            (PacketType::Data, DestinationType::Link, PacketContext::Keepalive),
            (PacketType::Data, DestinationType::Link, PacketContext::None),
            (PacketType::Data, DestinationType::Plain, PacketContext::Channel),
            (PacketType::Proof, DestinationType::Link, PacketContext::Channel),
            (PacketType::LinkRequest, DestinationType::Link, PacketContext::Channel),
        ] {
            let raw = wire(ty, dest, ctx);
            assert!(
                !matches!(classify_rx(&raw), RxClass::HashFreeLinkData),
                "{ty:?}/{dest:?}/{ctx:?} must keep hashing"
            );
            let prepared = prepare_cheap(InterfaceId(0), raw.clone());
            assert_eq!(prepared.pre.packet_hash, Some(leviculum_core::packet::packet_hash(&raw)));
        }
        // A single-destination datagram keeps its dedicated decrypt class.
        let single = wire(PacketType::Data, DestinationType::Single, PacketContext::None);
        assert!(matches!(classify_rx(&single), RxClass::SingleDest(_)));
    }
}

/// Announce packet hashes whose Ed25519 signature already verified, bounded FIFO.
///
/// A transport node hears every announce once per neighbour that rebroadcasts it, and each copy was
/// verified again off-lock (`precompute_announce`); on a 2-CPU VM without AVX2 that was a quarter of the
/// daemon's samples (field profile, 2026-11-10). The packet hash covers everything the verifier reads
/// (lower flag nibble, destination hash, context, the whole announce body including key and signature;
/// only hops and the HEADER_2 transport id are excluded), so equal hashes mean equal verifier input and
/// an equal verdict. Only successes are remembered: a failed copy is re-verified by the core as before.
pub(super) struct VerifiedAnnounces {
    seen: std::collections::HashSet<[u8; 32]>,
    order: std::collections::VecDeque<[u8; 32]>,
    capacity: usize,
}

/// Memo ceiling: 32 B per entry twice over, about 1 MiB at this size.
pub(super) const VERIFIED_ANNOUNCES_CAPACITY: usize = 16 * 1024;

impl VerifiedAnnounces {
    pub(super) fn new(capacity: usize) -> Self {
        Self { seen: Default::default(), order: Default::default(), capacity: capacity.max(1) }
    }
    pub(super) fn contains(&self, hash: &[u8; 32]) -> bool {
        self.seen.contains(hash)
    }
    pub(super) fn insert(&mut self, hash: [u8; 32]) {
        if !self.seen.insert(hash) {
            return;
        }
        self.order.push_back(hash);
        while self.order.len() > self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
    }
}

static VERIFIED_ANNOUNCES: std::sync::LazyLock<std::sync::Mutex<VerifiedAnnounces>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(VerifiedAnnounces::new(VERIFIED_ANNOUNCES_CAPACITY)));

/// Whether an announce with this packet hash already verified in this process.
pub(super) fn seen_verified_announce(hash: &[u8; 32]) -> bool {
    VERIFIED_ANNOUNCES.lock().unwrap_or_else(|e| e.into_inner()).contains(hash)
}

/// Memo key for an announce: the packet hash bound to the header's context flag.
///
/// The packet hash masks the upper flag nibble, but the announce parser reads the context flag from it to decide
/// whether a ratchet precedes the signature. Keyed by the packet hash alone, a relay could flip that bit on an
/// already-verified announce and have the differently-parsed copy reported as verified without a signature check.
/// Everything else the verifier reads (destination hash, context byte, payload, lower flag nibble) is in the hash.
pub(super) fn announce_memo_key(raw: &[u8], packet_hash: &[u8; 32]) -> [u8; 32] {
    let context_flag = raw.first().map_or(0, |f| f & 0x20);
    leviculum_core::crypto::full_hash_parts(&[packet_hash, &[context_flag]])
}

/// Pass `verified` through, remembering the hash when it is true.
pub(super) fn remember_if_verified(hash: [u8; 32], verified: bool) -> bool {
    if verified {
        VERIFIED_ANNOUNCES.lock().unwrap_or_else(|e| e.into_inner()).insert(hash);
    }
    verified
}

#[cfg(test)]
mod verified_announce_tests {
    use super::*;

    #[test]
    fn memo_is_bounded_fifo_and_keeps_only_what_was_inserted() {
        let mut m = VerifiedAnnounces::new(2);
        m.insert([1; 32]);
        m.insert([2; 32]);
        m.insert([1; 32]); // re-insert does not grow or reorder
        assert!(m.contains(&[1; 32]) && m.contains(&[2; 32]));
        m.insert([3; 32]);
        assert!(!m.contains(&[1; 32]), "oldest dropped at the ceiling");
        assert!(m.contains(&[2; 32]) && m.contains(&[3; 32]));
        assert_eq!(m.order.len(), 2);
    }

    #[test]
    fn memo_key_changes_with_the_context_flag_but_not_with_hops_or_header_type() {
        let mut raw = vec![0x01u8, 0x00];
        raw.extend_from_slice(&[7u8; 60]);
        let h = leviculum_core::packet::packet_hash(&raw);
        let base = announce_memo_key(&raw, &h);
        let mut flipped = raw.clone();
        flipped[0] ^= 0x20; // context flag: ratchet present / absent
        let hf = leviculum_core::packet::packet_hash(&flipped);
        assert_eq!(h, hf, "the packet hash alone does not see the context flag");
        assert_ne!(base, announce_memo_key(&flipped, &hf), "the memo key must");
        let mut hops = raw.clone();
        hops[1] = 5;
        assert_eq!(base, announce_memo_key(&hops, &leviculum_core::packet::packet_hash(&hops)), "hops do not matter");
    }

    #[test]
    fn only_successes_are_remembered() {
        let ok = [0xA5; 32];
        let bad = [0x5A; 32];
        assert!(!remember_if_verified(bad, false));
        assert!(!seen_verified_announce(&bad), "a failed verify must be re-checked next time");
        assert!(remember_if_verified(ok, true));
        assert!(seen_verified_announce(&ok));
    }
}
