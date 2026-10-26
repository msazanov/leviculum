//! The system clipboard as a terminal reaches it: an OSC 52 sequence the
//! shell writes to the terminal, which copies the payload. It works over SSH,
//! where no local clipboard API would (`docs/src/concepts/loquax-ui.md`, "The
//! mouse as a first-class citizen").
//!
//! Pure, so the encoding is tested without a terminal: lnomad's `y` and
//! loquax's drag selection both copy through it.

use alloc::format;
use alloc::string::String;

/// `text` as an OSC 52 clipboard-set sequence: `ESC ] 52 ; c ;
/// <base64(text)> ST`, with ST = `ESC \`.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x1b\\", base64_encode(text.as_bytes()))
}

/// Standard base64 (RFC 4648, with `=` padding) of `bytes`. Small and local so
/// the OSC 52 helper needs no extra dependency.
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(n >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(plain.as_bytes()), encoded, "{plain:?}");
        }
    }

    #[test]
    fn osc52_wraps_the_payload_in_the_set_sequence() {
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x1b\\");
    }
}
