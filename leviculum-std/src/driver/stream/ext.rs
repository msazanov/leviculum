use tokio::sync::mpsc;

use leviculum_core::transport::TickOutput;

use crate::error::Error;
use crate::sync_ext::MutexRecover;
use super::LinkHandle;

impl LinkHandle {
    /// Send one unreliable datagram (an IP packet of the tunnel) on this link.
    ///
    /// Sealed by the link cipher and sent once: no window, no ordering, no retransmission, no
    /// receipt. Returns when the packet has been handed to the driver; it never waits for the peer.
    pub async fn send_datagram(&self, data: &[u8]) -> Result<(), Error> {
        if self.closed {
            return Err(Error::NotRunning);
        }
        dispatch_datagram(&self.action_dispatch_tx, || {
            let mut core = self.inner.lock_recover();
            Ok(core.send_link_tunnel(&self.link_id, data)?)
        })
        .await
    }
}

/// Reserve dispatch capacity before mutating the core. Tunnel datagrams remain
/// unreliable and are committed exactly once; cancellation before reservation
/// runs `commit` zero times, and cancellation after reservation cannot lose its
/// output because `Permit::send` is synchronous.
pub(super) async fn dispatch_datagram(
    action_dispatch_tx: &mpsc::Sender<TickOutput>,
    commit: impl FnOnce() -> Result<TickOutput, Error>,
) -> Result<(), Error> {
    crate::driver::dispatch_committed(action_dispatch_tx, || commit().map(|output| ((), output))).await
}
