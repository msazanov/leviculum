//! `pn_announce`, in process, over TCP loopback (leviculum#448): one more
//! propagation announce on demand, the verb the Python helper already has
//! (`periculum/assets/scripts/lxmf_node.py`, `pn_announce`). A cell that
//! needs the node's announce to survive a collision asks for a second one;
//! the helper runs no interval of its own.
//!
//! A plain client node dials the hub and counts the announces of the
//! propagation destination it receives: the start announce, then one more
//! for the verb. Before `pn_enable` the verb is refused in words, as the
//! Python twin refuses it.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Helper, Setup, Wire};
use leviculum_core::NodeEvent;
use leviculum_std::driver::ReticulumNodeBuilder;

/// Announces the observer heard, as (destination hash, emission second).
type Heard = Arc<Mutex<Vec<([u8; 16], u64)>>>;

fn parse_hash(raw: &str) -> [u8; 16] {
    let mut hash = [0u8; 16];
    for (index, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw[2 * index..2 * index + 2], 16).expect("hex");
    }
    hash
}

/// Poll `hub` until `done` holds or `budget` passes.
async fn wait_for<F>(hub: &mut Helper, budget: Duration, mut done: F) -> bool
where
    F: FnMut(&Helper) -> bool,
{
    let deadline = std::time::Instant::now() + budget;
    loop {
        hub.drain();
        if done(hub) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn pn_announce_sends_one_more_propagation_announce() {
    let mut hub = Helper::start(Setup {
        transport: true,
        ..Setup::new("pn-hub", Wire::listen_any())
    })
    .await;
    let hub_addr = hub.listen_addr();

    // Negative first: no propagation node, nothing to announce.
    hub.command("pn_announce");
    assert!(
        wait_for(&mut hub, Duration::from_secs(5), |h| h.seen("lxmf_error")).await,
        "pn_announce before pn_enable must be answered"
    );
    let refusal = hub.find("lxmf_error").expect("seen");
    assert_eq!(
        refusal.field("detail"),
        Some("propagation_node_not_enabled"),
        "the refusal must name the reason, as the Python helper's does"
    );
    assert!(!hub.seen("lxmf_pn_announce_sent"));

    // The observer: a client node on the hub that counts announces of
    // every destination it hears.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut client = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .storage_path(dir.path().to_path_buf())
        .add_tcp_client(hub_addr)
        .build()
        .await
        .expect("build client node");
    client.start().await.expect("start client node");
    let mut events = client.take_event_receiver().expect("event rx");
    // Each heard announce as (destination, emission): the emission is the
    // whole-second timestamp in the announce's random hash.
    let heard: Heard = Arc::default();
    let heard_tap = Arc::clone(&heard);
    let tap = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let NodeEvent::AnnounceReceived { announce, .. } = event {
                let mut hash = [0u8; 16];
                hash.copy_from_slice(announce.destination_hash().as_bytes());
                let emission = announce.random_hash()[5..]
                    .iter()
                    .fold(0u64, |acc, byte| (acc << 8) | u64::from(*byte));
                heard_tap.lock().expect("lock").push((hash, emission));
            }
        }
    });
    let count_of = |hash: [u8; 16]| {
        heard
            .lock()
            .expect("lock")
            .iter()
            .filter(|(h, _)| *h == hash)
            .count()
    };

    // A one-second start delay, so the start announce has gone out before
    // the verb runs and the two cannot be one.
    hub.command("pn_enable 1");
    assert!(
        wait_for(&mut hub, Duration::from_secs(15), |h| h
            .seen("lxmf_pn_ready"))
        .await,
        "pn never ready"
    );
    let pn_hash = parse_hash(
        hub.find("lxmf_pn_ready")
            .and_then(|e| e.field("hash"))
            .expect("hash"),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while count_of(pn_hash) < 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "the start announce never reached the client"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Let the start announce's own rebroadcasts settle before counting.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = count_of(pn_hash);
    // A receiver takes an equal-hop announce only if its emission second is
    // newer than the one it holds (`Transport.py:1772`, ours alike): a
    // second announce inside the start announce's second is dropped there,
    // by both stacks. Measured: the verb in the same second went unheard
    // every time, one second later never. The cells call the verb seconds
    // apart; here it waits for the next second.
    let start_emission = heard
        .lock()
        .expect("lock")
        .iter()
        .filter(|(h, _)| *h == pn_hash)
        .map(|(_, emission)| *emission)
        .max()
        .expect("the start announce was heard");
    loop {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_secs();
        if now > start_emission {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    hub.command("pn_announce");
    assert!(
        wait_for(&mut hub, Duration::from_secs(5), |h| h
            .seen("lxmf_pn_announce_sent"))
        .await,
        "pn_announce must confirm with lxmf_pn_announce_sent"
    );
    let sent = hub.find("lxmf_pn_announce_sent").expect("seen");
    assert_eq!(
        sent.field("hash").map(parse_hash),
        Some(pn_hash),
        "the event names the propagation destination"
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while count_of(pn_hash) <= before {
        assert!(
            std::time::Instant::now() < deadline,
            "pn_announce confirmed, but no further announce of {pn_hash:02x?} reached the client"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    client.stop().await.expect("client stops");
    tap.abort();
}
