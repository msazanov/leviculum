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
//! | `command` | [`Request`], tagged by `t`: `open`, `close`, `send`, `react`, `pull`, `front` |
//! | `query` | [`Query`], tagged by `t`: `conversations`, `messages` |
//! | `renderMap` | [`MapRequest`] |
//!
//! Rust to Java:
//!
//! | call | answer |
//! |---|---|
//! | `start` | [`Started`] |
//! | `command` | [`Done`] |
//! | `query` | [`Conversations`] or [`Messages`], or [`Done`] with the error |
//! | `nextEvent` | [`Event`], tagged by `t`: `changed`, `notice`, `sync`, `link`, `stopped`; none when the wait ran out |
//! | `renderMap` | a frame: four bytes big endian, the length of a [`MapShown`], the [`MapShown`], and its `w` times `h` RGBA pixels |
//!
//! The map's pixels go raw behind the map, not as msgpack, so ten megabytes
//! of a phone's screen are one copy into a Java `byte[]` and one into its
//! bitmap. The map call needs no session: it draws the regions in the
//! app's maps directory and the pins the store knows, with or without one.
//! A change that Java cannot read is a new [`VERSION`]: every answer
//! carries it, and Java refuses one it does not know. A field or an event
//! added is no such change: Java reads keys by name and skips an event it
//! does not know, so `link` (event and field) came within version 1, and a
//! code with the address behind it in [`Start::code`] is still a string,
//! and the avatar's `initials`, `icon_fg` and `icon_bg` (1129) are fields,
//! as are the list row's `kind`, `reaction` and `state` and the list's
//! own `address` and `initials` (1130).
//! A request added is none either: an older app never sends `front`, and
//! the session then counts itself in the back, pulling on wakes and its
//! schedule as before. The map's margins and `moving` (1131) are such
//! fields too: an app that sends none gets the frame of the screen.

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
    /// A screen of the app came to the front (`true`) or left it: while in
    /// front and with the watching link down, the session pulls every
    /// minute.
    Front { front: bool },
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
    /// The avatar's letters (1129): from the name, or the address's first
    /// two hex digits for a peer without one.
    #[serde(default)]
    pub initials: String,
    /// The peer's own avatar colours, `rrggbb`, from the icon appearance of
    /// its latest message that carried one; absent, the app picks a colour
    /// from the address.
    #[serde(default)]
    pub icon_fg: Option<String>,
    #[serde(default)]
    pub icon_bg: Option<String>,
    /// What the last message is (1130): `text`, `position` for a report
    /// without text, `reaction`; the app words the last two in the phone's
    /// language, `last` stays the English line for an app that does not.
    #[serde(default)]
    pub kind: String,
    /// The reaction's content when `kind` is `reaction`.
    #[serde(default)]
    pub reaction: Option<String>,
    /// Ours only: the last message's delivery, as [`MessageRow::state`];
    /// the list draws its mark where an unread count would be.
    #[serde(default)]
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversations {
    pub v: u32,
    pub conversations: Vec<ConversationRow>,
    /// Our own address, the home node's, and its avatar's letters, for
    /// the list's bar (1130).
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub initials: String,
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
    /// The fix the message carries, a report's or a text's that came with
    /// telemetry, degrees times a million, which a tap opens the map on
    /// with the sender's pin selected; absent for any other message.
    #[serde(default)]
    pub lat_e6: Option<i32>,
    #[serde(default)]
    pub lon_e6: Option<i32>,
    /// The bubble's position line beside the fix: the coordinates short,
    /// `52.51627 N 13.37770 E`, and the accuracy, `±5 m`, when the sender
    /// gave one.
    #[serde(default)]
    pub place: Option<String>,
    #[serde(default)]
    pub accuracy: Option<String>,
    /// When the fix was taken, unix seconds (the receive time where the
    /// sender's clock claims the future), for the line's age, and from
    /// when the line is drawn dimmed as stale.
    #[serde(default)]
    pub fix_at: Option<i64>,
    #[serde(default)]
    pub stale_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Messages {
    pub v: u32,
    pub peer: String,
    pub name: String,
    pub messages: Vec<MessageRow>,
    /// The header's avatar, as in [`ConversationRow`].
    #[serde(default)]
    pub initials: String,
    #[serde(default)]
    pub icon_fg: Option<String>,
    #[serde(default)]
    pub icon_bg: Option<String>,
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

/// One frame of the map screen, and what the user did to the last one
/// (1128). Java keeps only the view it got back ([`MapShown`]'s centre,
/// zoom and selection) and hands it in again with the gestures since; the
/// rules of the view are loquax's (`loquax::ui::map`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapRequest {
    /// The app's files directory: the maps under `maps/`, the store and
    /// the config beside them.
    pub dir: String,
    /// The view's size in the screen's pixels.
    pub w: u32,
    pub h: u32,
    /// The screen's density times a thousand: 2750 on a phone of 2.75.
    pub density_milli: u32,
    /// Whether the phone is in its dark setting.
    #[serde(default)]
    pub dark: bool,
    /// The last frame's centre in world units and its zoom; absent on the
    /// first frame, which fits the pins.
    #[serde(default)]
    pub x: Option<u32>,
    #[serde(default)]
    pub y: Option<u32>,
    #[serde(default)]
    pub zoom: Option<u8>,
    /// The last frame's exact zoom in thousandths of a step (1134): where
    /// a pinch left it. Wins over `zoom` when given.
    #[serde(default)]
    pub zoom_milli: Option<u32>,
    /// The selected pin's peer, hex.
    #[serde(default)]
    pub selected: Option<String>,
    /// Moves the view by this many pixels, after the centre above: what a
    /// finger dragged the content by is the negative of it.
    #[serde(default)]
    pub pan_x: i32,
    #[serde(default)]
    pub pan_y: i32,
    /// Zooms by this many steps about the pixel `(about_x, about_y)` of the
    /// view, after the pan.
    #[serde(default)]
    pub zoom_by: i32,
    /// Zooms by this many thousandths of a step about the same pixel,
    /// after `zoom_by`: a pinch's live scale, `1000 * log2(scale)`, which
    /// the view keeps exactly (1134).
    #[serde(default)]
    pub zoom_by_milli: i32,
    #[serde(default)]
    pub about_x: i32,
    #[serde(default)]
    pub about_y: i32,
    /// A tap at this pixel of the last frame: the pin there is selected
    /// and named.
    #[serde(default)]
    pub tap_x: Option<i32>,
    #[serde(default)]
    pub tap_y: Option<i32>,
    /// Fit every pin, as `0` does in the terminal.
    #[serde(default)]
    pub fit: bool,
    /// Centre on this place, degrees times a million, as a report's fix
    /// opens the map in the terminal (1076); its region is loaded when no
    /// region holds it.
    #[serde(default)]
    pub at_lat_e6: Option<i32>,
    #[serde(default)]
    pub at_lon_e6: Option<i32>,
    /// Load the region of the view: "load this area", `d` in the terminal.
    #[serde(default)]
    pub load: bool,
    /// Pixels drawn past the view on each side, left and right, top and
    /// bottom (1131): a finger that pans first reveals drawn map, and a
    /// pinch that zooms out shrinks a bitmap that still fills the screen.
    /// The view's rules (fit, the one-world floor, a tap) are the `w` x
    /// `h` screen's; the frame is that much larger about the same centre.
    #[serde(default)]
    pub margin_x: u32,
    #[serde(default)]
    pub margin_y: u32,
    /// A finger is on the map: the frame shows the names its last frame at
    /// rest placed, where the view moved them, and places none anew; it
    /// keeps the zoom step it was composed from within a step of the
    /// pinch, scaled, and draws only a few missing tiles (1134).
    #[serde(default)]
    pub moving: bool,
    /// Switch the map, and only the map, to the other table than the one
    /// it shows (1133): the choice is stored as the config's `map_theme`
    /// and wins over the phone's setting from then on.
    #[serde(default)]
    pub toggle_theme: bool,
    /// Drop that choice: the map follows the phone again, `map_theme =
    /// auto`.
    #[serde(default)]
    pub follow_phone: bool,
}

/// The map frame's description; its pixels follow it (see the table
/// above).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapShown {
    pub v: u32,
    pub ok: bool,
    pub error: Option<String>,
    /// The frame's size: the view's and the margins on both sides.
    pub w: u32,
    pub h: u32,
    /// The margins drawn past the view on each side, the request's: the
    /// view's top left corner is this pixel of the frame.
    #[serde(default)]
    pub margin_x: u32,
    #[serde(default)]
    pub margin_y: u32,
    /// The view drawn, for the next request.
    pub x: u32,
    pub y: u32,
    /// The zoom step nearest the view's exact zoom.
    pub zoom: u8,
    /// The view's exact zoom in thousandths of a step (1134).
    #[serde(default)]
    pub zoom_milli: u32,
    /// The zoom step whose tiles the frame is composed from.
    #[serde(default)]
    pub tile_zoom: u8,
    /// The frame still owes tiles it showed another step's tiles for:
    /// ask again.
    #[serde(default)]
    pub pending: bool,
    /// The shallowest and the deepest zoom the regions allow.
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub selected: Option<String>,
    /// The credit the frame carries.
    pub attribution: String,
    /// The pins drawn, and the one a tap found, with its name.
    pub pins: u32,
    pub tapped: Option<String>,
    pub tapped_name: Option<String>,
    /// A region is loading: ask for the frame again in a while.
    pub loading: bool,
    /// A line for the user: a load that failed, a view loaded already.
    pub notice: Option<String>,
    /// How long the frame took to draw, milliseconds.
    pub ms: u32,
    /// The frame is in the map's dark table, and whether a stored choice
    /// put it there rather than the phone's setting.
    #[serde(default)]
    pub dark: bool,
    #[serde(default)]
    pub theme_chosen: bool,
}

