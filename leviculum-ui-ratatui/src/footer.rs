//! The footer strip: the laid-out buttons of [`leviculum_ui::footer`], each
//! key bright and bold, each label muted, with dividers and a hover
//! highlight.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as RtLine, Span as RtSpan};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use leviculum_ui::footer::FooterSlot;
use leviculum_ui::theme::Theme;

use crate::geometry::to_ratatui;
use crate::style::{chrome_muted_style, chrome_text_style};

/// Draw the footer button strip: each button's key in the bright chrome fg and
/// bold, its label in the muted chrome fg, a `│` divider between buttons (a
/// brighter one at the nav/mode boundary), and a bold + underline highlight over
/// the hovered button (`hover`, a [`FooterSlot::index`]) so it reads as
/// clickable.
pub fn render<A>(
    frame: &mut Frame,
    area: Rect,
    slots: &[FooterSlot<A>],
    hover: Option<usize>,
    no_color: bool,
    theme: Theme,
) {
    let key_style = chrome_text_style(no_color, theme).add_modifier(Modifier::BOLD);
    let label_style = chrome_muted_style(no_color, theme);
    let dim_div = chrome_muted_style(no_color, theme);
    let strong_div = chrome_text_style(no_color, theme);

    for slot in slots {
        // The divider before this button (none before the first).
        let dw = slot.div_before;
        if dw > 0 {
            let (glyph, style) = if slot.strong_divider {
                ("│", strong_div)
            } else {
                ("│", dim_div)
            };
            // Centre the │ in its 1-or-3-column gap.
            let gx = slot.rect.x.saturating_sub(dw).saturating_add(dw / 2);
            frame.render_widget(
                Paragraph::new(RtSpan::styled(glyph, style)),
                Rect {
                    x: gx,
                    y: area.y,
                    width: 1,
                    height: 1,
                },
            );
        }

        // The button itself: key bright + bold, label muted; an unavailable nav
        // button reads wholly muted.
        let (kstyle, lstyle) = if slot.item.available {
            (key_style, label_style)
        } else {
            (label_style, label_style)
        };
        let mut spans: Vec<RtSpan<'static>> = Vec::new();
        let key = slot.item.key;
        let label = slot.item.label;
        let show_label = !slot.collapsed && !label.is_empty();
        if slot.item.key_first {
            if !key.is_empty() {
                spans.push(RtSpan::styled(key, kstyle));
            }
            if show_label {
                if !key.is_empty() {
                    spans.push(RtSpan::styled(" ", lstyle));
                }
                spans.push(RtSpan::styled(label, lstyle));
            }
        } else {
            if show_label {
                spans.push(RtSpan::styled(label, lstyle));
                spans.push(RtSpan::styled(" ", lstyle));
            }
            if !key.is_empty() {
                spans.push(RtSpan::styled(key, kstyle));
            }
        }
        frame.render_widget(Paragraph::new(RtLine::from(spans)), to_ratatui(slot.rect));
    }

    // Highlight the hovered button, reusing the link-hover look (bold + underline
    // patched over the button's own styling), so it reads as a button.
    if let Some(idx) = hover {
        if let Some(slot) = slots.iter().find(|s| s.index == idx) {
            let area_all = frame.area();
            let buf = frame.buffer_mut();
            let hover = Modifier::BOLD | Modifier::UNDERLINED;
            for x in slot.rect.x..slot.rect.right() {
                if x >= area_all.width || slot.rect.y >= area_all.height {
                    continue;
                }
                if let Some(cell) = buf.cell_mut((x, slot.rect.y)) {
                    let style = cell.style().patch(Style::default().add_modifier(hover));
                    cell.set_style(style);
                }
            }
        }
    }
}
