//! mvr: two copies of ONE emission, relayed first and direct second
//! (Codeberg #231).
//!
//! ## The clause under test
//!
//! `transport.rs`'s equal-or-fewer-hops acceptance arm has a second
//! condition beside "the emission is newer": `announce_emitted ==
//! path_timebase && accept_same_emission(..)`, live as
//! [`SameEmissionRule::StrictlyNewerOnly`] since #231, i.e. Python's rule:
//! a second copy of an announce we already installed never replaces the
//! path. Until then it was [`SameEmissionRule::FewerHopsWins`], a deviation
//! that replaced the path whenever the copy took strictly fewer hops; #231
//! counted it at zero on the relayed pathchoice arms (loss30/50/70, 41 to
//! 43 PATH_ADD each) and retired it. Python has no such acceptance: the vendored
//! reference/Reticulum/RNS/Transport.py:1772 (1.3.5) demands an unseen
//! random blob AND `announce_emitted > path_timebase`, and 1.5.2 — the
//! version periculum's `rnsd` arm pins — adds only a `gravity` arm on equal
//! emission, which rejects whenever both interfaces carry the default
//! gravity of 0, i.e. on any default config. Hop count never appears in
//! either version's same-emission test. The 1.5.2 line numbers are in
//! docs/src/protocol-notes/announce-dedup-and-path-replacement.md §5, in
//! prose, because that tree is not vendored.
//!
//! ## Why this is the minimal test and not the soak
//!
//! The instrument #231 was expected to use — the `pathchoice_*` soak's
//! direct share — cannot see the clause. `path_soak` reads the hop count
//! out of the very `path_wait` invocation that resolved
//! (`periculum/src/executor.rs::execute_path_soak`), and `lnpath -w` polls
//! at 100 ms and reports as soon as the path appears
//! (`leviculum-cli/src/lnpath.rs::wait_and_report`). A second copy of one
//! emission is a whole frame behind the first (p332's loss50 arm measured
//! 150071 ms of airtime over 436 frames, ~344 ms per frame), so the swap
//! lands after the read: the share it reports is the hop count of the
//! FIRST install, in which the clause has by definition not fired yet.
//! What the clause does has to be asserted here, and counted in the logs
//! from `reason=`.
//!
//! ## What is pinned
//!
//! 1. Today's behaviour, Python's: relayed (hops 2, via bravo) first,
//!    direct (hops 1) second — the replay guard drops the direct copy, the
//!    entry stays at hops 2 via bravo, and no `PATH_ADD` is logged.
//! 2. The control: direct first, relayed second — the entry stays at
//!    hops 1 under every candidate rule, so a change to #231's decision
//!    cannot regress the ordinary case.
//! 3. The retired deviation, expressed but NOT wired in: the same two
//!    copies under [`SameEmissionRule::FewerHopsWins`] would swap to the
//!    1-hop route, which is what flipping the constant back would restore.
//! 4. Option (C) of #231: fewer hops only on a different interface — the
//!    one-radio room rejects, a genuine second route accepts.
//!
//! Sans-I/O: 1 node, 1-2 mock interfaces, deterministic, sub-second.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{HeaderType, Packet, TransportType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::{
    accept_same_emission, InterfaceId, SameEmissionCopy, SameEmissionRule, SAME_EMISSION_RULE,
};

type TransportNode = NodeCore<OsRng, MockClock, MemoryStorage>;

/// bravo's transport id: the relay that carries the 2-hop copy.
const BRAVO: [u8; TRUNCATED_HASHBYTES] = [0x42; TRUNCATED_HASHBYTES];

fn make_node() -> Box<TransportNode> {
    NodeCoreBuilder::new().enable_transport(true).build_boxed(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    )
}

fn add_iface(node: &mut TransportNode, name: &'static str, id: u8) -> usize {
    let idx = node
        .transport
        .register_interface(Box::new(MockInterface::new(name, id)));
    node.set_interface_name(idx, String::from(name));
    idx
}

/// charlie, the announced destination, and the ONE announce this test has
/// two copies of.
struct Charlie {
    dest_hash: crate::DestinationHash,
    raw: Vec<u8>,
}

