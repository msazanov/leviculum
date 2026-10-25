//! `leviculum-ui`: the terminal-free half of the UI that lnomad and lnmsg
//! share (Codeberg #475, `docs/src/concepts/handheld-messenger.md`, "One
//! interaction model").
//!
//! Code is shared where it is the same problem: a styled text model, one
//! palette and its colour maths, the programs' own key and mouse events with
//! the text-editing table over them, the chrome models (footer strip, help
//! overlay, toast), the loading spinner, and the delivery glyph table of
//! `docs/src/concepts/lnmsg-ui.md`. No terminal crate is named here, which
//! is what lets a program's core take these types and stay terminal-free;
//! `just ui-no-terminal` checks it. The ratatui widgets over this model are
//! `leviculum-ui-ratatui`.
//!
//! What is NOT here: a program's own labels and actions (lnomad's footer
//! buttons and help text stay in lnomad), its layout of the screen, and its
//! state.
//!
//! Without the default `std` feature the crate is `no_std` plus `alloc`: a
//! messenger core built for a board takes these types too (lnmsg's `ui`
//! model, checked by `just lnmsg-core-no-std`).

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod color;
pub mod commands;
pub mod delivery;
pub mod footer;
pub mod geometry;
pub mod help;
pub mod input;
pub mod spinner;
pub mod style;
pub mod text;
pub mod theme;
pub mod toast;

pub use color::{resolve_depth, rgb_to_ansi256, ColorDepth, ColorFlag};
pub use geometry::Rect;
pub use style::{RLine, RStyle, StyledChar};
pub use theme::{resolve_theme, Bg, Theme, ThemeFlag};
