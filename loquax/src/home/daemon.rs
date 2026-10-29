//! `loquax --daemon`: the home node (`docs/src/concepts/loquax-home-node.md`).
//!
//! Headless, the 2026-08-08 option C (`docs/src/concepts/loquax-architecture.md`,
//! "process model"): the engine, the store and the router against the local
//! shared instance, and no screen. It
//!
//! - receives as the messenger does: the delivery destination of the home
//!   identity, announced at start and every [`ANNOUNCE_EVERY`], so senders
//!   deliver DIRECT and get their proof;
//! - archives every received message, packed as it came, under a sequence
//!   ([`crate::store::Store::archive_append`]), within the configured cap;
//! - answers its admitted clients on `loquax.home` ([`super::server`]);
//! - collects from its propagation node on the messenger's own schedule
//!   ([`crate::sync::Schedule`]), so mail that senders propagated arrives
//!   too;
//! - delivers what its clients hand in with `/send`, by the outbox policy
//!   of [`super::outgoing`]: direct at once, a propagated copy on failure,
//!   direct retries on every announce of the recipient and on a slow
//!   cadence, each state change an archive entry.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use leviculum_core::RequestError;
use leviculum_lxmf::router::MessageState;
use leviculum_std::ReticulumNode;
use tokio::sync::mpsc::unbounded_channel;

use crate::clock::Tokio;
use crate::effects::{Effects, Terminal};
use crate::engine::{attach_as, AttachConfig, HomeRequest, Role};
use crate::fanout::Fanout;
use crate::fetch::{run_fetch, FetchOptions};
use crate::home::outgoing::{Act, Change, Leg, Outcome, Outgoing, Policy};
use crate::mailbox_state::{MailboxRun, Stored};
use crate::outbox::{Command, Outbox, OutboxEvent, Via};
use crate::seen::{SeenStore, SeenWrite};
use crate::store::Store;
use crate::sync::{Schedule, SyncEvent};

/// How often the home node announces its delivery destination again: the
/// interval lxmd's config template suggests for the same announce
/// (`reference/LXMF/LXMF/Utilities/lxmd.py:1140`, `announce_interval = 360`,
/// in minutes).
pub const ANNOUNCE_EVERY: Duration = Duration::from_secs(360 * 60);

/// How often the engine's events are read.
const PUMP: Duration = Duration::from_millis(20);
/// How often the outbox looks for a due retry or a passed horizon. Both are
/// counted in seconds.
const OUTBOX_TICK: Duration = Duration::from_secs(1);

/// What starting the home node needs.
pub struct DaemonOptions {
    pub home: PathBuf,
    pub instance: String,
    /// The daemon's Reticulum config directory, for its RPC key.
    pub config_dir: PathBuf,
    pub storage_dir: PathBuf,
    pub identity: leviculum_core::Identity,
    pub display_name: Vec<u8>,
    /// The archive's cap in bytes.
    pub archive_cap_bytes: u64,
    /// How long an admission token stays good, in seconds.
    pub token_ttl_secs: i64,
    /// The outbox's retry cadence and horizon.
    pub policy: Policy,
    /// The propagation node to collect from, as `--pn` and the config name
    /// it, with the word `LNMSG_PN` logs for where it came from.
    pub preferred_pn: Option<[u8; 16]>,
    pub pn_source: &'static str,
    /// The collection schedule's bound, `None` for no collection.
    pub sync_bound_ms: Option<u64>,
    /// One collection's budget.
    pub fetch_budget: Duration,
    /// How each collection selects its mailbox without a preference: `main`
    /// reads the daemon's path table for it.
    pub mailbox: MailboxSource,
}

/// A source of [`MailboxRun`]s, read once per collection.
pub type MailboxSource =
    Box<dyn Fn() -> Pin<Box<dyn Future<Output = MailboxRun> + Send>> + Send + Sync>;

/// The runners' effects on the home node: the terminal's files, its notices
/// on stderr (the journal under systemd), and a fetched message never
/// printed, because the archive has it and a journal is no place for mail.
struct Quiet(Terminal);

impl Effects for Quiet {
    fn notice(&mut self, line: &str) {
        self.0.notice(line);
    }

    fn print(&mut self, _rendered: &[u8]) -> Result<(), String> {
        Ok(())
    }

    fn save_mailbox(&mut self, stored: &Stored) -> Result<(), String> {
        self.0.save_mailbox(stored)
    }

    fn write_seen(&mut self, write: &SeenWrite) -> Result<(), String> {
        self.0.write_seen(write)
    }

    fn unix_now_ms(&self) -> u64 {
        self.0.unix_now_ms()
    }
}

