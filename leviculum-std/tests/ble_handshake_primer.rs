//! The AcquireWrite-triggering write arrives rewritten; a primed dial
//! does not (1004).
//!
//! ## The failure this reproduces
//!
//! Both tier-3 smoke runs of 2026-10-25 skipped on a "foreign
//! participant" that was our own T114: the host logged its handshake as
//! 16 bytes of GATT debris (run 1: bytes 3..19 of the board's own GAP
//! declaration response; run 2: the board's identity with its first two
//! bytes replaced by the tail of an ATT Error Response). Mechanism,
//! verified against the bluez-5.82 source this host runs: BlueZ reads
//! every inbound ATT PDU into ONE per-connection buffer
//! (`src/shared/att.c`, `chan->buf`); the first write to a
//! characteristic served over AcquireWrite is held as a POINTER into
//! that buffer across the AcquireWrite D-Bus round trip
//! (`src/gatt-database.c`, `pending_write_new` keeps `iov_base`,
//! `acquire_write_reply` sends it) — so every PDU the peripheral
//! receives inside the round trip rewrites the bytes the application is
//! handed. On the bench the rewriting PDU was the host's own reverse
//! discovery of the dual-role board, answered by the board exactly while
//! its handshake write was pending.
//!
//! Only the triggering write is exposed: once `acquire_write_reply` has
//! added the socket to `write_ios`, `chrc_write_cb` forwards later
//! writes synchronously from the fresh PDU. The fix is therefore a
//! primer in the dial (bluez.rs `layout_and_handshake`, the firmware's
//! `run_central_session`): one keepalive byte written WITH response
//! before the identity. Its content may decay in the race — a one-byte
//! frame is a keepalive whatever its byte says, to lnsd, the firmware
//! and the reference — and its ATT response only returns after the
//! socket is flushed, so the identity that follows arrives byte-exact.
//!
//! ## What this test does
//!
//! No daemon: both ends are this test over bluer, which makes the race
//! window deterministic — the peripheral side delays its AcquireWrite
//! accept by [`ACCEPT_DELAY`], and the central injects write-commands
//! into the window.
//!
//! 1. The bench's order (what the T114 did): the 16-byte identity as
//!    the first write, hostile traffic in the window. The socket's
//!    first frame is 16 bytes that are NOT the identity — the bench
//!    failure, reproduced. (If this half ever fails with the frame
//!    EQUAL to the identity, this host's BlueZ has started copying
//!    pending writes and the primer can retire — see 1004.)
//! 2. The primed order on a fresh connection, same hostile window: a
//!    one-byte keepalive with response, THEN the identity. The identity
//!    frame arrives byte-exact — the guarantee both our dials now rely
//!    on.
//!
//! ## Running it
//!
//! Needs the patched `btvirt`, `/dev/vhci` and the system `bluetoothd`
//! — host-global Bluetooth state, the `ble_room` lock class — so it is
//! `#[ignore]`d and run by name:
//!
//! ```text
//! cargo test -p leviculum-std --test ble_handshake_primer -- --ignored --nocapture
//! ```

#![cfg(all(target_os = "linux", feature = "ble"))]

use std::collections::BTreeSet;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures::StreamExt;
use leviculum_std::process::spawn_supervised;

const BTVIRT: &str = "/usr/local/bin/btvirt";

/// The Columba GATT layout (BLE_PROTOCOL_v2.2 §GATT Service Structure),
/// spelled as a foreign room spells it; only the RX characteristic
/// matters to the socket mechanism under test.
const SERVICE_UUID: bluer::Uuid = bluer::Uuid::from_u128(0x37145b00_442d_4a94_917f_8f42c5da28e3);
const RX_UUID: bluer::Uuid = bluer::Uuid::from_u128(0x37145b00_442d_4a94_917f_8f42c5da28e5);

