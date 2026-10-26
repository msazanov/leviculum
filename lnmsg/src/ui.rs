//! The screen of `lnmsg tui` as a model: what the reader sees and what a key
//! does to it, with no terminal anywhere (`docs/src/concepts/lnmsg-ui.md`,
//! sections 1 and 2; `lnmsg-architecture.md`, "the core must not know it has
//! a terminal").
//!
//! The shape is lnomad's, TEA: [`UiModel`] is the state, [`update`] folds one
//! [`UiEvent`] into it and returns the [`UiEffect`]s the shell is to run, and
//! [`view`] turns the model into what each pane shows. The shell draws that
//! and runs the effects; a test does neither and asserts on the model, the
//! effects and the view directly.
//!
//! Three panes, focus follows the pane: conversations, messages, compose.
//! What a key does is one table, [`crate::keys`]: lnomad's scroll rows in
//! the two command panes, lnmsg's own rows beside them, each chord with the
//! panes it applies in. The key handler, the help overlay, the footer, the
//! palette's keys and the compose pane's hint all read it
//! (`docs/src/concepts/lnmsg-keys-and-layout.md`, section 4). Compose takes
//! plain typing; the palette (`:` or `Alt-x`) is layered on top over the
//! shared names in [`leviculum_ui::commands`]. No modes.
//!
//! # What a delivery glyph claims
//!
//! A sent message carries a [`DeliveryState`] from the shared glyph table.
//! The router's states map onto it in [`delivery_of`]. While the frontend's
//! send runner is still working on a message, a giving-up state is held
//! back: `--via auto` cancels or loses its direct leg and then uploads the
//! same text to a mailbox, and a `✗` drawn for the leg would be a lie about
//! the message. The `✗` appears when the runner reports it gave up
//! ([`UiEvent::SendDone`] with an error).
//!
//! # The layout
//!
//! Signal's shape in cells (`lnmsg-keys-and-layout.md`, section 5): the list
//! with two rows a conversation, most recent first; the open conversation as
//! blocks, ours flush right and theirs flush left, with a date line between
//! days and a `new` rule where the unread begins; a detail view per message
//! with what this session saw happen to it. The model knows the screen's size
//! ([`UiEvent::Resize`]) and the time of day ([`UiEvent::Clock`]), because
//! where a block wraps and what a page is depend on the one, and every time on
//! screen on the other; [`layout`] turns a conversation into rows.
//!
//! # What is not here
//!
//! History: lnmsg keeps no message store on disk (only the seen record's
//! ids), so the conversations are this session's, and the top of the pane
//! says so (`nothing earlier on this device`). Names and trust states, the
//! mouse, search and QR codes are later slices.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use leviculum_lxmf::router::MessageState;
use leviculum_ui::commands::{self, Command as PaletteCommand, PaletteEntry};
use leviculum_ui::delivery::{DeliveryState, Tone};
use leviculum_ui::footer::{FooterItem, FooterRole};
use leviculum_ui::help::HelpGroup;
use leviculum_ui::input::{InputBuffer, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use leviculum_ui::keymap::{Ctx, ScrollCmd};
use leviculum_ui::toast::{Toast, ToastKind};
use leviculum_ui::{RLine, RStyle, StyledChar};

use crate::address::to_hex;
use crate::keys::{self, Action};
use crate::outbox::OutboxEvent;
use crate::send::Leg;
use crate::when::{list_time, WallTime};

pub mod layout;

/// How many ticks a toast stays up. The shell ticks every 250 ms, so three
/// seconds.
pub const TOAST_TICKS: u64 = 12;

/// Shell ticks in a second, for the leg's elapsed time on a row.
const TICKS_PER_SECOND: u64 = 4;

/// Hex characters of an address a pane shows; the full address is in the
/// title bar of the messages pane.
const SHORT_HEX: usize = 16;

/// The compose pane grows with its text up to this many rows, then scrolls.
pub const COMPOSE_MAX_ROWS: usize = 5;

/// The conversation list is never narrower than this, so a row keeps a name
/// and a time beside each other (the 80 x 24 mockup's 26 cells).
pub const LIST_MIN_WIDTH: u16 = 26;

/// From this inner width on the header is the 120 x 40 mockup's: the full
/// address in the title and a row for the mailbox under it.
pub const WIDE_HEADER: usize = 76;

/// The list pane's outer width on a screen `width` cells wide: three tenths,
/// at least [`LIST_MIN_WIDTH`], at most half.
pub fn list_width(width: u16) -> u16 {
    (width * 3 / 10).max(LIST_MIN_WIDTH).min(width / 2)
}

/// The inner sizes of the two panes the model lays out, in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneSizes {
    /// The conversation list, inside its border.
    pub list: (usize, usize),
    /// The conversation, between its top border and the compose box.
    pub messages: (usize, usize),
}

impl PaneSizes {
    /// The panes of a `width` x `height` screen whose compose box shows
    /// `compose_rows` rows: the list the full height above the footer, the
    /// conversation and the compose box beside it, sharing one border.
    pub fn for_screen(width: u16, height: u16, compose_rows: usize) -> Self {
        let body = usize::from(height.saturating_sub(1));
        let left = list_width(width);
        let right = usize::from(width - left);
        Self {
            list: (usize::from(left).saturating_sub(2), body.saturating_sub(2)),
            messages: (
                right.saturating_sub(2),
                body.saturating_sub(compose_rows + 2).saturating_sub(1),
            ),
        }
    }
}

/// A pane, and where the focus is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    /// The conversation list.
    Conversations,
    /// The selected conversation's messages.
    Messages,
    /// The compose line.
    Compose,
}

impl Pane {
    fn next(self) -> Self {
        match self {
            Self::Conversations => Self::Messages,
            Self::Messages => Self::Compose,
            Self::Compose => Self::Conversations,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Conversations => Self::Compose,
            Self::Messages => Self::Conversations,
            Self::Compose => Self::Messages,
        }
    }
}

/// One message in a conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Whether we wrote it.
    pub outbound: bool,
    /// The body, as text (lossy for bytes that are not UTF-8).
    pub body: String,
    /// The router's id: a received message's, or the id the router gave the
    /// latest leg of a sent one. `None` until the router queued it.
    pub id: Option<[u8; 32]>,
    /// Where a sent message stands; `None` for a received one.
    pub delivery: Option<DeliveryState>,
    /// Whether a received message's signature verified.
    pub verified: bool,
    /// The leg the send runner is on and the tick it began, while the runner
    /// still works on the message: the row says `direct 12 s` or `via
    /// mailbox 40 s` beside the spinner, so a wait has a reason on screen.
    pub leg: Option<(Leg, u64)>,
    /// The runner ended with the message on a link to the peer and no proof
    /// ([`UiEvent::SendUnproven`]). It stays `◇` from then on: only a proof
    /// (`✓`) or a refusal (`✗`) moves it, not the router's retries on new
    /// links nor its giving up on them, since the peer may well have it.
    pub unproven: bool,
    /// When this session wrote or received it; `None` before the shell said
    /// the time.
    pub at: Option<WallTime>,
    /// The body's bytes, as they went or came.
    pub size: usize,
    /// What this session saw happen to it, oldest first: the detail view.
    pub ledger: Vec<LedgerEntry>,
    /// The leg the runner last reported, kept after it ends so a late proof
    /// still names the way the message went.
    pub last_leg: Option<Leg>,
}

impl Message {
    fn ours(body: String, at: Option<WallTime>) -> Self {
        Self {
            outbound: true,
            size: body.len(),
            body,
            id: None,
            delivery: Some(DeliveryState::Queued),
            verified: true,
            leg: None,
            unproven: false,
            at,
            ledger: vec![LedgerEntry::new(at, "queued")],
            last_leg: None,
        }
    }

    fn note(&mut self, at: Option<WallTime>, what: &str) {
        if self.ledger.last().map(|e| e.what.as_str()) != Some(what) {
            self.ledger.push(LedgerEntry::new(at, what));
        }
    }
}

/// One line of a message's ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerEntry {
    pub at: Option<WallTime>,
    pub what: String,
}

impl LedgerEntry {
    fn new(at: Option<WallTime>, what: &str) -> Self {
        Self {
            at,
            what: what.to_string(),
        }
    }
}

/// Where the unread begins in a conversation, and how many arrived unread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewRule {
    /// The first unread message's index.
    pub at: usize,
    pub count: usize,
}

/// One correspondent and this session's messages with them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    /// Their LXMF address.
    pub peer: [u8; 16],
    /// Oldest first.
    pub messages: Vec<Message>,
    /// Received messages not yet looked at: the list's badge.
    pub unread: usize,
    /// The `new` rule: set by the first message that arrives unread, kept
    /// until the conversation is left.
    pub new_rule: Option<NewRule>,
    /// The model's activity count at the last message, for the list's order:
    /// most recent first, a conversation without messages by its place.
    pub activity: u64,
}

impl Conversation {
    fn new(peer: [u8; 16]) -> Self {
        Self {
            peer,
            messages: Vec::new(),
            unread: 0,
            new_rule: None,
            activity: 0,
        }
    }

    /// Note one more unread message, the newest.
    fn mark_new(&mut self) {
        let at = self.messages.len().saturating_sub(1);
        self.new_rule.get_or_insert(NewRule { at, count: 0 }).count += 1;
    }
}

/// The mailbox as the header says it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MailboxShown {
    /// The selected node, once a run or the stored record named one.
    pub node: Option<[u8; 16]>,
    /// Named with `--pn`, not adopted.
    pub by_user: bool,
    /// When this session's last fetch finished.
    pub synced: Option<WallTime>,
}

/// The open command palette.
#[derive(Clone, Debug, Default)]
pub struct Palette<B> {
    /// What the reader typed.
    pub query: B,
    /// The highlighted row among the matches.
    pub selected: usize,
}

impl<B: InputBuffer> Palette<B> {
    /// The rows the query matches, in table order.
    pub fn matches(&self) -> Vec<&'static PaletteEntry> {
        commands::filter(self.query.value())
    }
}

/// The whole screen's state. `B` is the text field: `tui_input::Input` in
/// the terminal, whatever a board brings on a board.
#[derive(Clone, Debug)]
pub struct UiModel<B> {
    /// Conversations, in the order they appeared.
    pub conversations: Vec<Conversation>,
    /// The selected conversation's index.
    pub selected: usize,
    /// The message the messages pane's cursor is on, in the selected
    /// conversation.
    pub message_cursor: usize,
    /// The compose line.
    pub compose: B,
    /// Where keys go.
    pub focus: Pane,
    /// Notes floated over the content, newest last.
    pub toasts: Vec<Toast>,
    /// The palette, when open.
    pub palette: Option<Palette<B>>,
    /// Whether the help overlay is up.
    pub help: bool,
    /// Our own address, once the engine is ready.
    pub address: Option<[u8; 16]>,
    /// Draw the delivery glyphs' ASCII fallback.
    pub ascii: bool,
    /// The tick counter: spinner frames and toast ages.
    pub tick: u64,
    /// Whether the reader asked to leave.
    pub quit: bool,
    /// Sent messages whose runner has not finished, as (conversation,
    /// message) indices, in the order the shell runs them.
    pub sending: VecDeque<(usize, usize)>,
    /// Whether a fetch is running.
    pub fetching: bool,
    /// Whether the terminal speaks the keyboard enhancement protocol, so
    /// `Shift-Enter` and `Ctrl-Enter` arrive as themselves: the hint and the
    /// footer offer them then.
    pub enhanced: bool,
    /// `Enter` in compose sends (the default). Off, it breaks the line and
    /// the modifier chord sends: `Ctrl-Enter` with enhancement, `Alt-Enter`
    /// without.
    pub send_on_enter: bool,
    /// The time of day, as the shell last said it.
    pub now: Option<WallTime>,
    /// The screen, in cells: what [`PaneSizes`] are made from.
    pub screen: (u16, u16),
    /// The conversation's first row on screen when the reader scrolled up;
    /// `None` keeps the newest at the bottom.
    pub scroll_top: Option<usize>,
    /// The message whose detail view is up, as (conversation, message).
    pub detail: Option<(usize, usize)>,
    /// The mailbox, for the header.
    pub mailbox: MailboxShown,
    /// Counts messages in and out, for the list's order.
    pub activity: u64,
}

