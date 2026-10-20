//! mvr for Codeberg #461: lnsd exits when a TCPClient hostname does not
//! resolve.
//!
//! The reference resolves inside its connect attempt
//! (`reference/Reticulum/RNS/Interfaces/TCPInterface.py:230-250`,
//! `socket.getaddrinfo` in `connect`), so a name that does not resolve is
//! one failed attempt, logged, and the reconnect thread tries again; a node
//! with a dead WAN name still comes up and serves its other interfaces. Ours
//! resolved once at build time (`driver/interface_build/tcp.rs`) and the
//! `Error::Config` ended `Driver::start`, so lnsd logged the lookup failure
//! and quit. A laptop that boots before its network is up lost its daemon.
//!
//! Three tests:
//!
//! - a spawned release lnsd with a reserved `.invalid` target (RFC 2606, it
//!   never resolves) must still be running after the first attempt, and its
//!   event log must carry that attempt with the resolver's error;
//! - an in-process node whose resolver always fails must come up with the
//!   interface present and down, and log one line per attempt;
//! - an in-process node whose resolver fails once and then answers with a
//!   refusing address before the listening one must come up on the second
//!   attempt, having tried every address the name gave.
//!
//! The in-process tests take the resolver seam
//! ([`ReticulumNodeBuilder::host_resolver`]) instead of the host's DNS so the
//! attempt count is deterministic; the spawned test uses the real resolver,
//! because the daemon's exit was the symptom.
//!
//! **Acceptance**: the spawned test is red on master a044f1df (lnsd exits 1
//! with "configuration error: cannot resolve" and the name), all three green
//! once resolution moved into the reconnect loop.

use std::fs;
use std::io::{self, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use leviculum_core::traits::InterfaceKind;
use leviculum_std::config::{Config, InterfaceConfig};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::process::spawn_supervised;
use leviculum_std::resolver::HostResolver;

/// A name in the reserved `.invalid` TLD: no resolver ever answers it.
const UNRESOLVABLE: &str = "lnsd-mvr-461.invalid";

/// The port written beside [`UNRESOLVABLE`]. Nothing binds or dials it: the
/// name in front of it never yields an address.
const UNDIALLED_PORT: u16 = 4242;

/// How long the spawned daemon must survive. The first attempt runs at
/// start; before the fix the daemon was gone well inside this window.
const SURVIVE: Duration = Duration::from_secs(3);

/// What the test resolver says when it refuses, so the event log can be
/// checked for the resolver's own words.
const RESOLVER_ERROR: &str = "mvr461 resolver refuses";

/// Same path arithmetic as the other mvr files that drive a real daemon
/// (see `unparsable_config_must_not_start_a_daemon`).
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
    crate::assert_release_bin_fresh(&bin);
    bin
}

/// The `TCP_CONNECT_FAILED` lines of an event log, in order.
fn connect_failures(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| l.starts_with("TCP_CONNECT_FAILED "))
        .cloned()
        .collect()
}

/// The value of `key=` in one event line.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(key)?.strip_prefix('='))
}

#[test]
fn lnsd_stays_up_when_a_tcp_client_target_does_not_resolve() {
    let lnsd = release_bin("lnsd");
    assert!(
        lnsd.exists(),
        "{} not found - run `just build-integ-bins` first",
        lnsd.display()
    );

    let dir = tempfile::Builder::new()
        .prefix("mvr_tcp_unresolvable_")
        .tempdir()
        .expect("temp config dir");
    let config = format!(
        "[reticulum]\n  enable_transport = False\n  share_instance = No\n\n\
         [interfaces]\n  [[WAN]]\n    type = TCPClientInterface\n    enabled = yes\n    \
         target_host = {UNRESOLVABLE}\n    target_port = {UNDIALLED_PORT}\n"
    );
    fs::write(dir.path().join("config"), config).expect("write config");
    let event_log = dir.path().join("events.log");

    let mut cmd = Command::new(&lnsd);
    cmd.arg("--config")
        .arg(dir.path())
        .env("LEVICULUM_EVENT_LOG", &event_log)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_supervised(cmd).expect("spawn lnsd");

    let deadline = Instant::now() + SURVIVE;
    let exited = loop {
        match child.try_wait().expect("poll lnsd") {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => break None,
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // Kill before reading: a live daemon holds both pipes open.
    if exited.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let mut output = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut output);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut output);
    }

    if let Some(status) = exited {
        panic!(
            "lnsd exited ({status}) within {SURVIVE:?} because a TCPClient name did not \
             resolve; rnsd keeps running and retries. Output:\n{output}"
        );
    }
    assert!(
        output.contains("Reticulum daemon running"),
        "lnsd never reported itself running. Output:\n{output}"
    );

    let events: Vec<String> = fs::read_to_string(&event_log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let failures = connect_failures(&events);
    let first = failures.first().unwrap_or_else(|| {
        panic!("no TCP_CONNECT_FAILED line for the first attempt. Events:\n{events:#?}")
    });
    assert_eq!(field(first, "attempt"), Some("1"), "{first}");
    assert_eq!(field(first, "stage"), Some("resolve"), "{first}");
    assert!(
        field(first, "target").is_some_and(|t| t.contains(UNRESOLVABLE)),
        "the line must name the target it could not resolve: {first}"
    );
    assert!(
        field(first, "error").is_some_and(|e| !e.is_empty()),
        "the line must carry the resolver's error: {first}"
    );
}