/// The hostile traffic's own characteristic (a room-local UUID, not a
/// Columba one): BlueZ serializes D-Bus writes PER characteristic
/// (`gatt-client.c`, `chrc->write_op`), so the commands racing into the
/// window must ride a second attribute. At the peripheral they land in
/// the same per-connection receive buffer as the pending handshake —
/// which is the whole point.
const CHAOS_UUID: bluer::Uuid = bluer::Uuid::from_u128(0x37145b00_442d_4a94_917f_8f42c5da28ff);

/// The identities the two halves write. Any 16 bytes are a valid
/// identity on the wire; distinct patterns keep the halves apart.
const IDENTITY_UNPRIMED: [u8; 16] = [0x1a; 16];
const IDENTITY_PRIMED: [u8; 16] = [0x2b; 16];

/// The hostile window: how long the peripheral holds the AcquireWrite
/// round trip open. On the bench the window is BlueZ's D-Bus round trip
/// (sub-millisecond against a prompt daemon); holding it open turns the
/// bench race into a certainty.
const ACCEPT_DELAY: Duration = Duration::from_secs(1);

/// What the central shoves into the window: write-commands whose PDU
/// (opcode + handle + value) covers the receive buffer past the pending
/// identity's 19 bytes, so the rewriting is total.
const HOSTILE_VALUE: [u8; 20] = [0xab; 20];

/// How long the fresh room gets to be sighted by the central.
const SIGHT_WAIT: Duration = Duration::from_secs(60);

/// How long collected frames get to cross the event channel.
const FRAME_WAIT: Duration = Duration::from_secs(15);

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

/// btvirt; killed on drop, with the room's devices forgotten.
struct Room {
    btvirt: Child,
    adapters: Vec<String>,
}

impl Drop for Room {
    fn drop(&mut self) {
        for adapter in &self.adapters {
            forget_devices(adapter);
        }
        let _ = self.btvirt.kill();
        let _ = self.btvirt.wait();
    }
}

