//! `loquax send` against the real Python LXMF stack (`reference/LXMF`).
//!
//! Standing policy is that every feature gets interop tests against real
//! Python, positive and negative, before it merges. This is that test for the
//! send path, and it drives the shipped binary rather than the library, so
//! what is asserted is exactly what a cron job would get: an exit code, a
//! silent stdout, and a message in someone else's inbox.
//!
//! The interop claim rests on one identity: the id our `LNMSG_ENQUEUED` line
//! carries is the `message_hash` **Python** computed over the message it
//! received. Both sides derive it independently from the LXMF wire bytes, so
//! the match is agreement between two stacks and not a value we handed over.
//!
//! ```text
//!   loquax (subprocess)                    python3 scripts/test_daemon.py
//!         │                                    (real RNS + LXMF.LXMRouter)
//!         │ shared-instance IPC                     │
//!         v                                         │ TCP
//!   in-process Rust daemon ────────────────────────-┘
//!   (share_instance + TCP client)
//! ```
//!
//! The Rust daemon is built in-process rather than by spawning `lnsd`: it is
//! the same `ReticulumNodeBuilder` configuration `lnsd` assembles from a
//! config file, and `CARGO_BIN_EXE_` only reaches this package's own binaries.
//! What is under test is `loquax`, and it attaches over the same IPC socket
//! either way — the drop-in property is exercised more directly by the
//! `rnsd_interop` suites, which point the same clients at both daemons.

use std::time::Duration;

#[path = "support/python.rs"]
mod python;

use python::{Mesh, PythonPeer};

/// Poll the Python router until it has delivered the message with this id.
async fn wait_for_python(peer: &PythonPeer, id: &str, within: Duration) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let seen = peer.received();
        if let Some(message) = seen
            .iter()
            .find(|m| m.get("message_hash").and_then(|v| v.as_str()) == Some(id))
        {
            return message.clone();
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("Python never received {id}; it holds: {seen:?}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn hex_bytes(message: &serde_json::Value, key: &str) -> Vec<u8> {
    hex::decode(
        message
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("{key} is missing from {message}")),
    )
    .unwrap_or_else(|_| panic!("{key} is not hex in {message}"))
}

