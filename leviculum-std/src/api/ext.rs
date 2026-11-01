use leviculum_core::LinkCipher;

use crate::driver::LinkHandle;
use crate::error::Result;
use super::{DestinationHash, Node, NodeBuilder};

impl NodeBuilder {
    /// Capacity of the application's data-event channel (received packets / link data). Data events are
    /// dropped when it is full; a packet-rate tunnel wants it sized for a burst of the interface reader
    /// (one QUIC read yields tens of packets at once), not for the average rate.
    pub fn data_channel_capacity(mut self, capacity: usize) -> Self {
        self.inner = self.inner.data_channel_capacity(capacity);
        self
    }

    pub fn link_cipher(mut self, cipher: LinkCipher) -> Self {
        self.inner = self.inner.link_cipher(cipher);
        self
    }

    /// Also accept inbound links that ask for standard CBC (links that cross stock Reticulum transports).
    pub fn accept_cbc_links(mut self, accept: bool) -> Self {
        self.inner = self.inner.accept_cbc_links(accept);
        self
    }
}

impl Node {
    /// [`spawn_byte_channel`](Self::spawn_byte_channel) with explicit tunables
    /// (fork addition, see [`ByteChannelOptions`](crate::interfaces::ByteChannelOptions)).
    #[must_use = "dropping the handle immediately detaches the interface"]
    pub fn spawn_byte_channel_with<S>(
        &self,
        name: &str,
        stream: S,
        options: crate::interfaces::ByteChannelOptions,
    ) -> Result<crate::interfaces::ByteChannelHandle>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    {
        self.inner.spawn_byte_channel_with(name, stream, options)
    }

    /// Attach a packet-channel interface (one packet per carrier datagram, no framing). See
    /// [`ReticulumNode::spawn_packet_channel`](crate::driver::ReticulumNode::spawn_packet_channel).
    #[must_use = "dropping the handle immediately detaches the interface"]
    pub fn spawn_packet_channel<R, W>(
        &self,
        name: &str,
        source: R,
        sink: W,
        options: crate::interfaces::PacketChannelOptions,
    ) -> Result<crate::interfaces::PacketChannelHandle>
    where
        R: crate::interfaces::PacketSource,
        W: crate::interfaces::PacketSink,
    {
        self.inner.spawn_packet_channel(name, source, sink, options)
    }

    /// [`Self::connect_with_key`] with an explicit cipher for this link only (standard CBC for links that cross stock Reticulum transports).
    pub async fn connect_with_key_cipher(
        &self,
        dest_hash: &DestinationHash,
        signing_key: &[u8; 32],
        cipher: LinkCipher,
    ) -> Result<LinkHandle> {
        self.inner
            .connect_with_cipher(dest_hash, signing_key, cipher)
            .await
    }
}
