//! A direct delivery to a recipient that has left ends `Failed` even when
//! every path request is answered at once (leviculum 1110).
//!
//! The field case: the home node runs as a shared instance client, the
//! recipient was another local client of the same daemon and exited. The
//! daemon keeps the recipient's path, so it answers every path request from
//! its table within milliseconds, while the link requests it forwards go
//! nowhere. Each failed handshake makes the core expire the path and ask for
//! it again (`link_management.rs`, `emit_link_closed`); the answer arrives
//! long before the router's next pass. This harness plays that daemon: every
//! link close is answered with a fresh announce of the recipient.

use super::*;
use alloc::{collections::VecDeque, rc::Rc};
use core::cell::Cell;

use leviculum_core::{
    Identity, InterfaceId, MemoryStorage, NodeCoreBuilder, NodeEvent, TickOutput,
};
use rand_core::OsRng;

use crate::{announce, node::LxmfNodeConfig};

const WALL_UNIX: u64 = leviculum_core::constants::BUILD_UNIX_SECS + 86_400;

struct SharedClock(Rc<Cell<u64>>);

impl Clock for SharedClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
    fn wall_unix_secs(&self) -> Option<u64> {
        Some(WALL_UNIX)
    }
}

type ClientNode = NodeCore<OsRng, SharedClock, MemoryStorage>;

fn identity(seed: u8) -> Identity {
    let mut private = [0u8; 64];
    for (index, byte) in private.iter_mut().enumerate() {
        *byte = seed.wrapping_add(index as u8);
    }
    Identity::from_private_key_bytes(&private).expect("deterministic identity")
}

struct Harness {
    node: ClientNode,
    clock: Rc<Cell<u64>>,
    router: LxmfRouter,
    events: Vec<RouterEvent>,
    recipient_seed: u8,
    /// Whether the simulated daemon answers path requests.
    daemon_answers: bool,
    /// Initiator link closes seen for the recipient, one per link the
    /// client gave up on.
    links_closed: usize,
}

impl Harness {
    fn new(daemon_answers: bool) -> Self {
        let clock = Rc::new(Cell::new(1_000));
        let mut node = NodeCoreBuilder::new().build(
            OsRng,
            SharedClock(Rc::clone(&clock)),
            MemoryStorage::with_defaults(),
        );
        let own = identity(1);
        let own_hash = *own.hash();
        let destination = LxmfNode::delivery_destination(own).expect("delivery destination");
        let lxmf = LxmfNode::register(&mut node, destination, LxmfNodeConfig::default())
            .expect("register delivery destination");
        Self {
            node,
            clock,
            router: LxmfRouter::new(lxmf, own_hash, RouterConfig::default()),
            events: Vec::new(),
            recipient_seed: 0x42,
            daemon_answers,
            links_closed: 0,
        }
    }

    /// The recipient's announce, as the daemon relays it (or answers a path
    /// request with it). Fresh random bytes each time, so the core never
    /// drops it as a replay.
    fn announce_recipient(&mut self) -> DestinationHash {
        let mut destination = LxmfNode::delivery_destination(identity(self.recipient_seed))
            .expect("recipient delivery destination");
        let hash = *destination.hash();
        let app_data = announce::delivery(Some(b"recipient"), None);
        let now_ms = self.node.now_ms();
        let packet = destination
            .announce(Some(&app_data), &mut OsRng, now_ms, now_ms / 1000)
            .expect("recipient announce");
        let mut packed = alloc::vec![0; packet.packed_size()];
        let length = packet.pack(&mut packed).expect("pack announce");
        let core = self.node.handle_packet(InterfaceId(0), &packed[..length]);
        self.absorb_core(core);
        hash
    }

