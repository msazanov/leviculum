//! mvr for 711 — an application's second path request inside the
//! transport's 20 s minimum interval never reached the wire.
//!
//! `Transport::request_path` drops any request for a destination asked for
//! less than `PATH_REQUEST_MIN_INTERVAL_MS` ago. The reference applies that
//! interval (`PATH_REQUEST_MI`, Transport.py:83) only to its own automated
//! requests (Transport.py:538, :703); the application API
//! `Transport.request_path` (Transport.py:2771) always transmits. Our
//! `ReticulumNode::request_path` is that API — the client tools, the FFI and
//! every embedding application reach the core through it — and it went
//! through the throttled call, so an application's retry a few seconds after
//! its first request put nothing on the wire and said nothing.
//!
//! The node's TCP client connects to a plain listener standing in for the
//! peer, and the test counts the path requests for one destination that
//! arrive there. No daemon, no Python, under five seconds.
//!
//! **Acceptance**: red while the driver's `request_path` is throttled (one
//! request seen), green once it always transmits (two).

use std::time::Duration;

use leviculum_core::DestinationHash;
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::interfaces::hdlc::{DeframeResult, Deframer};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

const DEST_HASH_LEN: usize = 16;
/// Type-1 header: flags(1) + hops(1) + dest(16) + context(1).
const HEADER_LEN: usize = 2 + DEST_HASH_LEN + 1;

/// Whether `raw` is a non-transport node's path request for `target`: a
/// Data packet whose payload is `target(16) + tag(16)` (Transport.py:2783).
fn is_path_request_for(raw: &[u8], target: &[u8; DEST_HASH_LEN]) -> bool {
    raw.len() == HEADER_LEN + 2 * DEST_HASH_LEN
        && raw[0] & 0b11 == 0b00
        && &raw[HEADER_LEN..HEADER_LEN + DEST_HASH_LEN] == target
}

/// Count the path requests for `target` that arrive on `stream` within
/// `window`.
async fn count_path_requests(
    stream: &mut TcpStream,
    target: &[u8; DEST_HASH_LEN],
    window: Duration,
) -> usize {
    let mut deframer = Deframer::new();
    let mut buf = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + window;
    let mut seen = 0;
    while let Ok(read) = tokio::time::timeout_at(deadline, stream.read(&mut buf)).await {
        let n = read.expect("read from node connection");
        assert!(n > 0, "node closed the connection");
        for result in deframer.process(&buf[..n]) {
            if let DeframeResult::Frame(data) = result {
                if is_path_request_for(&data, target) {
                    seen += 1;
                }
            }
        }
    }
    seen
}

#[tokio::test]
async fn a_second_application_path_request_inside_the_interval_leaves() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(listener.local_addr().expect("listener addr"))
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build node");
    node.start().await.expect("start node");
    let (mut peer, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("timed out waiting for the node's TCP connect")
        .expect("accept node connection");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let target = DestinationHash::new([0x71; DEST_HASH_LEN]);
    node.request_path(&target).await.expect("first request");
    // A retry a second later, far inside the 20 s interval.
    tokio::time::sleep(Duration::from_secs(1)).await;
    node.request_path(&target).await.expect("second request");

    let seen = count_path_requests(&mut peer, target.as_bytes(), Duration::from_secs(1)).await;
    node.stop().await.ok();
    println!("APP_PATH_REQUESTS asked=2 on_wire={seen}");
    assert_eq!(
        seen, 2,
        "both application path requests must reach the wire, as the \
         reference's Transport.request_path always transmits"
    );
}