/// Run the home node until SIGINT or SIGTERM.
pub async fn run(options: DaemonOptions) -> Result<(), String> {
    let DaemonOptions {
        home,
        instance,
        config_dir,
        storage_dir,
        identity,
        display_name,
        archive_cap_bytes,
        token_ttl_secs,
        policy,
        preferred_pn,
        pn_source,
        sync_bound_ms,
        fetch_budget,
        mailbox,
    } = options;
    let _lock = super::lock::DeliveryLock::acquire(&home, super::lock::HOME_NODE)?;
    let address = crate::engine::delivery_address(&identity)?;
    let home_destination = crate::home::destination_hash(&identity.public_key_bytes())?;
    let mut store = Store::open_in(&home, address).map_err(|e| e.to_string())?;
    let signer = identity.clone();
    let mut outgoing = Outgoing::new(policy);
    for kept in store.outbox_open().map_err(|e| e.to_string())? {
        outgoing.add(
            kept.id,
            kept.destination,
            kept.packed,
            kept.created_at,
            kept.propagated,
        );
    }
    let collector = Collector {
        home: home.clone(),
        instance: instance.clone(),
        budget: fetch_budget,
        preferred: preferred_pn,
        source: pn_source,
        mailbox,
    };
    let (requests_tx, mut requests) = unbounded_channel::<HomeRequest>();
    let mut attached = attach_as(
        AttachConfig {
            instance,
            config_dir: Some(config_dir),
            storage_dir,
            identity,
            display_name,
            notices: Terminal::new(home),
        },
        Role::Home {
            requests: requests_tx,
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    let node = attached.node();
    tracing::warn!(
        "HOME up address={} home={} archive_cap={}B outbox={}",
        crate::address::to_hex(&address),
        crate::address::to_hex(&home_destination),
        archive_cap_bytes,
        outgoing.len()
    );
    let serving = super::server::Home {
        identity: &signer,
        address,
        token_ttl_secs,
        cap_bytes: archive_cap_bytes,
    };
    let mut sender = Sender {
        cap_bytes: archive_cap_bytes,
        preferred_pn,
        collecting: sync_bound_ms.is_some(),
        told: std::collections::HashSet::new(),
    };

    let mut fanout = Fanout::default();
    let mut schedule = sync_bound_ms.map(|bound| Schedule::new(Some(bound)));
    let mut fetch: Option<tokio::task::JoinHandle<Option<usize>>> = None;
    // The node the engine's router has selected, as its PnSelected said:
    // not `--pn` itself, which the router learns only from a SelectPn, and
    // with collections off nothing else sends one.
    let mut selected_pn = None;
    let origin = tokio::time::Instant::now();
    let now_ms = || u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut pump = tokio::time::interval(PUMP);
    let mut outbox_tick = tokio::time::interval(OUTBOX_TICK);
    let mut announce =
        tokio::time::interval_at(tokio::time::Instant::now() + ANNOUNCE_EVERY, ANNOUNCE_EVERY);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("SIGTERM handler: {e}"))?;

    let outcome = loop {
        tokio::select! {
            Some(request) = requests.recv() => {
                if let Some(accepted) = answer(&node, &mut store, &serving, request).await {
                    outgoing.add(
                        accepted.id,
                        accepted.destination,
                        accepted.packed,
                        accepted.created_at,
                        false,
                    );
                    let acts = outgoing.tick(unix_now());
                    sender.carry_out(&mut attached.outbox, &mut store, &mut outgoing, acts, selected_pn);
                }
            }
            _ = outbox_tick.tick() => {
                let acts = outgoing.tick(unix_now());
                sender.carry_out(&mut attached.outbox, &mut store, &mut outgoing, acts, selected_pn);
            }
            _ = pump.tick() => {
                let events = match fanout.pump(&mut attached.outbox) {
                    Ok(events) => events,
                    Err(gone) => break Err(gone.to_string()),
                };
                for event in events {
                    match event {
                        OutboxEvent::Ready { .. } => {
                            if let Some(schedule) = &mut schedule {
                                schedule.on(SyncEvent::Start, now_ms());
                            }
                        }
                        OutboxEvent::Packed { message_id, packed } => {
                            archive(&mut store, &message_id, &packed, archive_cap_bytes);
                        }
                        OutboxEvent::PnSelected { destination, .. } => {
                            selected_pn = Some(destination);
                        }
                        OutboxEvent::PnUnavailable { detail } => {
                            tracing::info!("HOME no propagation node: {detail}");
                        }
                        OutboxEvent::Announced { destination, .. } => {
                            let acts = outgoing.announced(destination, unix_now());
                            sender.carry_out(&mut attached.outbox, &mut store, &mut outgoing, acts, selected_pn);
                        }
                        OutboxEvent::Left { message_id, last } => {
                            let outcome = match last {
                                Some(MessageState::Delivered) => Outcome::Delivered,
                                Some(MessageState::Sent | MessageState::AwaitingCollection) => {
                                    Outcome::Sent
                                }
                                _ => Outcome::Failed,
                            };
                            sender.left(&mut attached.outbox, &mut store, &mut outgoing, &message_id, outcome, selected_pn);
                        }
                        OutboxEvent::Unsent { message_id, detail } => {
                            tracing::info!(
                                "HOME outbox unsent message={} reason={detail}",
                                crate::address::to_hex(&message_id)
                            );
                            sender.left(&mut attached.outbox, &mut store, &mut outgoing, &message_id, Outcome::Failed, selected_pn);
                        }
                        OutboxEvent::NodeAnnounced { destination }
                            if Some(destination) == selected_pn =>
                        {
                            if let Some(schedule) = &mut schedule {
                                schedule.on(SyncEvent::Announce, now_ms());
                            }
                        }
                        OutboxEvent::Broken { detail } => {
                            tracing::warn!("HOME engine broken: {detail}");
                        }
                        _ => {}
                    }
                }
                if let Some(task) = fetch.take_if(|task| task.is_finished()) {
                    let event = match task.await {
                        Ok(Some(new)) => SyncEvent::Done { new },
                        _ => SyncEvent::Failed,
                    };
                    if let Some(schedule) = &mut schedule {
                        schedule.on(event, now_ms());
                    }
                }
                if fetch.is_none() {
                    if let Some(schedule) = &mut schedule {
                        if schedule.due(now_ms()).is_some() {
                            schedule.begin(now_ms());
                            fetch = Some(collector.collect(&mut fanout));
                        }
                    }
                }
            }
            _ = announce.tick() => {
                if attached.outbox.submit(Command::Announce).is_err() {
                    break Err("the message engine stopped".to_string());
                }
            }
            _ = tokio::signal::ctrl_c() => break Ok(()),
            _ = terminate.recv() => break Ok(()),
        }
    };
    if let Some(task) = fetch {
        task.abort();
    }
    drop(node);
    attached.stop().await;
    outcome
}

/// Keep one received message in the archive.
fn archive(store: &mut Store, message_id: &[u8; 32], packed: &[u8], cap: u64) {
    let now = leviculum_std::local_time::unix_now_secs();
    match store.archive_append(crate::home::KIND_RECEIVED, message_id, packed, now, cap) {
        Ok(Some(seq)) => tracing::info!(
            "HOME archived message={} seq={seq} bytes={}",
            crate::address::to_hex(message_id),
            packed.len()
        ),
        Ok(None) => tracing::info!(
            "HOME archived already message={}",
            crate::address::to_hex(message_id)
        ),
        Err(error) => tracing::warn!("HOME archive: {error}"),
    }
}

/// The unix time in seconds, the outbox's clock.
fn unix_now() -> i64 {
    leviculum_std::local_time::unix_now_secs()
}

/// Answer one request, as a packet when it fits and as a resource when not.
/// The message a `/send` put in the outbox comes back, when it is new.
async fn answer(
    node: &ReticulumNode,
    store: &mut Store,
    serving: &super::server::Home<'_>,
    request: HomeRequest,
) -> Option<crate::store::Outgoing> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64());
    let answer = super::server::answer(
        store,
        serving,
        &request.path,
        &request.data,
        request.remote,
        now,
    );
    let bytes = &answer.bytes;
    let sent = match node
        .send_response(&request.link_id, &request.request_id, bytes)
        .await
    {
        Err(leviculum_std::Error::Request(RequestError::PayloadTooLarge)) => {
            node.send_response_resource(&request.link_id, &request.request_id, bytes)
                .await
        }
        other => other,
    };
    if let Err(error) = sent {
        // The outbox has the message whether the answer arrived or not: a
        // client that retries with the same id is answered from it.
        tracing::warn!("HOME answer to {}: {error}", request.path);
    }
    answer.accepted
}

