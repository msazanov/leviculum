//! rawg Link GCM v1. Only negotiated Link tokens use this format; identity
//! tokens and the separately encrypted Resource body keep their existing format.
use aes_gcm::{Aes256Gcm, KeyInit, Nonce, Tag, aead::AeadInPlace};
use core::cell::{Cell, RefCell};

pub(crate) const OVERHEAD: usize = 12 + 16;
const MAX_RECORDS: u64 = 1 << 32;
const MAX_BYTES: u64 = 1 << 36; // Re-establish with fresh ephemeral keys at 64 GiB/direction.
const REPLAY_BITS: usize = 2048;

pub(crate) struct LinkGcm {
    tx: Aes256Gcm,
    rx: Aes256Gcm,
    tx_prefix: [u8; 4],
    rx_prefix: [u8; 4],
    tx_aad: [u8; 22],
    rx_aad: [u8; 22],
    counter: Cell<u64>,
    bytes: Cell<u64>,
    rx_highest: Cell<Option<u64>>,
    rx_seen: RefCell<[u64; REPLAY_BITS / 64]>,
}

impl LinkGcm {
    pub(crate) fn new(master: &[u8; 64], link_id: &[u8; 16], initiator: bool) -> Self {
        // Independent keys and nonce prefixes in each direction. Never reuse
        // CBC's AES key as a GCM key; include a versioned HKDF domain label.
        let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(link_id), master);
        let mut i2r = [0u8; 36];
        let mut r2i = [0u8; 36];
        hk.expand(b"rawg/link/aes256-gcm/v1/i2r", &mut i2r).unwrap();
        hk.expand(b"rawg/link/aes256-gcm/v1/r2i", &mut r2i).unwrap();
        let (tx, rx, tx_dir) = if initiator {
            (&i2r, &r2i, 0)
        } else {
            (&r2i, &i2r, 1)
        };
        let mut tx_aad = [0u8; 22];
        tx_aad[..4].copy_from_slice(b"GCM1");
        tx_aad[4..20].copy_from_slice(link_id);
        tx_aad[20] = tx_dir;
        let mut rx_aad = tx_aad;
        rx_aad[20] ^= 1;
        Self {
            tx: Aes256Gcm::new_from_slice(&tx[..32]).unwrap(),
            rx: Aes256Gcm::new_from_slice(&rx[..32]).unwrap(),
            tx_prefix: tx[32..].try_into().unwrap(),
            rx_prefix: rx[32..].try_into().unwrap(),
            tx_aad,
            rx_aad,
            counter: Cell::new(0),
            bytes: Cell::new(0),
            rx_highest: Cell::new(None),
            rx_seen: RefCell::new([0; REPLAY_BITS / 64]),
        }
    }

    #[cfg(test)]
    fn encrypt(&self, plaintext: &[u8], output: &mut [u8]) -> Result<usize, ()> {
        self.encrypt_context(plaintext, output, 0)
    }

    pub(crate) fn encrypt_context(
        &self,
        plaintext: &[u8],
        output: &mut [u8],
        context: u8,
    ) -> Result<usize, ()> {
        let len = plaintext.len().checked_add(OVERHEAD).ok_or(())?;
        let next_bytes = self
            .bytes
            .get()
            .checked_add(plaintext.len() as u64)
            .ok_or(())?;
        let counter = self.counter.get();
        if output.len() < len || counter >= MAX_RECORDS || next_bytes > MAX_BYTES {
            return Err(());
        }
        // Reserve the nonce before encryption, including on an encryption error.
        self.counter.set(counter + 1);
        self.bytes.set(next_bytes);
        let mut nonce = [0u8; 12];
        nonce[..4].copy_from_slice(&self.tx_prefix);
        nonce[4..].copy_from_slice(&counter.to_be_bytes());
        output[..12].copy_from_slice(&nonce);
        let ciphertext = &mut output[12..len - 16];
        ciphertext.copy_from_slice(plaintext);
        let mut aad = self.tx_aad;
        aad[21] = context;
        let tag = self
            .tx
            .encrypt_in_place_detached(Nonce::from_slice(&nonce), &aad, ciphertext)
            .map_err(|_| ())?;
        output[len - 16..len].copy_from_slice(&tag);
        Ok(len)
    }

    #[cfg(test)]
    fn decrypt(&self, token: &[u8], output: &mut [u8]) -> Result<usize, ()> {
        self.decrypt_context(token, output, 0)
    }

    pub(crate) fn decrypt_context(
        &self,
        token: &[u8],
        output: &mut [u8],
        context: u8,
    ) -> Result<usize, ()> {
        if token.len() < OVERHEAD || token[..4] != self.rx_prefix {
            return Err(());
        }
        let counter = u64::from_be_bytes(token[4..12].try_into().unwrap());
        if counter >= MAX_RECORDS || self.replayed(counter) {
            return Err(());
        }
        let len = token.len() - OVERHEAD;
        if output.len() < len {
            return Err(());
        }
        let plaintext = &mut output[..len];
        plaintext.copy_from_slice(&token[12..12 + len]);
        let mut aad = self.rx_aad;
        aad[21] = context;
        let result = self.rx.decrypt_in_place_detached(
            Nonce::from_slice(&token[..12]),
            &aad,
            plaintext,
            Tag::from_slice(&token[token.len() - 16..]),
        );
        if result.is_err() {
            plaintext.fill(0);
            return Err(());
        }
        // Only authenticated records advance replay state. Nonces are wider
        // than the Channel's wrapping 16-bit sequence numbers.
        self.accept_counter(counter);
        Ok(len)
    }

    fn replayed(&self, counter: u64) -> bool {
        let Some(highest) = self.rx_highest.get() else {
            return false;
        };
        if counter > highest {
            return false;
        }
        let distance = highest - counter;
        if distance >= REPLAY_BITS as u64 {
            return true;
        }
        let distance = distance as usize;
        self.rx_seen.borrow()[distance / 64] & (1u64 << (distance % 64)) != 0
    }

    fn accept_counter(&self, counter: u64) {
        let mut seen = self.rx_seen.borrow_mut();
        let Some(highest) = self.rx_highest.get() else {
            self.rx_highest.set(Some(counter));
            seen[0] = 1;
            return;
        };
        if counter <= highest {
            let distance = (highest - counter) as usize;
            seen[distance / 64] |= 1u64 << (distance % 64);
            return;
        }
        let shift = counter - highest;
        if shift >= REPLAY_BITS as u64 {
            seen.fill(0);
        } else {
            let words = shift as usize / 64;
            let bits = shift as usize % 64;
            for i in (0..seen.len()).rev() {
                let mut value = if i >= words {
                    seen[i - words] << bits
                } else {
                    0
                };
                if bits != 0 && i > words {
                    value |= seen[i - words - 1] >> (64 - bits);
                }
                seen[i] = value;
            }
        }
        seen[0] |= 1;
        self.rx_highest.set(Some(counter));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directions_tamper_binding_and_fresh_keys() {
        let a = LinkGcm::new(&[42; 64], &[1; 16], true);
        let b = LinkGcm::new(&[42; 64], &[1; 16], false);
        let wrong_link = LinkGcm::new(&[42; 64], &[2; 16], false);
        let fresh_keys = LinkGcm::new(&[43; 64], &[1; 16], false);
        let mut packet = [0u8; 128];
        let mut out = [0u8; 128];
        for payload in [&b""[..], &b"hello"[..], &[7u8; 64][..]] {
            let n = a.encrypt(payload, &mut packet).unwrap();
            assert_eq!(b.decrypt(&packet[..n], &mut out).unwrap(), payload.len());
            assert_eq!(&out[..payload.len()], payload);
            assert!(a.decrypt(&packet[..n], &mut out).is_err()); // no reflection
            assert!(wrong_link.decrypt(&packet[..n], &mut out).is_err());
            assert!(fresh_keys.decrypt(&packet[..n], &mut out).is_err());
            for byte in 0..n {
                let mut damaged = packet;
                damaged[byte] ^= 1;
                let fresh_receiver = LinkGcm::new(&[42; 64], &[1; 16], false);
                assert!(fresh_receiver.decrypt(&damaged[..n], &mut out).is_err());
            }
        }
        let n = b.encrypt(b"reverse", &mut packet).unwrap();
        assert_eq!(a.decrypt(&packet[..n], &mut out).unwrap(), 7);
        assert_eq!(&out[..7], b"reverse");
        let n = a.encrypt_context(b"channel", &mut packet, 0x0e).unwrap();
        assert!(b.decrypt_context(&packet[..n], &mut out, 0).is_err());
        assert_eq!(b.decrypt_context(&packet[..n], &mut out, 0x0e).unwrap(), 7);
    }

    #[test]
    fn nonce_uniqueness_and_budget_do_not_wrap() {
        let a = LinkGcm::new(&[42; 64], &[1; 16], true);
        let mut x = [0u8; 32];
        let mut y = x;
        a.encrypt(b"same", &mut x).unwrap();
        a.encrypt(b"same", &mut y).unwrap();
        assert_ne!(&x[..12], &y[..12]);
        a.counter.set(MAX_RECORDS);
        assert!(a.encrypt(b"", &mut x).is_err());
        a.counter.set(0);
        a.bytes.set(MAX_BYTES);
        assert!(a.encrypt(b"x", &mut x).is_err());
    }

    #[test]
    fn replay_window_allows_reordering_but_rejects_old_epochs() {
        let a = LinkGcm::new(&[42; 64], &[1; 16], true);
        let b = LinkGcm::new(&[42; 64], &[1; 16], false);
        let mut packets = [[0u8; 32]; 4];
        for packet in &mut packets {
            a.encrypt(b"data", packet).unwrap();
        }
        let mut out = [0u8; 32];
        b.decrypt(&packets[2], &mut out).unwrap();
        b.decrypt(&packets[0], &mut out).unwrap();
        assert!(b.decrypt(&packets[0], &mut out).is_err());
        let mut forged = packets[3];
        forged[4..12].copy_from_slice(&((1u64 << 20) + 3).to_be_bytes());
        assert!(b.decrypt(&forged, &mut out).is_err());
        b.decrypt(&packets[1], &mut out).unwrap();
        b.decrypt(&packets[3], &mut out).unwrap();
        // Channel seq wraps after 65536 messages. A replay from the prior
        // epoch must not become a new message even when its seq matches.
        a.counter.set(65536);
        let mut next_epoch = [0u8; 32];
        a.encrypt(b"data", &mut next_epoch).unwrap();
        b.decrypt(&next_epoch, &mut out).unwrap();
        assert!(b.decrypt(&packets[0], &mut out).is_err());
        assert!(b.decrypt(&packets[3], &mut out).is_err());
    }
}
