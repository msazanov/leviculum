//! The BlueZ half of the Columba BLE interface: everything that talks to
//! bluer. Thin by design — protocol decisions live in [`super::links`],
//! orchestration in [`super`]; this module performs advertising, the
//! GATT server, windowed scanning and the central-role connections, and
//! reports what happened as [`Ev`] events.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use bluer::adv::{Advertisement, AdvertisementHandle, Type as AdvType};
use bluer::gatt::local::{
    characteristic_control, Application, ApplicationHandle, Characteristic, CharacteristicControl,
    CharacteristicControlEvent, CharacteristicNotify, CharacteristicNotifyMethod,
    CharacteristicRead, CharacteristicWrite, CharacteristicWriteMethod, Service,
};
use bluer::gatt::remote::CharacteristicWriteRequest;
use bluer::gatt::CharacteristicReader;
use bluer::gatt::WriteOp;
use bluer::{Adapter, AdapterEvent, Address, DiscoveryFilter, DiscoveryTransport, Uuid};
use futures::stream::BoxStream;
use futures::{FutureExt, Stream, StreamExt};
use tokio::sync::{mpsc, oneshot, watch};

use super::links::{IdentityHash, SERVICE_UUID_U128};
use super::{Ev, LINK_QUEUE_DEPTH};
use crate::event_log::Scalar;
use leviculum_ble_tx::{AddrType, DialFailure, COMPANY_ID};

/// The Columba GATT service and its three characteristics
/// (BLE_PROTOCOL_v2.2 §GATT Service Structure).
pub(crate) const SERVICE_UUID: Uuid = Uuid::from_u128(SERVICE_UUID_U128);
/// TX: peripheral → central, read + notify.
pub(crate) const TX_UUID: Uuid = Uuid::from_u128(0x37145b00_442d_4a94_917f_8f42c5da28e4);
/// RX: central → peripheral, write + write-without-response.
pub(crate) const RX_UUID: Uuid = Uuid::from_u128(0x37145b00_442d_4a94_917f_8f42c5da28e5);
/// Identity: the 16-byte identity hash, read-only.
pub(crate) const IDENTITY_UUID: Uuid = Uuid::from_u128(0x37145b00_442d_4a94_917f_8f42c5da28e6);

/// One scan window per discovery cycle. The reference scans in windows
/// too (its `balanced` mode: 1 s scan, `discovery_interval` pause);
/// windowed discovery also keeps BlueZ's scanner out of the way of our
/// own connection attempts, which mostly land in the pauses.
const SCAN_WINDOW: Duration = Duration::from_secs(2);

/// Budget for one central-role connection setup: connect, resolve,
/// identity read, subscribe, handshake.
const SETUP_TIMEOUT: Duration = Duration::from_secs(20);

/// Budget for the connect step alone, the board's `CONNECT_TIMEOUT_10MS`
/// (5 s, `leviculum-nrf/src/ble/columba.rs`). A connection forms on the
/// peer's next advertisement, so the attempt is the one fresh read of
/// the record that chose the dial: a peer whose incoming slots filled
/// after we last saw it took its advertisement off the air
/// (`reconcile_advertising`), and nothing answers. Under the setup
/// budget alone such a dial held the adapter for the full 20 s; at two
/// central links per node, every one of the 58 setup timeouts in 727's
/// twenty-node rooms was a dial to a peer that had gone dark (732).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Register the Columba advertisement.
///
/// Separate from [`serve_gatt`] because the two have different
/// lifetimes: the GATT application serves live sessions for the whole
/// BlueZ session, while the advertisement is gated on link-table
/// occupancy (`LinkTable::should_advertise`, the firmware's `ADV_LOCK`
/// policy) — deregistered when the table fills, re-registered when a
/// slot frees. Dropping the returned handle deregisters it.
///
/// The advertisement carries the service UUID and the v0.3.0 capability
/// record with flags 0x00 — dual-role, and deliberately *present* rather
/// than omitted, so peers sort us by the confirmed rule instead of the
/// assumed one. Since #412 the record also carries the first four bytes
/// of our identity hash, so a scanning peer can tell us from a stranger
/// before it spends a dial; the firmware advertises the identical
/// record, built by the identical function. BlueZ places the
/// `LN-<hex8>` local name in the scan response, where it does not
/// compete with the 31 advertisement bytes (same layout as the
/// firmware's).
///
/// `free_slots` is the table's free INCOMING count (#432): the same
/// quantity a board publishes, since lnsd now has a board's slot shape.
/// It is a parameter rather than read here, because this function is
/// also the only re-registration path — the caller compares what is on
/// the air with what the table says and calls again when they differ.
pub(crate) async fn register_advertisement(
    adapter: &Adapter,
    identity: IdentityHash,
    iface: &str,
    free_slots: u8,
) -> bluer::Result<AdvertisementHandle> {
    let name = super::links::local_name(&identity);
    // The manufacturer-record payload minus the company ID: BlueZ keys
    // the record by CID and prepends it on the wire.
    let record = super::links::capability_record(&identity, free_slots)[2..].to_vec();

    let advertisement = Advertisement {
        advertisement_type: AdvType::Peripheral,
        service_uuids: BTreeSet::from([SERVICE_UUID]),
        manufacturer_data: BTreeMap::from([(COMPANY_ID, record)]),
        discoverable: Some(true),
        local_name: Some(name.clone()),
        ..Default::default()
    };
    let adv = adapter.advertise(advertisement).await?;
    tracing::info!(
        "BLE {iface}: advertising as {name} (service {SERVICE_UUID}, dual-role record present, \
         identity hint {}, free incoming slots {free_slots})",
        super::links::hint_str(Some(leviculum_ble_tx::identity_hint(&identity))),
    );
    Ok(adv)
}

