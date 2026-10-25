//! The mailbox adoption rule (`lnmsg/src/mailbox.rs`) through the real
//! binary against real daemons: which node a run without `--pn` adopts, what
//! it leaves in `${LNMSG_HOME}/mailbox`, and the one stderr line it prints.
//!
//! Two daemons, built in-process the way `python_interop.rs` builds its one:
//! `here`, the daemon lnmsg attaches to, and `elsewhere`, joined to it over
//! TCP. A propagation node is a client of one of them that announces an
//! enabled `lxmf.propagation` destination. Which daemon it sits on is the
//! whole difference between "on this device" and "a fine mailbox", and the
//! path table tells them apart as measured in order 967: zero hops behind
//! `LocalInterface[…]` against one hop behind the TCP interface. The nodes
//! answer no requests, so every run here ends in a failed drain after the
//! selection; the selection is what is under test.

use std::io::Write;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use leviculum_core::{Destination, DestinationHash, DestinationType, Direction};
use leviculum_lxmf::propagation::PropagationNodeAnnounce;
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::ReticulumNode;

const LNMSG: &str = env!("CARGO_BIN_EXE_lnmsg");

/// The node-on-this-device text `NoneReason::OnlyOnThisDevice` carries.
const ONLY_ON_THIS_DEVICE: &str = "the only propagation node heard runs on this device";

struct Daemon {
    node: ReticulumNode,
    instance: String,
    /// The config directory lnmsg is pointed at with `--config`: the authkey
    /// is read from `storage/transport_identity` under it.
    config_dir: tempfile::TempDir,
}

fn unique(tag: &str) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "lnmsg-mbx-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

async fn daemon(
    tag: &str,
    tcp: impl FnOnce(ReticulumNodeBuilder) -> ReticulumNodeBuilder,
) -> Daemon {
    let instance = unique(tag);
    let config_dir = tempfile::tempdir().expect("daemon config dir");
    let builder = ReticulumNodeBuilder::new()
        // A user's daemon: no transport, as `lnsd --writeconfig` writes it.
        .enable_transport(false)
        .share_instance(true)
        .instance_name(instance.clone())
        .storage_path(config_dir.path().join("storage"));
    let mut node = tcp(builder).build_sync().expect("build daemon");
    node.start().await.expect("start daemon");
    Daemon {
        node,
        instance,
        config_dir,
    }
}

/// `here` and `elsewhere`, joined over loopback TCP.
async fn two_daemons() -> (Daemon, Daemon) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("listener address");
    let elsewhere = daemon("elsewhere", |b| b.add_tcp_server_listener(listener)).await;
    let here = daemon("here", |b| b.add_tcp_client(addr)).await;
    (here, elsewhere)
}

/// A propagation node attached to `daemon`, announced once.
struct PropagationNode {
    hash: [u8; 16],
    _client: ReticulumNode,
    _storage: tempfile::TempDir,
}

async fn propagation_node(daemon: &Daemon) -> PropagationNode {
    let storage = tempfile::tempdir().expect("pn storage");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&daemon.instance)
        .storage_path(storage.path().to_path_buf())
        .build_sync()
        .expect("build pn client");
    node.start().await.expect("attach pn client");
    let destination = Destination::new(
        Some(leviculum_std::generate_identity()),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["propagation"],
    )
    .expect("propagation destination");
    let hash: DestinationHash = *destination.hash();
    node.register_destination(destination);
    let app_data = PropagationNodeAnnounce {
        legacy_support: false,
        timebase: 0,
        enabled: true,
        transfer_limit_kb: 256,
        sync_limit_kb: 10_240,
        stamp_cost: 0,
        stamp_cost_flexibility: 0,
        peering_cost: 0,
        metadata: Vec::new(),
    }
    .encode()
    .expect("encode the announce");
    node.announce_destination(&hash, Some(&app_data))
        .await
        .expect("announce");
    PropagationNode {
        hash: *hash.as_bytes(),
        _client: node,
        _storage: storage,
    }
}

