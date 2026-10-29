//! The requests a client sends the home node, and their answers, as
//! msgpack values on an RNS link (`docs/src/concepts/loquax-home-node.md`,
//! "The client protocol").
//!
//! ```text
//! /join  [token: bin16, name: str]           -> [status]
//! /sync  [cursor: uint]                       -> [status, first: uint, more: bool,
//!                                                 [[seq: uint, kind: uint, at: int, packed: bin], ...]]
//! /send  [client_id: bin16, destination: bin16, title: bin, content: bin,
//!         [[key: int, value: bin], ...]]      -> [status, id: bin32]
//! /mark  [flag: uint, [id: bin32, ...]]       -> [status]
//! ```
//!
//! A mark's flag is 0 for read and 1 for deleted ([`MarkFlag`]). The home
//! node keeps the request's bytes as the archive entry, so every client
//! reads the same mark the way the home node did.
//!
//! A field's value is its raw msgpack, carried as bytes, so the home node
//! signs exactly what the client composed.
//!
//! The status is the first element of every answer, so a refusal decodes
//! the same way whatever was asked. `first` is the oldest sequence the
//! archive still holds: a client whose cursor is older than `first - 1`
//! lost entries to the cap and can say so.

use alloc::vec::Vec;

use leviculum_lxmf::msgpack;

/// One archive entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its sequence: every entry after another has a larger one.
    pub seq: u64,
    /// What it records ([`super::KIND_RECEIVED`]).
    pub kind: u8,
    /// When the home node recorded it, unix seconds.
    pub at: i64,
    /// The LXMF message as it came, signature and all.
    pub packed: Vec<u8>,
}

/// One message a client hands the home node to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    /// The client's own id for it: the same id handed in again is the same
    /// message.
    pub client_id: [u8; 16],
    pub destination: [u8; 16],
    pub title: Vec<u8>,
    pub content: Vec<u8>,
    pub fields: Vec<leviculum_lxmf::Field>,
}

/// What a mark says of its messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkFlag {
    /// Read on one device: read on all.
    Read,
    /// Deleted on one device: deleted on all.
    Deleted,
}

impl MarkFlag {
    fn code(self) -> u64 {
        match self {
            Self::Read => 0,
            Self::Deleted => 1,
        }
    }

    fn from_code(code: u64) -> Option<Self> {
        match code {
            0 => Some(Self::Read),
            1 => Some(Self::Deleted),
            _ => None,
        }
    }

    /// The word logs use.
    pub fn word(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Deleted => "deleted",
        }
    }
}

/// One `/mark`: a flag over one or more messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub flag: MarkFlag,
    pub ids: Vec<[u8; 32]>,
}

/// The most message ids one mark carries: a conversation of unread
/// messages opened at once fits in one packet's worth of requests at most a
/// few times over, and a mark larger than this is split by the client.
pub const MARK_IDS_MAX: usize = 64;

/// One `/sync` answer's worth of the archive.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    pub entries: Vec<Entry>,
    /// Whether entries after the last one here wait for the next request.
    pub more: bool,
    /// The oldest sequence the archive holds; 0 when it is empty.
    pub first: u64,
}

/// How a request went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// The link was not identified, so there is no device to check.
    NotIdentified,
    /// The device is not admitted, or no longer.
    NotAdmitted,
    /// The admission token is unknown or used.
    BadToken,
    /// The request did not decode.
    Malformed,
    /// The home node could not answer (its store failed).
    Failed,
}

impl Status {
    fn code(self) -> u64 {
        match self {
            Self::Ok => 0,
            Self::NotIdentified => 1,
            Self::NotAdmitted => 2,
            Self::BadToken => 3,
            Self::Malformed => 4,
            Self::Failed => 5,
        }
    }

    fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            0 => Self::Ok,
            1 => Self::NotIdentified,
            2 => Self::NotAdmitted,
            3 => Self::BadToken,
            4 => Self::Malformed,
            5 => Self::Failed,
            _ => return None,
        })
    }
}

impl core::fmt::Display for Status {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Ok => "ok",
            Self::NotIdentified => "the home node did not learn who this device is",
            Self::NotAdmitted => "this device is not admitted to the home node (or was revoked)",
            Self::BadToken => "the home node does not know this admission code, or it was used",
            Self::Malformed => "the home node could not read the request",
            Self::Failed => "the home node could not answer",
        })
    }
}

