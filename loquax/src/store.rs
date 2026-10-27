//! The message store: conversations, messages and their ledgers across
//! restarts, in SQLite (`docs/src/concepts/loquax-storage.md`, "Decision
//! (2026-08-10)"), at `${LNMSG_HOME}/history.sqlite3`.
//!
//! The model says what changed ([`crate::ui::history::StoreWrite`]); this
//! file writes it, one transaction a fold, and reads the history back at
//! start ([`Store::restore`]), a page at a time further back
//! ([`Store::earlier`]), and through its FTS5 index ([`Store::search`]).
//!
//! # The schema
//!
//! The page's starting point, with its three commitments: every row is
//! scoped by our own address (`identity`), a message keeps the sender's
//! clock (`timestamp`) apart from ours (`received_at`), and `fields` is raw
//! msgpack. Where it differs:
//!
//! - **The key is the screen's message counter** (`seq`, the model's
//!   [`Message::activity`]), not `(id, identity)`: a message we write has
//!   no LXMF id until the router queued it, and a send that falls back to a
//!   mailbox gets a second one. The id is a column, indexed.
//! - **The ledger is a table of its own**, a row a line, deleted with its
//!   message; the delivery ledger shares the message's retention (the
//!   page's "Retention").
//! - **Conversations are a table** for what is the conversation's and not a
//!   message's: the pin and the unread count. Names heard from announces
//!   are one more, so a peer keeps its name across a restart.
//! - **`fields` is filled, `title` and `timestamp` stay empty for now**:
//!   the engine's `Received` event carries the fields, which the store keeps
//!   whole as one raw msgpack map; it does not carry the sender's time, and
//!   the screen drops the title. The columns are there so filling them is no
//!   migration.
//! - **Positions are a table** (`positions`, schema 2): every position a
//!   telemetry field carried, keyed by who measured and the fix time, so the
//!   same fix arriving directly and again in a collector's stream is one row
//!   (`docs/src/concepts/loquax-map.md`, "Stored where"). The wire's
//!   integers go in as they are.
//!
//! # Versions
//!
//! Schema 1 is the store of slice 6; schema 2 adds `positions` and nothing
//! else, so a schema 1 file is migrated in place at open, in one
//! transaction. A file newer than this program is refused
//! ([`StoreError::Newer`]): a loquax that knows only schema 1 refuses a
//! schema 2 file, and leaves it as it is.
//!
//! # The durability rule
//!
//! From the page: nothing here may treat a corrupt load as an empty one. A
//! missing file is a first start and makes a fresh store. A file that is
//! there and is not a whole loquax store (not SQLite at all, shorter than
//! its own header says, a schema that is not ours, or newer than this
//! program; SQLite's own checks at open, and the schema's) is refused, with the file named, and loquax does not start over
//! it. Every write is one transaction under SQLite's rollback journal with
//! `synchronous=FULL`, so a crash leaves the store as it was before the
//! write or after it.
//!
//! # Retention
//!
//! As decided: message text and its ledger are kept until the reader
//! deletes them. Attachments, which have the byte budget, do not exist yet.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use leviculum_ui::delivery::DeliveryState;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use crate::send::Leg;
use crate::telemetry::{pack_fields, unpack_fields, Fix};
use crate::ui::history::{Restored, StoreWrite, StoredHit, LOAD_WINDOW};
use crate::ui::{LedgerEntry, Message};
use crate::when::WallTime;
use leviculum_lxmf::telemetry::Location;

/// The store's file under the loquax home directory.
pub const STORE_FILE: &str = "history.sqlite3";

/// The schema this program writes, in SQLite's `user_version`.
pub const SCHEMA_VERSION: i32 = 2;

/// How long a write waits for another loquax holding the file.
const BUSY_MS: u64 = 5_000;

/// The most results a search returns from the store.
pub const SEARCH_LIMIT: usize = 200;

const SCHEMA: &str = "
CREATE TABLE conversations(
    identity BLOB NOT NULL,
    peer BLOB NOT NULL,
    pinned INTEGER NOT NULL DEFAULT 0,
    unread INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (identity, peer));
CREATE TABLE messages(
    seq INTEGER PRIMARY KEY,
    identity BLOB NOT NULL,
    conversation BLOB NOT NULL,
    id BLOB,
    direction INTEGER NOT NULL,
    state INTEGER,
    unproven INTEGER NOT NULL DEFAULT 0,
    verification INTEGER NOT NULL,
    last_leg INTEGER,
    timestamp REAL,
    received_at INTEGER,
    utc_offset INTEGER,
    size INTEGER NOT NULL,
    title BLOB,
    content TEXT NOT NULL,
    fields BLOB,
    error TEXT);
CREATE INDEX messages_by_conversation ON messages(identity, conversation, seq);
CREATE INDEX messages_by_id ON messages(id);
CREATE TABLE ledger(
    message INTEGER NOT NULL REFERENCES messages(seq) ON DELETE CASCADE,
    line INTEGER NOT NULL,
    at INTEGER,
    utc_offset INTEGER,
    what TEXT NOT NULL,
    PRIMARY KEY (message, line));
CREATE TABLE names(
    identity BLOB NOT NULL,
    peer BLOB NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (identity, peer));
CREATE VIRTUAL TABLE message_text USING fts5(
    content, content='messages', content_rowid='seq', tokenize='trigram');
CREATE TRIGGER message_text_insert AFTER INSERT ON messages BEGIN
    INSERT INTO message_text(rowid, content) VALUES (new.seq, new.content);
END;
CREATE TRIGGER message_text_delete AFTER DELETE ON messages BEGIN
    INSERT INTO message_text(message_text, rowid, content)
        VALUES ('delete', old.seq, old.content);
