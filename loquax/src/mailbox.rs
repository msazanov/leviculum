//! Which propagation node is the mailbox: the adoption rule of 2026-10-25.
//!
//! Lew decided (`docs/src/concepts/loquax-mailbox.md`, amended 2026-10-25)
//! that the messenger adopts the best candidate by itself on first run and
//! whenever the selected node becomes unreachable, and shows the choice; a
//! picker changes it, it is not a gate before it. Adoption makes a node
//! *selected*, never *trusted*.
//!
//! One exclusion: a node on this device is never adopted. A mailbox is where
//! others deliver while the messenger is off, and a node that is off whenever
//! the messenger is holds nothing. A stationary node elsewhere in the
//! household is a fine mailbox; the rule is "not the node on this device",
//! not "not our node". A user may still choose the own node explicitly, and
//! then it is kept.
//!
//! "Unreachable" is two things. A node with no route in the path table is
//! unreachable at once. A node with a route that does not answer is
//! unreachable after [`UNANSWERED_LIMIT`] syncs in a row that failed before
//! a link to it came up ([`Unanswered`]); it is then passed over for
//! [`UNANSWERED_EXCLUSION_MS`]. A node the user chose is never replaced.
//!
//! This module is the rule alone, with no I/O: the caller says which
//! candidates exist, which of them live on this device and which stopped
//! answering.

use alloc::vec::Vec;

/// Syncs in a row that fail before a link comes up, after which an adopted
/// node counts as unreachable. One failure is no verdict: on a path of
/// several hops a link request or its proof is lost now and then (of nine
/// link requests to a node five hops out, measured 2026-10-27, at most three
/// were answered), and the next sync's retry answers it. Three in a row, at
/// the schedule's retries of 30 s and 60 s (`crate::sync::RETRY_FIRST_MS`)
/// with each failure taking up to the fetch's budget, is three to five
/// minutes of a node that answers nothing; a node that answers one link in
/// three is replaced after three misses about once in four tries, which is
/// the right call for a mailbox that loses two collections in three.
pub const UNANSWERED_LIMIT: u32 = 3;
/// How long a node that stopped answering is passed over: an hour, four
/// bounds of the default schedule. After it the node is a candidate again,
/// and is only adopted when the node that replaced it fails in turn.
pub const UNANSWERED_EXCLUSION_MS: u64 = 60 * 60_000;

/// A propagation node the messenger could use as its mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// The node's `lxmf.propagation` destination hash.
    pub node: [u8; 16],
    /// Hops to the node, as the path table has it.
    pub hops: u8,
    /// When the node last announced, in the caller's clock (milliseconds).
    pub last_seen_ms: u64,
    /// Whether a route to the node is known now.
    pub reachable: bool,
    /// The node runs on this device: this messenger's own propagation node,
    /// or the one beside the daemon it is attached to.
    pub is_own_device: bool,
}

/// The node selected before this run, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Previous {
    pub node: [u8; 16],
    pub reachable: bool,
    /// The node runs on this device (see [`Candidate::is_own_device`]).
    pub is_own_device: bool,
    /// The user picked it, rather than an earlier adoption. Only an explicit
    /// choice keeps a node on this device.
    pub chosen_by_user: bool,
}

/// Why a node was adopted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdoptReason {
    /// Nothing was selected before.
    NoneSelected,
    /// The selected node has no route now.
    SelectedUnreachable,
    /// The selected node runs on this device and nobody chose it explicitly.
    SelectedOnThisDevice,
    /// The selected node has a route and did not answer
    /// [`UNANSWERED_LIMIT`] syncs in a row.
    SelectedDidNotAnswer,
}

impl AdoptReason {
    /// The words the `mailbox:` line shows after "adopted because".
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoneSelected => "none was selected; it is the most recently heard eligible node",
            Self::SelectedUnreachable => {
                "the selected one is unreachable; it is the most recently heard eligible node"
            }
            Self::SelectedOnThisDevice => {
                "the selected one runs on this device and holds nothing while it is off"
            }
            Self::SelectedDidNotAnswer => {
                "the selected one did not answer; it is the most recently heard eligible node"
            }
        }
    }
}

