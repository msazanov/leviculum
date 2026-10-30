//! Interop tests for per-link ciphers: a node whose own Links use GCM mode 7 still
//! admits standard CBC Links from Python Reticulum, and Channel data on them is proven
//! with the vanilla packet hash in both directions.
use std::time::Duration;

use leviculum_core::identity::Identity;
use leviculum_core::link::LinkId;
use leviculum_core::node::NodeEvent;
use leviculum_std::EventReceiver;
use leviculum_core::{Destination, DestinationType, Direction, LinkCipher, ProofStrategy};

use crate::common::*;
use crate::harness::*;

/// Build a Rust node whose own Links use `node_cipher`, optionally also
/// accepting CBC Links as responder.
pub async fn build_rust_node_with_link_profile(
    daemon: &crate::harness::TestDaemon,
    node_cipher: leviculum_core::LinkCipher,
    accept_cbc: bool,
) -> (
    leviculum_std::driver::ReticulumNode,
    EventReceiver,
    tempfile::TempDir,
) {
    let storage = temp_storage("build_rust_node", "node");
    let mut node = leviculum_std::driver::ReticulumNodeBuilder::new()
        .add_tcp_client(daemon.rns_addr())
        .storage_path(storage.path().to_path_buf())
        .link_cipher(node_cipher)
        .accept_cbc_links(accept_cbc)
        .build()
        .await
        .expect("Failed to build node");

    let event_rx = node.take_event_receiver().unwrap();
    node.start().await.expect("Failed to start node");

    tokio::time::sleep(Duration::from_secs(1)).await;

    (node, event_rx, storage)
}

async fn wait_for_channel_delivery_confirmation(
    event_rx: &mut leviculum_std::EventReceiver,
    link_id: &leviculum_core::link::LinkId,
    timeout: Duration,
) -> (usize, usize) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut confirmations = 0;
    let mut retransmits = 0;
    while confirmations == 0 && tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        match tokio::time::timeout(remaining, event_rx.recv()).await {
            Ok(Some(NodeEvent::LinkDeliveryConfirmed { link_id: id, .. })) if id == *link_id => {
                confirmations += 1;
            }
            Ok(Some(NodeEvent::ChannelRetransmit { link_id: id, .. })) if id == *link_id => {
                retransmits += 1;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    (confirmations, retransmits)
}

/// Bidirectional channel messages: Rust→Python + Python→Rust
#[tokio::test]
async fn test_channel_message_bidirectional_cbc_link_on_a_gcm_node() {
    let daemon = TestDaemon::start().await.expect("Failed to start daemon");
    // Match vpn.rs start_base(): GCM is the node's own-Link default, CBC is
    // admitted for public RNS enrollment, and proof binding follows the Link.
    let (mut rust_node, mut event_rx, _storage) = build_rust_node_with_link_profile(
        &daemon,
        LinkCipher::Gcm,
        true,
    )
    .await;

    let dest_info = daemon
        .register_destination("enc_test", &["bidir"])
        .await
        .expect("Failed to register destination");

    daemon
        .announce_destination(&dest_info.hash, b"bidir-test")
        .await
        .expect("Announce failed");

    let py_dest_hash = parse_dest_hash(&dest_info.hash);
    let found = crate::common::wait_for_path_reannounce(
        || rust_node.has_path(&py_dest_hash),
        &daemon,
        &dest_info.hash,
        b"bidir-test",
        Duration::from_secs(10),
    )
    .await;
    assert!(found, "Rust should learn path");

    let signing_key = extract_signing_key(&dest_info.public_key);
    let link_handle = rust_node
        .connect_with_cipher(&py_dest_hash, &signing_key, LinkCipher::Cbc)
        .await
        .expect("CBC enrollment Link should succeed");

    let link_id = *link_handle.link_id();
    let established =
        wait_for_link_established(&mut event_rx, &link_id, Duration::from_secs(10)).await;
    assert!(established, "Link should be established");
    assert_eq!(
        rust_node.with_link(&link_id, |l| l.cipher()).unwrap(),
        LinkCipher::Cbc
    );
    let bytes_before = rust_node.link_stats(&link_id).unwrap().bytes_delivered();

    // Rust→Python
    let r2p_payload = b"rust to python bidir";
    link_handle
        .send(r2p_payload)
        .await
        .expect("Rust link send should succeed");

    let (confirmations, rust_retransmits) =
        wait_for_channel_delivery_confirmation(&mut event_rx, &link_id, Duration::from_secs(10))
            .await;
    assert_eq!(
        confirmations, 1,
        "Python's PacketHash proof confirms Rust send"
    );
    assert_eq!(
        rust_retransmits, 0,
        "the Rust Channel envelope was not retransmitted"
    );
    let delivered_stats = rust_node.link_stats(&link_id).unwrap();
    assert_eq!(delivered_stats.tx_ring_size(), 0);
    assert!(delivered_stats.bytes_delivered() > bytes_before);

    let py_received = daemon
        .get_received_packets()
        .await
        .expect("Failed to get packets");
    assert!(
        py_received.iter().any(|p| p.data == r2p_payload),
        "Python should receive Rust's message"
    );

    // Python→Rust
    let links = daemon.get_links().await.expect("Failed to get links");
    let py_link_hash = links.keys().next().unwrap().clone();

    let p2r_payload = b"python to rust bidir";
    let (_, python_receipt_state, python_pending, python_retries) = daemon
        .send_channel_on_link(&py_link_hash, p2r_payload, Duration::from_secs(10))
        .await
        .expect("Python Channel send should complete");
    assert_eq!(python_receipt_state, "MSGSTATE_DELIVERED");
    assert!(
        !python_pending,
        "Rust PacketHash proof clears Python Channel tx ring"
    );
    assert_eq!(
        python_retries, 1,
        "Python Channel used only its initial attempt"
    );

    let rust_received = wait_for_data_event(&mut event_rx, &link_id, Duration::from_secs(10)).await;
    assert!(
        rust_received.is_some(),
        "Rust should receive Python's message"
    );
    assert_eq!(
        rust_received.unwrap(),
        p2r_payload,
        "Bidirectional data should match"
    );

    rust_node.stop().await.expect("Failed to stop node");
}

async fn setup_python_initiated_link_with_profile(
    proof_strategy: ProofStrategy,
    test_name: &str,
    node_cipher: leviculum_core::LinkCipher,
    accept_cbc_links: bool,
) -> (
    leviculum_std::driver::ReticulumNode,
    leviculum_std::EventReceiver,
    TestDaemon,
    LinkId,
    String,
    tempfile::TempDir,
) {
    let daemon = TestDaemon::start().await.expect("Failed to start daemon");

    let storage = temp_storage(test_name, "node");
    let mut rust_node = leviculum_std::driver::ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(daemon.rns_addr())
        .storage_path(storage.path().to_path_buf())
        .link_cipher(node_cipher)
        .accept_cbc_links(accept_cbc_links)
        .build()
        .await
        .expect("Failed to build node");
    let mut event_rx = rust_node.take_event_receiver().unwrap();
    rust_node.start().await.expect("Failed to start node");
    tokio::time::sleep(Duration::from_secs(1)).await;

    let identity = Identity::generate(&mut OsRng);
    let public_key_hex = hex::encode(identity.public_key_bytes());
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "rawlink",
        &["responder"],
    )
    .expect("destination");
    dest.set_accepts_links(true);
    dest.set_proof_strategy(proof_strategy);
    let dest_hash = *dest.hash();
    let dest_hash_hex = hex::encode(dest_hash.as_bytes());

    rust_node.register_destination(dest);
    rust_node
        .announce_destination(&dest_hash, Some(b"rawlink-responder"))
        .await
        .expect("announce");

    let has_path = wait_for_node_reannounce_on_daemon(
        &daemon,
        &dest_hash,
        &rust_node,
        b"rawlink-responder",
        Duration::from_secs(20),
    )
    .await;
    assert!(has_path, "daemon should learn path to Rust destination");

    let create_link_handle = {
        let cmd_addr = daemon.cmd_addr();
        let dh = dest_hash_hex.clone();
        let pk = public_key_hex.clone();
        tokio::spawn(async move { create_link_raw(cmd_addr, &dh, &pk, 30).await })
    };

    let link_id = wait_for_responder_established_link(&mut event_rx, Duration::from_secs(15))
        .await
        .expect("Rust should establish the incoming link");
    let py_link_hash = create_link_handle
        .await
        .expect("create_link task panicked")
        .expect("Python create_link should succeed");

    (rust_node, event_rx, daemon, link_id, py_link_hash, storage)
}