END;
CREATE TRIGGER message_text_update AFTER UPDATE OF content ON messages BEGIN
    INSERT INTO message_text(message_text, rowid, content)
        VALUES ('delete', old.seq, old.content);
    INSERT INTO message_text(rowid, content) VALUES (new.seq, new.content);
END;
";

/// What schema 2 adds to schema 1: the positions telemetry carried.
const SCHEMA_V2: &str = "
CREATE TABLE positions(
    identity BLOB NOT NULL,
    peer BLOB NOT NULL,
    via BLOB NOT NULL,
    message INTEGER REFERENCES messages(seq) ON DELETE SET NULL,
    fix_time INTEGER NOT NULL,
    received_at INTEGER,
    lat_e6 INTEGER NOT NULL, lon_e6 INTEGER NOT NULL,
    alt_e2 INTEGER, speed_e2 INTEGER, bearing_e2 INTEGER, accuracy_e2 INTEGER,
    telemetry BLOB NOT NULL,
    PRIMARY KEY (identity, peer, fix_time));
";

/// The tables a whole store of schema 1 has.
const TABLES_V1: [&str; 5] = [
    "conversations",
    "messages",
    "ledger",
    "names",
    "message_text",
];

/// The tables schema 2 adds.
const TABLES_V2: [&str; 1] = ["positions"];

/// Why the store could not be used.
#[derive(Debug)]
pub enum StoreError {
    /// The file is there and is not a whole store: loquax will not start
    /// over it.
    Corrupt { path: PathBuf, detail: String },
    /// A later loquax wrote it.
    Newer { path: PathBuf, version: i32 },
    /// Reading or writing failed.
    Failed { path: PathBuf, detail: String },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt { path, detail } => write!(
                f,
                "the message store {} is damaged ({detail}). loquax will not start over it, \
                 because an empty history would be written where the old one was. Move the \
                 file aside to start with an empty history, or put a backup in its place",
                path.display()
            ),
            Self::Newer { path, version } => write!(
                f,
                "the message store {} was written by a later loquax (schema {version}, this one \
                 knows {SCHEMA_VERSION}); update loquax",
                path.display()
            ),
            Self::Failed { path, detail } => {
                write!(f, "the message store {}: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for StoreError {}

/// The open store, scoped to one of our addresses.
pub struct Store {
    conn: Connection,
    path: PathBuf,
    identity: [u8; 16],
}

impl Store {
    /// `home/history.sqlite3` for `identity`, our LXMF address.
    pub fn open_in(home: &Path, identity: [u8; 16]) -> Result<Self, StoreError> {
        Self::open(&home.join(STORE_FILE), identity)
    }

    /// The store at `path`: made when the file is missing, refused when it
    /// is there and not whole ([`StoreError::Corrupt`]).
    pub fn open(path: &Path, identity: [u8; 16]) -> Result<Self, StoreError> {
        let fresh = match std::fs::symlink_metadata(path) {
            Ok(_) => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => return Err(failed(path, error)),
        };
        let flags = match fresh {
            true => OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
            // Never create over a name that vanished between the two calls.
            false => OpenFlags::SQLITE_OPEN_READ_WRITE,
        } | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags).map_err(|e| failed(path, e))?;
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_MS))
            .map_err(|e| failed(path, e))?;
        let store = Self {
            conn,
            path: path.to_path_buf(),
            identity,
        };
        match fresh {
            true => store.create()?,
            false => store.check()?,
        }
        store.configure()?;
        Ok(store)
    }

