//! Character-cell geometry: the rectangle a program lays its screen out in.
//!
//! A program's core does its own hit-testing (a click at a cell follows what
//! is drawn there), so it needs rectangles; owning them here keeps that
//! arithmetic off the terminal crate. `leviculum-ui-ratatui` converts to
//! ratatui's `Rect` at the drawing call and nowhere else.

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

/// Whether `rect` contains the point `(x, y)`.
pub fn rect_contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}