/// A map frame for Java: the description's length, the description, the
/// pixels.
pub fn frame(shown: &MapShown, rgba: &[u8]) -> Vec<u8> {
    let head = encode(shown);
    let mut out = Vec::with_capacity(4 + head.len() + rgba.len());
    out.extend_from_slice(&u32::try_from(head.len()).unwrap_or(0).to_be_bytes());
    out.extend_from_slice(&head);
    out.extend_from_slice(rgba);
    out
}

/// Split a frame into its description and its pixels.
pub fn unframe(frame: &[u8]) -> Result<(MapShown, &[u8]), String> {
    let (len, rest) = frame
        .split_first_chunk::<4>()
        .ok_or_else(|| "a frame without its length".to_string())?;
    let len = u32::from_be_bytes(*len) as usize;
    if rest.len() < len {
        return Err("a frame cut short".to_string());
    }
    let (head, pixels) = rest.split_at(len);
    Ok((decode(head)?, pixels))
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
        assert_eq!(
            decode::<Request>(&encode(&Request::Front { front: true })),
            Ok(Request::Front { front: true })
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
    fn a_map_frame_is_its_length_its_description_and_its_pixels() {
        let shown = MapShown {
            v: VERSION,
            ok: true,
            w: 2,
            h: 1,
            attribution: "© OpenStreetMap contributors".to_string(),
            ..MapShown::default()
        };
        let pixels = [1u8, 2, 3, 255, 4, 5, 6, 255];
        let bytes = frame(&shown, &pixels);
        let head = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        assert_eq!(bytes.len(), 4 + head + pixels.len());
        assert_eq!(&bytes[4 + head..], &pixels);
        assert_eq!(unframe(&bytes), Ok((shown, &pixels[..])));
        assert!(unframe(&bytes[..3]).is_err());
        assert!(unframe(&bytes[..4 + head - 1]).is_err());
    }

    #[test]
    fn garbage_is_an_error_in_words() {
        let error = decode::<Request>(b"\xc1").expect_err("not msgpack");
        assert!(error.starts_with("the request did not decode"), "{error}");
        let error = decode::<Request>(&encode(&Query::Conversations)).expect_err("a query");
        assert!(error.contains("conversations"), "{error}");
    }
}
