//! Character-cell geometry: the three fixed screen regions the core lays the
//! browser out in, over the shared cell rectangle of `leviculum-ui`.
//!
//! The core does its own hit-testing (a click at a cell follows the link drawn
//! there), so it needs rectangles; taking them from the terminal-free
//! `leviculum-ui` keeps that arithmetic off the terminal crate. The terminal
//! frontend converts to ratatui's `Rect` at the drawing call and nowhere else
//! (`leviculum-ui-ratatui` carries the conversion).

pub(crate) use leviculum_ui::geometry::rect_contains;
pub use leviculum_ui::geometry::Rect;

use super::model::{STATUS_ROWS, TOPBAR_ROWS};

/// Split the frame into the three fixed regions: top-bar, content, status.
///
/// The status bar takes its rows first, then the top-bar, and the content
/// whatever is left: on a one-row frame the single row is the status bar.
/// That is the split ratatui's `Layout` with `Length`, `Min(0)`, `Length`
/// gave the drawing code before the core owned it, and a frontend test pins
/// the two together.
pub(crate) fn regions(area: Rect) -> [Rect; 3] {
    let status = STATUS_ROWS.min(area.height);
    let top = TOPBAR_ROWS.min(area.height - status);
    let content = area.height - top - status;
    [
        Rect::new(area.x, area.y, area.width, top),
        Rect::new(area.x, area.y + top, area.width, content),
        Rect::new(area.x, area.y + top + content, area.width, status),
    ]
}
