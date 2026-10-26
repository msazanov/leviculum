//! The cross-run seen-message store, and why it exists.
//!
//! Within one run the router de-duplicates for itself. Across runs it
//! cannot: `loquax` builds a fresh router per invocation, so a message that
//! was delivered directly yesterday and is still sitting in a mailbox today
//! (the sender's fallback uploaded a copy, the peer came back before the
//! drain) would print twice. This store is the memory that survives the
//! process: one lowercase-hex message id per line, under
//! `${LNMSG_HOME}/seen`.
//!
//! The message id is the right key — not the transient id — because it is
//! the one identity a message keeps across its delivery methods: the direct
//! copy and the mailbox copy of one message share it by construction
//! (`leviculum-lxmf/src/message.rs`, the id covers the signed content).
//!
//! The set, the line format and the compaction rule are core and say what to
//! write ([`SeenWrite`]); the file is the shell's ([`SeenStore::load`] and
//! [`apply`], behind the `std` feature).

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

/// The store's file name under the loquax home directory.
pub const SEEN_FILE: &str = "seen";

/// Compaction threshold: at this many entries, loading rewrites the file to
/// the newest [`KEEP_ON_COMPACT`]. A messenger that has printed 65k messages
/// has long rotated its mailbox contents; unbounded growth is the only thing
/// this guards against, so the numbers are generous rather than tuned.
const COMPACT_AT: usize = 65_536;
/// How many newest entries a compaction keeps.
const KEEP_ON_COMPACT: usize = 32_768;

/// One change to the persisted record, for the host to apply
/// ([`crate::effects::Effects::write_seen`]; the terminal's is [`apply`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeenWrite {
    /// Append this line, newline included.
    Append(String),
    /// Replace the whole record with this text. The terminal writes it
    /// beside the file and renames it over, so a crash mid-compaction leaves
    /// the old file rather than half of one.
    Rewrite(String),
}

/// The set of already-delivered message ids, and the rules that keep its
/// record: the line format, the damaged-line rule and the compaction.
#[derive(Debug, Default)]
pub struct SeenStore {
    seen: BTreeSet<[u8; 32]>,
    /// File order, for compaction. `seen` is the lookup, this is the age.
    order: Vec<[u8; 32]>,
}

impl SeenStore {
    /// Read a record's text. An unreadable line is skipped rather than fatal,
    /// because refusing to fetch over a damaged dedup record would trade
    /// duplicate mail for no mail. A record that has grown to the compaction
    /// threshold (65 536 ids) comes back compacted, with the rewrite that
    /// makes the record match.
    pub fn from_text(text: &str) -> (Self, Option<SeenWrite>) {
        let mut store = Self::default();
        for line in text.lines() {
            if let Some(id) = parse_id(line.trim()) {
                if store.seen.insert(id) {
                    store.order.push(id);
                }
            }
        }
        let compaction = (store.order.len() >= COMPACT_AT).then(|| store.compact());
        (store, compaction)
    }

    /// Whether this id has been delivered by an earlier run (or earlier in
    /// this one).
    pub fn contains(&self, id: &[u8; 32]) -> bool {
        self.seen.contains(id)
    }

    /// Record one delivered id. A new id comes back as the line to append,
    /// which the caller applies immediately, so a fetch that dies mid-run
    /// does not re-print what it already printed; a known one as `None`.
    pub fn record(&mut self, id: [u8; 32]) -> Option<SeenWrite> {
        if !self.seen.insert(id) {
            return None;
        }
        self.order.push(id);
        let mut line = crate::address::to_hex(&id);
        line.push('\n');
        Some(SeenWrite::Append(line))
    }

    /// Keep the newest `KEEP_ON_COMPACT` entries; the record's new text.
    fn compact(&mut self) -> SeenWrite {
        let keep_from = self.order.len().saturating_sub(KEEP_ON_COMPACT);
        let kept: Vec<[u8; 32]> = self.order.split_off(keep_from);
        self.order = kept;
        self.seen = self.order.iter().copied().collect();
        let mut text = String::with_capacity(self.order.len() * 65);
        for id in &self.order {
            text.push_str(&crate::address::to_hex(id));
            text.push('\n');
        }
        SeenWrite::Rewrite(text)
    }
}

