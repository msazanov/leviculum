//! crossterm's events as the programs' own ([`leviculum_ui::input`]), and the
//! classification of one event-stream item for a program's loop.
//!
//! The programs' key and mouse types live in the terminal-free `leviculum-ui`
//! (a core takes them without the terminal crate) and are re-exported here,
//! so a frontend names one module for both.

use std::io;

use crossterm::event::Event;

pub use leviculum_ui::input::*;

/// A crossterm key event as the programs' own. One-to-one except for the keys
/// no binding names, which all become [`KeyCode::Other`].
pub fn key_event(key: crossterm::event::KeyEvent) -> KeyEvent {
    use crossterm::event::{KeyCode as Ct, KeyEventKind as CtKind};
    let code = match key.code {
        Ct::Backspace => KeyCode::Backspace,
        Ct::Enter => KeyCode::Enter,
        Ct::Left => KeyCode::Left,
        Ct::Right => KeyCode::Right,
        Ct::Up => KeyCode::Up,
        Ct::Down => KeyCode::Down,
        Ct::Home => KeyCode::Home,
        Ct::End => KeyCode::End,
        Ct::PageUp => KeyCode::PageUp,
        Ct::PageDown => KeyCode::PageDown,
        Ct::Tab => KeyCode::Tab,
        Ct::BackTab => KeyCode::BackTab,
        Ct::Delete => KeyCode::Delete,
        Ct::Insert => KeyCode::Insert,
        Ct::F(n) => KeyCode::F(n),
        Ct::Char(c) => KeyCode::Char(c),
        Ct::Null => KeyCode::Null,
        Ct::Esc => KeyCode::Esc,
        _ => KeyCode::Other,
    };
    let kind = match key.kind {
        CtKind::Press => KeyEventKind::Press,
        CtKind::Repeat => KeyEventKind::Repeat,
        CtKind::Release => KeyEventKind::Release,
    };
    KeyEvent {
        code,
        modifiers: key_modifiers(key.modifiers),
        kind,
    }
}

/// crossterm's modifier set as the programs' own; the bit order is the same.
pub fn key_modifiers(mods: crossterm::event::KeyModifiers) -> KeyModifiers {
    KeyModifiers::from_bits_truncate(mods.bits())
}

/// A crossterm mouse event as the programs' own, one-to-one.
pub fn mouse_event(mouse: crossterm::event::MouseEvent) -> MouseEvent {
    use crossterm::event::{MouseButton as CtButton, MouseEventKind as CtKind};
    let button = |b: CtButton| match b {
        CtButton::Left => MouseButton::Left,
        CtButton::Right => MouseButton::Right,
        CtButton::Middle => MouseButton::Middle,
        CtButton::Back => MouseButton::Back,
        CtButton::Forward => MouseButton::Forward,
    };
    let kind = match mouse.kind {
        CtKind::Down(b) => MouseEventKind::Down(button(b)),
        CtKind::Up(b) => MouseEventKind::Up(button(b)),
        CtKind::Drag(b) => MouseEventKind::Drag(button(b)),
        CtKind::Moved => MouseEventKind::Moved,
        CtKind::ScrollDown => MouseEventKind::ScrollDown,
        CtKind::ScrollUp => MouseEventKind::ScrollUp,
        CtKind::ScrollLeft => MouseEventKind::ScrollLeft,
        CtKind::ScrollRight => MouseEventKind::ScrollRight,
    };
    MouseEvent {
        kind,
        column: mouse.column,
        row: mouse.row,
        modifiers: key_modifiers(mouse.modifiers),
    }
}

/// What the event loop should do with one item pulled from the terminal
/// `EventStream` (crossterm's `event-stream` feature).
pub enum EventStep<E> {
    /// Apply this program event to the model (a mapped, actionable event).
    Apply(E),
    /// A recognised-but-inert event that maps to nothing: do nothing and keep
    /// looping. Crossterm discards unparseable byte sequences internally (its
    /// `parse_event` contract clears the buffer on error), so those never reach
    /// us as a `Some(Err)` — the only event we ignore is a mapped-to-`None` one
    /// (focus gained/lost, paste).
    Ignore,
    /// Leave the loop with this result: `Ok(())` on a clean close (the stream
    /// ended), `Err` on a genuine IO error from the event stream (a stdin/poll
    /// failure). Ignoring the latter would busy-loop on a dead terminal.
    End(io::Result<()>),
}

