//! mvr for 894 — the pre-push mvr lost its port to a neighbour.
//!
//! The land of df09dfec was refused on 2026-10-20: `tcp_client_reports_
//! offline_after_peer_is_gone` drew a port from the allocator and its
//! server's bind was refused with `EADDRINUSE` moments later. The allocator's
//! counter keeps every other *drawer* off the number, but between the probe's
//! release and the consumer's bind the number is unprotected against a socket
//! that never asked the counter (`support/port_alloc.rs`). These two tests
//! pin the mechanism and its fix with the neighbour made deterministic:
//!
//! * the handoff shape loses the port — a stranger binding inside the window
//!   kills the consumer's `start()` with exactly the land's red;
//! * the held-listener shape cannot — the port is bound from the probe until
//!   the node's accept loop owns it, so the stranger is refused instead.
//!
//! **Acceptance**: the second test is red on df09dfec's tree (no
//! `add_tcp_server_listener`; the scenario only exists as the handoff, where
//! the stranger wins), green once the listener can be handed over bound.

use std::net::{SocketAddr, TcpListener};

use leviculum_std::driver::ReticulumNodeBuilder;

/// The land's failure, deterministic: anything on the host that binds the
/// drawn number inside the handoff window takes it from the consumer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_handoff_window_loses_the_port_to_a_stranger() {
    // port-handoff: this draw re-creates the window under test on purpose;
    // the stranger below is the deterministic neighbour.
    let port = crate::ports::free_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    // The stranger: a socket that never asked the allocator, arriving after
    // the probe released the number and before the consumer binds it.
    let _stranger = TcpListener::bind(addr).expect("stranger bind");

    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_server(addr)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build");

    let refused = node
        .start()
        .await
        .expect_err("a handoff consumer must lose its port to a bind inside the window");
    let text = format!("{refused:?}");
    assert!(
        text.contains("AddrInUse"),
        "the stranger's hold must be what refused the start: {text}"
    );
}

/// The fix: the port is held from the allocator's probe to the node's accept
/// loop, so the same stranger is refused and the node starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_listener_keeps_its_port_through_the_handoff() {
    let listener = crate::ports::held_band_listener();
    let addr = listener.local_addr().expect("listener addr");

    // port-handoff: there is none left here — this bind is the stranger from
    // the test above, asserting the held port refuses it.
    let stranger = TcpListener::bind(addr);
    assert!(
        stranger.is_err(),
        "the held port must not be bindable by a socket that never asked the allocator"
    );

    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_server_listener(listener)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build");
    node.start().await.expect("start");

    assert_eq!(
        node.tcp_listen_addrs(),
        vec![addr],
        "the node must report the held listener's own address"
    );

    // The listener is live, not just bound: a dial reaches the accept loop.
    let connected = tokio::net::TcpStream::connect(addr).await;
    assert!(
        connected.is_ok(),
        "the handed-over listener must accept: {connected:?}"
    );

    node.stop().await.expect("stop");
}
