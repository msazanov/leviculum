//! The driver's command sequences, replayed against a scripted bus.
//!
//! No board: the SPI device records transactions and answers scripted reads.
//! BUSY is low; IRQ can be immediate or controlled by the test. Every case
//! holds the mock clock lock, and timed cases explicitly advance it while
//! polling the real driver future. The assertions cover command order, bytes,
//! receive ownership and deadlines, not a second implementation of the policy.

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};

use embassy_time::{Duration, Instant, MockDriver};
use embedded_hal::digital::{ErrorType as DigitalErrorType, InputPin, OutputPin};
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::{ErrorType as SpiErrorType, Operation, SpiDevice};
use leviculum_lr11xx::board::LR1110MB1DXS;
use leviculum_lr11xx::cmd::{irq, op, CmdStatus};
use leviculum_lr11xx::{ChannelSample, Chip, Error, Lr11xx, RxStatus};
use leviculum_rx_arming::{DeferPolicy, FrameSink, RxWindowProbe};

static CLOCK: Mutex<()> = Mutex::new(());

std::thread_local! {
    static SAMPLES: RefCell<Vec<ChannelSample>> = const { RefCell::new(Vec::new()) };
}

fn test_clock() -> MutexGuard<'static, ()> {
    let clock = CLOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    MockDriver::get().reset();
    SAMPLES.with(|samples| samples.borrow_mut().clear());
    clock
}

fn capture_sample(sample: ChannelSample) {
    SAMPLES.with(|samples| samples.borrow_mut().push(sample));
}

fn samples() -> Vec<ChannelSample> {
    SAMPLES.with(|samples| samples.borrow().clone())
}

fn poll_once<F: Future>(f: core::pin::Pin<&mut F>) -> Poll<F::Output> {
    f.poll(&mut Context::from_waker(core::task::Waker::noop()))
}

fn block_on<F: Future>(f: F) -> F::Output {
    match poll_once(pin!(f)) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("advance the mock clock or IRQ before polling this future"),
    }
}

fn advance(ms: u64) {
    MockDriver::get().advance(Duration::from_millis(ms));
}

/// One transaction as the fake saw it: the bytes written, concatenated
/// across the transaction's write operations.
type Written = Vec<u8>;

#[derive(Default)]
struct BusState {
    written: Vec<Written>,
    /// Bytes to answer the next transfer operations with, in order. A
    /// transaction with `Transfer` ops pops one entry per op.
    answers: VecDeque<Result<Vec<u8>, Never>>,
    /// Time consumed by each read transaction, for deadline accounting.
    read_delay_ms: u64,
}

#[derive(Clone, Default)]
struct FakeSpi(Rc<RefCell<BusState>>);

#[derive(Debug)]
struct Never;
impl embedded_hal::spi::Error for Never {
    fn kind(&self) -> embedded_hal::spi::ErrorKind {
        embedded_hal::spi::ErrorKind::Other
    }
}
impl embedded_hal::digital::Error for Never {
    fn kind(&self) -> embedded_hal::digital::ErrorKind {
        embedded_hal::digital::ErrorKind::Other
    }
}

impl SpiErrorType for FakeSpi {
    type Error = Never;
}

impl SpiDevice for FakeSpi {
    async fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), Never> {
        let mut st = self.0.borrow_mut();
        if operations
            .iter()
            .any(|opn| matches!(opn, Operation::Transfer(_, _)))
        {
            advance(st.read_delay_ms);
        }
        let mut wrote = Vec::new();
        for opn in operations.iter_mut() {
            match opn {
                Operation::Write(b) => wrote.extend_from_slice(b),
                Operation::Transfer(rx, tx) => {
                    // NOPs only on the read leg: the HAL contract.
                    assert!(tx.iter().all(|&b| b == 0), "non-NOP bytes while reading");
                    let answer = st
                        .answers
                        .pop_front()
                        .expect("scripted answer for a transfer")?;
                    assert_eq!(answer.len(), rx.len(), "answer length");
                    rx.copy_from_slice(&answer);
                }
                other => panic!("unexpected op {:?}", core::mem::discriminant(other)),
            }
        }
        st.written.push(wrote);
        Ok(())
    }
}

struct Pin;
impl DigitalErrorType for Pin {
    type Error = Never;
}
impl OutputPin for Pin {
    fn set_low(&mut self) -> Result<(), Never> {
        Ok(())
    }
    fn set_high(&mut self) -> Result<(), Never> {
        Ok(())
    }
}
impl InputPin for Pin {
    fn is_high(&mut self) -> Result<bool, Never> {
        Ok(false)
    }
    fn is_low(&mut self) -> Result<bool, Never> {
        Ok(true)
    }
}
impl Wait for Pin {
    async fn wait_for_high(&mut self) -> Result<(), Never> {
        Ok(())
    }
    async fn wait_for_low(&mut self) -> Result<(), Never> {
        Ok(())
    }
    async fn wait_for_rising_edge(&mut self) -> Result<(), Never> {
        Ok(())
    }
    async fn wait_for_falling_edge(&mut self) -> Result<(), Never> {
        Ok(())
    }
    async fn wait_for_any_edge(&mut self) -> Result<(), Never> {
        Ok(())
    }
}

/// A level-sensitive IRQ which stays pending until the test raises it.
#[derive(Clone, Default)]
struct IrqPin(Rc<Cell<bool>>);

impl IrqPin {
    fn high(&self) {
        self.0.set(true);
    }
}

impl DigitalErrorType for IrqPin {
    type Error = Never;
}

impl InputPin for IrqPin {
    fn is_high(&mut self) -> Result<bool, Never> {
        Ok(self.0.get())
    }

    fn is_low(&mut self) -> Result<bool, Never> {
        Ok(!self.0.get())
    }
}

impl Wait for IrqPin {
    async fn wait_for_high(&mut self) -> Result<(), Never> {
        core::future::poll_fn(|_| {
            if self.0.get() {
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            }
        })
        .await
    }

