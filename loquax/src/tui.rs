//! `loquax tui`: the terminal frontend over the core's screen model
//! ([`loquax::ui`]), slice one (`docs/src/concepts/loquax-ui.md`).
//!
//! The binary's module, not the library's: it reads the daemon's path table
//! and the config the way `send` and `fetch` do, through `main.rs`'s helpers.
//! Three parts:
//!
//! - [`draw`] turns a [`View`] into a ratatui frame over the shared widgets
//!   (footer, toast, help, the styled-line bridge).
//! - [`Shell`] holds the engine, the fan-out and the job queue: it folds
//!   events into the model and runs the effects that come back. A send is
//!   [`run_send_auto`] and a fetch is [`run_fetch`], the CLI's own runners,
//!   each on a [`Tap`]: sends one at a time, and a fetch alongside them, so a
//!   send never waits for a mailbox that does not answer (leviculum 1072).
//!   The screen reads the same events.
//! - [`run`] is the loop: the real terminal (`run_terminal`), or a key script
//!   that dumps frames to a file (`run_script`), which is how the snapshot
//!   test in `tests/tui_snapshot.rs` drives a session against a daemon.
//! - [`map`] draws the map's picture in the conversation pane's place, by
//!   sixel, kitty, half-blocks or braille.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crossterm::event::{
    Event, EventStream, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use futures::{FutureExt, StreamExt};
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tui_input::Input;

use leviculum_ui::clipboard::osc52;
use leviculum_ui::color::{resolve_tier_from_env, ColorFlag, Tier};
use leviculum_ui::delivery::DeliveryState;
use leviculum_ui::footer;
use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use leviculum_ui::input::{MouseButton, MouseEvent, MouseEventKind};
use leviculum_ui::theme::{resolve_theme, Theme, ThemeFlag};
use leviculum_ui::tokens::Tokens;
use leviculum_ui_ratatui::background::detect_background;
use leviculum_ui_ratatui::event::{key_event, mouse_event, step_event, EventStep};
use leviculum_ui_ratatui::geometry::to_ratatui;
use leviculum_ui_ratatui::images::{Picker, ProtocolType};
use leviculum_ui_ratatui::style::downgrade_buffer;
use leviculum_ui_ratatui::terminal::{install_panic_hook, set_mouse_capture, TerminalOps};
use leviculum_ui_ratatui::{help, to_ratatui_text, toast, CrosstermTerminal, TerminalGuard};

use loquax::clock::Tokio;
use loquax::effects::{Effects, Terminal as Files};
use loquax::engine::{attach, AttachConfig, Attached};
use loquax::fanout::{Fanout, Tap};
use loquax::fetch::{run_fetch, FetchError, FetchOptions};
use loquax::mailbox::{Unanswered, UNANSWERED_EXCLUSION_MS, UNANSWERED_LIMIT};
use loquax::mailbox_state::{self, short, Stored};
use loquax::outbox::{Command, Outbox, OutboxEvent, OutboxGone, SendRequest, Via};
use loquax::seen::{SeenStore, SeenWrite};
use loquax::send::{run_send_auto, AutoError, Leg, SendError, SendOptions};
use loquax::store::Store;
use loquax::ui::history::StoreRead;
use loquax::ui::{self, help_groups, update, Pane, UiEffect, UiEvent, UiModel, View};
use loquax::when::WallTime;
use loquax::{display_name, identity, keys};

pub mod map;

/// The model as the terminal holds it: its text fields are `tui-input`'s.
type Model = UiModel<Input>;

/// The spinner and toast cadence the brief names.
const TICK_MS: u64 = 250;
/// How often the engine's queue is read: the send runner's own poll.
const PUMP_MS: u64 = 20;
/// How long a script's `until` waits by default.
const UNTIL_SECS: u64 = 30;
/// The longest a script's `sleep` may ask for.
const SLEEP_MAX_MS: u64 = 60_000;
/// How long leaving waits for the running send or fetch to read what the
/// engine already said. A runner reads its tap every 20 ms, so one with its
/// final state in hand ends well inside this; one still waiting on the
/// network is dropped after it, as before.
const SETTLE_MS: u64 = 1_000;
/// A key script's time of day until its `clock` step says another:
/// Monday 26 October 2026, 12:00, at UTC. A script never reads the wall
/// clock, so its frames are the same on every run.
const SCRIPT_CLOCK: (i64, u32, u32, u32, u32) = (2026, 10, 26, 12, 0);
/// `tui.log`'s filter without `RUST_LOG`.
const TUI_LOG_FILTER: &str = "warn,loquax=info";

/// Everything `loquax tui` was told on the command line.
pub struct TuiOptions {
    /// `--sync-interval`, when given: the schedule's bound in milliseconds,
    /// `None` inside for the timer off.
    pub sync_interval: Option<Option<u64>>,
    pub to: Vec<[u8; 16]>,
    pub pn: Option<String>,
    pub instance: Option<String>,
    pub config: Option<PathBuf>,
    pub timeout: u64,
    pub ascii: bool,
    pub no_color: bool,
    /// `--color`: the ladder's second rung.
    pub color: ColorFlag,
    /// `--theme`, over the config file's `theme` key.
    pub theme: Option<ThemeFlag>,
    /// Start with the mouse not captured, so the terminal's own selection
    /// works; the palette's `mouse` turns capture on.
    pub no_mouse: bool,
    /// `--graphics`: how the map is drawn.
    pub graphics: map::GraphicsFlag,
    pub script: Option<PathBuf>,
    pub frames: Option<PathBuf>,
    pub size: (u16, u16),
}