impl<B: InputBuffer + Default> UiModel<B> {
    /// An empty screen with these correspondents already listed (`lnmsg tui
    /// --to`), focus on the conversation list.
    pub fn new(peers: &[[u8; 16]]) -> Self {
        let mut model = Self {
            conversations: Vec::new(),
            selected: 0,
            message_cursor: 0,
            compose: B::default(),
            focus: Pane::Conversations,
            toasts: Vec::new(),
            palette: None,
            help: false,
            address: None,
            ascii: false,
            tick: 0,
            quit: false,
            sending: VecDeque::new(),
            fetching: false,
            enhanced: false,
            send_on_enter: true,
            now: None,
            screen: (80, 24),
            scroll_top: None,
            detail: None,
            mailbox: MailboxShown::default(),
            activity: 0,
        };
        for peer in peers {
            model.conversation(*peer);
        }
        model
    }
}

impl<B> UiModel<B> {
    /// The keymap's context: the terminal's keyboard protocol and the
    /// `send_on_enter` switch.
    pub fn ctx(&self) -> Ctx {
        let mut ctx = Ctx::NONE;
        if self.enhanced {
            ctx = ctx.with(Ctx::ENHANCED);
        }
        if !self.send_on_enter {
            ctx = ctx.with(keys::ENTER_BREAKS);
        }
        ctx
    }

    /// The selected conversation's messages.
    pub fn messages_of_selected(&self) -> &[Message] {
        self.conversations
            .get(self.selected)
            .map(|c| c.messages.as_slice())
            .unwrap_or(&[])
    }

    /// The index of `peer`'s conversation, made if new.
    fn conversation(&mut self, peer: [u8; 16]) -> usize {
        match self.conversations.iter().position(|c| c.peer == peer) {
            Some(index) => index,
            None => {
                self.conversations.push(Conversation::new(peer));
                self.conversations.len() - 1
            }
        }
    }

    fn toast(&mut self, kind: ToastKind, text: String) {
        self.toasts.push(Toast {
            kind,
            text,
            shown_at: self.tick,
        });
    }

    /// The toast to draw now, if any is still young enough.
    pub fn current_toast(&self) -> Option<&Toast> {
        self.toasts
            .last()
            .filter(|t| self.tick.saturating_sub(t.shown_at) < TOAST_TICKS)
    }

    /// Open conversation `index`. The one left loses its `new` rule; the
    /// one opened keeps its own until it is left in turn, and shows its
    /// newest message at the bottom.
    fn select(&mut self, index: usize) {
        if index != self.selected {
            if let Some(left) = self.conversations.get_mut(self.selected) {
                left.new_rule = None;
            }
        }
        self.selected = index;
        self.scroll_top = None;
        if let Some(conversation) = self.conversations.get_mut(index) {
            conversation.unread = 0;
            self.message_cursor = conversation.messages.len().saturating_sub(1);
        }
    }

    /// The conversations in the list's order: by their last message, most
    /// recent first; those without one after, in the order they came.
    pub fn order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.conversations.len()).collect();
        order.sort_by_key(|&i| core::cmp::Reverse(self.conversations[i].activity));
        order
    }

    fn bump(&mut self) -> u64 {
        self.activity += 1;
        self.activity
    }

    /// The rows the compose box shows.
    pub fn compose_rows(&self) -> usize
    where
        B: InputBuffer,
    {
        self.compose
            .value()
            .split('\n')
            .count()
            .clamp(1, COMPOSE_MAX_ROWS)
    }

    /// The panes' inner sizes on this screen.
    pub fn sizes(&self) -> PaneSizes
    where
        B: InputBuffer,
    {
        PaneSizes::for_screen(self.screen.0, self.screen.1, self.compose_rows())
    }

    /// The header's rows inside the conversation pane: none, or the mailbox
    /// row and a rule on a wide one.
    fn header_rows(&self) -> usize
    where
        B: InputBuffer,
    {
        match self.sizes().messages.0 >= WIDE_HEADER {
            true => 2,
            false => 0,
        }
    }

    /// The rows of the conversation pane that carry messages.
    pub fn message_rows(&self) -> usize
    where
        B: InputBuffer,
    {
        self.sizes().messages.1.saturating_sub(self.header_rows())
    }

    /// The open conversation laid out at the pane's width.
    fn laid(&self) -> layout::Laid
    where
        B: InputBuffer,
    {
        let selected = (self.focus == Pane::Messages).then_some(self.message_cursor);
        match self.conversations.get(self.selected) {
            Some(conversation) => layout::conversation(
                conversation,
                self.sizes().messages.0,
                &layout::Draw {
                    now: self.now,
                    tick: self.tick,
                    ascii: self.ascii,
                    selected,
                },
            ),
            None => layout::Laid::empty(self.sizes().messages.0),
        }
    }

    /// The first row on screen of `total` laid-out rows.
    fn top_of(&self, total: usize) -> usize
    where
        B: InputBuffer,
    {
        let bottom = total.saturating_sub(self.message_rows());
        self.scroll_top.unwrap_or(bottom).min(bottom)
    }
}

/// What the model is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiEvent {
    /// A key, routed to the focused pane, or to the palette or help on top.
    Key(KeyEvent),
    /// A palette command run directly, as a GUI menu would.
    Run(PaletteCommand),
    /// The shell's 250 ms tick: the spinner's frame, the toasts' age.
    Tick,
    /// The time of day: before the first frame and on every tick in the
    /// terminal, whenever a key script says so.
    Clock(WallTime),
    /// The screen's size in cells.
    Resize { width: u16, height: u16 },
    /// The mailbox the stored record or a run selected.
    MailboxChosen { node: [u8; 16], by_user: bool },
    /// One event from the message engine.
    Engine(OutboxEvent),
    /// The oldest running send finished; the error text when it failed.
    SendDone(Result<(), String>),
    /// The oldest running send ended with its message on a link to the peer
    /// and no delivery proof (`SendError::Unproven`): shown `◇`, sent and
    /// not proven, with the runner's text in a toast.
    SendUnproven(String),
    /// The oldest running send moved onto this leg.
    SendLeg(Leg),
    /// The fetch finished: how many messages were new, or why it failed.
    FetchDone(Result<usize, String>),
    /// A line a runner or the stamp worker said along the way (the auto
    /// fallback's "trying a propagation node", a mailbox adoption), the
    /// lines the CLI writes to stderr.
    Notice(String),
}

/// What the shell is asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiEffect {
    /// Send `body` to `to` the way `lnmsg send --via auto` does. The shell
    /// runs sends one at a time in this order and reports each with
    /// [`UiEvent::SendDone`].
    Send { to: [u8; 16], body: String },
    /// Drain the mailbox, as `lnmsg fetch` does; reported with
    /// [`UiEvent::FetchDone`].
    Fetch,
    /// Leave.
    Quit,
    /// Draw the whole screen again from scratch (`Ctrl-l`).
    Redraw,
    /// A line worth keeping after the screen is gone: the shell prints it
    /// once the terminal is restored. A toast is cut to the screen's width,
    /// and a failure's full text is often longer.
    Notice(String),
}

/// Fold one event into the model.
pub fn update<B: InputBuffer + Default>(model: &mut UiModel<B>, event: UiEvent) -> Vec<UiEffect> {
    match event {
        UiEvent::Key(key) if key.kind == KeyEventKind::Release => Vec::new(),
        UiEvent::Key(key) => on_key(model, key),
        UiEvent::Run(command) => run(model, command),
        UiEvent::Tick => {
            model.tick = model.tick.wrapping_add(1);
            let tick = model.tick;
            model
                .toasts
                .retain(|t| tick.saturating_sub(t.shown_at) < TOAST_TICKS);
            Vec::new()
        }
        UiEvent::Clock(now) => {
            model.now = Some(now);
            Vec::new()
        }
        UiEvent::Resize { width, height } => {
            model.screen = (width, height);
            Vec::new()
        }
        UiEvent::MailboxChosen { node, by_user } => {
            model.mailbox.node = Some(node);
            model.mailbox.by_user = by_user;
            Vec::new()
        }
        UiEvent::Engine(event) => on_engine(model, event),
        UiEvent::Notice(line) => {
            model.toast(ToastKind::Info, line);
            Vec::new()
        }
        UiEvent::SendDone(result) => on_send_done(model, result),
        UiEvent::SendUnproven(detail) => on_send_unproven(model, detail),
        UiEvent::SendLeg(leg) => {
            let (tick, now) = (model.tick, model.now);
            if let Some(&(c, m)) = model.sending.front() {
                if let Some(message) = model.message_mut(c, m) {
                    message.leg = Some((leg, tick));
                    message.last_leg = Some(leg);
                    message.note(now, &format!("trying {}", leg_words(leg)));
                }
            }
            Vec::new()
        }
        UiEvent::FetchDone(result) => {
            model.fetching = false;
            if result.is_ok() {
                model.mailbox.synced = model.now;
            }
            match result {
                Ok(0) => model.toast(ToastKind::Info, "no new messages".to_string()),
                Ok(new) => model.toast(ToastKind::Info, format!("{new} new message(s)")),
                Err(detail) => {
                    model.toast(ToastKind::Error, format!("fetch failed: {detail}"));
                    return vec![UiEffect::Notice(format!("fetch: {detail}"))];
                }
            }
            Vec::new()
        }
    }
}

fn on_key<B: InputBuffer + Default>(model: &mut UiModel<B>, key: KeyEvent) -> Vec<UiEffect> {
    let ctx = model.ctx();
    if model.help || model.palette.is_some() || model.detail.is_some() {
        // Only `Ctrl-q`, `Ctrl-c` and backing out reach past an overlay.
        match keys::resolve(&key, keys::OVERLAY, ctx) {
            Some(Action::Run(PaletteCommand::Quit)) => return run(model, PaletteCommand::Quit),
            Some(Action::QuitHint) => return quit_hint(model),
            Some(Action::Back) => {
                model.help = false;
                model.palette = None;
                model.detail = None;
                return Vec::new();
            }
            _ => {}
        }
        return overlay_key(model, key);
    }
    let pane = model.focus;
    match keys::resolve(&key, keys::panes_of(pane), ctx) {
        Some(action) => act(model, action, key),
        None => {
            if pane == Pane::Compose {
                model.compose.edit(&key);
            }
            Vec::new()
        }
    }
}