    async fn wait_for_low(&mut self) -> Result<(), Never> {
        core::future::poll_fn(|_| {
            if self.0.get() {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        })
        .await
    }

    async fn wait_for_rising_edge(&mut self) -> Result<(), Never> {
        self.wait_for_low().await?;
        self.wait_for_high().await
    }

    async fn wait_for_falling_edge(&mut self) -> Result<(), Never> {
        self.wait_for_high().await?;
        self.wait_for_low().await
    }

    async fn wait_for_any_edge(&mut self) -> Result<(), Never> {
        let before = self.0.get();
        core::future::poll_fn(|_| {
            if self.0.get() == before {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        })
        .await
    }
}

type ControlledRadio = Lr11xx<FakeSpi, Pin, Pin, IrqPin>;

fn controlled_radio() -> (ControlledRadio, FakeSpi, IrqPin) {
    let spi = FakeSpi::default();
    let irq = IrqPin::default();
    (
        Lr11xx::new(spi.clone(), Pin, Pin, irq.clone(), &LR1110MB1DXS),
        spi,
        irq,
    )
}

type Radio = Lr11xx<FakeSpi, Pin, Pin, Pin>;

fn radio() -> (Radio, FakeSpi) {
    let spi = FakeSpi::default();
    (Lr11xx::new(spi.clone(), Pin, Pin, Pin, &LR1110MB1DXS), spi)
}

fn opcodes(spi: &FakeSpi) -> Vec<u16> {
    spi.0
        .borrow()
        .written
        .iter()
        .filter(|w| w.len() >= 2)
        .map(|w| u16::from_be_bytes([w[0], w[1]]))
        .collect()
}

fn answer(spi: &FakeSpi, bytes: &[u8]) {
    spi.0.borrow_mut().answers.push_back(Ok(bytes.to_vec()));
}

fn fail_transfer(spi: &FakeSpi) {
    spi.0.borrow_mut().answers.push_back(Err(Never));
}

/// A direct status read: stat1 ok, stat2 standby-rc, the given irq word.
fn status(spi: &FakeSpi, irq: u32) {
    let i = irq.to_be_bytes();
    answer(spi, &[0x04, 0x02, i[0], i[1], i[2], i[3]]);
}

/// A two-transaction read answers a status byte then the data.
fn read(spi: &FakeSpi, data: &[u8]) {
    answer(spi, &[0x04]);
    answer(spi, data);
}

fn frame_reads(spi: &FakeSpi, bytes: &[u8]) {
    read(spi, &[bytes.len() as u8, 3]);
    read(spi, bytes);
    read(spi, &[160, 20, 0]);
}

/// The command transactions, in order; a read's second (transfer-only)
/// transaction writes nothing and is left out, so an index here is a
/// command index.
fn written(spi: &FakeSpi) -> Vec<Written> {
    spi.0
        .borrow()
        .written
        .iter()
        .filter(|w| !w.is_empty())
        .cloned()
        .collect()
}

#[derive(Default)]
struct CapturedFrames(Vec<(Vec<u8>, i16, i16)>);

impl FrameSink for CapturedFrames {
    type Meta = RxStatus;

    async fn deliver(&mut self, bytes: &[u8], meta: &RxStatus) {
        self.0.push((bytes.to_vec(), meta.rssi, meta.snr));
    }
}

#[test]
fn init_runs_semtechs_system_then_radio_sequence() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    read(&spi, &[0x22, 0x01, 0x03, 0x08]); // GetVersion: LR1110, fw 0x0308
    read(&spi, &[0x00, 0x00]); // GetErrors: clean
    let v = block_on(r.init_radio()).expect("init");
    assert_eq!((v.hw, v.kind, v.fw), (0x22, 0x01, 0x0308));
    assert_eq!(
        opcodes(&spi),
        vec![
            op::GET_VERSION,
            op::SET_REG_MODE,
            op::SET_DIO_AS_RF_SWITCH,
            op::SET_TCXO_MODE,
            op::CFG_LF_CLK,
            op::CLEAR_ERRORS,
            op::CALIBRATE,
            op::GET_ERRORS,
            op::CLEAR_ERRORS,
            op::CLEAR_IRQ,
            op::SET_PKT_TYPE,
            op::SET_RX_TX_FALLBACK_MODE,
            op::SET_RX_BOOSTED,
            op::SET_LORA_PUBLIC_NETWORK,
        ]
    );
    let w = written(&spi);
    assert_eq!(w[1][2..], [0x01]);
    assert_eq!(w[2][2..], [0x07, 0x00, 0x01, 0x03, 0x02, 0x00, 0x04, 0x00]);
    assert_eq!(w[3][2..], [0x06, 0x00, 0x01, 0x2C]);
    assert_eq!(w[4][2..], [0x05]);
    assert_eq!(w[6][2..], [0x3F]);
    assert_eq!(w[9][2..], [0xFF; 4]);
    assert_eq!(w[10][2..], [0x02]);
    assert_eq!(w[11][2..], [0x01]);
    assert_eq!(w[12][2..], [0x00]);
    assert_eq!(w[13][2..], [0x00]);
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn a_bootloader_is_refused_before_anything_is_programmed() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    read(&spi, &[0x22, 0xDF, 0x01, 0x00]);
    match block_on(r.init_radio()) {
        Err(Error::Bootloader {
            hw: 0x22,
            fw: 0x0100,
        }) => {}
        other => panic!("expected Bootloader, got {:?}", other.map(|_| ())),
    }
    assert_eq!(opcodes(&spi), vec![op::GET_VERSION]);
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn another_lr11xx_than_the_board_names_is_refused() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    read(&spi, &[0x22, 0x03, 0x01, 0x00]); // LR1121 on an LR1110 board
    match block_on(r.init_radio()) {
        Err(Error::WrongChip {
            expected: Chip::Lr1110,
            hw: 0x22,
            kind: 0x03,
            fw: 0x0100,
        }) => {}
        other => panic!("expected WrongChip, got {:?}", other.map(|_| ())),
    }
    assert_eq!(opcodes(&spi), vec![op::GET_VERSION]);
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn a_failed_version_command_is_refused() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    answer(&spi, &[0x02]); // Stat1: Perr
    answer(&spi, &[0x22, 0x01, 0x03, 0x03]);
    match block_on(r.init_radio()) {
        Err(Error::Command(CmdStatus::Perr)) => {}
        other => panic!("expected Command(Perr), got {:?}", other.map(|_| ())),
    }
    assert_eq!(opcodes(&spi), vec![op::GET_VERSION]);
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn a_calibration_error_stops_init() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    read(&spi, &[0x22, 0x01, 0x03, 0x03]);
    read(&spi, &[0x00, 0x20]); // HF_XOSC_START
    match block_on(r.init_radio()) {
        Err(Error::Calibration { errors: 0x0020 }) => {}
        other => panic!("expected Calibration, got {:?}", other.map(|_| ())),
    }
    assert_eq!(*opcodes(&spi).last().unwrap(), op::GET_ERRORS);
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn an_absent_transceiver_is_refused_before_anything_is_programmed() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    read(&spi, &[0x00, 0x00, 0x00, 0x00]); // nothing on MISO
    match block_on(r.init_radio()) {
        Err(Error::NoRadio {
            hw: 0,
            kind: 0,
            fw: 0,
        }) => {}
        other => panic!("expected NoRadio, got {:?}", other.map(|_| ())),
    }
    assert_eq!(
        opcodes(&spi),
        vec![op::GET_VERSION],
        "nothing after the version check"
    );
    // Negative control: all-ones (a floating MISO) is refused too.
    let (mut r, spi) = radio();
    read(&spi, &[0xFF; 4]);
    assert!(matches!(
        block_on(r.init_radio()),
        Err(Error::NoRadio { .. })
    ));
}

#[test]
fn configure_programs_frequency_pa_modulation_and_packet_params() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    // No window standing, so the disarm reads nothing.
    let p = block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 22, 18)).expect("configure");
    assert_eq!(
        opcodes(&spi),
        vec![
            op::CALIBRATE_IMAGE,
            op::SET_RF_FREQUENCY,
            op::SET_RSSI_CALIBRATION,
            op::SET_PA_CFG,
            op::SET_TX_PARAMS,
            op::SET_MODULATION_PARAMS,
            op::SET_PKT_PARAMS,
            op::STOP_TIMEOUT_ON_PREAMBLE,
        ]
    );
    let w = written(&spi);
    assert_eq!(w[0][2..], [215, 218]);
    assert_eq!(w[1][2..], 869_463_000u32.to_be_bytes());
    assert_eq!(
        w[2][2..],
        [0x22, 0x32, 0x43, 0x45, 0x64, 0x55, 0x66, 0x76, 0x06, 0x00, 0x00]
    );
    assert_eq!(
        w[3][2..],
        [0x01, 0x01, 0x04, 0x07],
        "22 dBm row: HP, VBAT, duty 4, hp 7"
    );
    assert_eq!(w[4][2..], [22, 0x02]);
    assert_eq!(w[5][2..], [8, 0x04, 0x01, 0x00], "SF8 BW125 CR4/5, no LDRO");
    assert_eq!(w[6][2..], [0x00, 18, 0x00, 0xFF, 0x01, 0x00]);
    assert_eq!(w[7][2..], [0x01]);
    assert_eq!(p.programmed_dbm, 22);
    assert!(!p.clamped);
    assert_eq!(p.pa_config, [0x01, 0x01, 0x04, 0x07]);
}

