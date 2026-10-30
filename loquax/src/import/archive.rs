//! Imported history as home node archive entries
//! ([`crate::home::KIND_IMPORTED`], leviculum 1150): an import on a Loquax
//! Mailbox writes each message into the archive too, so its clients take it
//! on their next `/sync`, as they take what the home node received.
//!
//! An imported message from an app that kept no packed bytes (Ratspeak) has
//! no signed form to archive, so the entry carries the row as the store
//! keeps it, never as a message anyone signed:
//!
//! ```text
//! [1, origin: str, peer: bin16, id: bin32, outbound: bool,
//!  timestamp: f64 | nil, received_at: int | nil, title: bin, content: str,
//!  delivery: uint | nil]
//! ```
//!
//! The leading 1 is the entry's version. A client from before this kind
//! finds no message id in it and passes it by, as it passes every kind it
//! does not know.

use leviculum_lxmf::msgpack;

use crate::store::{state_code, state_of, ImportRow};
use crate::ui::Signature;

/// The entry version this module writes and reads.
const VERSION: u64 = 1;

/// One imported row as an archive entry's bytes.
pub fn entry(origin: &str, peer: &[u8; 16], row: &ImportRow) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 10);
    msgpack::uint(&mut out, VERSION);
    msgpack::string(&mut out, origin);
    msgpack::bin(&mut out, peer);
    msgpack::bin(&mut out, &row.id);
    msgpack::bool(&mut out, row.outbound);
    match row.timestamp {
        Some(ts) => msgpack::f64(&mut out, ts),
        None => msgpack::nil(&mut out),
    }
    match row.received_at {
        Some(at) => msgpack::int(&mut out, at),
        None => msgpack::nil(&mut out),
    }
    msgpack::bin(&mut out, &row.title);
    msgpack::string(&mut out, &row.content);
    match row.delivery {
        Some(state) => msgpack::uint(&mut out, state_code(state) as u64),
        None => msgpack::nil(&mut out),
    }
    out
}

/// An entry's origin, peer and row; `None` for bytes of another shape or a
/// later version.
pub fn read_entry(data: &[u8]) -> Option<(String, [u8; 16], ImportRow)> {
    read(data).ok().flatten()
}

fn read(d: &[u8]) -> Result<Option<(String, [u8; 16], ImportRow)>, msgpack::Error> {
    let mut p = 0;
    if msgpack::array_len(d, &mut p)? != 10 || msgpack::read_uint(d, &mut p)? != VERSION {
        return Ok(None);
    }
    let origin = msgpack::read_str(d, &mut p)?.to_string();
    let Ok(peer) = <[u8; 16]>::try_from(msgpack::read_bin(d, &mut p)?) else {
        return Ok(None);
    };
    let Ok(id) = <[u8; 32]>::try_from(msgpack::read_bin(d, &mut p)?) else {
        return Ok(None);
    };
    let outbound = msgpack::read_bool(d, &mut p)?;
    let timestamp = match msgpack::peek_kind(d, p)? {
        msgpack::Kind::Nil => msgpack::read_nil(d, &mut p).map(|()| None)?,
        _ => Some(msgpack::read_number_f64(d, &mut p)?),
    };
    let received_at = match msgpack::peek_kind(d, p)? {
        msgpack::Kind::Nil => msgpack::read_nil(d, &mut p).map(|()| None)?,
        _ => Some(msgpack::read_int(d, &mut p)?),
    };
    let title = msgpack::read_bin(d, &mut p)?.to_vec();
    let content = msgpack::read_str(d, &mut p)?.to_string();
    let delivery = match msgpack::peek_kind(d, p)? {
        msgpack::Kind::Nil => msgpack::read_nil(d, &mut p).map(|()| None)?,
        _ => state_of(i64::try_from(msgpack::read_uint(d, &mut p)?).unwrap_or(-1)),
    };
    Ok(Some((
        origin,
        peer,
        ImportRow {
            id,
            outbound,
            timestamp,
            received_at,
            title,
            content,
            fields: Vec::new(),
            delivery,
            signature: Signature::Unverified,
            origin_verdict: None,
            held: None,
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_ui::delivery::DeliveryState;

    #[test]
    fn an_entry_reads_back_as_written_and_another_shape_is_none() {
        let row = ImportRow {
            id: [3; 32],
            outbound: true,
            timestamp: Some(1_761_350_400.25),
            received_at: Some(1_761_350_400),
            title: b"t".to_vec(),
            content: "hello".to_string(),
            fields: Vec::new(),
            delivery: Some(DeliveryState::Delivered),
            signature: Signature::Unverified,
            origin_verdict: None,
            held: None,
        };
        let bytes = entry("ratspeak", &[7; 16], &row);
        assert_eq!(
            read_entry(&bytes),
            Some(("ratspeak".to_string(), [7; 16], row.clone()))
        );
        let inbound = ImportRow {
            outbound: false,
            timestamp: None,
            delivery: None,
            ..row
        };
        let bytes = entry("ratspeak", &[7; 16], &inbound);
        assert_eq!(read_entry(&bytes).map(|(_, _, r)| r), Some(inbound));
        assert_eq!(read_entry(&[0x90]), None);
        assert_eq!(read_entry(&[3; 32]), None);
    }
}
