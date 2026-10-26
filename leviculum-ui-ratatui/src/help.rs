//! The help overlay: a program's keybinding groups
//! ([`leviculum_ui::help`]) drawn centred, keys bright and bold, descriptions
//! muted, scrolled with a scrollbar when the terminal is too short.

use ratatui::layout::{Margin, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line as RtLine, Span as RtSpan, Text};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use leviculum_ui::help::{key_col, overlay_rect, HelpGroup, HELP_GAP, HELP_INDENT};
use leviculum_ui::theme::Theme;

use crate::geometry::{from_ratatui, to_ratatui};
use crate::style::{chrome_muted_style, chrome_style, chrome_text_style};

/// Draw the centered help overlay: the keybindings grouped under dim titles,
/// each entry's keys in the bright chrome fg and BOLD and its description in the
/// muted chrome fg — the same visual language as the footer strip, theme-aware
/// and `no_color`-safe. The block's border title carries the heading and the
/// close hints (`title`), so the body has no heading line. `scroll` is the
/// first line shown when the overlay is taller than `area`, clamped here.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    groups: &[HelpGroup],
    title: &str,
    scroll: usize,
    no_color: bool,
    theme: Theme,
) {
    let col = key_col(groups);

    // Group titles are dim; under `no_color` the reverse-video fill already
    // delineates them, so drop the DIM (no DIM under reverse) and let the plain
    // style show through.
    let muted = chrome_muted_style(no_color, theme);
    let title_style = if no_color {
        muted
    } else {
        muted.add_modifier(Modifier::DIM)
    };
    let keys_style = chrome_text_style(no_color, theme).add_modifier(Modifier::BOLD);
    let desc_style = muted;

    // Build the styled lines. Each entry's keys cell and its (padded) description
    // reproduce [`lines`] exactly, so the styled overlay and its plain-text
    // projection stay byte-for-byte aligned.
    let mut styled: Vec<RtLine> = Vec::new();
    for (gi, group) in groups.iter().enumerate() {
        if gi > 0 {
            styled.push(RtLine::from(""));
        }
        styled.push(RtLine::from(RtSpan::styled(
            group.title.to_string(),
            title_style,
        )));
        for e in &group.entries {
            let pad = col.saturating_sub(UnicodeWidthStr::width(e.keys)) + HELP_GAP;
            styled.push(RtLine::from(vec![
                RtSpan::styled(format!("{HELP_INDENT}{}", e.keys), keys_style),
                RtSpan::styled(format!("{}{}", " ".repeat(pad), e.desc), desc_style),
            ]));
        }
    }

    // Sized to the widest line plus its two borders, so no line wraps in a
    // normal-width terminal; `Wrap` below stays as a safety net for terminals
    // too narrow to hold the widest line. The rectangle is the model's
    // ([`overlay_rect`]), so a click hit-tests against what is drawn.
    let overlay = to_ratatui(overlay_rect(groups, from_ratatui(area)));
    let height = overlay.height;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(chrome_style(no_color, theme));
    frame.render_widget(Clear, overlay);

    // The block's inner height is the viewport; the help lines are the content.
    // When the terminal is too short to hold them all, draw the slice starting at
    // `scroll` (clamped) and paint a scrollbar on the right border. The clamp
    // keeps the offset in range, so slicing is always sound.
    let total = styled.len();
    let inner_h = height.saturating_sub(2) as usize;
    let max = total.saturating_sub(inner_h);
    let offset = scroll.min(max);
    let visible: Vec<RtLine> = styled.into_iter().skip(offset).take(inner_h).collect();
    frame.render_widget(
        Paragraph::new(Text::from(visible))
            .block(block)
            .wrap(Wrap { trim: false }),
        overlay,
    );
    if max > 0 {
        let mut state = ScrollbarState::new(max).position(offset);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None);
        frame.render_stateful_widget(scrollbar, overlay.inner(Margin::new(0, 1)), &mut state);
    }
}
