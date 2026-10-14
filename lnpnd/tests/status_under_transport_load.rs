//! `lnpnd --status` against a *loaded* shared instance (Codeberg #427).
//!
//! `status_default_budget.rs` reproduces the 2026-09-20 field shape — a
//! fresh client, a daemon that announced before the client existed — and
//! answers at the shipped budget on an **idle** instance. The 2026-09-25
//! field (miauhaus) fails 7 of 8 times at 5, 20 and 60 s alike on an
//! instance that differs in exactly one variable: load. Its transport
//! node holds ~24 700 paths, announces keep arriving, and lnpnd's engine
//! logs `CORE_PROCESSOR_OVER_BUDGET hook=on_tick elapsed_us=56000..152000`.
//!
//! This test adds that variable and it reproduced the field red on first
//! measurement (2026-09-25, `STATUS_UNDER_LOAD code=200 elapsed_ms=5066`),
//! which root-caused two defects on one causal chain, both divergences
//! from the reference:
//!
//! 1. A local client's path request for a destination absent from the
//!    instance's path table was forwarded to network interfaces only —
//!    never to the other local clients, one of whom (the lnpnd daemon)
//!    hosts the destination. The reference forwards it on every other
//!    interface (`elif is_from_local_client`, Transport.py:3006-3013).
//!    Fixed in `transport.rs::handle_path_request` case 4, pinned by
//!    `test_path_request_from_local_client_forwarded_to_other_local_clients`.
//! 2. The client's own uplink interface ran the announce ingress burst
//!    limiter (the reference's `LocalClientInterface.should_ingress_limit`
//!    returns False, LocalInterface.py:137-138), so a client born into a
//!    busy instance HELD the very path-response announce it had asked
//!    for. Fixed in `interfaces/local.rs::spawn_local_client`.
//!
//! The load here: the instance's path table driven past its ceiling
//! (6c9735fe2 gave every transport table one; the ceiling is set low
//! enough to be reachable in a test's budget), which evicts the control
//! destination's entry, then a sustained announce storm — the field's
//! "a few thousand PATH_ADDs per minute" — while the *shipped* client
//! flow runs at the *shipped* budget: `lnpnd::client::run`, fresh storage
//! per invocation as `main.rs` mints it, `DEFAULT_STATUS_TIMEOUT`. The
//! fill backlog is drained before the query so the storm, not a
//! test-artifact burst queue, is what the query runs against.
//!
//! Without either fix this was red at every budget: the request
//! black-holes (1), or its answer is held past any budget for as long as
//! the storm keeps the burst limiter armed (2). Fix 2 now pins this test
//! only on a starved host. Measured 2026-10-14 with the client uplink's
//! `ingress_control` back at `None`: the querying client's limiter arms,
//! but the answer passes as an announce for a destination the client has
//! a path request out for (`Transport::own_path_request_pending`, the
//! reference's Transport.py:1701 exemption), and under CPUQuota=25% the
//! query took 0.41 s. What the revert does break is the daemon, itself a
//! local client: it holds the fill announces (2 paths learned of 5097),
//! the drain below gives up on its stall rule, and under CPUQuota=10% the
//! query then waits behind the daemon's backlog and times out at 5 s.

use std::time::Duration;

use leviculum_core::{Destination, DestinationHash, DestinationType, Direction, Identity};
use leviculum_lxmf::control::CONTROL_ASPECTS;
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::{MemoryPeerStore, MemoryPropagationStore, PropagationNodeConfig};
use leviculum_std::config::Config;
use leviculum_std::driver::ReticulumNodeBuilder;
use lnpnd::client::{ClientAction, ClientOptions, DEFAULT_STATUS_TIMEOUT};
use lnpnd::engine::{Engine, EngineConfig, EngineEvent};

type EngineEvents = std::sync::mpsc::Receiver<EngineEvent>;

/// The instance's path-table ceiling for this test. Low enough that the
/// feeder can fill it inside the test budget, high enough that the tick's
/// per-entry work resembles a real table rather than a toy.
const PATH_TABLE_CAP: usize = 4096;

/// Distinct destinations the feeder announces before the query: past the
/// ceiling, so the table is full, evicting, and the daemon's control
/// entry is gone when the client arrives.
const FILL_ANNOUNCES: usize = PATH_TABLE_CAP + 1000;

