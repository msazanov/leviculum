//! mvr: a mailbox sync whose link request is never proven ends the round
//! when the link dies, even when the client knows a second propagation node.
//!
//! ## The failure
//!
//! `emulated/lxmf_pn_chain_sync_lora_lost_response_leviculum`, 2026-10-14
//! (733, leviculum 419bdb8a). The recipient's `lxmf_sync` (step 31, 90 s,
//! one client retry) lost all three link requests to node `c` in collisions:
//!
//! ```text
//! 17:33:42.678 LINK_REQUEST_TX   link=2531691e dest=98e16adb attempt=1
//! 17:33:42.678 PropagationSyncState(LinkEstablishing)
//! 17:34:24.405 LINK_DIED link=1e907424 detail=handshake_timeout attempts=3
//! (no further PropagationSyncState)
//! 17:35:11     EVENT lxmf_error detail=sync_timeout
//! ```
//!
//! The round never reached a terminal state, so the step's one retry, which
//! keys on `sync_failed_state_0x..`, was never taken.
//!
//! ## The mechanism
//!
//! The runtime's `LinkClosed` arm does fail a round in `LinkEstablishing`
//! with `LinkFailed` — an in-process client with ONE known node reports
//! 0xf1 on exactly this loss. What the cell had and that client did not is
//! a second propagation node (`a` and `b` announce in the same mesh), and
//! three facts line up in one driver tick:
//!
//! 1. The handshake timeout expires the node's path on a non-transport
//!    client (`emit_link_closed`, `leviculum-core/src/node/link_management.rs`,
//!    `Transport.py:521-544`) and pushes `LinkClosed`.
//! 2. The driver runs the processor's `on_tick` under the same lock as
//!    `handle_timeout`, BEFORE it hands that tick's events to the tap
//!    (`leviculum-std/src/driver/mod.rs`, timer branch).
//! 3. `tick_outbound` sees no route to the selected node and moves to the
//!    best reachable alternative, replacing the client state with a fresh
//!    `Idle` one and emitting nothing.
//!
//! When `LinkClosed` reaches the runtime a moment later it names a node that
//! is no longer the selected one, and is ignored. The round is gone without
//! a word, and lxmf-node waits out the step's budget.
//!
//! The reference has no such switch: `outbound_propagation_node` is only
//! ever set by its caller (`LXMRouter.py:412`), and a closed link below
//! `PR_LINK_ESTABLISHED` is `PR_LINK_FAILED` (`LXMRouter.py:991-996`).
//!
//! ## Topology
//!
//! ```text
//!   pn node (in process)     announces lxmf.propagation `c` (drops link
//!        │                   requests) and `a` (a reachable alternative)
//!        │  \0rns/<name>
//!   release lnsd (share_instance)
//!        │  \0rns/<name>
//!   release lxmf-node        set_pn c; sync
//! ```
//!
//! No hardware, no Docker, no Python. It runs as long as the client's three
//! link requests take to time out (about 40 s), because that wait is the
//! event under test.
//!
//! **Acceptance**: red on 419bdb8a..46c61ca7 with `sync_timeout` after the
//! whole budget; green once the round reports `sync_failed_state_0xf1`
//! within one handshake timeout of `LINK_DIED`.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use leviculum_core::identity::Identity;
use leviculum_core::{Destination, DestinationHash, DestinationType, Direction};
use leviculum_lxmf::propagation::PropagationNodeAnnounce;
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use leviculum_std::process::spawn_supervised;
use rand_core::OsRng;

/// Unique per test in this binary, so two mvrs never share a socket name.
static INSTANCE_SEQ: AtomicUsize = AtomicUsize::new(0);

/// The sync budget handed to lxmf-node. Longer than the three handshake
/// timeouts the round spends (3 x ~13.6 s) plus the failure report, so a red
/// run is `sync_timeout` at the end of it rather than an early verdict.
const SYNC_BUDGET_SECS: u64 = 75;

/// Resolve a release binary the way `lncp_fetch_rust_responder` does: the
/// `just mvr` recipe builds them (`build-integ-bins`), `cargo test` does not.
fn release_bin(name: &str) -> PathBuf {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"));
    let triple = std::env::var("CARGO_BUILD_TARGET")
        .unwrap_or_else(|_| "x86_64-unknown-linux-musl".to_string());
    let triple_release = target_dir.join(&triple).join("release").join(name);
    let bin = if triple_release.exists() {
        triple_release
    } else {
        target_dir.join("release").join(name)
    };
    assert!(
        bin.exists(),
        "{} not found - run `just build-integ-bins` first",
        bin.display()
    );
    crate::assert_release_bin_fresh(&bin);
    bin
}