    /// Where it is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn create(&self) -> Result<(), StoreError> {
        let batch = format!(
            "BEGIN IMMEDIATE; {SCHEMA} {SCHEMA_V2} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
        );
        self.conn
            .execute_batch(&batch)
            .map_err(|e| failed(&self.path, e))
    }

    /// The checks that tell a whole store from a damaged one, cheapest
    /// first, before anything reads it for the screen.
    fn check(&self) -> Result<(), StoreError> {
        let corrupt = |detail: String| StoreError::Corrupt {
            path: self.path.clone(),
            detail,
        };
        // Reading the schema makes SQLite read the header, roll back a write
        // a crash left in its journal, and refuse a file shorter than the
        // page count its header gives (`lockBtree`, "database disk image is
        // malformed"): a cut file is caught here, before anything reads it
        // for the screen.
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|e| corrupt(e.to_string()))?;
        let tables: Vec<String> = self
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| row.get(0))?
                    .collect::<Result<Vec<String>, _>>()
            })
            .map_err(|e| corrupt(e.to_string()))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::Newer {
                path: self.path.clone(),
                version,
            });
        }
        if !(1..=SCHEMA_VERSION).contains(&version) {
            return Err(corrupt(format!(
                "not a loquax store: schema version {version}"
            )));
        }
        let missing = |want: &[&'static str]| -> Option<&'static str> {
            want.iter()
                .find(|t| !tables.iter().any(|n| n == *t))
                .copied()
        };
        if let Some(missing) = missing(&TABLES_V1) {
            return Err(corrupt(format!("table {missing} is missing")));
        }
        if version == 1 {
            return self.migrate_v1();
        }
        if let Some(missing) = missing(&TABLES_V2) {
            return Err(corrupt(format!("table {missing} is missing")));
        }
        Ok(())
    }

    /// Schema 1 to 2: add `positions`, all or none. The version is read
    /// again under the write lock, so two loquax opening one old file at
    /// once migrate it once.
    fn migrate_v1(&self) -> Result<(), StoreError> {
        let batch = format!("{SCHEMA_V2} PRAGMA user_version = 2;");
        let migrate = || -> rusqlite::Result<()> {
            self.conn.execute_batch("BEGIN IMMEDIATE")?;
            let version: i32 = self
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if version == 1 {
                if let Err(error) = self.conn.execute_batch(&batch) {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(error);
                }
            }
            self.conn.execute_batch("COMMIT")
        };
        migrate().map_err(|e| failed(&self.path, e))
    }

    fn configure(&self) -> Result<(), StoreError> {
        self.conn
            .execute_batch(
                "PRAGMA journal_mode = DELETE; PRAGMA synchronous = FULL; \
                 PRAGMA foreign_keys = ON;",
            )
            .map_err(|e| failed(&self.path, e))
    }

    /// The largest key the store holds, from which the model counts on.
    pub fn last_key(&self) -> Result<u64, StoreError> {
        let last: Option<i64> = self
            .conn
            .query_row("SELECT MAX(seq) FROM messages", [], |row| row.get(0))
            .map_err(|e| self.failed(e))?;
        Ok(last.map_or(0, |key| key as u64))
    }

    /// Every conversation with its newest [`LOAD_WINDOW`] messages, the most
    /// recent conversation first.
    pub fn restore(&self) -> Result<Vec<Restored>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT c.peer, c.pinned, c.unread, \
                 (SELECT MAX(seq) FROM messages m \
                  WHERE m.identity = c.identity AND m.conversation = c.peer) AS last \
                 FROM conversations c WHERE c.identity = ?1 \
                 ORDER BY last DESC",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| self.failed(e))?;
        let mut out = Vec::new();
        for (peer, pinned, unread) in rows {
            let peer = self.address(&peer)?;
            let (messages, earlier) = self.page(peer, u64::MAX, None)?;
            out.push(Restored {
                peer,
                pinned,
                unread: unread.max(0) as usize,
                messages,
                earlier,
            });
        }
        Ok(out)
    }

    /// The [`LOAD_WINDOW`] messages of `peer`'s conversation before key
    /// `before`, or every one from `through` on when that reaches further,
    /// oldest first, and how many the store holds before those.
    pub fn earlier(
        &self,
        peer: [u8; 16],
        before: u64,
        through: Option<u64>,
    ) -> Result<(Vec<Message>, usize), StoreError> {
        self.page(peer, before, through)
    }

    fn page(
        &self,
        peer: [u8; 16],
        before: u64,
        through: Option<u64>,
    ) -> Result<(Vec<Message>, usize), StoreError> {
        let before = i64::try_from(before).unwrap_or(i64::MAX);
        // The window's oldest key: the LOAD_WINDOW-th before `before`, or
        // none when fewer are there, then down to `through`.
        let window_start: Option<i64> = self
            .conn
            .query_row(
                "SELECT seq FROM messages WHERE identity = ?1 AND conversation = ?2 \
                 AND seq < ?3 ORDER BY seq DESC LIMIT 1 OFFSET ?4",
                params![
                    &self.identity[..],
                    &peer[..],
                    before,
                    (LOAD_WINDOW - 1) as i64
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| self.failed(e))?;
        let through = through.and_then(|key| i64::try_from(key).ok());
        let lowest = match (window_start, through) {
            (Some(start), Some(through)) => start.min(through),
            (Some(start), None) => start,
            (None, _) => 0,
        };
        let mut statement = self
            .conn
            .prepare(
                "SELECT seq, id, direction, state, unproven, verification, last_leg, \
                 received_at, utc_offset, size, content, fields FROM messages \
                 WHERE identity = ?1 AND conversation = ?2 AND seq < ?3 AND seq >= ?4 \
                 ORDER BY seq",
            )
            .map_err(|e| self.failed(e))?;
        let mut messages = statement
            .query_map(
                params![&self.identity[..], &peer[..], before, lowest],
                row_message,
            )
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| self.failed(e))?;
        let mut ledger = self
            .conn
            .prepare("SELECT at, utc_offset, what FROM ledger WHERE message = ?1 ORDER BY line")
            .map_err(|e| self.failed(e))?;
        for message in &mut messages {
            message.ledger = ledger
                .query_map(params![message.activity as i64], |row| {
                    Ok(LedgerEntry {
                        at: wall_time(row.get(0)?, row.get(1)?),
                        what: row.get(2)?,
                    })
                })
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
                .map_err(|e| self.failed(e))?;
        }
        let first = messages.first().map_or(before, |m| m.activity as i64);
        let earlier: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM messages \
                 WHERE identity = ?1 AND conversation = ?2 AND seq < ?3",
                params![&self.identity[..], &peer[..], first],
                |row| row.get(0),
            )
            .map_err(|e| self.failed(e))?;
        Ok((messages, earlier.max(0) as usize))
    }

    /// Make the model's writes, all or none.
    pub fn write(&mut self, writes: &[StoreWrite]) -> Result<(), StoreError> {
        if writes.is_empty() {
            return Ok(());
        }
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        for write in writes {
            apply(&tx, &identity, write).map_err(|e| failed(&path, e))?;
        }
        tx.commit().map_err(|e| failed(&path, e))
    }

    /// Every peer's latest position, by [`Fix::newer_than`], the clamp for
    /// a fix ahead of our clock applied as at receive time.
    pub fn positions(&self) -> Result<Vec<Fix>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT peer, via, message, fix_time, received_at, lat_e6, lon_e6, alt_e2, \
                 speed_e2, bearing_e2, accuracy_e2, telemetry FROM positions \
                 WHERE identity = ?1",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    Location {
                        last_update: row.get(3)?,
                        latitude_e6: row.get(5)?,
                        longitude_e6: row.get(6)?,
                        altitude_e2: row.get::<_, Option<i32>>(7)?.unwrap_or(0),
                        speed_e2: row.get::<_, Option<u32>>(8)?.unwrap_or(0),
                        bearing_e2: row.get::<_, Option<i32>>(9)?.unwrap_or(0),
                        accuracy_e2: row.get::<_, Option<u16>>(10)?.unwrap_or(0),
                    },
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Vec<u8>>(11)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| self.failed(e))?;
        let mut latest: BTreeMap<[u8; 16], Fix> = BTreeMap::new();
        for (peer, via, message, location, received_at, telemetry) in rows {
            let fix = Fix {
                peer: self.address(&peer)?,
                via: self.address(&via)?,
                message: message.map(|key| key as u64),
                received_at,
                location,
                telemetry,
            };
            let newer = latest
                .get(&fix.peer)
                .is_none_or(|held| fix.newer_than(held));
            if newer {
                latest.insert(fix.peer, fix);
            }
        }
        Ok(latest.into_values().collect())
    }

    /// How many positions the store holds for `peer`: the track.
    pub fn track_len(&self, peer: [u8; 16]) -> Result<usize, StoreError> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM positions WHERE identity = ?1 AND peer = ?2",
                params![&self.identity[..], &peer[..]],
                |row| row.get(0),
            )
            .map_err(|e| self.failed(e))?;
        Ok(count.max(0) as usize)
    }

    /// The names heard from announces in earlier runs.
    pub fn names(&self) -> Result<Vec<([u8; 16], String)>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT peer, name FROM names WHERE identity = ?1")
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| self.failed(e))?;
        rows.into_iter()
            .map(|(peer, name)| Ok((self.address(&peer)?, name)))
            .collect()
    }

    /// Keep the name `peer` announced, sanitised.
    pub fn save_name(&mut self, peer: [u8; 16], name: &str) -> Result<(), StoreError> {
        self.conn
            .execute(
                "INSERT INTO names(identity, peer, name) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(identity, peer) DO UPDATE SET name = excluded.name \
                 WHERE name != excluded.name",
                params![&self.identity[..], &peer[..], name],
            )
            .map(|_| ())
            .map_err(|e| self.failed(e))
    }

    /// The messages whose text has `query` in it, ignoring case, that lie
    /// below what the screen loaded: before `loaded_from`'s key in a
    /// conversation it names, and none in a conversation it does not
    /// (the screen holds all of that one). The newest first, at most
    /// [`SEARCH_LIMIT`].
    pub fn search(
        &self,
        query: &str,
        loaded_from: &BTreeMap<[u8; 16], u64>,
    ) -> Result<Vec<StoredHit>, StoreError> {
        if query.is_empty() || loaded_from.is_empty() {
            return Ok(Vec::new());
        }
        // The trigram index matches a substring of three characters or
        // more; a shorter one is a scan, as the screen's matcher is.
        let (sql, pattern) = match query.chars().count() >= 3 {
            true => (
                "SELECT m.seq, m.conversation, m.direction, m.received_at, m.utc_offset, \
                 m.content FROM message_text JOIN messages m ON m.seq = message_text.rowid \
                 WHERE message_text MATCH ?1 AND m.identity = ?2 ORDER BY m.seq DESC",
                format!("\"{}\"", query.replace('"', "\"\"")),
            ),
            false => (
                "SELECT seq, conversation, direction, received_at, utc_offset, content \
                 FROM messages WHERE content LIKE ?1 ESCAPE '\\' AND identity = ?2 \
                 ORDER BY seq DESC",
                format!(
                    "%{}%",
                    query
                        .replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_")
                ),
            ),
        };
        let mut statement = self.conn.prepare(sql).map_err(|e| self.failed(e))?;
        let mut rows = statement
            .query(params![pattern, &self.identity[..]])
            .map_err(|e| self.failed(e))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| self.failed(e))? {
            let key: i64 = row.get(0).map_err(|e| self.failed(e))?;
            let peer: Vec<u8> = row.get(1).map_err(|e| self.failed(e))?;
            let peer = self.address(&peer)?;
            let below = loaded_from
                .get(&peer)
                .is_some_and(|&first| (key as u64) < first);
            if !below {
                continue;
            }
            let direction: i64 = row.get(2).map_err(|e| self.failed(e))?;
            let at = wall_time(
                row.get(3).map_err(|e| self.failed(e))?,
                row.get(4).map_err(|e| self.failed(e))?,
            );
            out.push(StoredHit {
                peer,
                key: key as u64,
                outbound: direction == 1,
                body: row.get(5).map_err(|e| self.failed(e))?,
                at,
            });
            if out.len() == SEARCH_LIMIT {
                break;
            }
        }
        Ok(out)
    }

    fn address(&self, bytes: &[u8]) -> Result<[u8; 16], StoreError> {
        bytes.try_into().map_err(|_| StoreError::Corrupt {
            path: self.path.clone(),
            detail: format!("an address of {} bytes", bytes.len()),
        })
    }

    fn failed(&self, error: impl fmt::Display) -> StoreError {
        failed(&self.path, error)
    }
}

