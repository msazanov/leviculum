//! The home node's outbox policy (`docs/src/concepts/loquax-home-node.md`,
//! "Sending through the home node"): a pure state machine, fed the events
//! that matter and answering with what to do, so every rule is tested
//! without a network. The daemon (`super::daemon`) carries it out.
//!
//! 1. Direct delivery at once, over a link, so a peer that is not there
//!    fails crisply after the router's stock attempts.
//! 2. On direct failure, the same packed message goes to the propagation
//!    node, and direct delivery is tried again on every announce of the
//!    recipient and every [`Policy::retry_secs`], for [`Policy::horizon_secs`].
//! 3. DELIVERED when a direct proof arrives, even after the propagation node
//!    took a copy: the recipient keeps one message per hash
//!    (`reference/LXMF/LXMF/LXMRouter.py:1906-1908`), so the copy it later
//!    collects is dropped there.
//!
//! The router holds one entry per message id, so one message is never on
//! two legs at once: a leg starts only when the last one ended.

use alloc::vec::Vec;

/// How often a message waiting for its recipient is tried direct again,
/// besides on every announce of the recipient: an hour. The announce is the
/// retry that matters, since it says the recipient is there; the cadence
/// covers an announce the home node did not hear, at a cost of one link
/// attempt an hour, 168 over the seven days.
pub const DEFAULT_RETRY_SECS: u64 = 3600;
/// How many days a message is retried direct: seven, as Lew decided on
/// 2026-10-29.
pub const DEFAULT_DAYS: u64 = 7;

/// The two ways a message leaves the home node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leg {
    /// To the recipient over a link, with a delivery proof.
    Direct,
    /// To the propagation node, for the recipient to collect.
    Propagated,
}

/// How a leg ended, as the router left it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A delivery proof came.
    Delivered,
    /// Handed on without a proof: a propagation node took it.
    Sent,
    /// It did not go.
    Failed,
}

/// A state change of an outbox message: each one is a row update and an
/// archive entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A propagation node took a copy; direct retries go on.
    Propagated,
    /// A direct delivery proof came; nothing more is tried.
    Delivered,
    /// The horizon passed without a proof; nothing more is tried.
    /// `propagated` says whether a copy waits on a propagation node.
    Ended { propagated: bool },
}

impl Change {
    /// The archive kind clients see this change as.
    pub fn kind(self) -> u8 {
        match self {
            Self::Propagated => super::KIND_PROPAGATED,
            Self::Delivered => super::KIND_DELIVERED,
            Self::Ended { propagated: false } => super::KIND_FAILED,
            Self::Ended { propagated: true } => super::KIND_RETRIES_ENDED,
        }
    }

    /// The word the daemon's log uses.
    pub fn word(self) -> &'static str {
        match self {
            Self::Propagated => "propagated",
            Self::Delivered => "delivered",
            Self::Ended { propagated: false } => "failed",
            Self::Ended { propagated: true } => "retries-ended",
        }
    }
}

/// What the daemon is to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Hand the message to the router on `leg`.
    Send { id: [u8; 32], leg: Leg },
    /// Record a state change: the outbox row and an archive entry.
    Record { id: [u8; 32], change: Change },
}

/// The timing, from the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// The slow cadence of direct retries, in seconds.
    pub retry_secs: i64,
    /// How long after it was handed in a message is retried, in seconds.
    pub horizon_secs: i64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            retry_secs: DEFAULT_RETRY_SECS as i64,
            horizon_secs: (DEFAULT_DAYS * 24 * 3600) as i64,
        }
    }
}

struct Item {
    id: [u8; 32],
    destination: [u8; 16],
    packed: Vec<u8>,
    created_at: i64,
    propagated: bool,
    in_flight: Option<Leg>,
    next_direct_at: i64,
}

/// The messages still retried, in the order they came.
pub struct Outgoing {
    policy: Policy,
    items: Vec<Item>,
}

