//! Order 1009: where the hooks spend their time under the miauhaus load.
//!
//! A measurement, not a gate: it reports a distribution and asserts only
//! that the load it claims to apply was applied. Run it by hand, in a
//! release build, against a directory on a real disk (the store and the
//! peer store `fsync`, and a tmpfs prices that at nothing):
//!
//! ```text
//! LNPND_LOAD_DIR=/var/tmp cargo test --release -p lnpnd hook_load -- --ignored --nocapture
//! ```
//!
//! The load is the field's of 2026-10-25 (lnpnd 419bdb8 on miauhaus):
//! tens of thousands of known destinations, a peer table at its limit with
//! further propagation nodes declined, announces arriving, and an inbound
//! peer sync of 100 messages stored a slice per hook while the announces
//! keep coming. Every hook runs on the production store types,
//! [`FilePropagationStore`] and [`FilePeerStore`].
//!
//! Knobs, all optional: `LNPND_LOAD_DESTINATIONS` (40000),
//! `LNPND_LOAD_SYNC` (messages in the inbound sync, 100),
//! `LNPND_LOAD_STORED` (messages already in the store, 0),
//! `LNPND_LOAD_FULL` (1: the store limit leaves no room, so every append of
//! the sync evicts; 0), `LNPND_LOAD_DIR` (the target directory),
//! `LNPND_LOAD_WRITE_BEHIND` (0: the message store appends inline, as it did
//! before order 1081; 1). The peer store writes off the hook either way.

use std::collections::BTreeMap;
use std::time::Instant;

use leviculum_core::node::{NodeCoreBuilder, NodeEvent};
use leviculum_core::{Destination, DestinationType, Direction, InterfaceId, LinkId};
use leviculum_lxmf::constants::{LXMF_OVERHEAD, STAMP_SIZE};
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::propagation_client::PROPAGATION_ASPECT;
use leviculum_lxmf::{PeerSyncEnvelope, PeeringConfig, PropagationNodeAnnounce};
use leviculum_std::driver::{
    CoreProcessor as _, StdClock, StdNodeCore, StdStorage, PROCESSOR_TICK_BUDGET,
};
use leviculum_std::{FilePeerStore, FilePropagationStore};

use super::tests::cost_free;
use super::{Engine, EngineConfig, State};
use crate::hook_profile::{HookTimes, Section};

const SECTIONS: [(Section, &str); 10] = [
    (Section::Ready, "ready"),
    (Section::Drain, "drain"),
    (Section::Sync, "sync"),
    (Section::Mailbox, "mailbox"),
    (Section::PeerTable, "peer_table"),
    (Section::Event, "event"),
    (Section::Announce, "announce"),
    (Section::Tick, "tick"),
    (Section::Links, "links"),
    (Section::Peering, "peering"),
];

