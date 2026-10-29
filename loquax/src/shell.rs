//! The messenger's shell below its frontends: the engine attached to the
//! running daemon, the history, the job queue, the mailbox's schedule, and
//! the model ([`loquax::ui`]) they feed. The terminal (`tui`) and the
//! window (`gui`) each start one with [`start`], fold their reader's events
//! into it, draw what it holds and hand it back to [`finish`]; neither
//! wires an engine, a store or a runner of its own.
//!
//! - [`Shell`] holds the engine, the fan-out and the job queue: it folds
//!   events into the model and runs the effects that come back. A send is
//!   [`run_send_auto`] and a fetch is [`run_fetch`], the CLI's own runners,
//!   each on a [`Tap`]: sends one at a time, and a fetch alongside them, so
//!   a send never waits for a mailbox that does not answer (leviculum 1072).
//!   The screen reads the same events.
//! - [`start`] is everything before the first frame: the identity, the
//!   store and what it restores, the daemon, the contacts, the schedule and
//!   the map's regions. [`finish`] lets the running jobs settle and detaches.
//!
//! Moved out of `tui.rs` when the GUI came (leviculum 1084): what is here
//! was the terminal's and is now both frontends'.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tui_input::Input;

use loquax::clock::Tokio;
use loquax::effects::{Effects, Terminal as Files};
use loquax::engine::{attach, AttachConfig, Attached};
use loquax::fanout::{Fanout, Tap};
use loquax::fetch::{run_fetch, FetchError, FetchOptions};
use loquax::mailbox::{Unanswered, UNANSWERED_EXCLUSION_MS, UNANSWERED_LIMIT};
use loquax::mailbox_state::{self, short, Stored};
use loquax::mapload::{Loader, Source, Step, CACHE_DEFAULT_MB};
use loquax::outbox::{Command, Outbox, OutboxEvent, OutboxGone, SendRequest, Via};
use loquax::picture::Maps;
use loquax::seen::{SeenStore, SeenWrite};
use loquax::send::{run_send_auto, AutoError, Leg, SendError, SendOptions};
use loquax::store::Store;
use loquax::ui::history::StoreRead;
use loquax::ui::map::{MapSetup, MapWant};
use loquax::ui::{update, MapLoad, UiEffect, UiEvent, UiModel};
use loquax::when::WallTime;
use loquax::{display_name, identity};

/// The model as both frontends hold it: its text fields are `tui-input`'s,
/// a buffer without a terminal in it.
pub(crate) type Model = UiModel<Input>;

/// The spinner and toast cadence the brief names.
pub(crate) const TICK_MS: u64 = 250;
/// How often the engine's queue is read: the send runner's own poll.
pub(crate) const PUMP_MS: u64 = 20;
/// How long leaving waits for the running send or fetch to read what the
/// engine already said. A runner reads its tap every 20 ms, so one with its
/// final state in hand ends well inside this; one still waiting on the
/// network is dropped after it, as before.
const SETTLE_MS: u64 = 1_000;
/// A key script's time of day until its `clock` step says another:
/// Monday 26 October 2026, 12:00, at UTC. A script never reads the wall
/// clock, so its frames are the same on every run.
const SCRIPT_CLOCK: (i64, u32, u32, u32, u32) = (2026, 10, 26, 12, 0);

/// The log file's filter without `RUST_LOG`.
const LOG_FILTER: &str = "warn,loquax=info";

/// What a frontend tells the shell it starts.
pub(crate) struct ShellOptions {
    /// `--sync-interval`, when given: the schedule's bound in milliseconds,
    /// `None` inside for the timer off.
    pub sync_interval: Option<Option<u64>>,
    pub to: Vec<[u8; 16]>,
    pub pn: Option<String>,
    pub instance: Option<String>,
    pub config: Option<PathBuf>,
    pub timeout: u64,
    pub ascii: bool,
    pub no_mouse: bool,
    /// A key script drives the session: its own clock, and a mailbox
    /// schedule only when the command line names one.
    pub scripted: bool,
    /// The file beside the identity the log goes to: `tui.log`, `gui.log`.
    pub log_file: &'static str,
}

/// A received message the store already holds, which the screen drops
/// before it folds: the mailbox copy of one that came directly, perhaps
/// before a restart and below the window the screen loaded. A store that
/// cannot answer lets the message through; a duplicate beats a lost one.
pub(crate) fn already_held(store: &Store, event: &OutboxEvent) -> bool {
    let OutboxEvent::Received { message_id, .. } = event else {
        return false;
    };
    match store.holds_received(message_id) {
        Ok(held) => {
            if held {
                tracing::info!("HELD message={}", loquax::address::to_hex(message_id));
            }
            held
        }
        Err(error) => {
            tracing::warn!("{error}");
            false
        }
    }
}

