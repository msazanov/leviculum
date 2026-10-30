//! The terminal lifecycle: raw mode, the alternate screen and mouse capture
//! on the way in, all of it reversed on the way out, on every path out
//! (normal return, `?`, panic unwind, and a panic's own message).

use std::io::{self, Stdout, Write};

use crossterm::cursor::MoveTo;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, Clear, ClearType,
    EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::QueueableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::{Frame, Terminal};

/// The terminal side-effects the IO shell needs, abstracted so a test can
/// substitute a mock and assert that `restore` runs, with no real terminal.
pub trait TerminalOps {
    /// Enter the UI: alternate screen, raw mode, mouse capture.
    fn enter(&mut self) -> io::Result<()>;
    /// Restore the terminal to how it was before [`enter`](TerminalOps::enter).
    fn restore(&mut self) -> io::Result<()>;
}

/// The real crossterm terminal: alt-screen + raw mode + mouse capture on
/// [`enter`](TerminalOps::enter), reversed on [`restore`](TerminalOps::restore).
pub struct CrosstermTerminal {
    out: Stdout,
    mouse: bool,
}

impl CrosstermTerminal {
    /// A terminal driver writing to stdout, capturing the mouse.
    pub fn new() -> Self {
        Self::with_mouse(true)
    }

    /// The same, capturing the mouse only when `mouse` is set: without
    /// capture the terminal's own selection and copy work as in any other
    /// program (loquax's `--no-mouse`).
    pub fn with_mouse(mouse: bool) -> Self {
        Self {
            out: io::stdout(),
            mouse,
        }
    }
}

/// Turn mouse capture on or off while the UI is up: a program's runtime
/// toggle. Leaving the UI turns it off either way.
pub fn set_mouse_capture(on: bool) -> io::Result<()> {
    let mut out = io::stdout();
    match on {
        true => execute!(out, EnableMouseCapture),
        false => execute!(out, DisableMouseCapture),
    }
}

impl Default for CrosstermTerminal {
    fn default() -> Self {
        Self::new()
    }
}

/// The bytes that enter the UI: the alternate screen, mouse capture when
/// `mouse` is set, then a cleared screen with the cursor home. ratatui's first
/// draw diffs against a blank buffer and never writes a blank cell, so the
/// clear is what blanks them. Not every terminal gives a fresh alternate
/// screen: mosh 1.4.0 ignores `?1049h` (its `get_DEC_mode` has no case for
/// it), and the shell output stayed visible behind the first frame.
fn write_enter<W: Write>(out: &mut W, mouse: bool) -> io::Result<()> {
    execute!(out, EnterAlternateScreen)?;
    if mouse {
        execute!(out, EnableMouseCapture)?;
    }
    execute!(out, Clear(ClearType::All), MoveTo(0, 0))
}

impl TerminalOps for CrosstermTerminal {
    fn enter(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        write_enter(&mut self.out, self.mouse)
    }

    fn restore(&mut self) -> io::Result<()> {
        execute!(self.out, LeaveAlternateScreen, DisableMouseCapture)?;
        disable_raw_mode()
    }
}

/// A RAII guard that restores the terminal on drop. Constructing it enters the
/// UI; dropping it (normally, on `?`, or during a panic unwind) restores it, so
/// no code path can leave the terminal in raw/alt-screen mode.
pub struct TerminalGuard<T: TerminalOps> {
    ops: T,
    restored: bool,
}

impl<T: TerminalOps> TerminalGuard<T> {
    /// Enter the UI and return a guard that will restore it.
    pub fn new(mut ops: T) -> io::Result<Self> {
        ops.enter()?;
        Ok(Self {
            ops,
            restored: false,
        })
    }

    /// Enter the UI again after something left it while the guard held it:
    /// the panic hook does, and a caller that caught the panic carries on.
    pub fn enter_again(&mut self) -> io::Result<()> {
        self.ops.enter()?;
        self.restored = false;
        Ok(())
    }

    /// Restore the terminal now (idempotent); the drop becomes a no-op.
    pub fn restore_now(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        self.ops.restore()
    }
}

impl<T: TerminalOps> Drop for TerminalGuard<T> {
    fn drop(&mut self) {
        let _ = self.restore_now();
    }
}

/// Install a panic hook that restores the terminal BEFORE the previous hook
/// runs, so a panic's backtrace prints to a sane (cooked) terminal instead of
/// the raw alternate screen.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        let _ = execute!(
            out,
            EndSynchronizedUpdate,
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        let _ = disable_raw_mode();
        previous(info);
    }));
}

