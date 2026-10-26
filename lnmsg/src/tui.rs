//! `lnmsg tui`: the terminal frontend over the core's screen model
//! ([`lnmsg::ui`]), slice one (`docs/src/concepts/lnmsg-ui.md`).
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
//!   one job at a time on a [`Tap`]; the screen reads the same events.
//! - [`run`] is the loop: the real terminal (`run_terminal`), or a key script
//!   that dumps frames to a file (`run_script`), which is how the snapshot
//!   test in `tests/tui_snapshot.rs` drives a session against a daemon.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tui_input::Input;

use leviculum_ui::delivery::DeliveryState;
use leviculum_ui::footer;
use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use leviculum_ui::theme::{resolve_theme, Theme, ThemeFlag};
use leviculum_ui_ratatui::background::detect_background;
use leviculum_ui_ratatui::event::{key_event, step_event, EventStep};
use leviculum_ui_ratatui::terminal::install_panic_hook;
use leviculum_ui_ratatui::{help, to_ratatui_text, toast, CrosstermTerminal, TerminalGuard};

use lnmsg::clock::Tokio;
use lnmsg::effects::{Effects, Terminal as Files};
use lnmsg::engine::{attach, AttachConfig, Attached};
use lnmsg::fanout::{Fanout, Tap};
use lnmsg::fetch::{run_fetch, FetchOptions};
use lnmsg::mailbox_state::Stored;
use lnmsg::outbox::{SendRequest, Via};
use lnmsg::seen::{SeenStore, SeenWrite};
use lnmsg::send::{run_send_auto, AutoError, Leg, SendError, SendOptions};
use lnmsg::ui::{self, help_groups, update, Pane, UiEffect, UiEvent, UiModel, View};
use lnmsg::{display_name, identity};

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
/// The compose pane grows with its text up to this many rows, then scrolls.
const COMPOSE_MAX_ROWS: usize = 5;
/// `tui.log`'s filter without `RUST_LOG`.
const TUI_LOG_FILTER: &str = "warn,lnmsg=info";

/// Everything `lnmsg tui` was told on the command line.
pub struct TuiOptions {
    pub to: Vec<[u8; 16]>,
    pub pn: Option<String>,
    pub instance: Option<String>,
    pub config: Option<PathBuf>,
    pub timeout: u64,
    pub ascii: bool,
    pub no_color: bool,
    pub script: Option<PathBuf>,
    pub frames: Option<PathBuf>,
    pub size: (u16, u16),
}

/// Effects for the runners and the stamp worker: their lines become toasts,
/// their files are the CLI's, and a fetched message is not printed, because
/// the screen already has it from the engine's `Received`.
#[derive(Clone)]
struct ScreenEffects {
    notices: UnboundedSender<String>,
    /// The send runner's leg changes, for the running send's row.
    legs: UnboundedSender<Leg>,
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
        Files::new(self.files.clone()).save_mailbox(stored)
    }

    fn write_seen(&mut self, write: &SeenWrite) -> Result<(), String> {
        Files::new(self.files.clone()).write_seen(write)
    }

    fn unix_now_ms(&self) -> u64 {
        lnmsg::mailbox_state::unix_now_ms()
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
}

enum Job {
    Send { to: [u8; 16], body: String },
    Fetch,
}

/// The engine, the fan-out, the job queue, and the model they feed.
struct Shell {
    model: Model,
    attached: Attached,
    fanout: Fanout,
    context: RunContext,
    jobs: VecDeque<Job>,
    running: bool,
    /// Every leg a runner reported, with the message it was for (the
    /// model's conversation and message index): a script's `until leg`.
    legs_seen: Vec<((usize, usize), Leg)>,
    done_tx: UnboundedSender<UiEvent>,
    done_rx: UnboundedReceiver<UiEvent>,
    notices_rx: UnboundedReceiver<String>,
    legs_rx: UnboundedReceiver<Leg>,
    /// Lines to print once the terminal is restored.
    afterwards: Vec<String>,
    /// The engine stopped answering.
    gone: bool,
}

impl Shell {
    /// Fold one event and run what comes back.
    fn apply(&mut self, event: UiEvent) {
        for effect in update(&mut self.model, event) {
            match effect {
                UiEffect::Send { to, body } => self.jobs.push_back(Job::Send { to, body }),
                UiEffect::Fetch => self.jobs.push_back(Job::Fetch),
                UiEffect::Quit => {}
                UiEffect::Notice(line) => self.afterwards.push(line),
            }
        }
        self.start_next();
    }