/// Positive: a message sent by the shipped binary arrives in a real Python
/// LXMF inbox with its body and title intact, non-ASCII included.
#[tokio::test(flavor = "multi_thread")]
async fn lnmsg_send_reaches_a_python_lxmf_receiver() {
    let peer = PythonPeer::start();
    let python_address = peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    peer.lxmf_announce();

    // Non-ASCII in both body and title: LXMF carries bytes, and a messenger
    // that mangles them is useless to everyone outside en_US.
    let body = "Grüße vom Dachboden — 91 % voll ☂";
    let title = "Füllstand";

    let sent = mesh
        .send(
            vec![
                python_address.clone(),
                "--title".to_string(),
                title.to_string(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            format!("{body}\n").into_bytes(),
        )
        .await;

    assert_eq!(sent.code, Some(0), "{sent}");
    // Byte-exact, not "contains no hex blob": an assertion that merely failed
    // to find an id would pass for any other thing we started printing.
    assert_eq!(
        sent.stdout, "",
        "a successful send says nothing on stdout: {sent}"
    );

    // The id from our own event log; `wait_for_python` accepts it only if it
    // equals the `message_hash` Python computed over what it received.
    let id = sent.enqueued_id();
    let message = wait_for_python(&peer, &id, Duration::from_secs(40)).await;
    assert_eq!(
        String::from_utf8(hex_bytes(&message, "content")).expect("the body round-trips as UTF-8"),
        body,
        "the body must arrive byte for byte"
    );
    assert_eq!(
        String::from_utf8(hex_bytes(&message, "title")).expect("the title round-trips as UTF-8"),
        title
    );
    assert_eq!(
        message.get("destination_hash").and_then(|v| v.as_str()),
        Some(python_address.as_str())
    );
    // loquax announces its own delivery destination before sending precisely so
    // this holds: without the announce the reference marks our message
    // SOURCE_UNKNOWN and nobody could reply to us.
    assert_eq!(
        message.get("signature_validated").and_then(|v| v.as_bool()),
        Some(true),
        "Python must be able to validate our signature: {message}"
    );

    // The name a recipient actually sees. Python resolves it out of the
    // announce app_data the way its clients do, so this is the only check that
    // covers the whole path: resolution, announce, wire, reference decoder.
    // The expected value is resolved here by the same code the child ran,
    // rather than hardcoded, because the account name differs per host.
    let expected_name = loquax::display_name::from_process(None)
        .expect("no override is set for the test process")
        .name;
    assert_eq!(
        message.get("source_display_name").and_then(|v| v.as_str()),
        Some(expected_name.as_str()),
        "Python must see the operator's name, not the tool's: {message}"
    );
    assert_ne!(
        message.get("source_display_name").and_then(|v| v.as_str()),
        Some("Loquax"),
        "'Loquax' names the program, not the person it sends for"
    );

    // And `--from` is what an operator uses when the bare account name is
    // ambiguous — three hosts, one user. Same identity, so this run's announce
    // replaces the name Python holds for us.
    let overridden = mesh
        .send(
            vec![
                python_address.clone(),
                "--from".to_string(),
                "lew@schneckenschreck".to_string(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            b"second line\n".to_vec(),
        )
        .await;
    assert_eq!(overridden.code, Some(0), "{overridden}");
    assert_eq!(
        overridden.stdout, "",
        "a successful send says nothing on stdout: {overridden}"
    );
    let overridden_id = overridden.enqueued_id();
    let message = wait_for_python(&peer, &overridden_id, Duration::from_secs(40)).await;
    assert_eq!(
        message.get("source_display_name").and_then(|v| v.as_str()),
        Some("lew@schneckenschreck"),
        "--from must reach the receiver verbatim: {message}"
    );

    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// Negative: an address that exists nowhere in the mesh must not produce a
/// success exit — a script that trusted exit 0 here would report a status line
/// nobody ever received. Silence alone means nothing now that a success is
/// silent too; the exit code is the whole signal, so this asserts it exactly.
#[tokio::test(flavor = "multi_thread")]
async fn a_destination_that_does_not_exist_is_not_a_success() {
    let peer = PythonPeer::start();
    peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    peer.lxmf_announce();

    // Well-formed, and belonging to nobody: same length and shape as the real
    // one above, so what is being tested is reachability and not parsing.
    let nowhere = "0123456789abcdef0123456789abcdef";

    let sent = mesh
        .send(
            vec![
                nowhere.to_string(),
                "--timeout".to_string(),
                "8".to_string(),
            ],
            b"this goes nowhere\n".to_vec(),
        )
        .await;

    assert_ne!(
        sent.code,
        Some(0),
        "an unreachable address is not a success"
    );
    assert_eq!(sent.code, Some(1), "{sent}");
    assert_eq!(sent.stdout, "", "stdout stays empty on failure too: {sent}");
    assert!(
        sent.stderr.contains(nowhere),
        "the error must name the address it could not reach: {sent}"
    );
    assert!(
        peer.received().is_empty(),
        "nothing may have reached the Python router"
    );

    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// `--via auto`, recipient online: the direct delivery link comes up, the
/// message arrives at the real Python receiver, and the exit code is 0 —
/// never 3, because no mailbox was involved. This is the leg that has to
/// work for auto to be an honest default rather than a propagation tax.
#[tokio::test(flavor = "multi_thread")]
async fn via_auto_delivers_direct_when_the_python_receiver_is_online() {
    let peer = PythonPeer::start();
    let python_address = peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    peer.lxmf_announce();

    let sent = mesh
        .send(
            vec![
                python_address.clone(),
                "--via".to_string(),
                "auto".to_string(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            b"direct while online\n".to_vec(),
        )
        .await;

    assert_eq!(sent.code, Some(0), "{sent}");
    assert_eq!(sent.stdout, "", "a successful send says nothing: {sent}");
    assert!(
        sent.log.contains("LNMSG_VIA")
            && sent.log.contains("method=direct")
            && sent.log.contains("reason=link-delivery"),
        "the decision line must record the direct leg winning: {sent}"
    );

    let id = sent.enqueued_id();
    let message = wait_for_python(&peer, &id, Duration::from_secs(40)).await;
    assert_eq!(
        String::from_utf8(hex_bytes(&message, "content")).expect("utf8"),
        "direct while online"
    );

    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// The `t=` of the first event-log line that starts with `prefix` and
/// carries every one of `fields`.
fn event_ms(log: &str, prefix: &str, fields: &[&str]) -> Option<u64> {
    log.lines()
        .filter(|line| line.starts_with(prefix))
        .find(|line| {
            fields
                .iter()
                .all(|field| line.split_whitespace().any(|token| token == *field))
        })
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|token| token.strip_prefix("t="))
                .and_then(|t| t.parse().ok())
        })
}

/// The compose line's send against a stock Python LXMF receiver on one
/// daemon: the direct leg is proven well inside a second (leviculum 1002).
///
/// Lew's hand test and the 1001 cell measured 3985 to 4008 ms from Enter to
/// the receive on one host, four runs alike, with the wire exchange itself
/// about 4 ms: the router held a message queued between two processing
/// passes until the next pass of its four-second interval. The reference
/// sends from `handle_outbound` at once, so the same receiver took 3 ms from
/// `lxmf_node send`. Measured between our own `LNMSG_ENQUEUED` and the
/// proof's `LNMSG_STATE`, so attaching and resolving are not in the number;
/// 2 s leaves the link handshake a wide margin on a loaded runner and is
/// still half of what the old interval cost.
#[tokio::test(flavor = "multi_thread")]
async fn via_auto_to_a_python_receiver_is_proven_well_inside_a_second() {
    let peer = PythonPeer::start();
    let python_address = peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    peer.lxmf_announce();

    let sent = mesh
        .send(
            vec![
                python_address,
                "--via".to_string(),
                "auto".to_string(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            b"how long does this take\n".to_vec(),
        )
        .await;
    assert_eq!(sent.code, Some(0), "{sent}");

    let queued = event_ms(&sent.log, "LNMSG_ENQUEUED ", &[]).expect("an enqueue line");
    let proven = event_ms(&sent.log, "LNMSG_STATE ", &["state=Delivered"])
        .unwrap_or_else(|| panic!("the direct leg ends on the proof: {sent}"));
    let took = proven.saturating_sub(queued);
    eprintln!("enqueue to proof: {took} ms");
    assert!(
        took < 2_000,
        "{took} ms from the enqueue to the proof; the router waited for its \
         processing interval: {sent}"
    );

    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// The whole mailbox round against a real Python propagation node: a sender
/// client uploads with `--via propagated` (exit 3, "a node holds it"), the
/// Python node stores it, and the recipient client drains it with the
/// genuine list/fetch/confirm round — the same flow the periculum control
/// cell runs with lxmd, here against `LXMRouter.enable_propagation` in
/// process. A second drain proves the confirm purged the store AND that the
/// seen store suppresses a reprint.
#[tokio::test(flavor = "multi_thread")]
async fn propagated_upload_and_fetch_round_trip_through_a_python_node() {
    let peer = PythonPeer::start();
    peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    let pn_hash = peer
        .rpc("lxmf_enable_propagation", serde_json::json!({}))
        .get("propagation_hash")
        .and_then(|v| v.as_str())
        .expect("the Python node reports its propagation hash")
        .to_string();
    peer.rpc("lxmf_announce_propagation_node", serde_json::json!({}));

    let sender_home = tempfile::tempdir().expect("sender home");
    let recipient_home = tempfile::tempdir().expect("recipient home");

    // The recipient announces itself once (any run announces on attach), so
    // the sender can encrypt to it; the drain coming back empty is itself
    // the negative half of the round.
    let first_drain = mesh
        .run(
            "fetch",
            recipient_home.path().to_path_buf(),
            vec![
                "--pn".to_string(),
                pn_hash.clone(),
                "--timeout".to_string(),
                "60".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(first_drain.code, Some(0), "{first_drain}");
    assert_eq!(
        first_drain.stdout, "",
        "an empty mailbox prints nothing: {first_drain}"
    );
    let recipient_address = {
        let line = first_drain
            .log
            .lines()
            .find(|line| line.starts_with("LNMSG_ATTACHED "))
            .expect("the drain attached");
        line.split_whitespace()
            .find_map(|token| token.strip_prefix("address="))
            .expect("the attach line carries our address")
            .to_string()
    };

    let sent = mesh
        .run(
            "send",
            sender_home.path().to_path_buf(),
            vec![
                recipient_address.clone(),
                "--via".to_string(),
                "propagated".to_string(),
                "--pn".to_string(),
                pn_hash.clone(),
                "--title".to_string(),
                "mailbox".to_string(),
                "--timeout".to_string(),
                "60".to_string(),
            ],
            b"held by the python node\n".to_vec(),
        )
        .await;
    assert_eq!(
        sent.code,
        Some(3),
        "a node holding the message is exit 3, not 0: {sent}"
    );
    assert_eq!(sent.stdout, "", "{sent}");

    let drained = mesh
        .run(
            "fetch",
            recipient_home.path().to_path_buf(),
            vec![
                "--pn".to_string(),
                pn_hash.clone(),
                "--timeout".to_string(),
                "60".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(drained.code, Some(0), "{drained}");
    assert!(
        drained.stdout.contains("held by the python node"),
        "the body must come back out of the Python node's store: {drained}"
    );
    assert!(
        drained.stdout.contains("title mailbox"),
        "the title travels too: {drained}"
    );

    let empty_again = mesh
        .run(
            "fetch",
            recipient_home.path().to_path_buf(),
            vec![
                "--pn".to_string(),
                pn_hash,
                "--timeout".to_string(),
                "60".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(empty_again.code, Some(0), "{empty_again}");
    assert_eq!(
        empty_again.stdout, "",
        "the confirm purged the store and the seen store suppresses reprints: {empty_again}"
    );

    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// Negative: a propagation node hash that belongs to nobody must fail inside
/// the timeout with the no-node/no-path story, never hang and never
/// fabricate an empty success.
#[tokio::test(flavor = "multi_thread")]
async fn a_fetch_from_a_nonexistent_node_fails_cleanly() {
    let peer = PythonPeer::start();
    peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;

    let nowhere = "0123456789abcdef0123456789abcdef";
    let fetched = mesh
        .run(
            "fetch",
            mesh.home.path().to_path_buf(),
            vec![
                "--pn".to_string(),
                nowhere.to_string(),
                "--timeout".to_string(),
                "25".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(fetched.code, Some(1), "{fetched}");
    assert_eq!(fetched.stdout, "", "no messages may be invented: {fetched}");

    mesh.daemon.stop().await.expect("stop the Rust daemon");
}
