//! The home node against the real Python LXMF stack (#232, slices 1 and 2
//! of `docs/src/concepts/loquax-home-node.md`): `loquax --daemon`, `loquax
//! home admit|join|revoke|devices` and a client's `loquax fetch` and `loquax
//! send`, each as the shipped binary, around a stock Python sender and
//! recipient. Needs the `tui` feature, which carries the home node's store.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

#[path = "support/python.rs"]
mod python;

use python::{Mesh, PythonPeer, Sent, LNMSG};

/// One `loquax` run with no daemon flag appended: `home admit`, `home
/// revoke`, `address`, the commands that only read and write the state
/// directory.
fn local(home: &std::path::Path, args: &[&str]) -> Sent {
    let output = Command::new(LNMSG)
        .args(args)
        .env("LNMSG_HOME", home)
        .output()
        .expect("run loquax");
    Sent {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        log: String::new(),
    }
}

/// `loquax --daemon` on `home`, killed with its handle.
struct HomeNode {
    process: std::process::Child,
    stderr: PathBuf,
}

impl HomeNode {
    /// Wait for `needle` in the daemon's log, for at most `secs`.
    async fn wait_for(&self, needle: &str, secs: u64) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
        while !self.log().contains(needle) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no {needle:?} in the home node's log within {secs} s:\n{}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn start(home: &std::path::Path, instance: &str) -> Self {
        let stderr = home.join("daemon.stderr");
        let file = std::fs::File::create(&stderr).expect("the daemon's stderr file");
        let mut command = Command::new(LNMSG);
        command
            .args(["--daemon", "--instance", instance, "--sync-interval", "off"])
            .env("LNMSG_HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(file));
        let process = leviculum_std::process::spawn_supervised(command).expect("spawn the daemon");
        let node = Self { process, stderr };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !node.log().contains("HOME up") {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the home node never came up:\n{}",
                node.log()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        node
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }
}

impl Drop for HomeNode {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// The home node's slice 1 end to end (#232): a stock Python LXMF sender
/// delivers DIRECT to `loquax --daemon` and gets DELIVERED; two admitted
/// clients both pull the message, and pulling deletes nothing; a reused
/// admission code is refused; a revoked client gets nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_home_node_takes_direct_mail_and_its_admitted_clients_pull_it() {
    let peer = PythonPeer::start();
    peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    let home = mesh.home.path().to_path_buf();

    // Three admissions on the home node, before it runs: the store is a
    // file, and admitting needs no daemon.
    let mut codes = Vec::new();
    for name in ["laptop", "desk", "stolen"] {
        let admitted = local(&home, &["home", "admit", "--name", name]);
        assert_eq!(admitted.code, Some(0), "{admitted}");
        let code = admitted
            .stdout
            .split_whitespace()
            .find(|word| word.starts_with("lqxh1"))
            .unwrap_or_else(|| panic!("no code in {admitted}"))
            .to_string();
        codes.push(code);
    }
    let address = local(&home, &["address"]);
    assert_eq!(address.code, Some(0), "{address}");
    let address = address.stdout.trim().to_string();

    let node = HomeNode::start(&home, &mesh.instance).await;

    let clients: Vec<tempfile::TempDir> = (0..3)
        .map(|_| tempfile::tempdir().expect("client home"))
        .collect();
    for (client, code) in clients.iter().zip(&codes) {
        let joined = mesh
            .run(
                "home",
                client.path().to_path_buf(),
                vec![
                    "join".to_string(),
                    code.clone(),
                    "--timeout".to_string(),
                    "40".to_string(),
                ],
                Vec::new(),
            )
            .await;
        assert_eq!(joined.code, Some(0), "{joined}\nhome node:\n{}", node.log());
        assert!(joined.stdout.contains(&address), "{joined}");
        // A client's address is its home node's.
        let shown = local(client.path(), &["address"]);
        assert_eq!(shown.stdout.trim(), address, "{shown}");
    }