/// Why an answer did not decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Malformed;

impl From<msgpack::Error> for Malformed {
    fn from(_: msgpack::Error) -> Self {
        Self
    }
}

pub fn join_request(token: &[u8; 16], name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::bin(&mut out, token);
    msgpack::string(&mut out, name);
    out
}

pub fn read_join_request(data: &[u8]) -> Result<([u8; 16], alloc::string::String), Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 2 {
        return Err(Malformed);
    }
    let token = <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    let name = msgpack::read_str(data, &mut p)?.into();
    Ok((token, name))
}

pub fn status_answer(status: Status) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 1);
    msgpack::uint(&mut out, status.code());
    out
}

pub fn read_status(data: &[u8]) -> Result<Status, Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? < 1 {
        return Err(Malformed);
    }
    Status::from_code(msgpack::read_uint(data, &mut p)?).ok_or(Malformed)
}

pub fn sync_request(cursor: u64) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 1);
    msgpack::uint(&mut out, cursor);
    out
}

pub fn read_sync_request(data: &[u8]) -> Result<u64, Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? < 1 {
        return Err(Malformed);
    }
    Ok(msgpack::read_uint(data, &mut p)?)
}

pub fn sync_answer(page: &Page) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 4);
    msgpack::uint(&mut out, Status::Ok.code());
    msgpack::uint(&mut out, page.first);
    msgpack::bool(&mut out, page.more);
    msgpack::array(&mut out, page.entries.len());
    for entry in &page.entries {
        msgpack::array(&mut out, 4);
        msgpack::uint(&mut out, entry.seq);
        msgpack::uint(&mut out, u64::from(entry.kind));
        msgpack::int(&mut out, entry.at);
        msgpack::bin(&mut out, &entry.packed);
    }
    out
}

pub fn send_request(submission: &Submission) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 5);
    msgpack::bin(&mut out, &submission.client_id);
    msgpack::bin(&mut out, &submission.destination);
    msgpack::bin(&mut out, &submission.title);
    msgpack::bin(&mut out, &submission.content);
    msgpack::array(&mut out, submission.fields.len());
    for (key, value) in &submission.fields {
        msgpack::array(&mut out, 2);
        msgpack::int(&mut out, *key);
        msgpack::bin(&mut out, value);
    }
    out
}

pub fn read_send_request(data: &[u8]) -> Result<Submission, Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 5 {
        return Err(Malformed);
    }
    let client_id =
        <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    let destination =
        <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    let title = msgpack::read_bin(data, &mut p)?.to_vec();
    let content = msgpack::read_bin(data, &mut p)?.to_vec();
    let count = msgpack::array_len(data, &mut p)?;
    let mut fields = Vec::new();
    for _ in 0..count {
        if msgpack::array_len(data, &mut p)? != 2 {
            return Err(Malformed);
        }
        let key = msgpack::read_int(data, &mut p)?;
        let value = msgpack::read_bin(data, &mut p)?.to_vec();
        fields.push((key, value));
    }
    Ok(Submission {
        client_id,
        destination,
        title,
        content,
        fields,
    })
}

pub fn send_answer(id: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::uint(&mut out, Status::Ok.code());
    msgpack::bin(&mut out, id);
    out
}

pub fn mark_request(mark: &Mark) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 2);
    msgpack::uint(&mut out, mark.flag.code());
    msgpack::array(&mut out, mark.ids.len());
    for id in &mark.ids {
        msgpack::bin(&mut out, id);
    }
    out
}

/// A mark, from a request or from the archive entry that keeps one. One
/// without ids, with more than [`MARK_IDS_MAX`] or with a flag this loquax
/// does not know is malformed.
pub fn read_mark(data: &[u8]) -> Result<Mark, Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 2 {
        return Err(Malformed);
    }
    let flag = MarkFlag::from_code(msgpack::read_uint(data, &mut p)?).ok_or(Malformed)?;
    let count = msgpack::array_len(data, &mut p)?;
    if count == 0 || count > MARK_IDS_MAX {
        return Err(Malformed);
    }
    let mut ids = Vec::new();
    for _ in 0..count {
        ids.push(<[u8; 32]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?);
    }
    Ok(Mark { flag, ids })
}

