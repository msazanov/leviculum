//! The backup of an address (1152; `docs/drafts/loquax-onboarding-2026-10.md`,
//! sections 2 and 4): the mailbox identity's private key, sealed with a
//! passphrase, as one line of text. The same line is the encrypted file the
//! app saves and the printable QR code, so a restore reads either the same
//! way.
//!
//! The seal is what the workspace already has, no new crate:
//!
//! - the key from the passphrase is PBKDF2 (RFC 8018, section 5.2) with
//!   HMAC-SHA256 as its PRF, built on [`leviculum_core::crypto::hmac_sha256`],
//!   64 bytes from a random 16-byte salt and [`ITERATIONS`] rounds;
//! - the box is Reticulum's token ([`leviculum_core::crypto::encrypt_token`]):
//!   AES-256-CBC, then HMAC-SHA256 over the IV and the ciphertext, which is
//!   what tells a wrong passphrase from a right one before anything is
//!   decrypted.
//!
//! The line is `lqxb1` and RFC 4648 base32 of: one version byte, the rounds
//! (four bytes, big endian), the salt, the token. 244 characters for the 64
//! bytes of a key; upper case, they fit the QR alphanumeric mode.

use alloc::string::String;
use alloc::vec::Vec;

use leviculum_core::crypto::{decrypt_token, encrypt_token, hmac_sha256, token_len};

use super::code::{base32, unbase32};

/// What a backup line starts with.
pub const PREFIX: &str = "lqxb1";
/// The rounds of a new seal: about half a second on a phone of 2020. Read
/// from the line on opening, so a later change leaves old backups readable.
pub const ITERATIONS: u32 = 200_000;
/// The most rounds a line may ask for: a line is untrusted input, and a
/// forged count must not hold the app for an hour.
const MAX_ITERATIONS: u32 = 10_000_000;
const VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const KEY_LEN: usize = 64;
const HEAD_LEN: usize = 1 + 4 + SALT_LEN;

/// Why a line did not open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupError {
    /// Not a backup line, or a damaged one.
    NotABackup,
    /// A backup of a later version than this app reads.
    Version(u8),
    /// The passphrase is not the one it was sealed with.
    WrongPassphrase,
    /// An empty passphrase seals nothing.
    NoPassphrase,
}

impl core::fmt::Display for BackupError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotABackup => write!(f, "that is not a Loquax backup, or it is damaged"),
            Self::Version(version) => {
                write!(
                    f,
                    "this backup is of version {version}; update Loquax to read it"
                )
            }
            Self::WrongPassphrase => write!(f, "the passphrase does not open this backup"),
            Self::NoPassphrase => write!(f, "a backup needs a passphrase"),
        }
    }
}

/// Seal `private_key` (an identity's 64 bytes) with `passphrase`, the salt
/// and the IV fresh from `rng`.
pub fn seal(
    private_key: &[u8; KEY_LEN],
    passphrase: &str,
    rng: &mut impl rand_core::CryptoRngCore,
) -> Result<String, BackupError> {
    let mut salt = [0u8; SALT_LEN];
    let mut iv = [0u8; 16];
    rng.fill_bytes(&mut salt);
    rng.fill_bytes(&mut iv);
    seal_with(private_key, passphrase, salt, iv, ITERATIONS)
}

/// [`seal`] with the salt, the IV and the rounds given: for tests, which
/// cannot wait for the real rounds in a debug build.
pub fn seal_with(
    private_key: &[u8; KEY_LEN],
    passphrase: &str,
    salt: [u8; SALT_LEN],
    iv: [u8; 16],
    iterations: u32,
) -> Result<String, BackupError> {
    if passphrase.is_empty() {
        return Err(BackupError::NoPassphrase);
    }
    let key = pbkdf2(passphrase.as_bytes(), &salt, iterations);
    let mut out = Vec::with_capacity(HEAD_LEN + token_len(KEY_LEN));
    out.push(VERSION);
    out.extend_from_slice(&iterations.to_be_bytes());
    out.extend_from_slice(&salt);
    let mut token = alloc::vec![0u8; token_len(KEY_LEN)];
    let written =
        encrypt_token(&key, &iv, private_key, &mut token).map_err(|_| BackupError::NotABackup)?;
    out.extend_from_slice(&token[..written]);
    let mut line = String::from(PREFIX);
    line.push_str(&base32(&out));
    Ok(line)
}

/// Open a backup line with `passphrase`: the private key it holds. Upper
/// case, spaces and line breaks are taken as a scan or a print gives them.
pub fn open(text: &str, passphrase: &str) -> Result<[u8; KEY_LEN], BackupError> {
    if passphrase.is_empty() {
        return Err(BackupError::NoPassphrase);
    }
    let compact: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let body = compact
        .strip_prefix(PREFIX)
        .ok_or(BackupError::NotABackup)?;
    let bytes = unbase32(body).ok_or(BackupError::NotABackup)?;
    if bytes.len() < HEAD_LEN {
        return Err(BackupError::NotABackup);
    }
    if bytes[0] != VERSION {
        return Err(BackupError::Version(bytes[0]));
    }
    let mut rounds = [0u8; 4];
    rounds.copy_from_slice(&bytes[1..5]);
    let iterations = u32::from_be_bytes(rounds);
    if iterations == 0 || iterations > MAX_ITERATIONS {
        return Err(BackupError::NotABackup);
    }
    let salt = &bytes[5..HEAD_LEN];
    let key = pbkdf2(passphrase.as_bytes(), salt, iterations);
    let mut plain = alloc::vec![0u8; bytes.len()];
    let read =
        decrypt_token(&key, &bytes[HEAD_LEN..], &mut plain).map_err(|error| match error {
            leviculum_core::crypto::TokenError::HmacVerificationFailed => {
                BackupError::WrongPassphrase
            }
            _ => BackupError::NotABackup,
        })?;
    plain
        .get(..read)
        .and_then(|plain| <[u8; KEY_LEN]>::try_from(plain).ok())
        .ok_or(BackupError::NotABackup)
}