/// Why no node is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoneReason {
    /// No propagation node is known at all.
    NoCandidates,
    /// Every known node runs on this device.
    OnlyOnThisDevice,
    /// Nodes are known, but none has a route now.
    NoneReachable,
}

impl NoneReason {
    /// Why there is no propagation node, after the `no propagation node:`
    /// the send and fetch errors put in front (since 1153; the words said
    /// `no mailbox chosen:` themselves before).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoCandidates => "none has been heard",
            Self::OnlyOnThisDevice => {
                "the only one heard runs on this device, \
                 and it holds nothing while this device is off"
            }
            Self::NoneReachable => "none heard has a route right now",
        }
    }
}

/// The rule's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Keep([u8; 16]),
    Adopt(Candidate, AdoptReason),
    None(NoneReason),
}

/// Decide the mailbox. The previous selection is kept while it is reachable,
/// answering, and not a node on this device the user never chose. Otherwise
/// the most recently heard reachable node not on this device and not
/// `excluded` is adopted, fewer hops breaking a tie and the hash breaking the
/// rest, so the answer is the same whatever order the candidates arrive in.
/// A previous node that stopped answering is kept when no other node can
/// replace it: a mailbox that might answer later beats none.
pub fn select(
    candidates: &[Candidate],
    previous: Option<Previous>,
    excluded: &[[u8; 16]],
) -> Selection {
    let reason = match previous {
        None => AdoptReason::NoneSelected,
        Some(p) if !p.reachable => AdoptReason::SelectedUnreachable,
        Some(p) if p.is_own_device && !p.chosen_by_user => AdoptReason::SelectedOnThisDevice,
        Some(p) if !p.chosen_by_user && excluded.contains(&p.node) => {
            AdoptReason::SelectedDidNotAnswer
        }
        Some(p) => return Selection::Keep(p.node),
    };
    let best = candidates
        .iter()
        .filter(|c| c.reachable && !c.is_own_device && !excluded.contains(&c.node))
        .min_by(|a, b| {
            b.last_seen_ms
                .cmp(&a.last_seen_ms)
                .then(a.hops.cmp(&b.hops))
                .then(a.node.cmp(&b.node))
        });
    match (best, previous) {
        (Some(candidate), _) => Selection::Adopt(*candidate, reason),
        (None, Some(p)) if reason == AdoptReason::SelectedDidNotAnswer => Selection::Keep(p.node),
        (None, _) if candidates.is_empty() => Selection::None(NoneReason::NoCandidates),
        (None, _) if candidates.iter().all(|c| c.is_own_device) => {
            Selection::None(NoneReason::OnlyOnThisDevice)
        }
        (None, _) => Selection::None(NoneReason::NoneReachable),
    }
}

/// The streak of syncs that failed before a link to the mailbox came up, and
/// the nodes it has excluded. The caller's clock, in milliseconds, monotonic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unanswered {
    /// The node of the current streak and its length.
    streak: Option<([u8; 16], u32)>,
    /// Excluded nodes and when each exclusion ends.
    excluded: Vec<([u8; 16], u64)>,
}

impl Unanswered {
    /// A sync with `node` completed: its streak ends.
    pub fn answered(&mut self, node: [u8; 16]) {
        if self.streak.is_some_and(|(n, _)| n == node) {
            self.streak = None;
        }
    }

    /// A sync with `node` failed. `before_link` says no link to it came up;
    /// a failure after the link (the transfer, a refusal) is an answer and
    /// ends the streak. True when this failure is the [`UNANSWERED_LIMIT`]th
    /// in a row: the node counts as unreachable, and the streak starts over.
    pub fn failed(&mut self, node: [u8; 16], before_link: bool) -> bool {
        if !before_link {
            self.answered(node);
            return false;
        }
        let count = match self.streak {
            Some((n, count)) if n == node => count + 1,
            _ => 1,
        };
        if count >= UNANSWERED_LIMIT {
            self.streak = None;
            true
        } else {
            self.streak = Some((node, count));
            false
        }
    }

