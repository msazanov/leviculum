//! The programs' own key and mouse events, and the text-editing table over
//! them.
//!
//! Shaped like crossterm's, field for field and variant for variant, because
//! the terminal frontend is the first translator and a one-to-one mapping is
//! the one that cannot drift. Owning them in this terminal-free crate is what
//! keeps a program's core free of the terminal crate: a GUI frontend builds
//! the same values from its own input and the core cannot tell the
//! difference. Positions are character cells, the unit the layout is measured
//! in (see [`crate::geometry`]). The crossterm translation lives in
//! `leviculum-ui-ratatui`.
//!
//! A text field a core holds is an [`InputBuffer`]: the trait is here and
//! `no_std`, its implementation over `tui_input::Input` comes with `std`, so
//! a messenger core built for a board names the field without the std-only
//! editor and a board brings its own. The command palette's names are
//! [`crate::commands`].

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::{BitOr, BitOrAssign};

#[cfg(feature = "std")]
use tui_input::InputRequest;

/// A key, as the programs understand it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// Backspace.
    Backspace,
    /// Enter.
    Enter,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Tab.
    Tab,
    /// Shift-Tab.
    BackTab,
    /// Delete.
    Delete,
    /// Insert.
    Insert,
    /// A function key, `F(1)` for F1.
    F(u8),
    /// A character key.
    Char(char),
    /// The null key.
    Null,
    /// Escape.
    Esc,
    /// Any other key a frontend can report (lock keys, media keys, a bare
    /// modifier). No binding names one; it exists so such a key still reaches
    /// a program's key handler as "some other key", exactly as it did when
    /// lnomad's core matched on crossterm's own codes.
    Other,
}

/// The modifier keys held with a key or mouse event, a bit set.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct KeyModifiers(u8);

impl KeyModifiers {
    /// No modifier.
    pub const NONE: Self = Self(0);
    /// Shift.
    pub const SHIFT: Self = Self(0b0000_0001);
    /// Control.
    pub const CONTROL: Self = Self(0b0000_0010);
    /// Alt (Option on a Mac).
    pub const ALT: Self = Self(0b0000_0100);
    /// Super (the Windows or Command key).
    pub const SUPER: Self = Self(0b0000_1000);
    /// Hyper.
    pub const HYPER: Self = Self(0b0001_0000);
    /// Meta.
    pub const META: Self = Self(0b0010_0000);

    /// The raw bit set, in crossterm's bit order.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// The set with these raw bits; unknown bits are dropped.
    pub const fn from_bits_truncate(bits: u8) -> Self {
        Self(bits & 0b0011_1111)
    }

    /// Whether every modifier in `other` is held.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no modifier is held.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for KeyModifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for KeyModifiers {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl core::fmt::Debug for KeyModifiers {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        const NAMES: [(KeyModifiers, &str); 6] = [
            (KeyModifiers::SHIFT, "SHIFT"),
            (KeyModifiers::CONTROL, "CONTROL"),
            (KeyModifiers::ALT, "ALT"),
            (KeyModifiers::SUPER, "SUPER"),
            (KeyModifiers::HYPER, "HYPER"),
            (KeyModifiers::META, "META"),
        ];
        let held: Vec<&str> = NAMES
            .iter()
            .filter(|(m, _)| self.contains(*m))
            .map(|(_, name)| *name)
            .collect();
        match held.is_empty() {
            true => write!(f, "KeyModifiers(NONE)"),
            false => write!(f, "KeyModifiers({})", held.join(" | ")),
        }
    }
}

/// Whether a key went down, is auto-repeating, or came up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyEventKind {
    /// Pressed.
    Press,
    /// Held and repeating.
    Repeat,
    /// Released. Only some terminals report it.
    Release,
}

/// One key event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    /// The key.
    pub code: KeyCode,
    /// The modifiers held with it.
    pub modifiers: KeyModifiers,
    /// Press, repeat or release.
    pub kind: KeyEventKind,
}

impl KeyEvent {
    /// A key press.
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self {
            code,
            modifiers,
            kind: KeyEventKind::Press,
        }
    }
}

/// A mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// Left button.
    Left,
    /// Right button.
    Right,
    /// Middle button.
    Middle,
    /// The back side button (button 8), history back.
    Back,
    /// The forward side button (button 9), history forward.
    Forward,
}

/// What the mouse did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseEventKind {
    /// A button went down.
    Down(MouseButton),
    /// A button came up.
    Up(MouseButton),
    /// The pointer moved with a button held.
    Drag(MouseButton),
    /// The pointer moved with no button held.
    Moved,
    /// The wheel turned towards the reader.
    ScrollDown,
    /// The wheel turned away from the reader.
    ScrollUp,
    /// The wheel tilted left.
    ScrollLeft,
    /// The wheel tilted right.
    ScrollRight,
}