/// Whether `text` starts like a backup line, in either case.
pub fn looks_like(text: &str) -> bool {
    text.trim_start()
        .get(..PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PREFIX))
}

/// PBKDF2-HMAC-SHA256, 64 bytes: two blocks of RFC 8018's `F`.
fn pbkdf2(password: &[u8], salt: &[u8], iterations: u32) -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    for (index, block) in key.chunks_mut(32).enumerate() {
        let mut first = Vec::with_capacity(salt.len() + 4);
        first.extend_from_slice(salt);
        // Blocks count from one.
        first.extend_from_slice(&(index as u32 + 1).to_be_bytes());
        let mut u = hmac_sha256(password, &first);
        let mut t = u;
        for _ in 1..iterations {
            u = hmac_sha256(password, &u);
            for (t, u) in t.iter_mut().zip(u.iter()) {
                *t ^= u;
            }
        }
        block.copy_from_slice(&t);
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 64] = [0x42; 64];

    fn sealed() -> String {
        seal_with(&KEY, "Kastanienallee 7", [7; 16], [9; 16], 1000).expect("sealed")
    }

    /// RFC 7914, section 11, the PBKDF2-HMAC-SHA256 vector: "passwd",
    /// "salt", one round, 64 bytes.
    #[test]
    fn the_key_derivation_matches_the_rfc_vector() {
        let expected = [
            0x55, 0xac, 0x04, 0x6e, 0x56, 0xe3, 0x08, 0x9f, 0xec, 0x16, 0x91, 0xc2, 0x25, 0x44,
            0xb6, 0x05, 0xf9, 0x41, 0x85, 0x21, 0x6d, 0xde, 0x04, 0x65, 0xe6, 0x8b, 0x9d, 0x57,
            0xc2, 0x0d, 0xac, 0xbc, 0x49, 0xca, 0x9c, 0xcc, 0xf1, 0x79, 0xb6, 0x45, 0x99, 0x16,
            0x64, 0xb3, 0x9d, 0x77, 0xef, 0x31, 0x7c, 0x71, 0xb8, 0x45, 0xb1, 0xe3, 0x0b, 0xd5,
            0x09, 0x11, 0x20, 0x41, 0xd3, 0xa1, 0x97, 0x83,
        ];
        assert_eq!(pbkdf2(b"passwd", b"salt", 1), expected);
    }

    #[test]
    fn a_backup_opens_with_its_passphrase_and_only_with_it() {
        let line = sealed();
        assert!(line.starts_with(PREFIX));
        assert_eq!(line.len(), 244);
        assert_eq!(open(&line, "Kastanienallee 7"), Ok(KEY));
        assert_eq!(
            open(&line, "Kastanienallee 8"),
            Err(BackupError::WrongPassphrase)
        );
        assert_eq!(open(&line, ""), Err(BackupError::NoPassphrase));
    }

    #[test]
    fn a_scanned_or_printed_line_still_opens() {
        let line = sealed().to_ascii_uppercase();
        let wrapped: String = line
            .as_bytes()
            .chunks(40)
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(looks_like(&wrapped));
        assert_eq!(open(&wrapped, "Kastanienallee 7"), Ok(KEY));
    }

    #[test]
    fn a_damaged_or_foreign_line_is_refused() {
        let line = sealed();
        let mut damaged = line.clone().into_bytes();
        let at = damaged.len() - 10;
        damaged[at] = if damaged[at] == b'a' { b'b' } else { b'a' };
        let damaged = String::from_utf8(damaged).expect("ascii");
        assert_eq!(
            open(&damaged, "Kastanienallee 7"),
            Err(BackupError::WrongPassphrase)
        );
        assert_eq!(
            open("lqxh1abcdef", "Kastanienallee 7"),
            Err(BackupError::NotABackup)
        );
        let mut later = line.into_bytes();
        // The version byte's first five bits are the body's first character.
        later[PREFIX.len()] = b'b';
        let later = String::from_utf8(later).expect("ascii");
        assert!(matches!(
            open(&later, "Kastanienallee 7"),
            Err(BackupError::Version(_))
        ));
    }

    #[cfg(feature = "qr")]
    #[test]
    fn the_paper_code_fits_a_qr_code_a_phone_reads() {
        let line = sealed();
        let qr = super::super::qr::encode(&line).expect("a QR code");
        assert!(
            qr.version().value() <= 12,
            "version {}",
            qr.version().value()
        );
    }
}
