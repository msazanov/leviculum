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
