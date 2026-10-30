//! A stranger's 16-byte write must not make it a mesh peer (996).
//!
//! ## The failure this reproduces
//!
//! On the bench (tier-3 smoke, 2026-10-25) a foreign central connected
//! to the host's GATT service and wrote 16 bytes that are no identity —
//! `00 02 03 00 00 2a 04 00 02 05 00 01 2a 06 00 02`, a pattern of
//! attribute handles and 16-bit GATT UUIDs (0x2a00 Device Name, 0x2a01
//! Appearance), the trace of a prober dumping discovery results into a
//! writable characteristic. The host took the bytes as the peer's
//! identity (`interfaces/ble/links.rs::peripheral_frame`: the first
//! 16-byte write from an unknown address IS the handshake, length is the
//! only check), emitted `BLE_LINK_UP` and `BLE_PEER_UP`, and re-offered
//! stored announces to the stranger.
//!
//! The handshake bytes themselves are opaque — a truncated identity
//! hash, unverifiable on receipt, and the reference accepts any 16 bytes
//! just as we did (`ble-reticulum@07d94130` `BLEInterface.py`,
//! `_handle_identity_handshake`; Columba `BleGattServer.kt`,
//! `acceptsRxWrite`). What distinguishes a Columba central is the
//! exchange around them: every reference central subscribes to the TX
//! characteristic BEFORE writing its identity (BLE_PROTOCOL_v2.2
//! §Handshake Flow steps 4 and 5; our own dial, `bluez.rs::
//! layout_and_handshake`; Columba `BleGattClient.kt`; the reference's
//! `linux_bluetooth_driver.py`), and aborts the connection when the
//! subscription fails. A 16-byte first write on a connection whose
//! central never subscribed is therefore not a Columba identity
//! exchange, and the fix refuses it: `BLE_LINK_REFUSED addr=…
//! reason=identity_invalid`, no link, no `BLE_PEER_UP`, the central
//! disconnected.
//!
//! ## What this test does
//!
//! One btvirt room, one release `lnsd` on controller 0. This test
//! process is the central on controller 1, via bluer:
//!
//! 1. Plain GATT discovery and nothing else — no link may come up.
//! 2. The stranger: write the bench's exact 16 bytes to RX without
//!    subscribing. RED before the fix (`BLE_PEER_UP` for the junk
//!    bytes), GREEN after (`BLE_LINK_REFUSED reason=identity_invalid`,
//!    no `BLE_PEER_UP`, the central disconnected within [`REFUSE_WAIT`]).
//! 3. Positive control, same address: the Columba sequence — read the
//!    identity characteristic, subscribe to TX, then write a 16-byte
//!    identity. The link must come up (`BLE_LINK_UP`, `BLE_PEER_UP`),
//!    which also pins the subscribe-then-handshake ordering through the
//!    real BlueZ path the fix relies on.
//!
//! ## Running it
//!
//! Needs the patched `btvirt`, `/dev/vhci`, the system `bluetoothd`, and
//! a fresh release `lnsd` — host-global Bluetooth state, the `ble_room`
//! lock class — so it is `#[ignore]`d and run by name:
//!
//! ```text
//! cargo build --release --bin lnsd
//! cargo test -p leviculum-std --test ble_stranger_identity -- --ignored --nocapture
//! ```

#![cfg(all(target_os = "linux", feature = "ble"))]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures::StreamExt;
use leviculum_std::process::spawn_supervised;

const BTVIRT: &str = "/usr/local/bin/btvirt";

/// The Columba GATT layout (BLE_PROTOCOL_v2.2 §GATT Service Structure;
/// `interfaces/ble/bluez.rs` serves it). Spelled here rather than
/// imported: this test plays a foreign central and must find the service
/// the way one does, by its advertised UUIDs.
const SERVICE_UUID: bluer::Uuid = bluer::Uuid::from_u128(0x37145b00_442d_4a94_917f_8f42c5da28e3);
const TX_UUID: bluer::Uuid = bluer::Uuid::from_u128(0x37145b00_442d_4a94_917f_8f42c5da28e4);
const RX_UUID: bluer::Uuid = bluer::Uuid::from_u128(0x37145b00_442d_4a94_917f_8f42c5da28e5);
const IDENTITY_UUID: bluer::Uuid = bluer::Uuid::from_u128(0x37145b00_442d_4a94_917f_8f42c5da28e6);

