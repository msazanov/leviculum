//! The one conversion between the shared cell rectangle and ratatui's, so a
//! program's core lays out in [`leviculum_ui::geometry::Rect`] and the drawing
//! code converts at the call.
//!
//! Functions, not `From` impls: both types are foreign here, and the
//! terminal-free crate cannot name ratatui to own the impl.

use leviculum_ui::geometry::Rect as CellRect;
use ratatui::layout::Rect;

/// A shared cell rectangle as the ratatui `Rect` covering the same cells.
pub fn to_ratatui(r: CellRect) -> Rect {
    Rect::new(r.x, r.y, r.width, r.height)
}

/// A ratatui `Rect` as the shared cell rectangle covering the same cells.
pub fn from_ratatui(r: Rect) -> CellRect {
    CellRect::new(r.x, r.y, r.width, r.height)
}
