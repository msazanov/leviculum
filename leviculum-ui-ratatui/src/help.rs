//! The help overlay: a program's keybinding groups
//! ([`leviculum_ui::help`]) drawn centred, keys bright and bold, descriptions
//! muted, scrolled with a scrollbar when the terminal is too short.

use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as RtLine, Span as RtSpan, Text};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use core::ops::Range;

use leviculum_ui::help::{
    key_col, overlay_rect, rows, scroll_bounds, HelpGroup, HELP_GAP, HELP_INDENT,
};
use leviculum_ui::theme::Theme;

use crate::geometry::{from_ratatui, to_ratatui};
use crate::style::{chrome_muted_style, chrome_style, chrome_text_style};

/// Draw the centered help overlay: the keybindings grouped under dim titles,
/// each entry's keys in the bright chrome fg and BOLD and its description in the
/// muted chrome fg — the same visual language as the footer strip, theme-aware
/// and `no_color`-safe. The block's border title carries the heading and the
/// close hints (`title`), so the body has no heading line. `scroll` is the
/// first drawn row ([`rows`]) shown when the overlay is taller than `area`,
/// clamped here.
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

    // Each line as segments of (characters, style): a title, or an entry's
    // keys cell and its padded description. They reproduce [`lines`] exactly,
    // so a drawn row ([`rows`], a character range of one line) is styled by
    // cutting its line's segments to that range.
    let mut segments: Vec<Vec<(String, Style)>> = Vec::new();
    for (gi, group) in groups.iter().enumerate() {
        if gi > 0 {
            segments.push(Vec::new());
        }
        segments.push(vec![(group.title.to_string(), title_style)]);
        for e in &group.entries {
            let pad = col.saturating_sub(UnicodeWidthStr::width(e.keys)) + HELP_GAP;
            segments.push(vec![
                (format!("{HELP_INDENT}{}", e.keys), keys_style),
                (format!("{}{}", " ".repeat(pad), e.desc), desc_style),
            ]);
        }
    }

    // Sized to the widest line plus its two borders, so no line wraps in a
    // normal-width terminal; a narrower one wraps by [`rows`]. The rectangle
    // is the model's ([`overlay_rect`]), so a click hit-tests against what is
    // drawn.
    let model_area = from_ratatui(area);
    let overlay = to_ratatui(overlay_rect(groups, model_area));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(chrome_style(no_color, theme));
    frame.render_widget(Clear, overlay);

    // The block's inner height is the viewport; the drawn rows are the
    // content. When the terminal is too short to hold them all, draw the slice
    // starting at `scroll` (clamped) and paint a scrollbar on the right border.
    // The bounds are the ones a program's key handler clamps to
    // ([`scroll_bounds`]), so the last row is always reachable.
    let (inner_h, max) = scroll_bounds(groups, model_area);
    let offset = scroll.min(max);
    let visible: Vec<RtLine> = rows(groups, model_area)
        .into_iter()
        .skip(offset)
        .take(inner_h)
        .map(|row| styled_row(&segments[row.line], row.chars))
        .collect();
    frame.render_widget(Paragraph::new(Text::from(visible)).block(block), overlay);
    if max > 0 {
        let mut state = ScrollbarState::new(max).position(offset);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None);
        frame.render_stateful_widget(scrollbar, overlay.inner(Margin::new(0, 1)), &mut state);
    }
}

/// The characters `chars` of a line given as styled segments, each keeping
/// its segment's style.
fn styled_row(segments: &[(String, Style)], chars: Range<usize>) -> RtLine<'static> {
    let mut spans = Vec::new();
    let mut at = 0;
    for (text, style) in segments {
        let len = text.chars().count();
        let from = chars.start.max(at);
        let to = chars.end.min(at + len);
        if from < to {
            let part: String = text.chars().skip(from - at).take(to - from).collect();
            spans.push(RtSpan::styled(part, *style));
        }
        at += len;
    }
    RtLine::from(spans)
}
