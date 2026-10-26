//! What the mailbox rule ([`crate::mailbox`]) is fed from, and what it leaves
//! behind: the daemon's path table, read once per run, and the selection
//! persisted under `${LNMSG_HOME}/mailbox`.
//!
//! # Which node is on this device
//!
//! Measured 2026-10-25 (order 967), one daemon and one propagation node on
//! it as a client, then the node moved to a second daemon joined over TCP:
//!
//! | daemon | node      | path table `interface`          | hops |
//! |--------|-----------|---------------------------------|------|
//! | lnsd   | lnpnd, local  | `LocalInterface[rns/<instance>]` | 0 |
//! | lnsd   | lnpnd, remote | `TCPInterface[toB/127.0.0.1:…]`  | 1 |
//! | rnsd   | lxmd, local   | `LocalInterface[rns/<instance>]` | 0 |
//!
//! A node that registers its destination through a local client of the
//! daemon loquax is attached to sits zero hops away behind that local-client
//! interface; anything else is at least one hop out on a real interface. Both
//! stacks name the local-client side `LocalInterface[…]`, over the abstract
//! socket and over the TCP shared instance alike (`LocalInterface.py:374`,
//! `docs/src/audits/2026-10-11-shared-instance-rpc.md`). The test needs no
//! new surface and does not care which program the node is.

use crate::effects::Effects;
use crate::mailbox::{AdoptReason, Previous};
use crate::outbox::MailboxInput;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The file the selection is persisted in, beside `seen`.
pub const MAILBOX_FILE: &str = "mailbox";

/// One entry of the daemon's path table, as far as the rule needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub node: [u8; 16],
    pub hops: u8,
    /// The next hop is a local client of the daemon (see the module docs).
    pub own_device: bool,
    /// When the daemon last heard the destination announce, in Unix
    /// milliseconds: the path table's `timestamp`.
    pub seen_ms: u64,
}

/// The interface name a local client's paths carry in either stack.
const LOCAL_CLIENT_PREFIX: &str = "LocalInterface[";

/// Read the `path_table` RPC answer, as `leviculum_std::rpc_query` decodes
/// it: a list of dicts with `hash` (hex), `hops`, `interface` and
/// `timestamp` (Unix seconds). An entry missing any of them is skipped, not
/// guessed at.
pub fn routes_from_path_table(table: &serde_json::Value) -> Vec<Route> {
    let Some(entries) = table.as_array() else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let node = crate::address::parse(entry.get("hash")?.as_str()?).ok()?;
            let hops = u8::try_from(entry.get("hops")?.as_u64()?).ok()?;
            let interface = entry.get("interface")?.as_str()?;
            let seconds = entry.get("timestamp")?.as_f64()?;
            Some(Route {
                node,
                hops,
                own_device: hops == 0 && interface.starts_with(LOCAL_CLIENT_PREFIX),
                // Saturating float-to-int: a negative or absurd timestamp
                // becomes 0 or u64::MAX, never a panic.
                seen_ms: (seconds * 1000.0) as u64,
            })
        })
        .collect()
}

/// The selection a run leaves for the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stored {
    pub node: [u8; 16],
    /// `user` in the file: named with `--pn`. Otherwise `adopted`.
    pub chosen_by_user: bool,
}

impl Stored {
    /// The rule's view of the stored node against this run's path table.
    pub fn previous(&self, routes: &[Route]) -> Previous {
        let route = routes.iter().find(|route| route.node == self.node);
        Previous {
            node: self.node,
            reachable: route.is_some(),
            is_own_device: route.is_some_and(|route| route.own_device),
            chosen_by_user: self.chosen_by_user,
        }
    }
}

impl Stored {
    /// The record's text: the node's hex and how it was chosen, one line.
    pub fn to_text(&self) -> String {
        let how = if self.chosen_by_user {
            "user"
        } else {
            "adopted"
        };
        format!("{} {how}\n", crate::address::to_hex(&self.node))
    }

    /// Read a record's text. Anything else than what [`Self::to_text`]
    /// writes is no selection: the rule adopts again, which is the right
    /// answer for a selection nobody can read.
    pub fn from_text(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        let node = crate::address::parse(words.next()?).ok()?;
        let chosen_by_user = match words.next()? {
            "user" => true,
            "adopted" => false,
            _ => return None,
        };
        Some(Self {
            node,
            chosen_by_user,
        })
    }
}

/// Load `home/mailbox`. A missing or unreadable file is no selection, like a
/// damaged one ([`Stored::from_text`]).
#[cfg(feature = "std")]
pub fn load(home: &std::path::Path) -> Option<Stored> {
    Stored::from_text(&std::fs::read_to_string(home.join(MAILBOX_FILE)).ok()?)
}