fn failed(path: &Path, error: impl fmt::Display) -> StoreError {
    let detail = error.to_string();
    // SQLite's words for a file that is not whole, wherever they surface.
    let damaged = ["malformed", "not a database", "corrupt"];
    match damaged.iter().any(|word| detail.contains(word)) {
        true => StoreError::Corrupt {
            path: path.to_path_buf(),
            detail,
        },
        false => StoreError::Failed {
            path: path.to_path_buf(),
            detail,
        },
    }
}

fn apply(
    tx: &rusqlite::Transaction<'_>,
    identity: &[u8; 16],
    write: &StoreWrite,
) -> rusqlite::Result<()> {
    match write {
        StoreWrite::Message { peer, message } => {
            tx.execute(
                "INSERT INTO conversations(identity, peer) VALUES (?1, ?2) \
                 ON CONFLICT DO NOTHING",
                params![&identity[..], &peer[..]],
            )?;
            let key = message.activity as i64;
            let (secs, offset) = split(message.at);
            tx.execute(
                "INSERT INTO messages(seq, identity, conversation, id, direction, state, \
                 unproven, verification, last_leg, received_at, utc_offset, size, content, \
                 fields) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
                 ON CONFLICT(seq) DO UPDATE SET id = excluded.id, state = excluded.state, \
                 unproven = excluded.unproven, verification = excluded.verification, \
                 last_leg = excluded.last_leg",
                params![
                    key,
                    &identity[..],
                    &peer[..],
                    message.id.as_ref().map(|id| &id[..]),
                    i64::from(message.outbound),
                    message.delivery.map(state_code),
                    message.unproven,
                    message.verified,
                    message.last_leg.map(leg_code),
                    secs,
                    offset,
                    message.size as i64,
                    message.body,
                    (!message.fields.is_empty()).then(|| pack_fields(&message.fields)),
                ],
            )?;
            // The ledger only grows: the lines past what the store has.
            let have: i64 = tx.query_row(
                "SELECT COUNT(*) FROM ledger WHERE message = ?1",
                params![key],
                |row| row.get(0),
            )?;
            for (line, entry) in message.ledger.iter().enumerate().skip(have as usize) {
                let (at, offset) = split(entry.at);
                tx.execute(
                    "INSERT INTO ledger(message, line, at, utc_offset, what) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![key, line as i64, at, offset, entry.what],
                )?;
            }
        }
        StoreWrite::Conversation {
            peer,
            pinned,
            unread,
        } => {
            tx.execute(
                "INSERT INTO conversations(identity, peer, pinned, unread) \
                 VALUES (?1, ?2, ?3, ?4) ON CONFLICT(identity, peer) DO UPDATE SET \
                 pinned = excluded.pinned, unread = excluded.unread",
                params![&identity[..], &peer[..], pinned, *unread as i64],
            )?;
        }
        StoreWrite::ForgetMessage { key } => {
            tx.execute(
                "DELETE FROM messages WHERE seq = ?1 AND identity = ?2",
                params![*key as i64, &identity[..]],
            )?;
        }
        StoreWrite::ForgetConversation { peer } => {
            tx.execute(
                "DELETE FROM messages WHERE identity = ?1 AND conversation = ?2",
                params![&identity[..], &peer[..]],
            )?;
            tx.execute(
                "DELETE FROM conversations WHERE identity = ?1 AND peer = ?2",
                params![&identity[..], &peer[..]],
            )?;
            tx.execute(
                "DELETE FROM positions WHERE identity = ?1 AND peer = ?2",
                params![&identity[..], &peer[..]],
            )?;
        }
        StoreWrite::Position(fix) => {
            let l = &fix.location;
            tx.execute(
                "INSERT OR IGNORE INTO positions(identity, peer, via, message, fix_time, \
                 received_at, lat_e6, lon_e6, alt_e2, speed_e2, bearing_e2, accuracy_e2, \
                 telemetry) \
                 VALUES (?1, ?2, ?3, \
                 (SELECT seq FROM messages WHERE seq = ?4 AND identity = ?1), \
                 ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    &identity[..],
                    &fix.peer[..],
                    &fix.via[..],
                    fix.message.map(|key| key as i64),
                    l.last_update,
                    fix.received_at,
                    l.latitude_e6,
                    l.longitude_e6,
                    l.altitude_e2,
                    i64::from(l.speed_e2),
                    l.bearing_e2,
                    i64::from(l.accuracy_e2),
                    fix.telemetry,
                ],
            )?;
        }
    }
    Ok(())
}