/// Draw one frame between a begin and an end of synchronized update (mode
/// 2026): the terminal shows the screen as it was until the frame is whole,
/// never a state inside it. A picture is written as an erase of its cells
/// and then the sixel; foot fed frames back to back showed the erased pane
/// in 128 of 206 grabs of a map drag with names, and none with the frame
/// bracketed (1118). A terminal without the mode ignores both. The end is
/// written when `render` panics as well, so a frame that panics never holds
/// the screen.
pub fn draw_synced<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    render: impl FnOnce(&mut Frame),
) -> io::Result<()> {
    terminal.backend_mut().queue(BeginSynchronizedUpdate)?;
    let drawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        terminal.draw(render).map(|_| ())
    }));
    let ended = execute!(terminal.backend_mut(), EndSynchronizedUpdate);
    match drawn {
        Ok(drawn) => drawn.and(ended),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    struct MockOps {
        entered: Rc<Cell<bool>>,
        restored: Rc<Cell<bool>>,
    }

    impl TerminalOps for MockOps {
        fn enter(&mut self) -> io::Result<()> {
            self.entered.set(true);
            Ok(())
        }
        fn restore(&mut self) -> io::Result<()> {
            self.restored.set(true);
            Ok(())
        }
    }

    #[test]
    fn guard_restores_on_drop() {
        let entered = Rc::new(Cell::new(false));
        let restored = Rc::new(Cell::new(false));
        {
            let guard = TerminalGuard::new(MockOps {
                entered: entered.clone(),
                restored: restored.clone(),
            })
            .expect("enter");
            assert!(entered.get(), "enter should have run");
            assert!(!restored.get(), "restore must not run before drop");
            drop(guard);
        }
        assert!(restored.get(), "restore must run on drop");
    }

    /// Entered again after a caught panic left the UI: the drop restores
    /// once more.
    #[test]
    fn guard_entered_again_restores_again() {
        let entered = Rc::new(Cell::new(false));
        let restored = Rc::new(Cell::new(false));
        let mut guard = TerminalGuard::new(MockOps {
            entered: entered.clone(),
            restored: restored.clone(),
        })
        .expect("enter");
        guard.restore_now().expect("restore");
        entered.set(false);
        restored.set(false);
        guard.enter_again().expect("enter again");
        assert!(entered.get());
        drop(guard);
        assert!(
            restored.get(),
            "restore must run on drop after entering again"
        );
    }

    /// The screen is cleared and the cursor homed after the alternate screen
    /// switch, with and without mouse capture.
    #[test]
    fn enter_clears_after_the_alternate_screen() {
        let clear = "\x1b[2J\x1b[1;1H";
        for mouse in [false, true] {
            let mut out = Vec::new();
            write_enter(&mut out, mouse).expect("write");
            let text = String::from_utf8(out).expect("utf8");
            let alt = text.find("\x1b[?1049h").expect("alternate screen");
            assert!(
                text.ends_with(clear),
                "mouse={mouse}: no clear at the end: {text:?}"
            );
            assert!(alt < text.len() - clear.len());
        }
    }

    #[test]
    fn guard_restore_now_is_idempotent() {
        let restored = Rc::new(Cell::new(false));
        let mut guard = TerminalGuard::new(MockOps {
            entered: Rc::new(Cell::new(false)),
            restored: restored.clone(),
        })
        .expect("enter");
        guard.restore_now().expect("restore");
        assert!(restored.get());
        restored.set(false);
        guard.restore_now().expect("restore idempotent");
        assert!(!restored.get(), "restore ran twice");
    }

    /// A writer whose bytes the test keeps after the terminal has them.
    #[derive(Clone, Default)]
    struct Sink(Rc<std::cell::RefCell<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn sink_terminal(sink: &Sink) -> Terminal<CrosstermBackend<Sink>> {
        let area = ratatui::layout::Rect::new(0, 0, 10, 2);
        let options = ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(area),
        };
        Terminal::with_options(CrosstermBackend::new(sink.clone()), options).expect("terminal")
    }

    /// The frame's bytes stand between the begin and the end of a
    /// synchronized update, the begin first and the end last (1118).
    #[test]
    fn a_synced_frame_is_bracketed_by_mode_2026() {
        let sink = Sink::default();
        let mut terminal = sink_terminal(&sink);
        draw_synced(&mut terminal, |f| {
            f.render_widget(ratatui::widgets::Paragraph::new("map"), f.area())
        })
        .expect("draw");
        let text = String::from_utf8(sink.0.borrow().clone()).expect("utf8");
        let (begin, end) = ("\x1b[?2026h", "\x1b[?2026l");
        assert!(text.starts_with(begin), "{text:?}");
        assert!(text.ends_with(end), "{text:?}");
        let map = text.find("map").expect("the frame");
        assert!(begin.len() <= map && map < text.len() - end.len());
    }

    /// A frame that panics still ends the update, so the terminal does not
    /// keep the screen held; the panic goes on to the caller.
    #[test]
    fn a_frame_that_panics_still_ends_the_update() {
        let sink = Sink::default();
        let mut terminal = sink_terminal(&sink);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            draw_synced(&mut terminal, |_| panic!("a frame that panics"))
        }));
        assert!(panicked.is_err(), "the panic reaches the caller");
        let text = String::from_utf8(sink.0.borrow().clone()).expect("utf8");
        assert!(text.starts_with("\x1b[?2026h"), "{text:?}");
        assert!(text.ends_with("\x1b[?2026l"), "{text:?}");
    }
}
