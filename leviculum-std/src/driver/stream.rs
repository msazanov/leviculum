//! Send-only async handle for Links
//!
//! Locks the core directly and dispatches actions through the event loop,
//! matching the pattern used by `PacketSender`. This ensures that
//! `send()` returns the real result from `send_on_link()`, including
//! `Busy`, instead of silently buffering through an mpsc channel.

use std::sync::{Arc, Mutex};

use crate::sync_ext::MutexRecover;

use tokio::sync::mpsc;

use leviculum_core::link::LinkId;
use leviculum_core::transport::TickOutput;
use leviculum_core::SendError;

use super::StdNodeCore;
use crate::error::Error;

/// Async handle for a Link (send-only)
///
/// `LinkHandle` provides async write operations for a link.
/// It locks the core directly for each send, ensuring the caller sees
/// the real result (including `Busy` mapped to `WouldBlock`).
///
/// Incoming data is delivered via `NodeEvent::LinkDataReceived` /
/// `NodeEvent::MessageReceived` on the node's event channel.
///
/// # Example
///
/// ```no_run
/// # use leviculum_std::driver::{ReticulumNodeBuilder, LinkHandle};
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// # let node = ReticulumNodeBuilder::new().build().await?;
/// # let dest_hash = leviculum_core::DestinationHash::new([0; 16]);
/// # let signing_key = [0u8; 32];
/// let handle = node.connect(&dest_hash, &signing_key).await?;
///
/// // Write data
/// handle.send(b"Hello!").await?;
///
/// // Read responses via node.take_event_receiver()
/// # Ok(())
/// # }
/// ```
pub struct LinkHandle {
    /// Link ID for this link
    link_id: LinkId,
    /// Shared handle to the core node
    inner: Arc<Mutex<StdNodeCore>>,
    /// Channel for dispatching actions to the event loop
    action_dispatch_tx: mpsc::Sender<TickOutput>,
    /// Whether the handle is closed
    closed: bool,
    window_opened: Arc<tokio::sync::Notify>,
    completions: Arc<super::completions::CompletionRegistry>,
}

impl LinkHandle {
    /// Create a new LinkHandle
    pub(crate) fn new(
        link_id: LinkId,
        inner: Arc<Mutex<StdNodeCore>>,
        action_dispatch_tx: mpsc::Sender<TickOutput>,
        completions: Arc<super::completions::CompletionRegistry>,
    ) -> Self {
        let window_opened = completions.send_window(link_id);
        Self {
            link_id,
            inner,
            action_dispatch_tx,
            closed: false,
            window_opened,
            completions,
        }
    }

    /// Get the link ID
    pub fn link_id(&self) -> &LinkId {
        &self.link_id
    }

    /// Check if the handle is closed
    pub fn is_closed(&self) -> bool {
        self.closed
    }

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

    /// Try to send data on this link (non-blocking)
    ///
    /// Locks the core, calls `send_on_link()`, and dispatches the
    /// resulting actions. Returns `SendError::Busy` if the send path is
    /// occupied, or `SendError::PacingDelay` if the channel is pacing.
    pub async fn try_send(&self, data: &[u8]) -> Result<(), Error> {
        if self.closed {
            return Err(Error::NotRunning);
        }

        super::dispatch_committed(&self.action_dispatch_tx, || {
            let mut core = self.inner.lock_recover();
            Ok(((), core.send_on_link(&self.link_id, data)?))
        })
        .await
    }

