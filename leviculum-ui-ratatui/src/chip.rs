//! The floating status chip: a one-line card on the content's last row, set
//! off from the page by a thin top edge, for the CURRENT state (a spinner and
//! what is loading, the target under the pointer). The program decides what
//! the chip says; this draws it.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as RtLine, Span as RtSpan};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use leviculum_ui::theme::Theme;

use crate::style::{chrome_style, rgb};

/// Columns of padding on each side of the chip's body.
pub const PAD: u16 = 1;

/// The display width the chip's body may take in `content`, padding removed:
/// truncate the body to this so the card never overflows the content width.
pub fn budget(content: Rect) -> usize {
    content.width.saturating_sub(2 * PAD) as usize
}

/// Draw the chip at the bottom-left of `content` with `spans` as its body,
/// `inner_w` columns wide (at most [`budget`]).
pub fn render(
    frame: &mut Frame,
    content: Rect,
    spans: Vec<RtSpan<'static>>,
    inner_w: u16,
    no_color: bool,
    theme: Theme,
) {
    let pad = PAD;
    let field_w = (inner_w + 2 * pad).min(content.width);
    let field_y = content.bottom().saturating_sub(1);
    let chip_bg = chrome_style(no_color, theme);
    // A thin top edge one row above the field sets the card off from the page and
    // from the footer beneath it, without a full border box. Theme-aware; under
    // `no_color` the reverse-video chrome fill plus an underline reads as an edge.
    if content.height >= 2 {
        let edge = Rect {
            x: content.x,
            y: field_y.saturating_sub(1),
            width: field_w,
            height: 1,
        };
        let edge_style = if no_color {
            chip_bg.add_modifier(Modifier::UNDERLINED)
        } else {
            chip_bg.patch(Style::default().fg(rgb(theme.chrome_muted_fg())))
        };
        frame.render_widget(Clear, edge);
        frame.render_widget(
            Paragraph::new(RtSpan::styled("▔".repeat(field_w as usize), edge_style)),
            edge,
        );
    }
    // The one-line field itself: chrome background, one-column left padding, the
    // spinner / target painted on top (same Clear-then-paint technique as the toast).
    let field = Rect {
        x: content.x,
        y: field_y,
        width: field_w,
        height: 1,
    };
    frame.render_widget(Clear, field);
    frame.render_widget(Block::default().style(chip_bg), field);
    let inner = Rect {
        x: field.x.saturating_add(pad),
        y: field.y,
        width: field.width.saturating_sub(2 * pad),
        height: 1,
    };
    frame.render_widget(Paragraph::new(RtLine::from(spans)), inner);
}