/// A key while the help, the palette or a detail view is up.
fn overlay_key<B: InputBuffer + Default>(model: &mut UiModel<B>, key: KeyEvent) -> Vec<UiEffect> {
    if model.help {
        if matches!(
            key.code,
            KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter
        ) {
            model.help = false;
        }
        return Vec::new();
    }
    if model.detail.is_some() {
        if matches!(key.code, KeyCode::Char('q') | KeyCode::Enter) {
            model.detail = None;
        }
        return Vec::new();
    }
    let Some(palette) = model.palette.as_mut() else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Enter => {
            let chosen = palette.matches().get(palette.selected).map(|e| e.command);
            model.palette = None;
            if let Some(command) = chosen {
                return run(model, command);
            }
        }
        KeyCode::Down => {
            let last = palette.matches().len().saturating_sub(1);
            palette.selected = (palette.selected + 1).min(last);
        }
        KeyCode::Up => palette.selected = palette.selected.saturating_sub(1),
        _ => {
            if palette.query.edit(&key) {
                palette.selected = 0;
            }
        }
    }
    Vec::new()
}

/// Run what a key resolved to.
fn act<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    action: Action,
    key: KeyEvent,
) -> Vec<UiEffect> {
    match action {
        Action::Run(command) => return run(model, command),
        Action::Scroll(cmd) => scroll(model, model.focus, cmd),
        Action::ScrollMessages(cmd) => scroll(model, Pane::Messages, cmd),
        Action::Newline => model.compose.newline(),
        Action::Edit => {
            model.compose.edit(&key);
        }
        // `Ctrl-c` in compose: a reflex that would throw a half-written
        // message away (lnmsg-ui.md, section 1, "What must not be copied"),
        // so it clears the line into the field's kill buffer, back with
        // `Ctrl-y`.
        Action::ClearDraft => {
            model
                .compose
                .edit(&KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        }
        Action::Back => {
            model.focus = match model.focus {
                Pane::Compose => Pane::Messages,
                _ => Pane::Conversations,
            };
        }
        Action::NextPane => model.focus = model.focus.next(),
        Action::PreviousPane => model.focus = model.focus.prev(),
        Action::Palette => model.palette = Some(Palette::default()),
        Action::QuitHint => return quit_hint(model),
    }
    Vec::new()
}

/// `Ctrl-c` quits nowhere: outside compose it says what does.
fn quit_hint<B>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    let quit = keys::row_named("quit")
        .and_then(|row| {
            row.chords()
                .find(|c| c.in_force(keys::OVERLAY, Ctx::NONE))
                .map(|c| c.label())
        })
        .unwrap_or("q");
    model.toast(ToastKind::Info, format!("{quit} quits"));
    Vec::new()
}

/// Move the cursor of the conversation list or the messages.
fn scroll<B: InputBuffer>(model: &mut UiModel<B>, pane: Pane, cmd: ScrollCmd) {
    match pane {
        Pane::Conversations => scroll_list(model, cmd),
        _ => scroll_messages(model, cmd),
    }
}

/// The list moves in its own order, a page being the conversations it
/// shows.
fn scroll_list<B: InputBuffer>(model: &mut UiModel<B>, cmd: ScrollCmd) {
    let order = model.order();
    let Some(at) = order.iter().position(|&i| i == model.selected) else {
        return;
    };
    let last = order.len() - 1;
    let page = layout::entries_shown(model.sizes().list.1).max(1);
    let to = match cmd {
        ScrollCmd::LineDown => at + 1,
        ScrollCmd::LineUp => at.saturating_sub(1),
        ScrollCmd::PageDown => at + page,
        ScrollCmd::PageUp => at.saturating_sub(page),
        ScrollCmd::HalfPageDown => at + (page / 2).max(1),
        ScrollCmd::HalfPageUp => at.saturating_sub((page / 2).max(1)),
        ScrollCmd::Top => 0,
        ScrollCmd::Bottom => last,
    }
    .min(last);
    if order[to] != model.selected {
        model.select(order[to]);
    }
}

/// The cursor moves a message at a time, a page a pane's height of rows, and
/// the view follows it just far enough to show its block. Back on the newest
/// message with the bottom in view, the pane follows the newest again.
fn scroll_messages<B: InputBuffer>(model: &mut UiModel<B>, cmd: ScrollCmd) {
    let count = model.messages_of_selected().len();
    if count == 0 {
        return;
    }
    let laid = model.laid();
    let rows = model.message_rows().max(1);
    let total = laid.rows.len();
    let bottom = total.saturating_sub(rows);
    let mut top = model.top_of(total);
    let last = count - 1;
    let at = model.message_cursor.min(last);
    let step = match cmd {
        ScrollCmd::HalfPageDown | ScrollCmd::HalfPageUp => (rows / 2).max(1),
        _ => rows,
    };
    let cursor = match cmd {
        ScrollCmd::LineDown => (at + 1).min(last),
        ScrollCmd::LineUp => at.saturating_sub(1),
        ScrollCmd::Top => 0,
        ScrollCmd::Bottom => last,
        ScrollCmd::PageDown | ScrollCmd::HalfPageDown => {
            top = (top + step).min(bottom);
            // The lowest block that starts in the new view.
            laid.spans
                .iter()
                .rposition(|span| span.start < top + rows)
                .unwrap_or(at)
                .max(at)
        }
        ScrollCmd::PageUp | ScrollCmd::HalfPageUp => {
            top = top.saturating_sub(step);
            // The highest block that ends in the new view.
            laid.spans
                .iter()
                .position(|span| span.end > top)
                .unwrap_or(0)
                .min(at)
        }
    };
    if let Some(span) = laid.spans.get(cursor) {
        if span.end > top + rows {
            top = span.end - rows;
        }
        // A block taller than the pane shows its start.
        if span.start < top {
            top = span.start;
        }
    }
    model.message_cursor = cursor;
    model.scroll_top = match cursor == last && top >= bottom {
        true => None,
        false => Some(top),
    };
}

/// Select the conversation `step` away in the list's order, or the next one
/// with unread messages that way.
fn step_conversation<B>(model: &mut UiModel<B>, forward: bool, unread_only: bool) {
    let order = model.order();
    let count = order.len();
    let Some(at) = order.iter().position(|&i| i == model.selected) else {
        return;
    };
    if !unread_only {
        let to = match forward {
            true => (at + 1).min(count - 1),
            false => at.saturating_sub(1),
        };
        if order[to] != model.selected {
            model.select(order[to]);
        }
        return;
    }
    let found = (1..count)
        .map(|step| match forward {
            true => order[(at + step) % count],
            false => order[(at + count - step) % count],
        })
        .find(|&i| model.conversations[i].unread > 0);
    match found {
        Some(index) => model.select(index),
        None => model.toast(ToastKind::Info, "no unread conversation".to_string()),
    }
}

/// The words a leg is said in.
fn leg_words(leg: Leg) -> &'static str {
    match leg {
        Leg::Direct => "direct",
        Leg::Propagated => "via mailbox",
    }
}

/// What a message's ledger says when it reaches a state.
fn ledger_words(state: DeliveryState) -> &'static str {
    match state {
        DeliveryState::Delivered => "delivered: proof received",
        DeliveryState::Rejected => "rejected by the peer",
        DeliveryState::Failed => "failed",
        other => state_words(other),
    }
}

/// The words a delivery state is said in.
fn state_words(state: DeliveryState) -> &'static str {
    match state {
        DeliveryState::Queued => "queued",
        DeliveryState::Stamping => "stamping",
        DeliveryState::InFlight => "in flight",
        DeliveryState::Sent => "sent, no proof yet",
        DeliveryState::InMailbox => "in the mailbox",
        DeliveryState::Delivered => "delivered",
        DeliveryState::Rejected => "rejected",
        DeliveryState::Failed => "not sent",
    }
}

/// `Enter` on a message: its detail view, with what this session saw
/// happen to it (lnmsg-keys-and-layout.md, section 7, slice 2).
fn details<B>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    if model
        .messages_of_selected()
        .get(model.message_cursor)
        .is_none()
    {
        model.toast(ToastKind::Info, "no message here".to_string());
        return Vec::new();
    }
    model.detail = Some((model.selected, model.message_cursor));
    Vec::new()
}

/// Run one palette command.
fn run<B: InputBuffer + Default>(model: &mut UiModel<B>, command: PaletteCommand) -> Vec<UiEffect> {
    match command {
        PaletteCommand::Send => send(model),
        PaletteCommand::Fetch => {
            if model.fetching {
                model.toast(ToastKind::Info, "a fetch is already running".to_string());
                return Vec::new();
            }
            model.fetching = true;
            model.toast(ToastKind::Info, "fetching the mailbox".to_string());
            vec![UiEffect::Fetch]
        }
        PaletteCommand::Quit => {
            model.quit = true;
            vec![UiEffect::Quit]
        }
        PaletteCommand::Help => {
            model.help = true;
            Vec::new()
        }
        PaletteCommand::FocusConversations => focus(model, Pane::Conversations),
        PaletteCommand::FocusMessages => focus(model, Pane::Messages),
        PaletteCommand::FocusCompose => focus(model, Pane::Compose),
        PaletteCommand::NextConversation => {
            step_conversation(model, true, false);
            Vec::new()
        }
        PaletteCommand::PreviousConversation => {
            step_conversation(model, false, false);
            Vec::new()
        }
        PaletteCommand::NextUnread => {
            step_conversation(model, true, true);
            Vec::new()
        }
        PaletteCommand::PreviousUnread => {
            step_conversation(model, false, true);
            Vec::new()
        }
        PaletteCommand::Details => details(model),
        PaletteCommand::Redraw => vec![UiEffect::Redraw],
        PaletteCommand::SendOnEnter => {
            model.send_on_enter = !model.send_on_enter;
            let hint = keys::compose_hint(model.ctx());
            model.toast(ToastKind::Info, hint);
            Vec::new()
        }
    }
}

fn focus<B>(model: &mut UiModel<B>, pane: Pane) -> Vec<UiEffect> {
    model.focus = pane;
    Vec::new()
}

fn send<B: InputBuffer + Default>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    if model.compose.value().trim().is_empty() {
        return Vec::new();
    }
    let Some(conversation) = model.conversations.get_mut(model.selected) else {
        model.toast(
            ToastKind::Error,
            "no conversation to send to: start lnmsg tui --to <address>".to_string(),
        );
        return Vec::new();
    };
    let body = model.compose.take();
    conversation
        .messages
        .push(Message::ours(body.clone(), model.now));
    let to = conversation.peer;
    let index = conversation.messages.len() - 1;
    let activity = model.bump();
    model.conversations[model.selected].activity = activity;
    // Signal's rule: what you send is where the view goes.
    model.message_cursor = index;
    model.scroll_top = None;
    model.sending.push_back((model.selected, index));
    vec![UiEffect::Send { to, body }]
}

/// The glyph-table state a router state is shown as.
///
/// | router state | shown as |
/// |---|---|
/// | `Generating`, `Outbound` | `Queued` ○ |
/// | `Sending` | `InFlight` (spinner) |
/// | `Sent` | `Sent` ◇ |
/// | `AwaitingCollection` | `InMailbox` ⌂ |
/// | `Delivered` | `Delivered` ✓ |
/// | `Rejected` | `Rejected` ✗ |
/// | `Failed`, `Cancelled` | `Failed` ✗ (dim) |
///
/// `Stamping` has no router state: the engine mines off the core and says
/// nothing until the message moves on, so slice one never draws it.
pub fn delivery_of(state: MessageState) -> DeliveryState {
    match state {
        MessageState::Generating | MessageState::Outbound => DeliveryState::Queued,
        MessageState::Sending => DeliveryState::InFlight,
        MessageState::Sent => DeliveryState::Sent,
        MessageState::AwaitingCollection => DeliveryState::InMailbox,
        MessageState::Delivered => DeliveryState::Delivered,
        MessageState::Rejected => DeliveryState::Rejected,
        MessageState::Failed | MessageState::Cancelled => DeliveryState::Failed,
    }
}