fn row_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let id: Option<Vec<u8>> = row.get(1)?;
    let direction: i64 = row.get(2)?;
    let state: Option<i64> = row.get(3)?;
    let leg: Option<i64> = row.get(6)?;
    Ok(Message {
        outbound: direction == 1,
        body: row.get(10)?,
        id: id.and_then(|id| <[u8; 32]>::try_from(id.as_slice()).ok()),
        delivery: state.and_then(state_of),
        verified: row.get(5)?,
        leg: None,
        unproven: row.get(4)?,
        at: wall_time(row.get(7)?, row.get(8)?),
        size: row.get::<_, i64>(9)?.max(0) as usize,
        ledger: Vec::new(),
        last_leg: leg.and_then(leg_of),
        activity: row.get::<_, i64>(0)? as u64,
        // Bytes that are not a fields map were not written by us: kept out
        // of the screen rather than misread.
        fields: row
            .get::<_, Option<Vec<u8>>>(11)?
            .and_then(|packed| unpack_fields(&packed))
            .unwrap_or_default(),
    })
}

fn split(at: Option<WallTime>) -> (Option<i64>, Option<i64>) {
    match at {
        Some(at) => (Some(at.unix_secs), Some(i64::from(at.offset_secs))),
        None => (None, None),
    }
}

fn wall_time(secs: Option<i64>, offset: Option<i64>) -> Option<WallTime> {
    Some(WallTime {
        unix_secs: secs?,
        offset_secs: i32::try_from(offset.unwrap_or(0)).unwrap_or(0),
    })
}

/// The store's number for a delivery state. Stable: these are on disk.
fn state_code(state: DeliveryState) -> i64 {
    match state {
        DeliveryState::Queued => 0,
        DeliveryState::Stamping => 1,
        DeliveryState::InFlight => 2,
        DeliveryState::Sent => 3,
        DeliveryState::InMailbox => 4,
        DeliveryState::Delivered => 5,
        DeliveryState::Rejected => 6,
        DeliveryState::Failed => 7,
    }
}

fn state_of(code: i64) -> Option<DeliveryState> {
    Some(match code {
        0 => DeliveryState::Queued,
        1 => DeliveryState::Stamping,
        2 => DeliveryState::InFlight,
        3 => DeliveryState::Sent,
        4 => DeliveryState::InMailbox,
        5 => DeliveryState::Delivered,
        6 => DeliveryState::Rejected,
        7 => DeliveryState::Failed,
        _ => return None,
    })
}

fn leg_code(leg: Leg) -> i64 {
    match leg {
        Leg::Direct => 0,
        Leg::Propagated => 1,
    }
}