#[test]
fn configure_clamps_an_out_of_range_power_and_says_so() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    let p = block_on(r.configure_lora(869_463_000, 12, 0x04, 0x04, 30, 18)).expect("configure");
    assert!(p.clamped);
    assert_eq!(p.programmed_dbm, 22);
    let w = written(&spi);
    // SF12 at BW125 turns LDRO on, as core decides.
    assert_eq!(w[5][2..], [12, 0x04, 0x04, 0x01]);
}

#[test]
fn image_calibration_runs_once_per_band() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    for _ in 0..2 {
        block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    }
    let count = |spi: &FakeSpi| {
        opcodes(spi)
            .iter()
            .filter(|&&o| o == op::CALIBRATE_IMAGE)
            .count()
    };
    assert_eq!(count(&spi), 1);
    spi.0.borrow_mut().written.clear();
    block_on(r.configure_lora(915_000_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    assert_eq!(count(&spi), 1);
    assert_eq!(written(&spi)[0], [0x01, 0x11, 225, 232]);
}

#[test]
fn an_oversized_payload_is_refused_before_the_bus() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    match block_on(r.transmit(&[0; 256], 1000)) {
        Err(Error::PayloadTooLong { len: 256 }) => {}
        other => panic!("expected PayloadTooLong, got {other:?}"),
    }
    assert!(spi.0.borrow().written.is_empty());
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn a_frame_larger_than_the_buffer_is_not_read_out() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    block_on(r.arm_rx(500, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    spi.0.borrow_mut().written.clear();
    status(&spi, irq::RX_DONE);
    read(&spi, &[20, 0]);
    let mut buf = [0u8; 8];
    match block_on(r.await_rx(&mut buf)) {
        Err(Error::BufferTooSmall { len: 20 }) => {}
        other => panic!("expected BufferTooSmall, got {:?}", other.map(|_| ())),
    }
    assert_eq!(opcodes(&spi), vec![op::CLEAR_IRQ, op::GET_RX_BUFFER_STATUS]);
    assert!(r.standing_window().is_none());
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn transmit_writes_the_payload_and_keys_the_radio() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    spi.0.borrow_mut().written.clear();
    status(&spi, irq::TX_DONE); // the IRQ rose: TxDone
    block_on(r.transmit(b"hello", 1000)).expect("tx");
    assert_eq!(
        opcodes(&spi),
        vec![
            op::SET_PKT_PARAMS,
            op::SET_DIO_IRQ_PARAMS,
            op::CLEAR_IRQ,
            op::WRITE_BUFFER8,
            op::SET_TX,
            op::CLEAR_IRQ,
        ]
    );
    let w = written(&spi);
    assert_eq!(
        w[0][2..],
        [0x00, 18, 0x00, 5, 0x01, 0x00],
        "payload length 5"
    );
    assert_eq!(
        w[1][2..],
        irq::DIO_TX
            .to_be_bytes()
            .iter()
            .chain([0u8; 4].iter())
            .copied()
            .collect::<Vec<_>>()
    );
    assert_eq!(&w[3][2..], b"hello");
    assert_eq!(w[4][2..], [0, 0, 0], "no hardware timeout");
}

#[test]
fn transmit_without_txdone_is_a_timeout_and_a_standby() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    spi.0.borrow_mut().written.clear();
    status(&spi, 0); // the pin rose but nothing is latched
    assert!(matches!(
        block_on(r.transmit(b"x", 1000)),
        Err(Error::Timeout)
    ));
    assert_eq!(*opcodes(&spi).last().unwrap(), op::SET_STANDBY);
}

#[test]
fn a_receive_after_a_transmit_reopens_the_payload_length() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    status(&spi, irq::TX_DONE);
    block_on(r.transmit(b"hello", 1000)).expect("tx");
    spi.0.borrow_mut().written.clear();
    block_on(r.arm_rx(500, leviculum_core::sx126x::RxSite::Ack)).expect("arm");
    assert_eq!(
        opcodes(&spi),
        vec![
            op::SET_PKT_PARAMS,
            op::SET_DIO_IRQ_PARAMS,
            op::CLEAR_IRQ,
            op::SET_RX
        ]
    );
    assert_eq!(
        written(&spi)[0][2..],
        [0x00, 18, 0x00, 0xFF, 0x01, 0x00],
        "an explicit header longer than the last frame must not fail"
    );
}