fn open_room() -> Room {
    assert!(
        !Command::new("pgrep")
            .args(["-x", "btvirt"])
            .output()
            .is_ok_and(|o| o.status.success()),
        "another btvirt is running; this test needs the controllers to itself"
    );
    let before = adapters();
    let mut cmd = Command::new(BTVIRT);
    cmd.args(["-L", "-l2"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
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
    let room = Room {
        btvirt,
        adapters: fresh.clone(),
    };
    for adapter in &fresh {
        forget_devices(adapter);
    }
    room
}

/// Sight the peripheral from the central's controller.
async fn sight(central: &bluer::Adapter, addr: bluer::Address) -> bluer::Device {
    let mut events = central.discover_devices().await.expect("discover");
    let deadline = Instant::now() + SIGHT_WAIT;
    loop {
        let timeout = tokio::time::timeout(Duration::from_secs(1), events.next()).await;
        if let Ok(Some(bluer::AdapterEvent::DeviceAdded(got))) = timeout {
            if got == addr {
                return central.device(addr).expect("device");
            }
        }
        assert!(
            Instant::now() < deadline,
            "peripheral {addr} not sighted within {SIGHT_WAIT:?}"
        );
    }
}

/// The RX and chaos characteristics of the room's service, by discovery.
async fn room_chars(
    device: &bluer::Device,
) -> (
    bluer::gatt::remote::Characteristic,
    bluer::gatt::remote::Characteristic,
) {
    let mut rx = None;
    let mut chaos = None;
    for service in device.services().await.expect("services") {
        if service.uuid().await.expect("uuid") != SERVICE_UUID {
            continue;
        }
        for c in service.characteristics().await.expect("chars") {
            match c.uuid().await.expect("uuid") {
                u if u == RX_UUID => rx = Some(c),
                u if u == CHAOS_UUID => chaos = Some(c),
                _ => {}
            }
        }
    }
    (
        rx.expect("RX characteristic"),
        chaos.expect("chaos characteristic"),
    )
}

/// A write WITH response, as both the handshake and the primer go out.
async fn write_request(rx: &bluer::gatt::remote::Characteristic, bytes: &[u8]) {
    rx.write_ext(
        bytes,
        &bluer::gatt::remote::CharacteristicWriteRequest {
            op_type: bluer::gatt::WriteOp::Request,
            ..Default::default()
        },
    )
    .await
    .expect("RX write request");
}

/// The hostile traffic: write-commands racing into the open window, on
/// their own characteristic (see [`CHAOS_UUID`]).
async fn hostile_commands(chaos: &bluer::gatt::remote::Characteristic) {
    for _ in 0..3 {
        chaos
            .write_ext(
                &HOSTILE_VALUE,
                &bluer::gatt::remote::CharacteristicWriteRequest {
                    op_type: bluer::gatt::WriteOp::Command,
                    ..Default::default()
                },
            )
            .await
            .expect("chaos write command");
    }
}

/// Drain frames from the channel until `pred` is satisfied over the
/// collected list, or panic with what arrived after [`FRAME_WAIT`].
async fn collect_until(
    frames_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    collected: &mut Vec<Vec<u8>>,
    what: &str,
    pred: impl Fn(&[Vec<u8>]) -> bool,
) {
    let deadline = Instant::now() + FRAME_WAIT;
    while !pred(collected) {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "{what} not reached within {FRAME_WAIT:?}; frames so far: {:?}",
            collected
                .iter()
                .map(|f| format!("[{}B] {:02x?}", f.len(), f))
                .collect::<Vec<_>>()
        );
        if let Ok(Some(frame)) = tokio::time::timeout(left, frames_rx.recv()).await {
            collected.push(frame);
        }
    }
}

#[test]
#[ignore = "needs btvirt + bluetoothd (host-global Bluetooth state); run by name"]
fn the_triggering_write_decays_and_the_primed_identity_does_not() {
    let room = open_room();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let session = bluer::Session::new().await.expect("session");
        let periph = session.adapter(&room.adapters[0]).expect("periph adapter");
        periph.set_powered(true).await.expect("periph power");
        let periph_addr = periph.address().await.expect("periph address");
        let central = session.adapter(&room.adapters[1]).expect("central adapter");
        central.set_powered(true).await.expect("central power");

        // The room's service: one RX characteristic over AcquireWrite,
        // the shape lnsd serves it in (`bluez.rs::serve_gatt`).
        use bluer::gatt::local::{
            characteristic_control, Application, Characteristic, CharacteristicControlEvent,
            CharacteristicWrite, CharacteristicWriteMethod, Service,
        };
        use futures::FutureExt;
        let (mut control, control_handle) = characteristic_control();
        let app = Application {
            services: vec![Service {
                uuid: SERVICE_UUID,
                primary: true,
                characteristics: vec![
                    Characteristic {
                        uuid: RX_UUID,
                        write: Some(CharacteristicWrite {
                            write: true,
                            write_without_response: true,
                            method: CharacteristicWriteMethod::Io,
                            ..Default::default()
                        }),
                        control_handle,
                        ..Default::default()
                    },
                    Characteristic {
                        uuid: CHAOS_UUID,
                        write: Some(CharacteristicWrite {
                            write: true,
                            write_without_response: true,
                            method: CharacteristicWriteMethod::Fun(Box::new(|_value, _req| {
                                async move { Ok(()) }.boxed()
                            })),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let _app_handle = periph
            .serve_gatt_application(app)
            .await
            .expect("serve gatt");
        let _adv_handle = periph
            .advertise(bluer::adv::Advertisement {
                advertisement_type: bluer::adv::Type::Peripheral,
                service_uuids: vec![SERVICE_UUID].into_iter().collect(),
                discoverable: Some(true),
                local_name: Some("primer-room".into()),
                ..Default::default()
            })
            .await
            .expect("advertise");

        // The acceptor: hold the AcquireWrite round trip open for
        // ACCEPT_DELAY — the deterministic stand-in for the D-Bus
        // round trip the bench race fit into — then read every frame
        // the socket yields into one channel, in order.
        let (frames_tx, mut frames_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(event) = control.next().await {
                let CharacteristicControlEvent::Write(request) = event else {
                    continue;
                };
                tokio::time::sleep(ACCEPT_DELAY).await;
                let reader = request.accept().expect("accept");
                let tx = frames_tx.clone();
                tokio::spawn(async move {
                    loop {
                        match reader.recv().await {
                            Ok(data) if data.is_empty() => break,
                            Ok(data) => {
                                if tx.send(data).is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });
            }
        });

        let device = sight(&central, periph_addr).await;
        device.connect().await.expect("connect");
        let (rx, chaos) = room_chars(&device).await;
        // Let both sides' reverse discovery finish, so the only PDUs in
        // the window below are this test's own.
        tokio::time::sleep(Duration::from_secs(1)).await;

        // 1. The bench's order: identity first, hostile window around
        // it. The write request pends in BlueZ while the acceptor holds
        // the round trip open; the commands rewrite the receive buffer
        // it points into.
        let rx_unprimed = rx.clone();
        let pending =
            tokio::spawn(async move { write_request(&rx_unprimed, &IDENTITY_UNPRIMED).await });
        tokio::time::sleep(Duration::from_millis(150)).await;
        hostile_commands(&chaos).await;
        pending.await.expect("identity write task");
        let mut frames = Vec::new();
        // Only RX-characteristic writes reach the socket; the hostile
        // commands ride the chaos characteristic and do not.
        collect_until(&mut frames_rx, &mut frames, "the triggering frame", |f| {
            !f.is_empty()
        })
        .await;
        assert_eq!(
            frames[0].len(),
            16,
            "the triggering write kept its length; frames: {frames:02x?}"
        );
        assert_ne!(
            frames[0],
            &IDENTITY_UNPRIMED[..],
            "the triggering write arrived byte-exact through the hostile window. \
             This host's BlueZ has started copying pending writes \
             (gatt-database.c pending_write_new) — the dial primer can \
             retire; see 1004."
        );

        // A fresh connection: the socket and its write_ios entry die
        // with the ATT bearer, so the next first write triggers a new
        // AcquireWrite.
        device.disconnect().await.expect("disconnect");
        let deadline = Instant::now() + FRAME_WAIT;
        while device.is_connected().await.unwrap_or(false) {
            assert!(
                Instant::now() < deadline,
                "still connected after disconnect"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        while frames_rx.try_recv().is_ok() {}
        device.connect().await.expect("reconnect");
        let (rx, chaos) = room_chars(&device).await;
        tokio::time::sleep(Duration::from_secs(1)).await;

        // 2. The primed order under the same hostile window: the
        // keepalive takes the decay, its response proves the socket is
        // live, the identity rides the direct path.
        let rx_primed = rx.clone();
        let primer = tokio::spawn(async move {
            write_request(&rx_primed, &[leviculum_core::framing::ble::KEEPALIVE_BYTE]).await
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        hostile_commands(&chaos).await;
        primer.await.expect("primer write task");
        write_request(&rx, &IDENTITY_PRIMED).await;
        let mut frames = Vec::new();
        collect_until(&mut frames_rx, &mut frames, "primer and identity", |f| {
            f.len() >= 2
        })
        .await;
        assert_eq!(
            frames[0].len(),
            1,
            "the primer kept its one-byte length; frames: {frames:02x?}"
        );
        let sixteens: Vec<&Vec<u8>> = frames.iter().filter(|f| f.len() == 16).collect();
        assert_eq!(
            sixteens.len(),
            1,
            "exactly one 16-byte frame after the primer; frames: {frames:02x?}"
        );
        assert_eq!(
            sixteens[0].as_slice(),
            &IDENTITY_PRIMED[..],
            "the primed identity arrived byte-exact; frames: {frames:02x?}"
        );

        let _ = device.disconnect().await;
    });
}
