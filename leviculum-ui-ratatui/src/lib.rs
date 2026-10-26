//! `leviculum-ui-ratatui`: the ratatui and crossterm half of the UI that
//! lnomad and loquax share (Codeberg #475), over the terminal-free model of
//! `leviculum-ui`.
//!
//! The widgets every keyboard program here draws the same way: the chrome
//! styles and the colour-depth post-pass ([`style`]), the bridge from the
//! styled text model to ratatui text, the spinner span, the toast, the help
//! overlay, the status chip, the footer strip, and inline pictures behind
//! the `images` feature. And the shell around them: the terminal lifecycle
//! with its restore guard and panic hook ([`terminal`]), crossterm's events
//! as the programs' own ([`event`]), and the background query behind
//! `--theme auto` ([`background`]).
//!
//! A program keeps its own labels, its screen layout and its state; these
//! take them as arguments.

pub mod background;
pub mod chip;
pub mod event;
pub mod footer;
pub mod geometry;
pub mod help;
#[cfg(feature = "images")]
pub mod images;
pub mod spinner;
pub mod style;
pub mod terminal;
pub mod toast;

pub use style::to_ratatui_text;
pub use terminal::{CrosstermTerminal, TerminalGuard, TerminalOps};