/// Classify one `EventStream` item without touching the model, so the loop's
/// exit behaviour is unit-testable. `map` is the program's own translation
/// into its event type, `None` for an event it does not act on. A `Some(Err)`
/// is a real IO error (stdin or poll failure) — crossterm never surfaces
/// unparseable sequences as errors, it discards them internally — so it is an
/// exit, NOT ignore-and-continue: ignoring it risks a busy-loop on a dead
/// terminal. `None` (stream closed) is a clean exit.
pub fn step_event<E>(
    item: Option<Result<Event, io::Error>>,
    map: impl FnOnce(Event) -> Option<E>,
) -> EventStep<E> {
    match item {
        Some(Ok(event)) => match map(event) {
            Some(app) => EventStep::Apply(app),
            None => EventStep::Ignore,
        },
        Some(Err(err)) => EventStep::End(Err(err)),
        None => EventStep::End(Ok(())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_event_exits_on_stream_error_and_close() {
        // The program's map: keys only, the way lnomad drops focus and paste.
        fn map(event: Event) -> Option<KeyEvent> {
            match event {
                Event::Key(key) => Some(key_event(key)),
                _ => None,
            }
        }
        // A Some(Err) from the event stream is a genuine IO error (stdin/poll
        // failure); crossterm discards unparseable sequences internally and
        // never surfaces them as errors. So it EXITS the loop, carrying the
        // error, rather than being ignored (which would busy-loop a dead tty).
        let err = io::Error::other("poll failure");
        assert!(matches!(
            step_event(Some(Err(err)), map),
            EventStep::End(Err(_))
        ));
        // A closed stream (None) is a clean exit.
        assert!(matches!(step_event(None, map), EventStep::End(Ok(()))));
        // A real key still maps to an applied program event.
        let ev = Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('q'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(matches!(
            step_event(Some(Ok(ev)), map),
            EventStep::Apply(KeyEvent {
                code: KeyCode::Char('q'),
                ..
            })
        ));
        // An event the program does not act on is ignored, not an exit.
        assert!(matches!(
            step_event(Some(Ok(Event::FocusGained)), map),
            EventStep::Ignore
        ));
    }

    /// Every crossterm key code the core knows, for the translation tests.
    fn crossterm_codes() -> Vec<crossterm::event::KeyCode> {
        use crossterm::event::KeyCode as Ct;
        let mut codes = vec![
            Ct::Backspace,
            Ct::Enter,
            Ct::Left,
            Ct::Right,
            Ct::Up,
            Ct::Down,
            Ct::Home,
            Ct::End,
            Ct::PageUp,
            Ct::PageDown,
            Ct::Tab,
            Ct::BackTab,
            Ct::Delete,
            Ct::Insert,
            Ct::F(1),
            Ct::F(5),
            Ct::Null,
            Ct::Esc,
            Ct::CapsLock,
            Ct::KeypadBegin,
        ];
        codes.extend("abdefghkquwyAZ:/?1 ".chars().map(Ct::Char));
        codes
    }

    /// Every combination of the six modifier bits, in crossterm's type.
    fn crossterm_modifier_sets() -> Vec<crossterm::event::KeyModifiers> {
        (0u8..64)
            .map(crossterm::event::KeyModifiers::from_bits_truncate)
            .collect()
    }

    /// The translation into the programs' key type loses nothing a binding can
    /// see: same code (or `Other` for the keys no binding names), same
    /// modifier bits, same press/repeat/release.
    #[test]
    fn crossterm_keys_reach_the_core_one_to_one() {
        use crossterm::event::{KeyCode as Ct, KeyEventKind as CtKind};
        for code in crossterm_codes() {
            for mods in crossterm_modifier_sets() {
                for kind in [CtKind::Press, CtKind::Repeat, CtKind::Release] {
                    let mut ct = crossterm::event::KeyEvent::new(code, mods);
                    ct.kind = kind;
                    let core = key_event(ct);
                    assert_eq!(core.modifiers.bits(), mods.bits(), "{ct:?}");
                    assert_eq!(format!("{:?}", core.kind), format!("{kind:?}"), "{ct:?}");
                    let want = match code {
                        Ct::CapsLock | Ct::KeypadBegin => "Other".to_string(),
                        other => format!("{other:?}"),
                    };
                    assert_eq!(format!("{:?}", core.code), want, "{ct:?}");
                }
            }
        }
    }

    /// The shared text-editing table is the one `tui-input`'s crossterm
    /// backend applied before lnomad's cut (#219): for every key, every modifier
    /// set and every kind, the same edit or the same refusal.
    #[test]
    fn the_core_edits_text_exactly_as_the_crossterm_backend_did() {
        use crossterm::event::KeyEventKind as CtKind;
        for code in crossterm_codes() {
            for mods in crossterm_modifier_sets() {
                for kind in [CtKind::Press, CtKind::Repeat, CtKind::Release] {
                    let mut ct = crossterm::event::KeyEvent::new(code, mods);
                    ct.kind = kind;
                    let reference =
                        tui_input::backend::crossterm::to_input_request(&Event::Key(ct));
                    assert_eq!(input_request(&key_event(ct)), reference, "{ct:?}");
                }
            }
        }
    }

    /// Mouse events cross with their button, position and modifiers intact.
    #[test]
    fn crossterm_mouse_events_reach_the_core_one_to_one() {
        use crossterm::event::{MouseButton as CtButton, MouseEventKind as CtKind};
        let buttons = [
            CtButton::Left,
            CtButton::Right,
            CtButton::Middle,
            CtButton::Back,
            CtButton::Forward,
        ];
        let mut kinds = vec![
            CtKind::Moved,
            CtKind::ScrollDown,
            CtKind::ScrollUp,
            CtKind::ScrollLeft,
            CtKind::ScrollRight,
        ];
        for b in buttons {
            kinds.extend([CtKind::Down(b), CtKind::Up(b), CtKind::Drag(b)]);
        }
        for kind in kinds {
            let ct = crossterm::event::MouseEvent {
                kind,
                column: 7,
                row: 3,
                modifiers: crossterm::event::KeyModifiers::ALT
                    | crossterm::event::KeyModifiers::SHIFT,
            };
            let core = mouse_event(ct);
            assert_eq!(format!("{:?}", core.kind), format!("{kind:?}"));
            assert_eq!((core.column, core.row), (7, 3));
            assert_eq!(core.modifiers, KeyModifiers::ALT | KeyModifiers::SHIFT);
        }
    }
}