/// What carrying out the outbox's acts needs besides the engine and the
/// store.
struct Sender {
    cap_bytes: u64,
    preferred_pn: Option<[u8; 16]>,
    /// Whether collections run, the only thing that selects a propagation
    /// node when none is named.
    collecting: bool,
    /// The messages already told they have no propagation node: said once
    /// each, not on every failed direct retry.
    told: std::collections::HashSet<[u8; 32]>,
}

impl Sender {
    /// The router let go of `id`: tell the outbox, and carry out what it
    /// says.
    fn left(
        &mut self,
        engine: &mut impl Outbox,
        store: &mut Store,
        outgoing: &mut Outgoing,
        id: &[u8; 32],
        outcome: Outcome,
        selected_pn: Option<[u8; 16]>,
    ) {
        if let Some(leg) = outgoing.in_flight(id) {
            tracing::info!(
                "HOME outbox {} {} message={}",
                leg_word(leg),
                match outcome {
                    Outcome::Delivered => "delivered",
                    Outcome::Sent => "handed-on",
                    Outcome::Failed => "failed",
                },
                crate::address::to_hex(id)
            );
        }
        let acts = outgoing.left(id, outcome);
        self.carry_out(engine, store, outgoing, acts, selected_pn);
    }

    /// Hand each message to the router on its leg, and record each state
    /// change. A leg that cannot start is ended as failed at once, so the
    /// outbox moves on.
    fn carry_out(
        &mut self,
        engine: &mut impl Outbox,
        store: &mut Store,
        outgoing: &mut Outgoing,
        acts: Vec<Act>,
        selected_pn: Option<[u8; 16]>,
    ) {
        let mut acts = std::collections::VecDeque::from(acts);
        while let Some(act) = acts.pop_front() {
            match act {
                Act::Record { id, change } => self.record(store, &id, change),
                Act::Send { id, leg } => {
                    let Some(packed) = outgoing.packed(&id).map(<[u8]>::to_vec) else {
                        continue;
                    };
                    let started = match leg {
                        Leg::Direct => engine
                            .submit(Command::SendPacked {
                                packed,
                                via: Via::Link,
                            })
                            .is_ok(),
                        Leg::Propagated => match selected_pn.or(self.preferred_pn) {
                            Some(node) => {
                                // The node the collections use, selected again
                                // if none was this run: one command queue, so
                                // the selection is in before the send.
                                let selected = selected_pn.is_some()
                                    || engine
                                        .submit(Command::SelectPn {
                                            preferred: Some(node),
                                            mailbox: None,
                                        })
                                        .is_ok();
                                selected
                                    && engine
                                        .submit(Command::SendPacked {
                                            packed,
                                            via: Via::Propagated,
                                        })
                                        .is_ok()
                            }
                            // Not selected here by the mailbox rule: that
                            // reads the daemon's path table and may wait for
                            // announce replays, which this path cannot. A
                            // collection selects one, or --pn names it.
                            None => {
                                if self.told.insert(id) {
                                    tracing::info!(
                                        "HOME outbox no propagation node message={}; direct \
                                         retries only{}",
                                        crate::address::to_hex(&id),
                                        match self.collecting {
                                            true => " until a collection selects one",
                                            false =>
                                                ", since collections are off \
                                                      (--sync-interval off) and no --pn \
                                                      names one",
                                        }
                                    );
                                }
                                false
                            }
                        },
                    };
                    if started {
                        tracing::info!(
                            "HOME outbox {} message={}",
                            leg_word(leg),
                            crate::address::to_hex(&id)
                        );
                    } else {
                        acts.extend(outgoing.left(&id, Outcome::Failed));
                    }
                }
            }
        }
    }