/// Fresh destinations announced per second while the query runs — the
/// field's "a few thousand PATH_ADD per minute", sustained. A ceiling: the
/// storm also waits for the daemon (see where it runs).
const STORM_PER_SEC: u64 = 50;

/// How long the daemon may take to work off the fill backlog. A liveness
/// bound, not a budget: the wait ends when the drain sentinel arrives. The
/// daemon learned about 24 announces a second under CPUQuota=10% in a debug
/// build (2026-10-14), so a 5000-announce backlog on a starved runner is
/// minutes; nothing here measures that time.
const DRAIN_LIVENESS: Duration = Duration::from_secs(600);

/// The drain stops waiting for the sentinel when the daemon learns fewer
/// than `DRAIN_STALL_MIN_PATHS` in a `DRAIN_STALL_WINDOW`. A starved daemon
/// still learned about 24 a second under CPUQuota=10%, with gaps of at most
/// 1.1 s; a daemon whose uplink holds announces learns one per held-release
/// interval (5 s), which is not working the backlog off but sitting on it.
const DRAIN_STALL_WINDOW: Duration = Duration::from_secs(10);
const DRAIN_STALL_MIN_PATHS: usize = 10;

/// The longest the storm waits for the daemon to learn one announce before
/// it sends the next: with the 20 ms clock pace, the storm stays above
/// about 8 announces a second whatever the daemon does.
const STORM_PACE_WAIT: Duration = Duration::from_millis(100);

/// lnpnd's production engine on a stock configuration, as in
/// `status_default_budget.rs`.
fn production_engine(identity: Identity) -> (Engine<MemoryPropagationStore>, EngineEvents) {
    Engine::new(EngineConfig {
        identity,
        node_config: PropagationNodeConfig::default(),
        store: MemoryPropagationStore::new(64_000),
        announce_interval_secs: 3600,
        announce_delay_secs: 0,
        announce_at_start: true,
        peering: leviculum_lxmf::PeeringConfig::default(),
        peer_store: Box::new(MemoryPeerStore::default()),
        control_allowed: Vec::new(),
        auth_allowed: None,
        mailbox: None,
        store_limit_bytes: 64_000,
        delivery_limit_kb: 1000,
    })
}