/// Effects for the runners and the stamp worker: their lines become toasts,
/// their files are the CLI's, and a fetched message is not printed, because
/// the screen already has it from the engine's `Received`.
#[derive(Clone)]
pub(crate) struct ScreenEffects {
    notices: UnboundedSender<String>,
    /// The send runner's leg changes, for the running send's row.
    legs: UnboundedSender<Leg>,
    /// Each selection a runner keeps, for the header's mailbox state.
    mailbox: UnboundedSender<Stored>,
    files: PathBuf,
}

impl Effects for ScreenEffects {
    fn notice(&mut self, line: &str) {
        let _ = self.notices.send(line.to_string());
    }

    fn leg(&mut self, leg: Leg) {
        let _ = self.legs.send(leg);
    }

    fn print(&mut self, _rendered: &[u8]) -> Result<(), String> {
        Ok(())
    }

    fn save_mailbox(&mut self, stored: &Stored) -> Result<(), String> {
        let _ = self.mailbox.send(*stored);
        Files::new(self.files.clone()).save_mailbox(stored)
    }

    fn write_seen(&mut self, write: &SeenWrite) -> Result<(), String> {
        Files::new(self.files.clone()).write_seen(write)
    }

    fn unix_now_ms(&self) -> u64 {
        loquax::mailbox_state::unix_now_ms()
    }
}

/// What the runners need besides a tap.
#[derive(Clone)]
pub(crate) struct RunContext {
    home: PathBuf,
    config_dir: PathBuf,
    instance: String,
    preferred_pn: Option<[u8; 16]>,
    pn_source: &'static str,
    timeout: Duration,
    effects: ScreenEffects,
    /// The nodes that stopped answering when the job started, for its
    /// mailbox selection.
    excluded: Vec<[u8; 16]>,
}

/// A runner that ended, as its task hands it to the shell.
enum Finished {
    /// The running send, as the event the model folds.
    Send(UiEvent),
    /// The running fetch.
    Fetch(FetchEnd),
}

/// How a fetch ended, for the screen, `tui.log` and the mailbox's streak.
struct FetchEnd {
    result: Result<usize, FetchError>,
    /// The node the drain was pointed at: the first selection it saw.
    node: Option<[u8; 16]>,
    elapsed_ms: u64,
}

/// A runner's tap that notes the first node a selection named.
struct Watched {
    tap: Tap,
    node: Option<[u8; 16]>,
}

impl Outbox for Watched {
    fn submit(&self, command: Command) -> Result<(), OutboxGone> {
        self.tap.submit(command)
    }

    fn try_next_event(&mut self) -> Result<Option<OutboxEvent>, OutboxGone> {
        let event = self.tap.try_next_event()?;
        if let Some(OutboxEvent::PnSelected { destination, .. }) = &event {
            self.node.get_or_insert(*destination);
        }
        Ok(event)
    }
}

/// The engine, the fan-out, the job queue, the store, and the model they
/// feed.
pub(crate) struct Shell {
    pub(crate) model: Model,
    /// The history: every fold's changes are written here before the next
    /// event, and pages and search results are read from it.
    store: Store,
    /// The store failed a write: said once, then on every later failure in
    /// the log.
    store_failed: bool,
    attached: Attached,
    fanout: Fanout,
    context: RunContext,
    /// Sends not yet started, oldest first. They run one at a time: the
    /// model names a queued message after the oldest running send.
    sends: VecDeque<([u8; 16], String)>,
    /// A fetch asked for and not yet started, with its reason's word. The
    /// model asks for one at a time.
    fetch_owed: Option<&'static str>,
    /// The running send's task, so leaving can let it finish its last poll.
    send_task: Option<tokio::task::JoinHandle<()>>,
    /// The running fetch's task and its reason's word.
    fetch_task: Option<(tokio::task::JoinHandle<()>, &'static str)>,
    /// The mailbox's syncs that failed before a link, and the nodes they
    /// excluded.
    unanswered: Unanswered,
    /// A node just excluded, named beside the node that replaces it.
    replacing: Option<[u8; 16]>,
    /// Every leg a runner reported, with the message it was for (the
    /// model's conversation and message index): a script's `until leg`.
    pub(crate) legs_seen: Vec<((usize, usize), Leg)>,
    done_tx: UnboundedSender<Finished>,
    done_rx: UnboundedReceiver<Finished>,
    notices_rx: UnboundedReceiver<String>,
    legs_rx: UnboundedReceiver<Leg>,
    mailbox_rx: UnboundedReceiver<Stored>,
    /// Lines to print once the terminal is restored.
    pub(crate) afterwards: Vec<String>,
    /// The engine stopped answering.
    pub(crate) gone: bool,
    /// The reader asked for the screen to be drawn again from scratch.
    pub(crate) redraw: bool,
    /// What the screen asked of the terminal and not yet done: the real
    /// terminal does it, a script writes it into its frames.
    pub(crate) outputs: Vec<Output>,
    /// The maps directory every painter of this session draws from.
    pub(crate) maps: Maps,
    /// The autoload: regions the model asks for, one load at a time.
    loader: Loader,
    /// The origin of the monotonic clock the mailbox schedule runs on.
    origin: tokio::time::Instant,
}

/// What the model asks of the terminal beyond drawing.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Output {
    /// Put the text on the clipboard.
    Copy(String),
    /// Hand the URL to the system.
    Open(String),
    /// Capture the mouse or let it go.
    Mouse(bool),
    /// Draw the map the `n`th way the frontend offered (1093).
    MapDrawing(usize),
}