/// A node whose own Links default to GCM, admits CBC enrollment
/// Links, and resolves proof binding per Link. A Python initiator requests CBC;
/// Channel payload and PacketHash proofs must work in both directions.
#[tokio::test]
async fn test_gcm_node_python_channel_cbc_packet_hash_ack() {
    let (mut rust_node, mut event_rx, daemon, link_id, python_link_hash, _storage) =
        setup_python_initiated_link_with_profile(
            ProofStrategy::None,
            "vpn_profile_cbc_channel_hash",
            leviculum_core::LinkCipher::Gcm,
            true,
        )
        .await;
    assert_eq!(
        rust_node.with_link(&link_id, |l| l.cipher()).unwrap(),
        leviculum_core::LinkCipher::Cbc
    );

    let python_payload = b"Python CBC Channel PacketHash";
    let (_, receipt_state, python_pending, python_tries) = daemon
        .send_channel_on_link(&python_link_hash, python_payload, Duration::from_secs(10))
        .await
        .expect("Python Channel send and receipt polling");
    assert_eq!(receipt_state, "MSGSTATE_DELIVERED");
    assert!(
        !python_pending,
        "Rust's proof clears Python's Channel tx ring"
    );
    assert_eq!(python_tries, 1, "Python used only its initial attempt");
    assert_eq!(
        crate::common::wait_for_data_event(&mut event_rx, &link_id, Duration::from_secs(5)).await,
        Some(python_payload.to_vec())
    );

    let link = rust_node.link_handle(&link_id);
    let delivered_before = rust_node.link_stats(&link_id).unwrap().bytes_delivered();
    let rust_payload = b"Rust CBC Channel PacketHash";
    link.send(rust_payload).await.expect("Rust Channel send");
    let confirmations =
        crate::common::wait_for_delivery_confirmations(&mut event_rx, 1, Duration::from_secs(10))
            .await;
    assert_eq!(confirmations, 1, "Python's proof clears Rust's receipt");
    let stats = rust_node.link_stats(&link_id).unwrap();
    assert_eq!(stats.tx_ring_size(), 0);
    assert!(stats.bytes_delivered() > delivered_before);

    let received = daemon.get_received_packets().await.unwrap();
    assert!(received.iter().any(|packet| packet.data == rust_payload));
    rust_node.stop().await.expect("stop node");
}