fn gives_up(state: DeliveryState) -> bool {
    matches!(state, DeliveryState::Rejected | DeliveryState::Failed)
}

fn on_engine<B>(model: &mut UiModel<B>, event: OutboxEvent) -> Vec<UiEffect> {
    match event {
        OutboxEvent::Ready { address } => model.address = Some(address),
        OutboxEvent::Broken { detail } => {
            model.toast(ToastKind::Error, format!("engine: {detail}"));
            return vec![UiEffect::Notice(format!("the message engine: {detail}"))];
        }
        // The router named the message the running send handed it: the
        // oldest unfinished one, because the shell runs sends in order. A
        // fallback leg names it again with the new id.
        OutboxEvent::Queued { message_id } => {
            let now = model.now;
            if let Some(&(c, m)) = model.sending.front() {
                if let Some(message) = model.message_mut(c, m) {
                    message.id = Some(message_id);
                    message.delivery = Some(DeliveryState::Queued);
                    let id = to_hex(&message_id[..4]);
                    message.note(now, &format!("queued by the router as {id}"));
                }
            }
        }
        OutboxEvent::State { message_id, state } => apply_state(model, message_id, state),
        OutboxEvent::Left {
            message_id,
            last: Some(state),
        } => apply_state(model, message_id, state),
        OutboxEvent::Received {
            message_id,
            source,
            body,
            verified,
            ..
        } => {
            let index = model.conversation(source);
            let open = index == model.selected;
            let follow = model.scroll_top.is_none();
            if model.conversations[index]
                .messages
                .iter()
                .any(|m| !m.outbound && m.id == Some(message_id))
            {
                return Vec::new();
            }
            let at = model.now;
            let activity = model.bump();
            let conversation = &mut model.conversations[index];
            conversation.activity = activity;
            conversation.messages.push(Message {
                outbound: false,
                size: body.len(),
                body: String::from_utf8_lossy(&body).into_owned(),
                id: Some(message_id),
                delivery: None,
                verified,
                leg: None,
                unproven: false,
                at,
                ledger: vec![LedgerEntry::new(
                    at,
                    match verified {
                        true => "received, signature verified",
                        false => "received, signature NOT verified",
                    },
                )],
                last_leg: None,
            });
            if !open {
                conversation.unread += 1;
                conversation.mark_new();
            } else if follow {
                // A reader at the newest message stays at the newest.
                model.message_cursor = conversation.messages.len() - 1;
            } else {
                // One reading earlier messages keeps the view, and the rule
                // says where the new ones begin.
                conversation.mark_new();
            }
            let from = name(&source);
            model.toast(ToastKind::Info, format!("message from {from}"));
        }
        _ => {}
    }
    Vec::new()
}

impl<B> UiModel<B> {
    fn message_mut(&mut self, conversation: usize, message: usize) -> Option<&mut Message> {
        self.conversations
            .get_mut(conversation)?
            .messages
            .get_mut(message)
    }
}

fn apply_state<B>(model: &mut UiModel<B>, id: [u8; 32], state: MessageState) {
    let shown = delivery_of(state);
    let now = model.now;
    let running = model.sending.clone();
    for (c, conversation) in model.conversations.iter_mut().enumerate() {
        for (m, message) in conversation.messages.iter_mut().enumerate() {
            if !message.outbound || message.id != Some(id) {
                continue;
            }
            if gives_up(shown) && running.contains(&(c, m)) {
                // The runner may still carry it another way.
                continue;
            }
            if message.unproven
                && !matches!(shown, DeliveryState::Delivered | DeliveryState::Rejected)
            {
                continue;
            }
            if message.delivery != Some(shown) {
                let words = ledger_words(shown);
                let line = match message.last_leg {
                    Some(leg) => format!("{words} ({})", leg_words(leg)),
                    None => words.to_string(),
                };
                message.note(now, &line);
            }
            message.delivery = Some(shown);
        }
    }
}

fn on_send_done<B>(model: &mut UiModel<B>, result: Result<(), String>) -> Vec<UiEffect> {
    let Some((c, m)) = model.sending.pop_front() else {
        return Vec::new();
    };
    if let Some(message) = model.message_mut(c, m) {
        message.leg = None;
    }
    let Err(detail) = result else {
        return Vec::new();
    };
    // Each leg's reason as the runner worded it, its continuation lines cut:
    // an auto send that failed both ways says why each failed, not only why
    // the direct one did.
    let reasons: Vec<&str> = detail
        .lines()
        .filter(|line| !line.starts_with(' '))
        .collect();
    let text = format!("not sent: {}", reasons.join("; "));
    let now = model.now;
    if let Some(message) = model.message_mut(c, m) {
        message.delivery = Some(DeliveryState::Failed);
        message.note(now, &text);
    }
    model.toast(ToastKind::Error, text);
    vec![UiEffect::Notice(format!("send: {detail}"))]
}

fn on_send_unproven<B>(model: &mut UiModel<B>, detail: String) -> Vec<UiEffect> {
    let Some((c, m)) = model.sending.pop_front() else {
        return Vec::new();
    };
    let now = model.now;
    if let Some(message) = model.message_mut(c, m) {
        message.leg = None;
        message.unproven = true;
        message.note(now, "sent over a link, no delivery proof yet");
        // A proof that came in the same breath as the runner's end stays.
        if message.delivery != Some(DeliveryState::Delivered) {
            message.delivery = Some(DeliveryState::Sent);
        }
    }
    model.toast(
        ToastKind::Info,
        "sent over a link, no delivery proof yet".to_string(),
    );
    vec![UiEffect::Notice(format!("send: {detail}"))]
}

/// What a footer button runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FooterAct(pub Option<PaletteCommand>);

impl FooterRole for FooterAct {
    fn is_nav(&self) -> bool {
        false
    }
}

/// The palette as drawn.
#[derive(Clone, Debug)]
pub struct PaletteView {
    /// The query line.
    pub query: String,
    /// The cursor in it, in characters.
    pub cursor: usize,
    /// The matching names, the highlighted one bold behind `▸`.
    pub rows: Vec<RLine>,
}

/// A message's detail view as drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailView {
    /// The overlay's title.
    pub title: String,
    /// Its lines: who, the peer, the id, the size, the state, the ledger.
    pub rows: Vec<String>,
}

/// What each part of the screen shows.
#[derive(Clone, Debug)]
pub struct View {
    /// The conversations pane's title: our own address, short.
    pub conversations_title: String,
    /// The list's rows on screen, two a conversation and a blank between.
    pub conversations: Vec<RLine>,
    /// The conversation's header: the peer's name and trust state.
    pub messages_title: String,
    /// The header's right end: the mailbox state, or the peer's full address
    /// on a wide pane, where the mailbox has a row of its own.
    pub messages_title_right: String,
    /// The conversation pane's rows, exactly its height: the header's rows on
    /// a wide pane, then the blocks, the newest at the bottom unless the
    /// reader scrolled up.
    pub messages: Vec<RLine>,
    /// The compose box's title: whom it writes to.
    pub compose_title: String,
    /// The compose line and its cursor, in characters.
    pub compose: String,
    pub compose_cursor: usize,
    /// The compose pane's border hint: the chords that send and break the
    /// line in this terminal and setting ([`keys::compose_hint`]).
    pub compose_hint: String,
    /// Where the focus is: the renderer marks that pane.
    pub focus: Pane,
    /// The footer buttons.
    pub footer: Vec<FooterItem<FooterAct>>,
    /// The toast to float, if any.
    pub toast: Option<Toast>,
    /// The palette, when open.
    pub palette: Option<PaletteView>,
    /// Whether the help overlay is up; its text is [`help_groups`].
    pub help: bool,
    /// The detail view, when open.
    pub detail: Option<DetailView>,
}

/// The empty conversations pane's line.
pub const NO_CONVERSATIONS: &str = "No conversations: start with --to <address>.";

/// The palette the delivery tones are drawn in.
pub fn tone_rgb(tone: Tone, dim: bool) -> (u8, u8, u8) {
    let (r, g, b) = match tone {
        Tone::Gray => (150, 150, 150),
        Tone::Yellow => (230, 200, 60),
        Tone::Amber => (255, 170, 0),
        Tone::Blue => (90, 150, 255),
        Tone::Green => (80, 200, 120),
        Tone::Red => (230, 80, 80),
    };
    match dim {
        true => (r / 3 * 2, g / 3 * 2, b / 3 * 2),
        false => (r, g, b),
    }
}

/// An address cut to what a pane shows.
pub fn short(address: &[u8; 16]) -> String {
    let mut hex = to_hex(address);
    hex.truncate(SHORT_HEX);
    hex
}

/// A peer as the list, the header and the compose box name it until names
/// arrive from announces: the address's first eight hex digits in angle
/// brackets (section 5, "The elements").
pub fn name(address: &[u8; 16]) -> String {
    format!("<{}…>", to_hex(&address[..4]))
}

/// The trust state the header says in words. Every peer is `unknown` until
/// names and petnames arrive (section 7, slice 5).
pub const TRUST_UNKNOWN: &str = "unknown";

fn push(line: &mut RLine, text: &str, st: RStyle) {
    line.cells.extend(text.chars().map(|ch| StyledChar {
        ch,
        st,
        link: None,
        field: None,
    }));
}

fn plain(text: &str) -> RLine {
    let mut line = RLine::default();
    push(&mut line, text, RStyle::default());
    line
}

fn bold() -> RStyle {
    RStyle {
        bold: true,
        ..RStyle::default()
    }
}

/// The mailbox state in the header, longest first: the renderer takes the
/// first that fits.
fn mailbox_words<B>(model: &UiModel<B>) -> Vec<String> {
    let Some(node) = model.mailbox.node else {
        return vec!["no mailbox".to_string()];
    };
    let node = to_hex(&node[..4]);
    let how = if model.mailbox.by_user {
        "chosen"
    } else {
        "adopted"
    };
    let sync = match (model.fetching, model.mailbox.synced, model.now) {
        (true, _, _) => Some("fetching".to_string()),
        (false, Some(at), Some(now)) => Some(format!("synced {}", list_time(&at, &now))),
        (false, Some(at), None) => Some(format!("synced {}", at.hh_mm())),
        (false, None, _) => None,
    };
    match sync {
        Some(sync) => vec![
            format!("mailbox {node} {how} · {sync}"),
            format!("mailbox · {sync}"),
            format!("mailbox {node}"),
            "mailbox".to_string(),
        ],
        None => vec![
            format!("mailbox {node} {how}"),
            format!("mailbox {node}"),
            "mailbox".to_string(),
        ],
    }
}

/// The detail view of message `m` of conversation `c`.
fn detail_view<B>(model: &UiModel<B>, c: usize, m: usize) -> Option<DetailView> {
    let conversation = model.conversations.get(c)?;
    let message = conversation.messages.get(m)?;
    let who = name(&conversation.peer);
    let mut rows = vec![
        match message.outbound {
            true => format!("from you to {who}"),
            false => format!("from {who} to you"),
        },
        format!("peer   {}", to_hex(&conversation.peer)),
        match message.id {
            Some(id) => format!("id     {}", to_hex(&id)),
            None => "id     none yet: the router has not queued it".to_string(),
        },
        format!("size   {} bytes", message.size),
        match (message.delivery, message.verified) {
            (Some(state), _) => format!("state  {}", state_words(state)),
            (None, true) => "signature verified".to_string(),
            (None, false) => "signature NOT verified".to_string(),
        },
        String::new(),
        "this session".to_string(),
    ];
    for entry in &message.ledger {
        let at = entry
            .at
            .map(|at| at.hh_mm_ss())
            .unwrap_or_else(|| "--:--:--".to_string());
        rows.push(format!("{at}  {}", entry.what));
    }
    Some(DetailView {
        title: "message".to_string(),
        rows,
    })
}

