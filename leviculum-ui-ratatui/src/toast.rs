//! The toast: a transient note ([`leviculum_ui::toast`]) floated at the
//! bottom-right of the content.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span as RtSpan;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use leviculum_ui::text::truncate_to_cols;
use leviculum_ui::toast::{Toast, ToastKind};

/// Draw the transient toast: a small bordered box floated at the bottom-right of
/// `content` (never over the status bar), coloured by kind. An error is an
/// attention red with a `⚠`; an info note is a calm green with a `✓`. Under
/// `no_color` both fall back to reverse video.
pub fn render(frame: &mut Frame, content: Rect, toast: &Toast, no_color: bool) {
    // Too little room to float a bordered box: skip rather than draw garbage.
    if content.width < 8 || content.height < 3 {
        return;
    }
    let (glyph, accent) = match toast.kind {
        ToastKind::Error => ('⚠', Color::Rgb(255, 95, 95)),
        ToastKind::Info => ('✓', Color::Rgb(120, 220, 120)),
    };
    let style = if no_color {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().fg(accent)
    };
    // A one-column margin from the content's right edge, room for both borders,
    // and the message truncated to whatever inner width is left.
    let margin = 1u16;
    let max_inner = content.width.saturating_sub(2 + 2 * margin).max(1) as usize;
    let body = truncate_to_cols(&format!("{glyph} {}", toast.text), max_inner);
    let inner_w = UnicodeWidthStr::width(body.as_str()) as u16;
    let box_w = inner_w + 2;
    let box_h = 3u16;
    // Bottom-right of the content, sitting just above the status bar.
    let overlay = Rect {
        x: content.right().saturating_sub(box_w + margin),
        y: content.bottom().saturating_sub(box_h),
        width: box_w,
        height: box_h,
    };
    let block = Block::default().borders(Borders::ALL).style(style);
    frame.render_widget(Clear, overlay);
    frame.render_widget(
        Paragraph::new(RtSpan::styled(body, style)).block(block),
        overlay,
    );
}
