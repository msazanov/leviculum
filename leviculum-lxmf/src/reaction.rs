//! `FIELD_REACTION`: an emoji, or any short text, attached later to an
//! earlier message.
//!
//! The reference fixes the shape and leaves the rest to the client
//! (`reference/LXMF/LXMF/LXMF.py:25`, `:104-110`): the field's value is a
//! dict `{REACTION_TO: the target's full LXMessage.hash, REACTION_CONTENT:
//! the reaction as UTF-8 bytes}`. Who reacted is not on the wire; it is the
//! message's source. One reaction per message, and no removal: a client that
//! wants one aggregates the reactions it holds per target.
//!
//! This codec produces and reads the value half of an LXMF
//! [`Field`](crate::Field) and nothing more. What a client draws for a
//! reaction, and what it does with one whose target it does not hold, is the
//! client's business.

use crate::constants::{REACTION_CONTENT, REACTION_TO};
use crate::msgpack::{self, Kind};
use alloc::string::String;
use alloc::vec::Vec;

/// One reaction, as a `FIELD_REACTION` value carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaction {
    /// The full LXMessage hash of the message reacted to.
    pub to: [u8; 32],
    /// What the reaction is: usually one emoji, never empty.
    pub content: String,
}

impl Reaction {
    /// The field value: a two-entry map, both values msgpack `bin`, as the
    /// reference's `umsgpack` packs Python `bytes`.
    pub fn encode_field_value(&self) -> Vec<u8> {
        let mut o = Vec::new();
        msgpack::map(&mut o, 2);
        msgpack::int(&mut o, REACTION_TO);
        msgpack::bin(&mut o, &self.to);
        msgpack::int(&mut o, REACTION_CONTENT);
        msgpack::bin(&mut o, self.content.as_bytes());
        o
    }

    /// Read a field value. `None` for anything that is not a reaction a
    /// reader could attach: not a map, no target, no content, content that
    /// is empty or not UTF-8.
    ///
    /// The deployed writers agree on `bin` for both values; a `str` is
    /// accepted for either, and a target written as 64 hex characters, as
    /// Ratspeak's reader accepts them (`standard_hash_hex`,
    /// ratspeak/Ratspeak `7da1fe3`, `crates/ratspeak-runtime/src/lxmf.rs`).
    /// Unknown keys are skipped and a repeated key resolves to its last
    /// occurrence, as a Python dict does.
    pub fn decode_field_value(d: &[u8]) -> Option<Self> {
        let mut p = 0;
        let entries = msgpack::map_len(d, &mut p).ok()?;
        let mut to = None;
        let mut content = None;
        for _ in 0..entries {
            let key = msgpack::raw(d, &mut p).ok()?;
            let value = msgpack::raw(d, &mut p).ok()?;
            let mut kp = 0;
            match msgpack::read_int(key, &mut kp) {
                Ok(REACTION_TO) => to = target(value),
                Ok(REACTION_CONTENT) => content = text(value),
                _ => {}
            }
        }
        let content = content.filter(|c| !c.is_empty())?;
        Some(Self { to: to?, content })
    }
}

/// The bytes of a `bin` or a `str` value.
fn bytes(value: &[u8]) -> Option<&[u8]> {
    let mut p = 0;
    match msgpack::peek_kind(value, 0).ok()? {
        Kind::Str => msgpack::read_str(value, &mut p).ok().map(str::as_bytes),
        _ => msgpack::read_bin(value, &mut p).ok(),
    }
}

fn target(value: &[u8]) -> Option<[u8; 32]> {
    let raw = bytes(value)?;
    if let Ok(hash) = <[u8; 32]>::try_from(raw) {
        return Some(hash);
    }
    if raw.len() != 64 {
        return None;
    }
    let mut hash = [0u8; 32];
    for (i, pair) in raw.chunks(2).enumerate() {
        let hex = core::str::from_utf8(pair).ok()?;
        hash[i] = u8::from_str_radix(hex, 16).ok()?;
    }
    Some(hash)
}

fn text(value: &[u8]) -> Option<String> {
    core::str::from_utf8(bytes(value)?).ok().map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thumbs() -> Reaction {
        Reaction {
            to: [0xab; 32],
            content: "\u{1F44D}".into(),
        }
    }

    #[test]
    fn a_reaction_reads_back_as_written() {
        let value = thumbs().encode_field_value();
        assert_eq!(Reaction::decode_field_value(&value), Some(thumbs()));
    }

    #[test]
    fn str_values_and_a_hex_target_are_read() {
        let mut o = Vec::new();
        msgpack::map(&mut o, 3);
        msgpack::int(&mut o, 0x07);
        msgpack::nil(&mut o);
        msgpack::int(&mut o, REACTION_TO);
        msgpack::string(&mut o, &"ab".repeat(32));
        msgpack::int(&mut o, REACTION_CONTENT);
        msgpack::string(&mut o, "\u{1F44D}");
        assert_eq!(Reaction::decode_field_value(&o), Some(thumbs()));
    }

    #[test]
    fn what_cannot_be_attached_is_none() {
        let mut empty = Vec::new();
        msgpack::map(&mut empty, 2);
        msgpack::int(&mut empty, REACTION_TO);
        msgpack::bin(&mut empty, &[0xab; 32]);
        msgpack::int(&mut empty, REACTION_CONTENT);
        msgpack::bin(&mut empty, b"");
        assert_eq!(Reaction::decode_field_value(&empty), None);

        let mut short = Vec::new();
        msgpack::map(&mut short, 2);
        msgpack::int(&mut short, REACTION_TO);
        msgpack::bin(&mut short, &[0xab; 16]);
        msgpack::int(&mut short, REACTION_CONTENT);
        msgpack::bin(&mut short, b"x");
        assert_eq!(Reaction::decode_field_value(&short), None);

        let mut no_target = Vec::new();
        msgpack::map(&mut no_target, 1);
        msgpack::int(&mut no_target, REACTION_CONTENT);
        msgpack::bin(&mut no_target, b"x");
        assert_eq!(Reaction::decode_field_value(&no_target), None);

        assert_eq!(Reaction::decode_field_value(&[0xc0]), None);
    }
}
