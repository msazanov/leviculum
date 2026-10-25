//! The seams between the orchestration and a chip family's backends.
//!
//! One declaration per row of the seam table in
//! `docs/src/concepts/firmware-layers.md`, "The seams, and their shape".
//! Two shapes: a **message seam** where the backend is a task the
//! orchestration talks to over channels it owns (Transceiver, BLE link), and
//! a **trait seam** where the backend is called or awaited once (the rest).
//! Where an ecosystem trait exists and has two implementers, the seam is
//! that trait, re-exported (Flash, Entropy); a project trait is written only
//! where none fits.
//!
//! Step 0 declares them empty. The methods, and for the message seams the
//! message variants, arrive with the step that cuts each seam: Flash,
//! Entropy, Clock, Watchdog and Sinks in step 1 (Sinks has its methods
//! since 973, Clock its monotonic half and ButtonInput its two since 976),
//! the control port in step
//! 2, the transceiver in step 3, the BLE link in step 4, the sensors in
//! step 5. Rules for all of them: no `dyn`, no per-chip `cfg`, and a host
//! fake for every seam.
//!
//! Each doc comment names the seam's implementer in this tree today and the
//! one in Cedric Rivard's fork (`cedrivard/Leviculum`, branch
//! `rp2350-m9-net` at `7a699446`), the second implementer the cut is for.

/// Flash: the record log, the propagation store, settings, identity, the
/// boot count and the time floor sit on it.
///
/// The seam is `embedded_storage_async::nor_flash::MultiwriteNorFlash`; the
/// `pn-store` regions and `record-log` stay on top as policy.
///
/// Today: `nrf_softdevice::Flash` for internal flash (`record_store.rs`,
/// `flash.rs`, `time_floor.rs` through `SharedFlash`), the QSPI part
/// (`qspi.rs`), and `boot_count.rs` over the NVMC. The fork: its
/// `leviculum-fw/src/record_store.rs` already binds this trait (lines 57,
/// 268 and 323) over RP2350 flash and ESP32 flash, and `leviculum-sd-store`
/// on a card.
pub use embedded_storage_async::nor_flash::MultiwriteNorFlash as Flash;

/// Entropy: what core's crypto draws.
///
/// The seam is `rand_core::RngCore`. Today: `rng.rs`, over the SoftDevice's
/// `sd_rand_application_bytes_available_get`. The fork: the ESP32 and
/// RP2350 hardware RNGs.
pub use rand_core::RngCore as Entropy;

/// Placeholder for the commands the orchestration sends a transceiver
/// backend (bring-up, configure, CAD, TX, RX arming). Variants arrive in
/// step 3, with the type set `Radio`/`RxStatus`/`Error`/`FrontEnd` of
/// `radio.rs` as the vocabulary.
#[derive(Debug)]
pub enum TransceiverCommand {}

/// Placeholder for what a transceiver backend reports (frames, IRQ facts,
/// CAD results). Variants arrive in step 3.
#[derive(Debug)]
pub enum TransceiverEvent {}

/// Transceiver, a message seam: a backend task that consumes
/// [`TransceiverCommand`] and produces [`TransceiverEvent`] on channels this
/// crate owns.
///
/// Today: `sx1262.rs` over core's `RegisterBus`/`CommandBus`, and
/// `leviculum-lr11xx` with the same method set name for name (`radio.rs`
/// selects one by cargo feature). The fork has no transceiver seam: its
/// `leviculum-fw/src/lora.rs` names `leviculum_lr11xx::Lr11xx` directly and
/// its Heltec SX1262 path is a separate `leviculum-esp`; the seam is ours to
/// cut.
pub trait Transceiver {}

/// Control port: the framed host link (bytes, and the DTR/RTS or XON/XOFF
/// flow it carries), under a control-frame executor that moves into this
/// crate in step 2.
///
/// Today: `usb.rs` over `embassy_usb` CDC, which is executor and port in
/// one (`ControlAction` dispatch). The fork: its `ControlPort` in
/// `leviculum-fw/src/control.rs` (lines 166 to 188), over `serial-mux` on
/// the M9's single UART and over RP2350 USB CDC. The fork's cut is adopted.
pub trait ControlPort {}

/// Placeholder for what the orchestration asks of a BLE backend (outbound
/// notifies, advertising on and off). Variants arrive in step 4.
#[derive(Debug)]
pub enum BleLinkCommand {}

/// Placeholder for what a BLE backend reports (connection up and down with
/// MTU and parameters, inbound writes). Variants arrive in step 4.
#[derive(Debug)]
pub enum BleLinkEvent {}