    // Negative: a used code admits nobody.
    let late = tempfile::tempdir().expect("late client");
    let reused = mesh
        .run(
            "home",
            late.path().to_path_buf(),
            vec![
                "join".to_string(),
                codes[0].clone(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(reused.code, Some(1), "{reused}");
    assert!(reused.stderr.contains("was used"), "{reused}");

    let revoked = local(&home, &["home", "revoke", "stolen"]);
    assert_eq!(revoked.code, Some(0), "{revoked}");
    let devices = local(&home, &["home", "devices"]);
    assert!(
        devices.stdout.contains("laptop")
            && devices.stdout.contains("desk")
            && !devices.stdout.contains("stolen"),
        "{devices}"
    );

    // A stock Python sender delivers DIRECT. It learns the home node's key
    // from the announce the daemon sent at start; until that has crossed,
    // its router says it knows no identity, and the send is asked again.
    let body = "vom Sender, direkt nach Hause";
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let message_hash = loop {
        match peer.try_rpc(
            "lxmf_send",
            serde_json::json!({
                "dest_hash": address,
                "method": "direct",
                "content": hex::encode(body),
            }),
        ) {
            Ok(result) => {
                break result
                    .get("message_hash")
                    .and_then(|v| v.as_str())
                    .expect("a message hash")
                    .to_string()
            }
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "Python never learned the home node: {error}\n{}",
                    node.log()
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    loop {
        let state = peer.rpc(
            "lxmf_get_outbound_status",
            serde_json::json!({ "message_hash": message_hash }),
        );
        match state.get("state").and_then(|v| v.as_str()) {
            Some("DELIVERED") => break,
            Some("FAILED") | Some("REJECTED") => {
                panic!("the sender gave up: {state}\n{}", node.log())
            }
            _ => {}
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the sender never got DELIVERED: {state}\n{}",
            node.log()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        node.log().contains("HOME archived message="),
        "{}",
        node.log()
    );

    // Both admitted clients pull it, the second after the first: fetching
    // deletes nothing on the home node.
    for client in &clients[..2] {
        let fetched = mesh
            .run(
                "fetch",
                client.path().to_path_buf(),
                vec!["--timeout".to_string(), "40".to_string()],
                Vec::new(),
            )
            .await;
        assert_eq!(
            fetched.code,
            Some(0),
            "{fetched}\nhome node:\n{}",
            node.log()
        );
        assert!(
            fetched
                .stdout
                .contains(&format!("message {message_hash} from")),
            "{fetched}"
        );
        assert!(fetched.stdout.contains(body), "{fetched}");
    }
    // The same client again: its cursor is past the message, so nothing new.
    let again = mesh
        .run(
            "fetch",
            clients[0].path().to_path_buf(),
            vec!["--timeout".to_string(), "40".to_string()],
            Vec::new(),
        )
        .await;
    assert_eq!(again.code, Some(0), "{again}");
    assert_eq!(again.stdout, "", "{again}");

    // The revoked client gets nothing, and is told why.
    let refused = mesh
        .run(
            "fetch",
            clients[2].path().to_path_buf(),
            vec!["--timeout".to_string(), "40".to_string()],
            Vec::new(),
        )
        .await;
    assert_eq!(refused.code, Some(1), "{refused}");
    assert_eq!(refused.stdout, "", "{refused}");
    assert!(refused.stderr.contains("not admitted"), "{refused}");

    drop(node);
    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// The home node's slice 2 end to end (#232): a client sends through its
/// home node to a stock Python LXMF recipient that is offline. The client is
/// done once the home node answered and is not running afterwards. The
/// home node's direct leg fails (the router's stock attempts), it has no
/// propagation node to hand a copy to, and it keeps the message. The
/// recipient comes up later and announces; the home node delivers DIRECT
/// and gets the proof; the client's next fetch shows DELIVERED from the
/// archive. A second daemon on the same home is refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_sends_through_its_home_node_to_a_recipient_that_comes_up_later() {
    let peer = PythonPeer::start();
    let mut mesh = Mesh::start(peer.rns_port).await;
    let home = mesh.home.path().to_path_buf();
    // Time compression: the slow cadence of direct retries down from an hour
    // to five seconds, so the announce is not the only way back.
    std::fs::write(home.join("config"), "outbox_retry_secs = 5\n").expect("config");
    let admitted = local(&home, &["home", "admit", "--name", "laptop"]);
    assert_eq!(admitted.code, Some(0), "{admitted}");
    let code = admitted
        .stdout
        .split_whitespace()
        .find(|word| word.starts_with("lqxh1"))
        .unwrap_or_else(|| panic!("no code in {admitted}"))
        .to_string();
    let tokens = local(&home, &["home", "tokens"]);
    assert_eq!(tokens.code, Some(0), "{tokens}");
    assert!(tokens.stdout.contains("laptop  expires"), "{tokens}");

    let node = HomeNode::start(&home, &mesh.instance).await;

    // Negative: one registrar of the delivery destination per home.
    let second = Command::new(LNMSG)
        .args([
            "--daemon",
            "--instance",
            &mesh.instance,
            "--sync-interval",
            "off",
        ])
        .env("LNMSG_HOME", &home)
        .output()
        .expect("run a second daemon");
    assert_eq!(second.status.code(), Some(1));
    let refusal = String::from_utf8_lossy(&second.stderr);
    assert!(
        refusal.contains("the home node (pid") && refusal.contains("already runs"),
        "{refusal}"
    );

    let client = tempfile::tempdir().expect("client home");
    let joined = mesh
        .run(
            "home",
            client.path().to_path_buf(),
            vec![
                "join".to_string(),
                code,
                "--timeout".to_string(),
                "40".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(joined.code, Some(0), "{joined}\nhome node:\n{}", node.log());
    let tokens = local(&home, &["home", "tokens"]);
    assert_eq!(tokens.stdout, "", "a used code is no longer open: {tokens}");

    // The recipient's identity exists before its router does, so the client
    // can write to an address nobody has announced yet.
    let recipient = leviculum_std::generate_identity();
    let recipient_address =
        hex::encode(loquax::engine::delivery_address(&recipient).expect("address"));
    let body = "durch das Zuhause, spaeter zugestellt";
    let sent = mesh
        .run(
            "send",
            client.path().to_path_buf(),
            vec![
                recipient_address.clone(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            body.as_bytes().to_vec(),
        )
        .await;
    assert_eq!(sent.code, Some(0), "{sent}\nhome node:\n{}", node.log());
    let message_id = sent.enqueued_id();
    node.wait_for("HOME send accepted", 5).await;

    // The client is gone. The home node's direct leg runs out of the
    // router's attempts against a recipient that is not there, and with no
    // propagation node configured the message stays in its outbox.
    node.wait_for(
        &format!("HOME outbox direct failed message={message_id}"),
        120,
    )
    .await;
    node.wait_for(
        &format!("HOME outbox no propagation node message={message_id}"),
        5,
    )
    .await;
    assert!(peer.received().is_empty(), "delivered to nobody yet");

    // The recipient comes up and announces.
    let started = peer.rpc(
        "lxmf_init",
        serde_json::json!({
            "identity_prv": hex::encode(recipient.private_key_bytes().expect("private key")),
        }),
    );
    assert_eq!(
        started.get("delivery_hash").and_then(|v| v.as_str()),
        Some(recipient_address.as_str()),
        "Python derives the same address from the same identity"
    );
    peer.lxmf_announce();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let arrived =
        loop {
            if let Some(message) = peer.received().into_iter().find(|m| {
                m.get("content").and_then(|v| v.as_str()) == Some(hex::encode(body).as_str())
            }) {
                break message;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the recipient never got it:\n{}",
                node.log()
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
    assert_eq!(arrived["method"], "direct", "{arrived}");
    assert_eq!(arrived["message_hash"], message_id.as_str(), "{arrived}");
    node.wait_for(&format!("HOME outbox delivered message={message_id}"), 30)
        .await;
    assert_eq!(
        peer.received().len(),
        1,
        "one message, however often it was tried"
    );

    // The client, started again, sees what happened from the archive.
    let fetched = mesh
        .run(
            "fetch",
            client.path().to_path_buf(),
            vec!["--timeout".to_string(), "40".to_string()],
            Vec::new(),
        )
        .await;
    assert_eq!(
        fetched.code,
        Some(0),
        "{fetched}\nhome node:\n{}",
        node.log()
    );
    assert!(
        fetched
            .stdout
            .contains(&format!("message {message_id} accepted")),
        "{fetched}"
    );
    assert!(
        fetched
            .stdout
            .contains(&format!("message {message_id} delivered")),
        "{fetched}"
    );
    // Its cursor moved past it: nothing again.
    let again = mesh
        .run(
            "fetch",
            client.path().to_path_buf(),
            vec!["--timeout".to_string(), "40".to_string()],
            Vec::new(),
        )
        .await;
    assert_eq!(again.code, Some(0), "{again}");
    assert_eq!(again.stdout, "", "{again}");

    drop(node);
    mesh.daemon.stop().await.expect("stop the Rust daemon");
}