fn make_charlie() -> Charlie {
    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["probe"],
    )
    .unwrap();
    let dest_hash = *dest.hash();
    let ann = dest
        .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000)
        .unwrap();
    let mut buf = [0u8; MTU];
    let len = ann.pack(&mut buf).unwrap();
    Charlie {
        dest_hash,
        raw: buf[..len].to_vec(),
    }
}

impl Charlie {
    /// The copy charlie's own radio put on the air: wire hops 0, no
    /// transport id. The transport increments on ingress, so it is
    /// processed at hops 1.
    fn direct(&self) -> Vec<u8> {
        self.raw.clone()
    }

    /// The same announce after bravo forwarded it: wire hops 1 and bravo's
    /// id, processed at hops 2. Only the header changes — the signed body,
    /// and with it the random blob and its emission timebase, is byte-for-
    /// byte the direct copy's (see
    /// `docs/src/protocol-notes/announce-dedup-and-path-replacement.md` §1:
    /// both copies even hash the same).
    fn relayed(&self) -> Vec<u8> {
        let mut p = Packet::unpack(&self.raw).unwrap();
        p.flags.header_type = HeaderType::Type2;
        p.flags.transport_type = TransportType::Transport;
        p.hops = 1;
        p.transport_id = Some(BRAVO);
        let mut buf = [0u8; MTU];
        let len = p.pack(&mut buf).unwrap();
        buf[..len].to_vec()
    }
}

/// Assertion 1 + 3: the relayed copy arrives first, the direct copy of the
/// SAME emission arrives second on the same radio. The live rule, Python's,
/// keeps the route; the retired deviation, expressed against the pure
/// function, would have swapped it.
#[test]
fn relayed_first_then_direct_keeps_the_relayed_route() {
    let mut node = make_node();
    let lora = add_iface(&mut node, "lora_sx1262", 0);
    let charlie = make_charlie();

    // bravo's copy wins the race: 2 hops, next hop bravo.
    let _ = node.handle_packet(InterfaceId(lora), &charlie.relayed());
    let installed = node
        .transport
        .get_path_clone(charlie.dest_hash.as_bytes())
        .expect("the relayed copy installs the first path");
    assert_eq!(installed.hops, 2, "one relay in between");
    assert_eq!(
        installed.next_hop,
        Some(BRAVO),
        "the 2-hop entry routes through bravo"
    );

    // charlie's own copy of the SAME announce arrives second.
    let replays_before = node.transport.stats().drops_announce_replay();
    let ((), logs) = crate::test_log_capture::with_captured_logs(|| {
        let _ = node.handle_packet(InterfaceId(lora), &charlie.direct());
    });
    let after = node
        .transport
        .get_path_clone(charlie.dest_hash.as_bytes())
        .expect("path still present");
    assert_eq!(
        after.hops, 2,
        "Python's rule (reference/Reticulum/RNS/Transport.py:1772, live as \
         SameEmissionRule::StrictlyNewerOnly) requires a strictly newer \
         emission and an unseen random blob; a second copy of one emission \
         is neither, however few hops it saved — Codeberg #231"
    );
    assert_eq!(
        after.next_hop,
        Some(BRAVO),
        "alpha keeps bravo's route until a fresh emission arrives"
    );
    assert_eq!(
        node.transport.stats().drops_announce_replay(),
        replays_before + 1,
        "a seen random blob is a replay whatever its hop count: the replay \
         guard drops the copy before the acceptance arm sees it"
    );
    assert!(
        !logs.contains("event=\"PATH_ADD\""),
        "nothing was installed, so nothing is logged as installed; \
         logs={logs:?}"
    );

    // Assertion 3 — the retired deviation on the very copies this run kept,
    // via the pure function, NOT wired in.
    let existing = SameEmissionCopy {
        hops: 2,
        interface_index: lora,
    };
    let incoming = SameEmissionCopy {
        hops: 1,
        interface_index: lora,
    };
    assert!(
        !accept_same_emission(SameEmissionRule::StrictlyNewerOnly, existing, incoming),
        "the live rule refuses a second copy of one emission"
    );
    assert!(
        accept_same_emission(SameEmissionRule::FewerHopsWins, existing, incoming),
        "the retired deviation took any strictly shorter copy: flipping \
         SAME_EMISSION_RULE back to it restores the swap to 1 hop"
    );
}