/// The screen.
pub fn view<B: InputBuffer>(model: &UiModel<B>) -> View {
    let sizes = model.sizes();
    let conversations = match model.conversations.is_empty() {
        true => wrap(&[plain(NO_CONVERSATIONS)], sizes.list.0, 0),
        false => layout::list(model, sizes.list.0, sizes.list.1),
    };
    let wide = sizes.messages.0 >= WIDE_HEADER;
    let peer = model.conversations.get(model.selected).map(|c| c.peer);
    let messages_title = match peer {
        Some(peer) => format!("{}  {TRUST_UNKNOWN}", name(&peer)),
        None => "messages".to_string(),
    };
    let mailbox = mailbox_words(model);
    // Room for the right end: the pane less its corners, the title with its
    // focus mark and the spaces around both.
    let room = sizes
        .messages
        .0
        .saturating_sub(messages_title.chars().count() + 6);
    let messages_title_right = match (wide, peer) {
        (true, Some(peer)) => to_hex(&peer),
        _ => mailbox
            .iter()
            .find(|words| words.chars().count() <= room)
            .cloned()
            .unwrap_or_default(),
    };
    let laid = model.laid();
    let rows = model.message_rows();
    let top = model.top_of(laid.rows.len());
    let mut messages = Vec::with_capacity(sizes.messages.1);
    if wide {
        let width = sizes.messages.0;
        let words = mailbox.first().cloned().unwrap_or_default();
        let pad = width.saturating_sub(words.chars().count() + 1);
        messages.push(plain(&format!("{}{words}", " ".repeat(pad))));
        messages.push(plain(&format!(" {} ", "─".repeat(width.saturating_sub(2)))));
    }
    let shown = laid.rows.len().saturating_sub(top).min(rows);
    messages.extend((shown..rows).map(|_| RLine::default()));
    messages.extend(laid.rows.into_iter().skip(top).take(shown));
    let compose_title = match peer {
        Some(peer) => format!("to {}", name(&peer)),
        None => "compose".to_string(),
    };
    let ctx = model.ctx();
    let palette = model.palette.as_ref().map(|p| PaletteView {
        query: p.query.value().to_string(),
        cursor: p.query.cursor(),
        rows: p
            .matches()
            .iter()
            .enumerate()
            .map(|(i, e)| {
                // The name, and the row's first chord this terminal sends.
                let shown = match keys::palette_key(e.name, ctx) {
                    Some(key) => format!("{}  {key}", e.name),
                    None => e.name.to_string(),
                };
                let mut line = RLine::default();
                match i == p.selected {
                    true => push(&mut line, &format!("▸ {shown}"), bold()),
                    false => push(&mut line, &format!("  {shown}"), RStyle::default()),
                }
                line
            })
            .collect(),
    });
    View {
        conversations_title: match model.address {
            Some(address) => format!("lnmsg {}", &to_hex(&address)[..8]),
            None => "lnmsg (attaching)".to_string(),
        },
        conversations,
        messages_title,
        messages_title_right,
        messages,
        compose_title,
        compose: model.compose.value().to_string(),
        compose_cursor: model.compose.cursor(),
        compose_hint: keys::compose_hint(ctx),
        focus: model.focus,
        footer: footer(model),
        toast: model.current_toast().cloned(),
        palette,
        help: model.help,
        detail: model.detail.and_then(|(c, m)| detail_view(model, c, m)),
    }
}

/// `direct 12 s` or `via mailbox 40 s`: which leg a waiting send is on and
/// for how long, while its mark still says it is waiting. Gone the moment it
/// is handed on, so the label never sits beside a `◇` or a `✓`.
fn leg_label(message: &Message, state: DeliveryState, tick: u64) -> Option<String> {
    let (leg, since) = message.leg?;
    if !matches!(
        state,
        DeliveryState::Queued | DeliveryState::Stamping | DeliveryState::InFlight
    ) {
        return None;
    }
    let secs = tick.saturating_sub(since) / TICKS_PER_SECOND;
    Some(format!("{} {secs} s", leg_words(leg)))
}

/// Cut `lines` to rows of at most `width` cells, every row after a line's
/// first set in by `indent`, and the cells keeping their styles. A row the
/// indent alone would fill is not indented, so a narrow pane still shows
/// text.
pub fn wrap(lines: &[RLine], width: usize, indent: usize) -> Vec<RLine> {
    let width = width.max(1);
    let indent = if indent < width { indent } else { 0 };
    let mut rows = Vec::new();
    for line in lines {
        let mut cells = line.cells.as_slice();
        let mut first = true;
        loop {
            let pad = if first { 0 } else { indent };
            let room = width - pad;
            // At the last space that fits, which the break then swallows; a
            // word longer than the row is cut where the row ends.
            let (take, skip) = if cells.len() <= room {
                (cells.len(), 0)
            } else {
                match cells[..=room].iter().rposition(|cell| cell.ch == ' ') {
                    Some(space) if space > 0 => (space, 1),
                    _ => (room, 0),
                }
            };
            let mut row = RLine::default();
            push(&mut row, &" ".repeat(pad), RStyle::default());
            row.cells.extend_from_slice(&cells[..take]);
            rows.push(row);
            cells = &cells[take + skip..];
            first = false;
            if cells.is_empty() {
                break;
            }
        }
    }
    rows
}

/// The most rows a toast takes: enough for both legs of a failed auto send
/// at 80 columns, few enough to leave the messages pane readable.
pub const TOAST_MAX_ROWS: usize = 3;

/// `text` wrapped to `width` at word boundaries with [`wrap`], as a block
/// wraps a message, in at most
/// [`TOAST_MAX_ROWS`] rows: what does not fit is cut at the end of the last
/// row with a `…`.
pub fn toast_rows(text: &str, width: usize) -> Vec<String> {
    let as_text = |row: &RLine| row.cells.iter().map(|cell| cell.ch).collect::<String>();
    let mut rows: Vec<String> = wrap(&[plain(text)], width, 0).iter().map(as_text).collect();
    if rows.len() > TOAST_MAX_ROWS {
        let rest = rows.split_off(TOAST_MAX_ROWS - 1).join(" ");
        rows.push(leviculum_ui::text::truncate_to_cols(&rest, width));
    }
    rows
}

/// The footer: the palette's own two keys while it is open, else the rows
/// with a footer label that have a chord in the focused pane.
fn footer<B>(model: &UiModel<B>) -> Vec<FooterItem<FooterAct>> {
    if model.palette.is_some() {
        return vec![
            FooterItem::action("Enter", "run", FooterAct(None)),
            FooterItem::action("Esc", "close", FooterAct(None)),
        ];
    }
    keys::footer(keys::panes_of(model.focus), model.ctx(), FooterAct)
}