    /// Send data on this link, absorbing pacing delays and busy conditions
    ///
    /// Unlike `try_send()`, this method retries automatically when the channel
    /// reports a pacing delay or busy condition, sleeping until ready.
    /// Only returns an error on fatal conditions (link lost, handle closed).
    pub async fn send(&self, data: &[u8]) -> Result<(), Error> {
        loop {
            if self.closed {
                return Err(Error::NotRunning);
            }

            // Register for the next delivery/close wake BEFORE checking the
            // window, so an ACK landing between the check and the wait is not lost.
            let window_opened = self.window_opened.notified();
            tokio::pin!(window_opened);
            window_opened.as_mut().enable();
            let result = super::dispatch_committed(&self.action_dispatch_tx, || {
                let mut core = self.inner.lock_recover();
                Ok(((), core.send_on_link(&self.link_id, data)?))
            })
            .await;

            match result {
                Ok(()) => return Ok(()),
                Err(Error::Send(SendError::PacingDelay { ready_at_ms })) => {
                    let now_ms = self.inner.lock_recover().now_ms();
                    let delay = ready_at_ms.saturating_sub(now_ms);
                    if delay > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }
                }
                Err(Error::Send(SendError::Busy)) => {
                    // Window full: wait for an inbound packet (an ACK frees a slot),
                    // with the former fixed 50 ms as the safety net.
                    let _ =
                        tokio::time::timeout(std::time::Duration::from_millis(50), window_opened)
                            .await;
                }
                Err(other) => {
                    return Err(other);
                }
            }
        }
    }

    /// Close the link gracefully, sending LINKCLOSE to the peer
    pub async fn close(&mut self) -> Result<(), Error> {
        if self.closed {
            return Ok(());
        }
        // Marked closed only once the call returns: an abort while it waits
        // for room in the dispatch channel leaves the link open in the core,
        // and the handle has to agree.
        let result = super::dispatch_committed(&self.action_dispatch_tx, || {
            let mut core = self.inner.lock_recover();
            Ok(((), core.close_link(&self.link_id)))
        })
        .await;
        self.closed = true;
        result
    }
}

/// Reserve dispatch capacity before mutating the core. Tunnel datagrams remain
/// unreliable and are committed exactly once; cancellation before reservation
/// runs `commit` zero times, and cancellation after reservation cannot lose its
/// output because `Permit::send` is synchronous.
async fn dispatch_datagram(
    action_dispatch_tx: &mpsc::Sender<TickOutput>,
    commit: impl FnOnce() -> Result<TickOutput, Error>,
) -> Result<(), Error> {
    super::dispatch_committed(action_dispatch_tx, || commit().map(|output| ((), output))).await
}

