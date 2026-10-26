//! The toast: a transient note ([`leviculum_ui::toast`]) floated at the
//! bottom-right of the content.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as RtSpan, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use leviculum_ui::text::truncate_to_cols;
use leviculum_ui::toast::{Toast, ToastKind};

/// A one-column margin from the content's right edge.
const MARGIN: u16 = 1;

/// The toast's text behind its kind's glyph, as the box shows it.
pub fn body(toast: &Toast) -> String {
    format!("{} {}", glyph(toast.kind), toast.text)
}

/// The widest row a toast over `content` can show: the content less both
/// borders and a margin each side.
pub fn max_inner(content: Rect) -> usize {
    content.width.saturating_sub(2 + 2 * MARGIN).max(1) as usize
}

fn glyph(kind: ToastKind) -> char {
    match kind {
        ToastKind::Error => '⚠',
        ToastKind::Info => '✓',
    }
}

/// Draw the transient toast: a small bordered box floated at the bottom-right of
/// `content` (never over the status bar), coloured by kind. An error is an
/// attention red with a `⚠`; an info note is a calm green with a `✓`. Under
/// `no_color` both fall back to reverse video. One row, the text cut to the
/// room there is; [`render_rows`] draws a toast its caller wrapped.
pub fn render(frame: &mut Frame, content: Rect, toast: &Toast, no_color: bool) {
    let row = truncate_to_cols(&body(toast), max_inner(content));
    render_rows(frame, content, toast.kind, &[row], no_color);
}

/// [`render`] with the rows already cut by the caller, each at most
/// [`max_inner`] wide (a longer one is cut here): the box is as wide as the
/// widest row and grows upward from just above the status bar, one row a
/// row, as far as `content` allows.
pub fn render_rows(
    frame: &mut Frame,
    content: Rect,
    kind: ToastKind,
    rows: &[String],
    no_color: bool,
) {
    // Too little room to float a bordered box: skip rather than draw garbage.
    if content.width < 8 || content.height < 3 || rows.is_empty() {
        return;
    }
    let accent = match kind {
        ToastKind::Error => Color::Rgb(255, 95, 95),
        ToastKind::Info => Color::Rgb(120, 220, 120),
    };
    let style = if no_color {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().fg(accent)
    };
    let max = max_inner(content);
    let shown = rows.len().min(content.height.saturating_sub(2) as usize);
    let lines: Vec<Line> = rows[..shown]
        .iter()
        .map(|row| Line::from(RtSpan::styled(truncate_to_cols(row, max), style)))
        .collect();
    let inner_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let box_w = inner_w + 2;
    let box_h = shown as u16 + 2;
    // Bottom-right of the content, sitting just above the status bar.
    let overlay = Rect {
        x: content.right().saturating_sub(box_w + MARGIN),
        y: content.bottom().saturating_sub(box_h),
        width: box_w,
        height: box_h,
    };
    let block = Block::default().borders(Borders::ALL).style(style);
    frame.render_widget(Clear, overlay);
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), overlay);
}