    /// Pass `node` over from `now_ms` for [`UNANSWERED_EXCLUSION_MS`].
    pub fn exclude(&mut self, node: [u8; 16], now_ms: u64) {
        self.excluded.retain(|(n, _)| *n != node);
        self.excluded
            .push((node, now_ms.saturating_add(UNANSWERED_EXCLUSION_MS)));
    }

    /// The nodes passed over at `now_ms`.
    pub fn excluded(&self, now_ms: u64) -> Vec<[u8; 16]> {
        self.excluded
            .iter()
            .filter(|(_, until)| now_ms < *until)
            .map(|(node, _)| *node)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(byte: u8) -> [u8; 16] {
        [byte; 16]
    }

    fn candidate(byte: u8, hops: u8, last_seen_ms: u64) -> Candidate {
        Candidate {
            node: node(byte),
            hops,
            last_seen_ms,
            reachable: true,
            is_own_device: false,
        }
    }

    fn own(byte: u8, hops: u8, last_seen_ms: u64) -> Candidate {
        Candidate {
            is_own_device: true,
            ..candidate(byte, hops, last_seen_ms)
        }
    }

    fn previous(byte: u8, reachable: bool) -> Previous {
        Previous {
            node: node(byte),
            reachable,
            is_own_device: false,
            chosen_by_user: false,
        }
    }

    #[test]
    fn the_table() {
        let a = candidate(0xaa, 3, 1_000);
        let b = candidate(0xbb, 1, 2_000);
        let c_same_age_fewer_hops = candidate(0xcc, 0, 2_000);
        let mine = own(0x01, 0, 9_000);
        let gone = Candidate {
            reachable: false,
            ..candidate(0xdd, 1, 8_000)
        };
        let cases: Vec<(&str, Vec<Candidate>, Option<Previous>, Selection)> = vec![
            (
                "nothing heard",
                vec![],
                None,
                Selection::None(NoneReason::NoCandidates),
            ),
            (
                "only candidate is the own device",
                vec![mine],
                None,
                Selection::None(NoneReason::OnlyOnThisDevice),
            ),
            (
                "only candidate has no route",
                vec![gone],
                None,
                Selection::None(NoneReason::NoneReachable),
            ),
            (
                "first run takes the most recently heard",
                vec![a, b],
                None,
                Selection::Adopt(b, AdoptReason::NoneSelected),
            ),
            (
                "first run passes over the own device though it is newest",
                vec![a, mine],
                None,
                Selection::Adopt(a, AdoptReason::NoneSelected),
            ),
            (
                "first run passes over a node with no route",
                vec![a, gone],
                None,
                Selection::Adopt(a, AdoptReason::NoneSelected),
            ),
            (
                "same age: fewer hops wins",
                vec![b, c_same_age_fewer_hops],
                None,
                Selection::Adopt(c_same_age_fewer_hops, AdoptReason::NoneSelected),
            ),
            (
                "selected node still reachable is kept over a newer one",
                vec![a, b],
                Some(previous(0xaa, true)),
                Selection::Keep(node(0xaa)),
            ),
            (
                "selected node gone: adopt the next",
                vec![a, b],
                Some(previous(0xee, false)),
                Selection::Adopt(b, AdoptReason::SelectedUnreachable),
            ),
            (
                "selected node gone and only the own device left",
                vec![mine],
                Some(previous(0xee, false)),
                Selection::None(NoneReason::OnlyOnThisDevice),
            ),
            (
                "own device adopted earlier is replaced",
                vec![a, mine],
                Some(Previous {
                    is_own_device: true,
                    ..previous(0x01, true)
                }),
                Selection::Adopt(a, AdoptReason::SelectedOnThisDevice),
            ),
            (
                "own device the user chose is kept",
                vec![a, mine],
                Some(Previous {
                    is_own_device: true,
                    chosen_by_user: true,
                    ..previous(0x01, true)
                }),
                Selection::Keep(node(0x01)),
            ),
        ];
        for (name, candidates, prev, want) in cases {
            assert_eq!(select(&candidates, prev, &[]), want, "{name}");
            let mut reversed = candidates.clone();
            reversed.reverse();
            assert_eq!(select(&reversed, prev, &[]), want, "{name}, reversed order");
        }
    }

    #[test]
    fn a_node_that_stopped_answering_is_replaced_and_not_adopted() {
        let a = candidate(0xaa, 1, 1_000);
        let b = candidate(0xbb, 5, 2_000);
        let silent = [node(0xbb)];
        let cases: Vec<(&str, Vec<Candidate>, Option<Previous>, Selection)> = vec![
            (
                "adopted node stopped answering: the next one",
                vec![a, b],
                Some(previous(0xbb, true)),
                Selection::Adopt(a, AdoptReason::SelectedDidNotAnswer),
            ),
            (
                "first run passes over a node that stopped answering though it is newest",
                vec![a, b],
                None,
                Selection::Adopt(a, AdoptReason::NoneSelected),
            ),
            (
                "no other node: the silent one is kept",
                vec![b],
                Some(previous(0xbb, true)),
                Selection::Keep(node(0xbb)),
            ),
            (
                "a node the user chose is never replaced",
                vec![a, b],
                Some(Previous {
                    chosen_by_user: true,
                    ..previous(0xbb, true)
                }),
                Selection::Keep(node(0xbb)),
            ),
            (
                "no route outranks not answering",
                vec![a],
                Some(previous(0xbb, false)),
                Selection::Adopt(a, AdoptReason::SelectedUnreachable),
            ),
        ];
        for (name, candidates, prev, want) in cases {
            assert_eq!(select(&candidates, prev, &silent), want, "{name}");
            let mut reversed = candidates.clone();
            reversed.reverse();
            assert_eq!(
                select(&reversed, prev, &silent),
                want,
                "{name}, reversed order"
            );
        }
    }

    #[test]
    fn three_failures_before_a_link_in_a_row_make_a_node_unreachable() {
        let (x, y) = (node(0x0a), node(0x0b));
        let mut unanswered = Unanswered::default();
        assert!(!unanswered.failed(x, true));
        assert!(!unanswered.failed(x, true));
        assert!(unanswered.failed(x, true), "the third in a row");
        assert!(!unanswered.failed(x, true), "the streak starts over");

        // A completed sync ends the streak.
        let mut unanswered = Unanswered::default();
        unanswered.failed(x, true);
        unanswered.failed(x, true);
        unanswered.answered(x);
        assert!(!unanswered.failed(x, true));

        // A failure after the link is an answer, and ends it too.
        let mut unanswered = Unanswered::default();
        unanswered.failed(x, true);
        unanswered.failed(x, true);
        assert!(!unanswered.failed(x, false));
        assert!(!unanswered.failed(x, true));

        // Another node's failure starts its own streak.
        let mut unanswered = Unanswered::default();
        unanswered.failed(x, true);
        unanswered.failed(x, true);
        assert!(!unanswered.failed(y, true));
        assert!(!unanswered.failed(x, true));
    }

    #[test]
    fn an_exclusion_ends_after_its_window() {
        let x = node(0x0a);
        let mut unanswered = Unanswered::default();
        unanswered.exclude(x, 1_000);
        assert_eq!(unanswered.excluded(1_000), vec![x]);
        assert_eq!(
            unanswered.excluded(1_000 + UNANSWERED_EXCLUSION_MS - 1),
            vec![x]
        );
        assert!(unanswered
            .excluded(1_000 + UNANSWERED_EXCLUSION_MS)
            .is_empty());
        // Excluded again, the window starts again and the node is listed once.
        unanswered.exclude(x, 5_000);
        unanswered.exclude(x, 6_000);
        assert_eq!(
            unanswered.excluded(6_000 + UNANSWERED_EXCLUSION_MS - 1),
            vec![x]
        );
    }
}