impl Outgoing {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            items: Vec::new(),
        }
    }

    /// Take one message in: a new one from `/send`, or one the store kept
    /// across a restart. Either is due direct at the next [`Outgoing::tick`].
    pub fn add(
        &mut self,
        id: [u8; 32],
        destination: [u8; 16],
        packed: Vec<u8>,
        created_at: i64,
        propagated: bool,
    ) {
        if self.items.iter().any(|item| item.id == id) {
            return;
        }
        self.items.push(Item {
            id,
            destination,
            packed,
            created_at,
            propagated,
            in_flight: None,
            next_direct_at: i64::MIN,
        });
    }

    /// The packed message, while it is retried.
    pub fn packed(&self, id: &[u8; 32]) -> Option<&[u8]> {
        self.item(id).map(|item| item.packed.as_slice())
    }

    /// The leg `id` is on now.
    pub fn in_flight(&self, id: &[u8; 32]) -> Option<Leg> {
        self.item(id).and_then(|item| item.in_flight)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn item(&self, id: &[u8; 32]) -> Option<&Item> {
        self.items.iter().find(|item| item.id == *id)
    }

    /// The clock moved: end what passed its horizon, and try direct what is
    /// due.
    pub fn tick(&mut self, now: i64) -> Vec<Act> {
        let mut acts = Vec::new();
        let horizon = self.policy.horizon_secs;
        let retry = self.policy.retry_secs;
        self.items.retain_mut(|item| {
            if item.in_flight.is_some() {
                return true;
            }
            if now.saturating_sub(item.created_at) >= horizon {
                acts.push(Act::Record {
                    id: item.id,
                    change: Change::Ended {
                        propagated: item.propagated,
                    },
                });
                return false;
            }
            if now >= item.next_direct_at {
                item.in_flight = Some(Leg::Direct);
                item.next_direct_at = now.saturating_add(retry);
                acts.push(Act::Send {
                    id: item.id,
                    leg: Leg::Direct,
                });
            }
            true
        });
        acts
    }

    /// `destination` announced: it is there now, so every message for it
    /// that is not on a leg goes direct at once.
    pub fn announced(&mut self, destination: [u8; 16], now: i64) -> Vec<Act> {
        let horizon = self.policy.horizon_secs;
        let retry = self.policy.retry_secs;
        let mut acts = Vec::new();
        for item in &mut self.items {
            if item.destination != destination
                || item.in_flight.is_some()
                || now.saturating_sub(item.created_at) >= horizon
            {
                continue;
            }
            item.in_flight = Some(Leg::Direct);
            item.next_direct_at = now.saturating_add(retry);
            acts.push(Act::Send {
                id: item.id,
                leg: Leg::Direct,
            });
        }
        acts
    }

    /// The router let go of `id` with `outcome`. A message not on a leg is
    /// not this outbox's, and changes nothing.
    pub fn left(&mut self, id: &[u8; 32], outcome: Outcome) -> Vec<Act> {
        let Some(index) = self.items.iter().position(|item| item.id == *id) else {
            return Vec::new();
        };
        let item = &mut self.items[index];
        let Some(leg) = item.in_flight.take() else {
            return Vec::new();
        };
        match (leg, outcome) {
            (Leg::Direct, Outcome::Delivered) => {
                self.items.remove(index);
                alloc::vec![Act::Record {
                    id: *id,
                    change: Change::Delivered,
                }]
            }
            // A direct leg without a proof failed, whatever the router
            // called its last state: only a proof says the peer has it.
            (Leg::Direct, _) if !item.propagated => {
                item.in_flight = Some(Leg::Propagated);
                alloc::vec![Act::Send {
                    id: *id,
                    leg: Leg::Propagated,
                }]
            }
            (Leg::Direct, _) => Vec::new(),
            (Leg::Propagated, Outcome::Sent | Outcome::Delivered) => {
                item.propagated = true;
                alloc::vec![Act::Record {
                    id: *id,
                    change: Change::Propagated,
                }]
            }
            // No node took it; the next direct failure tries again.
            (Leg::Propagated, Outcome::Failed) => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: [u8; 32] = [1; 32];
    const PEER: [u8; 16] = [2; 16];
    const DAY: i64 = 24 * 3600;

    fn outbox() -> Outgoing {
        let mut outgoing = Outgoing::new(Policy::default());
        outgoing.add(ID, PEER, alloc::vec![9; 40], 1000, false);
        outgoing
    }

    fn send(leg: Leg) -> Act {
        Act::Send { id: ID, leg }
    }

    fn record(change: Change) -> Act {
        Act::Record { id: ID, change }
    }

    #[test]
    fn direct_goes_at_once_and_a_proof_ends_it() {
        let mut outgoing = outbox();
        assert_eq!(outgoing.tick(1000), [send(Leg::Direct)]);
        // On a leg, nothing more is started, not even by an announce.
        assert!(outgoing.tick(1001).is_empty());
        assert!(outgoing.announced(PEER, 1002).is_empty());
        assert_eq!(
            outgoing.left(&ID, Outcome::Delivered),
            [record(Change::Delivered)]
        );
        assert!(outgoing.is_empty());
        assert!(outgoing.tick(1000 + 30 * DAY).is_empty());
    }

    #[test]
    fn a_failed_direct_leg_propagates_then_a_later_proof_delivers() {
        let mut outgoing = outbox();
        assert_eq!(outgoing.tick(1000), [send(Leg::Direct)]);
        assert_eq!(outgoing.left(&ID, Outcome::Failed), [send(Leg::Propagated)]);
        assert_eq!(outgoing.in_flight(&ID), Some(Leg::Propagated));
        assert_eq!(
            outgoing.left(&ID, Outcome::Sent),
            [record(Change::Propagated)]
        );
        // Nothing until the recipient announces or the cadence comes round.
        assert!(outgoing.tick(1100).is_empty());
        assert_eq!(outgoing.announced([7; 16], 1200), []);
        assert_eq!(outgoing.announced(PEER, 1200), [send(Leg::Direct)]);
        // A second failure does not propagate again: the node has a copy.
        assert!(outgoing.left(&ID, Outcome::Failed).is_empty());
        // The cadence counts from the last direct attempt.
        assert!(outgoing.tick(1200 + 3599).is_empty());
        assert_eq!(outgoing.tick(1200 + 3600), [send(Leg::Direct)]);
        assert_eq!(
            outgoing.left(&ID, Outcome::Delivered),
            [record(Change::Delivered)]
        );
        assert!(outgoing.is_empty());
    }

    #[test]
    fn a_propagation_no_node_took_is_tried_again_after_the_next_direct_failure() {
        let mut outgoing = outbox();
        outgoing.tick(1000);
        outgoing.left(&ID, Outcome::Failed);
        assert!(outgoing.left(&ID, Outcome::Failed).is_empty());
        assert_eq!(outgoing.tick(1000 + 3600), [send(Leg::Direct)]);
        assert_eq!(outgoing.left(&ID, Outcome::Failed), [send(Leg::Propagated)]);
    }

    #[test]
    fn the_seven_day_horizon_ends_the_retries() {
        let mut outgoing = outbox();
        outgoing.tick(1000);
        outgoing.left(&ID, Outcome::Failed);
        outgoing.left(&ID, Outcome::Sent);
        assert!(outgoing.tick(1000 + 7 * DAY - 1).len() == 1);
        outgoing.left(&ID, Outcome::Failed);
        // An announce after the horizon starts nothing; the tick ends it.
        assert!(outgoing.announced(PEER, 1000 + 7 * DAY).is_empty());
        assert_eq!(
            outgoing.tick(1000 + 7 * DAY),
            [record(Change::Ended { propagated: true })]
        );
        assert!(outgoing.is_empty());
    }

    #[test]
    fn a_message_no_node_ever_took_ends_as_failed() {
        let mut outgoing = Outgoing::new(Policy {
            retry_secs: 10,
            horizon_secs: 100,
        });
        outgoing.add(ID, PEER, Vec::new(), 0, false);
        let mut now = 0;
        while !outgoing.is_empty() {
            for act in outgoing.tick(now) {
                if let Act::Record { change, .. } = act {
                    assert_eq!(change, Change::Ended { propagated: false });
                    assert_eq!(change.kind(), crate::home::KIND_FAILED);
                }
            }
            while outgoing.in_flight(&ID).is_some() {
                outgoing.left(&ID, Outcome::Failed);
            }
            now += 5;
            assert!(now <= 200, "the horizon never ended it");
        }
        assert_eq!(now, 105);
    }

    #[test]
    fn a_leg_running_at_the_horizon_finishes_first() {
        let mut outgoing = Outgoing::new(Policy {
            retry_secs: 10,
            horizon_secs: 100,
        });
        outgoing.add(ID, PEER, Vec::new(), 0, true);
        assert_eq!(outgoing.tick(95), [send(Leg::Direct)]);
        assert!(outgoing.tick(150).is_empty());
        assert_eq!(
            outgoing.left(&ID, Outcome::Delivered),
            [record(Change::Delivered)]
        );
    }

    #[test]
    fn a_restored_message_is_due_at_once_and_keeps_its_propagated_copy() {
        let mut outgoing = Outgoing::new(Policy::default());
        outgoing.add(ID, PEER, Vec::new(), 1000, true);
        assert_eq!(outgoing.tick(5000), [send(Leg::Direct)]);
        assert!(outgoing.left(&ID, Outcome::Failed).is_empty());
    }

    #[test]
    fn what_is_not_on_a_leg_is_not_ours() {
        let mut outgoing = outbox();
        assert!(outgoing.left(&ID, Outcome::Delivered).is_empty());
        assert!(outgoing.left(&[5; 32], Outcome::Delivered).is_empty());
        assert_eq!(outgoing.len(), 1);
        // The same message twice is one.
        outgoing.add(ID, PEER, Vec::new(), 1000, false);
        assert_eq!(outgoing.len(), 1);
    }
}