/// The help overlay's text, every row of the keymap ([`keys::help_groups`]).
pub fn help_groups() -> Vec<HelpGroup> {
    keys::help_groups()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal field for the model's tests: insert and backspace, the two
    /// edits these tests make, and `Ctrl-U`. The terminal's field is `tui_input::Input`.
    #[derive(Default, Clone, Debug)]
    struct Field(String);

    impl InputBuffer for Field {
        fn value(&self) -> &str {
            &self.0
        }
        fn cursor(&self) -> usize {
            self.0.chars().count()
        }
        fn edit(&mut self, key: &KeyEvent) -> bool {
            match key.code {
                KeyCode::Char(c)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    self.0.push(c);
                    true
                }
                KeyCode::Backspace => {
                    self.0.pop();
                    true
                }
                KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                    self.0.clear();
                    true
                }
                _ => false,
            }
        }
        fn take(&mut self) -> String {
            core::mem::take(&mut self.0)
        }
        fn newline(&mut self) {
            self.0.push('\n');
        }
    }

    const PEER: [u8; 16] = [0xab; 16];
    const OTHER: [u8; 16] = [0xcd; 16];
    const ID: [u8; 32] = [0x11; 32];
    const ID2: [u8; 32] = [0x22; 32];

    fn key(code: KeyCode) -> UiEvent {
        UiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl_key(c: char) -> UiEvent {
        UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn typed(model: &mut UiModel<Field>, text: &str) {
        for c in text.chars() {
            update(model, key(KeyCode::Char(c)));
        }
    }

    fn engine(model: &mut UiModel<Field>, event: OutboxEvent) {
        update(model, UiEvent::Engine(event));
    }

    fn state(model: &mut UiModel<Field>, id: [u8; 32], state: MessageState) {
        engine(
            model,
            OutboxEvent::State {
                message_id: id,
                state,
            },
        );
    }

    fn last_delivery(model: &UiModel<Field>) -> Option<DeliveryState> {
        model.messages_of_selected().last().and_then(|m| m.delivery)
    }

    /// Compose a message to the first conversation and press Enter.
    fn sent(model: &mut UiModel<Field>, body: &str) -> Vec<UiEffect> {
        update(model, key(KeyCode::Tab));
        update(model, key(KeyCode::Tab));
        assert_eq!(model.focus, Pane::Compose);
        typed(model, body);
        update(model, key(KeyCode::Enter))
    }

    #[test]
    fn tab_and_shift_tab_cycle_the_three_panes() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(model.focus);
            update(&mut model, key(KeyCode::Tab));
        }
        assert_eq!(seen, [Pane::Conversations, Pane::Messages, Pane::Compose]);
        assert_eq!(model.focus, Pane::Conversations);
        update(&mut model, key(KeyCode::BackTab));
        assert_eq!(model.focus, Pane::Compose);
    }

    #[test]
    fn compose_takes_plain_typing_and_q_does_not_quit_there() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        typed(&mut model, "quit?");
        assert!(!model.quit);
        assert!(!model.help);
        assert_eq!(model.compose.value(), "quit?");
        update(&mut model, key(KeyCode::Esc));
        assert_eq!(model.focus, Pane::Messages);
        let effects = update(&mut model, key(KeyCode::Char('q')));
        assert!(model.quit);
        assert_eq!(effects, [UiEffect::Quit]);
    }

    #[test]
    fn ctrl_c_clears_compose_instead_of_quitting_and_ctrl_q_quits() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        typed(&mut model, "draft");
        assert!(update(&mut model, ctrl_key('c')).is_empty());
        assert!(!model.quit);
        assert_eq!(model.compose.value(), "");
        assert_eq!(update(&mut model, ctrl_key('q')), [UiEffect::Quit]);
        assert!(model.quit);
    }

    #[test]
    fn colon_opens_the_palette_in_a_command_pane_and_types_in_compose() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Char(':')));
        assert!(model.palette.is_some());
        update(&mut model, key(KeyCode::Esc));
        update(&mut model, key(KeyCode::BackTab));
        update(&mut model, key(KeyCode::Char(':')));
        assert!(model.palette.is_none());
        assert_eq!(model.compose.value(), ":");
    }

    #[test]
    fn enter_sends_to_the_selected_conversation_and_clears_the_line() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        update(&mut model, key(KeyCode::Char('j')));
        assert_eq!(model.selected, 1);
        let effects = sent(&mut model, "hello");
        assert_eq!(
            effects,
            [UiEffect::Send {
                to: OTHER,
                body: "hello".to_string()
            }]
        );
        assert_eq!(model.compose.value(), "");
        assert_eq!(last_delivery(&model), Some(DeliveryState::Queued));
        // An empty line sends nothing.
        assert!(update(&mut model, key(KeyCode::Enter)).is_empty());
    }

    #[test]
    fn the_glyph_follows_the_router_states_of_the_message_it_queued() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        state(&mut model, ID, MessageState::Sending);
        assert_eq!(last_delivery(&model), Some(DeliveryState::InFlight));
        state(&mut model, ID, MessageState::Sent);
        assert_eq!(last_delivery(&model), Some(DeliveryState::Sent));
        update(&mut model, UiEvent::SendDone(Ok(())));
        assert!(model.sending.is_empty());
        // The proof arrives after the runner has finished.
        state(&mut model, ID, MessageState::Delivered);
        assert_eq!(last_delivery(&model), Some(DeliveryState::Delivered));
        // A state for some other id touches nothing.
        state(&mut model, ID2, MessageState::Failed);
        assert_eq!(last_delivery(&model), Some(DeliveryState::Delivered));
    }

    #[test]
    fn an_auto_fallback_never_shows_the_cancelled_direct_leg_as_failed() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        state(&mut model, ID, MessageState::Cancelled);
        assert_eq!(last_delivery(&model), Some(DeliveryState::Queued));
        engine(&mut model, OutboxEvent::Queued { message_id: ID2 });
        state(&mut model, ID2, MessageState::AwaitingCollection);
        assert_eq!(last_delivery(&model), Some(DeliveryState::InMailbox));
        update(&mut model, UiEvent::SendDone(Ok(())));
        assert_eq!(last_delivery(&model), Some(DeliveryState::InMailbox));
    }

    #[test]
    fn a_runner_that_gave_up_marks_the_message_failed_and_keeps_the_text() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        let effects = update(
            &mut model,
            UiEvent::SendDone(Err("no route\n  more".to_string())),
        );
        assert_eq!(last_delivery(&model), Some(DeliveryState::Failed));
        assert_eq!(
            effects,
            [UiEffect::Notice("send: no route\n  more".to_string())]
        );
        assert_eq!(
            model.current_toast().map(|t| t.text.as_str()),
            Some("not sent: no route")
        );
    }

    #[test]
    fn a_toast_quotes_both_legs_of_a_failed_auto_send() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        update(
            &mut model,
            UiEvent::SendDone(Err(
                "direct: no route after 30.0s\n  more\npropagated: no propagation node".to_string(),
            )),
        );
        assert_eq!(
            model.current_toast().map(|t| t.text.as_str()),
            Some("not sent: direct: no route after 30.0s; propagated: no propagation node")
        );
    }

    /// The policy half of leviculum 1002 on the screen: a message the runner
    /// left on a link without a proof is `◇` and stays `◇` through the
    /// router's retries and its giving up, with no red cross; a late proof
    /// still turns it `✓`.
    #[test]
    fn an_unproven_send_stays_sent_until_a_proof_and_never_turns_red() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        state(&mut model, ID, MessageState::Sending);
        let effects = update(
            &mut model,
            UiEvent::SendUnproven("went onto a link".to_string()),
        );
        assert_eq!(last_delivery(&model), Some(DeliveryState::Sent));
        assert!(model.sending.is_empty());
        assert_eq!(
            effects,
            [UiEffect::Notice("send: went onto a link".to_string())]
        );
        for retry in [
            MessageState::Outbound,
            MessageState::Sending,
            MessageState::Failed,
        ] {
            state(&mut model, ID, retry);
            assert_eq!(
                last_delivery(&model),
                Some(DeliveryState::Sent),
                "{retry:?}"
            );
        }
        engine(
            &mut model,
            OutboxEvent::Left {
                message_id: ID,
                last: Some(MessageState::Failed),
            },
        );
        assert_eq!(last_delivery(&model), Some(DeliveryState::Sent));

        let mut late = UiModel::<Field>::new(&[PEER]);
        sent(&mut late, "hi");
        engine(&mut late, OutboxEvent::Queued { message_id: ID });
        update(&mut late, UiEvent::SendUnproven(String::new()));
        state(&mut late, ID, MessageState::Delivered);
        assert_eq!(last_delivery(&late), Some(DeliveryState::Delivered));
    }

    /// The row names the leg and its seconds while the send waits, and drops
    /// both once the message is handed on.
    #[test]
    fn a_waiting_send_shows_its_leg_and_the_seconds_on_it() {
        let text =
            |model: &UiModel<Field>| -> String { shown(model).last().cloned().expect("a message") };
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        update(&mut model, UiEvent::SendLeg(Leg::Direct));
        for _ in 0..12 {
            update(&mut model, UiEvent::Tick);
        }
        assert!(text(&model) == "hi  direct 3 s ○", "{}", text(&model));
        update(&mut model, UiEvent::SendLeg(Leg::Propagated));
        for _ in 0..8 {
            update(&mut model, UiEvent::Tick);
        }
        assert!(text(&model) == "hi  via mailbox 2 s ○", "{}", text(&model));
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        state(&mut model, ID, MessageState::AwaitingCollection);
        assert_eq!(text(&model), "hi  ⌂");
    }

    #[test]
    fn the_mapping_is_the_documented_table() {
        use MessageState::*;
        let rows = [
            (Generating, DeliveryState::Queued),
            (Outbound, DeliveryState::Queued),
            (Sending, DeliveryState::InFlight),
            (Sent, DeliveryState::Sent),
            (AwaitingCollection, DeliveryState::InMailbox),
            (Delivered, DeliveryState::Delivered),
            (Rejected, DeliveryState::Rejected),
            (Cancelled, DeliveryState::Failed),
            (Failed, DeliveryState::Failed),
        ];
        for (router, shown) in rows {
            assert_eq!(delivery_of(router), shown, "{router:?}");
        }
    }

    #[test]
    fn a_received_message_opens_its_conversation_and_counts_unread() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        let received = OutboxEvent::Received {
            message_id: ID,
            source: OTHER,
            title: Vec::new(),
            body: b"hey".to_vec(),
            verified: true,
        };
        engine(&mut model, received.clone());
        engine(&mut model, received);
        assert_eq!(model.conversations.len(), 2);
        assert_eq!(model.conversations[1].messages.len(), 1, "deduplicated");
        assert_eq!(model.conversations[1].unread, 1);
        // Most recent first: the new conversation is on top.
        assert_eq!(model.order(), [1, 0]);
        update(&mut model, key(KeyCode::Char('g')));
        assert_eq!(model.selected, 1);
        assert_eq!(model.conversations[1].unread, 0);
        assert_eq!(model.messages_of_selected()[0].body, "hey");
    }

    #[test]
    fn a_reader_at_the_newest_message_follows_a_new_one() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        let from_peer = |id: [u8; 32]| OutboxEvent::Received {
            message_id: id,
            source: PEER,
            title: Vec::new(),
            body: b"hey".to_vec(),
            verified: true,
        };
        engine(&mut model, from_peer(ID));
        assert_eq!(model.message_cursor, 0);
        engine(&mut model, from_peer(ID2));
        assert_eq!(model.message_cursor, 1, "followed");
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Char('g')));
        engine(&mut model, from_peer([0x33; 32]));
        assert_eq!(model.message_cursor, 0, "a reader scrolled up stays put");
    }

    fn alt_key(c: char) -> UiEvent {
        UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT))
    }

    #[test]
    fn the_palette_filters_runs_and_closes() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Char(':')));
        assert!(model.palette.is_some());
        typed(&mut model, "foc");
        update(&mut model, key(KeyCode::Down));
        update(&mut model, key(KeyCode::Down));
        update(&mut model, key(KeyCode::Enter));
        assert!(model.palette.is_none());
        assert_eq!(model.focus, Pane::Compose);
        // From compose the palette is Alt-x.
        update(&mut model, alt_key('x'));
        typed(&mut model, "fetch");
        assert_eq!(update(&mut model, key(KeyCode::Enter)), [UiEffect::Fetch]);
        assert!(model.fetching);
        update(&mut model, alt_key('x'));
        update(&mut model, key(KeyCode::Esc));
        assert!(model.palette.is_none());
        assert_eq!(
            model.focus,
            Pane::Compose,
            "closing the palette keeps the pane"
        );
    }

    /// `:` `q` `Enter` quits, as a vi reader expects.
    #[test]
    fn colon_q_enter_quits() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Char(':')));
        typed(&mut model, "q");
        assert_eq!(update(&mut model, key(KeyCode::Enter)), [UiEffect::Quit]);
        assert!(model.quit);
    }

    /// Decided change 1: `Enter` on a message opens its detail view; compose
    /// is `i`, `a`, `r`.
    #[test]
    fn enter_on_a_message_shows_its_details_and_i_a_r_compose() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        update(&mut model, key(KeyCode::Esc));
        assert_eq!(model.focus, Pane::Messages);
        assert!(update(&mut model, key(KeyCode::Enter)).is_empty());
        assert_eq!(model.focus, Pane::Messages, "Enter no longer composes");
        let detail = view(&model).detail.expect("the detail view");
        assert!(
            detail.rows.contains(&"state  queued".to_string()),
            "{detail:?}"
        );
        update(&mut model, key(KeyCode::Esc));
        assert!(model.detail.is_none());
        for c in ['i', 'a', 'r'] {
            model.focus = Pane::Messages;
            update(&mut model, key(KeyCode::Char(c)));
            assert_eq!(model.focus, Pane::Compose, "{c}");
        }
        // From the list too, and Enter there opens the conversation.
        model.focus = Pane::Conversations;
        update(&mut model, key(KeyCode::Enter));
        assert_eq!(model.focus, Pane::Messages);
    }

    /// Decided change 2: `Ctrl-p` and `Ctrl-n` in compose are the buffer's
    /// (line motions), not the palette; in a command pane they move the
    /// cursor, as in lnomad.
    #[test]
    fn ctrl_p_is_a_motion_everywhere_and_never_the_palette() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        update(&mut model, ctrl_key('n'));
        assert_eq!(model.selected, 1);
        update(&mut model, ctrl_key('p'));
        assert_eq!(model.selected, 0);
        assert!(model.palette.is_none());
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        typed(&mut model, "x");
        update(&mut model, ctrl_key('p'));
        assert!(model.palette.is_none());
        assert_eq!(model.compose.value(), "x");
    }

    /// Decided change 3: `Ctrl-c` quits nowhere. Outside compose it says
    /// what does, and over an overlay too.
    #[test]
    fn ctrl_c_quits_nowhere_and_says_ctrl_q_does() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        assert!(update(&mut model, ctrl_key('c')).is_empty());
        assert!(!model.quit);
        assert_eq!(
            model.current_toast().map(|t| t.text.as_str()),
            Some("Ctrl-q quits")
        );
        update(&mut model, key(KeyCode::Char('?')));
        assert!(update(&mut model, ctrl_key('c')).is_empty());
        assert!(!model.quit);
        assert_eq!(update(&mut model, ctrl_key('q')), [UiEffect::Quit]);
    }

    /// Decided change 4: with `send_on_enter` off, `Enter` breaks the line
    /// and `Alt-Enter` sends; the palette toggles it and says the new rule.
    #[test]
    fn send_on_enter_off_swaps_enter_and_alt_enter() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Char(':')));
        typed(&mut model, "send on enter");
        update(&mut model, key(KeyCode::Enter));
        assert!(!model.send_on_enter);
        assert_eq!(
            model.current_toast().map(|t| t.text.as_str()),
            Some("Alt-Enter send · Enter newline")
        );
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        typed(&mut model, "one");
        assert!(update(&mut model, key(KeyCode::Enter)).is_empty());
        typed(&mut model, "two");
        assert_eq!(view(&model).compose_hint, "Alt-Enter send · Enter newline");
        let effects = update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
        );
        assert_eq!(
            effects,
            [UiEffect::Send {
                to: PEER,
                body: "one\ntwo".to_string()
            }]
        );
        model.enhanced = true;
        assert_eq!(view(&model).compose_hint, "Ctrl-Enter send · Enter newline");
    }

    /// The Enter rule under the enhancement protocol: `Shift-Enter` breaks
    /// the line, `Ctrl-Enter` sends and never breaks it, and the hint says
    /// so; `Ctrl-j` breaks the line in every terminal.
    #[test]
    fn shift_enter_breaks_ctrl_enter_sends_and_ctrl_j_breaks_everywhere() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.enhanced = true;
        assert_eq!(
            view(&model).compose_hint,
            "Enter send · Shift-Enter newline"
        );
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        typed(&mut model, "a");
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
        );
        typed(&mut model, "b");
        update(&mut model, ctrl_key('j'));
        typed(&mut model, "c");
        let effects = update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
        );
        assert_eq!(
            effects,
            [UiEffect::Send {
                to: PEER,
                body: "a\nb\nc".to_string()
            }]
        );
        model.enhanced = false;
        assert_eq!(view(&model).compose_hint, "Enter send · Alt-Enter newline");
    }

    /// Signal's conversation keys: `Alt-↓`/`Alt-↑` step, `Alt-Shift-↓` jumps
    /// to the next unread, from any pane.
    #[test]
    fn alt_arrows_step_conversations_and_alt_shift_finds_unread() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER, [0xef; 16]]);
        let alt = |code| UiEvent::Key(KeyEvent::new(code, KeyModifiers::ALT));
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, alt(KeyCode::Down));
        assert_eq!((model.selected, model.focus), (1, Pane::Compose));
        update(&mut model, alt(KeyCode::Up));
        assert_eq!(model.selected, 0);
        model.conversations[2].unread = 1;
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::ALT | KeyModifiers::SHIFT,
            )),
        );
        assert_eq!(model.selected, 2);
        assert_eq!(model.conversations[2].unread, 0);
    }

    /// Help, footer and palette all read the table: the footer's keys move
    /// with the pane, the palette shows each name's chord, the help lists
    /// every row.
    #[test]
    fn help_footer_and_palette_derive_from_the_table() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        let footer_keys = |model: &UiModel<Field>| -> Vec<(&str, &str)> {
            view(model)
                .footer
                .iter()
                .map(|f| (f.key, f.label))
                .collect()
        };
        assert_eq!(
            footer_keys(&model),
            [
                ("Enter", "open"),
                ("i", "write"),
                ("Tab", "pane"),
                (":", "commands"),
                ("?", "help"),
                ("q", "quit")
            ]
        );
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        assert_eq!(
            footer_keys(&model),
            [
                ("Enter", "send"),
                ("Tab", "pane"),
                ("Esc", "back"),
                ("Alt-x", "commands"),
                ("F1", "help"),
                ("Ctrl-q", "quit")
            ]
        );
        update(&mut model, alt_key('x'));
        let rows: Vec<String> = view(&model)
            .palette
            .map(|p| p.rows.iter().map(text).collect())
            .unwrap_or_default();
        assert_eq!(rows[0], "▸ send  Enter");
        assert!(rows.contains(&"  quit  q".to_string()), "{rows:?}");
        let help: Vec<&str> = help_groups()
            .iter()
            .flat_map(|g| g.entries.iter().map(|e| e.desc))
            .collect();
        assert!(help.contains(&"scroll a line"));
        assert!(help.contains(&"the message's details"));
    }

    #[test]
    fn help_opens_from_a_command_pane_and_closes_on_esc() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Char('?')));
        assert!(model.help);
        assert!(view(&model).help);
        update(&mut model, key(KeyCode::Char('j')));
        assert!(model.help, "keys do not leak through the overlay");
        update(&mut model, key(KeyCode::Esc));
        assert!(!model.help);
    }

    #[test]
    fn toasts_age_out_on_the_tick() {
        let mut model = UiModel::<Field>::new(&[]);
        sent(&mut model, "hi");
        assert!(model.current_toast().is_some());
        for _ in 0..TOAST_TICKS {
            update(&mut model, UiEvent::Tick);
        }
        assert!(model.current_toast().is_none());
        assert!(model.toasts.is_empty());
    }

    fn text(line: &RLine) -> String {
        line.cells.iter().map(|c| c.ch).collect()
    }

    /// The conversation pane's rows that carry something, as text, trimmed.
    fn shown(model: &UiModel<Field>) -> Vec<String> {
        view(model)
            .messages
            .iter()
            .map(text)
            .map(|row| row.trim().to_string())
            .filter(|row| !row.is_empty())
            .collect()
    }

    #[test]
    fn the_view_draws_the_glyph_and_says_nothing_is_earlier() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        assert_eq!(shown(&model), [layout::EARLIEST]);
        assert_eq!(view(&model).messages_title, "<abababab…>  unknown");
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        assert_eq!(shown(&model), [layout::EARLIEST, "hi  ○"]);
        state(&mut model, ID, MessageState::Delivered);
        assert_eq!(shown(&model)[1], "hi  ✓");
        model.ascii = true;
        assert_eq!(shown(&model)[1], "hi  v");
        assert_eq!(
            text(&view(&model).conversations[0]).trim_end(),
            "▌<abababab…>"
        );
        assert_eq!(text(&view(&model).conversations[1]).trim_end(), "▌you: hi");
    }

    /// `Alt-Enter` puts a line break in the compose pane, `Enter` still
    /// sends, and the sent body keeps its lines (leviculum 1002).
    #[test]
    fn alt_enter_breaks_the_line_and_enter_sends_both_lines() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        typed(&mut model, "one");
        let effects = update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
        );
        assert!(effects.is_empty(), "Alt-Enter must not send: {effects:?}");
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
        );
        typed(&mut model, "three");
        assert_eq!(model.compose.value(), "one\n\nthree");
        let effects = update(&mut model, key(KeyCode::Enter));
        assert_eq!(
            effects,
            [UiEffect::Send {
                to: PEER,
                body: "one\n\nthree".to_string(),
            }]
        );
    }

    /// A body with line breaks is drawn as its lines in one block, the mark
    /// closing the last, both ways.
    #[test]
    fn a_multi_line_body_is_its_lines_in_one_block_with_the_mark_last() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "");
        model.compose.0 = "first\nsecond".to_string();
        update(&mut model, key(KeyCode::Enter));
        engine(
            &mut model,
            OutboxEvent::Received {
                message_id: ID2,
                source: PEER,
                title: Vec::new(),
                body: b"back\nagain".to_vec(),
                verified: true,
            },
        );
        assert_eq!(
            shown(&model),
            [layout::EARLIEST, "first", "second  ○", "back", "again"]
        );
    }

    #[test]
    fn a_toast_wraps_at_words_to_three_rows_and_cuts_the_rest() {
        assert_eq!(toast_rows("short", 20), ["short"]);
        assert_eq!(
            toast_rows("not sent: direct: no route; propagated: none", 20),
            ["not sent: direct: no", "route; propagated:", "none"]
        );
        let long = toast_rows(&"word ".repeat(20), 12);
        assert_eq!(long.len(), TOAST_MAX_ROWS, "{long:?}");
        assert!(long[2].ends_with('…'), "{long:?}");
        assert!(long.iter().all(|row| row.chars().count() <= 12), "{long:?}");
    }

    #[test]
    fn wrap_cuts_to_the_width_and_sets_the_rest_in() {
        let rows: Vec<String> = wrap(&[plain("abcdefghij"), plain("")], 6, 2)
            .iter()
            .map(text)
            .collect();
        assert_eq!(rows, ["abcdef", "  ghij", ""]);
        // At a space when one fits, and the space goes with the break.
        let rows: Vec<String> = wrap(&[plain("ab cd efgh")], 6, 2)
            .iter()
            .map(text)
            .collect();
        assert_eq!(rows, ["ab cd", "  efgh"]);
        // An indent as wide as the pane would leave no room: none then.
        let rows: Vec<String> = wrap(&[plain("abcd")], 2, 2).iter().map(text).collect();
        assert_eq!(rows, ["ab", "cd"]);
    }

    // The chat layout (lnmsg-keys-and-layout.md, section 5; slice 2).

    /// 2026-10-26 at `hour:minute`, an hour east of UTC.
    fn at(day: u32, hour: u32, minute: u32) -> WallTime {
        WallTime::from_civil(2026, 10, day, hour, minute, 3600)
    }

    fn clock(model: &mut UiModel<Field>, day: u32, hour: u32, minute: u32) {
        update(model, UiEvent::Clock(at(day, hour, minute)));
    }

    fn receive(model: &mut UiModel<Field>, from: [u8; 16], id: u8, body: &str) {
        engine(
            model,
            OutboxEvent::Received {
                message_id: [id; 32],
                source: from,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                verified: true,
            },
        );
    }

    /// Send from compose and come back to the list, the focus where it was.
    fn say(model: &mut UiModel<Field>, body: &str) {
        let focus = model.focus;
        model.focus = Pane::Compose;
        typed(model, body);
        update(model, key(KeyCode::Enter));
        model.focus = focus;
    }

    /// The conversation pane's rows as text, untrimmed.
    fn rows(model: &UiModel<Field>) -> Vec<String> {
        view(model).messages.iter().map(text).collect()
    }

    /// The column of a row's first and last non-blank cell.
    fn extent(row: &str) -> (usize, usize) {
        let cells: Vec<char> = row.chars().collect();
        let first = cells.iter().position(|c| *c != ' ').unwrap_or(0);
        let last = cells.iter().rposition(|c| *c != ' ').unwrap_or(0);
        (first, last)
    }

    const LONG: &str = "yes, from ten. Shall I bring the T114 and the long antenna, \
                        or is the stick enough for the hill?";

    /// Ours end one cell short of the pane's right edge, theirs start one cell
    /// in from its left, both at most three quarters of the pane wide, and a
    /// line breaks at a space.
    #[test]
    fn blocks_sit_by_side_capped_at_three_quarters_and_wrapped_at_a_space() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 14, 2);
        say(&mut model, LONG);
        receive(&mut model, PEER, 1, LONG);
        let width = model.sizes().messages.0;
        assert_eq!(width, 52, "the 80 x 24 mockup's pane");
        let cap = layout::block_cap(width);
        assert_eq!(cap, 37);
        let rows = rows(&model);
        let text_rows: Vec<&String> = rows
            .iter()
            .filter(|r| !r.trim().is_empty() && !r.contains(layout::EARLIEST) && !r.contains("──"))
            .collect();
        let (ours, theirs) = text_rows.split_at(text_rows.len() / 2);
        // The text within the cap; the time closing the last row may reach
        // past it, as long as the row keeps its margin cells.
        let body_rows = |rows: &[&String]| -> Vec<String> {
            rows.iter()
                .map(|r| r.replace("  14:02", "").replace(" ○", ""))
                .collect()
        };
        for row in body_rows(ours) {
            let (first, last) = extent(&row);
            assert!(last - first < cap, "{row:?}");
        }
        for row in ours {
            assert!(extent(row).0 > 1, "{row:?}");
        }
        // The block's right edge: its widest row ends at the pane's last
        // cell but one.
        assert_eq!(
            ours.iter().map(|r| extent(r).1).max(),
            Some(width - 2),
            "{ours:?}"
        );
        // Left-aligned inside the block: every row of it starts in one column.
        let starts: Vec<usize> = ours.iter().map(|r| extent(r).0).collect();
        assert!(starts.windows(2).all(|w| w[0] == w[1]), "{starts:?}");
        for row in theirs {
            assert_eq!(extent(row).0, 1, "{row:?}");
            assert!(extent(row).1 < width - 1, "{row:?}");
        }
        for row in body_rows(theirs) {
            assert!(extent(&row).1 < cap + 1, "{row:?}");
        }
        // Each wrapped row ends at a word's end: the words come out whole.
        let words: Vec<&str> = theirs.iter().flat_map(|r| r.split_whitespace()).collect();
        let expected: Vec<&str> = LONG.split_whitespace().chain(["14:02"]).collect();
        assert_eq!(words, expected);
        assert!(ours
            .last()
            .is_some_and(|r| r.trim_end().ends_with("14:02 ○")));
        assert!(theirs
            .last()
            .is_some_and(|r| r.trim_end().ends_with("14:02")));
    }

    /// A centred date before each day's first message: the day before and
    /// `Today`, across midnight.
    #[test]
    fn a_date_line_separates_the_days_across_midnight() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 25, 23, 59);
        receive(&mut model, PEER, 1, "late");
        clock(&mut model, 26, 0, 1);
        receive(&mut model, PEER, 2, "early");
        receive(&mut model, PEER, 3, "again");
        assert_eq!(
            shown(&model),
            [
                layout::EARLIEST,
                "── Sunday 25 October ──",
                "late  23:59",
                "── Today ──",
                "early  00:01",
                "again  00:01",
            ]
        );
        let rows = rows(&model);
        let date = rows.iter().find(|r| r.contains("Today")).expect("today");
        let (first, last) = extent(date);
        assert!(first.abs_diff(52 - 1 - last) <= 1, "centred: {date:?}");
    }

    /// A conversation that is not open collects its unread under a `2 new`
    /// rule, which is there when it is opened and stays until it is left.
    #[test]
    fn the_new_rule_marks_where_the_unread_begins_until_the_conversation_is_left() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        clock(&mut model, 26, 14, 0);
        say(&mut model, "to the first");
        receive(&mut model, OTHER, 1, "read later");
        receive(&mut model, OTHER, 2, "and this");
        assert_eq!(model.conversations[1].unread, 2);
        let list: Vec<String> = view(&model).conversations.iter().map(text).collect();
        assert!(
            list.iter()
                .any(|r| r.starts_with(" and this ") && r.trim_end().ends_with("[2]")),
            "{list:?}"
        );
        update(&mut model, key(KeyCode::Char('g')));
        assert_eq!(model.selected, 1);
        let rule = "───── 2 new ─────";
        let shown_now = shown(&model);
        let at = shown_now
            .iter()
            .position(|r| r.contains(" 2 new "))
            .expect("the rule");
        assert_eq!(shown_now[at + 1], "read later  14:00", "{shown_now:?}");
        assert!(shown_now[at].contains(rule.trim_matches('─')));
        // Full width: the margin cell either side, dashes to them.
        let row = rows(&model)
            .into_iter()
            .find(|r| r.contains(" 2 new "))
            .expect("rule");
        assert_eq!(extent(&row), (1, 50), "{row:?}");
        // It stays while the conversation is open, through a reply.
        say(&mut model, "thanks");
        assert!(shown(&model).iter().any(|r| r.contains(" 2 new ")));
        // Left, and back: gone.
        update(&mut model, key(KeyCode::Char('j')));
        update(&mut model, key(KeyCode::Char('g')));
        assert_eq!(model.selected, 1);
        assert!(!shown(&model).iter().any(|r| r.contains(" new ")));
    }

    /// A reply arriving while the reader is scrolled up draws the rule above
    /// it and does not move the view; going back to the newest follows again.
    #[test]
    fn a_reply_while_scrolled_up_draws_the_rule_and_does_not_jump() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 9, 0);
        for id in 1..=12 {
            receive(&mut model, PEER, id, &format!("message {id}"));
        }
        update(&mut model, key(KeyCode::Tab));
        assert_eq!(model.focus, Pane::Messages);
        for _ in 0..8 {
            update(&mut model, key(KeyCode::Char('k')));
        }
        assert!(model.scroll_top.is_some(), "scrolled up");
        let before = rows(&model);
        receive(&mut model, PEER, 13, "a reply");
        assert_eq!(rows(&model), before, "the view did not move");
        assert_eq!(model.message_cursor, 3, "nor did the cursor");
        update(&mut model, key(KeyCode::Char('G')));
        assert!(model.scroll_top.is_none(), "following again");
        let now = shown(&model);
        let n = now.len();
        assert!(now[n - 2].contains(" 1 new "), "{now:?}");
        assert_eq!(now[n - 1], "▌a reply  09:00");
        // Following, a message arrives read: no rule of its own, the view
        // keeps the newest at the bottom.
        receive(&mut model, PEER, 14, "and another");
        let now = shown(&model);
        assert_eq!(now.last().map(String::as_str), Some("▌and another  09:00"));
        assert_eq!(now.iter().filter(|r| r.contains(" new ")).count(), 1);
    }

    /// A page is the pane's height now that the model knows it.
    #[test]
    fn a_page_moves_the_view_by_the_panes_height() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        for id in 1..=40 {
            receive(&mut model, PEER, id, &format!("message {id}"));
        }
        update(&mut model, key(KeyCode::Tab));
        let height = model.message_rows();
        let total = model.laid().rows.len();
        update(&mut model, key(KeyCode::PageUp));
        assert_eq!(model.scroll_top, Some(total - 2 * height));
        update(&mut model, key(KeyCode::PageDown));
        assert_eq!(model.scroll_top, None, "back at the bottom");
        update(
            &mut model,
            UiEvent::Resize {
                width: 80,
                height: 40,
            },
        );
        assert!(model.message_rows() > height);
    }

    /// Below 40 cells of pane ours go flush left with `you` before the time;
    /// at 40 they are flush right again.
    #[test]
    fn the_narrow_fallback_starts_below_forty_cells() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 9, 12);
        say(&mut model, "on my way");
        update(
            &mut model,
            UiEvent::Resize {
                width: 67,
                height: 24,
            },
        );
        assert_eq!(model.sizes().messages.0, 39);
        let narrow = rows(&model)
            .into_iter()
            .find(|r| r.contains("on my way"))
            .expect("row");
        assert_eq!(extent(&narrow).0, 1, "{narrow:?}");
        assert_eq!(narrow.trim(), "on my way  you 09:12 ○");
        update(
            &mut model,
            UiEvent::Resize {
                width: 68,
                height: 24,
            },
        );
        assert_eq!(model.sizes().messages.0, 40);
        let wide = rows(&model)
            .into_iter()
            .find(|r| r.contains("on my way"))
            .expect("row");
        assert_eq!(extent(&wide).1, 38, "{wide:?}");
        assert_eq!(wide.trim(), "on my way  09:12 ○");
    }

    /// The selected block, under focus, is reversed behind the bar; nothing
    /// is marked while another pane has the focus.
    #[test]
    fn the_selected_block_is_reversed_behind_the_bar_under_focus() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        receive(&mut model, PEER, 1, "one");
        receive(&mut model, PEER, 2, "two");
        let marked = |model: &UiModel<Field>| -> Vec<String> {
            view(model)
                .messages
                .iter()
                .filter(|r| r.cells.iter().any(|c| c.st.reverse))
                .map(text)
                .map(|r| r.trim().to_string())
                .collect()
        };
        assert!(marked(&model).is_empty());
        update(&mut model, key(KeyCode::Tab));
        assert_eq!(marked(&model), ["▌two"]);
        update(&mut model, key(KeyCode::Char('k')));
        assert_eq!(marked(&model), ["▌one"]);
    }

    /// The detail view of a sent message lists what this session saw: the
    /// queue, the leg, the router's states and the proof, with the times, the
    /// full id, the peer and the size; `q` closes it.
    #[test]
    fn the_detail_view_lists_the_ledger_with_the_proof() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 14, 2);
        say(&mut model, "hello");
        update(&mut model, UiEvent::SendLeg(Leg::Direct));
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        clock(&mut model, 26, 14, 3);
        state(&mut model, ID, MessageState::Sending);
        update(&mut model, UiEvent::SendDone(Ok(())));
        state(&mut model, ID, MessageState::Delivered);
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Enter));
        let detail = view(&model).detail.expect("the detail view");
        assert_eq!(
            detail.rows,
            [
                "from you to <abababab…>",
                "peer   abababababababababababababababab",
                &format!("id     {}", "11".repeat(32)),
                "size   5 bytes",
                "state  delivered",
                "",
                "this session",
                "14:02:00  queued",
                "14:02:00  trying direct",
                "14:02:00  queued by the router as 11111111",
                "14:03:00  in flight (direct)",
                "14:03:00  delivered: proof received (direct)",
            ]
        );
        update(&mut model, key(KeyCode::Char('q')));
        assert!(model.detail.is_none());
        assert_eq!(model.focus, Pane::Messages);
    }

    /// A failure names its reason in the ledger; a received message says
    /// whether its signature verified.
    #[test]
    fn the_ledger_keeps_a_failure_and_a_received_messages_signature() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        say(&mut model, "hi");
        update(&mut model, UiEvent::SendDone(Err("no route".to_string())));
        let ledger: Vec<&str> = model.conversations[0].messages[0]
            .ledger
            .iter()
            .map(|e| e.what.as_str())
            .collect();
        assert_eq!(ledger, ["queued", "not sent: no route"]);
        engine(
            &mut model,
            OutboxEvent::Received {
                message_id: ID2,
                source: PEER,
                title: Vec::new(),
                body: b"who?".to_vec(),
                verified: false,
            },
        );
        let them = &model.conversations[0].messages[1];
        assert_eq!(them.ledger[0].what, "received, signature NOT verified");
        assert_eq!(them.size, 4);
        assert!(shown(&model).contains(&"who?  unverified".to_string()));
    }

    /// The header names the peer, its trust and the mailbox, the longest
    /// form that fits; a wide pane gives the address and a mailbox row.
    #[test]
    fn the_header_says_the_trust_and_the_mailbox_state() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        assert_eq!(view(&model).messages_title_right, "no mailbox");
        update(
            &mut model,
            UiEvent::MailboxChosen {
                node: [0x9c; 16],
                by_user: false,
            },
        );
        assert_eq!(
            view(&model).messages_title_right,
            "mailbox 9c9c9c9c adopted"
        );
        clock(&mut model, 26, 14, 3);
        update(&mut model, UiEvent::Run(PaletteCommand::Fetch));
        update(&mut model, UiEvent::FetchDone(Ok(0)));
        assert_eq!(view(&model).messages_title_right, "mailbox · synced 14:03");
        update(
            &mut model,
            UiEvent::Resize {
                width: 120,
                height: 40,
            },
        );
        let wide = view(&model);
        assert_eq!(wide.messages_title_right, to_hex(&PEER));
        assert_eq!(
            text(&wide.messages[0]).trim(),
            "mailbox 9c9c9c9c adopted · synced 14:03"
        );
        assert!(text(&wide.messages[1]).trim().chars().all(|c| c == '─'));
        assert_eq!(wide.compose_title, "to <abababab…>");
    }
}
