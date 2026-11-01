use super::*;
use super::super::ext::dispatch_datagram;

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