    /// Everything that arrived since the last call, without waiting.
    fn poll(&mut self) {
        match self.fanout.pump(&mut self.attached.outbox) {
            Ok(events) => {
                for event in events {
                    self.apply(UiEvent::Engine(event));
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
        while let Ok(event) = self.done_rx.try_recv() {
            self.running = false;
            self.apply(event);
        }
        while let Ok(line) = self.notices_rx.try_recv() {
            self.apply(UiEvent::Notice(line));
        }
        self.start_next();
    }

    /// Run the next job if none is running. One at a time: the runners each
    /// read every event, and the model names a queued message after the
    /// oldest running send.
    fn start_next(&mut self) {
        if self.running {
            return;
        }
        let Some(job) = self.jobs.pop_front() else {
            return;
        };
        self.running = true;
        let tap = self.fanout.tap();
        let context = self.context.clone();
        let done = self.done_tx.clone();
        tokio::spawn(async move {
            let event = match job {
                Job::Send { to, body } => send(tap, context, to, body).await,
                Job::Fetch => UiEvent::FetchDone(fetch(tap, context).await),
            };
            let _ = done.send(event);
        });
    }
}

/// One compose-line send, exactly as `lnmsg send --via auto` runs it.
async fn send(mut tap: Tap, context: RunContext, to: [u8; 16], body: String) -> UiEvent {
    let mut options = SendOptions::new(context.instance.clone(), context.timeout);
    let mut effects = context.effects.clone();
    let (home, config_dir, instance, pn) = (
        context.home.clone(),
        context.config_dir.clone(),
        context.instance.clone(),
        context.preferred_pn,
    );
    let request = SendRequest {
        destination: to,
        title: Vec::new(),
        body: body.into_bytes(),
        via: Via::Link,
    };
    let outcome = run_send_auto(
        &mut tap,
        request,
        context.preferred_pn,
        context.pn_source,
        &mut options,
        || async move { super::mailbox_run(&home, &config_dir, &instance, pn).await },
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

/// One mailbox drain, exactly as `lnmsg fetch` runs it.
async fn fetch(mut tap: Tap, context: RunContext) -> Result<usize, String> {
    let mut seen = SeenStore::load(&context.home).map_err(|e| format!("seen store: {e}"))?;
    // A drain gets twice a send's budget, as on the command line.
    let mut options = FetchOptions::new(context.instance.clone(), context.timeout * 2);
    options.mailbox = super::mailbox_run(
        &context.home,
        &context.config_dir,
        &context.instance,
        context.preferred_pn,
    )
    .await;
    let mut effects = context.effects.clone();
    run_fetch(
        &mut tap,
        context.preferred_pn,
        context.pn_source,
        &options,
        &mut effects,
        &mut seen,
        &Tokio::new(),
    )
    .await
    .map(|fetched| fetched.new)
    .map_err(|error| error.to_string())
}

/// `lnmsg tui`.
pub async fn run(options: TuiOptions) -> ExitCode {
    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return super::failure(error, "no-home"),
    };
    // A full screen has no stderr to spare: the warnings a CLI run prints go
    // to a file beside the identity instead, with lnmsg's own `info` lines,
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
    let effects = ScreenEffects {
        notices: notices_tx,
        legs: legs_tx,
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

    let mut model = Model::new(&options.to);
    model.ascii = options.ascii;
    let (done_tx, done_rx) = unbounded_channel();
    let mut shell = Shell {
        model,
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
        },
        jobs: VecDeque::new(),
        running: false,
        legs_seen: Vec::new(),
        done_tx,
        done_rx,
        notices_rx,
        legs_rx,
        afterwards: Vec::new(),
        gone: false,
    };

    let outcome = match &options.script {
        Some(script) => {
            let frames = options
                .frames
                .clone()
                .unwrap_or_else(|| script.with_extension("frames"));
            run_script(&mut shell, script, &frames, &options).await
        }
        None => run_terminal(&mut shell, &options).await,
    };
    let Shell {
        attached,
        afterwards,
        ..
    } = shell;
    attached.stop().await;
    for line in afterwards {
        eprintln!("lnmsg: {line}");
    }
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => super::failure(error, "failed"),
    }
}

/// The real terminal: keys in, frames out, until the reader quits.
async fn run_terminal(shell: &mut Shell, options: &TuiOptions) -> Result<(), String> {
    install_panic_hook();
    // Before the alternate screen, as lnomad does: the background query reads
    // its answer on stdin, which raw mode would hand to the key handler.
    let theme = resolve_theme(detect_background(ThemeFlag::Auto), ThemeFlag::Auto);
    let mut guard = TerminalGuard::new(CrosstermTerminal::new()).map_err(|e| e.to_string())?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(std::io::stdout())).map_err(|e| e.to_string())?;
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(TICK_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pump = tokio::time::interval(Duration::from_millis(PUMP_MS));
    pump.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let result = loop {
        let view = ui::view(&shell.model);
        if let Err(error) = terminal.draw(|frame| draw(frame, &view, theme, options.no_color)) {
            break Err(error.to_string());
        }
        if shell.model.quit {
            break Ok(());
        }
        tokio::select! {
            item = events.next() => match step_event(item, map_event) {
                EventStep::Apply(event) => shell.apply(event),
                EventStep::Ignore => {}
                EventStep::End(result) => break result.map_err(|e| e.to_string()),
            },
            _ = pump.tick() => shell.poll(),
            _ = tick.tick() => shell.apply(UiEvent::Tick),
        }
    };
    guard.restore_now().map_err(|e| e.to_string())?;
    result
}

/// Keys become key events; a resize is picked up by the next draw.
fn map_event(event: Event) -> Option<UiEvent> {
    match event {
        Event::Key(key) => Some(UiEvent::Key(key_event(key))),
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
    Type(String),
    Tick(u32),
    Frame(String),
    Until(Until, Duration),
    Sleep(u64),
}

/// The script's key names: `tab`, `backtab`, `enter`, `esc`, `up`, `down`,
/// `home`, `end`, `backspace`, `ctrl-<c>`, or one character.
fn parse_key(name: &str) -> Result<KeyEvent, String> {
    let plain = |code| Ok(KeyEvent::new(code, KeyModifiers::NONE));
    match name {
        "tab" => plain(KeyCode::Tab),
        "backtab" => Ok(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
        "enter" => plain(KeyCode::Enter),
        "esc" => plain(KeyCode::Esc),
        "up" => plain(KeyCode::Up),
        "down" => plain(KeyCode::Down),
        "home" => plain(KeyCode::Home),
        "end" => plain(KeyCode::End),
        "backspace" => plain(KeyCode::Backspace),
        other => {
            if let Some(c) = other.strip_prefix("ctrl-").and_then(single) {
                return Ok(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
            }
            match single(other) {
                Some(c) if c.is_uppercase() => {
                    Ok(KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT))
                }
                Some(c) => plain(KeyCode::Char(c)),
                None => Err(format!("unknown key {other:?}")),
            }
        }
    }
}

fn single(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
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
/// tick [n]                       n spinner/toast ticks (1)
/// frame [label]                  dump the screen
/// until ready [secs]             the engine is attached
/// until delivery <state> [secs]  the newest sent message shows <state>
/// until received <n> [secs]      n messages have arrived
/// until leg <direct|propagated> [secs]
///                                the runner is on that leg for the newest
///                                sent message
/// sleep <ms>                     wait as the idle terminal does, up to 60000:
///                                the engine is read every 20 ms and one tick
///                                applied per 250 ms of the script's clock
/// ```
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
            "tick" => Step::Tick(match rest.trim() {
                "" => 1,
                n => n.parse().map_err(|_| at(format!("bad tick count {n:?}")))?,
            }),
            "frame" => Step::Frame(rest.trim().to_string()),
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
    let mut out =
        std::fs::File::create(frames).map_err(|e| format!("{}: {e}", frames.display()))?;
    let (width, height) = options.size;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).map_err(|e| e.to_string())?;
    let mut count = 0usize;
    let mut dump = |terminal: &mut Terminal<TestBackend>, model: &Model, label: &str| {
        let view = ui::view(model);
        terminal
            .draw(|frame| draw(frame, &view, Theme::Dark, true))
            .map_err(|e| e.to_string())?;
        count += 1;
        let text = frame_text(terminal.backend());
        write!(out, "--- frame {count}: {label} ---\n{text}")
            .and_then(|()| out.flush())
            .map_err(|e| format!("{}: {e}", frames.display()))
    };
    for step in steps {
        match step {
            Step::Key(key) => shell.apply(UiEvent::Key(key)),
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
            Step::Frame(label) => dump(&mut terminal, &shell.model, &label)?,
            Step::Until(until, within) => {
                let deadline = tokio::time::Instant::now() + within;
                loop {
                    shell.poll();
                    if until.holds(&shell.model, &shell.legs_seen) {
                        break;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        dump(&mut terminal, &shell.model, &format!("timeout: {until:?}"))?;
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
        if shell.model.quit {
            break;
        }
    }
    Ok(())
}

/// Bold for the focused pane's border and title.
fn pane_block(title: &str, focused: bool) -> Block<'static> {
    let (title, style) = match focused {
        true => (
            format!("● {title}"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        false => (format!("  {title}"), Style::default()),
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(Span::styled(title, style))
}

/// Draw the screen: conversations left, messages right, the compose line
/// under them, the footer at the bottom; the toast, the palette and the help
/// on top.
pub fn draw(frame: &mut Frame, view: &View, theme: Theme, no_color: bool) {
    let compose_lines: Vec<&str> = view.compose.split('\n').collect();
    let compose_rows = compose_lines.len().clamp(1, COMPOSE_MAX_ROWS) as u16;
    let [body, compose, footer_row] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(compose_rows + 2),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)]).areas(body);

    let block = pane_block(&view.conversations_title, view.focus == Pane::Conversations);
    let inner = block.inner(left);
    frame.render_widget(block, left);
    frame.render_widget(Paragraph::new(to_ratatui_text(&view.conversations)), inner);

    let block = pane_block(&view.messages_title, view.focus == Pane::Messages);
    let inner = block.inner(right);
    frame.render_widget(block, right);
    // Each message wrapped to the pane, then scrolled by rows so the
    // cursor's message ends on screen.
    let rows = inner.height as usize;
    let mut wrapped = Vec::new();
    let mut cursor_end = 0;
    for (index, lines) in view.messages.iter().enumerate() {
        wrapped.extend(ui::wrap(
            lines,
            inner.width as usize,
            ui::CONTINUATION_INDENT,
        ));
        if index == view.message_cursor {
            cursor_end = wrapped.len();
        }
    }
    let skip = cursor_end.saturating_sub(rows);
    let visible: Vec<_> = wrapped.into_iter().skip(skip).collect();
    frame.render_widget(Paragraph::new(to_ratatui_text(&visible)), inner);

    // The compose pane: one row a line, up to five, scrolled to the
    // cursor's line and, sideways, to its column.
    let focused = view.focus == Pane::Compose;
    let block = pane_block("compose", focused);
    let inner = block.inner(compose);
    frame.render_widget(block, compose);
    let (cursor_line, cursor_column) = line_and_column(&view.compose, view.compose_cursor);
    let width = inner.width.max(1) as usize;
    let height = inner.height.max(1) as usize;
    let scroll = (cursor_column + 1).saturating_sub(width);
    let top = (cursor_line + 1).saturating_sub(height);
    let shown: Vec<Line> = compose_lines
        .iter()
        .skip(top)
        .take(height)
        .map(|line| Line::from(line.chars().skip(scroll).collect::<String>()))
        .collect();
    frame.render_widget(Paragraph::new(Text::from(shown)), inner);
    if focused && view.palette.is_none() && !view.help {
        frame.set_cursor_position(Position::new(
            inner.x + (cursor_column - scroll) as u16,
            inner.y + (cursor_line - top) as u16,
        ));
    }

    let slots = footer::layout(view.footer.clone(), from_rect(footer_row));
    leviculum_ui_ratatui::footer::render(frame, footer_row, &slots, None, no_color, theme);

    if let Some(note) = &view.toast {
        toast::render(frame, body, note, no_color);
    }
    if let Some(palette) = &view.palette {
        let width = 40.min(frame.area().width);
        let height = (palette.rows.len() as u16 + 3).min(frame.area().height);
        let area = Rect {
            x: frame.area().x + frame.area().width.saturating_sub(width) / 2,
            y: frame.area().y + 2,
            width,
            height,
        };
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
            " lnmsg help ",
            0,
            no_color,
            theme,
        );
    }
}

/// The line and the column, in characters, of the `cursor`th character of
/// `text`.
fn line_and_column(text: &str, cursor: usize) -> (usize, usize) {
    let mut line = 0;
    let mut column = 0;
    for c in text.chars().take(cursor) {
        if c == '\n' {
            line += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    (line, column)
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
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
        let view = ui::view(model);
        terminal
            .draw(|frame| draw(frame, &view, Theme::Dark, true))
            .expect("draw");
        frame_text(terminal.backend())
    }

    #[test]
    fn the_three_panes_and_the_footer_are_drawn_with_the_focus_marked() {
        let model = Model::new(&[[0xab; 16]]);
        let screen = render(&model);
        assert!(screen.contains("● lnmsg (attaching)"), "{screen}");
        assert!(screen.contains("▸ abababababababab"), "{screen}");
        assert!(screen.contains(ui::NO_HISTORY), "{screen}");
        assert!(screen.contains("  compose"), "{screen}");
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
    /// longer than its pane wraps under its mark.
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
            .position(|row| row.contains("● compose"))
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
        assert!(screen.contains("me: word word"), "{screen}");
        assert!(
            screen.contains("│    word word"),
            "the wrapped row is set in: {screen}"
        );
        assert!(
            !screen.contains("w│"),
            "no word is cut at the edge: {screen}"
        );
    }

    #[test]
    fn the_help_and_the_palette_draw_over_the_panes() {
        let mut model = Model::new(&[[0xab; 16]]);
        update(&mut model, UiEvent::Key(parse_key("?").expect("key")));
        assert!(render(&model).contains("lnmsg help"));
        update(&mut model, UiEvent::Key(parse_key("esc").expect("key")));
        update(&mut model, UiEvent::Key(parse_key("ctrl-p").expect("key")));
        let screen = render(&model);
        assert!(screen.contains("commands"), "{screen}");
        assert!(screen.contains("▸ send"), "{screen}");
        assert!(screen.contains("focus compose"), "{screen}");
    }
}