/// Assertion 2, the control: the ordinary order. The direct copy installs
/// first, and the relayed copy of the same emission must not drag the entry
/// back out to 2 hops — which is the worse-hops arm's job, and is true
/// under all three candidate rules.
#[test]
fn direct_first_then_relayed_keeps_the_one_hop_route() {
    let mut node = make_node();
    let lora = add_iface(&mut node, "lora_sx1262", 0);
    let charlie = make_charlie();

    let _ = node.handle_packet(InterfaceId(lora), &charlie.direct());
    let installed = node
        .transport
        .get_path_clone(charlie.dest_hash.as_bytes())
        .expect("the direct copy installs the first path");
    assert_eq!(installed.hops, 1, "heard straight from charlie");
    assert_eq!(installed.next_hop, None, "no relay in between");

    let _ = node.handle_packet(InterfaceId(lora), &charlie.relayed());
    let after = node
        .transport
        .get_path_clone(charlie.dest_hash.as_bytes())
        .expect("path still present");
    assert_eq!(
        after.hops, 1,
        "a worse-hops copy of an emission already installed is ignored \
         unless the path expired or went unresponsive \
         (Transport.py:1793-1826)"
    );
    assert_eq!(after.next_hop, None, "still the direct route");

    // The control holds for every rule #231 could pick, because none of
    // them is even consulted: this is the worse-hops arm, not the
    // equal-or-fewer-hops one.
    for rule in [
        SameEmissionRule::FewerHopsWins,
        SameEmissionRule::StrictlyNewerOnly,
        SameEmissionRule::FewerHopsOnOtherInterface,
    ] {
        assert!(
            !accept_same_emission(
                rule,
                SameEmissionCopy {
                    hops: 1,
                    interface_index: lora,
                },
                SameEmissionCopy {
                    hops: 2,
                    interface_index: lora,
                },
            ),
            "no candidate rule lets a MORE-hops copy of the same emission \
             through: {rule:?}"
        );
    }
}

/// Assertion 4: option (C) of #231 — the clause restricted to a genuine
/// second route. One radio hearing both copies is a retry of one route and
/// is rejected; the same hop counts across two interfaces are two routes
/// and are accepted.
#[test]
fn option_c_accepts_only_across_interfaces() {
    let mut node = make_node();
    let lora = add_iface(&mut node, "lora_sx1262", 0);
    let ble = add_iface(&mut node, "ble_nrf", 1);
    assert_ne!(lora, ble, "two distinct interfaces");

    let same_radio = accept_same_emission(
        SameEmissionRule::FewerHopsOnOtherInterface,
        SameEmissionCopy {
            hops: 2,
            interface_index: lora,
        },
        SameEmissionCopy {
            hops: 1,
            interface_index: lora,
        },
    );
    assert!(
        !same_radio,
        "both copies on one radio: option (C) reads that as the same route \
         heard twice, not a shorter second one"
    );

    let other_radio = accept_same_emission(
        SameEmissionRule::FewerHopsOnOtherInterface,
        SameEmissionCopy {
            hops: 2,
            interface_index: lora,
        },
        SameEmissionCopy {
            hops: 1,
            interface_index: ble,
        },
    );
    assert!(
        other_radio,
        "a shorter copy on a second interface is a real alternative route, \
         which is the case the deviation was presumably written for"
    );
}

/// The live rule is Python's (#231). Flipping [`SAME_EMISSION_RULE`] back
/// to a hop-counting rule is a deviation and needs a measurement that it
/// improves delivery, e.g. a BLE-plus-LoRa board hearing a phone over both.
#[test]
fn the_live_rule_is_pythons() {
    assert_eq!(
        SAME_EMISSION_RULE,
        SameEmissionRule::StrictlyNewerOnly,
        "a pass that flips the live rule must land the measurement with it: \
         #231 counted FewerHopsWins at 0 on the relayed pathchoice arms"
    );
}