/// Write `home/mailbox`, through a rename so a crash leaves the old file.
#[cfg(feature = "std")]
pub fn save(home: &std::path::Path, stored: &Stored) -> std::io::Result<()> {
    let path = home.join(MAILBOX_FILE);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, stored.to_text())?;
    std::fs::rename(&tmp, &path)
}

/// What the engine decided, for the frontend to show and persist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The stored selection stands.
    Kept { node: [u8; 16] },
    /// The stored selection stands although the daemon's path table could
    /// not be read: it was vetted when it was written, or the user chose it,
    /// and an identity does not move onto this device by itself.
    KeptWithoutTable { node: [u8; 16] },
    /// A node was adopted; it is written back as `adopted`.
    Adopted {
        node: [u8; 16],
        hops: u8,
        seen_ms: u64,
        reason: AdoptReason,
    },
}

impl Decision {
    /// The one stderr line a run that adopts or keeps prints.
    pub fn line(&self, now_ms: u64) -> String {
        match self {
            Self::Kept { node } => format!("mailbox: {} kept", short(node)),
            Self::KeptWithoutTable { node } => {
                format!("mailbox: {} kept (path table unreadable)", short(node))
            }
            Self::Adopted {
                node,
                hops,
                seen_ms,
                reason,
            } => format!(
                "mailbox: {} ({hops} hop{}, seen {}) adopted: {}",
                short(node),
                if *hops == 1 { "" } else { "s" },
                age(now_ms.saturating_sub(*seen_ms)),
                reason.as_str()
            ),
        }
    }

    /// What goes into the file, if anything changed.
    pub fn to_store(&self) -> Option<Stored> {
        match self {
            Self::Kept { .. } | Self::KeptWithoutTable { .. } => None,
            Self::Adopted { node, .. } => Some(Stored {
                node: *node,
                chosen_by_user: false,
            }),
        }
    }
}

/// How one run selects its mailbox, as `main` hands it to the send and
/// fetch runners. Where the selection is kept is the runners' [`Effects`].
#[derive(Debug, Clone, Default)]
pub struct MailboxRun {
    /// The rule's input, for a run with neither `--pn` nor a configured node.
    pub input: Option<MailboxInput>,
}

/// Show the decision and keep it. A selection that cannot be kept costs the
/// next run a re-adoption, not this run its mailbox, so it is reported and
/// not fatal.
pub fn report(decision: &Decision, effects: &mut impl Effects) {
    let line = decision.line(effects.unix_now_ms());
    effects.notice(&line);
    if let Some(stored) = decision.to_store() {
        keep(&stored, effects);
    }
}

/// Keep a node the user named with `--pn`, once it is selected.
pub fn record_user_choice(node: [u8; 16], effects: &mut impl Effects) {
    keep(
        &Stored {
            node,
            chosen_by_user: true,
        },
        effects,
    );
}

fn keep(stored: &Stored, effects: &mut impl Effects) {
    if let Err(error) = effects.save_mailbox(stored) {
        effects.notice(&format!("could not record the mailbox: {error}"));
    }
}

/// The wall clock in Unix milliseconds, the path table's timescale.
#[cfg(feature = "std")]
pub fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn short(node: &[u8; 16]) -> String {
    crate::address::to_hex(&node[..4])
}