/// A `/send` answer: the message id, or the status that refused it.
pub fn read_send_answer(data: &[u8]) -> Result<Result<[u8; 32], Status>, Malformed> {
    let status = read_status(data)?;
    if status != Status::Ok {
        return Ok(Err(status));
    }
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 2 {
        return Err(Malformed);
    }
    msgpack::read_uint(data, &mut p)?;
    let id = <[u8; 32]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
    Ok(Ok(id))
}

/// A `/sync` answer: the page, or the status that refused it.
pub fn read_sync_answer(data: &[u8]) -> Result<Result<Page, Status>, Malformed> {
    let status = read_status(data)?;
    if status != Status::Ok {
        return Ok(Err(status));
    }
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? != 4 {
        return Err(Malformed);
    }
    msgpack::read_uint(data, &mut p)?;
    let first = msgpack::read_uint(data, &mut p)?;
    let more = msgpack::read_bool(data, &mut p)?;
    let count = msgpack::array_len(data, &mut p)?;
    let mut entries = Vec::new();
    for _ in 0..count {
        if msgpack::array_len(data, &mut p)? != 4 {
            return Err(Malformed);
        }
        let seq = msgpack::read_uint(data, &mut p)?;
        let kind = u8::try_from(msgpack::read_uint(data, &mut p)?).map_err(|_| Malformed)?;
        let at = msgpack::read_int(data, &mut p)?;
        let packed = msgpack::read_bin(data, &mut p)?.to_vec();
        entries.push(Entry {
            seq,
            kind,
            at,
            packed,
        });
    }
    Ok(Ok(Page {
        entries,
        more,
        first,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_join_request_round_trips() {
        let data = join_request(&[7; 16], "laptop");
        assert_eq!(read_join_request(&data), Ok(([7; 16], "laptop".into())));
    }

    #[test]
    fn a_page_round_trips() {
        let page = Page {
            entries: alloc::vec![
                Entry {
                    seq: 3,
                    kind: 0,
                    at: 1_800_000_000,
                    packed: alloc::vec![1, 2, 3],
                },
                Entry {
                    seq: 70_000,
                    kind: 0,
                    at: -1,
                    packed: alloc::vec![0; 300],
                },
            ],
            more: true,
            first: 2,
        };
        assert_eq!(read_sync_answer(&sync_answer(&page)), Ok(Ok(page)));
    }

    #[test]
    fn a_refusal_decodes_as_its_status() {
        let data = status_answer(Status::NotAdmitted);
        assert_eq!(read_sync_answer(&data), Ok(Err(Status::NotAdmitted)));
        assert_eq!(read_status(&data), Ok(Status::NotAdmitted));
    }

    #[test]
    fn a_submission_and_its_answer_round_trip() {
        let submission = Submission {
            client_id: [3; 16],
            destination: [4; 16],
            title: b"t".to_vec(),
            content: "grüß dich".as_bytes().to_vec(),
            fields: alloc::vec![(2, alloc::vec![0xc3]), (-1, alloc::vec![0x01])],
        };
        assert_eq!(
            read_send_request(&send_request(&submission)),
            Ok(submission)
        );
        assert_eq!(read_send_answer(&send_answer(&[8; 32])), Ok(Ok([8; 32])));
        assert_eq!(
            read_send_answer(&status_answer(Status::NotAdmitted)),
            Ok(Err(Status::NotAdmitted))
        );
    }

    #[test]
    fn a_mark_round_trips_and_an_empty_or_unknown_one_is_malformed() {
        let mark = Mark {
            flag: MarkFlag::Deleted,
            ids: alloc::vec![[1; 32], [2; 32]],
        };
        assert_eq!(read_mark(&mark_request(&mark)), Ok(mark));
        let empty = Mark {
            flag: MarkFlag::Read,
            ids: Vec::new(),
        };
        assert_eq!(read_mark(&mark_request(&empty)), Err(Malformed));
        let mut unknown = Vec::new();
        msgpack::array(&mut unknown, 2);
        msgpack::uint(&mut unknown, 7);
        msgpack::array(&mut unknown, 1);
        msgpack::bin(&mut unknown, &[1; 32]);
        assert_eq!(read_mark(&unknown), Err(Malformed));
    }

    #[test]
    fn a_cursor_round_trips() {
        assert_eq!(read_sync_request(&sync_request(u64::MAX)), Ok(u64::MAX));
    }
}
