//! One host fake per seam (test builds only).
//!
//! Step 0's fakes implement empty seams, so all most of them prove is that
//! the seam is implementable off the board. The two ecosystem seams already
//! carry methods, and their fakes are working ones: a byte-array flash with
//! NOR semantics (program clears bits, erase sets them) and a seeded
//! xorshift entropy source. Each fake grows with its seam.

use alloc::vec;
use alloc::vec::Vec;
use core::convert::Infallible;
use embedded_storage_async::nor_flash::{ErrorType, NorFlash, ReadNorFlash};

use crate::seams::{BleLink, Clock, ControlPort, Flash, Sensors, Sinks, Transceiver, Watchdog};

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

/// A clock the test moves by hand, once step 1 gives the seam a reading.
pub struct FakeClock;
impl Clock for FakeClock {}

/// Sensors with fixed readings, once step 5 cuts them.
pub struct FakeSensors;
impl Sensors for FakeSensors {}

/// Sinks that record what they were given, once step 1 cuts them.
pub struct FakeSinks;
impl Sinks for FakeSinks {}

/// A watchdog that counts its feeds, once step 1 gives it one.
pub struct FakeWatchdog;
impl Watchdog for FakeWatchdog {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seams::Entropy;
    use core::future::Future;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    /// The fakes never await anything, so one poll finishes them.
    fn ready<F: Future>(f: F) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
        match pin!(f).poll(&mut cx) {
            Poll::Ready(out) => out,
            Poll::Pending => panic!("a fake future was not ready on its first poll"),
        }
    }

    fn is_transceiver<T: Transceiver>(_: &T) {}
    fn is_control_port<T: ControlPort>(_: &T) {}
    fn is_ble_link<T: BleLink>(_: &T) {}
    fn is_flash<T: Flash>(_: &T) {}
    fn is_entropy<T: Entropy>(_: &T) {}
    fn is_clock<T: Clock>(_: &T) {}
    fn is_sensors<T: Sensors>(_: &T) {}
    fn is_sinks<T: Sinks>(_: &T) {}
    fn is_watchdog<T: Watchdog>(_: &T) {}

    #[test]
    fn every_seam_has_a_host_fake() {
        is_transceiver(&FakeTransceiver);
        is_control_port(&FakeControlPort);
        is_ble_link(&FakeBleLink);
        is_flash(&FakeFlash::new(1));
        is_entropy(&FakeEntropy::new(1));
        is_clock(&FakeClock);
        is_sensors(&FakeSensors);
        is_sinks(&FakeSinks);
        is_watchdog(&FakeWatchdog);
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