impl Drop for LinkHandle {
    fn drop(&mut self) {
        self.completions
            .release_send_window(self.link_id, &self.window_opened);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::ReticulumNodeBuilder;

    // TempDir is the fourth tuple element; callers must bind it to a
    // live variable so RAII drop doesn't delete the storage directory
    // out from under the node.
    fn make_node_and_inner() -> (
        Arc<Mutex<StdNodeCore>>,
        mpsc::Sender<TickOutput>,
        mpsc::Receiver<TickOutput>,
        tempfile::TempDir,
    ) {
        let td = tempfile::tempdir().expect("tempdir");
        let node = ReticulumNodeBuilder::new()
            .storage_path(td.path().to_path_buf())
            .build_sync()
            .expect("build_sync");
        let inner = node.inner();
        let (tx, rx) = mpsc::channel(16);
        (inner, tx, rx, td)
    }

    #[tokio::test]
    async fn test_link_handle_send_no_link() {
        let (inner, tx, _rx, _td) = make_node_and_inner();
        let link_id = LinkId::from([0u8; 16]);

        let handle = LinkHandle::new(
            link_id,
            inner,
            tx,
            super::super::completions::CompletionRegistry::new(),
        );

        // Sending on a non-existent link should return Send(NoLink)
        let result = handle.try_send(b"hello").await;
        assert!(result.is_err());
        assert!(
            matches!(result, Err(Error::Send(SendError::NoLink))),
            "expected Send(NoLink), got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_link_handle_close() {
        let (inner, tx, _rx, _td) = make_node_and_inner();
        let link_id = LinkId::from([0u8; 16]);

        let mut handle = LinkHandle::new(
            link_id,
            inner,
            tx,
            super::super::completions::CompletionRegistry::new(),
        );

        assert!(!handle.is_closed());
        // close_link on a non-existent link is harmless (returns empty output)
        handle.close().await.unwrap();
        assert!(handle.is_closed());

        // Send should fail after close
        let result = handle.try_send(b"test").await;
        assert!(result.is_err());
        assert!(matches!(result, Err(Error::NotRunning)));
    }

    #[tokio::test]
    async fn test_link_handle_close_idempotent() {
        let (inner, tx, _rx, _td) = make_node_and_inner();
        let link_id = LinkId::from([0u8; 16]);

        let mut handle = LinkHandle::new(
            link_id,
            inner,
            tx,
            super::super::completions::CompletionRegistry::new(),
        );

        handle.close().await.unwrap();
        // Second close is a no-op
        handle.close().await.unwrap();
        assert!(handle.is_closed());
    }

    /// L-0059: a caller aborted while it waits on a full dispatch channel
    /// must leave core and channel consistent. Either the abort lands before
    /// the core moved (link still there, nothing enqueued) or after the
    /// enqueue (link gone, its close output in the channel). Committing the
    /// close and then losing the output is the defect: the link is removed
    /// and LinkClosed plus the LINKCLOSE bytes never reach the event loop.
    #[tokio::test(flavor = "current_thread")]
    async fn test_link_handle_close_aborted_on_full_channel_loses_nothing() {
        let (inner, _, _, _td) = make_node_and_inner();
        let identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
        let signing_key = identity.ed25519_verifying().to_bytes();
        let dest_hash = leviculum_core::DestinationHash::new([0x42; 16]);
        let (link_id, _, connect_output) = inner
            .lock_recover()
            .connect(dest_hash, &signing_key)
            .expect("connect");
        drop(connect_output);
        assert_eq!(inner.lock_recover().link_count(), 1);

        // A dispatch channel that is already full: the event loop is stalled.
        let (tx, mut rx) = mpsc::channel::<TickOutput>(1);
        tx.try_send(TickOutput::empty()).expect("fill the one slot");

        let mut handle = LinkHandle::new(
            link_id,
            Arc::clone(&inner),
            tx,
            super::super::completions::CompletionRegistry::new(),
        );
        let task = tokio::spawn(async move { handle.close().await });
        // Let the task run until it parks on the full channel.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(
            !task.is_finished(),
            "close must be parked on the full channel"
        );
        task.abort();
        let _ = task.await;

        let link_count = inner.lock_recover().link_count();
        let mut queued = 0;
        while rx.try_recv().is_ok() {
            queued += 1;
        }
        // One output is the filler. A second one is the close, if it was
        // enqueued at all.
        let close_enqueued = queued == 2;
        assert!(
            (link_count == 1 && !close_enqueued) || (link_count == 0 && close_enqueued),
            "core and dispatch channel disagree: link_count={link_count}, outputs queued={queued}"
        );
    }

    #[tokio::test]
    async fn test_link_handle_send_closed_dispatch_channel() {
        let (inner, _, _, _td) = make_node_and_inner();
        let link_id = LinkId::from([0u8; 16]);

        // Create with a channel whose receiver is dropped
        let (tx, rx) = mpsc::channel::<TickOutput>(1);
        drop(rx);

        let handle = LinkHandle::new(
            link_id,
            inner,
            tx,
            super::super::completions::CompletionRegistry::new(),
        );

        // Send fails because there's no link, so dispatch channel
        // closure is not reached, but Send(NoLink) is returned first
        let result = handle.try_send(b"hello").await;
        assert!(result.is_err());
    }

    /// Cancellation while the dispatch queue is full happens before core
    /// mutation (and therefore before GCM nonce reservation). After capacity is
    /// made available, a new call commits once and its datagram output is
    /// delivered; no application retransmission is performed by this helper.
    #[tokio::test(flavor = "current_thread")]
    async fn test_datagram_cancelled_under_backpressure_does_not_commit() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (tx, mut rx) = mpsc::channel::<TickOutput>(1);
        tx.try_send(TickOutput::empty())
            .expect("fill dispatch queue");
        let commits = Arc::new(AtomicUsize::new(0));
        let task_tx = tx.clone();
        let task_commits = Arc::clone(&commits);
        let task = tokio::spawn(async move {
            dispatch_datagram(&task_tx, || {
                task_commits.fetch_add(1, Ordering::SeqCst);
                Ok(TickOutput::empty())
            })
            .await
        });

        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(!task.is_finished(), "full queue must apply backpressure");
        assert_eq!(commits.load(Ordering::SeqCst), 0);
        task.abort();
        let _ = task.await;
        assert_eq!(
            commits.load(Ordering::SeqCst),
            0,
            "cancelled call did not commit"
        );

        let _ = rx.try_recv().expect("remove queue filler");
        let committed = Arc::clone(&commits);
        dispatch_datagram(&tx, || {
            committed.fetch_add(1, Ordering::SeqCst);
            Ok(TickOutput::empty())
        })
        .await
        .expect("resumed datagram dispatch");
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        assert!(rx.try_recv().is_ok(), "committed output is delivered");
        assert!(rx.try_recv().is_err(), "exactly one output is delivered");
    }
}