fn knob(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// A remote node that announces, and announces again.
struct Announcer {
    destination: Destination,
    app_data: Option<Vec<u8>>,
    /// The emission time of the last announce: the transport drops an
    /// announce whose emission is not newer than the one it holds, so a
    /// re-announce inside the same second is stepped one second on.
    emitted: u64,
}

impl Announcer {
    fn plain(index: usize) -> Self {
        let aspect = format!("load{index}");
        Self {
            destination: Destination::new(
                Some(leviculum_std::generate_identity()),
                Direction::In,
                DestinationType::Single,
                "hookload",
                &[&aspect],
            )
            .expect("a plain destination"),
            app_data: None,
            emitted: 0,
        }
    }

    fn propagation(timebase: u64) -> Self {
        let app_data = PropagationNodeAnnounce {
            legacy_support: false,
            timebase,
            enabled: true,
            transfer_limit_kb: 256,
            sync_limit_kb: 1024,
            stamp_cost: 16,
            stamp_cost_flexibility: 3,
            peering_cost: 0,
            metadata: Vec::new(),
        }
        .encode()
        .expect("propagation app data");
        Self {
            destination: Destination::new(
                Some(leviculum_std::generate_identity()),
                Direction::In,
                DestinationType::Single,
                APP_NAME,
                &[PROPAGATION_ASPECT],
            )
            .expect("a propagation destination"),
            app_data: Some(app_data),
            emitted: 0,
        }
    }

    fn hash(&self) -> [u8; 16] {
        *self.destination.hash().as_bytes()
    }

    fn wire(&mut self, now_ms: u64, now_secs: u64) -> Vec<u8> {
        self.emitted = now_secs.max(self.emitted + 1);
        let packet = self
            .destination
            .announce(
                self.app_data.as_deref(),
                &mut rand_core::OsRng,
                now_ms,
                self.emitted,
            )
            .expect("announce packet");
        let mut buf = vec![0u8; 600];
        let len = packet.pack(&mut buf).expect("pack the announce");
        buf.truncate(len);
        buf
    }
}

/// Every hook invocation, and every dispatch as the driver times it.
#[derive(Default)]
struct Ledger {
    /// (hook, arm or phase) → the invocations.
    calls: BTreeMap<(&'static str, String), Vec<HookTimes>>,
    /// phase → total of all `on_event` calls one dispatch made, the number
    /// `CORE_PROCESSOR_OVER_BUDGET hook=on_event` reports.
    dispatches: BTreeMap<&'static str, Vec<(u64, usize)>>,
}

impl Ledger {
    fn record(&mut self, hook: &'static str, group: String, times: HookTimes) {
        self.calls.entry((hook, group)).or_default().push(times);
    }

    fn print(&self) {
        let budget = PROCESSOR_TICK_BUDGET.as_micros() as u64;
        eprintln!("HOOK_LOAD per invocation (us; p50/p95/max; over = total past {budget} us)");
        for ((hook, group), calls) in &self.calls {
            let over = calls.iter().filter(|c| c.total_us > budget).count();
            let totals: Vec<u64> = calls.iter().map(|c| c.total_us).collect();
            let mut line = format!(
                "  {hook:<8} {group:<34} n={:<6} over={over:<5} total={}",
                calls.len(),
                quantiles(totals)
            );
            for (section, name) in SECTIONS {
                let values: Vec<u64> = calls.iter().map(|c| c.of(section)).collect();
                if values.iter().any(|v| *v > 0) {
                    line.push_str(&format!(" {name}={}", quantiles(values)));
                }
            }
            let appends: usize = calls.iter().map(|c| c.appends).sum();
            line.push_str(&format!(" appends={appends}"));
            eprintln!("{line}");
        }
        eprintln!("HOOK_LOAD per driver dispatch of on_event (sum over the dispatch's events)");
        for (phase, dispatches) in &self.dispatches {
            let over = dispatches.iter().filter(|(t, _)| *t > budget).count();
            let events: Vec<u64> = dispatches.iter().map(|(_, e)| *e as u64).collect();
            eprintln!(
                "  {phase:<43} n={:<6} over={over:<5} total={} events={}",
                dispatches.len(),
                quantiles(dispatches.iter().map(|(t, _)| *t).collect()),
                quantiles(events)
            );
        }
    }
}

fn quantiles(mut values: Vec<u64>) -> String {
    values.sort_unstable();
    let at = |q: f64| values[((values.len() - 1) as f64 * q).round() as usize];
    format!("{}/{}/{}", at(0.5), at(0.95), at(1.0))
}

/// One packet through the core and its events through the engine, as the
/// driver's tap hands them over: one dispatch, one budget.
fn deliver(
    engine: &mut Engine<FilePropagationStore>,
    core: &mut StdNodeCore,
    ledger: &mut Ledger,
    phase: &'static str,
    wire: &[u8],
) {
    let output = core.handle_packet(InterfaceId(0), wire);
    dispatch(engine, core, ledger, phase, &output.events);
}

fn dispatch(
    engine: &mut Engine<FilePropagationStore>,
    core: &mut StdNodeCore,
    ledger: &mut Ledger,
    phase: &'static str,
    events: &[NodeEvent],
) {
    if events.is_empty() {
        return;
    }
    let started = Instant::now();
    for event in events {
        let _ = engine.on_event(core, event);
        ledger.record(
            "on_event",
            format!("{phase}:{}", event.variant_name()),
            engine.last_profile,
        );
    }
    ledger
        .dispatches
        .entry(phase)
        .or_default()
        .push((started.elapsed().as_micros() as u64, events.len()));
}

fn tick(
    engine: &mut Engine<FilePropagationStore>,
    core: &mut StdNodeCore,
    ledger: &mut Ledger,
    phase: &'static str,
    now_ms: u64,
) {
    let _ = engine.on_tick(core, now_ms);
    ledger.record("on_tick", phase.to_string(), engine.last_profile);
}

fn stamped(seed: u32) -> Vec<u8> {
    let mut message = vec![0x11u8; LXMF_OVERHEAD + 240];
    message[16..20].copy_from_slice(&seed.to_be_bytes());
    message.extend_from_slice(&[0x5b; STAMP_SIZE]);
    message
}

#[test]
#[ignore = "a measurement (order 1009): run by hand in release, see the module docs"]
fn hook_profile_under_field_load() {
    let destinations = knob("LNPND_LOAD_DESTINATIONS", 40_000);
    let sync_messages = knob("LNPND_LOAD_SYNC", 100);
    let prefilled = knob("LNPND_LOAD_STORED", 0);
    let full = knob("LNPND_LOAD_FULL", 0) == 1;
    let base = std::env::var("LNPND_LOAD_DIR")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../target").to_string());
    let dir = tempfile::tempdir_in(&base).expect("a directory on the measured disk");
    eprintln!(
        "HOOK_LOAD dir={} destinations={destinations} sync={sync_messages} \
         stored={prefilled} full={full}",
        dir.path().display()
    );

    // The store as a node that has run a while holds it: written through
    // the store itself, before the engine exists, so none of it is timed.
    let body_len = stamped(0).len() as u64;
    let limit = if full {
        prefilled as u64 * body_len
    } else {
        500_000_000
    };
    let mut store =
        FilePropagationStore::open(dir.path().join("store"), limit).expect("message store");
    let write_behind = knob("LNPND_LOAD_WRITE_BEHIND", 1) == 1;
    let received_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    for index in 0..prefilled {
        use leviculum_lxmf::PropagationStore as _;
        let mut transient_id = [0u8; 32];
        transient_id[..8].copy_from_slice(&(index as u64).to_be_bytes());
        transient_id[8] = 0xee;
        store
            .append(
                &transient_id,
                received_at - (index as u64 % 86_400),
                0,
                &stamped(1_000_000 + index as u32),
            )
            .expect("prefill the store");
    }
    // As the daemon runs it (order 1081), unless the knob asks for the
    // inline store this measured before.
    if write_behind {
        store = store.write_behind().expect("the store's writer");
    }

    let mut core = NodeCoreBuilder::new().enable_transport(false).build(
        rand_core::OsRng,
        StdClock::new(),
        StdStorage::new(dir.path().join("core")).expect("core storage"),
    );
    // The daemon's uplink to its shared instance runs no ingress burst
    // limiter (`spawn_local_client`, leviculum-std), and neither does this.
    core.set_interface_ingress_control(0, false);
    let peering = PeeringConfig::default();
    let max_peers = peering.max_peers;
    let (engine, _events) = Engine::new(EngineConfig {
        identity: leviculum_std::generate_identity(),
        node_config: cost_free(),
        store,
        announce_interval_secs: 3600,
        announce_delay_secs: 0,
        announce_at_start: true,
        peering,
        peer_store: Box::new(FilePeerStore::open(dir.path().join("peers")).expect("peer store")),
        control_allowed: Vec::new(),
        auth_allowed: None,
        mailbox: None,
        store_limit_bytes: limit,
        delivery_limit_kb: 1000,
    });
    let mut engine = engine;
    let mut ledger = Ledger::default();

    let mut now_ms = core.now_ms();
    tick(&mut engine, &mut core, &mut ledger, "start", now_ms);

    // 1. The known destinations, each announce delivered as the driver
    // delivers it. Generated first so the clock below times only the node.
    let mut plain: Vec<Announcer> = (0..destinations).map(Announcer::plain).collect();
    let fill_started = Instant::now();
    for (index, announcer) in plain.iter_mut().enumerate() {
        let wire = announcer.wire(core.now_ms(), core.emission_secs());
        deliver(&mut engine, &mut core, &mut ledger, "fill", &wire);
        if index % 200 == 0 {
            now_ms = core.now_ms();
            tick(&mut engine, &mut core, &mut ledger, "fill", now_ms);
        }
    }
    let announced = ledger
        .calls
        .get(&("on_event", "fill:AnnounceReceived".to_string()))
        .map_or(0, Vec::len);
    eprintln!(
        "HOOK_LOAD filled {destinations} in {} ms: {announced} announces reached the engine, \
         path table {}",
        fill_started.elapsed().as_millis(),
        core.path_count()
    );
    assert_eq!(announced, destinations, "every fill announce must arrive");

    // 2. The peer table to its limit, then candidates it declines.
    let mut nodes: Vec<Announcer> = (0..max_peers * 3)
        .map(|_| Announcer::propagation(core.emission_secs()))
        .collect();
    for node in &mut nodes {
        let wire = node.wire(core.now_ms(), core.emission_secs());
        deliver(&mut engine, &mut core, &mut ledger, "peer_fill", &wire);
    }
    let peers = match &engine.state {
        State::Ready(ready) => ready.peering.peer_count(),
        _ => panic!("engine must be ready"),
    };
    assert_eq!(peers, max_peers, "the peer table must be at its limit");

    // 3. The inbound sync of a peer, stored a slice per hook while
    // announces keep arriving: per tick, one plain announce, one peer's
    // refresh and one declined candidate.
    let link_id = LinkId::new([0x42; 16]);
    let peer = nodes[0].hash();
    let mut ready = match std::mem::replace(&mut engine.state, State::Failed) {
        State::Ready(ready) => ready,
        _ => panic!("engine must be ready"),
    };
    ready.peering.validate_link_for_tests(link_id, peer);
    let envelope = PeerSyncEnvelope {
        timestamp: 1.0,
        messages: (0..sync_messages as u32).map(stamped).collect(),
    }
    .encode();
    engine.on_inbound_resource_completed(&mut ready, &core, &link_id, &envelope);
    engine.state = State::Ready(ready);

    let mut round = 0usize;
    let writes_outstanding = |engine: &Engine<FilePropagationStore>| match &engine.state {
        State::Ready(ready) => ready.node.store().writes_outstanding(),
        _ => false,
    };
    while !engine.pending.is_empty()
        || engine.validation.busy()
        || writes_outstanding(&engine)
        || round < 50
    {
        assert!(round < sync_messages * 4 + 100, "the sync did not drain");
        now_ms = core.now_ms();
        tick(&mut engine, &mut core, &mut ledger, "sync", now_ms);
        let fresh = plain.len();
        let mut announcer = Announcer::plain(fresh);
        let wire = announcer.wire(core.now_ms(), core.emission_secs());
        deliver(&mut engine, &mut core, &mut ledger, "sync", &wire);
        plain.push(announcer);
        let peer_index = round % max_peers;
        let wire = nodes[peer_index].wire(core.now_ms(), core.emission_secs());
        deliver(&mut engine, &mut core, &mut ledger, "sync", &wire);
        let declined = max_peers + round % (max_peers * 2);
        let wire = nodes[declined].wire(core.now_ms(), core.emission_secs());
        deliver(&mut engine, &mut core, &mut ledger, "sync", &wire);
        round += 1;
    }
    let stored = match &engine.state {
        State::Ready(ready) => {
            use leviculum_lxmf::PropagationStore as _;
            ready.node.store().count().unwrap_or(0)
        }
        _ => 0,
    };
    if full {
        assert_eq!(stored, prefilled, "a full store evicts one for one");
    } else {
        assert_eq!(
            stored,
            prefilled + sync_messages,
            "the inbound sync must be stored"
        );
    }

    // 4. Quiet ticks over the loaded node, maintenance included.
    engine.next_maintenance_at = 0;
    for _ in 0..50 {
        now_ms = core.now_ms();
        tick(&mut engine, &mut core, &mut ledger, "quiet", now_ms);
    }

    // 5. The passes that run on a cadence of minutes, each forced: a
    // rotation pass (every 22 min 24 s in the field), and the start of an
    // outbound round to a peer whose cursor is at zero, which walks the
    // whole store for its offer.
    for _ in 0..10 {
        if let State::Ready(ready) = &mut engine.state {
            ready.peering.rotate_on_next_tick_for_tests();
        }
        now_ms = core.now_ms();
        tick(&mut engine, &mut core, &mut ledger, "rotation", now_ms);
    }
    if let State::Ready(ready) = &mut engine.state {
        assert!(
            ready.peering.reoffer(&nodes[1].hash()),
            "a peer to re-offer to"
        );
    }
    now_ms = core.now_ms();
    tick(&mut engine, &mut core, &mut ledger, "round_start", now_ms);

    ledger.print();
}