/// Kills its child on drop, so a failing assertion leaves no daemon behind.
struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Whether anything listens on the instance's abstract socket yet.
fn socket_is_bound(name: &str) -> bool {
    use std::os::linux::net::SocketAddrExt;
    let Ok(addr) =
        std::os::unix::net::SocketAddr::from_abstract_name(format!("rns/{name}").as_bytes())
    else {
        return false;
    };
    std::os::unix::net::UnixStream::connect_addr(&addr).is_ok()
}

fn spawn_lnsd(dir: &Path, name: &str) -> Reaped {
    fs::create_dir_all(dir.join("storage")).expect("lnsd storage dir");
    fs::write(
        dir.join("config"),
        format!(
            "[reticulum]\n  enable_transport = yes\n  share_instance = yes\n  \
             instance_name = {name}\n\n[logging]\n  loglevel = 5\n\n[interfaces]\n"
        ),
    )
    .expect("lnsd config");
    let mut cmd = Command::new(release_bin("lnsd"));
    cmd.arg("-v")
        .arg("--config")
        .arg(dir)
        .stdout(Stdio::from(
            fs::File::create(dir.join("lnsd-stdout.log")).expect("stdout log"),
        ))
        .stderr(Stdio::from(
            fs::File::create(dir.join("lnsd-stderr.log")).expect("stderr log"),
        ));
    Reaped(spawn_supervised(cmd).expect("spawn lnsd"))
}

/// The release lxmf-node, attached to `config_dir`'s shared instance, with
/// its stdout lines arriving on a channel and its stderr in a file.
struct LxmfNode {
    _child: Reaped,
    stdin: ChildStdin,
    lines: Receiver<String>,
    seen: Vec<String>,
    stderr_path: PathBuf,
}

