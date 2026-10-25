//! Character-cell geometry: the rectangle type the core lays the browser out
//! in, and the three fixed screen regions.
//!
//! The core does its own hit-testing (a click at a cell follows the link drawn
//! there), so it needs rectangles; owning them here keeps that arithmetic off
//! the terminal crate. The terminal frontend converts to ratatui's `Rect` at
//! the drawing call and nowhere else.

use super::model::{STATUS_ROWS, TOPBAR_ROWS};

/// A rectangle of character cells. The same fields and edge arithmetic as
/// ratatui's `Rect`, so a converted rectangle covers the same cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    /// Left column.
    pub x: u16,
    /// Top row.
    pub y: u16,
    /// Width in columns.
    pub width: u16,
    /// Height in rows.
    pub height: u16,
}

impl Rect {
    /// A rectangle at `(x, y)`, `width` by `height`.
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The first column past the right edge.
    pub const fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// The first row past the bottom edge.
    pub const fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }
}

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

/// Whether `rect` contains the point `(x, y)`.
pub(crate) fn rect_contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}