/// A resolver that refuses the first `fail_first` lookups and then answers
/// with `answer`. Counts every call.
fn scripted_resolver(
    fail_first: usize,
    answer: Vec<SocketAddr>,
) -> (HostResolver, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let resolver: HostResolver = Arc::new(move |_host: &str, _port: u16| {
        let n = seen.fetch_add(1, Ordering::SeqCst);
        if n < fail_first {
            Err(io::Error::new(io::ErrorKind::NotFound, RESOLVER_ERROR))
        } else {
            Ok(answer.clone())
        }
    });
    (resolver, calls)
}

fn tcp_client_config(host: &str, port: u16) -> Config {
    let mut config = Config::default();
    config.interfaces.insert(
        "WAN".to_string(),
        InterfaceConfig {
            interface_type: "TCPClientInterface".to_string(),
            target_host: Some(host.to_string()),
            target_port: Some(port),
            // The shortest interval the config can say: attempts 1..=3 run at
            // the base rate, so three attempts fit in about two seconds.
            reconnect_interval_secs: Some(1),
            ..Default::default()
        },
    );
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_client_with_an_unresolvable_target_is_present_and_down() {
    let evlog = leviculum_std::test_support::event_log::init_event_log();
    let (resolver, calls) = scripted_resolver(usize::MAX, Vec::new());
    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .config(tcp_client_config(UNRESOLVABLE, UNDIALLED_PORT))
        .storage_path(storage.path().to_path_buf())
        .host_resolver(resolver)
        .build()
        .await
        .expect("a TCPClient name that does not resolve is not a config error");
    node.start()
        .await
        .expect("the node must start with a TCPClient name that does not resolve");

    // Three attempts: at start, then two base-interval waits (1 s +/-20 %).
    let deadline = tokio::time::Instant::now() + Duration::from_millis(3500);
    while calls.load(Ordering::SeqCst) < 3 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let stats = node.interface_stats();
    let tcp: Vec<_> = stats
        .iter()
        .filter(|s| s.kind == InterfaceKind::Tcp)
        .collect();
    assert_eq!(
        tcp.len(),
        1,
        "the TCPClient interface must exist: {stats:#?}"
    );
    assert!(
        !tcp[0].online,
        "a target that never resolved is down: {:?}",
        tcp[0]
    );

    let failures = connect_failures(&evlog.dump());
    node.stop().await.expect("stop node");

    assert!(
        calls.load(Ordering::SeqCst) >= 3,
        "the name must be looked up again on every attempt"
    );
    for (i, line) in failures.iter().take(3).enumerate() {
        let attempt = (i + 1).to_string();
        assert_eq!(field(line, "attempt"), Some(attempt.as_str()), "{line}");
        assert_eq!(field(line, "stage"), Some("resolve"), "{line}");
        assert!(
            line.contains(&RESOLVER_ERROR.replace(' ', "_")),
            "each attempt's line must carry the resolver's error: {line}"
        );
    }
    assert!(
        failures.len() >= 3,
        "one TCP_CONNECT_FAILED line per attempt, got {failures:#?}"
    );
    leviculum_std::assert_no_schema_violations!(evlog);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_client_comes_up_when_the_name_resolves_on_the_second_attempt() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let listening = listener.local_addr().expect("listener addr");
    // A port nothing listens on: bind, read the port, close. The answer puts
    // it first, so the interface only comes up if it tries the next address.
    let refusing = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("draw a closed port");
    let (resolver, calls) = scripted_resolver(1, vec![refusing, listening]);

    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .config(tcp_client_config("peer.mvr461.invalid", listening.port()))
        .storage_path(storage.path().to_path_buf())
        .host_resolver(resolver)
        .build()
        .await
        .expect("build node");
    node.start().await.expect("start node");

    let accepted = tokio::time::timeout(Duration::from_secs(3), listener.accept()).await;
    let online = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            let up = node
                .interface_stats()
                .iter()
                .any(|s| s.kind == InterfaceKind::Tcp && s.online);
            if up || tokio::time::Instant::now() >= deadline {
                break up;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    node.stop().await.expect("stop node");

    assert!(
        matches!(accepted, Ok(Ok(_))),
        "the interface must connect once the name resolves (resolver calls: {})",
        calls.load(Ordering::SeqCst)
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "one refused lookup, then one that answered"
    );
    assert!(online, "the interface must report online after connecting");
}
