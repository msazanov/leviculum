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
//!  delivery: uint | nil, (fields: bin)]
//! ```
//!
//! The leading 1 is the entry's version. A client from before this kind
//! finds no message id in it and passes it by, as it passes every kind it
//! does not know. `fields`, the store's packed map
//! ([`crate::telemetry::pack_fields`]), is there only when the row has any:
//! an imported reply or reaction (leviculum 1158). A client from before it
//! passes such an entry by and keeps the rest.
//!
//! An app that kept the signed bytes (NomadNet, leviculum 1162) gives a row
//! more than version 1 holds: our own verdict on its signature, the source
//! app's, and, for a received message whose sender's key is not known yet,
//! the sender and the signed bytes the store keeps for the check
//! (`ImportRow::held`). Such a row is version 2, so a device stores the row
//! the mailbox stores:
//!
//! ```text
//! [2, origin, peer, id, outbound, timestamp, received_at, title, content,
//!  delivery, fields: bin | nil, signature: uint, origin_verdict: bool | nil,
//!  held: [source: bin16, packed: bin] | nil]
//! ```
//!
//! `signature` is the store's code (0 unverified, 1 verified, 2 invalid). A
//! row with none of the three stays version 1, so every Ratspeak entry reads
//! as before. A client from before version 2 finds another version, says
//! "an imported entry of another shape" in its log and passes it by.

use leviculum_lxmf::msgpack;

use crate::store::{signature_code, signature_of, state_code, state_of, ImportRow};
use crate::telemetry::{pack_fields, unpack_fields};
use crate::ui::Signature;

/// The entry version of a row the source kept no signed bytes for.
const VERSION: u64 = 1;

/// The entry version of a row with a signature's provenance.
const SIGNED: u64 = 2;

/// Version 2's length: version 1's eleven and the three provenance items.
const SIGNED_LEN: usize = 14;

/// One imported row as an archive entry's bytes.
pub fn entry(origin: &str, peer: &[u8; 16], row: &ImportRow) -> Vec<u8> {
    let mut out = Vec::new();
    let signed = row.signature != Signature::Unverified
        || row.origin_verdict.is_some()
        || row.held.is_some();
    let with_fields = !row.fields.is_empty();
    match (signed, with_fields) {
        (true, _) => msgpack::array(&mut out, SIGNED_LEN),
        (false, true) => msgpack::array(&mut out, 11),
        (false, false) => msgpack::array(&mut out, 10),
    }
    msgpack::uint(&mut out, if signed { SIGNED } else { VERSION });
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
    match (signed, with_fields) {
        (_, true) => msgpack::bin(&mut out, &pack_fields(&row.fields)),
        (true, false) => msgpack::nil(&mut out),
        (false, false) => {}
    }
    if signed {
        msgpack::uint(&mut out, signature_code(row.signature) as u64);
        match row.origin_verdict {
            Some(verdict) => msgpack::bool(&mut out, verdict),
            None => msgpack::nil(&mut out),
        }
        match &row.held {
            Some((source, packed)) => {
                msgpack::array(&mut out, 2);
                msgpack::bin(&mut out, source);
                msgpack::bin(&mut out, packed);
            }
            None => msgpack::nil(&mut out),
        }
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
    let len = msgpack::array_len(d, &mut p)?;
    let version = msgpack::read_uint(d, &mut p)?;
    let known = match version {
        VERSION => (10..=11).contains(&len),
        SIGNED => len == SIGNED_LEN,
        _ => false,
    };
    if !known {
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
    let packed_fields = match (version, len) {
        (VERSION, 10) => None,
        (VERSION, _) => Some(msgpack::read_bin(d, &mut p)?),
        _ => match msgpack::peek_kind(d, p)? {
            msgpack::Kind::Nil => msgpack::read_nil(d, &mut p).map(|()| None)?,
            _ => Some(msgpack::read_bin(d, &mut p)?),
        },
    };
    let fields = match packed_fields.map(unpack_fields) {
        None => Vec::new(),
        Some(Some(fields)) => fields,
        Some(None) => return Ok(None),
    };
    let (signature, origin_verdict, held) = match version {
        SIGNED => {
            let signature =
                signature_of(i64::try_from(msgpack::read_uint(d, &mut p)?).unwrap_or(-1));
            let origin_verdict = match msgpack::peek_kind(d, p)? {
                msgpack::Kind::Nil => msgpack::read_nil(d, &mut p).map(|()| None)?,
                _ => Some(msgpack::read_bool(d, &mut p)?),
            };
            let held = match msgpack::peek_kind(d, p)? {
                msgpack::Kind::Nil => msgpack::read_nil(d, &mut p).map(|()| None)?,
                _ => {
                    if msgpack::array_len(d, &mut p)? != 2 {
                        return Ok(None);
                    }
                    let Ok(source) = <[u8; 16]>::try_from(msgpack::read_bin(d, &mut p)?) else {
                        return Ok(None);
                    };
                    Some((source, msgpack::read_bin(d, &mut p)?.to_vec()))
                }
            };
            (signature, origin_verdict, held)
        }
        _ => (Signature::Unverified, None, None),
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
            fields,
            delivery,
            signature,
            origin_verdict,
            held,
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
        assert_eq!(bytes[0], 0x9a, "no fields, an array of ten");
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
        assert_eq!(read_entry(&bytes).map(|(_, _, r)| r), Some(inbound.clone()));
        // A reply carries its fields; the entry without them stays ten long.
        let reply = ImportRow {
            fields: vec![(0x30, vec![0xc4, 0x01, 0x07])],
            ..inbound
        };
        let bytes = entry("ratspeak", &[7; 16], &reply);
        assert_eq!(bytes[0], 0x9b, "an array of eleven");
        assert_eq!(read_entry(&bytes).map(|(_, _, r)| r), Some(reply));
        assert_eq!(read_entry(&[0x90]), None);
        assert_eq!(read_entry(&[3; 32]), None);
    }

    /// 1162: a NomadNet row keeps our verdict, the source's and the held
    /// bytes, so its entry is version 2; a version 1 reader's checks, the
    /// length ten or eleven and the version 1, both pass it by.
    #[test]
    fn a_signed_row_is_version_2_and_reads_back_whole() {
        let held = ImportRow {
            id: [4; 32],
            outbound: false,
            timestamp: Some(2_000.0),
            received_at: Some(2_001),
            title: Vec::new(),
            content: "an answer".to_string(),
            fields: Vec::new(),
            delivery: None,
            signature: Signature::Unverified,
            origin_verdict: None,
            held: Some(([9; 16], vec![1, 2, 3])),
        };
        let bytes = entry("nomadnet", &[7; 16], &held);
        assert_eq!(&bytes[..2], [0x9e, 2], "an array of fourteen, version 2");
        assert_eq!(
            read_entry(&bytes),
            Some(("nomadnet".to_string(), [7; 16], held.clone()))
        );
        let verified = ImportRow {
            outbound: true,
            signature: Signature::Verified,
            origin_verdict: Some(false),
            held: None,
            fields: vec![(0x30, vec![0xc4, 0x01, 0x07])],
            delivery: Some(DeliveryState::Sent),
            ..held
        };
        let bytes = entry("nomadnet", &[7; 16], &verified);
        assert_eq!(&bytes[..2], [0x9e, 2]);
        assert_eq!(read_entry(&bytes).map(|(_, _, r)| r), Some(verified));
    }
}