impl Shell {
    /// The loquax home: the identity, the store, the config. The terminal
    /// writes its map drawing there.
    #[cfg(feature = "tui")]
    pub(crate) fn home(&self) -> &std::path::Path {
        &self.context.home
    }

    /// Fold one event, write what it changed to the store, and run what
    /// comes back.
    pub(crate) fn apply(&mut self, event: UiEvent) {
        let effects = update(&mut self.model, event);
        self.persist();
        let mut answers = Vec::new();
        for effect in effects {
            match effect {
                UiEffect::Store(read) => answers.extend(self.read(read)),
                UiEffect::Send { to, body } => self.sends.push_back((to, body)),
                UiEffect::Fetch => {
                    let reason = self.model.fetch_reason.map_or("key", |r| r.word());
                    tracing::info!("SYNC reason={reason}");
                    self.fetch_owed = Some(reason);
                }
                UiEffect::Quit => {}
                UiEffect::Redraw => self.redraw = true,
                UiEffect::Notice(line) => self.afterwards.push(line),
                UiEffect::Copy(text) => self.outputs.push(Output::Copy(text)),
                UiEffect::Open(url) => self.outputs.push(Output::Open(url)),
                UiEffect::Mouse(on) => self.outputs.push(Output::Mouse(on)),
                UiEffect::MapDrawing(n) => self.outputs.push(Output::MapDrawing(n)),
                // The screen already shows the change; a file that would not
                // take it is said, because the next start would not show it.
                UiEffect::Contact(contact) => {
                    if let Err(error) = loquax::names::save_contact(&self.context.home, &contact) {
                        let line = format!(
                            "{}: not saved: {error}",
                            self.context
                                .home
                                .join(loquax::names::CONTACTS_FILE)
                                .display()
                        );
                        tracing::warn!("{line}");
                        self.afterwards.push(line.clone());
                        answers.push(UiEvent::Notice(line));
                    }
                }
                UiEffect::Log(line) => tracing::warn!("{line}"),
                UiEffect::LoadMap(want) => self.map_ask(want),
            }
        }
        for answer in answers {
            self.apply(answer);
        }
        self.start_next();
    }

    /// The fold's changes, in one transaction. A failed write is said on
    /// screen and kept for after the screen: the message is still shown,
    /// but the reader must know it will not be there after a restart.
    fn persist(&mut self) {
        let writes = self.model.take_writes();
        if let Err(error) = self.store.write(&writes) {
            loquax::events::store_failed(&error.to_string());
            if !self.store_failed {
                self.store_failed = true;
                self.afterwards.push(error.to_string());
            }
            update(
                &mut self.model,
                UiEvent::Notice(format!("not saved: {error}")),
            );
        }
    }

    /// What the screen asked the store, answered as the event the model
    /// waits for.
    fn read(&mut self, read: StoreRead) -> Option<UiEvent> {
        let answer = match read {
            StoreRead::Earlier {
                peer,
                before,
                through,
            } => self
                .store
                .earlier(peer, before, through)
                .map(|(messages, earlier)| UiEvent::Earlier {
                    peer,
                    messages,
                    earlier,
                }),
            StoreRead::Search(query) => self
                .store
                .search(&query, &self.model.loaded_from())
                .map(|hits| UiEvent::StoredHits { query, hits }),
        };
        match answer {
            Ok(event) => Some(event),
            Err(error) => {
                loquax::events::store_failed(&error.to_string());
                self.afterwards.push(error.to_string());
                Some(UiEvent::Notice(format!("history: {error}")))
            }
        }
    }