/// BLE link, a message seam above GATT: the Columba link as the
/// orchestration sees it, [`BleLinkCommand`] in and [`BleLinkEvent`] out.
///
/// Today: the S140 SoftDevice backend in `ble/` (`ble/mod.rs`,
/// `ble/columba.rs`), whose upward calls into `media`, `boot_trace`, `name`
/// and `iface_bytes` become these messages. The fork: TrouBLE over
/// `bt_hci::Transport` (`leviculum-fw/src/ble/mod.rs`, lines 81 to 91) on
/// the CYW43439 and the ESP32-S3 radio; its `HciTransport` stays inside its
/// backend, because the SoftDevice is not an HCI controller.
pub trait BleLink {}

/// Clock: monotonic time, and the calendar sources with their rank.
///
/// Today: embassy time, GNSS time (`gnss-time`), host time over the control
/// port. The fork: the same, plus SNTP (`TimeSource::Ntp`) and the PCF8563
/// (`rtc-pcf8563`).
///
/// The monotonic half has its methods since 976, for the button: a reading
/// and a wait for a moment. Both futures here and in [`ButtonInput`] are
/// spelled `impl Future` rather than `async fn`, which a public trait may
/// not use without a warning about its `Send` bound. The implementation in
/// this tree is [`crate::clock::EmbassyClock`], over embassy time on every
/// family; the calendar sources are still the `clock` module's free
/// functions.
pub trait Clock {
    /// Milliseconds since boot.
    fn now_ms(&self) -> u64;

    /// Wait until [`now_ms`](Self::now_ms) has reached `at_ms`; at once if
    /// it already has.
    fn wait_until(&self, at_ms: u64) -> impl core::future::Future<Output = ()>;
}

/// Sensors: GNSS UART, motion, battery, die temperature, later a
/// magnetometer. One trait per sensor once step 5 cuts them.
///
/// Today: the AG3335 (`gnss.rs`), the QMA6100P (`motion.rs`), the ADC
/// divider (`battery.rs`), `nrf_softdevice::temperature_celsius`
/// (`telemetry.rs`). The fork: the AG3352 on the same Airoha init
/// (`gnss-init`) and the MAX17048 (`battery-scale`).
pub trait Sensors {}

/// Button input: the board's one user button as the press handling
/// ([`crate::button`]) reads it, a level and a wait for its next edge.
/// Nothing about pins, pulls or which level a press drives: the
/// implementation folds the wiring into [`is_pressed`](Self::is_pressed).
///
/// Today (976): `leviculum_nrf::button::NrfButton`, over
/// `embassy_nrf::gpio::Input` and the board's `active_high`. The fork: the
/// M9's keypad, once its keys are inputs of their own.
pub trait ButtonInput {
    /// Wait for the next edge, either way. A wait that arms after the
    /// level changed may only wake on the way back, so the caller reads
    /// [`is_pressed`](Self::is_pressed) right before it.
    fn wait_for_edge(&mut self) -> impl core::future::Future<Output = ()>;

    /// Whether the button is down now.
    fn is_pressed(&self) -> bool;
}

/// Sinks: the feedback players, the LED, the notification LED and the
/// buzzer, as the feedback policy ([`crate::feedback`]) drives them. The
/// display, the debug log and the post-mortem store are sinks of their own,
/// each its own trait once its cut comes; the button is an input
/// ([`ButtonInput`]) since 976.
///
/// A player owns its pin and plays a [`leviculum_feedback::Pattern`] to its
/// end, starting over with a newer one raised meanwhile, and writes the
/// `FEEDBACK pattern=<name> sink=<sink>` line when it starts one: so all the
/// policy hands down is which pattern, on which sink, and all it asks is
/// whether the board has that sink.
///
/// Today (973): `leviculum_nrf::feedback::NrfSinks`, over the players
/// `led_task` and `buzzer_task` in the bind crate's `feedback.rs`. The
/// fork: the M9's LED and buzzer.
pub trait Sinks {
    /// Whether the board has `sink`; a sink it lacks is never asked to
    /// play.
    fn present(&self, sink: crate::feedback::Sink) -> bool;

    /// Play `pattern` on `sink`, replacing whatever it is playing.
    fn play(&self, sink: crate::feedback::Sink, pattern: leviculum_feedback::Pattern);
}

/// Watchdog: feed and timeout.
///
/// Today: none; nothing in `leviculum-nrf/src` feeds the nRF52840 WDT.
/// The fork: per family. The seam is declared so that the first board that
/// arms one does it behind this trait.
pub trait Watchdog {}
