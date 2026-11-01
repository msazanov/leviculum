use super::{BYTE_CHANNEL_DEFAULT_BUFFER_SIZE, BYTE_CHANNEL_HW_MTU};

/// Default read buffer size (bytes) used to pull data off the caller's stream.
pub(crate) const BYTE_CHANNEL_DEFAULT_READ_BUF: usize = 1024;

/// Tunables of a byte-channel interface. [`Default`] reproduces the upstream
/// behaviour exactly (HW_MTU 1064, 64-packet channels, 1 KiB reads).
///
/// Fork addition: the upstream values are sized for serial-like
/// pipes and cap throughput over a fast reliable stream such as QUIC.
#[derive(Debug, Clone, Copy)]
pub struct ByteChannelOptions {
    /// `HW_MTU` advertised for the interface (bounds link MTU negotiation).
    pub hw_mtu: u32,
    /// Capacity, in packets, of the incoming and outgoing channels.
    pub buffer_size: usize,
    /// Size of the buffer used for each read from the stream.
    pub read_buf_size: usize,
}

impl Default for ByteChannelOptions {
    fn default() -> Self {
        Self {
            hw_mtu: BYTE_CHANNEL_HW_MTU,
            buffer_size: BYTE_CHANNEL_DEFAULT_BUFFER_SIZE,
            read_buf_size: BYTE_CHANNEL_DEFAULT_READ_BUF,
        }
    }
}