    /// A name an announce carried, kept for the next run.
    fn keep_name(&mut self, destination: [u8; 16]) {
        if self.model.address == Some(destination) {
            return;
        }
        let Some(name) = self.model.names.announced(&destination).map(str::to_string) else {
            return;
        };
        if let Err(error) = self.store.save_name(destination, &name) {
            loquax::events::store_failed(&error.to_string());
        }
    }

    /// The store's side of a signature that waits on its sender's key: the
    /// message's bytes while it waits, the verdict once it is in. The
    /// verdict goes to the store here because the screen holds only a
    /// window of each conversation (leviculum 1079).
    fn keep_signature(&mut self, event: &OutboxEvent) {
        let kept = match event {
            OutboxEvent::Unverified {
                message_id,
                source,
                packed,
            } => self.store.keep_unverified(message_id, source, packed),
            OutboxEvent::Verified {
                message_id, valid, ..
            } => self.store.settle_verdict(message_id, *valid),
            _ => return,
        };
        if let Err(error) = kept {
            loquax::events::store_failed(&error.to_string());
        }
    }

    /// Everything that arrived since the last call, without waiting.
    pub(crate) fn poll(&mut self) {
        match self.fanout.pump(&mut self.attached.outbox) {
            Ok(events) => {
                for event in events {
                    let named = match &event {
                        OutboxEvent::Announced {
                            destination,
                            name: Some(_),
                        } => Some(*destination),
                        _ => None,
                    };
                    if already_held(&self.store, &event) {
                        continue;
                    }
                    self.keep_signature(&event);
                    self.apply(UiEvent::Engine(event));
                    if let Some(destination) = named {
                        self.keep_name(destination);
                    }
                }
            }
            Err(gone) => {
                if !self.gone {
                    self.gone = true;
                    self.apply(UiEvent::Notice(gone.to_string()));
                    self.afterwards.push(gone.to_string());
                }
            }
        }
        // Before the runner's end: a leg it moved onto belongs to the send
        // that is still at the front of the model's queue.
        while let Ok(leg) = self.legs_rx.try_recv() {
            if let Some(&message) = self.model.sending.front() {
                self.legs_seen.push((message, leg));
            }
            self.apply(UiEvent::SendLeg(leg));
        }
        while let Ok(finished) = self.done_rx.try_recv() {
            match finished {
                Finished::Send(event) => {
                    self.send_task = None;
                    self.apply(event);
                }
                Finished::Fetch(end) => self.fetch_done(end),
            }
        }
        while let Ok(line) = self.notices_rx.try_recv() {
            self.apply(UiEvent::Notice(line));
        }
        while let Ok(stored) = self.mailbox_rx.try_recv() {
            self.replaced(&stored);
            self.apply(chosen(&stored));
        }
        let steps = self.loader.poll(self.elapsed_ms());
        self.map_steps(steps);
        self.apply(UiEvent::Elapsed(self.elapsed_ms()));
    }

    /// The clock's tick, every [`TICK_MS`]: the time of day to the model,
    /// a suspend said and folded where the wall clock ran ahead of the
    /// monotonic one (`clocks` holds both as last read), then the
    /// spinner's tick.
    pub(crate) fn tick(&mut self, clocks: &mut (i64, u64)) {
        let now = local_now();
        let read = (now.unix_secs, self.elapsed_ms());
        self.apply(UiEvent::Clock(now));
        if loquax::sync::resumed(*clocks, read) {
            tracing::info!("SYNC resumed after {}s", read.0 - clocks.0);
            self.apply(UiEvent::Resumed);
        }
        *clocks = read;
        self.apply(UiEvent::Tick);
    }

    /// The schedule's clock: tokio's, which on Linux stands still in
    /// suspend, as `crate::sync::resumed` needs.
    pub(crate) fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Start the oldest send if no send is running, and the owed fetch if no
    /// fetch is. A send and a fetch run alongside: each runner reads every
    /// event and acts only on its own (a send on its message id, a fetch on
    /// the drain's `Sync*` events), and both select the mailbox by the same
    /// rule from the same input, so a selection either asks for is the one
    /// the other uses. Sends stay one at a time, because the model names a
    /// queued message after the oldest running send.
    fn start_next(&mut self) {
        if self.send_task.is_none() {
            if let Some((to, body)) = self.sends.pop_front() {
                let tap = self.fanout.tap();
                let context = self.job_context();
                let done = self.done_tx.clone();
                self.send_task = Some(tokio::spawn(async move {
                    let event = send(tap, context, to, body).await;
                    let _ = done.send(Finished::Send(event));
                }));
            }
        }
        if self.fetch_task.is_none() {
            if let Some(reason) = self.fetch_owed.take() {
                let tap = self.fanout.tap();
                let context = self.job_context();
                let done = self.done_tx.clone();
                let task = tokio::spawn(async move {
                    let end = fetch(tap, context).await;
                    let _ = done.send(Finished::Fetch(end));
                });
                self.fetch_task = Some((task, reason));
            }
        }
    }