impl LxmfNode {
    fn spawn(config_dir: &Path, storage: &Path) -> LxmfNode {
        let stderr_path = config_dir.join("lxmf-node-stderr.log");
        let mut cmd = Command::new(release_bin("lxmf-node"));
        cmd.arg("--config")
            .arg(config_dir)
            .arg("mvr-recipient")
            .env("LXMF_STORAGE", storage)
            // LINK_DIED and the router's state lines are debug-level.
            .env("RUST_LOG", "debug")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(
                fs::File::create(&stderr_path).expect("lxmf-node stderr log"),
            ));
        let mut child = spawn_supervised(cmd).expect("spawn lxmf-node");
        let stdin = child.stdin.take().expect("lxmf-node stdin");
        let stdout = child.stdout.take().expect("lxmf-node stdout");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        LxmfNode {
            _child: Reaped(child),
            stdin,
            lines,
            seen: Vec::new(),
            stderr_path,
        }
    }

    fn command(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("lxmf-node stdin write");
        self.stdin.flush().expect("lxmf-node stdin flush");
    }

    /// The first stdout line from `from` on that starts with `prefix`,
    /// waiting up to `limit` for it.
    fn wait_line(&mut self, from: usize, prefix: &str, limit: Duration) -> Option<String> {
        let end = Instant::now() + limit;
        loop {
            if let Some(line) = self.seen[from..].iter().find(|l| l.starts_with(prefix)) {
                return Some(line.clone());
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            match self.lines.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(line) => self.seen.push(line),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    fn stderr(&self) -> String {
        fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }

    /// The structured lines a failure is diagnosed from.
    fn timeline(&self) -> String {
        let stderr = self.stderr();
        let protocol = stderr
            .lines()
            .filter(|l| {
                l.contains("LINK_REQUEST_")
                    || l.contains("LINK_DIED")
                    || l.contains("PropagationSyncState")
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "stdout:\n{}\nstderr (link/sync lines):\n{protocol}",
            self.seen.join("\n")
        )
    }
}

fn propagation_destination(accepts_links: bool) -> (Destination, DestinationHash) {
    let mut dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["propagation"],
    )
    .expect("propagation destination");
    dest.set_accepts_links(accepts_links);
    let hash = *dest.hash();
    (dest, hash)
}

fn propagation_app_data() -> Vec<u8> {
    PropagationNodeAnnounce {
        legacy_support: false,
        timebase: 1_700_000_000,
        enabled: true,
        transfer_limit_kb: 256,
        sync_limit_kb: 10_240,
        stamp_cost: 0,
        stamp_cost_flexibility: 3,
        peering_cost: 18,
        metadata: Vec::new(),
    }
    .encode()
    .expect("propagation announce app data")
}

async fn announce_both(pn: &ReticulumNode, dead: &DestinationHash, alive: &DestinationHash) {
    let app_data = propagation_app_data();
    for hash in [alive, dead] {
        pn.announce_destination(hash, Some(&app_data))
            .await
            .expect("announce propagation node");
    }
}

/// THE pin: the round ends with the reference's `PR_LINK_FAILED` within one
/// handshake timeout of the link's death, with a second node known.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dead_sync_link_ends_the_round_beside_a_second_node() {
    let name = format!(
        "mvr_pn_dead_link_{}_{}",
        std::process::id(),
        INSTANCE_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let scratch = tempfile::tempdir().expect("tempdir");
    let config_dir = scratch.path().join("lnsd");
    let _lnsd = spawn_lnsd(&config_dir, &name);
    let up = Instant::now() + Duration::from_secs(10);
    while !socket_is_bound(&name) {
        assert!(Instant::now() < up, "lnsd never bound \\0rns/{name}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Two propagation nodes on one client: `c` ignores every link request
    // (the three collisions of the cell), `a` is the reachable alternative
    // that `tick_outbound` moves to.
    let pn_storage = scratch.path().join("pn");
    let mut pn = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(name.clone())
        .storage_path(pn_storage)
        .build()
        .await
        .expect("build pn node");
    pn.start().await.expect("start pn node");
    let (dead, dead_hash) = propagation_destination(false);
    let (alive, alive_hash) = propagation_destination(true);
    pn.register_destination(dead);
    pn.register_destination(alive);

    let lxmf_storage = scratch.path().join("lxmf-state");
    let mut client = LxmfNode::spawn(&config_dir, &lxmf_storage);
    assert!(
        client
            .wait_line(0, "EVENT lxmf_ready", Duration::from_secs(20))
            .is_some(),
        "lxmf-node never became ready:\n{}",
        client.timeline()
    );

    // Select `c` once the client has heard both announces: `set_pn` fails
    // until the node's identity is known, and the alternative has to be known
    // before the round starts for the switch to be possible at all.
    let dead_hex = hex::encode(dead_hash.as_bytes());
    let alive_hex = hex::encode(alive_hash.as_bytes());
    let selected_by = Instant::now() + Duration::from_secs(30);
    loop {
        announce_both(&pn, &dead_hash, &alive_hash).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let stderr = client.stderr();
        let both_known = [&dead_hex, &alive_hex].iter().all(|hex| {
            stderr
                .lines()
                .any(|l| l.contains("PATH_ADD") && l.contains(hex.as_str()))
        });
        if both_known {
            let from = client.seen.len();
            client.command(&format!("set_pn {dead_hex}"));
            if client
                .wait_line(from, "EVENT lxmf_pn_selected", Duration::from_secs(2))
                .is_some()
            {
                break;
            }
        }
        assert!(
            Instant::now() < selected_by,
            "the client never selected `c` with `a` known:\n{}",
            client.timeline()
        );
    }

    // Wait for the verdict, noting when LINK_DIED reaches the stderr file:
    // both ends of the interval the acceptance bounds are then read on this
    // clock, polled every 50 ms.
    let from = client.seen.len();
    client.command(&format!("sync {SYNC_BUDGET_SECS}"));
    let budget_end = Instant::now() + Duration::from_secs(SYNC_BUDGET_SECS + 10);
    let mut died: Option<(Instant, String)> = None;
    let verdict = loop {
        if died.is_none() {
            died = client
                .stderr()
                .lines()
                .find(|l| l.contains("LINK_DIED"))
                .map(|l| (Instant::now(), l.to_string()));
        }
        if let Some(line) = client.wait_line(from, "EVENT lxmf_", Duration::from_millis(50)) {
            break line;
        }
        assert!(
            Instant::now() < budget_end,
            "lxmf-node said nothing within the budget:\n{}",
            client.timeline()
        );
    };
    let reported_at = Instant::now();

    // Not seen before the verdict means both reached their files inside the
    // same 50 ms poll; the lag is then bounded by that poll, not zero.
    let seen_before = died.is_some();
    let Some((died_at, died_line)) = died.or_else(|| {
        client
            .stderr()
            .lines()
            .find(|l| l.contains("LINK_DIED"))
            .map(|l| (reported_at, l.to_string()))
    }) else {
        panic!(
            "the sync link never died, so this run did not reproduce the loss \
             (verdict `{verdict}`):\n{}",
            client.timeline()
        );
    };
    assert!(
        verdict.contains("detail=sync_failed_state_0xf1"),
        "the round must end as PR_LINK_FAILED (0xf1) when its link dies, got `{verdict}`:\n{}",
        client.timeline()
    );
    // The threshold of the attempt that died: the report owes nothing more
    // than one more handshake timeout after it.
    let threshold_ms = died_line
        .split_whitespace()
        .find_map(|t| t.strip_prefix("threshold_ms="))
        .and_then(|v| v.parse::<u64>().ok())
        .expect("LINK_DIED carries threshold_ms");
    let lag = reported_at.duration_since(died_at);
    assert!(
        lag <= Duration::from_millis(threshold_ms),
        "the failure came {lag:?} after LINK_DIED, past one handshake timeout \
         ({threshold_ms} ms):\n{}",
        client.timeline()
    );
    if seen_before {
        eprintln!("verdict `{verdict}` {lag:?} after `{died_line}`");
    } else {
        eprintln!("verdict `{verdict}` inside the same 50 ms poll as `{died_line}`");
    }

    drop(client);
    pn.stop().await.ok();
}
