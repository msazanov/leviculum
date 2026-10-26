//! The keymap: one table of rows, each an action with its chords, read by a
//! program's key handler, its help overlay, its footer and its palette
//! (`docs/src/concepts/loquax-keys-and-layout.md`, section 4).
//!
//! A [`Chord`] is a key, the modifiers it asks for, the action it runs, the
//! panes it applies in and the contexts it needs. A [`Row`] is one action as
//! the reader sees it: its description, its help group, its chords grouped
//! into spellings (vi, Emacs, the arrows), its palette name, its footer label,
//! a lost-habit note and whether it transmits. The key strings the help and
//! the footer show are DERIVED from the chords, at compile time, so the help
//! can never list a key the handler does not honour.
//!
//! Panes and contexts are bit sets whose meaning is the program's, except
//! [`Panes::SCROLL`] (where the shared scroll rows apply) and
//! [`Ctx::ENHANCED`] (the terminal speaks the keyboard enhancement protocol).
//!
//! Resolution ([`best`]) takes the most specific matching chord: exact
//! modifiers beat a modifier class (`Ctrl`, `Alt`, plain), which beats a
//! chord that takes any modifiers. Among equals the first in table order
//! wins, which is what lnomad's first-match scan did, so its behaviour does
//! not move; a program's tests hold that no chord resolves to two actions in
//! one pane ([`canonical`] is the event to check a chord with).
//!
//! The scroll rows lnomad and loquax share are [`SCROLL_ROWS`].

use crate::input::{KeyCode, KeyEvent, KeyModifiers};

/// A fixed-size string built at compile time: a chord's or a row's displayed
/// keys. Rows live in `const` tables, so the label borrowed from one is
/// `&'static str`, which is what the footer and the help take.
#[derive(Clone, Copy)]
pub struct Label<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Label<N> {
    const EMPTY: Self = Self {
        bytes: [0; N],
        len: 0,
    };

    /// Append `text`. A table whose label outgrows `N` fails to compile.
    const fn push(mut self, text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            assert!(self.len < N, "a key label outgrew its buffer");
            self.bytes[self.len] = bytes[i];
            self.len += 1;
            i += 1;
        }
        self
    }

    const fn push_label<const M: usize>(self, other: &Label<M>) -> Self {
        let mut out = self;
        let mut i = 0;
        while i < other.len {
            assert!(out.len < N, "a key label outgrew its buffer");
            out.bytes[out.len] = other.bytes[i];
            out.len += 1;
            i += 1;
        }
        out
    }

    const fn push_char(self, c: char) -> Self {
        let mut buf = [0u8; 4];
        let text: &str = c.encode_utf8(&mut buf);
        self.push(text)
    }

    /// The text.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }

    const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const N: usize> core::fmt::Debug for Label<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// The modifiers a chord asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mods {
    /// Whatever is held: the bare arrows and paging keys scroll however they
    /// are held. The weakest match.
    Any,
    /// Neither Ctrl nor Alt; Shift may be held (`G` arrives with it).
    Plain,
    /// Ctrl held, whatever else is.
    Ctrl,
    /// Alt held, whatever else is.
    Alt,
    /// Exactly these.
    Exact(KeyModifiers),
}

impl Mods {
    /// Whether `held` satisfies this.
    pub fn matches(self, held: KeyModifiers) -> bool {
        let ctrl = held.contains(KeyModifiers::CONTROL);
        let alt = held.contains(KeyModifiers::ALT);
        match self {
            Self::Any => true,
            Self::Plain => !ctrl && !alt,
            Self::Ctrl => ctrl,
            Self::Alt => alt,
            Self::Exact(mods) => held == mods,
        }
    }

    /// How specific the match is: exact over a class over any.
    pub const fn rank(self) -> u8 {
        match self {
            Self::Any => 0,
            Self::Plain | Self::Ctrl | Self::Alt => 1,
            Self::Exact(_) => 2,
        }
    }