#[test]
fn a_receive_window_arms_waits_and_reads_the_frame_out() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    spi.0.borrow_mut().written.clear();
    block_on(r.arm_rx(500, leviculum_core::sx126x::RxSite::Ack)).expect("arm");
    assert_eq!(
        opcodes(&spi),
        vec![op::SET_DIO_IRQ_PARAMS, op::CLEAR_IRQ, op::SET_RX]
    );
    let w = written(&spi);
    assert_eq!(
        w[2][2..],
        leviculum_lr11xx::cmd::set_rx(leviculum_lr11xx::cmd::ms_to_rtc_steps(500))
    );
    spi.0.borrow_mut().written.clear();
    // The IRQ rose: RxDone, 4 bytes at offset 3, RSSI -80 SNR 5.
    status(&spi, irq::RX_DONE);
    read(&spi, &[4, 3]);
    read(&spi, b"abcd");
    read(&spi, &[160, 20, 0]);
    let mut buf = [0u8; 255];
    let (len, st) = block_on(r.await_rx(&mut buf)).expect("rx");
    assert_eq!((len, &buf[..4]), (4, &b"abcd"[..]));
    assert_eq!((st.rssi, st.snr), (-80, 5));
    assert_eq!(
        opcodes(&spi),
        vec![
            op::CLEAR_IRQ,
            op::GET_RX_BUFFER_STATUS,
            op::READ_BUFFER8,
            op::GET_PKT_STATUS
        ]
    );
    assert_eq!(
        written(&spi)[2][2..],
        [3, 4],
        "read from the buffer start the chip named"
    );
}

/// A window whose expiry licenses a transmit — jitter or CSMA backoff — runs
/// the preamble detector's settle past its asked bound, so a preamble that
/// begins inside the window's last detector horizon latches and stops the
/// hardware timer instead of dying with it (929, #466: both witnessed
/// t1000e_rnode_pair losses were jitter timeouts 11–17 ms into the lost
/// probe's preamble). The ack window two tests up is the control: it stays at
/// exactly its asked bound.
#[test]
fn a_tx_gating_window_runs_the_detector_settle_past_its_bound() {
    for site in [
        leviculum_core::sx126x::RxSite::Jitter,
        leviculum_core::sx126x::RxSite::Csma,
    ] {
        let _clock = test_clock();
        let (mut r, spi) = radio();
        block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
        spi.0.borrow_mut().written.clear();
        block_on(r.arm_rx(500, site)).expect("arm");
        let settle =
            leviculum_core::sx126x::latch_settle_ms(125_000, 8).expect("a configured radio");
        let w = written(&spi);
        assert_eq!(
            w.last().expect("an arm writes commands")[2..],
            leviculum_lr11xx::cmd::set_rx(leviculum_lr11xx::cmd::ms_to_rtc_steps(
                500 + settle as u32
            )),
            "site {} must pad its hardware bound by the settle",
            site.tag()
        );
    }
}

#[test]
fn a_crc_failure_is_measured_and_the_payload_left_in_the_chip() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    block_on(r.arm_rx(500, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    spi.0.borrow_mut().written.clear();
    status(&spi, irq::RX_DONE | irq::CRC_ERROR);
    read(&spi, &[77, 0]);
    read(&spi, &[200, 0xF8, 0]);
    let mut buf = [0u8; 255];
    match block_on(r.await_rx(&mut buf)) {
        Err(Error::Crc(f)) => assert_eq!((f.len, f.rssi, f.snr), (77, -100, -2)),
        other => panic!("expected Crc, got {:?}", other.map(|_| ())),
    }
    assert!(
        !opcodes(&spi).contains(&op::READ_BUFFER8),
        "a corrupt payload is not read out"
    );
}

#[test]
fn a_hardware_timeout_ends_the_window_without_a_standby() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    block_on(r.arm_rx(500, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    spi.0.borrow_mut().written.clear();
    status(&spi, irq::TIMEOUT);
    let mut buf = [0u8; 255];
    assert!(matches!(
        block_on(r.await_rx(&mut buf)),
        Err(Error::Timeout)
    ));
    assert_eq!(
        opcodes(&spi),
        vec![op::CLEAR_IRQ],
        "the chip fell back to standby by itself"
    );
    // Negative control: awaiting with no window standing is refused.
    assert!(matches!(
        block_on(r.await_rx(&mut buf)),
        Err(Error::NotArmed)
    ));
}

#[test]
fn cad_reports_channel_activity_from_the_detected_flag() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 10, 0x04, 0x01, 14, 18)).expect("configure");
    spi.0.borrow_mut().written.clear();
    status(&spi, irq::CAD_DONE | irq::CAD_DETECTED);
    let mut buf = [0u8; 255];
    let mut sink = CapturedFrames::default();
    assert!(block_on(r.cad(10, &mut buf, &mut sink)).expect("cad"));
    assert_eq!(
        opcodes(&spi),
        vec![
            op::SET_CAD_PARAMS,
            op::SET_DIO_IRQ_PARAMS,
            op::CLEAR_IRQ,
            op::SET_CAD,
            op::CLEAR_IRQ
        ]
    );
    assert_eq!(
        written(&spi)[0][2..],
        [0x08, 50, 10, 0x00, 0, 0, 0],
        "8 symbols from SF10"
    );
    status(&spi, irq::CAD_DONE);
    assert!(!block_on(r.cad(10, &mut buf, &mut sink)).expect("cad"));
    assert!(sink.0.is_empty());
}

#[test]
fn a_standing_window_is_stood_down_before_a_transmit() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    block_on(r.arm_rx(0, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    spi.0.borrow_mut().written.clear();
    status(&spi, 0); // the teardown's latch read: an empty channel
    status(&spi, irq::TX_DONE);
    block_on(r.transmit(b"x", 1000)).expect("tx");
    let ops = opcodes(&spi);
    assert_eq!(
        ops[0],
        op::SET_STANDBY,
        "one standby for the standing window"
    );
    assert_eq!(ops[1], op::SET_PKT_PARAMS);
    assert_eq!(ops.iter().filter(|&&o| o == op::SET_STANDBY).count(), 1);
}

#[test]
fn false_preamble_bound_tracks_the_programmed_phy() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    assert_eq!(r.false_preamble_ms(), None);
    for (sf, bw_code, bw_hz, preamble) in [(8, 0x04, 125_000, 18), (10, 0x05, 250_000, 24)] {
        block_on(r.configure_lora(869_463_000, sf, bw_code, 0x01, 14, preamble))
            .expect("configure");
        spi.0.borrow_mut().written.clear();
        let expected = leviculum_core::sx126x::false_preamble_ms(bw_hz, sf, preamble);
        assert!(expected.is_some());
        assert_eq!(r.false_preamble_ms(), expected);
        assert!(opcodes(&spi).is_empty(), "the bound must not touch the bus");
    }
}