/// Register the Columba GATT application; it serves every
/// peripheral-role session for the whole BlueZ session. Dropping the
/// returned handle deregisters it.
///
/// The RX characteristic takes its writes over AcquireWrite
/// ([`CharacteristicWriteMethod::Io`], #436): BlueZ hands us one
/// SEQPACKET socket per connected central, and [`periph_reader`] reads
/// it in one task, so a central's frames reach the event channel in
/// the order the kernel received them. Through WriteValue each write
/// was its own D-Bus call on its own tokio task (bluer-0.17.4
/// `session.rs` spawns every crossroads method), and two fragments
/// issued back to back could arrive inverted. Once a central's socket
/// is acquired BlueZ routes every write on that connection through it,
/// with or without response (bluez-5.82 `src/gatt-database.c`,
/// `chrc_write_cb`: a `write_ios` entry for the ATT channel short-cuts
/// to `sock_io_send` before any WriteValue), so the identity
/// handshake, a Write Request, arrives on the socket too: it is the
/// write that triggers the AcquireWrite, and BlueZ forwards it once the
/// socket exists. The WriteValue method bluer still exports answers
/// NotSupported under `Io`; BlueZ only falls back to it when an
/// AcquireWrite fails.
pub(crate) async fn serve_gatt(
    adapter: &Adapter,
    identity: IdentityHash,
    ev_tx: mpsc::Sender<Ev>,
    iface: String,
) -> bluer::Result<ApplicationHandle> {
    let write_tx = ev_tx.clone();
    let notify_tx = ev_tx;
    let (rx_control, rx_control_handle) = characteristic_control();
    let app = Application {
        services: vec![Service {
            uuid: SERVICE_UUID,
            primary: true,
            characteristics: vec![
                Characteristic {
                    uuid: TX_UUID,
                    read: Some(CharacteristicRead {
                        read: true,
                        // The TX value is only ever pushed as
                        // notifications; a read returns nothing. The
                        // read flag exists because the reference
                        // declares it (`ble-reticulum@07d94130` `linux_bluetooth_driver.py`, TX char flags).
                        fun: Box::new(|_req| async move { Ok(Vec::new()) }.boxed()),
                        ..Default::default()
                    }),
                    notify: Some(CharacteristicNotify {
                        notify: true,
                        method: CharacteristicNotifyMethod::Fun(Box::new(move |notifier| {
                            let tx = notify_tx.clone();
                            async move {
                                let _ = tx.send(Ev::PeriphNotify(notifier)).await;
                            }
                            .boxed()
                        })),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Characteristic {
                    uuid: RX_UUID,
                    write: Some(CharacteristicWrite {
                        write: true,
                        write_without_response: true,
                        method: CharacteristicWriteMethod::Io,
                        ..Default::default()
                    }),
                    control_handle: rx_control_handle,
                    ..Default::default()
                },
                Characteristic {
                    uuid: IDENTITY_UUID,
                    read: Some(CharacteristicRead {
                        read: true,
                        fun: Box::new(move |_req| async move { Ok(identity.to_vec()) }.boxed()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    let handle = adapter.serve_gatt_application(app).await?;
    // The acceptor ends with the registration: deregistering drops the
    // control handle, which ends the event stream.
    tokio::spawn(accept_writers(rx_control, write_tx, iface));
    Ok(handle)
}

/// Accept every central's AcquireWrite on the RX characteristic and
/// give each socket its own [`periph_reader`]. bluer's AcquireWrite
/// handler waits for this answer, so the loop does nothing else.
async fn accept_writers(
    mut control: CharacteristicControl,
    ev_tx: mpsc::Sender<Ev>,
    iface: String,
) {
    while let Some(event) = control.next().await {
        let CharacteristicControlEvent::Write(request) = event else {
            continue;
        };
        let addr = request.device_address();
        match request.accept() {
            Ok(reader) => {
                let mtu = reader.mtu();
                tracing::debug!("BLE {iface}: write socket acquired for {addr} (mtu {mtu})");
                tokio::spawn(periph_reader(
                    addr,
                    mtu,
                    reader_frames(reader),
                    ev_tx.clone(),
                ));
            }
            Err(e) => {
                // BlueZ falls back to WriteValue, which `Io` refuses: the
                // central's writes fail and the handshake timeout clears it.
                tracing::warn!("BLE {iface}: write socket for {addr} not created: {e}");
            }
        }
    }
}

/// One acquired write socket as a stream of written values, one item
/// per ATT write.
fn reader_frames(reader: CharacteristicReader) -> impl Stream<Item = std::io::Result<Vec<u8>>> {
    futures::stream::unfold(reader, |reader| async move {
        let frame = reader.recv().await;
        Some((frame, reader))
    })
}

/// One central's write stream, read by ONE task: each value goes to the
/// event channel as [`Ev::PeriphWrite`] in the order the socket yields
/// it, which is the order the kernel received the writes (#436). The
/// stream's end is the central's disconnect — BlueZ closes its end of
/// the socket when the ATT channel goes (bluez-5.82
/// `src/gatt-database.c`, `client_io_new` registers
/// `att_disconnect_cb`, which frees the client and its io) — and is
/// reported as [`Ev::PeriphGone`], always the reader's last event.
///
/// A zero-length read is that end of stream on SEQPACKET. A central
/// writing an empty value would read the same; the v2.2 protocol never
/// does (its shortest frame is the one-byte keepalive).
pub(crate) async fn periph_reader<S>(addr: Address, mtu: usize, frames: S, ev_tx: mpsc::Sender<Ev>)
where
    S: Stream<Item = std::io::Result<Vec<u8>>>,
{
    let mut frames = std::pin::pin!(frames);
    while let Some(Ok(data)) = frames.next().await {
        if data.is_empty() {
            break;
        }
        if ev_tx
            .send(Ev::PeriphWrite { addr, mtu, data })
            .await
            .is_err()
        {
            return;
        }
    }
    let _ = ev_tx.send(Ev::PeriphGone { addr }).await;
}

/// Windowed discovery: scan for [`SCAN_WINDOW`], report every sighting,
/// pause for the configured interval, repeat. Dropping the discovery
/// stream stops the BlueZ scan between windows.
///
/// While a dial waits out its jitter (`hold`, the orchestrator's
/// `DialQueue::wants_scan`) the scanner stays on (757): a window runs
/// on past its end, and a pause is cut short by a fresh run. BlueZ
/// refreshes a device's record only from the reports it hears while
/// discovering, so before 757 every dial read a record 3 to 10 s old
/// (755: in 16 of 16 misses the peer had said it was full before the
/// dial started, and in 0 of 96 dials was discovery running).
pub(crate) struct ScanTask {
    pub adapter: Adapter,
    pub ev_tx: mpsc::Sender<Ev>,
    pub interval: Duration,
    pub iface: String,
    pub hold: watch::Receiver<bool>,
    /// The running run's tally for its debug line at the end (757):
    /// start, events probed, cache entries among them, first live
    /// report.
    pub run: RunTally,
}

/// One discovery run's tally (757).
#[derive(Default)]
pub(crate) struct RunTally {
    started: Option<tokio::time::Instant>,
    events: usize,
    cached: usize,
    first_live_ms: Option<u128>,
}

impl ScanTask {
    pub(crate) async fn run(self) {
        let filter = DiscoveryFilter {
            uuids: [SERVICE_UUID].into_iter().collect(),
            transport: DiscoveryTransport::Le,
            ..Default::default()
        };
        if let Err(e) = self.adapter.set_discovery_filter(filter).await {
            tracing::warn!("BLE {}: discovery filter rejected: {e}", self.iface);
        }
        let hold = self.hold.clone();
        let interval = self.interval;
        scan_cadence(self, hold, interval).await;
    }

    /// Read the sighting's decision inputs from BlueZ's parsed properties.
    async fn probe(&self, addr: Address) -> Option<Ev> {
        let device = self.adapter.device(addr).ok()?;
        let rssi = device.rssi().await.ok()?;
        let addr_type = device_addr_type(&device).await;
        let offers_service = device
            .uuids()
            .await
            .ok()?
            .is_some_and(|uuids| uuids.contains(&SERVICE_UUID));
        let record = device
            .manufacturer_data()
            .await
            .ok()
            .flatten()
            .and_then(|mut m| m.remove(&COMPANY_ID));
        Some(Ev::Scan {
            addr,
            addr_type,
            rssi,
            offers_service,
            record,
        })
    }
}

impl Discovery for ScanTask {
    type Item = AdapterEvent;

    async fn start(&mut self) -> Result<BoxStream<'static, AdapterEvent>, RunFailed> {
        // `discover_devices_with_changes`, not `discover_devices`:
        // plain `DeviceAdded` fires only when bluetoothd CREATES a
        // device object. For a device it already holds (stored after
        // an earlier connect, or created by a previous window), the
        // stream opens with a synthetic `DeviceAdded` carrying no
        // RSSI — BlueZ drops RSSI outside discovery, so the receiver
        // discards it as a cache entry — and the adv reports that
        // then land in the window only update properties, which the
        // plain stream never surfaces. Every window after the first
        // was blind to every known peer (BLE path hunt round 2,
        // 2026-08-31). The `_with_changes` stream re-emits
        // `DeviceAdded` on each property change, so the first adv
        // report of the window re-offers the device with its RSSI
        // set.
        match self.adapter.discover_devices_with_changes().await {
            Ok(events) => Ok(events.boxed()),
            Err(e) => {
                // 600: an adapter object that is gone (bluetoothd
                // restarted, the dongle re-enumerated) fails every
                // later discovery the same way, forever. Hand it to
                // the orchestrator, which rebuilds the session on a
                // fresh resolution; retrying here would only scan a
                // dead object path.
                let adapter_present = self.adapter.is_powered().await.is_ok();
                if adapter_lost(&e, adapter_present) {
                    let _ = self
                        .ev_tx
                        .send(Ev::AdapterLost {
                            error: e.to_string(),
                        })
                        .await;
                    return Err(RunFailed::Stop);
                }
                tracing::debug!("BLE {}: discovery failed: {e}", self.iface);
                Err(RunFailed::Retry)
            }
        }
    }

    async fn sighting(&mut self, event: AdapterEvent) -> bool {
        let AdapterEvent::DeviceAdded(addr) = event else {
            return true;
        };
        let ev = self.probe(addr).await;
        self.run.events += 1;
        match &ev {
            Some(Ev::Scan { rssi: None, .. }) => self.run.cached += 1,
            Some(Ev::Scan { .. }) if self.run.first_live_ms.is_none() => {
                self.run.first_live_ms = self.run.started.map(|t| t.elapsed().as_millis());
            }
            _ => {}
        }
        match ev {
            Some(ev) => self.ev_tx.send(ev).await.is_ok(),
            None => true,
        }
    }

    async fn run_edge(&mut self, on: bool) -> bool {
        if on {
            self.run = RunTally {
                started: Some(tokio::time::Instant::now()),
                ..RunTally::default()
            };
        } else {
            tracing::debug!(
                "BLE {}: scan run of {} ms: {} events, {} cache entries, first live report after {} ms",
                self.iface,
                self.run.started.map_or(0, |t| t.elapsed().as_millis()),
                self.run.events,
                self.run.cached,
                self.run
                    .first_live_ms
                    .map_or("-".to_string(), |ms| ms.to_string()),
            );
        }
        self.ev_tx.send(Ev::ScanRun { on }).await.is_ok()
    }
}

/// Why a discovery run did not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunFailed {
    /// This run failed; the next one is tried after the pause.
    Retry,
    /// The adapter is gone and the orchestrator was told: no more runs.
    Stop,
}

/// What [`scan_cadence`] drives: BlueZ discovery in lnsd, a counting
/// fake in the tests. Every method's `false` means the orchestrator is
/// gone and the cadence ends.
pub(crate) trait Discovery {
    type Item: Send;
    /// Start one discovery run; dropping the stream ends it.
    fn start(
        &mut self,
    ) -> impl std::future::Future<Output = Result<BoxStream<'static, Self::Item>, RunFailed>> + Send;
    /// One event of the running discovery.
    fn sighting(&mut self, item: Self::Item) -> impl std::future::Future<Output = bool> + Send;
    /// A run started (`on`) or ended, for `DialQueue::scan_run` (757).
    fn run_edge(&mut self, on: bool) -> impl std::future::Future<Output = bool> + Send;
}

/// The scan cadence: a [`SCAN_WINDOW`] run, then `interval` with the
/// scanner off, repeated, exactly as before 757 while `hold` stays
/// false. While `hold` is true the scanner stays on (757): a run goes
/// on past its window until `hold` clears, and a pause that sees `hold`
/// set starts a run at once, which ends as soon as `hold` clears. The
/// pause after a run is always the full `interval`, so a drained queue
/// hands the cadence back where it would have been after a window.
pub(crate) async fn scan_cadence<D: Discovery>(
    mut discovery: D,
    mut hold: watch::Receiver<bool>,
    interval: Duration,
) {
    let mut held_start = false;
    loop {
        let mut started = false;
        match discovery.start().await {
            Ok(mut events) => {
                started = true;
                if !discovery.run_edge(true).await {
                    return;
                }
                let window_end = tokio::time::Instant::now() + SCAN_WINDOW;
                // A run the hold started has no window of its own.
                let mut window_over = held_start;
                loop {
                    if window_over && !*hold.borrow_and_update() {
                        break;
                    }
                    tokio::select! {
                        event = events.next() => match event {
                            Some(item) => {
                                if !discovery.sighting(item).await {
                                    return;
                                }
                            }
                            None => break,
                        },
                        _ = tokio::time::sleep_until(window_end), if !window_over => {
                            window_over = true;
                        }
                        changed = hold.changed() => {
                            if changed.is_err() {
                                return;
                            }
                        }
                    }
                }
                drop(events);
                if !discovery.run_edge(false).await {
                    return;
                }
            }
            Err(RunFailed::Stop) => return,
            Err(RunFailed::Retry) => {}
        }
        // A failed start pauses the full interval whatever the hold
        // says: a discovery that keeps failing must not become a busy
        // loop against bluetoothd.
        held_start = if started {
            tokio::select! {
                _ = tokio::time::sleep(interval) => false,
                held = hold.wait_for(|held| *held) => {
                    if held.is_err() {
                        return;
                    }
                    true
                }
            }
        } else {
            tokio::time::sleep(interval).await;
            false
        };
    }
}

/// The address type BlueZ holds for a device (#506), `None` when it
/// cannot say: the device object is gone, or the property read failed.
pub(crate) async fn addr_type(adapter: &Adapter, addr: Address) -> Option<AddrType> {
    device_addr_type(&adapter.device(addr).ok()?).await
}

async fn device_addr_type(device: &bluer::Device) -> Option<AddrType> {
    let kind = device.address_type().await.ok()?;
    Some(super::links::addr_type_from_bluez(
        kind == bluer::AddressType::LeRandom,
        &device.address().0,
    ))
}

/// One central-role connection: connect, read the peer's identity, ask
/// the orchestrator for admission, subscribe, handshake, then pump
/// frames in both directions until either side ends the session.
/// Always reports [`Ev::CentralGone`] as its last event.
pub(crate) struct CentralTask {
    pub adapter: Adapter,
    pub addr: Address,
    pub own_identity: IdentityHash,
    pub ev_tx: mpsc::Sender<Ev>,
    pub iface: String,
    /// The interface's `tx_gap_ms` override, for this link's pacer.
    pub tx_gap_ms: Option<u16>,
}

impl CentralTask {
    pub(crate) async fn run(self) {
        let failure = match bounded_setup(&self.iface, self.addr, self.establish()).await {
            Some(Ok(Some(session))) => {
                self.session_loop(session).await;
                None
            }
            Some(Ok(None)) => {
                // Admission refused; the orchestrator logged why.
                None
            }
            Some(Err(e)) => {
                tracing::debug!(
                    "BLE {}: central {} failed: {} ({})",
                    self.iface,
                    self.addr,
                    e.source,
                    e.failure.as_str()
                );
                Some(e.failure)
            }
            None => {
                tracing::debug!("BLE {}: central {} setup timed out", self.iface, self.addr);
                Some(DialFailure::Transport)
            }
        };
        if let Ok(device) = self.adapter.device(self.addr) {
            let _ = device.disconnect().await;
        }
        let _ = self
            .ev_tx
            .send(Ev::CentralGone {
                addr: self.addr,
                failure,
            })
            .await;
    }

    async fn establish(&self) -> Result<Option<CentralSession>, SetupError> {
        // Everything up to the layout check is the air's business: a
        // connect that fails, a discovery the link drops under. Only a
        // device object that is gone (595) and the two answers below
        // are the peer's (579).
        let device = self
            .adapter
            .device(self.addr)
            .map_err(SetupError::with(DialFailure::Transport))?;
        let connect = async {
            if !device.is_connected().await? {
                device.connect().await?;
            }
            Ok::<(), bluer::Error>(())
        };
        bounded_connect(&self.iface, self.addr, connect, async {
            self.adapter.is_powered().await.is_ok()
        })
        .await?;
        self.layout_and_handshake(&device).await
    }

    async fn layout_and_handshake(
        &self,
        device: &bluer::Device,
    ) -> Result<Option<CentralSession>, SetupError> {
        let transport = SetupError::with(DialFailure::Transport);

        // `Device::services` waits for BlueZ's ServicesResolved itself.
        let mut tx_char = None;
        let mut rx_char = None;
        let mut id_char = None;
        for service in device.services().await.map_err(transport)? {
            if service.uuid().await.map_err(transport)? != SERVICE_UUID {
                continue;
            }
            for characteristic in service.characteristics().await.map_err(transport)? {
                match characteristic.uuid().await.map_err(transport)? {
                    u if u == TX_UUID => tx_char = Some(characteristic),
                    u if u == RX_UUID => rx_char = Some(characteristic),
                    u if u == IDENTITY_UUID => id_char = Some(characteristic),
                    _ => {}
                }
            }
        }
        let (Some(tx_char), Some(rx_char), Some(id_char)) = (tx_char, rx_char, id_char) else {
            return Err(SetupError::not_columba());
        };

        // Identity first: admission is decided before any subscription
        // or handshake touches the peer's session state (the firmware
        // orders it the same way).
        let identity_bytes = id_char.read().await.map_err(transport)?;
        let identity: IdentityHash = identity_bytes
            .as_slice()
            .try_into()
            .map_err(|_| SetupError::not_columba())?;
        // BlueZ ≥ 5.62 exposes the exchanged ATT MTU as a characteristic
        // property; on anything older fall back to the BLE 4.0 floor,
        // the reference's final fallback too.
        let mtu = tx_char
            .mtu()
            .await
            .unwrap_or(leviculum_core::framing::ble::MIN_MTU);

        let (frames_tx, frames_rx) = super::census::packet_queue(LINK_QUEUE_DEPTH);
        let (ack_tx, ack_rx) = oneshot::channel();
        let sent = self
            .ev_tx
            .send(Ev::CentralIdentity {
                addr: self.addr,
                identity,
                mtu,
                frames: frames_tx,
                ack: ack_tx,
            })
            .await;
        if sent.is_err() || !ack_rx.await.unwrap_or(false) {
            return Ok(None);
        }

        // Subscribe before the handshake so nothing the peer sends lands
        // in an unsubscribed gap.
        let notify_stream = tx_char.notify().await.map_err(transport)?;

        // The 16-byte identity handshake, written WITH response
        // (BLE_PROTOCOL_v2.2 §Identity Handshake: a Write Request).
        rx_char
            .write_ext(
                &self.own_identity,
                &CharacteristicWriteRequest {
                    op_type: WriteOp::Request,
                    ..Default::default()
                },
            )
            .await
            // The peer answered our identity with an error: it will not
            // handshake (579).
            .map_err(SetupError::with(DialFailure::NoHandshake))?;

        Ok(Some(CentralSession {
            notify_stream: Box::pin(notify_stream),
            frames_rx,
            rx_char,
        }))
    }

    async fn session_loop(&self, mut session: CentralSession) {
        let write_req = CharacteristicWriteRequest {
            op_type: WriteOp::Command,
            ..Default::default()
        };
        // This link's inter-packet gap (#376): the queue carries one
        // packet's fragments per unit, so pacing packet-to-packet is a
        // sleep between units. Keepalives bypass the gap and never
        // slide its window, same rule as the firmware pumps.
        let start = tokio::time::Instant::now();
        let mut pacer = super::links::LinkPacer::new(self.tx_gap_ms);
        loop {
            tokio::select! {
                frame = session.notify_stream.next() => {
                    let Some(data) = frame else { break };
                    let ev = Ev::CentralFrame { addr: self.addr, data };
                    if self.ev_tx.send(ev).await.is_err() {
                        break;
                    }
                }
                unit = session.frames_rx.recv() => {
                    let Some(frames) = unit else { break };
                    let keepalive = super::census::is_keepalive_unit(&frames);
                    if !keepalive {
                        let now_ms = tokio::time::Instant::now()
                            .duration_since(start)
                            .as_millis() as u64;
                        let wait = pacer.wait_ms(now_ms);
                        if wait > 0 {
                            tracing::info!(
                                event = "BLE_TX_GAP",
                                iface = %Scalar(&self.iface),
                                addr = %self.addr,
                                waited_ms = wait,
                            );
                            tokio::time::sleep(Duration::from_millis(wait)).await;
                        }
                    }
                    let mut failed = false;
                    for payload in &frames {
                        if let Err(e) = session.rx_char.write_ext(payload, &write_req).await {
                            tracing::debug!(
                                "BLE {}: write to {} failed: {e}",
                                self.iface,
                                self.addr
                            );
                            failed = true;
                            break;
                        }
                    }
                    if !keepalive {
                        pacer.packet_done(
                            tokio::time::Instant::now()
                                .duration_since(start)
                                .as_millis() as u64,
                        );
                    }
                    if failed {
                        break;
                    }
                }
            }
        }
    }
}

struct CentralSession {
    notify_stream: std::pin::Pin<Box<dyn futures::Stream<Item = Vec<u8>> + Send>>,
    frames_rx: super::census::PacketRx,
    rx_char: bluer::gatt::remote::Characteristic,
}

/// What a BlueZ error at the connect step says about the peer (595).
///
/// `NotFound` is D-Bus `UnknownObject`: the device object is gone.
/// BlueZ removes a temporary device whose last advertisement was
/// non-connectable when the discovery window stops (bluez-5.82
/// `src/adapter.c`, `discovery_cleanup`), and the dial comes after the
/// window, so a broadcaster fails here every time, at once. That is the
/// address's doing and books as a peer failure. Unless the adapter
/// object is gone too (`adapter_present` false, 589's run 1): then
/// every object is, and it says nothing about the peer. Anything else,
/// a connect that timed out or was refused on the air, stays transport.
fn connect_failure(source: &bluer::Error, adapter_present: bool) -> DialFailure {
    match source.kind {
        bluer::ErrorKind::NotFound if adapter_present => DialFailure::NoObject,
        _ => DialFailure::Transport,
    }
}

/// A whole central setup under [`SETUP_TIMEOUT`]; `None` when the budget
/// ran out, which logs `BLE_SETUP_TIMEOUT` (758). The event is the
/// record a dial census counts; the DEBUG line beside it is prose.
async fn bounded_setup<T>(
    iface: &str,
    addr: Address,
    setup: impl std::future::Future<Output = T>,
) -> Option<T> {
    let outcome = tokio::time::timeout(SETUP_TIMEOUT, setup).await.ok();
    if outcome.is_none() {
        tracing::info!(
            event = "BLE_SETUP_TIMEOUT",
            iface = %Scalar(iface),
            addr = %super::hex12(&addr.0),
            after_ms = SETUP_TIMEOUT.as_millis() as u64,
        );
    }
    outcome
}

/// The connect step of a central setup under [`CONNECT_TIMEOUT`] (732).
/// A failed connect is classified by [`connect_failure`], which reads
/// `adapter_present` only then; a connect that nothing answered is the
/// air's business (`Transport`), the class the setup timeout gave it
/// before, so the address's backoff is unchanged and only the wait is
/// shorter. That case logs `BLE_CONNECT_UNANSWERED` (758).
async fn bounded_connect(
    iface: &str,
    addr: Address,
    connect: impl std::future::Future<Output = Result<(), bluer::Error>>,
    adapter_present: impl std::future::Future<Output = bool>,
) -> Result<(), SetupError> {
    match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(source)) => Err(SetupError {
            failure: connect_failure(&source, adapter_present.await),
            source,
        }),
        Err(_) => {
            tracing::info!(
                event = "BLE_CONNECT_UNANSWERED",
                iface = %Scalar(iface),
                addr = %super::hex12(&addr.0),
                after_ms = CONNECT_TIMEOUT.as_millis() as u64,
            );
            Err(unanswered_connect())
        }
    }
}

/// The error a connect nothing answered within [`CONNECT_TIMEOUT`] ends
/// the setup with.
fn unanswered_connect() -> SetupError {
    SetupError {
        failure: DialFailure::Transport,
        source: bluer::Error {
            kind: bluer::ErrorKind::ConnectionAttemptFailed,
            message: format!(
                "connect not answered within {} s: the peer is not advertising",
                CONNECT_TIMEOUT.as_secs()
            ),
        },
    }
}

/// Whether a failed BlueZ call says the ADAPTER's object is gone (600):
/// D-Bus `UnknownObject` (`NotFound`, 595's classification) on a call
/// against the adapter, confirmed by a property read of the adapter
/// failing too (`adapter_present` false). The confirmation keeps a
/// `NotFound` that named some other object, a device the call touched,
/// from tearing down a healthy session.
fn adapter_lost(source: &bluer::Error, adapter_present: bool) -> bool {
    source.kind == bluer::ErrorKind::NotFound && !adapter_present
}

/// Why a central setup gave up: the BlueZ error, and what it says about
/// the peer (579).
struct SetupError {
    failure: DialFailure,
    source: bluer::Error,
}

impl SetupError {
    /// A `map_err` adapter that tags a BlueZ error with `failure`.
    fn with(failure: DialFailure) -> impl Fn(bluer::Error) -> Self + Copy {
        move |source| Self { failure, source }
    }

    /// The peer serves no Columba layout, or an identity of the wrong
    /// length in it.
    fn not_columba() -> Self {
        Self {
            failure: DialFailure::NotColumba,
            source: bluer::Error {
                kind: bluer::ErrorKind::ServicesUnresolved,
                message: "Columba GATT layout not found on peer".to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interfaces::ble::links::{
        Inbound, LinkTable, DEFAULT_CENTRAL_LINKS, DEFAULT_MAX_LINKS,
    };
    use leviculum_core::framing::ble::fragment_packet;
    use std::os::fd::OwnedFd;

    const OWN: IdentityHash = [0x0E; 16];
    const PEER: IdentityHash = [0xA1; 16];
    const ADDR: Address = Address([0xC0, 0x00, 0x00, 0x00, 0x00, 0x01]);
    const MTU: usize = 185;

    /// The error bluer hands back for a D-Bus error reply, built the way
    /// bluer builds it (`impl From<dbus::Error> for bluer::Error`).
    fn bluez_reply(name: &str, message: &str) -> bluer::Error {
        bluer::Error::from(dbus::Error::new_custom(name, message))
    }

    /// 595: a dial at an address whose device object is gone (D-Bus
    /// `UnknownObject`, what 579's strangers drew) is the peer's
    /// failure; with the adapter gone too it is not, and a connect the
    /// air refused or timed out stays transport.
    #[test]
    fn a_dial_without_an_object_is_a_peer_failure() {
        let gone = bluez_reply(
            "org.freedesktop.DBus.Error.UnknownObject",
            "Method \"Connect\" with signature \"\" on interface \"org.bluez.Device1\" doesn't exist",
        );
        assert_eq!(gone.kind, bluer::ErrorKind::NotFound);
        assert_eq!(
            gone.to_string(),
            "the target object was either not present or removed",
            "the message 579's log printed"
        );
        assert_eq!(connect_failure(&gone, true), DialFailure::NoObject);
        assert_eq!(connect_failure(&gone, false), DialFailure::Transport);

        // bluetoothd's own connect failures (bluez-5.82 `src/error.h`,
        // `btd_error_failed`): the air's, not the peer's.
        for message in ["le-connection-timeout", "le-connection-abort-by-local"] {
            let air = bluez_reply("org.bluez.Error.Failed", message);
            assert_eq!(air.kind, bluer::ErrorKind::Failed);
            assert_eq!(connect_failure(&air, true), DialFailure::Transport);
        }
        let busy = bluez_reply("org.bluez.Error.InProgress", "In Progress");
        assert_eq!(connect_failure(&busy, true), DialFailure::Transport);
    }

    /// 600: a discovery that failed because the adapter's object is
    /// gone (589's run 1: `the target object was either not present or
    /// removed`, with the adapter's own property read failing too) is an
    /// adapter loss; the same error with the adapter answering, or any
    /// other error, is a failed window and the scan loop goes on.
    #[test]
    fn a_discovery_without_an_adapter_object_is_an_adapter_loss() {
        let gone = bluez_reply(
            "org.freedesktop.DBus.Error.UnknownObject",
            "Method \"StartDiscovery\" with signature \"\" on interface \"org.bluez.Adapter1\" doesn't exist",
        );
        assert_eq!(
            gone.to_string(),
            "the target object was either not present or removed",
            "the message 589's run 1 printed every 5 s"
        );
        assert!(adapter_lost(&gone, false));
        assert!(!adapter_lost(&gone, true));
        let busy = bluez_reply(
            "org.bluez.Error.InProgress",
            "Operation already in progress",
        );
        assert!(!adapter_lost(&busy, false));
        let not_ready = bluez_reply("org.bluez.Error.NotReady", "Resource Not Ready");
        assert!(!adapter_lost(&not_ready, false));
    }

    /// What BlueZ and bluer build for one AcquireWrite: a SEQPACKET pair
    /// (bluer-0.17.4 `gatt/mod.rs` `make_socket_pair`). The first half
    /// stands in for bluetoothd's end, the second is ours, as tokio
    /// reads it.
    fn acquired_socket() -> (socket2::Socket, tokio::net::UnixDatagram) {
        let (central, ours) =
            socket2::Socket::pair(socket2::Domain::UNIX, socket2::Type::SEQPACKET, None)
                .expect("socketpair");
        let ours = std::os::unix::net::UnixDatagram::from(OwnedFd::from(ours));
        ours.set_nonblocking(true).expect("nonblocking");
        (
            central,
            tokio::net::UnixDatagram::from_std(ours).expect("tokio socket"),
        )
    }

    /// The test's `reader_frames`: the same one-value-per-read stream,
    /// over the stand-in socket.
    fn socket_frames(
        socket: tokio::net::UnixDatagram,
    ) -> impl Stream<Item = std::io::Result<Vec<u8>>> {
        futures::stream::unfold(socket, |socket| async move {
            let mut buf = vec![0u8; MTU];
            let frame = socket.recv(&mut buf).await.map(|n| {
                buf.truncate(n);
                buf
            });
            Some((frame, socket))
        })
    }

    /// Run the reader over everything the central wrote, then close the
    /// central's end, and return every event the reader reported.
    async fn read_all(writes: &[Vec<u8>]) -> Vec<Ev> {
        let (central, ours) = acquired_socket();
        for w in writes {
            central.send(w).expect("central write");
        }
        drop(central);
        let (ev_tx, mut ev_rx) = mpsc::channel(64);
        periph_reader(ADDR, MTU, socket_frames(ours), ev_tx).await;
        let mut events = Vec::new();
        while let Ok(ev) = ev_rx.try_recv() {
            events.push(ev);
        }
        events
    }

    /// #436: two fragments a central wrote back to back reach the event
    /// channel in the order it wrote them, tagged with its address and
    /// the acquired MTU. Through WriteValue each write had its own task
    /// and the END could overtake its START.
    #[tokio::test]
    async fn the_write_socket_delivers_a_centrals_fragments_in_order() {
        let packet = vec![0x33; 211];
        let frags = fragment_packet(&packet, MTU);
        assert_eq!(frags.len(), 2);

        let events = read_all(&frags).await;
        let writes: Vec<Vec<u8>> = events
            .iter()
            .filter_map(|ev| match ev {
                Ev::PeriphWrite { addr, mtu, data } => {
                    assert_eq!((*addr, *mtu), (ADDR, MTU));
                    Some(data.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(writes, frags, "fragments arrive in write order");
    }

    /// #436: the socket's end is the central's disconnect. The reader
    /// reports it as its last event, and the orchestrator's answer to
    /// that event releases the peripheral link the handshake opened;
    /// before the socket the link waited for the expiry sweep.
    #[tokio::test]
    async fn the_write_sockets_end_releases_the_link() {
        let packet = vec![0x44; 211];
        let mut writes = vec![PEER.to_vec()];
        writes.extend(fragment_packet(&packet, MTU));

        let events = read_all(&writes).await;
        assert!(
            matches!(events.last(), Some(Ev::PeriphGone { addr }) if *addr == ADDR),
            "end of stream is reported, last"
        );

        // The orchestrator's two arms, as `handle_event` runs them.
        let mut table = LinkTable::new(OWN, DEFAULT_MAX_LINKS, DEFAULT_CENTRAL_LINKS);
        let mut delivered = Vec::new();
        for ev in events {
            match ev {
                Ev::PeriphWrite { addr, mtu, data } => {
                    match table.peripheral_frame(addr.0, mtu, &data, 0) {
                        Inbound::Packet(p) => delivered.push(p),
                        Inbound::HandshakeComplete { .. } | Inbound::NeedMore => {}
                        other => panic!("unexpected {other:?}"),
                    }
                    assert_eq!(table.link_count(), 1);
                }
                Ev::PeriphGone { addr } => {
                    let gone = table.remove_peripheral_by_addr(&addr.0);
                    assert_eq!(gone.map(|(id, _, _)| id), Some(PEER));
                }
                _ => panic!("reader reports only writes and its end"),
            }
        }
        assert_eq!(delivered, vec![packet]);
        assert_eq!(table.link_count(), 0, "the link is released");
    }

    /// Write `writes` from a central on its own thread, as fast as the
    /// socket takes them, while the reader and the orchestrator's
    /// `PeriphWrite` arm run concurrently over the interface's 64-deep
    /// event channel (`mod.rs`, `session`). Returns the packets the
    /// link table delivered and every abandonment report it made.
    async fn deliver_under_load(
        writes: Vec<Vec<u8>>,
    ) -> (Vec<Vec<u8>>, Vec<(IdentityHash, u32, u32)>) {
        let (central, ours) = acquired_socket();
        let writer = std::thread::spawn(move || {
            for w in &writes {
                central.send(w).expect("central write");
            }
        });
        let (ev_tx, mut ev_rx) = mpsc::channel(64);
        let reader = tokio::spawn(periph_reader(ADDR, MTU, socket_frames(ours), ev_tx));

        let mut table = LinkTable::new(OWN, DEFAULT_MAX_LINKS, DEFAULT_CENTRAL_LINKS);
        let mut delivered = Vec::new();
        let mut abandoned = Vec::new();
        while let Some(ev) = ev_rx.recv().await {
            match ev {
                Ev::PeriphWrite { addr, mtu, data } => {
                    if let Inbound::Packet(p) = table.peripheral_frame(addr.0, mtu, &data, 0) {
                        delivered.push(p);
                    }
                    abandoned.extend(table.take_abandon_reports());
                }
                Ev::PeriphGone { .. } => break,
                _ => panic!("reader reports only writes and its end"),
            }
        }
        writer.join().expect("writer");
        reader.await.expect("reader");
        (delivered, abandoned)
    }

    /// The handshake, then 400 packets of one to five fragments each.
    fn load_writes() -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        let packets: Vec<Vec<u8>> = (0..400u16)
            .map(|i| {
                let len = 40 + usize::from(i % 5) * 180;
                let [hi, lo] = i.to_be_bytes();
                (0..len).map(|j| hi ^ lo.wrapping_add(j as u8)).collect()
            })
            .collect();
        let mut writes = vec![PEER.to_vec()];
        for p in &packets {
            writes.extend(fragment_packet(p, MTU));
        }
        (packets, writes)
    }

    /// #436, under load: a central writes 400 packets of up to five
    /// fragments back to back, the reader and the link table run on
    /// other threads, and the channel between them fills. Every packet
    /// arrives, in the order written, and the link reports no
    /// `BLE_RX_ABANDON`: on the acquired socket nothing reorders, so the
    /// one-frame park of bc07cee3 is never needed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_write_socket_keeps_order_under_load_without_an_abandon() {
        let (packets, writes) = load_writes();
        assert!(writes.len() > 1000, "{} writes", writes.len());

        let (delivered, abandoned) = deliver_under_load(writes).await;
        assert_eq!(abandoned, vec![], "no BLE_RX_ABANDON");
        assert_eq!(delivered.len(), packets.len());
        assert!(delivered == packets, "packets arrive in write order");
    }

    /// The control for the test above: the same load with one packet's
    /// three fragments written in reverse, the shuffle WriteValue's
    /// per-write tasks could produce, is an abandonment the counter
    /// sees. Without this the clean count above could be a counter that
    /// never fires.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shuffled_packet_under_the_same_load_is_a_counted_abandon() {
        let (packets, mut writes) = load_writes();
        // Packet 2 (len 400) is three fragments; reverse them.
        let first =
            1 + fragment_packet(&packets[0], MTU).len() + fragment_packet(&packets[1], MTU).len();
        assert_eq!(fragment_packet(&packets[2], MTU).len(), 3);
        writes[first..first + 3].reverse();

        let (delivered, abandoned) = deliver_under_load(writes).await;
        assert!(!abandoned.is_empty(), "the shuffle is counted");
        assert!(!delivered.contains(&packets[2]), "and the packet is lost");
    }

    /// A discovery that hears nothing and records when each run started
    /// and ended, on the paused test clock.
    struct CountingDiscovery {
        t0: tokio::time::Instant,
        edges: std::sync::Arc<std::sync::Mutex<Vec<(bool, u64)>>>,
    }

    impl Discovery for CountingDiscovery {
        type Item = ();

        async fn start(&mut self) -> Result<BoxStream<'static, ()>, RunFailed> {
            Ok(futures::stream::pending().boxed())
        }

        async fn sighting(&mut self, _: ()) -> bool {
            true
        }

        async fn run_edge(&mut self, on: bool) -> bool {
            let ms = self.t0.elapsed().as_millis() as u64;
            self.edges.lock().expect("edges").push((on, ms));
            true
        }
    }

    /// Run the cadence for `ms` on the paused clock, with `script`
    /// setting the hold at the given times, and return every run edge.
    async fn cadence_edges(ms: u64, script: &[(u64, bool)]) -> Vec<(bool, u64)> {
        let t0 = tokio::time::Instant::now();
        let edges = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (hold_tx, hold_rx) = watch::channel(false);
        let task = tokio::spawn(scan_cadence(
            CountingDiscovery {
                t0,
                edges: edges.clone(),
            },
            hold_rx,
            Duration::from_secs(5),
        ));
        for &(at, held) in script {
            tokio::time::sleep_until(t0 + Duration::from_millis(at)).await;
            hold_tx.send_replace(held);
        }
        tokio::time::sleep_until(t0 + Duration::from_millis(ms)).await;
        task.abort();
        let edges = edges.lock().expect("edges").clone();
        edges
    }

    /// 757 (c): with the dial queue empty the scanner keeps the cadence
    /// it had before 757: a 2 s window every 7 s, off in between. Over a
    /// quiet minute that is runs at 0, 7, ..., 56 s: nine starts, each
    /// stopped 2 s later.
    #[tokio::test(start_paused = true)]
    async fn with_the_queue_empty_discovery_is_off_between_windows_as_before() {
        let edges = cadence_edges(60_000, &[]).await;
        let starts: Vec<u64> = edges.iter().filter(|(on, _)| *on).map(|e| e.1).collect();
        let stops: Vec<u64> = edges.iter().filter(|(on, _)| !*on).map(|e| e.1).collect();
        let cycle = (SCAN_WINDOW + Duration::from_secs(5)).as_millis() as u64;
        assert_eq!(starts, (0..9).map(|i| i * cycle).collect::<Vec<_>>());
        assert_eq!(
            stops,
            (0..9)
                .map(|i| i * cycle + SCAN_WINDOW.as_millis() as u64)
                .collect::<Vec<_>>(),
            "discovery is off between windows"
        );
    }

    /// 757: a dial queued in the pause starts a run at once, which ends
    /// when the queue drains; a dial queued while a window runs keeps
    /// it on past its end. Either way the full pause follows.
    #[tokio::test(start_paused = true)]
    async fn a_waiting_dial_holds_the_scanner_on_until_the_queue_drains() {
        // Horizons sit clear of the next run edge: an abort due at the
        // same paused instant as a start would race it.
        let edges = cadence_edges(
            19_000,
            &[
                (3_000, true),
                (4_500, false),
                (12_500, true),
                (15_000, false),
            ],
        )
        .await;
        assert_eq!(
            edges,
            vec![
                (true, 0),
                (false, 2_000),
                // Held in the pause: on at once, off with the hold.
                (true, 3_000),
                (false, 4_500),
                // The full pause after it.
                (true, 9_500),
                (false, 11_500),
                // Held in the next pause the same way.
                (true, 12_500),
                (false, 15_000),
            ]
        );
        let edges = cadence_edges(10_000, &[(1_000, true), (3_500, false)]).await;
        assert_eq!(
            edges,
            vec![(true, 0), (false, 3_500), (true, 8_500)],
            "a window held past its end stops when the hold clears"
        );
    }

    /// 732: a dial to a peer that went dark (its incoming slots filled
    /// after the sighting that chose it) gives up at the connect bound,
    /// as a `Transport` failure, instead of holding the adapter for the
    /// whole setup budget.
    #[tokio::test(start_paused = true)]
    async fn a_connect_nothing_answers_gives_up_at_the_connect_bound() {
        let start = tokio::time::Instant::now();
        let outcome = tokio::time::timeout(
            SETUP_TIMEOUT,
            bounded_connect("ble0", ADDR, std::future::pending(), async { true }),
        )
        .await;
        let Ok(Err(e)) = outcome else {
            panic!("the connect bound must end the dial before the setup budget");
        };
        assert_eq!(e.failure, DialFailure::Transport);
        assert_eq!(start.elapsed(), CONNECT_TIMEOUT);
        assert!(CONNECT_TIMEOUT < SETUP_TIMEOUT);
    }

    /// 758: a connect nothing answered logs one `BLE_CONNECT_UNANSWERED`
    /// with the bound it waited, and a connect answered inside the bound
    /// logs none.
    #[tokio::test(start_paused = true)]
    async fn an_unanswered_connect_logs_one_event() {
        let (buf, _guard) = crate::test_support::log_capture::capture_logs();
        let _ = bounded_connect("ble0", ADDR, std::future::pending(), async { true }).await;
        let _ = bounded_connect("ble0", ADDR, async { Ok(()) }, async { true }).await;
        let text = crate::test_support::log_capture::captured(&buf);
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| l.contains("BLE_CONNECT_UNANSWERED"))
            .collect();
        assert_eq!(lines.len(), 1, "{text}");
        for field in ["iface=ble0", "addr=c00000000001", "after_ms=5000"] {
            assert!(lines[0].contains(field), "{field} missing: {}", lines[0]);
        }
    }

    /// 758: a setup that outlives its budget logs one `BLE_SETUP_TIMEOUT`
    /// with the budget, and a setup that finishes inside it logs none.
    #[tokio::test(start_paused = true)]
    async fn a_setup_past_its_budget_logs_one_event() {
        let (buf, _guard) = crate::test_support::log_capture::capture_logs();
        let start = tokio::time::Instant::now();
        assert!(bounded_setup("ble0", ADDR, std::future::pending::<()>())
            .await
            .is_none());
        assert_eq!(start.elapsed(), SETUP_TIMEOUT);
        assert_eq!(bounded_setup("ble0", ADDR, async { 7 }).await, Some(7));
        let text = crate::test_support::log_capture::captured(&buf);
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| l.contains("BLE_SETUP_TIMEOUT"))
            .collect();
        assert_eq!(lines.len(), 1, "{text}");
        for field in ["iface=ble0", "addr=c00000000001", "after_ms=20000"] {
            assert!(lines[0].contains(field), "{field} missing: {}", lines[0]);
        }
    }

    /// 732: the bound does not cut a connect the peer answers inside it,
    /// however late in the budget.
    #[tokio::test(start_paused = true)]
    async fn a_connect_answered_inside_the_bound_proceeds() {
        let late = CONNECT_TIMEOUT - Duration::from_millis(100);
        let connect = async move {
            tokio::time::sleep(late).await;
            Ok(())
        };
        assert!(bounded_connect("ble0", ADDR, connect, async { true })
            .await
            .is_ok());
    }
}