    fn absorb_core(&mut self, core: TickOutput) {
        let mut events: VecDeque<NodeEvent> = core.events.into();
        let mut answer_path = false;
        while let Some(event) = events.pop_front() {
            if let NodeEvent::LinkClosed {
                is_initiator: true, ..
            } = event
            {
                self.links_closed += 1;
                answer_path = self.daemon_answers;
            }
            let follow_up = self
                .router
                .handle_event(&mut self.node, &event)
                .expect("router handles NodeCore event");
            self.events.extend(follow_up.events);
            events.extend(follow_up.core.events);
        }
        if answer_path {
            self.announce_recipient();
        }
    }

    fn step(&mut self, ms: u64) {
        self.clock.set(self.clock.get() + ms);
        let core = self.node.handle_timeout();
        self.absorb_core(core);
        let output = self.router.tick(&mut self.node).expect("router tick");
        self.events.extend(output.events);
        self.absorb_core(output.core);
    }

    fn failed(&self, id: &[u8; 32]) -> bool {
        self.events.iter().any(|event| {
            matches!(
                event,
                RouterEvent::MessageState { message_id, state: MessageState::Failed }
                    if message_id == id
            )
        })
    }

    /// Queue a direct message and step until it fails or `budget_ms` runs
    /// out. Returns the simulated time to failure, `None` on a hang.
    fn run_to_failure(&mut self, budget_ms: u64) -> Option<u64> {
        let recipient = self.announce_recipient();
        assert!(self.node.has_path(&recipient));
        let message = Message::create(
            recipient.into_bytes(),
            [0x11; 16],
            &identity(1),
            WALL_UNIX as f64,
            b"title".to_vec(),
            b"to a recipient that has left".to_vec(),
            Vec::new(),
            DeliveryMethod::Direct,
        )
        .expect("message");
        let id = message.message_id;
        let output = self.router.enqueue(&self.node, message).expect("enqueue");
        self.events.extend(output.events);
        self.absorb_core(output.core);
        let start = self.clock.get();
        while self.clock.get() - start < budget_ms {
            self.step(250);
            if self.failed(&id) {
                return Some(self.clock.get() - start);
            }
        }
        None
    }
}

/// The bound a departed recipient owes: one link per delivery attempt, each
/// spending the core's link-request retries at the establishment timeout of
/// a one-hop path (6 s per hop, two hop terms, plus jitter headroom), and
/// one retry wait between attempts.
fn failure_bound_ms() -> u64 {
    let link_tries = u64::from(leviculum_core::constants::LINK_REQUEST_MAX_RETRIES) + 1;
    let per_link = link_tries * 2 * 6_000 * 3 / 2;
    u64::from(MAX_DELIVERY_ATTEMPTS) * (per_link + DELIVERY_RETRY_WAIT_MS + PROCESSING_INTERVAL_MS)
}

#[test]
fn direct_delivery_to_a_departed_recipient_fails_when_paths_are_answered() {
    let mut harness = Harness::new(true);
    let elapsed = harness.run_to_failure(4 * failure_bound_ms());
    assert!(
        harness.links_closed <= usize::from(MAX_DELIVERY_ATTEMPTS),
        "{} links for {} attempts: a link started outside the attempt budget",
        harness.links_closed,
        MAX_DELIVERY_ATTEMPTS
    );
    let elapsed = elapsed.unwrap_or_else(|| {
        panic!(
            "no Failed after {} ms and {} closed links",
            4 * failure_bound_ms(),
            harness.links_closed
        )
    });
    assert!(
        elapsed <= failure_bound_ms(),
        "failed after {elapsed} ms, bound {} ms",
        failure_bound_ms()
    );
}

/// Control: the same harness with a daemon that stays silent ends, so the
/// harness itself does not hang a message.
#[test]
fn direct_delivery_to_a_departed_recipient_fails_when_paths_go_unanswered() {
    let mut harness = Harness::new(false);
    let elapsed = harness.run_to_failure(4 * failure_bound_ms());
    assert!(elapsed.is_some_and(|elapsed| elapsed <= failure_bound_ms()));
}