/// One mouse event, at a character cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MouseEvent {
    /// What happened.
    pub kind: MouseEventKind,
    /// The cell column, from 0 at the left edge.
    pub column: u16,
    /// The cell row, from 0 at the top edge.
    pub row: u16,
    /// The modifiers held.
    pub modifiers: KeyModifiers,
}

/// A text field as a core holds it: what a key does to it, and what it holds.
/// lnmsg's compose pane is one; it holds more than one line once
/// [`InputBuffer::newline`] has been called, and every other edit treats the
/// line break as one character.
pub trait InputBuffer {
    /// The text.
    fn value(&self) -> &str;
    /// The cursor, in characters from the start.
    fn cursor(&self) -> usize;
    /// Apply what `key` asks of the text. `false` when the key is not an
    /// edit at all, so the caller may bind it to something else.
    fn edit(&mut self, key: &KeyEvent) -> bool;
    /// Empty the field and hand back what it held.
    fn take(&mut self) -> String;
    /// Insert a line break at the cursor. Not an [`InputBuffer::edit`] key:
    /// which key means "newline" is the caller's keymap (lnmsg's compose
    /// pane binds `Alt-Enter`, where `Enter` sends), not the field's.
    fn newline(&mut self);
}

/// The terminal's field: `tui-input`, edited through [`input_request`], the
/// same table lnomad's address bar and form fields edit with.
#[cfg(feature = "std")]
impl InputBuffer for tui_input::Input {
    fn value(&self) -> &str {
        tui_input::Input::value(self)
    }

    fn cursor(&self) -> usize {
        tui_input::Input::cursor(self)
    }

    fn edit(&mut self, key: &KeyEvent) -> bool {
        match input_request(key) {
            Some(request) => {
                self.handle(request);
                true
            }
            None => false,
        }
    }

    fn take(&mut self) -> String {
        self.value_and_reset()
    }

    fn newline(&mut self) {
        self.handle(InputRequest::InsertChar('\n'));
    }
}

/// The text-editing request a key makes of a [`tui_input::Input`], if any.
///
/// The same table `tui-input`'s crossterm backend applies (its
/// `to_input_request`, version 0.15), restated over this module's types so the
/// address bar, the search bar and the form fields edit exactly as they did
/// when the core called that backend directly. Releases edit nothing.
#[cfg(feature = "std")]
pub fn input_request(key: &KeyEvent) -> Option<InputRequest> {
    use InputRequest::*;
    use KeyCode::*;
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let none = KeyModifiers::NONE;
    let ctrl = KeyModifiers::CONTROL;
    let alt = KeyModifiers::ALT;
    let meta = KeyModifiers::META;
    let mods = key.modifiers;
    match key.code {
        Backspace if mods == none => Some(DeletePrevChar),
        Char('h') if mods == ctrl => Some(DeletePrevChar),
        Delete if mods == none => Some(DeleteNextChar),
        Tab if mods == none => None,
        Left if mods == none => Some(GoToPrevChar),
        Char('b') if mods == ctrl => Some(GoToPrevChar),
        Left if mods == ctrl => Some(GoToPrevWord),
        Char('b') if mods == meta => Some(GoToPrevWord),
        Right if mods == none => Some(GoToNextChar),
        Char('f') if mods == ctrl => Some(GoToNextChar),
        Right if mods == ctrl => Some(GoToNextWord),
        Char('f') if mods == meta => Some(GoToNextWord),
        Char('u') if mods == ctrl => Some(DeleteLine),
        Char('w') if mods == ctrl => Some(DeletePrevWord),
        Char('d') if mods == meta => Some(DeletePrevWord),
        Backspace if mods == meta || mods == alt => Some(DeletePrevWord),
        Delete if mods == ctrl => Some(DeleteNextWord),
        Char('k') if mods == ctrl => Some(DeleteTillEnd),
        Char('y') if mods == ctrl => Some(Yank),
        Char('a') if mods == ctrl => Some(GoToStart),
        Home if mods == none => Some(GoToStart),
        Char('e') if mods == ctrl => Some(GoToEnd),
        End if mods == none => Some(GoToEnd),
        Char(c) if mods == none || mods == KeyModifiers::SHIFT || mods == ctrl | alt => {
            Some(InsertChar(c))
        }
        _ => None,
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    /// The line break goes in at the cursor, and the field's other edits
    /// treat it as the one character it is: a backspace right after it
    /// joins the lines again.
    #[test]
    fn a_newline_goes_in_at_the_cursor_and_is_one_character() {
        let mut field = tui_input::Input::default();
        for c in "onetwo".chars() {
            field.edit(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        for _ in 0..3 {
            field.edit(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }
        field.newline();
        assert_eq!(InputBuffer::value(&field), "one\ntwo");
        assert_eq!(InputBuffer::cursor(&field), 4);
        field.edit(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(field.take(), "onetwo");
    }
}