/// Wait until `daemon`'s path table, read over the RPC lnmsg reads it
/// through, holds `node` at `hops`.
async fn wait_for_path(daemon: &Daemon, node: &[u8; 16], hops: u64) {
    let access = leviculum_std::daemon_rpc::DaemonAccess::resolve(
        Some(daemon.config_dir.path()),
        Some(&daemon.instance),
    )
    .expect("daemon access");
    let hex: String = node.iter().map(|b| format!("{b:02x}")).collect();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let table = access.query("path_table").await.expect("path table");
        let found = table.as_array().is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry["hash"].as_str() == Some(hex.as_str()) && entry["hops"].as_u64() == Some(hops)
            })
        });
        if found {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{hex} never reached the path table at {hops} hops: {table}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct Run {
    code: Option<i32>,
    stderr: String,
}

/// One `lnmsg fetch` against `daemon`, on a blocking thread so the daemons'
/// runtime keeps serving while the child talks to them.
async fn fetch(daemon: &Daemon, home: &Path, extra: &[&str]) -> Run {
    let mut args: Vec<String> = vec![
        "fetch".into(),
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--timeout".into(),
        "4".into(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &home)
            .env_remove("LEVICULUM_EVENT_LOG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = leviculum_std::process::spawn_supervised(command).expect("spawn lnmsg");
        drop(child.stdin.take());
        let output = child.wait_with_output().expect("wait for lnmsg");
        Run {
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    })
    .await
    .expect("the lnmsg child thread")
}

fn hex(node: &[u8; 16]) -> String {
    node.iter().map(|b| format!("{b:02x}")).collect()
}

fn mailbox_file(home: &Path) -> Option<String> {
    std::fs::read_to_string(home.join("mailbox")).ok()
}

fn home() -> tempfile::TempDir {
    tempfile::tempdir().expect("lnmsg home")
}

/// First run: the node elsewhere is adopted, though the node on this device
/// announced after it and is the most recently heard. The file says
/// `adopted`. Second run: the stored node is kept and the file is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_run_adopts_the_node_elsewhere_and_the_second_keeps_it() {
    let (here, elsewhere) = two_daemons().await;
    let remote_node = propagation_node(&elsewhere).await;
    let remote = remote_node.hash;
    wait_for_path(&here, &remote, 1).await;
    // Newer than the remote one, by the path table's own clock.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let local_node = propagation_node(&here).await;
    let local = local_node.hash;
    wait_for_path(&here, &local, 0).await;

    let home_dir = home();
    let home = home_dir.path();
    let first = fetch(&here, home, &[]).await;
    let adopted = format!("lnmsg: mailbox: {} (1 hop, seen ", &hex(&remote)[..8]);
    assert!(
        first.stderr.contains(&adopted)
            && first
                .stderr
                .contains("adopted: no mailbox was selected; it is the most recently heard node"),
        "the first run must adopt the node elsewhere and say so: {}",
        first.stderr
    );
    assert!(
        !first.stderr.contains(&hex(&local)[..8]),
        "the node on this device must not be adopted: {}",
        first.stderr
    );
    assert_eq!(
        mailbox_file(home),
        Some(format!("{} adopted\n", hex(&remote)))
    );

    let second = fetch(&here, home, &[]).await;
    assert!(
        second
            .stderr
            .contains(&format!("lnmsg: mailbox: {} kept", &hex(&remote)[..8])),
        "the second run must keep the stored node: {}",
        second.stderr
    );
    assert_eq!(
        mailbox_file(home),
        Some(format!("{} adopted\n", hex(&remote)))
    );
    drop((remote_node, local_node, here.node, elsewhere.node));
}

/// `--pn` is the user's choice and is recorded as one, even for the node on
/// this device, which the rule would never adopt; and a later run without
/// `--pn` keeps it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_pn_is_recorded_as_the_users_choice_and_kept() {
    let (here, _elsewhere) = two_daemons().await;
    let local_node = propagation_node(&here).await;
    let local = local_node.hash;
    wait_for_path(&here, &local, 0).await;

    let home_dir = home();
    let home = home_dir.path();
    let chosen = fetch(&here, home, &["--pn", &hex(&local)]).await;
    assert_eq!(
        mailbox_file(home),
        Some(format!("{} user\n", hex(&local))),
        "stderr: {}",
        chosen.stderr
    );

    let later = fetch(&here, home, &[]).await;
    assert!(
        later
            .stderr
            .contains(&format!("lnmsg: mailbox: {} kept", &hex(&local)[..8])),
        "the user's own-device choice must be kept: {}",
        later.stderr
    );
}

/// A node on this device alone is no mailbox: the run says why, fails like
/// any run without a node, and records nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_on_this_device_alone_is_not_adopted() {
    let (here, _elsewhere) = two_daemons().await;
    let local_node = propagation_node(&here).await;
    let local = local_node.hash;
    wait_for_path(&here, &local, 0).await;

    let home_dir = home();
    let home = home_dir.path();
    let run = fetch(&here, home, &[]).await;
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains(ONLY_ON_THIS_DEVICE),
        "the reason must be the own-device one: {}",
        run.stderr
    );
    assert!(
        !run.stderr.contains("mailbox: "),
        "nothing may be adopted or kept: {}",
        run.stderr
    );
    assert_eq!(mailbox_file(home), None);
}

/// A path table that cannot be read adopts nothing: lnmsg cannot tell a node
/// on this device from one elsewhere without it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_path_table_nothing_is_adopted() {
    let (here, _elsewhere) = two_daemons().await;
    let local_node = propagation_node(&here).await;
    let local = local_node.hash;
    wait_for_path(&here, &local, 0).await;

    let home_dir = home();
    let home = home_dir.path();
    let mut args = vec![
        "fetch".to_string(),
        "--instance".into(),
        here.instance.clone(),
        // A config directory with no identity in it: no authkey, no RPC.
        "--config".into(),
        home.display().to_string(),
        "--timeout".into(),
        "4".into(),
    ];
    let home_for_child = home.to_path_buf();
    let run = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(args.drain(..))
            .env("LNMSG_HOME", &home_for_child)
            .env_remove("LEVICULUM_EVENT_LOG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = leviculum_std::process::spawn_supervised(command).expect("spawn lnmsg");
        let _ = child.stdin.take().map(|mut stdin| stdin.flush());
        child.wait_with_output().expect("wait for lnmsg")
    })
    .await
    .expect("the lnmsg child thread");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert_eq!(run.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("the daemon's path table could not be read"),
        "{stderr}"
    );
    assert_eq!(mailbox_file(home), None);
}
