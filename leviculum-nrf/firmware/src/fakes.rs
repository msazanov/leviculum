//! One host fake per seam (test builds only).
//!
//! Step 0's fakes implement empty seams, so all most of them prove is that
//! the seam is implementable off the board. The two ecosystem seams already
//! carry methods, and their fakes are working ones: a byte-array flash with
//! NOR semantics (program clears bits, erase sets them) and a seeded
//! xorshift entropy source. Each fake grows with its seam: the sinks'
//! records what it was asked to play since 973, the clock is moved by hand
//! and the button follows a scripted edge sequence on it since 976, and the
//! panel keeps its frames and power writes since 980, when the driver that
//! polls a module's loop over the clock ([`drive`]) moved here from the
//! button's tests.

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::convert::Infallible;
use embedded_storage_async::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
use leviculum_feedback::Pattern;

use crate::feedback::Sink;

use crate::seams::{
    BleLink, ButtonInput, Clock, ControlPort, Flash, Sensors, Sinks, Transceiver, Watchdog,
};

/// A transceiver backend. Grows a command log and a scripted event queue
/// when step 3 gives the seam its messages.
pub struct FakeTransceiver;
impl Transceiver for FakeTransceiver {}

/// A control port. Grows two byte buffers in step 2.
pub struct FakeControlPort;
impl ControlPort for FakeControlPort {}

/// A BLE backend. Grows its message logs in step 4.
pub struct FakeBleLink;
impl BleLink for FakeBleLink {}

/// A NOR part in RAM: erased bytes are 0xFF, a program can only clear bits.
pub struct FakeFlash {
    pub bytes: Vec<u8>,
}

impl FakeFlash {
    pub const ERASE_SIZE: usize = 4096;

    pub fn new(sectors: usize) -> Self {
        Self {
            bytes: vec![0xFF; sectors * Self::ERASE_SIZE],
        }
    }
}

impl ErrorType for FakeFlash {
    type Error = Infallible;
}