fn age(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0..=119 => format!("{seconds} s ago"),
        120..=7199 => format!("{} min ago", seconds / 60),
        7200..=172_799 => format!("{} h ago", seconds / 3600),
        _ => format!("{} d ago", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(hash: &str, hops: u64, interface: &str, timestamp: f64) -> serde_json::Value {
        serde_json::json!({
            "hash": hash,
            "hops": hops,
            "interface": interface,
            "timestamp": timestamp,
            "via": hash,
        })
    }

    /// The three rows Part A measured, fed back through the parser.
    #[test]
    fn the_measured_rows_classify_as_measured() {
        let local_lnsd = "b6a7e221e807044b2041af6ca10d1b5b";
        let remote_lnsd = "ae905591eaebd434678cbd1f59b1ba4d";
        let local_rnsd = "054777234535a19d45e890c8d5fa45c1";
        let table = serde_json::json!([
            entry(
                local_lnsd,
                0,
                "LocalInterface[rns/lnlocalA]",
                1_761_356_349.5
            ),
            entry(
                remote_lnsd,
                1,
                "TCPInterface[toB/127.0.0.1:47103]",
                1_761_356_373.0
            ),
            entry(
                local_rnsd,
                0,
                "LocalInterface[rns/pylocalA]",
                1_761_356_434.25
            ),
        ]);
        let routes = routes_from_path_table(&table);
        let own: Vec<bool> = routes.iter().map(|route| route.own_device).collect();
        assert_eq!(own, vec![true, false, true]);
        assert_eq!(routes[0].seen_ms, 1_761_356_349_500);
        assert_eq!(routes[1].hops, 1);
    }

    /// Zero hops alone is not enough, nor is the interface name alone: a
    /// one-hop path that happens to come in over a local client is a node
    /// behind another program on this host's daemon, not one beside it.
    #[test]
    fn own_device_needs_both_zero_hops_and_a_local_client() {
        let hash = "00112233445566778899aabbccddeeff";
        let table = serde_json::json!([
            entry(hash, 1, "LocalInterface[rns/default]", 1.0),
            entry(hash, 0, "TCPInterface[x]", 1.0),
        ]);
        assert!(routes_from_path_table(&table)
            .iter()
            .all(|route| !route.own_device));
    }

    #[test]
    fn a_malformed_entry_is_skipped() {
        let table = serde_json::json!([
            {"hash": "zz", "hops": 1, "interface": "x", "timestamp": 1.0},
            {"hash": "00112233445566778899aabbccddeeff", "hops": 1},
            entry("00112233445566778899aabbccddeeff", 2, "TCPInterface[x]", 1.0),
        ]);
        assert_eq!(routes_from_path_table(&table).len(), 1);
        assert!(routes_from_path_table(&serde_json::json!(null)).is_empty());
    }

    #[test]
    fn the_file_round_trips_and_a_damaged_one_is_no_selection() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(load(dir.path()), None);
        for chosen_by_user in [false, true] {
            let stored = Stored {
                node: [0x5a; 16],
                chosen_by_user,
            };
            save(dir.path(), &stored).expect("save");
            assert_eq!(load(dir.path()), Some(stored));
        }
        std::fs::write(dir.path().join(MAILBOX_FILE), "5a5a nonsense\n").expect("write");
        assert_eq!(load(dir.path()), None);
    }

    #[test]
    fn the_record_text_is_one_line_naming_node_and_choice() {
        let stored = Stored {
            node: [0x5a; 16],
            chosen_by_user: true,
        };
        assert_eq!(stored.to_text(), "5a".repeat(16) + " user\n");
        assert_eq!(Stored::from_text(&stored.to_text()), Some(stored));
        assert_eq!(Stored::from_text(&("5a".repeat(16) + " maybe\n")), None);
        assert_eq!(Stored::from_text(""), None);
    }

    /// What `loquax` prints and keeps for a decision, as the effects see it:
    /// an adoption is shown and kept, a kept selection only shown.
    #[test]
    fn a_report_shows_every_decision_and_keeps_only_an_adoption() {
        let mut effects = crate::effects::fake::Recorded {
            now_ms: 181_000,
            ..Default::default()
        };
        let adopted = Decision::Adopted {
            node: [0xab; 16],
            hops: 2,
            seen_ms: 1_000,
            reason: AdoptReason::NoneSelected,
        };
        report(&adopted, &mut effects);
        report(&Decision::Kept { node: [0xab; 16] }, &mut effects);
        assert_eq!(
            effects.notices,
            vec![adopted.line(181_000), "mailbox: abababab kept".to_string()]
        );
        assert_eq!(
            effects.mailboxes,
            vec![Stored {
                node: [0xab; 16],
                chosen_by_user: false
            }]
        );
        record_user_choice([0xcd; 16], &mut effects);
        assert_eq!(
            effects.mailboxes.last().map(|s| s.chosen_by_user),
            Some(true)
        );
    }

    #[test]
    fn the_previous_selection_reads_the_path_table() {
        let routes = [Route {
            node: [1; 16],
            hops: 0,
            own_device: true,
            seen_ms: 5,
        }];
        let stored = Stored {
            node: [1; 16],
            chosen_by_user: false,
        };
        let previous = stored.previous(&routes);
        assert!(previous.reachable && previous.is_own_device);
        let gone = Stored {
            node: [2; 16],
            ..stored
        }
        .previous(&routes);
        assert!(!gone.reachable && !gone.is_own_device);
    }

    #[test]
    fn the_lines_read_as_the_order_spells_them() {
        let adopted = Decision::Adopted {
            node: [0xab; 16],
            hops: 2,
            seen_ms: 1_000,
            reason: AdoptReason::NoneSelected,
        };
        assert_eq!(
            adopted.line(181_000),
            "mailbox: abababab (2 hops, seen 3 min ago) adopted: no mailbox was selected; \
             it is the most recently heard eligible node"
        );
        assert_eq!(
            Decision::Kept { node: [0xab; 16] }.line(0),
            "mailbox: abababab kept"
        );
        assert_eq!(adopted.to_store().map(|s| s.chosen_by_user), Some(false));
        assert_eq!(Decision::Kept { node: [0xab; 16] }.to_store(), None);
        let unreadable = Decision::KeptWithoutTable { node: [0xab; 16] };
        assert_eq!(
            unreadable.line(0),
            "mailbox: abababab kept (path table unreadable)"
        );
        assert_eq!(unreadable.to_store(), None);
    }
}