/// The exact 16 bytes the bench stranger wrote (996's authority, the
/// tier-3 smoke of 2026-10-25): GATT discovery debris, not an identity.
const STRANGER_BYTES: [u8; 16] = [
    0x00, 0x02, 0x03, 0x00, 0x00, 0x2a, 0x04, 0x00, 0x02, 0x05, 0x00, 0x01, 0x2a, 0x06, 0x00, 0x02,
];

/// The positive control's identity — any 16 bytes are a valid identity
/// hash on the wire; what makes this exchange Columba is the
/// subscription before it.
const CONTROL_IDENTITY: [u8; 16] = [0x5a; 16];

/// How long the daemon gets to come up and advertise, and the scan gets
/// to sight it (the resight test's budget for a fresh room).
const SIGHT_WAIT: Duration = Duration::from_secs(60);

/// How long a refusal gets to land in the log and the disconnect to
/// reach the central. The refusal is written in the same event-loop turn
/// as the write's arrival; 15 s is BlueZ propagation slack.
const REFUSE_WAIT: Duration = Duration::from_secs(15);

/// How long the positive control's handshake gets to become a link.
const UP_WAIT: Duration = Duration::from_secs(30);

/// The release `lnsd`, resolved the way the mvrs resolve it, and refused
/// when it is older than the BLE interface under test (same rule and
/// reason as `ble_link_gap_floor`).
fn release_lnsd() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let triple = std::env::var("CARGO_BUILD_TARGET")
        .unwrap_or_else(|_| "x86_64-unknown-linux-musl".to_string());
    let with_triple = target.join(triple).join("release").join("lnsd");
    let bin = if with_triple.exists() {
        with_triple
    } else {
        target.join("release").join("lnsd")
    };
    let built = std::fs::metadata(&bin)
        .and_then(|m| m.modified())
        .unwrap_or_else(|e| panic!("{}: {e}; cargo build --release --bin lnsd", bin.display()));
    let ble = root.join("leviculum-std/src/interfaces/ble");
    for entry in std::fs::read_dir(&ble).expect("ble sources") {
        let entry = entry.expect("entry");
        let touched = entry.metadata().and_then(|m| m.modified()).expect("mtime");
        assert!(
            built >= touched,
            "{} is older than {}; cargo build --release --bin lnsd",
            bin.display(),
            entry.path().display()
        );
    }
    bin
}

fn adapters() -> BTreeSet<String> {
    std::fs::read_dir("/sys/class/bluetooth")
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("hci") && !n.contains(':'))
                .collect()
        })
        .unwrap_or_default()
}