    /// Load `want`'s region, from the model's [`UiEffect::LoadMap`] or the
    /// window's key: queued behind a running load, skipped while its box
    /// backs off.
    pub(crate) fn map_ask(&mut self, want: MapWant) {
        let steps = self.loader.ask(want, self.elapsed_ms());
        self.map_steps(steps);
    }

    /// The loader's steps: one `tui.log` line each, and the model told. A
    /// region that landed reaches the model's setup before its `Done`, so
    /// the fit that follows sees it.
    fn map_steps(&mut self, steps: Vec<Step>) {
        for step in steps {
            match step {
                Step::Started(want) => {
                    tracing::info!(
                        "MAPLOAD start box={} zooms={}-{} reason={}",
                        box_words(&want),
                        want.zooms.0,
                        want.zooms.1,
                        want.reason.word()
                    );
                    self.apply(UiEvent::MapLoad(MapLoad::Started(want)));
                }
                Step::Done {
                    want,
                    landed,
                    elapsed_ms,
                } => {
                    tracing::info!(
                        "MAPLOAD done box={} zooms={}-{} bytes={} fetched={} elapsed_ms={elapsed_ms} file={} evicted={}",
                        box_words(&want),
                        landed.zooms.0,
                        landed.zooms.1,
                        landed.bytes,
                        landed.fetched,
                        landed.name,
                        landed.evicted.len()
                    );
                    let regions = self.maps.headers();
                    let zooms = leviculum_map::choice::view_zooms(regions.iter());
                    let setup = MapSetup {
                        zooms: (*zooms.start(), *zooms.end()),
                        regions,
                        ..self.model.map_setup.clone()
                    };
                    self.apply(UiEvent::MapSetup(setup));
                    self.apply(UiEvent::MapLoad(MapLoad::Done(want)));
                }
                Step::Failed {
                    want,
                    detail,
                    toast,
                    elapsed_ms,
                } => {
                    tracing::info!(
                        "MAPLOAD failed box={} zooms={}-{} detail={detail:?} elapsed_ms={elapsed_ms}",
                        box_words(&want),
                        want.zooms.0,
                        want.zooms.1
                    );
                    self.apply(UiEvent::MapLoad(MapLoad::Failed { want, toast }));
                }
                Step::Skipped { want, why } => {
                    tracing::info!("MAPLOAD skipped box={} why={why}", box_words(&want));
                }
            }
        }
    }

    /// What a job starting now runs with: the nodes excluded at this moment.
    fn job_context(&self) -> RunContext {
        let mut context = self.context.clone();
        context.excluded = self.unanswered.excluded(self.elapsed_ms());
        context
    }

    /// The fetch ended: said in `tui.log`, judged for the mailbox's streak,
    /// and folded.
    fn fetch_done(&mut self, end: FetchEnd) {
        let reason = self.fetch_task.take().map_or("key", |(_, reason)| reason);
        match &end.result {
            Ok(new) => tracing::info!(
                "SYNC done reason={reason} result=ok new={new} elapsed_ms={}",
                end.elapsed_ms
            ),
            Err(error) => {
                let text = error.to_string();
                let detail = text.lines().next().unwrap_or_default();
                tracing::info!(
                    "SYNC done reason={reason} result=failed detail={detail:?} elapsed_ms={}",
                    end.elapsed_ms
                );
            }
        }
        self.judge(&end);
        self.apply(UiEvent::FetchDone(end.result.map_err(|e| e.to_string())));
    }