    /// The modifiers a key event made for this chord carries.
    const fn canonical(self) -> KeyModifiers {
        match self {
            Self::Any | Self::Plain => KeyModifiers::NONE,
            Self::Ctrl => KeyModifiers::CONTROL,
            Self::Alt => KeyModifiers::ALT,
            Self::Exact(mods) => mods,
        }
    }
}

/// The panes a chord applies in, a bit set. The bits are the program's except
/// [`Panes::SCROLL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Panes(pub u16);

impl Panes {
    /// The panes the shared scroll rows apply in: lnomad's content and its
    /// overlays, loquax's conversation list and messages.
    pub const SCROLL: Self = Self(1);
    /// Every pane.
    pub const ALL: Self = Self(u16::MAX);

    /// Both sets.
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether the sets share a pane.
    pub const fn meets(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

/// What holds right now besides the pane, a bit set: the terminal's keyboard
/// protocol, and the program's own switches. The bits are the program's
/// except [`Ctx::ENHANCED`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ctx(pub u8);

impl Ctx {
    /// Nothing set.
    pub const NONE: Self = Self(0);
    /// The terminal speaks the keyboard enhancement protocol, so a chord a
    /// legacy terminal cannot tell from its plain key (`Shift-Enter`,
    /// `Ctrl-Tab`) arrives as itself.
    pub const ENHANCED: Self = Self(1);

    /// Both sets.
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every bit of `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any bit of `other` is set.
    pub const fn meets(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

/// One key chord: the key, the modifiers, what it runs and where.
#[derive(Clone, Copy, Debug)]
pub struct Chord<A> {
    /// The key.
    pub code: KeyCode,
    /// The modifiers it asks for.
    pub mods: Mods,
    /// What it runs.
    pub action: A,
    /// Where it applies.
    pub panes: Panes,
    /// Whether only a terminal speaking the keyboard enhancement protocol
    /// sends it (marked *(enh.)* on the design page). It still resolves when
    /// it arrives; it is only never offered as the live chord without
    /// [`Ctx::ENHANCED`].
    pub enhanced: bool,
    /// Context bits that must all be set.
    pub when: Ctx,
    /// Context bits none of which may be set.
    pub unless: Ctx,
    /// Left out of the row's displayed keys: a second spelling the help has
    /// never listed (lnomad's `Space`, `Alt-v`, `Alt-<`), kept quiet so the
    /// overlay keeps its width. It resolves like any other.
    pub quiet: bool,
    label: Label<24>,
}

impl<A: Copy> Chord<A> {
    /// A chord that applies in every pane and context.
    pub const fn new(code: KeyCode, mods: Mods, action: A) -> Self {
        Self {
            code,
            mods,
            action,
            panes: Panes::ALL,
            enhanced: false,
            when: Ctx::NONE,
            unless: Ctx::NONE,
            quiet: false,
            label: chord_label(code, mods),
        }
    }

    /// Resolves, but is not listed in the row's displayed keys.
    pub const fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    /// Only in these panes.
    pub const fn panes(mut self, panes: Panes) -> Self {
        self.panes = panes;
        self
    }

    /// Only a terminal under the keyboard enhancement protocol sends it.
    pub const fn enhanced(mut self) -> Self {
        self.enhanced = true;
        self
    }

    /// Only while all of `ctx` is set.
    pub const fn when(mut self, ctx: Ctx) -> Self {
        self.when = ctx;
        self
    }

    /// Only while none of `ctx` is set.
    pub const fn unless(mut self, ctx: Ctx) -> Self {
        self.unless = ctx;
        self
    }

    /// The chord as the reader reads it (`Ctrl-n`, `Alt-<`, `↓`, `Space`).
    pub fn label(&self) -> &str {
        self.label.as_str()
    }

    /// Whether the chord is in force in `pane` under `ctx`.
    pub fn in_force(&self, pane: Panes, ctx: Ctx) -> bool {
        self.panes.meets(pane) && ctx.contains(self.when) && !ctx.meets(self.unless)
    }

    /// Whether this terminal can send it and it is in force: the chord to
    /// offer in a hint or the footer.
    pub fn live(&self, pane: Panes, ctx: Ctx) -> bool {
        self.in_force(pane, ctx) && (!self.enhanced || ctx.contains(Ctx::ENHANCED))
    }

    /// Whether `key` is this chord, in `pane` under `ctx`.
    pub fn matches(&self, key: &KeyEvent, pane: Panes, ctx: Ctx) -> bool {
        self.code == key.code && self.mods.matches(key.modifiers) && self.in_force(pane, ctx)
    }

    /// The single keystroke a reader could type by accident: no Ctrl or Alt,
    /// and not a function key. The quiet keyboard keeps transmitting actions
    /// off these, but for sending from compose.
    pub fn is_single_keystroke(&self) -> bool {
        let mods = self.mods.canonical();
        !matches!(self.mods, Mods::Ctrl | Mods::Alt)
            && !mods.contains(KeyModifiers::CONTROL)
            && !mods.contains(KeyModifiers::ALT)
            && !matches!(self.code, KeyCode::F(_))
    }
}

/// The key event a chord stands for: what a test resolves to check that the
/// chord reaches its own row.
pub fn canonical<A>(chord: &Chord<A>) -> KeyEvent {
    let mut mods = chord.mods.canonical();
    if let KeyCode::Char(c) = chord.code {
        if c.is_uppercase() && matches!(chord.mods, Mods::Plain | Mods::Any) {
            mods |= KeyModifiers::SHIFT;
        }
    }
    if chord.code == KeyCode::BackTab && !matches!(chord.mods, Mods::Exact(_)) {
        mods |= KeyModifiers::SHIFT;
    }
    KeyEvent::new(chord.code, mods)
}

const fn key_name<const N: usize>(label: Label<N>, code: KeyCode) -> Label<N> {
    match code {
        KeyCode::Backspace => label.push("Backspace"),
        KeyCode::Enter => label.push("Enter"),
        KeyCode::Left => label.push("←"),
        KeyCode::Right => label.push("→"),
        KeyCode::Up => label.push("↑"),
        KeyCode::Down => label.push("↓"),
        KeyCode::Home => label.push("Home"),
        KeyCode::End => label.push("End"),
        KeyCode::PageUp => label.push("PageUp"),
        KeyCode::PageDown => label.push("PageDown"),
        KeyCode::Tab => label.push("Tab"),
        KeyCode::BackTab => label.push("Shift-Tab"),
        KeyCode::Delete => label.push("Delete"),
        KeyCode::Insert => label.push("Insert"),
        KeyCode::F(n) => {
            let label = label.push("F");
            if n >= 10 {
                label
                    .push_char((b'0' + n / 10) as char)
                    .push_char((b'0' + n % 10) as char)
            } else {
                label.push_char((b'0' + n) as char)
            }
        }
        KeyCode::Char(' ') => label.push("Space"),
        KeyCode::Char(c) => label.push_char(c),
        KeyCode::Null => label.push("Null"),
        KeyCode::Esc => label.push("Esc"),
        KeyCode::Other => label.push("?"),
    }
}

const fn chord_label(code: KeyCode, mods: Mods) -> Label<24> {
    let held = match mods {
        Mods::Ctrl => KeyModifiers::CONTROL,
        Mods::Alt => KeyModifiers::ALT,
        Mods::Exact(mods) => mods,
        Mods::Any | Mods::Plain => KeyModifiers::NONE,
    };
    let mut label = Label::<24>::EMPTY;
    if held.contains(KeyModifiers::CONTROL) {
        label = label.push("Ctrl-");
    }
    if held.contains(KeyModifiers::ALT) {
        label = label.push("Alt-");
    }
    if held.contains(KeyModifiers::META) {
        label = label.push("Meta-");
    }
    // A bare uppercase letter and `Shift-Tab` carry their Shift already; with
    // Ctrl or Alt it is spelled out (`Ctrl-Shift-T`).
    let chorded = held.contains(KeyModifiers::CONTROL) || held.contains(KeyModifiers::ALT);
    let shift_shown = match code {
        KeyCode::Char(_) => !chorded,
        KeyCode::BackTab => true,
        _ => false,
    };
    if held.contains(KeyModifiers::SHIFT) && !shift_shown {
        label = label.push("Shift-");
    }
    key_name(label, code)
}

/// A spelling: the chords of one key family that together make a row (`j` and
/// `k`; `Ctrl-n` and `Ctrl-p`). Shown joined by ` / `.
pub type Spelling<A> = &'static [Chord<A>];

/// A footer button a row offers: its label and when it drops on a narrow row
/// (see [`crate::footer::FooterItem`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FooterSpec {
    /// The muted word beside the key.
    pub label: &'static str,
    /// `None` never drops, else the rank it drops at, lowest first.
    pub drop_rank: Option<u8>,
    /// One button a live spelling, its key the spelling's chords joined
    /// (`n / N next, previous`), where a row's chords are a pair of
    /// directions; else one button, its key the first live chord.
    pub spelled: bool,
}

/// The most spellings a row has, so each one's keys fit a fixed table.
pub const MAX_SPELLINGS: usize = 4;

/// One action as the reader sees it.
#[derive(Clone, Copy, Debug)]
pub struct Row<A: 'static> {
    /// What it does, the help overlay's description.
    pub desc: &'static str,
    /// The help overlay's group title.
    pub group: &'static str,
    /// The chords, a spelling at a time, in display order.
    pub spellings: &'static [Spelling<A>],
    /// Its palette name, if the palette offers it.
    pub palette: Option<&'static str>,
    /// Its footer button, if it has one.
    pub footer: Option<FooterSpec>,
    /// Where a habit from another program went ("`Ctrl-N` in Signal: here
    /// `m`"), shown under the row in the help.
    pub note: Option<&'static str>,
    /// Whether running it puts bytes on the air.
    pub tx: bool,
    keys: Label<96>,
    spelled: [Label<48>; MAX_SPELLINGS],
}

impl<A: Copy + 'static> Row<A> {
    /// A row with these chords, in no palette, footer or note.
    pub const fn new(
        desc: &'static str,
        group: &'static str,
        spellings: &'static [Spelling<A>],
    ) -> Self {
        Self {
            desc,
            group,
            spellings,
            palette: None,
            footer: None,
            note: None,
            tx: false,
            keys: row_keys(spellings),
            spelled: spelling_keys(spellings),
        }
    }

    /// Offered in the palette under `name`. A row with no chord is reached
    /// through the palette only, and its help keys say so (`: name`).
    pub const fn palette(mut self, name: &'static str) -> Self {
        self.palette = Some(name);
        if self.keys.is_empty() {
            self.keys = Label::EMPTY.push(": ").push(name);
        }
        self
    }

    /// A footer button labelled `label`, never dropped.
    pub const fn footer(mut self, label: &'static str) -> Self {
        self.footer = Some(FooterSpec {
            label,
            drop_rank: None,
            spelled: false,
        });
        self
    }

    /// A footer button labelled `label` for each spelling with a live chord,
    /// never dropped: a pair of directions reads as one (`n / N`).
    pub const fn footer_spelled(mut self, label: &'static str) -> Self {
        self.footer = Some(FooterSpec {
            label,
            drop_rank: None,
            spelled: true,
        });
        self
    }

    /// A footer button labelled `label`, dropped at `rank` when the row is
    /// too narrow.
    pub const fn footer_droppable(mut self, label: &'static str, rank: u8) -> Self {
        self.footer = Some(FooterSpec {
            label,
            drop_rank: Some(rank),
            spelled: false,
        });
        self
    }

    /// Where a habit from another program went.
    pub const fn note(mut self, note: &'static str) -> Self {
        self.note = Some(note);
        self
    }

    /// Running it transmits.
    pub const fn tx(mut self) -> Self {
        self.tx = true;
        self
    }

    /// The keys as the help shows them, derived from the chords.
    pub fn keys(&self) -> &str {
        self.keys.as_str()
    }

    /// Spelling `index`'s keys as the help shows them (`n / N`), its quiet
    /// chords left out; empty past the last spelling.
    pub fn spelling_keys(&self, index: usize) -> &str {
        self.spelled.get(index).map_or("", Label::as_str)
    }

    /// Every chord, in display order.
    pub fn chords(&self) -> impl Iterator<Item = &Chord<A>> {
        self.spellings.iter().flat_map(|s| s.iter())
    }

    /// The chord to offer in `pane` under `ctx`: the first live one.
    pub fn live_chord(&self, pane: Panes, ctx: Ctx) -> Option<&Chord<A>> {
        self.chords().find(|c| c.live(pane, ctx))
    }
}

const fn row_keys<A: Copy + 'static>(spellings: &[Spelling<A>]) -> Label<96> {
    let mut label = Label::<96>::EMPTY;
    let mut s = 0;
    while s < spellings.len() {
        let chords = spellings[s];
        let mut shown = 0;
        let mut c = 0;
        while c < chords.len() {
            if !chords[c].quiet {
                if shown > 0 {
                    label = label.push(" / ");
                } else if !label.is_empty() {
                    label = label.push("   ");
                }
                label = label.push_label(&chords[c].label);
                shown += 1;
            }
            c += 1;
        }
        s += 1;
    }
    label
}

const fn spelling_keys<A: Copy + 'static>(spellings: &[Spelling<A>]) -> [Label<48>; MAX_SPELLINGS] {
    assert!(
        spellings.len() <= MAX_SPELLINGS,
        "a row has more spellings than MAX_SPELLINGS"
    );
    let mut out = [Label::<48>::EMPTY; MAX_SPELLINGS];
    let mut s = 0;
    while s < spellings.len() {
        let chords = spellings[s];
        let mut c = 0;
        while c < chords.len() {
            if !chords[c].quiet {
                if !out[s].is_empty() {
                    out[s] = out[s].push(" / ");
                }
                out[s] = out[s].push_label(&chords[c].label);
            }
            c += 1;
        }
        s += 1;
    }
    out
}

/// The action `key` resolves to among `rows` in `pane` under `ctx`, with the
/// rank of the chord that won: the most specific chord, the first among
/// equals. A program reading two tables compares the ranks.
pub fn best<A: Copy + 'static>(
    rows: &[Row<A>],
    key: &KeyEvent,
    pane: Panes,
    ctx: Ctx,
) -> Option<(u8, A)> {
    let mut found: Option<(u8, A)> = None;
    for chord in rows.iter().flat_map(|r| r.chords()) {
        if !chord.matches(key, pane, ctx) {
            continue;
        }
        let rank = chord.mods.rank();
        if found.is_none_or(|(have, _)| rank > have) {
            found = Some((rank, chord.action));
        }
    }
    found
}

/// Every action `key` reaches among `rows` at the winning rank, in table
/// order: more than one distinct action is a shadowed chord.
pub fn all_best<A: Copy + PartialEq + 'static>(
    rows: &[Row<A>],
    key: &KeyEvent,
    pane: Panes,
    ctx: Ctx,
) -> alloc::vec::Vec<(u8, A)> {
    let mut hits: alloc::vec::Vec<(u8, A)> = rows
        .iter()
        .flat_map(|r| r.chords())
        .filter(|c| c.matches(key, pane, ctx))
        .map(|c| (c.mods.rank(), c.action))
        .collect();
    let top = hits.iter().map(|(rank, _)| *rank).max().unwrap_or(0);
    hits.retain(|(rank, _)| *rank == top);
    let mut distinct = alloc::vec::Vec::new();
    for hit in hits {
        if !distinct.contains(&hit) {
            distinct.push(hit);
        }
    }
    distinct
}

/// A vertical scroll motion, resolved against the viewport by the program.
/// Bound to the vi, Emacs and arrow keys of [`SCROLL_ROWS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollCmd {
    /// Up one line.
    LineUp,
    /// Down one line.
    LineDown,
    /// Up half a viewport.
    HalfPageUp,
    /// Down half a viewport.
    HalfPageDown,
    /// Up one full viewport.
    PageUp,
    /// Down one full viewport.
    PageDown,
    /// To the very top.
    Top,
    /// To the very bottom.
    Bottom,
}

/// The help group the scroll rows sit in.
pub const MOVE: &str = "Move";

const fn scroll(code: KeyCode, mods: Mods, cmd: ScrollCmd) -> Chord<ScrollCmd> {
    Chord::new(code, mods, cmd).panes(Panes::SCROLL)
}

/// The scroll rows lnomad and loquax share: vi, Emacs and the arrows side by
/// side, in [`Panes::SCROLL`]. The bare arrows and paging keys take any
/// modifiers, as lnomad's always did; a program that gives `Ctrl-↑` or
/// `Alt-↑` a meaning of its own binds it with exact modifiers, which win.
pub const SCROLL_ROWS: &[Row<ScrollCmd>] = {
    use KeyCode::{Char, Down, End, Home, Up};
    use Mods::*;
    use ScrollCmd::*;
    &[
        Row::new(
            "scroll a line",
            MOVE,
            &[
                &[
                    scroll(Char('j'), Plain, LineDown),
                    scroll(Char('k'), Plain, LineUp),
                ],
                &[scroll(Down, Any, LineDown), scroll(Up, Any, LineUp)],
                &[
                    scroll(Char('n'), Ctrl, LineDown),
                    scroll(Char('p'), Ctrl, LineUp),
                ],
            ],
        ),
        Row::new(
            "page down / up",
            MOVE,
            &[
                &[
                    scroll(Char('f'), Ctrl, PageDown),
                    scroll(Char('b'), Ctrl, PageUp),
                ],
                &[
                    scroll(Char('v'), Ctrl, PageDown),
                    scroll(Char('v'), Alt, PageUp).quiet(),
                ],
                &[scroll(Char(' '), Plain, PageDown).quiet()],
                &[
                    scroll(KeyCode::PageDown, Any, PageDown),
                    scroll(KeyCode::PageUp, Any, PageUp),
                ],
            ],
        ),
        Row::new(
            "half a page",
            MOVE,
            &[&[
                scroll(Char('d'), Ctrl, HalfPageDown),
                scroll(Char('u'), Ctrl, HalfPageUp),
            ]],
        ),
        Row::new(
            "top / bottom",
            MOVE,
            &[
                &[
                    scroll(Char('g'), Plain, Top),
                    scroll(Char('G'), Plain, Bottom),
                ],
                &[
                    scroll(Char('<'), Alt, Top).quiet(),
                    scroll(Char('>'), Alt, Bottom).quiet(),
                ],
                &[scroll(Home, Any, Top), scroll(End, Any, Bottom)],
            ],
        ),
    ]
};

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn labels_are_derived_from_the_chords() {
        let keys: alloc::vec::Vec<&str> = SCROLL_ROWS.iter().map(|r| r.keys()).collect();
        assert_eq!(
            keys,
            [
                "j / k   ↓ / ↑   Ctrl-n / Ctrl-p",
                "Ctrl-f / Ctrl-b   Ctrl-v   PageDown / PageUp",
                "Ctrl-d / Ctrl-u",
                "g / G   Home / End",
            ]
        );
        let exact = |code, mods| Chord::new(code, Mods::Exact(mods), ());
        assert_eq!(
            exact(KeyCode::Up, KeyModifiers::ALT | KeyModifiers::SHIFT).label(),
            "Alt-Shift-↑"
        );
        assert_eq!(
            exact(
                KeyCode::BackTab,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )
            .label(),
            "Ctrl-Shift-Tab"
        );
        assert_eq!(
            exact(KeyCode::Enter, KeyModifiers::SHIFT).label(),
            "Shift-Enter"
        );
        assert_eq!(exact(KeyCode::F(12), KeyModifiers::NONE).label(), "F12");
        assert_eq!(
            exact(
                KeyCode::Char('T'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )
            .label(),
            "Ctrl-Shift-T"
        );
        assert_eq!(Chord::new(KeyCode::Char('G'), Mods::Plain, ()).label(), "G");
        let palette_only: Row<()> = Row::new("focus", "Everywhere", &[]).palette("focus messages");
        assert_eq!(palette_only.keys(), ": focus messages");
    }

    /// The most specific chord wins; a plain arrow still scrolls however it
    /// is held, as it always did in lnomad.
    /// Each spelling's keys on their own, quiet chords left out, for a
    /// footer that shows a pair of directions as one button.
    #[test]
    fn a_spelling_s_keys_are_its_chords_joined() {
        let line = &SCROLL_ROWS[0];
        assert_eq!(line.spelling_keys(0), "j / k");
        assert_eq!(line.spelling_keys(2), "Ctrl-n / Ctrl-p");
        assert_eq!(line.spelling_keys(3), "");
        let page = &SCROLL_ROWS[1];
        assert_eq!(page.spelling_keys(1), "Ctrl-v");
        assert_eq!(page.spelling_keys(2), "");
        let row = Row::<ScrollCmd>::new("pair", "group", &[]).footer_spelled("next, previous");
        assert_eq!(row.footer.map(|f| f.spelled), Some(true));
        assert_eq!(row.footer.map(|f| f.drop_rank), Some(None));
    }

    #[test]
    fn exact_modifiers_beat_a_class_which_beats_any() {
        const OWN: &[Row<u8>] = &[Row::new(
            "first",
            "x",
            &[&[Chord::new(
                KeyCode::Up,
                Mods::Exact(KeyModifiers::CONTROL),
                7,
            )]],
        )];
        let ctrl_up = key(KeyCode::Up, KeyModifiers::CONTROL);
        assert_eq!(
            best(SCROLL_ROWS, &ctrl_up, Panes::SCROLL, Ctx::NONE),
            Some((0, ScrollCmd::LineUp))
        );
        assert_eq!(best(OWN, &ctrl_up, Panes::SCROLL, Ctx::NONE), Some((2, 7)));
        // Ctrl-Alt-v is both scroll chords at one rank: the first wins, as
        // lnomad's first-match scan had it.
        let both = key(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert_eq!(
            best(SCROLL_ROWS, &both, Panes::SCROLL, Ctx::NONE),
            Some((1, ScrollCmd::PageDown))
        );
        assert_eq!(
            all_best(SCROLL_ROWS, &both, Panes::SCROLL, Ctx::NONE).len(),
            2
        );
    }

    #[test]
    fn panes_and_contexts_gate_a_chord() {
        const BREAKS: Ctx = Ctx(2);
        let chord = Chord::new(KeyCode::Enter, Mods::Exact(KeyModifiers::NONE), ())
            .panes(Panes(4))
            .unless(BREAKS);
        let enter = key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(chord.matches(&enter, Panes(4), Ctx::NONE));
        assert!(!chord.matches(&enter, Panes(2), Ctx::NONE));
        assert!(!chord.matches(&enter, Panes(4), BREAKS));
        let shifted = Chord::new(KeyCode::Enter, Mods::Exact(KeyModifiers::SHIFT), ()).enhanced();
        assert!(shifted.in_force(Panes(4), Ctx::NONE));
        assert!(!shifted.live(Panes(4), Ctx::NONE));
        assert!(shifted.live(Panes(4), Ctx::ENHANCED));
    }

    /// No scroll chord reaches two motions: each one's own event resolves to
    /// its own command alone.
    #[test]
    fn no_scroll_chord_is_shadowed() {
        for row in SCROLL_ROWS {
            for chord in row.chords() {
                let event = canonical(chord);
                let hits = all_best(SCROLL_ROWS, &event, Panes::SCROLL, Ctx::NONE);
                assert_eq!(hits.len(), 1, "{} reaches {hits:?}", chord.label());
                assert_eq!(hits[0].1, chord.action, "{}", chord.label());
            }
        }
    }
}