fn bluez_has(adapter: &str) -> bool {
    Command::new("busctl")
        .args(["introspect", "org.bluez", &format!("/org/bluez/{adapter}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Forget every device BlueZ cached on `adapter`: btvirt hands out the
/// same addresses every time, and a stale device object would let this
/// room inherit a previous room's GATT cache.
fn forget_devices(adapter: &str) {
    let Ok(out) = Command::new("busctl")
        .args(["--list", "tree", "org.bluez"])
        .output()
    else {
        return;
    };
    let prefix = format!("/org/bluez/{adapter}/dev_");
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let path = line.trim();
        if path.starts_with(&prefix) && path[prefix.len()..].len() == 17 {
            let _ = Command::new("busctl")
                .args([
                    "call",
                    "org.bluez",
                    &format!("/org/bluez/{adapter}"),
                    "org.bluez.Adapter1",
                    "RemoveDevice",
                    "o",
                    path,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// btvirt and the host daemon; killed in reverse on drop.
struct Room {
    btvirt: Child,
    daemon: Option<Child>,
    adapters: Vec<String>,
    dir: tempfile::TempDir,
}

impl Drop for Room {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.as_mut() {
            let _ = d.kill();
            let _ = d.wait();
        }
        for adapter in &self.adapters {
            forget_devices(adapter);
        }
        let _ = self.btvirt.kill();
        let _ = self.btvirt.wait();
        // `STRANGER_KEEP=<dir>` copies the room's log out before the
        // tempdir goes, green and red alike.
        if let Ok(keep) = std::env::var("STRANGER_KEEP") {
            let _ = std::fs::create_dir_all(&keep);
            let _ = std::fs::copy(
                self.dir.path().join("host.log"),
                Path::new(&keep).join("host.log"),
            );
        }
    }
}

impl Room {
    /// The host daemon's log so far, ANSI colour codes stripped.
    fn log(&self) -> String {
        let raw = std::fs::read_to_string(self.dir.path().join("host.log")).unwrap_or_default();
        let mut out = String::with_capacity(raw.len());
        let mut chars = raw.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// Wait until the host log contains a line matching `pred`, or panic
    /// with the log after `wait`.
    fn wait_for_line(&self, wait: Duration, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + wait;
        loop {
            let log = self.log();
            if log.lines().any(&pred) {
                return log;
            }
            assert!(
                Instant::now() < deadline,
                "host: no {what} within {wait:?}\n--- host ---\n{log}",
            );
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

fn open_room(lnsd: &Path) -> Room {
    assert!(
        !Command::new("pgrep")
            .args(["-x", "btvirt"])
            .output()
            .is_ok_and(|o| o.status.success()),
        "another btvirt is running; this test needs the controllers to itself"
    );
    let before = adapters();
    let dir = tempfile::tempdir().expect("room dir");
    let log = std::fs::File::create(dir.path().join("btvirt.log")).expect("btvirt log");
    let mut cmd = Command::new(BTVIRT);
    cmd.args(["-L", "-l2"])
        .stdin(Stdio::null())
        .stdout(log.try_clone().expect("clone"))
        .stderr(log);
    let btvirt = spawn_supervised(cmd).expect("spawn btvirt");
    let deadline = Instant::now() + Duration::from_secs(15);
    let fresh: Vec<String> = loop {
        let mut fresh: Vec<(u32, String)> = adapters()
            .difference(&before)
            .filter_map(|n| n[3..].parse().ok().map(|i| (i, n.clone())))
            .collect();
        fresh.sort();
        if fresh.len() >= 2 && fresh.iter().take(2).all(|(_, n)| bluez_has(n)) {
            break fresh.into_iter().take(2).map(|(_, n)| n).collect();
        }
        assert!(
            Instant::now() < deadline,
            "btvirt brought up {} of 2 controllers bluetoothd owns",
            fresh.len()
        );
        std::thread::sleep(Duration::from_millis(250));
    };
    let mut room = Room {
        btvirt,
        daemon: None,
        adapters: fresh.clone(),
        dir,
    };
    for adapter in &fresh {
        forget_devices(adapter);
    }
    let cdir = room.dir.path().join("c0");
    std::fs::create_dir_all(&cdir).expect("config dir");
    std::fs::write(
        cdir.join("config"),
        format!(
            "[reticulum]\n  enable_transport = no\n  share_instance = yes\n  \
             instance_name = stranger_{pid}\n\n[logging]\n  loglevel = 4\n\n\
             [interfaces]\n  [[BLE Room]]\n    type = BLEInterface\n    enabled = yes\n    \
             device = {adapter}\n",
            pid = std::process::id(),
            adapter = fresh[0],
        ),
    )
    .expect("config");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(room.dir.path().join("host.log"))
        .expect("log");
    let mut cmd = Command::new(lnsd);
    cmd.arg("--config")
        .arg(&cdir)
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .stdout(log.try_clone().expect("clone"))
        .stderr(log);
    room.daemon = Some(spawn_supervised(cmd).expect("spawn lnsd"));
    room
}

/// Sight the host from the test's controller and return its device. The
/// discovery stream is polled until the host's address appears; the
/// returned device object then persists for the connection.
async fn sight_host(central: &bluer::Adapter, host_addr: bluer::Address) -> bluer::Device {
    let mut events = central.discover_devices().await.expect("discover");
    let deadline = Instant::now() + SIGHT_WAIT;
    loop {
        let timeout = tokio::time::timeout(Duration::from_secs(1), events.next()).await;
        if let Ok(Some(bluer::AdapterEvent::DeviceAdded(addr))) = timeout {
            if addr == host_addr {
                return central.device(addr).expect("device");
            }
        }
        assert!(
            Instant::now() < deadline,
            "host {host_addr} not sighted within {SIGHT_WAIT:?}"
        );
    }
}

/// The Columba characteristics of the host's service, by discovery —
/// the "plain GATT discovery" a stranger performs.
async fn columba_chars(
    device: &bluer::Device,
) -> (
    bluer::gatt::remote::Characteristic,
    bluer::gatt::remote::Characteristic,
    bluer::gatt::remote::Characteristic,
) {
    let mut tx = None;
    let mut rx = None;
    let mut id = None;
    for service in device.services().await.expect("services") {
        if service.uuid().await.expect("uuid") != SERVICE_UUID {
            continue;
        }
        for c in service.characteristics().await.expect("chars") {
            match c.uuid().await.expect("uuid") {
                u if u == TX_UUID => tx = Some(c),
                u if u == RX_UUID => rx = Some(c),
                u if u == IDENTITY_UUID => id = Some(c),
                _ => {}
            }
        }
    }
    (
        tx.expect("TX characteristic"),
        rx.expect("RX characteristic"),
        id.expect("identity characteristic"),
    )
}

/// A 16-byte write to RX as the handshake is written: with response
/// (BLE_PROTOCOL_v2.2 §Handshake Packet Format, a Write Request).
async fn write_sixteen(rx: &bluer::gatt::remote::Characteristic, bytes: &[u8; 16]) {
    rx.write_ext(
        bytes,
        &bluer::gatt::remote::CharacteristicWriteRequest {
            op_type: bluer::gatt::WriteOp::Request,
            ..Default::default()
        },
    )
    .await
    .expect("RX write");
}

#[test]
#[ignore = "needs btvirt + bluetoothd + a fresh release lnsd (host-global Bluetooth state); run by name"]
fn a_strangers_sixteen_byte_write_is_refused_and_the_columba_sequence_is_not() {
    let lnsd = release_lnsd();
    let room = open_room(&lnsd);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let session = bluer::Session::new().await.expect("session");
        let host = session.adapter(&room.adapters[0]).expect("host adapter");
        let host_addr = host.address().await.expect("host address");
        let central = session.adapter(&room.adapters[1]).expect("central adapter");
        central.set_powered(true).await.expect("power");

        // The host must be advertising before a stranger can find it.
        let device = sight_host(&central, host_addr).await;
        device.connect().await.expect("connect");

        // 1. Plain GATT discovery and nothing else: no link, no peer.
        let (_tx_char, rx_char, _id_char) = columba_chars(&device).await;
        let log = room.log();
        assert!(
            !log.contains("BLE_LINK_UP") && !log.contains("BLE_PEER_UP"),
            "discovery alone made a link:\n{log}"
        );

        // 2. The stranger: the bench's exact bytes, no subscription.
        write_sixteen(&rx_char, &STRANGER_BYTES).await;
        let log = room.wait_for_line(REFUSE_WAIT, "BLE_LINK_REFUSED", |l| {
            l.contains("BLE_LINK_REFUSED") && l.contains("reason=\"identity_invalid\"")
        });
        assert!(
            !log.contains("BLE_PEER_UP"),
            "the stranger became a peer:\n{log}"
        );
        assert!(
            !log.contains("BLE_LINK_UP"),
            "the stranger got a link:\n{log}"
        );
        // The refusal disconnects the central.
        let deadline = Instant::now() + REFUSE_WAIT;
        while device.is_connected().await.unwrap_or(false) {
            assert!(
                Instant::now() < deadline,
                "still connected {REFUSE_WAIT:?} after the refusal"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // 3. Positive control, same address: the Columba sequence — read
        // the identity characteristic, subscribe to TX, then handshake.
        device.connect().await.expect("reconnect");
        let (tx_char2, rx_char2, id_char2) = columba_chars(&device).await;
        let host_identity = id_char2.read().await.expect("identity read");
        assert_eq!(host_identity.len(), 16, "host identity is 16 bytes");
        let notify = tx_char2.notify().await.expect("subscribe");
        write_sixteen(&rx_char2, &CONTROL_IDENTITY).await;
        // `peer=` is the identity's first four bytes, unquoted
        // (`links::hex8`, as the bench log prints it).
        let log = room.wait_for_line(UP_WAIT, "BLE_PEER_UP for the control identity", |l| {
            l.contains("BLE_PEER_UP") && l.contains("peer=5a5a5a5a")
        });
        assert!(
            log.lines()
                .any(|l| l.contains("BLE_LINK_UP") && l.contains("peer=5a5a5a5a")),
            "no BLE_LINK_UP for the control identity:\n{log}"
        );
        assert!(
            device.is_connected().await.unwrap_or(false),
            "the Columba-sequence central was disconnected"
        );
        drop(notify);
        let _ = device.disconnect().await;
    });
}