    /// Count the fetch toward its node's streak. A node that did not answer
    /// [`UNANSWERED_LIMIT`] syncs in a row is excluded, and the next
    /// selection replaces it; one the user chose is only reported
    /// (`docs/src/concepts/loquax-mailbox.md`, section 1, "Which node").
    fn judge(&mut self, end: &FetchEnd) {
        let Some(node) = end.node else {
            return;
        };
        let error = match &end.result {
            Ok(_) => {
                self.unanswered.answered(node);
                return;
            }
            Err(error) => error,
        };
        if !self.unanswered.failed(node, error.before_link()) {
            return;
        }
        let by_user = self.context.preferred_pn.is_some()
            || mailbox_state::load(&self.context.home)
                .is_some_and(|stored| stored.node == node && stored.chosen_by_user);
        if by_user {
            tracing::info!(
                "MAILBOX unanswered node={} syncs={UNANSWERED_LIMIT} kept=user",
                loquax::address::to_hex(&node)
            );
            self.apply(UiEvent::Notice(format!(
                "mailbox {} did not answer {UNANSWERED_LIMIT} syncs in a row; you chose it, \
                 so it stays",
                short(&node)
            )));
            return;
        }
        tracing::info!(
            "MAILBOX unanswered node={} syncs={UNANSWERED_LIMIT} excluded_ms={UNANSWERED_EXCLUSION_MS}",
            loquax::address::to_hex(&node)
        );
        self.unanswered.exclude(node, self.elapsed_ms());
        self.replacing = Some(node);
    }

    /// A runner kept a selection: when it adopted a node in place of one
    /// that stopped answering, say both.
    fn replaced(&mut self, stored: &Stored) {
        let Some(old) = self.replacing else {
            return;
        };
        if stored.chosen_by_user || stored.node == old {
            return;
        }
        self.replacing = None;
        let line = format!(
            "mailbox {} adopted, {} did not answer",
            short(&stored.node),
            short(&old)
        );
        tracing::info!("{line}");
        self.apply(UiEvent::Notice(line));
    }