/// A received message the store already holds, which the screen drops
/// before it folds: the mailbox copy of one that came directly, perhaps
/// before a restart and below the window the screen loaded. A store that
/// cannot answer lets the message through; a duplicate beats a lost one.
fn already_held(store: &Store, event: &OutboxEvent) -> bool {
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
struct ScreenEffects {
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
struct RunContext {
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
struct Shell {
    model: Model,
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
    legs_seen: Vec<((usize, usize), Leg)>,
    done_tx: UnboundedSender<Finished>,
    done_rx: UnboundedReceiver<Finished>,
    notices_rx: UnboundedReceiver<String>,
    legs_rx: UnboundedReceiver<Leg>,
    mailbox_rx: UnboundedReceiver<Stored>,
    /// Lines to print once the terminal is restored.
    afterwards: Vec<String>,
    /// The engine stopped answering.
    gone: bool,
    /// The reader asked for the screen to be drawn again from scratch.
    redraw: bool,
    /// What the screen asked of the terminal and not yet done: the real
    /// terminal does it, a script writes it into its frames.
    outputs: Vec<Output>,
    /// The region files found at start, for the map painter the loop
    /// makes once it knows the backend.
    regions: Vec<map::Installed>,
    /// The origin of the monotonic clock the mailbox schedule runs on.
    origin: tokio::time::Instant,
}

/// What the model asks of the terminal beyond drawing.
#[derive(Debug, PartialEq, Eq)]
enum Output {
    /// Put the text on the clipboard.
    Copy(String),
    /// Hand the URL to the system.
    Open(String),
    /// Capture the mouse or let it go.
    Mouse(bool),
}

impl Shell {
    /// Fold one event, write what it changed to the store, and run what
    /// comes back.
    fn apply(&mut self, event: UiEvent) {
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
                UiEffect::Log(line) => tracing::warn!("{line}"),
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

    /// Everything that arrived since the last call, without waiting.
    fn poll(&mut self) {
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
        self.apply(UiEvent::Elapsed(self.elapsed_ms()));
    }

    /// The schedule's clock: tokio's, which on Linux stands still in
    /// suspend, as `crate::sync::resumed` needs.
    fn elapsed_ms(&self) -> u64 {
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
    async fn settle(&mut self) {
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

/// The header's mailbox state for a kept selection.
fn chosen(stored: &Stored) -> UiEvent {
    UiEvent::MailboxChosen {
        node: stored.node,
        by_user: stored.chosen_by_user,
    }
}

/// A key script's clock before its first `clock` step.
fn script_clock() -> WallTime {
    let (year, month, day, hour, minute) = SCRIPT_CLOCK;
    WallTime::from_civil(year, month, day, hour, minute, 0)
}

/// The wall clock with the host's UTC offset at this instant.
fn local_now() -> WallTime {
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
        || async move { super::mailbox_run(&home, &config_dir, &instance, pn, excluded).await },
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
    options.mailbox = super::mailbox_run(
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

/// `loquax tui`.
pub async fn run(options: TuiOptions) -> ExitCode {
    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return super::failure(error, "no-home"),
    };
    // A full screen has no stderr to spare: the warnings a CLI run prints go
    // to a file beside the identity instead, with loquax's own `info` lines,
    // the `SEND` step of every leg, the `PATH` answers and the `RPC` calls,
    // so a send that waits can be read back afterwards (leviculum 1002).
    // `RUST_LOG` replaces the filter whole.
    leviculum_std::event_log::install_global_subscriber_to_file(
        TUI_LOG_FILTER,
        &home.join("tui.log"),
    );
    let (preferred_pn, pn_source) = match super::resolve_pn(options.pn.as_deref(), &home) {
        Ok(resolved) => resolved,
        Err(error) => return super::usage(error),
    };
    let identity = match identity::load_or_create(&home.join("identity")) {
        Ok(identity) => identity,
        Err(error) => return super::failure(error, "no-identity"),
    };
    // The history, before the daemon: a damaged store stops the start with
    // the file named, and nothing is written over it.
    let address = match loquax::engine::delivery_address(&identity) {
        Ok(address) => address,
        Err(error) => return super::failure(error, "no-identity"),
    };
    let store = match Store::open_in(&home, address) {
        Ok(store) => store,
        Err(error) => return super::failure(error, "no-store"),
    };
    let history = match store.restore().and_then(|restored| {
        Ok((
            restored,
            store.names()?,
            store.last_key()?,
            store.positions()?,
        ))
    }) {
        Ok(history) => history,
        Err(error) => return super::failure(error, "no-store"),
    };
    let display_name = match display_name::from_process(None) {
        Ok(resolved) => resolved,
        Err(error) => return super::usage(error),
    };
    let config_dir = options
        .config
        .clone()
        .unwrap_or_else(leviculum_std::config::Config::default_config_dir);
    let instance = options
        .instance
        .clone()
        .unwrap_or_else(|| super::instance_name(&config_dir));
    let storage_dir = match super::storage_dir(&home) {
        Ok(dir) => dir,
        Err(error) => return super::failure(error, "no-daemon"),
    };

    let (notices_tx, notices_rx) = unbounded_channel();
    let (legs_tx, legs_rx) = unbounded_channel();
    let (mailbox_tx, mailbox_rx) = unbounded_channel();
    let effects = ScreenEffects {
        notices: notices_tx,
        legs: legs_tx,
        mailbox: mailbox_tx,
        files: home.clone(),
    };
    let attached = match attach(AttachConfig {
        instance: instance.clone(),
        storage_dir,
        identity,
        display_name: display_name.name.into_bytes(),
        notices: effects.clone(),
    })
    .await
    {
        Ok(attached) => attached,
        Err(error) => return super::failure(error, "no-daemon"),
    };

    let config = match loquax::config::load(&home) {
        Ok(config) => config,
        Err(error) => return super::usage(error.to_string()),
    };
    let contacts = match loquax::names::load_contacts(&home) {
        Ok(contacts) => contacts,
        Err(error) => return super::usage(error.to_string()),
    };
    let mut model = Model::new(&options.to);
    model.names = loquax::names::Names::new(contacts);
    let (restored, names, last_key, positions) = history;
    for (peer, name) in &names {
        model.names.remember(*peer, name);
    }
    // The time the restart's ledger lines carry: the script's own clock, or
    // the wall clock.
    let start = match options.script {
        Some(_) => script_clock(),
        None => local_now(),
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
    let bound = match (&options.script, options.sync_interval) {
        (_, Some(bound)) => Some(bound),
        (None, None) => Some(
            config
                .sync_interval
                .unwrap_or(Some(loquax::sync::DEFAULT_BOUND_MS)),
        ),
        (Some(_), None) => None,
    };
    model.sync = bound.map(loquax::sync::Schedule::new);
    if let Some(bound) = bound {
        tracing::info!(
            "SYNC bound={}",
            bound.map_or("off".to_string(), |ms| format!("{}s", ms / 1000))
        );
    }
    // A script plays its own terminal: monochrome and dark until its `tier`
    // and `theme` steps say otherwise, whatever the host's.
    let theme_flag = options.theme.or(config.theme).unwrap_or_default();
    if options.script.is_none() {
        model.tier = resolve_tier_from_env(options.no_color, options.color);
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
    // The map's regions, read once: a file that does not open is named in
    // tui.log and left out.
    let (regions, problems) = map::maps_dir()
        .map(|dir| map::load(&dir))
        .unwrap_or_default();
    for problem in problems {
        tracing::warn!("{problem}");
    }
    let (done_tx, done_rx) = unbounded_channel();
    let mut shell = Shell {
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
        regions,
        origin: tokio::time::Instant::now(),
    };

    let outcome = match &options.script {
        Some(script) => {
            let frames = options
                .frames
                .clone()
                .unwrap_or_else(|| script.with_extension("frames"));
            run_script(&mut shell, script, &frames, &options).await
        }
        None => run_terminal(&mut shell, theme_flag, options.graphics).await,
    };
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
        Err(error) => super::failure(error, "failed"),
    }
}

/// The terminal with the keyboard enhancement protocol on top, where the
/// terminal speaks it: `Shift-Enter` and `Ctrl-Enter` then arrive as
/// themselves (loquax-keys-and-layout.md, section 3, "What the terminal can and
/// cannot send"). Pushed after the screen is entered and popped before it is
/// left, so the guard pops it on every way out; the panic hook pops it too.
struct KeysTerminal {
    inner: CrosstermTerminal,
    enhanced: bool,
}

impl TerminalOps for KeysTerminal {
    fn enter(&mut self) -> std::io::Result<()> {
        self.inner.enter()?;
        if self.enhanced {
            crossterm::execute!(
                std::io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
        }
        Ok(())
    }

    fn restore(&mut self) -> std::io::Result<()> {
        if self.enhanced {
            let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        }
        self.inner.restore()
    }
}

/// The once-at-start `tui.log` line naming the chords live in this terminal.
fn log_keys(model: &Model) {
    let ctx = model.ctx();
    tracing::info!(
        "KEYS enhancement={} newline={} send={}",
        if model.enhanced { "yes" } else { "no" },
        keys::newline_chord(ctx),
        keys::send_chord(ctx)
    );
}

/// The once-at-start `tui.log` line naming how the map is drawn, beside
/// the `KEYS` line.
fn log_graphics(painter: &map::MapPainter, detected: Option<ProtocolType>) {
    let setup = painter.setup();
    tracing::info!(
        "GRAPHICS backend={} detected={} cell={}x{} zooms={}-{}",
        painter.graphics().name(),
        detected.map_or("none".to_string(), |p| format!("{p:?}").to_lowercase()),
        setup.cell_px.0,
        setup.cell_px.1,
        setup.zooms.0,
        setup.zooms.1
    );
}

/// The real terminal: keys in, frames out, until the reader quits.
async fn run_terminal(
    shell: &mut Shell,
    theme: ThemeFlag,
    graphics: map::GraphicsFlag,
) -> Result<(), String> {
    install_panic_hook();
    // Before the alternate screen, as lnomad does: the background query reads
    // its answer on stdin, which raw mode would hand to the key handler. The
    // keyboard query likewise. Asked only under `auto`.
    shell.model.theme = resolve_theme(detect_background(theme), theme);
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        // Runs before install_panic_hook's restore, which leaves the screen.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
            previous(info);
        }));
    }
    shell.model.enhanced = enhanced;
    log_keys(&shell.model);
    let mut guard = TerminalGuard::new(KeysTerminal {
        inner: CrosstermTerminal::with_mouse(shell.model.mouse),
        enhanced,
    })
    .map_err(|e| e.to_string())?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(std::io::stdout())).map_err(|e| e.to_string())?;
    // The map's backend, asked of the terminal in raw mode, before the
    // event stream reads stdin. Only when a protocol may be taken: braille
    // and half-blocks need no answer, and `--no-color` sends no query.
    let ask = match graphics {
        map::GraphicsFlag::Auto => shell.model.tier != Tier::Mono,
        map::GraphicsFlag::Kitty | map::GraphicsFlag::Sixel => true,
        map::GraphicsFlag::Halfblocks | map::GraphicsFlag::Braille => false,
    };
    let picker = match ask {
        true => Picker::from_query_stdio().ok(),
        false => None,
    };
    let detected = picker.as_ref().map(Picker::protocol_type);
    let chosen = map::choose_graphics(graphics, detected, shell.model.tier);
    // The map is drawn on its own thread: a key is read while the last
    // one's view is painted and encoded (1075).
    let mut painter =
        map::MapPainter::new(std::mem::take(&mut shell.regions), chosen, picker).threaded();
    log_graphics(&painter, detected);
    shell.apply(UiEvent::MapSetup(painter.setup()));
    let mut events = EventStream::new();
    shell.apply(UiEvent::Clock(local_now()));
    let size = terminal.size().map_err(|e| e.to_string())?;
    shell.apply(UiEvent::Resize {
        width: size.width,
        height: size.height,
    });
    let mut tick = tokio::time::interval(Duration::from_millis(TICK_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pump = tokio::time::interval(Duration::from_millis(PUMP_MS));
    pump.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The wall clock against the monotonic one, read on every tick: the
    // wall running ahead is a suspend the monotonic clock slept through.
    let mut clocks = (local_now().unix_secs, shell.elapsed_ms());

    let result = loop {
        for output in std::mem::take(&mut shell.outputs) {
            if let Err(error) = run_output(output) {
                shell.apply(UiEvent::Notice(error));
            }
        }
        if shell.redraw {
            shell.redraw = false;
            if let Err(error) = terminal.clear() {
                break Err(error.to_string());
            }
        }
        let view = ui::view(&shell.model);
        if let Err(error) = terminal.draw(|frame| draw(frame, &view, Some(&mut painter))) {
            break Err(error.to_string());
        }
        if shell.model.quit {
            break Ok(());
        }
        tokio::select! {
            item = events.next() => {
                // A held key's repeats and a drag's motions that queued
                // while the last frame was drawn are one frame at the
                // newest view, not a frame each (1075).
                let ended = apply_waiting(item, &mut events, |event| shell.apply(event));
                if let Some(result) = ended {
                    break result;
                }
            }
            _ = painter.ready() => {}
            _ = pump.tick() => shell.poll(),
            _ = tick.tick() => {
                let now = local_now();
                let read = (now.unix_secs, shell.elapsed_ms());
                shell.apply(UiEvent::Clock(now));
                if loquax::sync::resumed(clocks, read) {
                    tracing::info!("SYNC resumed after {}s", read.0 - clocks.0);
                    shell.apply(UiEvent::Resumed);
                }
                clocks = read;
                shell.apply(UiEvent::Tick);
            }
        }
    };
    // How many map pictures went up, beside the GRAPHICS line: a held key
    // that drew far fewer than it pressed was coalesced (1075).
    tracing::info!("GRAPHICS paints={}", painter.paints());
    guard.restore_now().map_err(|e| e.to_string())?;
    result
}

/// `first` and every event already waiting behind it on `events`, applied
/// in order without waiting for more; `Some` when the stream ended.
fn apply_waiting<S>(
    first: Option<std::io::Result<Event>>,
    events: &mut S,
    mut apply: impl FnMut(UiEvent),
) -> Option<Result<(), String>>
where
    S: futures::Stream<Item = std::io::Result<Event>> + Unpin,
{
    let mut item = first;
    loop {
        match step_event(item, map_event) {
            EventStep::Apply(event) => apply(event),
            EventStep::Ignore => {}
            EventStep::End(result) => return Some(result.map_err(|e| e.to_string())),
        }
        item = events.next().now_or_never()?;
    }
}

/// What the model asked of the terminal, done: the copy as OSC 52, the URL
/// to the system's handler as lnomad hands it, the capture switched.
fn run_output(output: Output) -> Result<(), String> {
    match output {
        Output::Copy(text) => {
            crossterm::execute!(std::io::stdout(), crossterm::style::Print(osc52(&text)))
                .map_err(|e| format!("copy: {e}"))
        }
        Output::Open(url) => open::that(&url).map_err(|e| format!("could not open {url}: {e}")),
        Output::Mouse(on) => set_mouse_capture(on).map_err(|e| format!("mouse: {e}")),
    }
}

/// Keys and the mouse become the model's events; a resize tells the model
/// its new pages.
fn map_event(event: Event) -> Option<UiEvent> {
    match event {
        Event::Key(key) => Some(UiEvent::Key(key_event(key))),
        Event::Mouse(mouse) => Some(UiEvent::Mouse(mouse_event(mouse))),
        Event::Resize(width, height) => Some(UiEvent::Resize { width, height }),
        _ => None,
    }
}

/// A script's `until` condition.
#[derive(Debug)]
enum Until {
    /// The engine said `Ready`.
    Ready,
    /// The newest message we wrote in the selected conversation shows this
    /// state.
    Delivery(DeliveryState),
    /// This many received messages, across every conversation.
    Received(usize),
    /// This many peers' announces carried a name.
    Named(usize),
    /// The send runner reported this leg for the newest message we wrote in
    /// the selected conversation.
    Leg(Leg),
}

impl Until {
    fn holds(&self, model: &Model, legs_seen: &[((usize, usize), Leg)]) -> bool {
        match self {
            Self::Ready => model.address.is_some(),
            Self::Delivery(state) => {
                model
                    .messages_of_selected()
                    .iter()
                    .rev()
                    .find(|m| m.outbound)
                    .and_then(|m| m.delivery)
                    == Some(*state)
            }
            Self::Received(count) => {
                model
                    .conversations
                    .iter()
                    .flat_map(|c| c.messages.iter())
                    .filter(|m| !m.outbound)
                    .count()
                    >= *count
            }
            Self::Named(count) => model.names.heard_count() >= *count,
            Self::Leg(leg) => {
                let newest = model
                    .messages_of_selected()
                    .iter()
                    .rposition(|m| m.outbound)
                    .map(|m| (model.selected, m));
                newest.is_some_and(|newest| legs_seen.contains(&(newest, *leg)))
            }
        }
    }
}

/// One line of a key script.
#[derive(Debug)]
enum Step {
    Key(KeyEvent),
    /// The terminal world the script plays: enhanced or plain keyboard.
    Keyboard(bool),
    Type(String),
    Tick(u32),
    Frame(String),
    Until(Until, Duration),
    Sleep(u64),
    Clock(WallTime),
    /// Mouse events, at screen cells.
    Mouse(Vec<MouseEvent>),
    /// The colour tier the screen is drawn in from now on.
    Tier(Tier),
    /// Light or dark from now on.
    Theme(Theme),
    /// The screen's style map, to the styles file.
    Styles(String),
}

/// The script's key names: `tab`, `backtab`, `enter`, `esc`, `up`, `down`,
/// `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `backspace`,
/// `delete`, `space`, `f1` to `f12`, or one character, each after any of the
/// prefixes `ctrl-`, `alt-` and `shift-` (`shift-enter`, `ctrl-enter`,
/// `alt-<`, `ctrl-j`). An uppercase letter carries its Shift. A chord only an
/// enhanced terminal sends is delivered as such whatever the host terminal
/// is, so a script pins both worlds (`keyboard enhanced` sets which one the
/// screen believes it is in).
fn parse_key(name: &str) -> Result<KeyEvent, String> {
    let mut mods = KeyModifiers::NONE;
    let mut rest = name;
    loop {
        let (prefix, flag) = match rest {
            r if r.starts_with("ctrl-") && r.len() > 5 => ("ctrl-", KeyModifiers::CONTROL),
            r if r.starts_with("alt-") && r.len() > 4 => ("alt-", KeyModifiers::ALT),
            r if r.starts_with("shift-") && r.len() > 6 => ("shift-", KeyModifiers::SHIFT),
            _ => break,
        };
        mods |= flag;
        rest = &rest[prefix.len()..];
    }
    let code = match rest {
        "tab" => KeyCode::Tab,
        "backtab" => {
            mods |= KeyModifiers::SHIFT;
            KeyCode::BackTab
        }
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "space" => KeyCode::Char(' '),
        other => match (single(other), other.strip_prefix('f')) {
            (Some(c), _) => {
                if c.is_uppercase() {
                    mods |= KeyModifiers::SHIFT;
                }
                KeyCode::Char(c)
            }
            (None, Some(n)) => match n.parse::<u8>() {
                Ok(n @ 1..=12) => KeyCode::F(n),
                _ => return Err(format!("unknown key {name:?}")),
            },
            (None, None) => return Err(format!("unknown key {name:?}")),
        },
    };
    Ok(KeyEvent::new(code, mods))
}

fn single(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

/// A mouse event at a script's cell, 1-based as the frames are counted.
fn mouse_at(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: col - 1,
        row: row - 1,
        modifiers: KeyModifiers::NONE,
    }
}

/// `click`, `wheel` and `drag`: the events each stands for. Columns and rows
/// are screen cells from 1, the frames' first row and column.
fn parse_mouse(word: &str, rest: &str) -> Result<Vec<MouseEvent>, String> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let cell = |text: &str| -> Result<u16, String> {
        match text.parse::<u16>() {
            Ok(n) if n >= 1 => Ok(n),
            _ => Err(format!("bad cell {text:?}, from 1")),
        }
    };
    let left = MouseButton::Left;
    match (word, words.as_slice()) {
        ("click", [col, row]) => {
            let (col, row) = (cell(col)?, cell(row)?);
            Ok(vec![
                mouse_at(MouseEventKind::Down(left), col, row),
                mouse_at(MouseEventKind::Up(left), col, row),
            ])
        }
        ("wheel", [way, col, row, tail @ ..]) => {
            let kind = match *way {
                "up" => MouseEventKind::ScrollUp,
                "down" => MouseEventKind::ScrollDown,
                other => return Err(format!("unknown wheel {other:?}, up or down")),
            };
            let n = match tail {
                [] => 1,
                [n] => n.parse::<usize>().map_err(|_| format!("bad count {n:?}"))?,
                _ => return Err(format!("wheel takes up|down <col> <row> [n], not {rest:?}")),
            };
            let (col, row) = (cell(col)?, cell(row)?);
            Ok((0..n).map(|_| mouse_at(kind, col, row)).collect())
        }
        ("drag", [c1, r1, c2, r2]) => {
            let (c1, r1, c2, r2) = (cell(c1)?, cell(r1)?, cell(c2)?, cell(r2)?);
            Ok(vec![
                mouse_at(MouseEventKind::Down(left), c1, r1),
                mouse_at(MouseEventKind::Drag(left), c2, r2),
                mouse_at(MouseEventKind::Up(left), c2, r2),
            ])
        }
        ("click", _) => Err(format!("click takes <col> <row>, not {rest:?}")),
        ("wheel", _) => Err(format!("wheel takes up|down <col> <row> [n], not {rest:?}")),
        _ => Err(format!("drag takes <col> <row> <col> <row>, not {rest:?}")),
    }
}

/// `2026-10-24 13:58`, at UTC.
fn parse_clock(text: &str) -> Result<WallTime, String> {
    let bad = || format!("bad clock {text:?}, YYYY-MM-DD HH:MM");
    let (date, time) = text.split_once(' ').ok_or_else(bad)?;
    let mut date = date.split('-').map(str::parse::<u32>);
    let mut time = time.trim().split(':').map(str::parse::<u32>);
    let next = |part: Option<Result<u32, _>>| part.and_then(Result::ok).ok_or_else(bad);
    let (year, month, day) = (next(date.next())?, next(date.next())?, next(date.next())?);
    let (hour, minute) = (next(time.next())?, next(time.next())?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return Err(bad());
    }
    Ok(WallTime::from_civil(
        i64::from(year),
        month,
        day,
        hour,
        minute,
        0,
    ))
}

fn parse_state(name: &str) -> Result<DeliveryState, String> {
    Ok(match name {
        "queued" => DeliveryState::Queued,
        "stamping" => DeliveryState::Stamping,
        "inflight" => DeliveryState::InFlight,
        "sent" => DeliveryState::Sent,
        "mailbox" => DeliveryState::InMailbox,
        "delivered" => DeliveryState::Delivered,
        "rejected" => DeliveryState::Rejected,
        "failed" => DeliveryState::Failed,
        other => return Err(format!("unknown delivery state {other:?}")),
    })
}

/// Parse a key script. One step a line, `#` starts a comment:
///
/// ```text
/// key <name>                     one key (see parse_key)
/// type <text>                    each character of the rest of the line
/// newline                        Alt-Enter: a line break in compose
/// keyboard <enhanced|plain>      the terminal world the screen is told it
///                                is in (plain until said): its hints offer
///                                Shift-Enter and Ctrl-Enter only when enhanced
/// tick [n]                       n spinner/toast ticks (1)
/// frame [label]                  dump the screen
/// until ready [secs]             the engine is attached
/// until delivery <state> [secs]  the newest sent message shows <state>
/// until received <n> [secs]      n messages have arrived
/// until named <n> [secs]         n peers have announced a name
/// until leg <direct|propagated> [secs]
///                                the runner is on that leg for the newest
///                                sent message
/// sleep <ms>                     wait as the idle terminal does, up to 60000:
///                                the engine is read every 20 ms and one tick
///                                applied per 250 ms of the script's clock
/// clock <YYYY-MM-DD> <HH:MM>      the time of day from now on, at UTC (the
///                                script starts at 2026-10-26 12:00); it
///                                stands still until the next clock step
/// click <col> <row>              the left button down and up at a cell
/// wheel <up|down> <col> <row> [n]
///                                n wheel notches at a cell (1)
/// drag <col> <row> <col> <row>   the left button down at the first cell,
///                                moved to the second and up there
/// tier <mono|16|256|truecolor>   the colour tier from now on (mono until
///                                said, whatever the host terminal is)
/// theme <dark|light>             the theme from now on (dark until said)
/// styles <name>                  the screen's style map, one letter a cell
///                                (see style_letter), to the frames file's
///                                path with `.styles` added
/// ```
///
/// Cells count from 1 at the top left, as the frames' rows and columns do.
/// What the screen asks of the terminal is written into the frames where it
/// happens: `--- osc52 <sequence> ---` for a copy (the escape character as
/// `\e`), `--- open <url> ---` for a link handed to the system, which a
/// script never launches, and `--- mouse on|off ---`.
fn parse_script(text: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (number, raw) in text.lines().enumerate() {
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let at = |error: String| format!("line {}: {error}", number + 1);
        let secs = |text: Option<&str>| -> Result<Duration, String> {
            match text {
                None => Ok(Duration::from_secs(UNTIL_SECS)),
                Some(n) => n
                    .parse()
                    .map(Duration::from_secs)
                    .map_err(|_| format!("bad seconds {n:?}")),
            }
        };
        let step = match word {
            "key" => Step::Key(parse_key(rest.trim()).map_err(at)?),
            "type" => Step::Type(rest.to_string()),
            "newline" => Step::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            "keyboard" => match rest.trim() {
                "enhanced" => Step::Keyboard(true),
                "plain" => Step::Keyboard(false),
                other => return Err(at(format!("unknown keyboard {other:?}"))),
            },
            "tick" => Step::Tick(match rest.trim() {
                "" => 1,
                n => n.parse().map_err(|_| at(format!("bad tick count {n:?}")))?,
            }),
            "frame" => Step::Frame(rest.trim().to_string()),
            "clock" => Step::Clock(parse_clock(rest.trim()).map_err(at)?),
            "click" | "wheel" | "drag" => Step::Mouse(parse_mouse(word, rest).map_err(at)?),
            "tier" => match Tier::from_name(rest.trim()) {
                Some(tier) => Step::Tier(tier),
                None => {
                    return Err(at(format!(
                        "unknown tier {:?}, mono, 16, 256 or truecolor",
                        rest.trim()
                    )))
                }
            },
            "theme" => match rest.trim() {
                "dark" => Step::Theme(Theme::Dark),
                "light" => Step::Theme(Theme::Light),
                other => return Err(at(format!("unknown theme {other:?}, dark or light"))),
            },
            "styles" => Step::Styles(rest.trim().to_string()),
            "sleep" => {
                let ms = rest.trim();
                match ms.parse::<u64>() {
                    Ok(ms) if ms <= SLEEP_MAX_MS => Step::Sleep(ms),
                    _ => return Err(at(format!("bad sleep {ms:?}, 0 to {SLEEP_MAX_MS} ms"))),
                }
            }
            "until" => {
                let words: Vec<&str> = rest.split_whitespace().collect();
                match words.as_slice() {
                    ["ready", tail @ ..] => {
                        Step::Until(Until::Ready, secs(tail.first().copied()).map_err(at)?)
                    }
                    ["delivery", state, tail @ ..] => Step::Until(
                        Until::Delivery(parse_state(state).map_err(at)?),
                        secs(tail.first().copied()).map_err(at)?,
                    ),
                    ["leg", leg, tail @ ..] => Step::Until(
                        Until::Leg(match *leg {
                            "direct" => Leg::Direct,
                            "propagated" => Leg::Propagated,
                            other => return Err(at(format!("unknown leg {other:?}"))),
                        }),
                        secs(tail.first().copied()).map_err(at)?,
                    ),
                    ["received", n, tail @ ..] => Step::Until(
                        Until::Received(
                            n.parse()
                                .map_err(|_| at(format!("bad message count {n:?}")))?,
                        ),
                        secs(tail.first().copied()).map_err(at)?,
                    ),
                    ["named", n, tail @ ..] => Step::Until(
                        Until::Named(n.parse().map_err(|_| at(format!("bad peer count {n:?}")))?),
                        secs(tail.first().copied()).map_err(at)?,
                    ),
                    _ => return Err(at(format!("unknown condition {rest:?}"))),
                }
            }
            other => return Err(at(format!("unknown step {other:?}"))),
        };
        steps.push(step);
    }
    Ok(steps)
}

/// The screen of a test backend as text, one row a line, trailing blanks cut.
fn frame_text(backend: &TestBackend) -> String {
    let buffer = backend.buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut row = String::new();
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                row.push_str(cell.symbol());
            }
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

/// A cell's letter in a style map: which token's colours it carries, the
/// first in this order that matches.
///
/// ```text
/// e   the error colour (an error toast's border)
/// t   the toast
/// ^   a selected block's tint, or reverse video (the list's selection, the
///     badge, and the selection and the toast where the tier draws them
///     reversed)
/// .   our own block's tint
/// ,   their block's tint
/// =   the date pill
/// !   the unread rule and its count
///     anything else: the pane
/// ```
///
/// Where two tokens share their colours the earlier letter wins: in 16
/// colours on light the date pill is their block's pair, and draws as `,`.
pub fn style_letter(cell: &ratatui::buffer::Cell, tokens: &Tokens) -> char {
    use leviculum_ui_ratatui::style::ink;
    let fg = |p: leviculum_ui::Paint| p.fg.map(ink);
    let bg = |p: leviculum_ui::Paint| p.bg.map(ink);
    let is = |c: ratatui::style::Color, want: Option<ratatui::style::Color>| want == Some(c);
    if is(cell.fg, fg(tokens.error)) {
        return 'e';
    }
    if is(cell.fg, fg(tokens.toast)) && is(cell.bg, bg(tokens.toast)) {
        return 't';
    }
    if is(cell.bg, bg(tokens.own_selected)) || is(cell.bg, bg(tokens.their_selected)) {
        return '^';
    }
    if is(cell.bg, bg(tokens.own)) {
        return '.';
    }
    if is(cell.bg, bg(tokens.their)) {
        return ',';
    }
    if is(cell.bg, bg(tokens.date_pill)) {
        return '=';
    }
    if is(cell.fg, fg(tokens.unread)) {
        return '!';
    }
    if cell.modifier.contains(Modifier::REVERSED) {
        return '^';
    }
    ' '
}

/// The screen of a test backend as a style map, one row a line, trailing
/// blanks cut.
fn styles_text(backend: &TestBackend, tokens: &Tokens) -> String {
    let buffer = backend.buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let row: String = (0..buffer.area.width)
            .filter_map(|x| buffer.cell((x, y)))
            .map(|cell| style_letter(cell, tokens))
            .collect();
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

/// A key script against the real engine, frames to a file. No tick happens
/// unless the script asks for one (`tick`, or `sleep`'s one per 250 ms of its
/// own length, however long the wall clock took), so a frame is a pure
/// function of the script and the engine's events.
async fn run_script(
    shell: &mut Shell,
    script: &Path,
    frames: &Path,
    options: &TuiOptions,
) -> Result<(), String> {
    let text = std::fs::read_to_string(script).map_err(|e| format!("{}: {e}", script.display()))?;
    let steps = parse_script(&text).map_err(|e| format!("{}: {e}", script.display()))?;
    log_keys(&shell.model);
    // A script asks no terminal: `auto` climbs the ladder on its tier.
    let chosen = map::choose_graphics(options.graphics, None, shell.model.tier);
    let mut painter = map::MapPainter::new(std::mem::take(&mut shell.regions), chosen, None);
    log_graphics(&painter, None);
    shell.apply(UiEvent::MapSetup(painter.setup()));
    let mut out =
        std::fs::File::create(frames).map_err(|e| format!("{}: {e}", frames.display()))?;
    let styles_path = {
        let mut path = frames.as_os_str().to_owned();
        path.push(".styles");
        PathBuf::from(path)
    };
    let mut styles: Option<std::fs::File> = None;
    let (width, height) = options.size;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).map_err(|e| e.to_string())?;
    shell.apply(UiEvent::Resize { width, height });
    shell.apply(UiEvent::Clock(script_clock()));
    let mut count = 0usize;
    // A frame of the model, or without one a line of what the screen asked
    // of the terminal.
    let mut dump = |terminal: &mut Terminal<TestBackend>,
                    painter: &mut map::MapPainter,
                    model: Option<&Model>,
                    label: &str| {
        let Some(model) = model else {
            return writeln!(out, "--- {label} ---")
                .and_then(|()| out.flush())
                .map_err(|e| format!("{}: {e}", frames.display()));
        };
        let view = ui::view(model);
        terminal
            .draw(|frame| draw(frame, &view, Some(painter)))
            .map_err(|e| e.to_string())?;
        count += 1;
        let text = frame_text(terminal.backend());
        write!(out, "--- frame {count}: {label} ---\n{text}")
            .and_then(|()| out.flush())
            .map_err(|e| format!("{}: {e}", frames.display()))
    };
    for step in steps {
        match step {
            Step::Mouse(events) => {
                for event in events {
                    shell.apply(UiEvent::Mouse(event));
                }
            }
            Step::Key(key) => shell.apply(UiEvent::Key(key)),
            Step::Keyboard(enhanced) => shell.model.enhanced = enhanced,
            Step::Tier(tier) => shell.model.tier = tier,
            Step::Theme(theme) => shell.model.theme = theme,
            Step::Styles(label) => {
                let view = ui::view(&shell.model);
                terminal
                    .draw(|frame| draw(frame, &view, Some(&mut painter)))
                    .map_err(|e| e.to_string())?;
                let map = styles_text(terminal.backend(), shell.model.tokens());
                let file = match styles.as_mut() {
                    Some(file) => file,
                    None => styles.insert(
                        std::fs::File::create(&styles_path)
                            .map_err(|e| format!("{}: {e}", styles_path.display()))?,
                    ),
                };
                write!(file, "--- styles: {label} ---\n{map}")
                    .and_then(|()| file.flush())
                    .map_err(|e| format!("{}: {e}", styles_path.display()))?;
            }
            Step::Clock(now) => shell.apply(UiEvent::Clock(now)),
            Step::Type(text) => {
                for c in text.chars() {
                    shell.apply(UiEvent::Key(KeyEvent::new(
                        KeyCode::Char(c),
                        KeyModifiers::NONE,
                    )));
                }
            }
            Step::Tick(n) => {
                for _ in 0..n {
                    shell.apply(UiEvent::Tick);
                }
            }
            Step::Frame(label) => {
                // A script's screen is drawn whole every frame anyway.
                shell.redraw = false;
                dump(&mut terminal, &mut painter, Some(&shell.model), &label)?
            }
            Step::Until(until, within) => {
                let deadline = tokio::time::Instant::now() + within;
                loop {
                    shell.poll();
                    if until.holds(&shell.model, &shell.legs_seen) {
                        break;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        dump(
                            &mut terminal,
                            &mut painter,
                            Some(&shell.model),
                            &format!("timeout: {until:?}"),
                        )?;
                        return Err(format!(
                            "script: {until:?} did not hold within {}s",
                            within.as_secs()
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(PUMP_MS)).await;
                }
            }
            Step::Sleep(ms) => {
                // The ticks are counted on the script's clock, not read off
                // the wall: `sleep 2500` is ten ticks on a loaded host too.
                let mut slept = 0;
                let mut ticked = 0;
                while slept < ms {
                    let step = PUMP_MS.min(ms - slept);
                    tokio::time::sleep(Duration::from_millis(step)).await;
                    slept += step;
                    shell.poll();
                    while ticked + TICK_MS <= slept {
                        ticked += TICK_MS;
                        shell.apply(UiEvent::Tick);
                    }
                }
            }
        }
        for output in std::mem::take(&mut shell.outputs) {
            let line = match output {
                Output::Copy(text) => format!("osc52 {}", osc52(&text).replace('\x1b', "\\e")),
                Output::Open(url) => format!("open {url}"),
                Output::Mouse(on) => format!("mouse {}", if on { "on" } else { "off" }),
            };
            dump(&mut terminal, &mut painter, None, &line)?;
        }
        if shell.model.quit {
            break;
        }
    }
    Ok(())
}

/// Bold for the focused pane's border and title.
fn pane_block(title: &str, focused: bool, borders: Borders) -> Block<'static> {
    let (title, style) = match focused {
        true => (
            format!("● {title}"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        false => (format!("  {title}"), Style::default()),
    };
    Block::default()
        .borders(borders)
        .border_style(style)
        .title(Span::styled(title, style))
}

/// The compose box's border: its top corners join the conversation pane's
/// sides, so the two read as one column.
const COMPOSE_BORDER: border::Set = border::Set {
    top_left: "├",
    top_right: "┤",
    ..border::PLAIN
};

/// Draw the screen: the list left the full height, the conversation and the
/// compose box under it right, the footer at the bottom; the toast, the
/// palette, the detail view and the help on top.
pub fn draw(frame: &mut Frame, view: &View, painter: Option<&mut map::MapPainter>) {
    let (theme, tier) = (view.theme, view.tier);
    let no_color = tier == Tier::Mono;
    let tokens = leviculum_ui::tokens(tier, theme);
    let compose_lines: Vec<&str> = view.compose.split('\n').collect();
    let compose_rows = compose_lines.len().clamp(1, ui::COMPOSE_MAX_ROWS);
    // The model's rectangles, the ones the mouse hit-tests against.
    let area = frame.area();
    let screen = ui::Screen::of(area.width, area.height, compose_rows);
    let left = to_ratatui(screen.list);
    let messages = to_ratatui(screen.messages);
    let compose = to_ratatui(screen.compose);
    let footer_row = to_ratatui(screen.footer);
    let body = Rect {
        height: footer_row.y,
        ..area
    };

    // The list, or the search in its place: its field on top, `esc closes`
    // on its border; `/ search` on the bottom border while none is open.
    let mut block = pane_block(
        &view.conversations_title,
        matches!(view.focus, Pane::Conversations | Pane::Search),
        Borders::ALL,
    );
    if !view.conversations_title_right.is_empty() {
        block = block
            .title_top(Line::from(format!(" {} ", view.conversations_title_right)).right_aligned());
    }
    if !view.list_hint.is_empty() {
        block = block.title_bottom(Line::from(format!(" {} ", view.list_hint)).right_aligned());
    }
    let inner = block.inner(left);
    frame.render_widget(block, left);
    frame.render_widget(Paragraph::new(to_ratatui_text(&view.conversations)), inner);
    let search_cursor = view
        .search_cursor
        .map(|column| Position::new(inner.x + column as u16, inner.y));

    // The model laid the rows out at this pane's size: drawn as they come.
    let mut block = pane_block(
        &view.messages_title,
        matches!(view.focus, Pane::Messages | Pane::Map),
        Borders::TOP | Borders::LEFT | Borders::RIGHT,
    );
    if !view.messages_title_right.is_empty() {
        block =
            block.title_top(Line::from(format!(" {} ", view.messages_title_right)).right_aligned());
    }
    let inner = block.inner(messages);
    frame.render_widget(block, messages);
    match (&view.map, painter) {
        // The map in the conversation's place: the picture, the pins and
        // the attribution, all inside the pane.
        (Some(shown), Some(painter)) => {
            painter.render(frame, shown, Style::default().add_modifier(Modifier::DIM));
        }
        (Some(shown), None) => {
            for pin in &shown.pins {
                let apart = pin.apart.iter().map(|a| (a.col, a.row, &a.text));
                for (col, row, text) in std::iter::once((pin.col, pin.row, &pin.text)).chain(apart)
                {
                    frame.buffer_mut().set_stringn(
                        col,
                        row,
                        text,
                        usize::from(inner.width),
                        Style::default(),
                    );
                }
            }
        }
        (None, _) => {
            // Laid out for a taller pane than this frame has (a resize the
            // model has not heard of yet): the bottom rows, where the newest
            // are.
            let excess = view
                .messages
                .len()
                .saturating_sub(usize::from(inner.height));
            frame.render_widget(
                Paragraph::new(to_ratatui_text(&view.messages[excess..])),
                inner,
            );
        }
    }

    // The compose pane: one row a line, up to five, scrolled to the
    // cursor's line and, sideways, to its column.
    let focused = view.focus == Pane::Compose;
    let block = pane_block(&view.compose_title, focused, Borders::ALL)
        .border_set(COMPOSE_BORDER)
        .title_top(Line::from(format!(" {} ", view.compose_hint)).right_aligned());
    let inner = block.inner(compose);
    frame.render_widget(block, compose);
    let (top, scroll, cursor_line, cursor_column) = ui::compose_window(
        &view.compose,
        view.compose_cursor,
        usize::from(inner.width),
        usize::from(inner.height),
    );
    let shown: Vec<Line> = compose_lines
        .iter()
        .skip(top)
        .take(usize::from(inner.height.max(1)))
        .map(|line| Line::from(line.chars().skip(scroll).collect::<String>()))
        .collect();
    frame.render_widget(Paragraph::new(Text::from(shown)), inner);
    let overlay = view.palette.is_some() || view.help || view.detail.is_some();
    if focused && !overlay {
        frame.set_cursor_position(Position::new(
            inner.x + (cursor_column - scroll) as u16,
            inner.y + (cursor_line - top) as u16,
        ));
    }
    if let (Some(position), false) = (search_cursor, overlay) {
        frame.set_cursor_position(position);
    }

    let slots = footer::layout(view.footer.clone(), screen.footer);
    leviculum_ui_ratatui::footer::render(
        frame,
        footer_row,
        &slots,
        view.footer_hover,
        no_color,
        theme,
    );

    if let Some(note) = &view.toast {
        // Over both columns above the compose box, so it has the screen's
        // width. Wrapped, not cut: a failed auto send's toast names both legs.
        let over = Rect {
            height: messages.height,
            ..body
        };
        let rows = ui::toast_rows(&toast::body(note), toast::max_inner(over));
        toast::render_rows(frame, over, note.kind, &rows, tokens);
    }
    if let Some(detail) = &view.detail {
        draw_detail(frame, detail);
    }
    if let Some(palette) = &view.palette {
        let area = to_ratatui(ui::palette_rect(palette.rows.len(), screen.area()));
        frame.render_widget(Clear, area);
        let block = Block::default().borders(Borders::ALL).title(" commands ");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let mut lines = vec![Line::from(format!("> {}", palette.query))];
        lines.extend(to_ratatui_text(&palette.rows).lines);
        frame.render_widget(Paragraph::new(Text::from(lines)), inner);
        frame.set_cursor_position(Position::new(inner.x + 2 + palette.cursor as u16, inner.y));
    }
    if view.help {
        help::render(
            frame,
            frame.area(),
            &help_groups(),
            " loquax help ",
            0,
            no_color,
            theme,
        );
    }
    // The footer and the help draw lnomad's chrome in RGB; the tokens name
    // their own indexes and pass.
    if let Some(depth) = tier.depth() {
        downgrade_buffer(frame, depth, false);
    }
}

/// A message's detail view, centred over the panes, its rows wrapped.
fn draw_detail(frame: &mut Frame, detail: &ui::DetailView) {
    let (area, rows) = ui::detail_layout(detail, from_rect(frame.area()));
    let area = to_ratatui(area);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", detail.title))
        .title_bottom(Line::from(" Esc closes ").right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines: Vec<Line> = rows.into_iter().map(Line::from).collect();
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn from_rect(rect: Rect) -> leviculum_ui::Rect {
    leviculum_ui::Rect::new(rect.x, rect.y, rect.width, rect.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_parses_every_step_kind() {
        let steps = parse_script(
            "# a comment\n\nkey tab\nkey ctrl-p\nkey G\ntype hello world\ntick 3\n\
             frame after typing\nuntil ready 5\nuntil delivery delivered\nuntil received 1 9\n",
        )
        .expect("parses");
        assert_eq!(steps.len(), 9);
        assert!(matches!(
            steps[1],
            Step::Key(KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            })
        ));
        assert!(matches!(&steps[3], Step::Type(t) if t == "hello world"));
        assert!(matches!(&steps[5], Step::Frame(l) if l == "after typing"));
        assert!(matches!(
            steps[7],
            Step::Until(Until::Delivery(DeliveryState::Delivered), _)
        ));
        assert!(parse_script("key nope\n").is_err());
        assert!(parse_script("until weather\n").is_err());
    }

    #[test]
    fn sleep_and_until_leg_parse_and_sleep_is_capped() {
        let steps =
            parse_script("sleep 2500\nuntil leg direct 5\nuntil leg propagated\n").expect("parses");
        assert!(matches!(steps[0], Step::Sleep(2500)));
        assert!(matches!(
            steps[1],
            Step::Until(Until::Leg(Leg::Direct), d) if d == Duration::from_secs(5)
        ));
        assert!(matches!(
            steps[2],
            Step::Until(Until::Leg(Leg::Propagated), d) if d == Duration::from_secs(UNTIL_SECS)
        ));
        assert!(parse_script("sleep 60000\n").is_ok());
        assert!(parse_script("sleep 60001\n").is_err());
        assert!(parse_script("sleep\n").is_err());
        assert!(parse_script("until leg sideways\n").is_err());
    }

    #[test]
    fn until_named_parses_a_count_and_refuses_nonsense() {
        let steps = parse_script("until named 2 9\nuntil named 1\n").expect("parses");
        assert!(matches!(
            steps[0],
            Step::Until(Until::Named(2), d) if d == Duration::from_secs(9)
        ));
        assert!(matches!(
            steps[1],
            Step::Until(Until::Named(1), d) if d == Duration::from_secs(UNTIL_SECS)
        ));
        assert!(parse_script("until named\n").is_err());
        assert!(parse_script("until named some\n").is_err());
    }

    /// `clock` names a local date and time at UTC; nonsense is refused.
    #[test]
    fn the_clock_step_parses_a_date_and_a_time() {
        let steps = parse_script("clock 2026-10-24 13:58\n").expect("parses");
        assert!(matches!(
            steps[0],
            Step::Clock(t) if t == WallTime::from_civil(2026, 10, 24, 13, 58, 0)
        ));
        for bad in [
            "clock 2026-10-24\n",
            "clock 2026-13-01 10:00\n",
            "clock 2026-10-24 24:00\n",
            "clock today noon\n",
        ] {
            assert!(parse_script(bad).is_err(), "{bad}");
        }
    }

    /// `until leg` holds for the newest message we wrote, not an older one.
    #[test]
    fn until_leg_reads_the_newest_sent_message() {
        let mut model = Model::new(&[[0xab; 16]]);
        for name in ["tab", "tab", "a", "enter", "b", "enter"] {
            update(&mut model, UiEvent::Key(parse_key(name).expect("key")));
        }
        let until = Until::Leg(Leg::Direct);
        assert!(!until.holds(&model, &[((0, 0), Leg::Direct)]));
        assert!(!until.holds(&model, &[((0, 1), Leg::Propagated)]));
        assert!(until.holds(&model, &[((0, 1), Leg::Direct)]));
    }

    fn render(model: &Model) -> String {
        let mut model = model.clone();
        update(
            &mut model,
            UiEvent::Resize {
                width: 80,
                height: 16,
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
        let view = ui::view(&model);
        terminal
            .draw(|frame| draw(frame, &view, None))
            .expect("draw");
        frame_text(terminal.backend())
    }

    /// The style map of a model drawn 80 x 16 in a tier and theme.
    fn styles(model: &Model, tier: Tier, theme: Theme) -> String {
        let mut model = model.clone();
        model.tier = tier;
        model.theme = theme;
        update(
            &mut model,
            UiEvent::Resize {
                width: 80,
                height: 16,
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
        let view = ui::view(&model);
        terminal
            .draw(|frame| draw(frame, &view, None))
            .expect("draw");
        styles_text(terminal.backend(), model.tokens())
    }

    /// An error toast's border is the error colour and its inside the toast's;
    /// where the toast is reverse video, all of it is.
    #[test]
    fn an_error_toast_maps_its_border_to_the_error_letter() {
        let mut model = Model::new(&[[0xab; 16]]);
        model.toasts.push(leviculum_ui::toast::Toast {
            kind: leviculum_ui::toast::ToastKind::Error,
            text: "fetch failed".into(),
            shown_at: 0,
        });
        for theme in [Theme::Dark, Theme::Light] {
            for tier in [Tier::Truecolor, Tier::Ansi256] {
                let map = styles(&model, tier, theme);
                assert!(map.contains("eeeeeeeee"), "{tier:?} {theme:?}:\n{map}");
                assert!(map.contains("ettttt"), "{tier:?} {theme:?}:\n{map}");
            }
            let map = styles(&model, Tier::Ansi16, theme);
            assert!(
                !map.contains('e') && map.contains("^^^^^"),
                "16 {theme:?}:\n{map}"
            );
        }
        let map = styles(&model, Tier::Mono, Theme::Dark);
        assert!(map.chars().all(|c| matches!(c, ' ' | '^' | '\n')), "{map}");
    }

    #[test]
    fn a_script_sets_the_tier_and_the_theme_and_asks_for_a_style_map() {
        let steps =
            parse_script("tier 256\ntheme light\nstyles both sides\ntier mono\n").expect("parses");
        assert!(matches!(steps[0], Step::Tier(Tier::Ansi256)));
        assert!(matches!(steps[1], Step::Theme(Theme::Light)));
        assert!(matches!(&steps[2], Step::Styles(l) if l == "both sides"));
        assert!(matches!(steps[3], Step::Tier(Tier::Mono)));
        assert!(parse_script("tier 8\n").is_err());
        assert!(parse_script("theme solarized\n").is_err());
    }

    #[test]
    fn the_three_panes_and_the_footer_are_drawn_with_the_focus_marked() {
        let model = Model::new(&[[0xab; 16]]);
        let screen = render(&model);
        assert!(screen.contains("● loquax (attaching)"), "{screen}");
        assert!(screen.contains("▌<abababab…>"), "{screen}");
        assert!(screen.contains(ui::layout::EARLIEST), "{screen}");
        assert!(screen.contains("  to <abababab…>"), "{screen}");
        assert!(screen.contains("Tab pane"), "{screen}");
    }

    #[test]
    fn ctrl_c_in_compose_keeps_the_line_for_ctrl_y() {
        let mut model = Model::new(&[[0xab; 16]]);
        for name in ["tab", "tab", "h", "i", "ctrl-c"] {
            update(&mut model, UiEvent::Key(parse_key(name).expect("key")));
        }
        assert!(!model.quit);
        assert_eq!(model.compose.value(), "");
        update(&mut model, UiEvent::Key(parse_key("ctrl-y").expect("key")));
        assert_eq!(model.compose.value(), "hi");
    }

    /// The enhanced chords and the modifier prefixes parse, in any order of
    /// prefixes, and the keyboard step names the world.
    #[test]
    fn script_keys_spell_the_enhanced_chords() {
        let parsed = |name: &str| parse_key(name).expect(name);
        assert_eq!(
            parsed("shift-enter"),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
        );
        assert_eq!(
            parsed("ctrl-enter"),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)
        );
        assert_eq!(
            parsed("alt-<"),
            KeyEvent::new(KeyCode::Char('<'), KeyModifiers::ALT)
        );
        assert_eq!(
            parsed("ctrl-shift-T"),
            KeyEvent::new(
                KeyCode::Char('T'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )
        );
        assert_eq!(
            parsed("f5"),
            KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE)
        );
        assert_eq!(
            parsed("f"),
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE)
        );
        assert_eq!(
            parsed("-"),
            KeyEvent::new(KeyCode::Char('-'), KeyModifiers::NONE)
        );
        assert!(parse_key("ctrl-").is_err());
        assert!(parse_key("f13").is_err());
        let steps = parse_script("keyboard enhanced\nkeyboard plain\n").expect("parses");
        assert!(matches!(steps[0], Step::Keyboard(true)));
        assert!(matches!(steps[1], Step::Keyboard(false)));
        assert!(parse_script("keyboard fancy\n").is_err());
    }

    /// In the terminal's field the Emacs line keys are line motions: after a
    /// `Ctrl-j`, `Ctrl-a` goes to the second line's start, `Ctrl-p` up to
    /// the first, `Alt->` to the body's end.
    #[test]
    fn compose_line_keys_move_by_line_in_the_real_field() {
        let mut model = Model::new(&[[0xab; 16]]);
        for name in ["tab", "tab", "a", "b", "ctrl-j", "c", "d", "ctrl-a"] {
            update(&mut model, UiEvent::Key(parse_key(name).expect("key")));
        }
        assert_eq!(model.compose.value(), "ab\ncd");
        assert_eq!(model.compose.cursor(), 3);
        update(&mut model, UiEvent::Key(parse_key("ctrl-p").expect("key")));
        assert_eq!(model.compose.cursor(), 0);
        assert!(model.palette.is_none());
        update(&mut model, UiEvent::Key(parse_key("alt->").expect("key")));
        assert_eq!(model.compose.cursor(), 5);
        update(&mut model, UiEvent::Key(parse_key("ctrl-n").expect("key")));
        assert_eq!(model.compose.cursor(), 5, "no line below the last");
    }

    /// The compose pane's border carries the live chords.
    #[test]
    fn the_compose_border_shows_the_live_hint() {
        let mut model = Model::new(&[[0xab; 16]]);
        assert!(
            render(&model).contains("Enter send · Alt-Enter newline"),
            "{}",
            render(&model)
        );
        model.enhanced = true;
        assert!(render(&model).contains("Enter send · Shift-Enter newline"));
    }

    /// `click`, `wheel` and `drag` become the events a terminal sends, at
    /// cells counted from 1 as the frames are; nonsense is refused.
    #[test]
    fn the_mouse_steps_parse_to_events_at_one_based_cells() {
        let steps = parse_script("click 3 4\nwheel down 10 2 3\nwheel up 1 1\ndrag 2 2 9 5\n")
            .expect("parses");
        let events = |step: &Step| match step {
            Step::Mouse(events) => events.iter().map(|e| (e.kind, e.column, e.row)).collect(),
            _ => Vec::new(),
        };
        let left = MouseButton::Left;
        assert_eq!(
            events(&steps[0]),
            [
                (MouseEventKind::Down(left), 2, 3),
                (MouseEventKind::Up(left), 2, 3)
            ]
        );
        assert_eq!(events(&steps[1]), [(MouseEventKind::ScrollDown, 9, 1); 3]);
        assert_eq!(events(&steps[2]), [(MouseEventKind::ScrollUp, 0, 0)]);
        assert_eq!(
            events(&steps[3]),
            [
                (MouseEventKind::Down(left), 1, 1),
                (MouseEventKind::Drag(left), 8, 4),
                (MouseEventKind::Up(left), 8, 4)
            ]
        );
        for bad in [
            "click 0 1\n",
            "click 1\n",
            "wheel sideways 1 1\n",
            "wheel up 1 1 many\n",
            "drag 1 1 2\n",
        ] {
            assert!(parse_script(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_newline_step_is_alt_enter() {
        let steps = parse_script("newline\n").expect("parses");
        assert!(matches!(
            steps[0],
            Step::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::ALT,
                ..
            })
        ));
    }

    /// The compose pane grows by a row per line, up to five, and a message
    /// longer than its block wraps at a space inside it.
    #[test]
    fn compose_grows_with_its_lines_and_a_long_message_wraps() {
        let mut model = Model::new(&[[0xab; 16]]);
        for name in ["tab", "tab", "a"] {
            update(&mut model, UiEvent::Key(parse_key(name).expect("key")));
        }
        for c in ['b', 'c', 'd', 'e', 'f', 'g'] {
            leviculum_ui::input::InputBuffer::newline(&mut model.compose);
            update(
                &mut model,
                UiEvent::Key(parse_key(&c.to_string()).expect("key")),
            );
        }
        let screen = render(&model);
        let rows: Vec<&str> = screen.lines().collect();
        let top = rows
            .iter()
            .position(|row| row.contains("● to <abababab…>"))
            .expect("the compose pane");
        // Five rows of text between the borders, the last five lines.
        let inner: Vec<&str> = rows[top + 1..top + 6]
            .iter()
            .map(|row| row.trim_matches(|c: char| c == '│' || c == ' '))
            .collect();
        assert_eq!(inner, ["c", "d", "e", "f", "g"], "{screen}");
        assert!(rows[top + 6].starts_with('└'), "{screen}");

        let mut model = Model::new(&[[0xab; 16]]);
        for name in ["tab", "tab"] {
            update(&mut model, UiEvent::Key(parse_key(name).expect("key")));
        }
        for c in "word ".repeat(20).chars() {
            update(
                &mut model,
                UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
            );
        }
        update(&mut model, UiEvent::Key(parse_key("enter").expect("key")));
        let screen = render(&model);
        // The conversation pane's side of each row, past the list.
        let block: Vec<String> = screen
            .lines()
            .map(|row| row.chars().skip(26).collect::<String>())
            .filter(|row| row.contains("word"))
            .collect();
        assert!(block.len() > 1, "{screen}");
        for row in &block {
            let inside = row.trim_matches(|c: char| c == '│' || c == ' ');
            assert!(inside.starts_with("word"), "{inside:?}");
            assert!(
                inside.ends_with("word") || inside.ends_with('○'),
                "{inside:?}"
            );
        }
    }

    /// A send that failed both ways: at 80x24 the toast wraps to three rows
    /// and both legs' reasons are on screen, the frame pinned in
    /// `tests/fixtures/tui_toast_two_legs.frame`.
    #[test]
    fn a_two_leg_failure_toast_wraps_and_shows_both_legs() {
        let mut model = Model::new(&[[0xab; 16]]);
        for name in ["tab", "tab", "h", "i", "enter"] {
            update(&mut model, UiEvent::Key(parse_key(name).expect("key")));
        }
        let direct = SendError::Unreachable {
            destination: [0xab; 16],
            waited: Duration::from_secs(30),
        };
        let fallback = SendError::NoPropagationNode(
            "no mailbox chosen: no propagation node has been heard".to_string(),
        );
        update(
            &mut model,
            UiEvent::SendDone(Err(format!("direct: {direct}\npropagated: {fallback}"))),
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        let view = ui::view(&model);
        terminal
            .draw(|frame| draw(frame, &view, None))
            .expect("draw");
        let screen = frame_text(terminal.backend());
        assert!(screen.contains("not sent: direct: no route to"), "{screen}");
        assert!(
            screen.contains("propagated: no propagation node"),
            "{screen}"
        );
        let expected = include_str!("../tests/fixtures/tui_toast_two_legs.frame");
        if screen != expected {
            let kept = std::env::temp_dir().join("loquax-tui-toast-two-legs.frame");
            let _ = std::fs::write(&kept, &screen);
            panic!(
                "the frame moved; this run's is in {}, the pinned one in \
                 loquax/tests/fixtures/tui_toast_two_legs.frame\n{screen}",
                kept.display()
            );
        }
    }

    #[test]
    fn the_help_and_the_palette_draw_over_the_panes() {
        let mut model = Model::new(&[[0xab; 16]]);
        update(&mut model, UiEvent::Key(parse_key("?").expect("key")));
        assert!(render(&model).contains("loquax help"));
        update(&mut model, UiEvent::Key(parse_key("esc").expect("key")));
        update(&mut model, UiEvent::Key(parse_key(":").expect("key")));
        let screen = render(&model);
        assert!(screen.contains("commands"), "{screen}");
        assert!(screen.contains("▸ send"), "{screen}");
        assert!(screen.contains("focus compose"), "{screen}");
    }

    use leviculum_lxmf::router::MessageState;
    use loquax::outbox::OutboxEvent;

    /// The 80 x 24 mockup's story with Alice and Bob: three days with Alice,
    /// both sides, a mailbox and a sent-unproven mark, Bob opened while Alice
    /// writes twice, Alice opened again at her `2 new`, and a reply on its
    /// way.
    fn mockup(width: u16, height: u16) -> Model {
        let alice = [0x4f; 16];
        let bob = [0xb7; 16];
        let mut model = Model::new(&[alice, bob]);
        update(&mut model, UiEvent::Resize { width, height });
        update(
            &mut model,
            UiEvent::Engine(OutboxEvent::Ready {
                address: [0xaf; 16],
            }),
        );
        update(
            &mut model,
            UiEvent::MailboxChosen {
                node: [0x9c; 16],
                by_user: false,
            },
        );
        let clock = |m: &mut Model, d, h, mi| {
            update(
                m,
                UiEvent::Clock(WallTime::from_civil(2026, 10, d, h, mi, 3600)),
            );
        };
        let mut n = 0u8;
        let mut recv = |m: &mut Model, from: [u8; 16], body: &str| {
            n += 1;
            update(
                m,
                UiEvent::Engine(OutboxEvent::Received {
                    message_id: [n; 32],
                    source: from,
                    title: Vec::new(),
                    body: body.as_bytes().to_vec(),
                    fields: Vec::new(),
                    verified: true,
                }),
            );
        };
        let send = |m: &mut Model, body: &str, id: u8, state: MessageState| {
            m.focus = Pane::Compose;
            for c in body.chars() {
                update(
                    m,
                    UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                );
            }
            update(
                m,
                UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            );
            update(
                m,
                UiEvent::Engine(OutboxEvent::Queued {
                    message_id: [id; 32],
                }),
            );
            update(
                m,
                UiEvent::Engine(OutboxEvent::State {
                    message_id: [id; 32],
                    state,
                }),
            );
            update(m, UiEvent::SendDone(Ok(())));
        };
        clock(&mut model, 23, 19, 40);
        recv(&mut model, alice, "did the board come back from the soak?");
        clock(&mut model, 23, 19, 52);
        send(
            &mut model,
            "yes, 100 % over the night, log is on the share",
            0xa1,
            MessageState::Delivered,
        );
        clock(&mut model, 24, 13, 58);
        recv(&mut model, alice, "are you at the field site tomorrow? Dana says the north gate is locked after six, so the car park by the road it is.");
        clock(&mut model, 24, 14, 2);
        send(&mut model, "yes, from ten. Shall I bring the T114 and the long antenna, or is the stick enough for the hill?", 0xa2, MessageState::Delivered);
        clock(&mut model, 24, 14, 3);
        recv(&mut model, alice, "long one, the hill eats signal");
        clock(&mut model, 26, 9, 12);
        send(
            &mut model,
            "on my way",
            0xa3,
            MessageState::AwaitingCollection,
        );
        clock(&mut model, 26, 9, 31);
        send(
            &mut model,
            "stuck at the level crossing",
            0xa4,
            MessageState::Sent,
        );
        clock(&mut model, 26, 12, 40);
        recv(&mut model, bob, "the antenna is in the van");
        // Bob is opened, Alice writes meanwhile.
        model.focus = Pane::Conversations;
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)),
        );
        clock(&mut model, 26, 14, 4);
        recv(&mut model, alice, "ok, bring the T114.");
        clock(&mut model, 26, 14, 5);
        recv(
            &mut model,
            alice,
            "and the small tripod\nif it fits in the bag",
        );
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)),
        );
        clock(&mut model, 26, 14, 6);
        send(&mut model, "the tripod fits", 0xa5, MessageState::Sending);
        model.toasts.clear();
        model
    }

    fn screen(model: &Model, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let view = ui::view(model);
        terminal
            .draw(|frame| draw(frame, &view, None))
            .expect("draw");
        frame_text(terminal.backend())
    }

    /// The two mockups of section 5 drawn from a model: the same elements in
    /// the same rows (the text differs), then the cursor on a sent message
    /// in the conversation, its detail view, and the narrow fallback. Pinned
    /// in `tests/fixtures/tui_layout.frames`.
    #[test]
    fn the_layout_draws_the_mockups_structure_at_both_sizes() {
        let mut frames = String::new();
        for (width, height) in [(80, 24), (120, 40)] {
            let model = mockup(width, height);
            let shot = screen(&model, width, height);
            frames.push_str(&format!("--- {width} x {height} ---\n{shot}"));
            let rows: Vec<&str> = shot.lines().collect();
            // The list: Alice on top with the reply's time and the bar, Bob
            // under her; the header with the trust state.
            assert!(rows[1].starts_with("│▌<4f4f4f4f…>"), "{shot}");
            assert!(rows[2].starts_with("│▌you: the tripod fits"), "{shot}");
            assert!(rows[4].starts_with("│ <b7b7b7b7…>"), "{shot}");
            assert!(rows[0].contains("<4f4f4f4f…>  unknown"), "{shot}");
            // The rule, the date line, ours right, theirs left.
            assert!(shot.contains("── Today ──"), "{shot}");
            assert!(shot.contains(" 2 new "), "{shot}");
            assert!(shot.contains("│ ok, bring the T114.  14:04 "), "{shot}");
            assert!(shot.contains(" the tripod fits  14:06 ⠋ │"), "{shot}");
            assert!(shot.contains("to <4f4f4f4f…>"), "{shot}");
        }
        let mut model = mockup(80, 24);
        model.focus = Pane::Messages;
        for _ in 0..6 {
            update(&mut model, UiEvent::Key(parse_key("k").expect("key")));
        }
        frames.push_str(&format!("--- cursor ---\n{}", screen(&model, 80, 24)));
        update(&mut model, UiEvent::Key(parse_key("enter").expect("key")));
        let detail = screen(&model, 80, 24);
        assert!(detail.contains("delivered: proof received"), "{detail}");
        frames.push_str(&format!("--- detail ---\n{detail}"));
        let model = mockup(60, 20);
        frames.push_str(&format!(
            "--- 60 x 20, narrow ---\n{}",
            screen(&model, 60, 20)
        ));
        let expected = include_str!("../tests/fixtures/tui_layout.frames");
        if frames != expected {
            let kept = std::env::temp_dir().join("loquax-tui-layout.frames");
            let _ = std::fs::write(&kept, &frames);
            panic!(
                "the frames moved; this run's are in {}, the pinned ones in \
                 loquax/tests/fixtures/tui_layout.frames\n{frames}",
                kept.display()
            );
        }
    }

    /// The search of section 5's 120 x 40 mockup, drawn from the model at
    /// both sizes: the field and the messages group as `tripod` is typed,
    /// the third result opened with its match marked in place, both groups
    /// for a query that is a name and a word of a message, and the list back
    /// after `Esc`. Pinned in `tests/fixtures/tui_search.frames`.
    #[test]
    fn the_search_draws_the_mockups_structure_at_both_sizes() {
        let keys = |model: &mut Model, names: &[&str]| {
            for name in names {
                update(model, UiEvent::Key(parse_key(name).expect("key")));
            }
        };
        let typed = |model: &mut Model, text: &str| {
            for c in text.chars() {
                update(
                    model,
                    UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                );
            }
        };
        let mut frames = String::new();
        for (width, height) in [(120, 40), (80, 24)] {
            let mut model = mockup(width, height);
            update(
                &mut model,
                UiEvent::Engine(OutboxEvent::Received {
                    message_id: [0x7b; 32],
                    source: [0xb7; 16],
                    title: Vec::new(),
                    body: b"the tripod is with 4f4f4f4f".to_vec(),
                    fields: Vec::new(),
                    verified: true,
                }),
            );
            model.toasts.clear();
            keys(&mut model, &["esc", "esc", "/"]);
            typed(&mut model, "tripod");
            let shot = screen(&model, width, height);
            assert!(shot.contains("● Search"), "{shot}");
            assert!(shot.contains(" esc closes "), "{shot}");
            assert!(shot.contains("│ / tripod"), "{shot}");
            assert!(shot.contains("│ MESSAGES"), "{shot}");
            assert!(shot.contains("Esc close search"), "{shot}");
            frames.push_str(&format!("--- {width} x {height}: typed ---\n{shot}"));
            keys(&mut model, &["tab", "j", "j", "enter"]);
            let shot = screen(&model, width, height);
            assert!(shot.contains("▌and the small «tripod»"), "{shot}");
            assert!(shot.contains(" the «tripod» fits "), "{shot}");
            frames.push_str(&format!("--- {width} x {height}: opened ---\n{shot}"));
            keys(&mut model, &["/"]);
            for _ in 0.."tripod".len() {
                keys(&mut model, &["backspace"]);
            }
            typed(&mut model, "4f4f");
            let shot = screen(&model, width, height);
            assert!(shot.contains("│ CONVERSATIONS"), "{shot}");
            assert!(shot.contains("│ MESSAGES"), "{shot}");
            assert!(shot.contains("<«4f4f»4f4f…>"), "{shot}");
            frames.push_str(&format!("--- {width} x {height}: both groups ---\n{shot}"));
            keys(&mut model, &["esc"]);
            let shot = screen(&model, width, height);
            assert!(shot.contains(" / search "), "{shot}");
            assert!(!shot.contains('«'), "{shot}");
            frames.push_str(&format!("--- {width} x {height}: closed ---\n{shot}"));
        }
        let expected = include_str!("../tests/fixtures/tui_search.frames");
        if frames != expected {
            let kept = std::env::temp_dir().join("loquax-tui-search.frames");
            let _ = std::fs::write(&kept, &frames);
            panic!(
                "the frames moved; this run's are in {}, the pinned ones in \
                 loquax/tests/fixtures/tui_search.frames\n{frames}",
                kept.display()
            );
        }
    }

    /// The text a frame shows in `cells`, in order.
    fn under(frame: &str, cells: &[(u16, u16)]) -> String {
        let rows: Vec<Vec<char>> = frame.lines().map(|row| row.chars().collect()).collect();
        cells
            .iter()
            .map(|&(col, row)| {
                rows.get(usize::from(row))
                    .and_then(|r| r.get(usize::from(col)))
                    .copied()
                    .unwrap_or(' ')
            })
            .collect()
    }

    /// The hit boxes as the drawn frame shows them, at both mockup sizes:
    /// the cells the model calls `send` read `send` on the compose box's
    /// border, the mailbox state's read the mailbox words, a badge's its
    /// count and a mark's its glyph.
    #[test]
    fn the_hit_boxes_are_the_cells_of_their_words() {
        use loquax::ui::hit::{target_at, Target};
        for (width, height) in [(80, 24), (120, 40)] {
            let mut model = mockup(width, height);
            // Bob's conversation gets a badge.
            update(
                &mut model,
                UiEvent::Engine(OutboxEvent::Received {
                    message_id: [0x77; 32],
                    source: [0xb7; 16],
                    title: Vec::new(),
                    body: b"one for the badge".to_vec(),
                    fields: Vec::new(),
                    verified: true,
                }),
            );
            model.toasts.clear();
            let shot = screen(&model, width, height);
            let mut map: Vec<(Target, Vec<(u16, u16)>)> = Vec::new();
            for row in 0..height {
                for col in 0..width {
                    let target = target_at(&model, col, row);
                    match map.iter_mut().find(|(t, _)| *t == target) {
                        Some((_, cells)) => cells.push((col, row)),
                        None => map.push((target, vec![(col, row)])),
                    }
                }
            }
            let of = |want: &dyn Fn(&Target) -> bool| -> Vec<(u16, u16)> {
                map.iter()
                    .filter(|(t, _)| want(t))
                    .flat_map(|(_, cells)| cells.clone())
                    .collect()
            };
            let send = of(&|t| *t == Target::Send);
            assert_eq!(under(&shot, &send), "send", "{width}: {shot}");
            assert!(send.iter().all(|&(_, row)| row == send[0].1));
            let mailbox = of(&|t| *t == Target::Mailbox);
            assert_eq!(
                under(&shot, &mailbox),
                "mailbox 9c9c9c9c adopted",
                "{width}: {shot}"
            );
            let badge = of(&|t| matches!(t, Target::Badge(_)));
            assert_eq!(under(&shot, &badge), "[1]", "{width}: {shot}");
            let marks = of(&|t| matches!(t, Target::Block { mark: true, .. }));
            let glyphs = under(&shot, &marks);
            assert!(!glyphs.is_empty(), "{width}: {shot}");
            assert!(
                glyphs.chars().all(|c| "○◇⌂✓✗⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(c)),
                "{width}: {glyphs:?}\n{shot}"
            );
        }
    }

    /// A message held from a direct delivery, older than the window the
    /// restart loads, arriving again from the mailbox after the restart
    /// (leviculum 1070, the sender's fallback of 1069): one row in the
    /// store, no second block on the screen. The screen's own check sees
    /// only what is loaded; the store's sees every row.
    #[test]
    fn a_mailbox_copy_of_a_message_below_the_loaded_window_is_not_kept_twice() {
        use loquax::ui::history::LOAD_WINDOW;
        use std::collections::BTreeMap;

        let us = [0x01; 16];
        let alice = [0xa1; 16];
        let direct = [0xd1; 32];
        let from_alice = |id: [u8; 32], body: &str| {
            UiEvent::Engine(OutboxEvent::Received {
                message_id: id,
                source: alice,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                fields: Vec::new(),
                verified: true,
            })
        };
        let home = tempfile::tempdir().expect("home");

        // The first run: the direct copy, then a window's worth after it.
        let mut store = Store::open_in(home.path(), us).expect("open");
        let mut model = Model::new(&[]);
        update(&mut model, from_alice(direct, "the direct copy"));
        for n in 0..LOAD_WINDOW {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&(n as u64).to_be_bytes());
            update(&mut model, from_alice(id, &format!("later {n}")));
        }
        store.write(&model.take_writes()).expect("write");
        drop(store);

        // The restart, as `run` makes it, and the mailbox copy.
        let mut store = Store::open_in(home.path(), us).expect("reopen");
        let mut model = Model::new(&[]);
        model.restore(store.restore().expect("restore"));
        model.activity = model.activity.max(store.last_key().expect("last key"));
        assert!(
            model.conversations[0]
                .messages
                .iter()
                .all(|m| m.id != Some(direct)),
            "the direct copy lies below the loaded window"
        );
        let again = from_alice(direct, "the direct copy");
        let UiEvent::Engine(event) = &again else {
            unreachable!("an engine event")
        };
        // What `Shell::poll` asks before it folds.
        if !already_held(&store, event) {
            update(&mut model, again);
        }
        store.write(&model.take_writes()).expect("write");

        let everything: BTreeMap<[u8; 16], u64> = [(alice, u64::MAX)].into_iter().collect();
        let rows = store
            .search("the direct copy", &everything)
            .expect("search");
        assert_eq!(rows.len(), 1, "one row: {rows:?}");
        assert_eq!(
            model.conversations[0].messages.len(),
            LOAD_WINDOW,
            "no second block"
        );
    }

    /// Ten pans queued behind the one that woke the loop, as a held arrow
    /// leaves them while a frame is drawn, are applied in one go: the next
    /// frame is the only one, at the sum of all ten (1075).
    #[test]
    fn ten_queued_pans_are_one_frame_at_their_sum() {
        let mut model = Model::new(&[[0x22; 16]]);
        let mut painter = map::MapPainter::new(Vec::new(), map::Graphics::Braille, None);
        update(&mut model, UiEvent::MapSetup(painter.setup()));
        update(
            &mut model,
            UiEvent::Resize {
                width: 80,
                height: 24,
            },
        );
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Char('M'), KeyModifiers::NONE)),
        );
        let start = model.map.expect("the map is open");
        let mut want = ui::map::viewport(&start, model.map_area(), &model.map_setup);
        let (w, _) = want.size();
        for _ in 0..10 {
            want.pan((w / 8) as i32, 0);
        }
        let right = || {
            Ok(Event::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Right,
                crossterm::event::KeyModifiers::NONE,
            )))
        };
        // Nine waiting behind the first, then nothing more: the drain must
        // not wait for an eleventh.
        let mut events =
            futures::stream::iter((0..9).map(|_| right())).chain(futures::stream::pending());
        let mut applied = 0;
        let ended = apply_waiting(Some(right()), &mut events, |event| {
            applied += 1;
            update(&mut model, event);
        });
        assert!(ended.is_none(), "the stream goes on");
        assert_eq!(applied, 10);
        let state = model.map.expect("open");
        assert_eq!(state.centre, want.centre, "ten eighths east");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        let Ok(_) = terminal.draw(|f| draw(f, &ui::view(&model), Some(&mut painter)));
        assert_eq!(painter.paints(), 1, "one frame for the ten");
        assert_eq!(painter.painted_view(), Some(want));
    }
}
