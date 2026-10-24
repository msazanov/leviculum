//! Which propagation node is the mailbox: the adoption rule of 2026-10-25.
//!
//! Lew decided (`docs/src/concepts/lnmsg-mailbox.md`, amended 2026-10-25)
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
//! This module is the rule alone, with no I/O: the caller says which
//! candidates exist and which of them live on this device.

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
}

impl AdoptReason {
    /// The words the `mailbox:` line shows after "adopted because".
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoneSelected => "no mailbox was selected; it is the most recently heard node",
            Self::SelectedUnreachable => {
                "the selected mailbox is unreachable; it is the most recently heard node"
            }
            Self::SelectedOnThisDevice => {
                "the selected mailbox runs on this device and holds nothing while it is off"
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
    /// The words the UI shows when there is no mailbox.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoCandidates => "no mailbox chosen: no propagation node has been heard",
            Self::OnlyOnThisDevice => {
                "no mailbox chosen: the only propagation node heard runs on this device, \
                 and it holds nothing while this device is off"
            }
            Self::NoneReachable => {
                "no mailbox chosen: no propagation node heard has a route right now"
            }
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

/// Decide the mailbox. The previous selection is kept while it is reachable
/// and not a node on this device the user never chose. Otherwise the most
/// recently heard reachable node not on this device is adopted, fewer hops
/// breaking a tie and the hash breaking the rest, so the answer is the same
/// whatever order the candidates arrive in.
pub fn select(candidates: &[Candidate], previous: Option<Previous>) -> Selection {
    let reason = match previous {
        None => AdoptReason::NoneSelected,
        Some(p) if !p.reachable => AdoptReason::SelectedUnreachable,
        Some(p) if p.is_own_device && !p.chosen_by_user => AdoptReason::SelectedOnThisDevice,
        Some(p) => return Selection::Keep(p.node),
    };
    let best = candidates
        .iter()
        .filter(|c| c.reachable && !c.is_own_device)
        .min_by(|a, b| {
            b.last_seen_ms
                .cmp(&a.last_seen_ms)
                .then(a.hops.cmp(&b.hops))
                .then(a.node.cmp(&b.node))
        });
    match best {
        Some(candidate) => Selection::Adopt(*candidate, reason),
        None if candidates.is_empty() => Selection::None(NoneReason::NoCandidates),
        None if candidates.iter().all(|c| c.is_own_device) => {
            Selection::None(NoneReason::OnlyOnThisDevice)
        }
        None => Selection::None(NoneReason::NoneReachable),
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
            assert_eq!(select(&candidates, prev), want, "{name}");
            let mut reversed = candidates.clone();
            reversed.reverse();
            assert_eq!(select(&reversed, prev), want, "{name}, reversed order");
        }
    }
}