#[test]
fn transmit_stand_down_harvests_a_completed_frame_under_both_policies() {
    let _clock = test_clock();
    for policy in [DeferPolicy::OneFrame, DeferPolicy::ReleaseFalsePreamble] {
        let (mut r, spi) = radio();
        block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
        block_on(r.arm_rx(0, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
        spi.0.borrow_mut().written.clear();
        status(&spi, irq::RX_DONE); // the stand-down probe
        status(&spi, irq::RX_DONE); // the latched-frame read
        read(&spi, &[4, 3]);
        read(&spi, b"abcd");
        read(&spi, &[160, 20, 0]);
        let mut buf = [0u8; 255];
        let mut sink = CapturedFrames::default();
        block_on(r.disarm_rx_for_tx(
            leviculum_core::sx126x::RxTeardownBy::Select,
            policy,
            &mut buf,
            &mut sink,
        ))
        .expect("stand-down");
        assert_eq!(sink.0, vec![(b"abcd".to_vec(), -80, 5)]);
        assert!(r.standing_window().is_none());
        assert_eq!(
            opcodes(&spi),
            vec![
                op::CLEAR_IRQ,
                op::GET_RX_BUFFER_STATUS,
                op::READ_BUFFER8,
                op::GET_PKT_STATUS,
            ],
            "harvesting must read the frame without forcing standby"
        );
        assert!(spi.0.borrow().answers.is_empty());
    }
}

#[test]
fn releasing_policy_rechecks_a_bare_preamble_and_stands_down_once() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    block_on(r.arm_rx(0, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    spi.0.borrow_mut().written.clear();
    status(&spi, irq::PREAMBLE_DETECTED); // earns the short bound
    status(&spi, irq::PREAMBLE_DETECTED); // no header after that bound
    status(&spi, irq::PREAMBLE_DETECTED); // no completed frame to take
    status(&spi, irq::PREAMBLE_DETECTED); // teardown sample
    let mut buf = [0u8; 255];
    let mut sink = CapturedFrames::default();
    block_on(r.disarm_rx_for_tx(
        leviculum_core::sx126x::RxTeardownBy::Cad,
        DeferPolicy::ReleaseFalsePreamble,
        &mut buf,
        &mut sink,
    ))
    .expect("stand-down");
    assert!(sink.0.is_empty());
    assert!(r.standing_window().is_none());
    assert_eq!(opcodes(&spi), vec![op::SET_STANDBY]);
    assert!(spi.0.borrow().answers.is_empty(), "all probes were read");
}

#[test]
fn instant_rssi_uses_the_single_byte_read_and_reference_rounding() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    for (raw, expected) in [(0, 0), (1, 0), (159, -79), (160, -80), (255, -127)] {
        read(&spi, &[raw]);
        assert_eq!(block_on(r.get_rssi_inst()).expect("RSSI"), expected);
    }
    assert_eq!(written(&spi), vec![vec![0x02, 0x05]; 5], "no parameters");
    assert!(
        spi.0.borrow().answers.is_empty(),
        "one response byte per read"
    );
}

#[test]
fn an_instant_rssi_read_failure_is_not_a_fabricated_measurement() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    answer(&spi, &[0x04]);
    fail_transfer(&spi);
    assert!(matches!(block_on(r.get_rssi_inst()), Err(Error::Spi)));
    assert_eq!(written(&spi), vec![vec![0x02, 0x05]]);
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn cad_harvests_a_completed_receive_before_programming_detection() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    block_on(r.arm_rx(0, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    spi.0.borrow_mut().written.clear();
    status(&spi, irq::RX_DONE); // CAD's standing-window probe
    status(&spi, irq::RX_DONE); // take the completed frame
    frame_reads(&spi, b"before-cad");
    status(&spi, irq::CAD_DONE | irq::CAD_DETECTED);
    let mut buf = [0u8; 255];
    let mut sink = CapturedFrames::default();
    assert!(block_on(r.cad(8, &mut buf, &mut sink)).expect("CAD"));
    assert_eq!(sink.0, vec![(b"before-cad".to_vec(), -80, 5)]);
    assert!(r.standing_window().is_none());
    assert_eq!(
        opcodes(&spi),
        vec![
            op::CLEAR_IRQ,
            op::GET_RX_BUFFER_STATUS,
            op::READ_BUFFER8,
            op::GET_PKT_STATUS,
            op::SET_CAD_PARAMS,
            op::SET_DIO_IRQ_PARAMS,
            op::CLEAR_IRQ,
            op::SET_CAD,
            op::CLEAR_IRQ,
        ],
        "the payload is harvested before CAD, with no standby cutting it"
    );
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn cad_keeps_receiving_when_a_preamble_becomes_a_header() {
    let _clock = test_clock();
    let (mut r, spi, irq) = controlled_radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    block_on(r.arm_rx(0, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    let short_ms = r.false_preamble_ms().expect("programmed PHY");
    spi.0.borrow_mut().written.clear();
    status(&spi, irq::PREAMBLE_DETECTED);
    status(&spi, irq::PREAMBLE_DETECTED | irq::SYNC_WORD_HEADER_VALID);
    status(&spi, irq::RX_DONE);
    frame_reads(&spi, b"still-arriving");
    status(&spi, irq::CAD_DONE);
    let mut buf = [0u8; 255];
    let mut sink = CapturedFrames::default();
    {
        let mut cad = pin!(r.cad(8, &mut buf, &mut sink));
        assert!(poll_once(cad.as_mut()).is_pending());
        assert!(opcodes(&spi).is_empty());
        advance(short_ms);
        assert!(poll_once(cad.as_mut()).is_pending());
        assert!(
            opcodes(&spi).is_empty(),
            "a header must extend the deferral"
        );
        advance(7);
        irq.high();
        assert!(!block_on(cad.as_mut()).expect("CAD"));
    }
    assert_eq!(Instant::now().as_millis(), short_ms + 7);
    assert_eq!(sink.0, vec![(b"still-arriving".to_vec(), -80, 5)]);
    assert!(!opcodes(&spi).contains(&op::SET_STANDBY));
    assert_eq!(
        &opcodes(&spi)[..4],
        &[
            op::CLEAR_IRQ,
            op::GET_RX_BUFFER_STATUS,
            op::READ_BUFFER8,
            op::GET_PKT_STATUS
        ]
    );
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn expiry_harvests_a_frame_after_extension_without_clearing_its_latch() {
    let _clock = test_clock();
    let (mut r, spi, irq) = controlled_radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    block_on(r.arm_rx(500, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    let extension_ms = leviculum_core::sx126x::rx_extend_ms(
        leviculum_core::sx126x::IRQ_PREAMBLE_DETECTED,
        125_000,
        8,
        5,
        18,
    )
    .expect("extension");
    spi.0.borrow_mut().written.clear();
    status(&spi, irq::PREAMBLE_DETECTED); // software wait expired
    status(&spi, irq::PREAMBLE_DETECTED | irq::SYNC_WORD_HEADER_VALID); // extension expired
    status(&spi, irq::RX_DONE); // receive teardown's deferred harvest
    frame_reads(&spi, b"late-frame");
    let mut buf = [0u8; 255];
    let (len, meta) = {
        let mut receive = pin!(r.await_rx(&mut buf));
        assert!(poll_once(receive.as_mut()).is_pending());
        advance(1_000);
        assert!(poll_once(receive.as_mut()).is_pending());
        assert!(opcodes(&spi).is_empty(), "no clear before the RX extension");
        advance(extension_ms);
        assert!(poll_once(receive.as_mut()).is_pending());
        assert!(
            opcodes(&spi).is_empty(),
            "the last honest latch must survive expiry"
        );
        advance(11);
        irq.high();
        block_on(receive.as_mut()).expect("the deferred reception")
    };
    assert_eq!(&buf[..len as usize], b"late-frame");
    assert_eq!((meta.rssi, meta.snr), (-80, 5));
    assert!(r.standing_window().is_none());
    assert_eq!(
        opcodes(&spi),
        vec![
            op::CLEAR_IRQ,
            op::GET_RX_BUFFER_STATUS,
            op::READ_BUFFER8,
            op::GET_PKT_STATUS
        ],
        "one terminal clear/readout, with no premature standby"
    );
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn expiry_releases_a_false_preamble_after_its_short_bound() {
    let _clock = test_clock();
    let (mut r, spi, _irq) = controlled_radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    block_on(r.arm_rx(500, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    let extension_ms = leviculum_core::sx126x::rx_extend_ms(
        leviculum_core::sx126x::IRQ_PREAMBLE_DETECTED,
        125_000,
        8,
        5,
        18,
    )
    .expect("extension");
    let short_ms = r.false_preamble_ms().expect("programmed PHY");
    spi.0.borrow_mut().written.clear();
    for _ in 0..4 {
        status(&spi, irq::PREAMBLE_DETECTED);
    }
    let mut buf = [0u8; 255];
    {
        let mut receive = pin!(r.await_rx(&mut buf));
        assert!(poll_once(receive.as_mut()).is_pending());
        advance(1_000);
        assert!(poll_once(receive.as_mut()).is_pending());
        advance(extension_ms);
        assert!(poll_once(receive.as_mut()).is_pending());
        assert!(
            opcodes(&spi).is_empty(),
            "nonterminal expiry must not clear IRQs"
        );
        advance(short_ms - 1);
        assert!(poll_once(receive.as_mut()).is_pending());
        advance(1);
        assert!(matches!(block_on(receive.as_mut()), Err(Error::Timeout)));
    }
    assert_eq!(Instant::now().as_millis(), 1_000 + extension_ms + short_ms);
    assert_eq!(opcodes(&spi), vec![op::SET_STANDBY]);
    assert!(r.standing_window().is_none());
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn a_header_error_is_not_a_completed_payload_crc_reception() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    block_on(r.arm_rx(500, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    spi.0.borrow_mut().written.clear();
    status(&spi, irq::HEADER_ERROR);
    let mut buf = [0u8; 255];
    assert!(matches!(
        block_on(r.await_rx(&mut buf)),
        Err(Error::HeaderError)
    ));
    assert_eq!(
        opcodes(&spi),
        vec![op::SET_STANDBY, op::CLEAR_IRQ],
        "no payload or CRC statistics"
    );
    assert!(r.standing_window().is_none());
    assert!(spi.0.borrow().answers.is_empty());
}

/// The firmware's receive census reads the window's flags after a window
/// that delivered nothing (`leviculum-nrf/channel-access/src/rx_census.rs`):
/// a header error has to still be there, in the SX126x's position, and the
/// next arming has to start from nothing.
#[test]
fn a_header_error_stays_in_the_window_flags_until_the_next_arming() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    block_on(r.arm_rx(500, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    assert_eq!(r.rx_window_flags(), 0);
    status(&spi, irq::PREAMBLE_DETECTED | irq::HEADER_ERROR);
    let mut buf = [0u8; 255];
    assert!(matches!(
        block_on(r.await_rx(&mut buf)),
        Err(Error::HeaderError)
    ));
    assert_eq!(
        r.rx_window_flags(),
        leviculum_core::sx126x::IRQ_PREAMBLE_DETECTED | leviculum_core::sx126x::IRQ_HEADER_ERR
    );
    block_on(r.arm_rx(500, leviculum_core::sx126x::RxSite::Idle)).expect("re-arm");
    assert_eq!(r.rx_window_flags(), 0);
}

#[test]
fn a_header_error_raises_the_irq_and_ends_the_window() {
    let _clock = test_clock();
    let (mut r, spi, irq) = controlled_radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    spi.0.borrow_mut().written.clear();
    block_on(r.arm_rx(0, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    assert_eq!(written(&spi)[0][2..], [0, 0, 0x04, 0xC8, 0, 0, 0, 0]);
    spi.0.borrow_mut().written.clear();
    let mut buf = [0u8; 255];
    {
        let mut receive = pin!(r.await_rx(&mut buf));
        assert!(poll_once(receive.as_mut()).is_pending());
        status(&spi, irq::HEADER_ERROR);
        irq.high();
        assert!(matches!(
            poll_once(receive.as_mut()),
            Poll::Ready(Err(Error::HeaderError))
        ));
    }
    assert_eq!(opcodes(&spi), vec![op::SET_STANDBY, op::CLEAR_IRQ]);
    assert!(r.standing_window().is_none());
    assert!(spi.0.borrow().answers.is_empty());
}

/// BUSY stuck high: a part that never finishes booting.
struct StuckBusy;
impl DigitalErrorType for StuckBusy {
    type Error = Never;
}
impl InputPin for StuckBusy {
    fn is_high(&mut self) -> Result<bool, Never> {
        Ok(true)
    }
    fn is_low(&mut self) -> Result<bool, Never> {
        Ok(false)
    }
}
impl Wait for StuckBusy {
    async fn wait_for_high(&mut self) -> Result<(), Never> {
        Ok(())
    }
    async fn wait_for_low(&mut self) -> Result<(), Never> {
        core::future::pending().await
    }
    async fn wait_for_rising_edge(&mut self) -> Result<(), Never> {
        core::future::pending().await
    }
    async fn wait_for_falling_edge(&mut self) -> Result<(), Never> {
        core::future::pending().await
    }
    async fn wait_for_any_edge(&mut self) -> Result<(), Never> {
        core::future::pending().await
    }
}

#[test]
fn reset_reports_a_busy_line_that_never_falls() {
    let _clock = test_clock();
    let spi = FakeSpi::default();
    let mut r = Lr11xx::new(spi.clone(), Pin, StuckBusy, Pin, &LR1110MB1DXS);
    let result = {
        let mut reset = pin!(r.reset());
        loop {
            if let Poll::Ready(result) = poll_once(reset.as_mut()) {
                break result;
            }
            assert!(Instant::now().as_millis() <= 515, "reset never gave up");
            advance(1);
        }
    };
    assert!(matches!(result, Err(Error::Busy)));
    assert_eq!(Instant::now().as_millis(), 515);
    assert!(spi.0.borrow().written.is_empty());
}

#[test]
fn cad_below_sf10_listens_for_four_symbols() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    spi.0.borrow_mut().written.clear();
    status(&spi, irq::CAD_DONE);
    let mut buf = [0u8; 255];
    let mut sink = CapturedFrames::default();
    assert!(!block_on(r.cad(8, &mut buf, &mut sink)).expect("cad"));
    assert_eq!(written(&spi)[0][2..], [0x04, 50, 10, 0x00, 0, 0, 0]);
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn a_failed_crc_measurement_reports_spi_error_not_synthetic_status() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    block_on(r.arm_rx(500, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    spi.0.borrow_mut().written.clear();
    status(&spi, irq::RX_DONE | irq::CRC_ERROR);
    read(&spi, &[77, 3]);
    answer(&spi, &[0x04]);
    fail_transfer(&spi);
    let mut buf = [0u8; 255];
    assert!(matches!(block_on(r.await_rx(&mut buf)), Err(Error::Spi)));
    assert_eq!(
        opcodes(&spi),
        vec![op::CLEAR_IRQ, op::GET_RX_BUFFER_STATUS, op::GET_PKT_STATUS]
    );
    assert!(r.standing_window().is_none(), "the chip already left RX");
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn a_canceled_irq_wait_leaves_the_completed_readout_for_the_uncanceled_half() {
    let _clock = test_clock();
    let (mut r, spi, irq) = controlled_radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    block_on(r.arm_rx(500, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    spi.0.borrow_mut().written.clear();
    {
        let mut wait = pin!(r.wait_rx_ready());
        assert!(poll_once(wait.as_mut()).is_pending());
    }
    assert!(r.standing_window().is_some());
    assert!(opcodes(&spi).is_empty());
    status(&spi, irq::RX_DONE);
    frame_reads(&spi, b"owned-once");
    irq.high();
    block_on(r.wait_rx_ready()).expect("IRQ ready");
    assert!(
        spi.0.borrow().written.is_empty(),
        "waiting must not consume the latch"
    );
    let mut buf = [0u8; 255];
    let (len, meta) = block_on(r.read_ready_rx(&mut buf)).expect("uncanceled readout");
    assert_eq!(&buf[..len as usize], b"owned-once");
    assert_eq!((meta.rssi, meta.snr), (-80, 5));
    assert!(matches!(block_on(r.wait_rx_ready()), Err(Error::NotArmed)));
    assert!(matches!(
        block_on(r.read_ready_rx(&mut buf)),
        Err(Error::NotArmed)
    ));
    assert_eq!(
        opcodes(&spi),
        vec![
            op::CLEAR_IRQ,
            op::GET_RX_BUFFER_STATUS,
            op::READ_BUFFER8,
            op::GET_PKT_STATUS
        ]
    );
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn sampling_reads_the_channel_without_clearing_irq_or_leaving_rx() {
    let _clock = test_clock();
    let (mut r, spi, irq) = controlled_radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    r.set_channel_sampler(Duration::from_millis(30), capture_sample);
    block_on(r.arm_rx(500, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    spi.0.borrow_mut().written.clear();
    let cases = [
        (0, 0, 0, false, false),
        (irq::PREAMBLE_DETECTED, 1, 0, true, false),
        (
            irq::PREAMBLE_DETECTED | irq::SYNC_WORD_HEADER_VALID,
            159,
            -79,
            true,
            true,
        ),
        (
            irq::PREAMBLE_DETECTED | irq::HEADER_ERROR,
            160,
            -80,
            true,
            false,
        ),
        (irq::SYNC_WORD_HEADER_VALID, 255, -127, false, true),
    ];
    for &(flags, raw, _, _, _) in &cases {
        status(&spi, flags);
        read(&spi, &[raw]);
    }
    status(&spi, irq::TIMEOUT);
    let mut buf = [0u8; 255];
    {
        let mut receive = pin!(r.await_rx(&mut buf));
        assert!(poll_once(receive.as_mut()).is_pending());
        for (index, &(_, _, rssi_dbm, preamble, header)) in cases.iter().enumerate() {
            advance(30);
            assert!(poll_once(receive.as_mut()).is_pending());
            assert_eq!(opcodes(&spi), vec![op::GET_RSSI_INST; index + 1]);
            assert_eq!(
                samples()[index],
                ChannelSample {
                    at_ms: (index as u64 + 1) * 30,
                    preamble,
                    header,
                    rssi_dbm
                }
            );
        }
        advance(1);
        irq.high();
        assert!(matches!(block_on(receive.as_mut()), Err(Error::Timeout)));
    }
    let mut expected = vec![op::GET_RSSI_INST; cases.len()];
    expected.push(op::CLEAR_IRQ);
    assert_eq!(opcodes(&spi), expected);
    assert!(r.standing_window().is_none());
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn sample_reads_are_charged_to_one_absolute_receive_deadline() {
    let _clock = test_clock();
    let (mut r, spi, _irq) = controlled_radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    r.set_channel_sampler(Duration::from_millis(30), capture_sample);
    block_on(r.arm_rx(1, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    spi.0.borrow_mut().written.clear();
    spi.0.borrow_mut().read_delay_ms = 15;
    for _ in 0..8 {
        status(&spi, 0);
        read(&spi, &[160]);
    }
    status(&spi, 0); // the final classification, not a ninth sample
    let mut buf = [0u8; 255];
    {
        let mut receive = pin!(r.await_rx(&mut buf));
        assert!(poll_once(receive.as_mut()).is_pending());
        for index in 1..=8 {
            advance(30);
            assert!(poll_once(receive.as_mut()).is_pending());
            assert_eq!(Instant::now().as_millis(), index * 60);
            assert_eq!(samples().len(), index as usize);
        }
        advance(20);
        assert!(
            poll_once(receive.as_mut()).is_pending(),
            "the last wait is only 21 ms"
        );
        advance(1);
        assert!(matches!(block_on(receive.as_mut()), Err(Error::Timeout)));
    }
    assert_eq!(
        Instant::now().as_millis(),
        501 + 15,
        "only final SPI readout remains"
    );
    let mut expected = vec![op::GET_RSSI_INST; 8];
    expected.push(op::SET_STANDBY);
    assert_eq!(opcodes(&spi), expected);
    assert_eq!(
        samples()
            .iter()
            .map(|sample| sample.at_ms)
            .collect::<Vec<_>>(),
        (1..=8).map(|index| index * 60).collect::<Vec<_>>()
    );
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn failed_samples_do_not_fabricate_carrier_or_discard_the_receive() {
    let _clock = test_clock();
    let (mut r, spi, irq) = controlled_radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    r.set_channel_sampler(Duration::from_millis(30), capture_sample);
    block_on(r.arm_rx(500, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    spi.0.borrow_mut().written.clear();
    fail_transfer(&spi); // IRQ read failed: do not issue RSSI read
    status(&spi, irq::PREAMBLE_DETECTED);
    answer(&spi, &[0x04]);
    fail_transfer(&spi); // RSSI response failed: do not publish this sample
    status(&spi, irq::PREAMBLE_DETECTED | irq::SYNC_WORD_HEADER_VALID);
    read(&spi, &[255]);
    status(&spi, irq::RX_DONE);
    frame_reads(&spi, b"survived-errors");
    let mut buf = [0u8; 255];
    let (len, _) = {
        let mut receive = pin!(r.await_rx(&mut buf));
        assert!(poll_once(receive.as_mut()).is_pending());
        advance(30);
        assert!(poll_once(receive.as_mut()).is_pending());
        assert!(samples().is_empty());
        assert!(opcodes(&spi).is_empty(), "failed IRQ must skip RSSI");
        advance(30);
        assert!(poll_once(receive.as_mut()).is_pending());
        assert!(samples().is_empty());
        assert_eq!(opcodes(&spi), vec![op::GET_RSSI_INST]);
        advance(30);
        assert!(poll_once(receive.as_mut()).is_pending());
        assert_eq!(
            samples(),
            vec![ChannelSample {
                at_ms: 90,
                preamble: true,
                header: true,
                rssi_dbm: -127
            }]
        );
        assert_eq!(opcodes(&spi), vec![op::GET_RSSI_INST; 2]);
        irq.high();
        block_on(receive.as_mut()).expect("frame after failed samples")
    };
    assert_eq!(&buf[..len as usize], b"survived-errors");
    assert!(!opcodes(&spi).contains(&op::SET_STANDBY));
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn sampling_also_preserves_a_receive_during_cad_deferral() {
    let _clock = test_clock();
    let (mut r, spi, irq) = controlled_radio();
    block_on(r.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
    r.set_channel_sampler(Duration::from_millis(30), capture_sample);
    block_on(r.arm_rx(0, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    spi.0.borrow_mut().written.clear();
    status(&spi, irq::PREAMBLE_DETECTED | irq::SYNC_WORD_HEADER_VALID); // CAD probe
    status(&spi, irq::PREAMBLE_DETECTED | irq::SYNC_WORD_HEADER_VALID); // timed sample
    read(&spi, &[160]);
    status(&spi, irq::RX_DONE);
    frame_reads(&spi, b"sampled-deferral");
    status(&spi, irq::CAD_DONE);
    let mut buf = [0u8; 255];
    let mut sink = CapturedFrames::default();
    {
        let mut cad = pin!(r.cad(8, &mut buf, &mut sink));
        assert!(poll_once(cad.as_mut()).is_pending());
        advance(30);
        assert!(poll_once(cad.as_mut()).is_pending());
        assert_eq!(opcodes(&spi), vec![op::GET_RSSI_INST]);
        assert_eq!(
            samples(),
            vec![ChannelSample {
                at_ms: 30,
                preamble: true,
                header: true,
                rssi_dbm: -80
            }]
        );
        advance(1);
        irq.high();
        assert!(!block_on(cad.as_mut()).expect("CAD after readout"));
    }
    assert_eq!(sink.0, vec![(b"sampled-deferral".to_vec(), -80, 5)]);
    assert_eq!(samples().len(), 1, "CAD itself must not produce RX samples");
    assert!(!opcodes(&spi).contains(&op::SET_STANDBY));
    assert!(spi.0.borrow().answers.is_empty());
}

#[test]
fn zero_sample_period_is_refused_before_any_radio_command() {
    let _clock = test_clock();
    let (mut r, spi) = radio();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        r.set_channel_sampler(Duration::from_ticks(0), capture_sample);
    }));
    assert!(result.is_err());
    assert!(spi.0.borrow().written.is_empty());
}

#[test]
fn a_sampler_on_one_radio_does_not_sample_another_instance() {
    let _clock = test_clock();
    let (mut measured, measured_spi, measured_irq) = controlled_radio();
    let (mut unmeasured, unmeasured_spi, unmeasured_irq) = controlled_radio();
    for radio in [&mut measured, &mut unmeasured] {
        block_on(radio.configure_lora(869_463_000, 8, 0x04, 0x01, 14, 18)).expect("configure");
        block_on(radio.arm_rx(500, leviculum_core::sx126x::RxSite::Idle)).expect("arm");
    }
    measured.set_channel_sampler(Duration::from_millis(30), capture_sample);
    measured_spi.0.borrow_mut().written.clear();
    unmeasured_spi.0.borrow_mut().written.clear();
    status(&measured_spi, 0);
    read(&measured_spi, &[160]);
    status(&measured_spi, irq::TIMEOUT);
    status(&unmeasured_spi, irq::TIMEOUT);
    let mut buf = [0u8; 255];
    {
        let mut receive = pin!(measured.await_rx(&mut buf));
        assert!(poll_once(receive.as_mut()).is_pending());
        advance(30);
        assert!(poll_once(receive.as_mut()).is_pending());
        assert_eq!(samples().len(), 1);
        measured_irq.high();
        assert!(matches!(block_on(receive.as_mut()), Err(Error::Timeout)));
    }
    {
        let mut receive = pin!(unmeasured.await_rx(&mut buf));
        assert!(poll_once(receive.as_mut()).is_pending());
        advance(30);
        assert!(poll_once(receive.as_mut()).is_pending());
        assert_eq!(samples().len(), 1);
        assert!(unmeasured_spi.0.borrow().written.is_empty());
        unmeasured_irq.high();
        assert!(matches!(block_on(receive.as_mut()), Err(Error::Timeout)));
    }
    assert_eq!(opcodes(&unmeasured_spi), vec![op::CLEAR_IRQ]);
    assert!(measured_spi.0.borrow().answers.is_empty());
    assert!(unmeasured_spi.0.borrow().answers.is_empty());
}
