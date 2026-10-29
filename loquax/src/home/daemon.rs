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
//!   too.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use leviculum_core::RequestError;
use leviculum_std::ReticulumNode;
use tokio::sync::mpsc::unbounded_channel;

use crate::clock::Tokio;
use crate::effects::{Effects, Terminal};
use crate::engine::{attach_as, AttachConfig, HomeRequest, Role};
use crate::fanout::Fanout;
use crate::fetch::{run_fetch, FetchOptions};
use crate::mailbox_state::{MailboxRun, Stored};
use crate::outbox::{Command, Outbox, OutboxEvent};
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

/// What starting the home node needs.
pub struct DaemonOptions {
    pub home: PathBuf,
    pub instance: String,
    pub storage_dir: PathBuf,
    pub identity: leviculum_core::Identity,
    pub display_name: Vec<u8>,
    /// The archive's cap in bytes.
    pub archive_cap_bytes: u64,
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
        storage_dir,
        identity,
        display_name,
        archive_cap_bytes,
        preferred_pn,
        pn_source,
        sync_bound_ms,
        fetch_budget,
        mailbox,
    } = options;
    let address = crate::engine::delivery_address(&identity)?;
    let home_destination = crate::home::destination_hash(&identity.public_key_bytes())?;
    let mut store = Store::open_in(&home, address).map_err(|e| e.to_string())?;
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
        "HOME up address={} home={} archive_cap={}B",
        crate::address::to_hex(&address),
        crate::address::to_hex(&home_destination),
        archive_cap_bytes
    );

    let mut fanout = Fanout::default();
    let mut schedule = sync_bound_ms.map(|bound| Schedule::new(Some(bound)));
    let mut fetch: Option<tokio::task::JoinHandle<Option<usize>>> = None;
    let mut selected_pn = preferred_pn;
    let origin = tokio::time::Instant::now();
    let now_ms = || u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut pump = tokio::time::interval(PUMP);
    let mut announce =
        tokio::time::interval_at(tokio::time::Instant::now() + ANNOUNCE_EVERY, ANNOUNCE_EVERY);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("SIGTERM handler: {e}"))?;

    let outcome = loop {
        tokio::select! {
            Some(request) = requests.recv() => {
                answer(&node, &mut store, request).await;
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

/// Answer one request, as a packet when it fits and as a resource when not.
async fn answer(node: &ReticulumNode, store: &mut Store, request: HomeRequest) {
    let now = leviculum_std::local_time::unix_now_secs();
    let bytes = super::server::answer(store, &request.path, &request.data, request.remote, now);
    let sent = match node
        .send_response(&request.link_id, &request.request_id, &bytes)
        .await
    {
        Err(leviculum_std::Error::Request(RequestError::PayloadTooLarge)) => {
            node.send_response_resource(&request.link_id, &request.request_id, &bytes)
                .await
        }
        other => other,
    };
    if let Err(error) = sent {
        tracing::warn!("HOME answer to {}: {error}", request.path);
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
