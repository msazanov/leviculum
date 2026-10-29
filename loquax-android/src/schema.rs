//! What crosses JNI, as msgpack maps with string keys
//! (`rmp_serde::to_vec_named`). Java reads and writes them with its own
//! small codec (`MsgPack.java`), so the shapes stay flat: strings, integers,
//! booleans, arrays of maps. Addresses and message ids travel as lowercase
//! hex, the form the screen shows and hands back.
//!
//! # Version 1
//!
//! Java to Rust:
//!
//! | call | payload |
//! |---|---|
//! | `start` | [`Start`] |
//! | `command` | [`Request`], tagged by `t`: `open`, `close`, `send`, `react`, `pull` |
//! | `query` | [`Query`], tagged by `t`: `conversations`, `messages` |
//!
//! Rust to Java:
//!
//! | call | answer |
//! |---|---|
//! | `start` | [`Started`] |
//! | `command` | [`Done`] |
//! | `query` | [`Conversations`] or [`Messages`], or [`Done`] with the error |
//! | `nextEvent` | [`Event`], tagged by `t`: `changed`, `notice`, `sync`, `link`, `stopped`; none when the wait ran out |
//!
//! A change that Java cannot read is a new [`VERSION`]: every answer
//! carries it, and Java refuses one it does not know. A field or an event
//! added is no such change: Java reads keys by name and skips an event it
//! does not know, so `link` (event and field) came within version 1, and a
//! code with the address behind it in [`Start::code`] is still a string.

use serde::{Deserialize, Serialize};

/// The schema's version, in every answer as `v`.
pub const VERSION: u32 = 1;

/// Start the session: on the first run with the code and the address, after
/// that with neither (both are kept in `dir`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Start {
    /// The app's files directory: the loquax home on the phone.
    pub dir: String,
    /// The home node's TCP server, `host:port`; the saved one when absent.
    #[serde(default)]
    pub server: Option<String>,
    /// The admission code from `loquax home admit` (`lqxh1...`, or
    /// `lqxh1...@host:port` with the server behind it, which stands in for
    /// [`Start::server`] when that is absent); needed only while the phone
    /// is no client yet.
    #[serde(default)]
    pub code: Option<String>,
    /// The name the home node lists this phone under.
    #[serde(default)]
    pub name: Option<String>,
    /// The phone's offset from UTC in seconds, for the times the model
    /// writes into a message's history.
    #[serde(default)]
    pub offset: i32,
}

/// What became of [`Start`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    pub v: u32,
    pub ok: bool,
    /// Why not, in words for the screen.
    pub error: Option<String>,
    /// What the join screen has to ask for when `ok` is false: `code`,
    /// `server`, or nothing when the error is of another kind.
    pub need: Option<String>,
    /// The home node's LXMF address, which is this phone's own.
    pub address: Option<String>,
    /// The server the session talks to.
    pub server: Option<String>,
    /// Whether its TCP interface to the home node is up right now; the
    /// `link` event tells each change after.
    #[serde(default)]
    pub link: bool,
}

/// What the user did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Request {
    /// Opened a conversation, a new one by typing the address: its unread
    /// are read and marked so, and a pull starts.
    Open { peer: String },
    /// Went back to the list: no conversation is open, so what arrives
    /// stays unread.
    Close,
    /// Sent text in a conversation, through the home node's `/send`.
    Send { peer: String, body: String },
    /// Reacted to a message.
    React {
        peer: String,
        target: String,
        content: String,
    },
    /// Asked for a pull now: the refresh button.
    Pull,
}

/// What the screen wants to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Query {
    /// The conversation list, in the list's order.
    Conversations,
    /// One conversation's messages, oldest first.
    Messages { peer: String },
}

/// The answer to a call that has nothing else to say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Done {
    pub v: u32,
    pub ok: bool,
    pub error: Option<String>,
}

impl Done {
    pub fn ok() -> Self {
        Self {
            v: VERSION,
            ok: true,
            error: None,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            v: VERSION,
            ok: false,
            error: Some(error.into()),
        }
    }
}

/// One row of the conversation list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationRow {
    pub peer: String,
    /// The peer as every loquax screen names it: petname or announced name
    /// with the short hash, or the address.
    pub name: String,
    /// The last message's first line, a reaction as `reacted 👍`.
    pub last: String,
    /// Whether that message is ours.
    pub ours: bool,
    /// When it was written or came, seconds since 1970 UTC; 0 for none.
    pub at: i64,
    pub unread: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversations {
    pub v: u32,
    pub conversations: Vec<ConversationRow>,
}

/// A reaction under a message: its content, how many sent it, and whether
/// one of them is ours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chip {
    pub content: String,
    pub count: u32,
    pub ours: bool,
}

/// One bubble.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRow {
    /// The LXMF id; empty for a send the home node has not taken yet.
    pub id: String,
    pub ours: bool,
    pub body: String,
    /// Ours only: `queued`, `sending`, `sent`, `delivered` or `failed`.
    pub state: String,
    pub at: i64,
    pub chips: Vec<Chip>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Messages {
    pub v: u32,
    pub peer: String,
    pub name: String,
    pub messages: Vec<MessageRow>,
}

/// What happened, for the screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Event {
    /// Something the screen shows changed: ask again.
    Changed { v: u32 },
    /// A line for the user: what the terminal shows as a toast.
    Notice { v: u32, text: String },
    /// A pull began (`running`), ended (`done`, with how many entries
    /// came), or failed (`failed`, with why).
    Sync {
        v: u32,
        state: String,
        detail: String,
    },
    /// The TCP interface to the home node came up or went down. Once
    /// soon after the start, then on each change.
    Link { v: u32, up: bool },
    /// The session ended; `detail` says why when it was not asked to. One
    /// that ended by itself is gone from the library then: the next
    /// `start` makes a fresh one.
    Stopped { v: u32, detail: String },
}

/// Encode an answer.
pub fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    // A map of strings, numbers and vectors always encodes; an empty
    // answer is what Java takes for a broken library.
    rmp_serde::to_vec_named(value).unwrap_or_default()
}

/// Decode a request, the error in words.
pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, String> {
    rmp_serde::from_slice(bytes).map_err(|error| format!("the request did not decode: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_a_map_tagged_by_t() {
        let request = Request::Send {
            peer: "ab".repeat(16),
            body: "hallo".to_string(),
        };
        let bytes = encode(&request);
        // fixmap of three, then "t" => "send".
        assert_eq!(bytes[0], 0x83);
        assert_eq!(&bytes[1..3], b"\xa1t");
        assert_eq!(&bytes[3..8], b"\xa4send");
        assert_eq!(decode::<Request>(&bytes), Ok(request));
        assert_eq!(
            decode::<Request>(&encode(&Request::Pull)),
            Ok(Request::Pull)
        );
    }

    #[test]
    fn a_start_without_code_or_server_decodes() {
        #[derive(Serialize)]
        struct Bare {
            dir: String,
        }
        let bytes = encode(&Bare {
            dir: "/data".to_string(),
        });
        let start: Start = decode(&bytes).expect("a bare start");
        assert_eq!(start.dir, "/data");
        assert_eq!((start.server, start.code, start.offset), (None, None, 0));
    }

    #[test]
    fn garbage_is_an_error_in_words() {
        let error = decode::<Request>(b"\xc1").expect_err("not msgpack");
        assert!(error.starts_with("the request did not decode"), "{error}");
        let error = decode::<Request>(&encode(&Query::Conversations)).expect_err("a query");
        assert!(error.contains("conversations"), "{error}");
    }
}