    /// Before leaving: hand the running send and fetch what the engine has
    /// said and give them [`SETTLE_MS`] to read it. Without this a send whose proof the screen
    /// had already shown was cut off before its runner logged `sending` and
    /// `proof`: a script that quits on `until delivery delivered` leaves in
    /// the same pump that delivered the proof (leviculum 1014).
    pub(crate) async fn settle(&mut self) {
        let mut tasks: Vec<tokio::task::JoinHandle<()>> =
            self.send_task.take().into_iter().collect();
        tasks.extend(self.fetch_task.take().map(|(task, _)| task));
        if tasks.is_empty() {
            return;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(SETTLE_MS);
        loop {
            if let Err(_gone) = self.fanout.pump(&mut self.attached.outbox) {
                break;
            }
            if tasks.iter().all(|task| task.is_finished()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(PUMP_MS)).await;
            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }
        for task in tasks {
            task.abort();
        }
    }
}

/// A load's box as `tui.log` writes it: west, south, east, north in
/// degrees.
fn box_words(want: &MapWant) -> String {
    let d = |e7: i32| format!("{:.5}", f64::from(e7) / 1e7);
    let b = &want.bounds;
    format!("{},{},{},{}", d(b.west), d(b.south), d(b.east), d(b.north))
}

/// The header's mailbox state for a kept selection.
fn chosen(stored: &Stored) -> UiEvent {
    UiEvent::MailboxChosen {
        node: stored.node,
        by_user: stored.chosen_by_user,
    }
}

/// A key script's clock before its first `clock` step.
pub(crate) fn script_clock() -> WallTime {
    let (year, month, day, hour, minute) = SCRIPT_CLOCK;
    WallTime::from_civil(year, month, day, hour, minute, 0)
}

/// The wall clock with the host's UTC offset at this instant.
pub(crate) fn local_now() -> WallTime {
    let unix_secs = leviculum_std::local_time::unix_now_secs();
    WallTime {
        unix_secs,
        offset_secs: leviculum_std::local_time::utc_offset_secs(unix_secs),
    }
}

/// One compose-line send, exactly as `loquax send --via auto` runs it.
async fn send(mut tap: Tap, context: RunContext, to: [u8; 16], body: String) -> UiEvent {
    let mut options = SendOptions::new(context.instance.clone(), context.timeout);
    let mut effects = context.effects.clone();
    let (home, config_dir, instance, pn, excluded) = (
        context.home.clone(),
        context.config_dir.clone(),
        context.instance.clone(),
        context.preferred_pn,
        context.excluded.clone(),
    );
    let request = SendRequest {
        destination: to,
        title: Vec::new(),
        body: body.into_bytes(),
        fields: Vec::new(),
        via: Via::Link,
    };
    let outcome = run_send_auto(
        &mut tap,
        request,
        context.preferred_pn,
        context.pn_source,
        &mut options,
        || async move { crate::mailbox_run(&home, &config_dir, &instance, pn, excluded).await },
        &mut effects,
        &Tokio::new(),
    )
    .await;
    match outcome {
        Ok(_) => UiEvent::SendDone(Ok(())),
        Err(AutoError::Direct(error @ SendError::Unproven { .. })) => {
            UiEvent::SendUnproven(error.to_string())
        }
        Err(AutoError::Direct(error)) => UiEvent::SendDone(Err(error.to_string())),
        Err(AutoError::Both { direct, fallback }) => {
            UiEvent::SendDone(Err(format!("direct: {direct}\npropagated: {fallback}")))
        }
    }
}

/// One mailbox drain, exactly as `loquax fetch` runs it.
async fn fetch(tap: Tap, context: RunContext) -> FetchEnd {
    let started = tokio::time::Instant::now();
    let mut watched = Watched { tap, node: None };
    let result = drain(&mut watched, context).await;
    FetchEnd {
        result,
        node: watched.node,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

async fn drain(outbox: &mut Watched, context: RunContext) -> Result<usize, FetchError> {
    let mut seen = SeenStore::load(&context.home)
        .map_err(|e| FetchError::SyncFailed(format!("seen store: {e}")))?;
    // A drain gets twice a send's budget, as on the command line.
    let mut options = FetchOptions::new(context.instance.clone(), context.timeout * 2);
    options.mailbox = crate::mailbox_run(
        &context.home,
        &context.config_dir,
        &context.instance,
        context.preferred_pn,
        context.excluded.clone(),
    )
    .await;
    let mut effects = context.effects.clone();
    run_fetch(
        outbox,
        context.preferred_pn,
        context.pn_source,
        &options,
        &mut effects,
        &mut seen,
        &Tokio::new(),
    )
    .await
    .map(|fetched| fetched.new)
}

/// Everything before the first frame: the log, the identity, the history
/// and what it restores, the daemon, the contacts and the config, the
/// mailbox's schedule and the map's regions. The config comes back beside
/// the shell for what only a frontend reads (the theme). An `Err` is the
/// exit the run ends with, said already.
pub(crate) async fn start(
    options: &ShellOptions,
) -> Result<(Shell, loquax::config::Config), ExitCode> {
    let home = identity::home_dir().map_err(|error| crate::failure(error, "no-home"))?;
    // A full screen has no stderr to spare: the warnings a CLI run prints go
    // to a file beside the identity instead, with loquax's own `info` lines,
    // the `SEND` step of every leg, the `PATH` answers and the `RPC` calls,
    // so a send that waits can be read back afterwards (leviculum 1002).
    // `RUST_LOG` replaces the filter whole.
    leviculum_std::event_log::install_global_subscriber_to_file(
        LOG_FILTER,
        &home.join(options.log_file),
    );
    let (preferred_pn, pn_source) =
        crate::resolve_pn(options.pn.as_deref(), &home).map_err(crate::usage)?;
    let identity = identity::load_or_create(&home.join("identity"))
        .map_err(|error| crate::failure(error, "no-identity"))?;
    // The history, before the daemon: a damaged store stops the start with
    // the file named, and nothing is written over it.
    let address = loquax::engine::delivery_address(&identity)
        .map_err(|error| crate::failure(error, "no-identity"))?;
    let store =
        Store::open_in(&home, address).map_err(|error| crate::failure(error, "no-store"))?;
    let history = store
        .restore()
        .and_then(|restored| {
            Ok((
                restored,
                store.names()?,
                store.last_key()?,
                store.positions()?,
                store.unverified()?,
            ))
        })
        .map_err(|error| crate::failure(error, "no-store"))?;
    let display_name = display_name::from_process(None).map_err(crate::usage)?;
    let config_dir = options
        .config
        .clone()
        .unwrap_or_else(leviculum_std::config::Config::default_config_dir);
    let instance = options
        .instance
        .clone()
        .unwrap_or_else(|| crate::instance_name(&config_dir));
    let storage_dir =
        crate::storage_dir(&home).map_err(|error| crate::failure(error, "no-daemon"))?;

    let (notices_tx, notices_rx) = unbounded_channel();
    let (legs_tx, legs_rx) = unbounded_channel();
    let (mailbox_tx, mailbox_rx) = unbounded_channel();
    let effects = ScreenEffects {
        notices: notices_tx,
        legs: legs_tx,
        mailbox: mailbox_tx,
        files: home.clone(),
    };
    let attached = attach(AttachConfig {
        instance: instance.clone(),
        storage_dir,
        identity,
        display_name: display_name.name.into_bytes(),
        notices: effects.clone(),
    })
    .await
    .map_err(|error| crate::failure(error, "no-daemon"))?;

    let config = loquax::config::load(&home).map_err(|error| crate::usage(error.to_string()))?;
    let contacts =
        loquax::names::load_contacts(&home).map_err(|error| crate::usage(error.to_string()))?;
    let mut model = Model::new(&options.to);
    model.names = loquax::names::Names::new(contacts);
    let (restored, names, last_key, positions, unverified) = history;
    // The messages an earlier run kept unverified: the engine checks each
    // once its sender's key is known, and asks for it until then.
    for (message_id, source, packed) in unverified {
        if attached
            .outbox
            .submit(Command::Verify {
                message_id,
                source,
                packed,
            })
            .is_err()
        {
            break;
        }
    }
    for (peer, name) in &names {
        model.names.remember(*peer, name);
    }
    // The time the restart's ledger lines carry: the script's own clock, or
    // the wall clock.
    let start = match options.scripted {
        true => script_clock(),
        false => local_now(),
    };
    update(&mut model, UiEvent::Clock(start));
    model.restore(restored);
    model.restore_positions(positions);
    model.activity = model.activity.max(last_key);
    model.ascii = options.ascii;
    model.mouse = !options.no_mouse;
    model.send_on_enter = config.send_on_enter.unwrap_or(true);
    // The mailbox's schedule: the flag, else the config file, else fifteen
    // minutes. A script runs one only when its command line names the flag,
    // as it ticks only when it says so.
    let bound = match (options.scripted, options.sync_interval) {
        (_, Some(bound)) => Some(bound),
        (false, None) => Some(
            config
                .sync_interval
                .unwrap_or(Some(loquax::sync::DEFAULT_BOUND_MS)),
        ),
        (true, None) => None,
    };
    model.sync = bound.map(loquax::sync::Schedule::new);
    if let Some(bound) = bound {
        tracing::info!(
            "SYNC bound={}",
            bound.map_or("off".to_string(), |ms| format!("{}s", ms / 1000))
        );
    }
    // The mailbox the header names before any run: `--pn` or the configured
    // node, else what the last run kept.
    let mailbox = match preferred_pn {
        Some(node) => Some(UiEvent::MailboxChosen {
            node,
            by_user: true,
        }),
        None => mailbox_state::load(&home).as_ref().map(chosen),
    };
    if let Some(event) = mailbox {
        update(&mut model, event);
    }
    // The map's regions: a file that does not open is named in the log
    // and left out. The painters read the directory again whenever the
    // autoload changes it.
    let maps = Maps::new(loquax::picture::maps_dir());
    for problem in maps.load().1 {
        tracing::warn!("{problem}");
    }
    model.map_autoload = config.map_autoload.unwrap_or(true);
    let source = Source::of(config.map_source.as_deref());
    tracing::info!(
        "MAPLOAD autoload={} source={}",
        if model.map_autoload { "on" } else { "off" },
        match &source {
            Source::Url(url) => url.clone(),
            Source::Daily(base) => format!("{base}<newest>.pmtiles"),
        }
    );
    let cache_bytes = config.map_cache_mb.unwrap_or(CACHE_DEFAULT_MB) << 20;
    let loader = Loader::new(maps.clone(), source, cache_bytes);
    let (done_tx, done_rx) = unbounded_channel();
    let shell = Shell {
        model,
        store,
        store_failed: false,
        attached,
        fanout: Fanout::default(),
        context: RunContext {
            home,
            config_dir,
            instance,
            preferred_pn,
            pn_source,
            timeout: Duration::from_secs(options.timeout),
            effects,
            excluded: Vec::new(),
        },
        sends: VecDeque::new(),
        fetch_owed: None,
        send_task: None,
        fetch_task: None,
        unanswered: Unanswered::default(),
        replacing: None,
        legs_seen: Vec::new(),
        done_tx,
        done_rx,
        notices_rx,
        legs_rx,
        mailbox_rx,
        afterwards: Vec::new(),
        gone: false,
        redraw: false,
        outputs: Vec::new(),
        maps,
        loader,
        origin: tokio::time::Instant::now(),
    };
    Ok((shell, config))
}

/// After the last frame: the running send and fetch settle, the engine
/// detaches, and what the screen kept for afterwards is printed.
pub(crate) async fn finish(mut shell: Shell, outcome: Result<(), String>) -> ExitCode {
    shell.settle().await;
    let Shell {
        attached,
        afterwards,
        ..
    } = shell;
    attached.stop().await;
    for line in afterwards {
        eprintln!("loquax: {line}");
    }
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => crate::failure(error, "failed"),
    }
}