fn leg_of(code: i64) -> Option<Leg> {
    match code {
        0 => Some(Leg::Direct),
        1 => Some(Leg::Propagated),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const US: [u8; 16] = [0x01; 16];
    const ALICE: [u8; 16] = [0xa1; 16];
    const BOB: [u8; 16] = [0xb0; 16];

    fn at(secs: i64) -> Option<WallTime> {
        Some(WallTime {
            unix_secs: secs,
            offset_secs: 3600,
        })
    }

    fn message(key: u64, outbound: bool, body: &str) -> Message {
        Message {
            outbound,
            body: body.to_string(),
            id: Some([key as u8; 32]),
            delivery: outbound.then_some(DeliveryState::Delivered),
            verified: true,
            leg: None,
            unproven: false,
            at: at(1_000 + key as i64),
            size: body.len(),
            ledger: vec![LedgerEntry {
                at: at(1_000 + key as i64),
                what: "queued".to_string(),
            }],
            last_leg: outbound.then_some(Leg::Direct),
            activity: key,
            fields: Vec::new(),
        }
    }

    fn put(store: &mut Store, peer: [u8; 16], message: Message) {
        store
            .write(&[StoreWrite::Message { peer, message }])
            .expect("write");
    }

    #[test]
    fn a_missing_file_starts_a_fresh_store() {
        let home = tempfile::tempdir().expect("home");
        let store = Store::open_in(home.path(), US).expect("a fresh store");
        assert!(store.restore().expect("restore").is_empty());
        assert_eq!(store.last_key().expect("last key"), 0);
        assert!(home.path().join(STORE_FILE).exists());
    }

    #[test]
    fn messages_their_ledger_and_the_pin_survive_a_reopen() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let mut sent = message(1, true, "are you at the field site?");
        put(&mut store, ALICE, sent.clone());
        put(&mut store, ALICE, message(2, false, "yes, from ten"));
        sent.ledger.push(LedgerEntry {
            at: at(1_010),
            what: "delivered (direct)".to_string(),
        });
        put(&mut store, ALICE, sent.clone());
        store
            .write(&[StoreWrite::Conversation {
                peer: ALICE,
                pinned: true,
                unread: 1,
            }])
            .expect("pin");
        drop(store);

        let store = Store::open_in(home.path(), US).expect("reopen");
        let restored = store.restore().expect("restore");
        assert_eq!(restored.len(), 1);
        let alice = &restored[0];
        assert_eq!(alice.peer, ALICE);
        assert!(alice.pinned);
        assert_eq!(alice.unread, 1);
        assert_eq!(alice.earlier, 0);
        assert_eq!(
            alice.messages,
            vec![sent, message(2, false, "yes, from ten")]
        );
        assert_eq!(store.last_key().expect("last key"), 2);
    }

    #[test]
    fn rows_are_scoped_to_our_address() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        put(&mut store, ALICE, message(1, false, "for us"));
        drop(store);
        let other = Store::open_in(home.path(), [0x02; 16]).expect("open as another");
        assert!(other.restore().expect("restore").is_empty());
    }

    #[test]
    fn the_newest_window_loads_and_earlier_pages_follow() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let total = LOAD_WINDOW as u64 * 2 + 5;
        let writes: Vec<StoreWrite> = (1..=total)
            .map(|key| StoreWrite::Message {
                peer: BOB,
                message: message(key, key % 2 == 0, &format!("line {key}")),
            })
            .collect();
        store.write(&writes).expect("write");
        let restored = store.restore().expect("restore");
        let bob = &restored[0];
        assert_eq!(bob.messages.len(), LOAD_WINDOW);
        assert_eq!(bob.earlier, total as usize - LOAD_WINDOW);
        assert_eq!(bob.messages[0].activity, total - LOAD_WINDOW as u64 + 1);
        assert_eq!(bob.messages.last().map(|m| m.activity), Some(total));

        let first = bob.messages[0].activity;
        let (page, earlier) = store.earlier(BOB, first, None).expect("earlier");
        assert_eq!(page.len(), LOAD_WINDOW);
        assert_eq!(earlier, 5);
        let (rest, earlier) = store
            .earlier(BOB, page[0].activity, None)
            .expect("the rest");
        assert_eq!(rest.len(), 5);
        assert_eq!(earlier, 0);
        assert_eq!(rest[0].activity, 1);

        // Down to a key further back than one page.
        let (through, earlier) = store.earlier(BOB, first, Some(3)).expect("through");
        assert_eq!(through[0].activity, 3);
        assert_eq!(earlier, 2);
    }

    #[test]
    fn the_search_finds_only_what_lies_below_the_loaded_window() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        put(&mut store, ALICE, message(1, false, "bring the Tripod"));
        put(&mut store, ALICE, message(2, true, "the tripod fits"));
        put(&mut store, BOB, message(3, false, "tripod in the van"));
        put(&mut store, ALICE, message(4, false, "no match here"));
        // Alice's screen loaded from key 2 on; Bob's holds all of his.
        let loaded: BTreeMap<[u8; 16], u64> = [(ALICE, 2)].into_iter().collect();
        let hits = store.search("TRIPOD", &loaded).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, 1);
        assert_eq!(hits[0].peer, ALICE);
        assert_eq!(hits[0].body, "bring the Tripod");
        // Two characters go past the trigram index, with the same answer.
        let short = store.search("tR", &loaded).expect("short search");
        assert_eq!(short.iter().map(|h| h.key).collect::<Vec<_>>(), vec![1]);
        // LIKE's own characters are text.
        assert!(store.search("%", &loaded).expect("percent").is_empty());
        // A quote does not break the query.
        assert!(store.search("\"tri", &loaded).expect("quote").is_empty());
        // Nothing is below the window when every conversation is whole.
        assert!(store
            .search("tripod", &BTreeMap::new())
            .expect("whole")
            .is_empty());
    }

    #[test]
    fn deletes_remove_the_text_from_the_index_too() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        put(&mut store, ALICE, message(1, false, "secret tripod"));
        put(&mut store, ALICE, message(2, false, "kept tripod"));
        put(&mut store, BOB, message(3, false, "bob's tripod"));
        store
            .write(&[StoreWrite::ForgetMessage { key: 1 }])
            .expect("forget");
        let all: BTreeMap<[u8; 16], u64> =
            [(ALICE, u64::MAX), (BOB, u64::MAX)].into_iter().collect();
        let keys = |store: &Store| -> Vec<u64> {
            store
                .search("tripod", &all)
                .expect("search")
                .iter()
                .map(|h| h.key)
                .collect()
        };
        assert_eq!(keys(&store), vec![3, 2]);
        store
            .write(&[StoreWrite::ForgetConversation { peer: BOB }])
            .expect("forget bob");
        assert_eq!(keys(&store), vec![2]);
        let restored = store.restore().expect("restore");
        assert_eq!(restored.len(), 1);
        let ledger_rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM ledger", [], |row| row.get(0))
            .expect("count");
        assert_eq!(ledger_rows, 1, "the ledger goes with its message");
    }

    #[test]
    fn names_are_kept_and_replaced() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        store.save_name(ALICE, "Alice").expect("save");
        store.save_name(ALICE, "Ally").expect("replace");
        store.save_name(BOB, "Bob").expect("save");
        drop(store);
        let store = Store::open_in(home.path(), US).expect("reopen");
        let mut names = store.names().expect("names");
        names.sort();
        assert_eq!(
            names,
            vec![(ALICE, "Ally".to_string()), (BOB, "Bob".to_string())]
        );
    }

    /// A store with some history in it, closed, and its length.
    fn written(home: &Path) -> u64 {
        let mut store = Store::open_in(home, US).expect("open");
        let writes: Vec<StoreWrite> = (1..=300)
            .map(|key| StoreWrite::Message {
                peer: ALICE,
                message: message(key, false, &"a long enough body ".repeat(8)),
            })
            .collect();
        store.write(&writes).expect("write");
        drop(store);
        std::fs::metadata(home.join(STORE_FILE))
            .expect("metadata")
            .len()
    }

    fn refused(home: &Path) -> String {
        match Store::open_in(home, US) {
            Err(error @ StoreError::Corrupt { .. }) => {
                let text = error.to_string();
                assert!(
                    text.contains(&home.join(STORE_FILE).display().to_string()),
                    "the refusal names the file: {text}"
                );
                text
            }
            Err(other) => panic!("refused, but not as damaged: {other}"),
            Ok(_) => panic!("a damaged store was opened"),
        }
    }

    /// The positive control first: the same file, whole, opens and holds
    /// its history; cut in half it is refused, never opened as empty.
    #[test]
    fn a_truncated_store_is_refused_and_left_as_it_is() {
        let home = tempfile::tempdir().expect("home");
        let length = written(home.path());
        let whole = Store::open_in(home.path(), US).expect("the whole file opens");
        assert_eq!(whole.restore().expect("restore")[0].earlier, 100);
        drop(whole);

        let path = home.path().join(STORE_FILE);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open for truncation");
        // The last page only, which nothing reads at open: SQLite compares
        // the file with its header's page count.
        file.set_len(length - 4096).expect("truncate a page");
        let text = refused(home.path());
        assert!(text.contains("malformed"), "{text}");
        // Half of it: SQLite finds the schema's pages gone.
        file.set_len(length / 2).expect("truncate to half");
        drop(file);
        refused(home.path());
        // Refusing wrote nothing over it.
        assert_eq!(
            std::fs::metadata(&path).expect("still there").len(),
            length / 2
        );
    }

    #[test]
    fn an_empty_or_foreign_file_is_refused() {
        let home = tempfile::tempdir().expect("home");
        let path = home.path().join(STORE_FILE);
        std::fs::write(&path, b"").expect("empty file");
        refused(home.path());
        std::fs::write(
            &path,
            b"this is a text file, not a database at all, honestly".repeat(100),
        )
        .expect("text file");
        refused(home.path());
        // Another program's SQLite file.
        std::fs::remove_file(&path).expect("remove");
        let foreign = Connection::open(&path).expect("foreign");
        foreign
            .execute_batch("CREATE TABLE notes(text TEXT); INSERT INTO notes VALUES ('x');")
            .expect("foreign schema");
        drop(foreign);
        let text = refused(home.path());
        assert!(text.contains("not a loquax store"), "{text}");
    }

    #[test]
    fn a_store_from_a_later_loquax_is_refused() {
        let home = tempfile::tempdir().expect("home");
        written(home.path());
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("open");
        conn.execute_batch(&format!("PRAGMA user_version = {}", SCHEMA_VERSION + 1))
            .expect("bump");
        drop(conn);
        assert!(matches!(
            Store::open_in(home.path(), US),
            Err(StoreError::Newer { .. })
        ));
    }

    /// A write is all or none: one that fails half way leaves the store as
    /// it was before it.
    #[test]
    fn a_failed_write_leaves_nothing_half_written() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        put(&mut store, ALICE, message(1, false, "before"));
        // The second write's ledger line collides with one already there,
        // so the transaction fails after its first message went in.
        store
            .conn
            .execute(
                "INSERT INTO ledger(message, line, what) VALUES (1, 5, 'planted')",
                [],
            )
            .expect("plant");
        let mut bad = message(1, false, "before");
        bad.ledger = (0..6)
            .map(|i| LedgerEntry {
                at: None,
                what: format!("line {i}"),
            })
            .collect();
        let result = store.write(&[
            StoreWrite::Message {
                peer: BOB,
                message: message(2, false, "half"),
            },
            StoreWrite::Message {
                peer: ALICE,
                message: bad,
            },
        ]);
        assert!(result.is_err(), "the collision fails the write");
        let restored = store.restore().expect("restore");
        assert!(
            restored.iter().all(|c| c.peer != BOB),
            "the first half of the failed write is not in the store"
        );
    }

    // Telemetry (loquax-map.md, slice (a), "Stored where").

    fn fix(peer: [u8; 16], via: [u8; 16], message: Option<u64>, fix_time: i64) -> Fix {
        Fix {
            peer,
            via,
            message,
            received_at: Some(fix_time + 30),
            location: Location {
                latitude_e6: 43_773_100,
                longitude_e6: 11_256_000,
                altitude_e2: -150,
                speed_e2: 4_000_000_000,
                bearing_e2: 27_000,
                accuracy_e2: 65_535,
                last_update: fix_time,
            },
            telemetry: vec![0x81, 0x01, 0x05],
        }
    }

    #[test]
    fn fields_are_kept_raw_and_come_back_with_their_message() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let mut report = message(1, false, "");
        report.fields = vec![(0x02, vec![0xc4, 0x01, 0x80]), (0x10, vec![0xc0])];
        put(&mut store, BOB, report.clone());
        drop(store);
        let store = Store::open_in(home.path(), US).expect("reopen");
        assert_eq!(store.restore().expect("restore")[0].messages, vec![report]);
        let raw: Vec<u8> = store
            .conn
            .query_row("SELECT fields FROM messages WHERE seq = 1", [], |row| {
                row.get(0)
            })
            .expect("the column");
        assert_eq!(raw, vec![0x82, 0x02, 0xc4, 0x01, 0x80, 0x10, 0xc0]);
    }

    #[test]
    fn a_fix_arriving_twice_is_one_row_and_the_latest_comes_back() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        put(&mut store, BOB, message(1, false, ""));
        put(&mut store, ALICE, message(2, false, ""));
        let direct = fix(BOB, BOB, Some(1), 5_000);
        let relayed = fix(BOB, ALICE, Some(2), 5_000);
        let older = fix(BOB, BOB, None, 4_000);
        store
            .write(&[
                StoreWrite::Position(direct.clone()),
                StoreWrite::Position(relayed),
                StoreWrite::Position(older),
            ])
            .expect("write");
        assert_eq!(
            store.track_len(BOB).expect("track"),
            2,
            "the repeat is one row"
        );
        drop(store);
        let store = Store::open_in(home.path(), US).expect("reopen");
        assert_eq!(
            store.positions().expect("positions"),
            vec![direct],
            "integers as they were"
        );
        // Scoped to our address like every table.
        let other = Store::open_in(home.path(), [0x02; 16]).expect("as another");
        assert!(other.positions().expect("positions").is_empty());
    }

    #[test]
    fn deleting_a_message_keeps_its_position_and_a_conversation_takes_them() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        put(&mut store, BOB, message(1, false, ""));
        store
            .write(&[StoreWrite::Position(fix(BOB, BOB, Some(1), 5_000))])
            .expect("write");
        store
            .write(&[StoreWrite::ForgetMessage { key: 1 }])
            .expect("forget the message");
        let left = store.positions().expect("positions");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].message, None, "the reference is cleared");
        store
            .write(&[StoreWrite::ForgetConversation { peer: BOB }])
            .expect("forget bob");
        assert!(store.positions().expect("positions").is_empty());
    }

    /// The schema 1 store of slice 6, as that loquax wrote it.
    fn v1_store(home: &Path) {
        let conn = Connection::open(home.join(STORE_FILE)).expect("create");
        conn.execute_batch(&format!("BEGIN; {SCHEMA} PRAGMA user_version = 1; COMMIT;"))
            .expect("schema 1");
        conn.execute_batch(
            "INSERT INTO conversations(identity, peer) VALUES (X'01010101010101010101010101010101', \
             X'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1'); \
             INSERT INTO messages(seq, identity, conversation, direction, verification, \
             received_at, utc_offset, size, content) VALUES (7, \
             X'01010101010101010101010101010101', X'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1', 0, 1, \
             1000, 3600, 5, 'older');",
        )
        .expect("a message");
    }

    #[test]
    fn a_schema_1_store_migrates_in_place_and_keeps_its_history() {
        let home = tempfile::tempdir().expect("home");
        v1_store(home.path());
        let mut store = Store::open_in(home.path(), US).expect("schema 1 opens");
        assert_eq!(
            store
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
                .expect("version"),
            2
        );
        let restored = store.restore().expect("restore");
        assert_eq!(restored[0].messages[0].body, "older");
        assert!(restored[0].messages[0].fields.is_empty());
        put(&mut store, ALICE, message(8, false, ""));
        store
            .write(&[StoreWrite::Position(fix(ALICE, ALICE, Some(8), 9_000))])
            .expect("a position in the migrated store");
        drop(store);
        // Opened again it is a schema 2 store, not migrated twice.
        let store = Store::open_in(home.path(), US).expect("reopen");
        assert_eq!(store.positions().expect("positions").len(), 1);
    }

    /// A store this loquax wrote, opened by the loquax before it: the check
    /// of that program, which knew schema 1 only, refuses it as newer. Run
    /// here as the same comparison against version 1.
    #[test]
    fn a_schema_2_store_is_newer_than_a_schema_1_reader() {
        let home = tempfile::tempdir().expect("home");
        drop(Store::open_in(home.path(), US).expect("fresh"));
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("open");
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, 2);
        assert!(
            version > 1,
            "store.rs's check refuses `version > SCHEMA_VERSION`"
        );
    }

    #[test]
    fn a_schema_2_store_without_its_positions_is_refused() {
        let home = tempfile::tempdir().expect("home");
        drop(Store::open_in(home.path(), US).expect("fresh"));
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("open");
        conn.execute_batch("DROP TABLE positions").expect("drop");
        drop(conn);
        let text = refused(home.path());
        assert!(text.contains("table positions is missing"), "{text}");
    }
}
