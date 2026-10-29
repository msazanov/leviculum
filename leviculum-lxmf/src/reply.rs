//! Replies: a message that answers an earlier one and says which.
//!
//! The reference defines two shapes and leaves the choice to the client
//! (`reference/LXMF/LXMF/LXMF.py:23-26`, `:112-118`): `FIELD_REPLY_TO`, the
//! target's full LXMessage.hash as bytes, with `FIELD_REPLY_QUOTE`, the
//! quoted content as UTF-8 bytes beside it; and `FIELD_COMMENT`, a dict
//! `{COMMENT_FOR: the target's hash}`. The deployed clients write the first:
//! Columba (`FIELD_REPLY_HASH = 0x30`, torlando-tech/columba `9222e38`,
//! `columba/rns-api/src/main/java/network/columba/app/rns/api/util/LxmfFields.kt:125-135`),
//! MeshChatX (`send_message`, Quad4-Software/MeshChatX `4378392`,
//! `meshchatx/meshchat.py`) and Ratspeak (`standard_chat_fields`, read in
//! `decode_ratspeak_chat_extension_checked`, ratspeak/Ratspeak `7da1fe3`,
//! `crates/ratspeak-runtime/src/lxmf.rs`). None
//! of them writes or reads `FIELD_COMMENT`, so [`Reply::fields`] writes the
//! pair and [`Reply::from_fields`] reads either.
//!
//! In every shape the reply's text is the message's content, so a client
//! without replies shows it as a message.

use crate::constants::{FIELD_COMMENT, FIELD_REPLY_QUOTE, FIELD_REPLY_TO};
use crate::message::Field;
use crate::msgpack::{self, Kind};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// `FIELD_COMMENT` dict key (`reference/LXMF/LXMF/LXMF.py:118`).
pub const COMMENT_FOR: i64 = 0x00;

/// What a message replies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// The full LXMessage hash of the message replied to.
    pub to: [u8; 32],
    /// What of it the reply quotes, so a reader without the target can
    /// still show it; `None` for none.
    pub quote: Option<String>,
}

impl Reply {
    /// The fields the deployed clients read: `FIELD_REPLY_TO`, and
    /// `FIELD_REPLY_QUOTE` when there is a quote, both msgpack `bin`.
    pub fn fields(&self) -> Vec<Field> {
        let mut to = Vec::new();
        msgpack::bin(&mut to, &self.to);
        let mut fields = vec![(FIELD_REPLY_TO, to)];
        if let Some(quote) = self.quote.as_ref().filter(|q| !q.is_empty()) {
            let mut value = Vec::new();
            msgpack::bin(&mut value, quote.as_bytes());
            fields.push((FIELD_REPLY_QUOTE, value));
        }
        fields
    }

    /// The reply `fields` carry: `FIELD_REPLY_TO` with its quote first,
    /// else `FIELD_COMMENT`. `None` without either, or with a target that
    /// is not 32 bytes. A quote that is not UTF-8 is dropped, the reply
    /// kept.
    pub fn from_fields(fields: &[Field]) -> Option<Self> {
        let value = |key: i64| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| v);
        if let Some(to) = value(FIELD_REPLY_TO).and_then(|v| hash(v)) {
            let quote = value(FIELD_REPLY_QUOTE)
                .and_then(|v| bytes(v))
                .and_then(|b| core::str::from_utf8(b).ok())
                .filter(|q| !q.is_empty())
                .map(String::from);
            return Some(Self { to, quote });
        }
        let to = comment_for(value(FIELD_COMMENT)?)?;
        Some(Self { to, quote: None })
    }
}

/// `FIELD_COMMENT`'s value for a reply to `to`: `{COMMENT_FOR: bin(to)}`.
pub fn comment_field_value(to: &[u8; 32]) -> Vec<u8> {
    let mut o = Vec::new();
    msgpack::map(&mut o, 1);
    msgpack::int(&mut o, COMMENT_FOR);
    msgpack::bin(&mut o, to);
    o
}

/// The target of a `FIELD_COMMENT` value; unknown keys are skipped.
fn comment_for(d: &[u8]) -> Option<[u8; 32]> {
    let mut p = 0;
    let entries = msgpack::map_len(d, &mut p).ok()?;
    let mut to = None;
    for _ in 0..entries {
        let key = msgpack::raw(d, &mut p).ok()?;
        let value = msgpack::raw(d, &mut p).ok()?;
        let mut kp = 0;
        if msgpack::read_int(key, &mut kp) == Ok(COMMENT_FOR) {
            to = hash(value);
        }
    }
    to
}

/// The bytes of a `bin` or a `str` value.
fn bytes(value: &[u8]) -> Option<&[u8]> {
    let mut p = 0;
    match msgpack::peek_kind(value, 0).ok()? {
        Kind::Str => msgpack::read_str(value, &mut p).ok().map(str::as_bytes),
        _ => msgpack::read_bin(value, &mut p).ok(),
    }
}

fn hash(value: &[u8]) -> Option<[u8; 32]> {
    <[u8; 32]>::try_from(bytes(value)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_reads_back_as_written() {
        let reply = Reply {
            to: [0xab; 32],
            quote: Some("hello".into()),
        };
        assert_eq!(Reply::from_fields(&reply.fields()), Some(reply));
        let bare = Reply {
            to: [0xab; 32],
            quote: None,
        };
        assert_eq!(bare.fields().len(), 1);
        assert_eq!(Reply::from_fields(&bare.fields()), Some(bare.clone()));
        let empty = Reply {
            quote: Some(String::new()),
            ..bare.clone()
        };
        assert_eq!(empty.fields(), bare.fields());
    }

    #[test]
    fn a_comment_is_read_as_a_reply_without_quote() {
        let fields = vec![(FIELD_COMMENT, comment_field_value(&[7; 32]))];
        assert_eq!(
            Reply::from_fields(&fields),
            Some(Reply {
                to: [7; 32],
                quote: None
            })
        );
    }

    #[test]
    fn what_names_no_target_is_none() {
        let mut short = Vec::new();
        msgpack::bin(&mut short, &[1; 16]);
        assert_eq!(Reply::from_fields(&[(FIELD_REPLY_TO, short)]), None);
        assert_eq!(Reply::from_fields(&[(FIELD_COMMENT, vec![0xc0])]), None);
        assert_eq!(Reply::from_fields(&[]), None);
    }
}