impl ReadNorFlash for FakeFlash {
    const READ_SIZE: usize = 1;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let at = offset as usize;
        bytes.copy_from_slice(&self.bytes[at..at + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for FakeFlash {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = FakeFlash::ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.bytes[from as usize..to as usize].fill(0xFF);
        Ok(())
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let at = offset as usize;
        for (cell, b) in self.bytes[at..at + bytes.len()].iter_mut().zip(bytes) {
            *cell &= *b;
        }
        Ok(())
    }
}

impl Flash for FakeFlash {}

/// A seeded xorshift64 generator: deterministic, never zero.
pub struct FakeEntropy {
    state: u64,
}

impl FakeEntropy {
    pub fn new(seed: u64) -> Self {
        Self { state: seed | 1 }
    }
}

impl rand_core::RngCore for FakeEntropy {
    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    fn next_u64(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(8) {
            let word = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

/// A clock the test moves by hand ([`set`](Self::set)). A wait for a
/// moment it has not reached is pending and remembers the moment
/// ([`take_asked`](Self::take_asked)), so a driver knows where to move it.
#[derive(Default)]
pub struct FakeClock {
    now_ms: Cell<u64>,
    asked: Cell<Option<u64>>,
}

impl FakeClock {
    /// A clock at 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// Move the clock to `ms`.
    pub fn set(&self, ms: u64) {
        self.now_ms.set(ms);
    }

    /// The earliest moment a wait asked for since the last call, if one
    /// is still ahead of the clock.
    pub fn take_asked(&self) -> Option<u64> {
        self.asked.take()
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.now_ms.get()
    }

    fn wait_until(&self, at_ms: u64) -> impl core::future::Future<Output = ()> {
        core::future::poll_fn(move |_| {
            if self.now_ms.get() >= at_ms {
                return core::task::Poll::Ready(());
            }
            let earliest = self.asked.get().map_or(at_ms, |asked| asked.min(at_ms));
            self.asked.set(Some(earliest));
            core::task::Poll::Pending
        })
    }
}

/// A button that follows a script of edges, `(t_ms, pressed)` in time
/// order, on a [`FakeClock`]: an edge whose moment the clock has reached
/// ends a wait and sets the level. The seam is implemented on `&Self`, so
/// the test reads [`next_edge_ms`](Self::next_edge_ms) while the handling
/// holds the input.
pub struct FakeButtonInput<'a> {
    clock: &'a FakeClock,
    script: RefCell<VecDeque<(u64, bool)>>,
    pressed: Cell<bool>,
}

impl<'a> FakeButtonInput<'a> {
    /// A released button that will see `edges`.
    pub fn new(clock: &'a FakeClock, edges: &[(u64, bool)]) -> Self {
        Self {
            clock,
            script: RefCell::new(edges.iter().copied().collect()),
            pressed: Cell::new(false),
        }
    }

    /// When the next scripted edge comes, `None` once the script is spent.
    pub fn next_edge_ms(&self) -> Option<u64> {
        self.script.borrow().front().map(|(at, _)| *at)
    }
}

impl ButtonInput for &FakeButtonInput<'_> {
    fn wait_for_edge(&mut self) -> impl core::future::Future<Output = ()> {
        let input = *self;
        core::future::poll_fn(move |_| {
            let mut script = input.script.borrow_mut();
            match script.front() {
                Some(&(at, pressed)) if at <= input.clock.now_ms() => {
                    script.pop_front();
                    input.pressed.set(pressed);
                    core::task::Poll::Ready(())
                }
                _ => core::task::Poll::Pending,
            }
        })
    }

    fn is_pressed(&self) -> bool {
        self.pressed.get()
    }
}

/// Sensors with fixed readings, once step 5 cuts them.
pub struct FakeSensors;
impl Sensors for FakeSensors {}

/// Sinks that record what they were given, each call with the time of a
/// clock the test moves by hand ([`advance`](Self::advance)): the host
/// fake of the feedback players.
pub struct RecordingSinks {
    present: Vec<Sink>,
    now_ms: Cell<u64>,
    calls: RefCell<Vec<(u64, Sink, &'static str)>>,
}

impl RecordingSinks {
    /// A board with these sinks, its clock at 0.
    pub fn new(present: &[Sink]) -> Self {
        Self {
            present: present.to_vec(),
            now_ms: Cell::new(0),
            calls: RefCell::new(Vec::new()),
        }
    }

    /// Move the clock by `ms`.
    pub fn advance(&self, ms: u64) {
        self.now_ms.set(self.now_ms.get() + ms);
    }

    /// What was played: `(t_ms, sink, pattern name)`, in order.
    pub fn calls(&self) -> Vec<(u64, Sink, &'static str)> {
        self.calls.borrow().clone()
    }
}

impl Sinks for RecordingSinks {
    fn present(&self, sink: Sink) -> bool {
        self.present.contains(&sink)
    }

    fn play(&self, sink: Sink, pattern: Pattern) {
        self.calls
            .borrow_mut()
            .push((self.now_ms.get(), sink, pattern.name));
    }
}

/// The frame a [`RecordingPanel`] lends: the Pocket V2's 128x64, one bit
/// a pixel.
#[cfg(feature = "display")]
pub type RecordingPanelFb = leviculum_screen::MonoFb<128, 64, 1024>;

/// A panel that keeps every frame pushed and every power write, each with
/// the time of a [`FakeClock`]: the host fake of the display drivers.
#[cfg(feature = "display")]
pub struct RecordingPanel<'a> {
    clock: &'a FakeClock,
    power_control: bool,
    frame: RecordingPanelFb,
    pushes: Vec<(u64, RecordingPanelFb)>,
    power: Vec<(u64, bool)>,
}

#[cfg(feature = "display")]
impl<'a> RecordingPanel<'a> {
    /// A blank panel, with power control (the SSD1306) or without (the
    /// ST7789).
    pub fn new(clock: &'a FakeClock, power_control: bool) -> Self {
        Self {
            clock,
            power_control,
            frame: RecordingPanelFb::new(),
            pushes: Vec::new(),
            power: Vec::new(),
        }
    }

    /// What was pushed: `(t_ms, frame)`, in order.
    pub fn pushes(&self) -> &[(u64, RecordingPanelFb)] {
        &self.pushes
    }

    /// The power writes: `(t_ms, on)`, in order.
    pub fn power(&self) -> Vec<(u64, bool)> {
        self.power.clone()
    }
}

#[cfg(feature = "display")]
impl crate::seams::Panel for RecordingPanel<'_> {
    type Target = RecordingPanelFb;
    type Error = Infallible;
    const WIDTH: u32 = 128;

    fn frame(&mut self) -> &mut RecordingPanelFb {
        &mut self.frame
    }

    fn flush(&mut self) -> impl core::future::Future<Output = Result<(), Infallible>> {
        let mut shown = RecordingPanelFb::new();
        shown.copy_from(&self.frame);
        self.pushes.push((self.clock.now_ms(), shown));
        core::future::ready(Ok(()))
    }

    fn has_power_control(&self) -> bool {
        self.power_control
    }

    fn set_power(&mut self, on: bool) -> Result<(), Infallible> {
        self.power.push((self.clock.now_ms(), on));
        Ok(())
    }
}

/// A watchdog that counts its feeds, once step 1 gives it one.
pub struct FakeWatchdog;
impl Watchdog for FakeWatchdog {}

/// Run a future over the fakes to completion. The fakes never await
/// anything, so one poll finishes it; a second would mean a module under
/// test awaited something no fake answers.
pub fn ready<F: core::future::Future>(f: F) -> F::Output {
    use core::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    match core::pin::pin!(f).poll(&mut cx) {
        Poll::Ready(out) => out,
        Poll::Pending => panic!("a fake future was not ready on its first poll"),
    }
}

/// Poll `run`, a module's loop that never returns, over a [`FakeClock`]
/// until nothing is due by `until_ms`. The fakes never wake a waker, so
/// the driver polls, then moves the clock to the earlier of the moment
/// the loop waits for and the next moment of the test's own script, and
/// polls again. `step` is called with the clock's time before every poll
/// and once after the last: it raises what the script has due by then,
/// takes what the last poll raised, and answers the script's next moment.
pub fn drive<F: core::future::Future>(
    run: F,
    clock: &FakeClock,
    until_ms: u64,
    mut step: impl FnMut(u64) -> Option<u64>,
) {
    use core::task::{Context, Waker};
    let mut run = core::pin::pin!(run);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        let scripted = step(clock.now_ms());
        assert!(
            run.as_mut().poll(&mut cx).is_pending(),
            "the loop never returns"
        );
        let next = [scripted, clock.take_asked()].into_iter().flatten().min();
        match next {
            Some(at) if at <= until_ms => clock.set(at.max(clock.now_ms())),
            _ => break,
        }
    }
    step(clock.now_ms());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seams::Entropy;

    fn is_transceiver<T: Transceiver>(_: &T) {}
    fn is_control_port<T: ControlPort>(_: &T) {}
    fn is_ble_link<T: BleLink>(_: &T) {}
    fn is_flash<T: Flash>(_: &T) {}
    fn is_entropy<T: Entropy>(_: &T) {}
    fn is_clock<T: Clock>(_: &T) {}
    fn is_button_input<T: ButtonInput>(_: &T) {}
    fn is_sensors<T: Sensors>(_: &T) {}
    fn is_sinks<T: Sinks>(_: &T) {}
    fn is_watchdog<T: Watchdog>(_: &T) {}
    #[cfg(feature = "display")]
    fn is_panel<T: crate::seams::Panel>(_: &T) {}

    #[test]
    fn every_seam_has_a_host_fake() {
        is_transceiver(&FakeTransceiver);
        is_control_port(&FakeControlPort);
        is_ble_link(&FakeBleLink);
        is_flash(&FakeFlash::new(1));
        is_entropy(&FakeEntropy::new(1));
        let clock = FakeClock::new();
        is_clock(&clock);
        is_button_input(&&FakeButtonInput::new(&clock, &[]));
        is_sensors(&FakeSensors);
        is_sinks(&RecordingSinks::new(&[]));
        is_watchdog(&FakeWatchdog);
        #[cfg(feature = "display")]
        is_panel(&RecordingPanel::new(&clock, true));
    }

    #[test]
    fn fake_flash_programs_only_clear_bits_and_erase_restores() {
        let mut flash = FakeFlash::new(2);
        ready(flash.write(8, &[0x0F, 0xF0, 0xAA, 0x55])).unwrap();
        ready(flash.write(8, &[0xFF, 0x0F, 0xFF, 0xFF])).unwrap();
        let mut out = [0u8; 4];
        ready(flash.read(8, &mut out)).unwrap();
        assert_eq!(out, [0x0F, 0x00, 0xAA, 0x55]);
        ready(flash.erase(0, FakeFlash::ERASE_SIZE as u32)).unwrap();
        ready(flash.read(8, &mut out)).unwrap();
        assert_eq!(out, [0xFF; 4]);
        assert_eq!(flash.capacity(), 2 * FakeFlash::ERASE_SIZE);
    }

    #[test]
    fn fake_entropy_is_deterministic_per_seed() {
        let (mut a, mut b) = (FakeEntropy::new(7), FakeEntropy::new(7));
        let (mut x, mut y) = ([0u8; 13], [0u8; 13]);
        rand_core::RngCore::fill_bytes(&mut a, &mut x);
        rand_core::RngCore::fill_bytes(&mut b, &mut y);
        assert_eq!(x, y);
        assert_ne!(x, [0u8; 13]);
    }
}