/// One announceable destination with its own identity, registered on the
/// feeder. The aspect varies so the destination hashes differ too.
fn feeder_destination(node: &leviculum_std::ReticulumNode, index: usize) -> DestinationHash {
    let identity = Identity::generate(&mut rand_core::OsRng);
    let aspect = format!("load{index}");
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "pathload",
        &[&aspect],
    )
    .expect("destination");
    let hash = *dest.hash();
    node.register_destination(dest);
    hash
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_at_the_shipped_budget_on_a_loaded_instance() {
    let instance = format!("lnpnd_load_{}", std::process::id());

    // The instance: transport on, path table capped where the test can
    // reach it (`path_table_cap` is the same knob a config file sets).
    let mut config = Config::default();
    config.reticulum.path_table_cap = Some(PATH_TABLE_CAP);
    let instance_storage = tempfile::tempdir().expect("tempdir instance");
    let mut shared = ReticulumNodeBuilder::new()
        .identity(leviculum_std::generate_identity())
        .config(config)
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance.clone())
        .storage_path(instance_storage.path().to_path_buf())
        .build()
        .await
        .expect("build the shared instance");
    shared.start().await.expect("start the shared instance");

    // lnpnd, attached exactly as `main.rs` attaches it.
    let node_identity = leviculum_std::generate_identity();
    let (engine, _events) = production_engine(node_identity.clone());
    let daemon_storage = tempfile::tempdir().expect("tempdir daemon");
    let mut daemon = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&instance)
        .storage_path(daemon_storage.path().to_path_buf())
        .core_processor(engine)
        .build()
        .await
        .expect("build lnpnd");
    daemon.start().await.expect("start lnpnd");

    let name_hash = Destination::compute_name_hash(APP_NAME, &CONTROL_ASPECTS);
    let control_hash: DestinationHash =
        Destination::compute_destination_hash(&name_hash, node_identity.hash());

    // The daemon's announce reaches the instance before any load exists,
    // as in the field: the node had been announcing normally all along.
    let announced = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if shared.has_path(&control_hash) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(
        announced.is_ok(),
        "the daemon never announced its control destination — the test \
         would then measure a different absence than #427's"
    );

    // The feeder: a third local client that fills the instance's path
    // table past its ceiling.
    let feeder_storage = tempfile::tempdir().expect("tempdir feeder");
    let mut feeder = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&instance)
        .storage_path(feeder_storage.path().to_path_buf())
        .build()
        .await
        .expect("build the feeder");
    feeder.start().await.expect("start the feeder");

    let fill_started = std::time::Instant::now();
    for index in 0..FILL_ANNOUNCES {
        let hash = feeder_destination(&feeder, index);
        feeder
            .announce_destination(&hash, None)
            .await
            .expect("announce");
    }
    // The announces are queued through channels on both sides; the table
    // is loaded when the instance's count says so, not when the loop ends.
    //
    // BOTH conditions, because the count reaching the cap is not the one
    // this test needs. `path_count()` hits 4096 on the 4096th insertion —
    // the first eviction has not happened yet — and the control entry is
    // evicted somewhere in the ~1000 insertions after that. A poll landing
    // in that window sees a full table with the entry still in it, and the
    // assertion below then fails a test whose fill was working. Measured
    // 2026-09-26 with a 20 ms probe: count crosses the cap and the entry
    // goes in the same window (cap at t=1659 ms, `has_control=false` at
    // 1659 ms, from `has_control=true` at 1638 ms with 4078 paths), and the
    // 200 ms poll that used to stand here landed inside it once in a
    // ci-gate run (`PATH_FILL announced=5096 table=4096 cap=4096
    // elapsed_ms=1581`, the control entry present). Waiting for the
    // eviction itself is waiting for the precondition instead of for a
    // proxy that precedes it.
    let filled = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if shared.path_count() >= PATH_TABLE_CAP && !shared.has_path(&control_hash) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    eprintln!(
        "PATH_FILL announced={FILL_ANNOUNCES} table={} cap={PATH_TABLE_CAP} elapsed_ms={}",
        shared.path_count(),
        fill_started.elapsed().as_millis()
    );
    assert!(
        filled.is_ok(),
        "60 s after {FILL_ANNOUNCES} announces the instance holds {} of \
         {PATH_TABLE_CAP} paths and the control entry is {} — the load \
         this test exists to apply was not applied",
        shared.path_count(),
        if shared.has_path(&control_hash) {
            "still there"
        } else {
            "gone"
        }
    );
    // Restated as an assertion although the wait above already holds it:
    // it is the one line that says WHY the eviction matters, and an edit
    // that loosens the wait condition should fail here rather than quietly
    // measure a cached answer.
    assert!(
        !shared.has_path(&control_hash),
        "the fill was meant to evict the control destination's path entry \
         from the instance's capped table; with it still present the query \
         is answered from the cache and the recovery path is not exercised"
    );

    // Drain the fill burst before the query: the instance fans every
    // announce out to its local clients, and the daemon verifies each, so
    // a query launched into that backlog measures the test's own burst
    // instead of the sustained load. Field announce arrival is a rate,
    // not a one-shot 5000-announce burst.
    //
    // The backlog is drained when the daemon has learned a sentinel
    // announced after the last fill announce: the path from the feeder
    // through the instance to the daemon is one FIFO, so the sentinel is
    // processed after everything ahead of it. This used to wait for the
    // daemon's path count to stand still for 500 ms, a proxy that a starved
    // host satisfies early. Measured 2026-10-14 under CPUQuota=10%: the
    // daemon's progress came in bursts with a 1.1 s gap, the plateau fired
    // with 1507 to 2073 of the 5096 fill paths learned (three runs, all
    // red), and the client's path request then sat behind the other 3000 or
    // more in the daemon's inbound queue for the whole 5 s budget: the
    // instance never relearned the control path, which is the forge red of
    // pipeline 518 line for line. At 25 % the same proxy fired at 5096 and
    // the query took 0.6 to 0.7 s. With the sentinel the query found no
    // backlog and took 0.7 to 1.3 s under 10 %, 0.4 to 0.6 s under 25 %.
    let sentinel = feeder_destination(&feeder, FILL_ANNOUNCES);
    feeder
        .announce_destination(&sentinel, None)
        .await
        .expect("announce the drain sentinel");
    let drain_started = std::time::Instant::now();
    let drained = tokio::time::timeout(DRAIN_LIVENESS, async {
        let mut checkpoint = (daemon.path_count(), std::time::Instant::now());
        while !daemon.has_path(&sentinel) {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if checkpoint.1.elapsed() >= DRAIN_STALL_WINDOW {
                let now = daemon.path_count();
                if now < checkpoint.0 + DRAIN_STALL_MIN_PATHS {
                    return false;
                }
                checkpoint = (now, std::time::Instant::now());
            }
        }
        true
    })
    .await;
    eprintln!(
        "PATH_DRAIN daemon_paths={} elapsed_ms={} sentinel={}",
        daemon.path_count(),
        drain_started.elapsed().as_millis(),
        daemon.has_path(&sentinel)
    );
    // A daemon that has all but stopped learning is not slow, it is holding
    // the announces (an armed burst limiter on its own uplink does that),
    // and the sentinel would only arrive with the liveness bound. Such a
    // drain goes on to the query, which is the assertion that names the
    // outcome. Only a daemon still learning after `DRAIN_LIVENESS` fails
    // here.
    assert!(
        drained.is_ok(),
        "the daemon was still working off the fill backlog after \
         {DRAIN_LIVENESS:?} ({} paths learned)",
        daemon.path_count()
    );

    // Sustained churn while the query runs: fresh destinations at up to
    // `STORM_PER_SEC`, each one a PATH_ADD plus an eviction at the cap —
    // and, fanned out to the clients, exactly the announce pressure that
    // used to keep the client's burst limiter armed (defect 2 above).
    //
    // Paced by the daemon's own progress as well as by the clock: the next
    // announce leaves once the daemon has learned the previous one, or
    // after `STORM_PACE_WAIT`. On a host that verifies 50 announces a
    // second with CPU to spare that is the full rate; on a starved one the
    // storm slows with the daemon instead of building a queue in front of
    // the client's request that grows for as long as the query runs, which
    // would measure the host's CPU and not the budget. Under CPUQuota=10%
    // that queue held the answer for 2.7 s with the clock alone and 1.0 to
    // 1.5 s with this pace. The wait is capped so a daemon that stops learning
    // cannot stop the storm: it still runs well above the new-interface
    // burst threshold of 3 a second.
    let storm_sent = std::sync::atomic::AtomicUsize::new(0);
    let storm = async {
        let mut pace = tokio::time::interval(Duration::from_millis(1000 / STORM_PER_SEC));
        pace.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut index = FILL_ANNOUNCES + 1;
        loop {
            pace.tick().await;
            let hash = feeder_destination(&feeder, index);
            index += 1;
            if feeder.announce_destination(&hash, None).await.is_err() {
                return;
            }
            storm_sent.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let learned_by = tokio::time::Instant::now() + STORM_PACE_WAIT;
            while !daemon.has_path(&hash) && tokio::time::Instant::now() < learned_by {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    };

    // The shipped client at the shipped budget: `client::run` is the code
    // `main.rs` runs, fresh per-invocation storage and all. The storm runs
    // beside it on this task and ends when the query does.
    let client_storage = tempfile::tempdir().expect("tempdir client");
    let query_started = std::time::Instant::now();
    let query = lnpnd::client::run(
        ClientOptions {
            instance: instance.clone(),
            storage_dir: client_storage.path().join("state"),
            identity: node_identity,
            remote: None,
            timeout: DEFAULT_STATUS_TIMEOUT,
        },
        ClientAction::Status {
            show_status: true,
            show_peers: false,
        },
    );
    let code = tokio::select! {
        code = query => code,
        () = storm => panic!("the storm ended before the query: the feeder could not announce"),
    };
    let query_elapsed = query_started.elapsed();

    eprintln!(
        "STATUS_UNDER_LOAD code={code} elapsed_ms={} table={} cap={PATH_TABLE_CAP} storm_sent={}",
        query_elapsed.as_millis(),
        shared.path_count(),
        storm_sent.load(std::sync::atomic::Ordering::Relaxed)
    );

    assert_eq!(
        code, 0,
        "the shipped --status budget of {DEFAULT_STATUS_TIMEOUT:?} did not \
         survive a path table at its ceiling with announces still arriving \
         — the Codeberg #427 field shape (elapsed {query_elapsed:?})"
    );

    let _ = daemon.stop().await;
    let _ = shared.stop().await;
}