    fn record(&mut self, store: &mut Store, id: &[u8; 32], change: Change) {
        if change != Change::Propagated {
            self.told.remove(id);
        }
        match store.outbox_record(id, change, unix_now(), self.cap_bytes) {
            Ok(seq) => tracing::info!(
                "HOME outbox {} message={} seq={}",
                change.word(),
                crate::address::to_hex(id),
                seq.map_or_else(|| "-".to_string(), |seq| seq.to_string())
            ),
            Err(error) => tracing::warn!("HOME outbox record: {error}"),
        }
    }
}

fn leg_word(leg: Leg) -> &'static str {
    match leg {
        Leg::Direct => "direct",
        Leg::Propagated => "propagated",
    }
}

/// What a collection from the propagation node needs.
struct Collector {
    home: PathBuf,
    instance: String,
    budget: Duration,
    preferred: Option<[u8; 16]>,
    source: &'static str,
    mailbox: MailboxSource,
}

impl Collector {
    /// One collection, on a tap of its own, the way `loquax fetch` runs it:
    /// how many messages it brought, `None` when it failed.
    fn collect(&self, fanout: &mut Fanout) -> tokio::task::JoinHandle<Option<usize>> {
        let mut tap = fanout.tap();
        let home = self.home.clone();
        let mut options = FetchOptions::new(self.instance.clone(), self.budget);
        let (preferred, source) = (self.preferred, self.source);
        let mailbox = (self.mailbox)();
        tokio::spawn(async move {
            options.mailbox = mailbox.await;
            let mut seen = match SeenStore::load(&home) {
                Ok(seen) => seen,
                Err(error) => {
                    tracing::warn!("HOME collect: seen store: {error}");
                    return None;
                }
            };
            let mut effects = Quiet(Terminal::new(home));
            match run_fetch(
                &mut tap,
                preferred,
                source,
                &options,
                &mut effects,
                &mut seen,
                &Tokio::new(),
            )
            .await
            {
                Ok(fetched) => Some(fetched.received),
                Err(error) => {
                    tracing::info!("HOME collect: {error}");
                    None
                }
            }
        })
    }
}