#[cfg(feature = "std")]
impl SeenStore {
    /// Load `home/seen`. A missing file is an empty store; one that has grown
    /// to the threshold is compacted on disk before it is used.
    pub fn load(home: &std::path::Path) -> std::io::Result<Self> {
        let text = match std::fs::read_to_string(home.join(SEEN_FILE)) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(error),
        };
        let (store, compaction) = Self::from_text(&text);
        if let Some(write) = compaction {
            apply(home, &write)?;
        }
        Ok(store)
    }
}

/// Apply one [`SeenWrite`] to `home/seen`.
#[cfg(feature = "std")]
pub fn apply(home: &std::path::Path, write: &SeenWrite) -> std::io::Result<()> {
    use std::io::Write as _;

    let path = home.join(SEEN_FILE);
    match write {
        SeenWrite::Append(line) => std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?
            .write_all(line.as_bytes()),
        SeenWrite::Rewrite(text) => {
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, &path)
        }
    }
}

fn parse_id(line: &str) -> Option<[u8; 32]> {
    if line.len() != 64 || !line.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut id = [0u8; 32];
    for (byte, pair) in id.iter_mut().zip(line.as_bytes().chunks_exact(2)) {
        let digit = |c: u8| match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => c.to_ascii_lowercase() - b'a' + 10,
        };
        *byte = (digit(pair[0]) << 4) | digit(pair[1]);
    }
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_id_is_one_line_to_append_and_a_known_one_is_nothing() {
        let mut store = SeenStore::default();
        let id = [0xab; 32];
        assert!(!store.contains(&id));
        assert_eq!(
            store.record(id),
            Some(SeenWrite::Append("ab".repeat(32) + "\n")),
            "first sight is new"
        );
        assert!(store.contains(&id));
        assert_eq!(store.record(id), None, "second sight is not");
    }

    #[test]
    fn a_damaged_line_is_skipped_not_fatal() {
        let (store, compaction) =
            SeenStore::from_text(&("not hex at all\n".to_string() + &"cd".repeat(32) + "\n"));
        assert!(store.contains(&[0xcd; 32]), "the intact line still counts");
        assert_eq!(compaction, None);
    }

    fn numbered(n: u32) -> [u8; 32] {
        let mut id = [0u8; 32];
        id[..4].copy_from_slice(&n.to_be_bytes());
        id
    }

    fn record_of(count: u32) -> String {
        let mut text = String::new();
        // `count` distinct ids, oldest first.
        for n in 0..count {
            text.push_str(&crate::address::to_hex(&numbered(n)));
            text.push('\n');
        }
        text
    }

    #[test]
    fn compaction_keeps_the_newest_entries() {
        let (store, compaction) = SeenStore::from_text(&record_of(COMPACT_AT as u32));
        assert!(
            !store.contains(&numbered(0)),
            "the oldest entries are dropped"
        );
        assert!(
            store.contains(&numbered(COMPACT_AT as u32 - 1)),
            "the newest entries are kept"
        );
        let Some(SeenWrite::Rewrite(text)) = compaction else {
            panic!("a full record is rewritten, not just the in-memory view: {compaction:?}");
        };
        assert_eq!(text.lines().count(), KEEP_ON_COMPACT);
        assert_eq!(
            SeenStore::from_text(&record_of(COMPACT_AT as u32 - 1)).1,
            None,
            "below the threshold nothing is rewritten"
        );
    }

    #[test]
    fn a_recorded_id_survives_a_reload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let id = [0xab; 32];

        let mut store = SeenStore::load(dir.path()).expect("empty load");
        let write = store.record(id).expect("first sight is new");
        apply(dir.path(), &write).expect("append");

        let reloaded = SeenStore::load(dir.path()).expect("reload");
        assert!(
            reloaded.contains(&id),
            "the whole point: the memory survives the process"
        );
    }

    #[test]
    fn loading_a_full_file_compacts_it_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(SEEN_FILE), record_of(COMPACT_AT as u32)).expect("write");

        let store = SeenStore::load(dir.path()).expect("load compacts");
        assert!(store.contains(&numbered(COMPACT_AT as u32 - 1)));

        let on_disk = std::fs::read_to_string(dir.path().join(SEEN_FILE)).expect("read back");
        assert_eq!(
            on_disk.lines().count(),
            KEEP_ON_COMPACT,
            "the file itself is rewritten, not just the in-memory view"
        );
    }
}
