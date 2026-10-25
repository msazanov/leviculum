//! The footer strip's model: a row of buttons, each a key you can press and a
//! label you can click, laid out by priority so the row never overflows.
//!
//! The program supplies the items and what each one does (its own action
//! type, which tells the layout which items form the leading navigation
//! group); this module lays them out, and `leviculum-ui-ratatui` draws the
//! result. Click hit-testing and hover run over the same slots.

use unicode_width::UnicodeWidthStr;

use crate::geometry::Rect;

/// What a footer action tells the layout about itself.
pub trait FooterRole: Copy {
    /// Whether the item belongs to the leading navigation group. The first
    /// item after that group gets the stronger divider.
    fn is_nav(&self) -> bool;
}

/// One footer button: a key you can press and a label, which are the same thing
/// you can also click. The key cell renders bright + bold, the label muted.
#[derive(Clone, Copy, Debug)]
pub struct FooterItem<A> {
    /// The pressable key (bright + bold), e.g. `Alt-←`, `f`, `Esc`.
    pub key: &'static str,
    /// The muted descriptive label, e.g. `back`, `hint` (empty for a sentence).
    pub label: &'static str,
    /// Whether the key renders before the label. Everything leads with its key
    /// (`Alt-← back`, `f hint`).
    pub key_first: bool,
    /// What a press or click runs.
    pub action: A,
    /// Truncation priority: `None` never drops; `Some(rank)` drops when the row
    /// is too narrow, lowest rank first.
    pub drop_rank: Option<u8>,
    /// Whether the control is currently usable; an unavailable nav item reads
    /// muted rather than active.
    pub available: bool,
}

impl<A> FooterItem<A> {
    /// A never-dropped, always-available mode-action button.
    pub const fn action(key: &'static str, label: &'static str, act: A) -> Self {
        FooterItem {
            key,
            label,
            key_first: true,
            action: act,
            drop_rank: None,
            available: true,
        }
    }

    /// The same, but droppable at rank `r` when the row is too narrow.
    pub const fn droppable(key: &'static str, label: &'static str, act: A, r: u8) -> Self {
        FooterItem {
            key,
            label,
            key_first: true,
            action: act,
            drop_rank: Some(r),
            available: true,
        }
    }

    /// This item's rendered width, with labels (`false`) or collapsed to the key
    /// glyph only (`true`).
    pub fn width(&self, collapsed: bool) -> u16 {
        let kw = UnicodeWidthStr::width(self.key) as u16;
        if collapsed || self.label.is_empty() {
            return kw.max(UnicodeWidthStr::width(self.label) as u16);
        }
        let lw = UnicodeWidthStr::width(self.label) as u16;
        // key + one space + label (the key may be empty for a sentence).
        if kw == 0 {
            lw
        } else {
            kw + 1 + lw
        }
    }
}

impl<A: FooterRole> FooterItem<A> {
    /// Whether this item is part of the leading navigation group (rendered
    /// before the stronger divider).
    pub fn is_nav(&self) -> bool {
        self.action.is_nav()
    }
}

/// The width of the divider drawn before the item at `kept`-position `pos`: a
/// ` │ ` between items (a single space in collapsed mode, except at the stronger
/// nav/mode boundary). Position 0 has no leading divider.
pub fn divider_width(pos: usize, is_strong: bool, collapsed: bool) -> u16 {
    if pos == 0 {
        0
    } else if !collapsed || is_strong {
        3 // " │ "
    } else {
        1 // a single space between collapsed glyphs
    }
}

/// A laid-out footer button: the original item, whether its label is shown, and
/// its screen rect. Drives rendering, click hit-testing and hover highlight.
#[derive(Clone, Copy, Debug)]
pub struct FooterSlot<A> {
    /// The item this slot lays out.
    pub item: FooterItem<A>,
    /// Index into the item list it was laid out from (stable per list), so a
    /// hover can be remembered across the move-then-render round trip.
    pub index: usize,
    /// Whether the divider before this slot is the stronger nav/mode one.
    pub strong_divider: bool,
    /// The width of the divider gap before this slot (0 for the first slot).
    pub div_before: u16,
    pub collapsed: bool,
    pub rect: Rect,
}

/// Lay the footer buttons out across `area`, dropping and then collapsing by
/// priority so the row never overflows. Returns the kept slots (in order) with
/// their screen rects.
pub fn layout<A: FooterRole>(items: Vec<FooterItem<A>>, area: Rect) -> Vec<FooterSlot<A>> {
    // Total width of a kept subset, laid with the given collapse.
    let total_width = |kept: &[usize], collapsed: bool| -> u16 {
        let mut w = 0u16;
        for (pos, &i) in kept.iter().enumerate() {
            let strong = strong_before(&items, kept, pos);
            w = w.saturating_add(divider_width(pos, strong, collapsed));
            w = w.saturating_add(items[i].width(collapsed));
        }
        w
    };

    // Drop droppable items (lowest rank first) until the subset fits.
    let shrink = |collapsed: bool| -> Vec<usize> {
        let mut kept: Vec<usize> = (0..items.len()).collect();
        while total_width(&kept, collapsed) > area.width {
            let victim = kept
                .iter()
                .copied()
                .filter(|&i| items[i].drop_rank.is_some())
                .min_by_key(|&i| items[i].drop_rank.unwrap_or(u8::MAX));
            match victim {
                Some(v) => kept.retain(|&i| i != v),
                None => break,
            }
        }
        kept
    };

    // Prefer full labels; only when even the non-droppable core overflows do we
    // collapse to keys (re-adding items, since keys keep more of them).
    let (kept, collapsed) = {
        let labelled = shrink(false);
        if total_width(&labelled, false) <= area.width {
            (labelled, false)
        } else {
            (shrink(true), true)
        }
    };

    // Assign rects left to right.
    let mut out = Vec::with_capacity(kept.len());
    let mut x = area.x;
    for (pos, &i) in kept.iter().enumerate() {
        let strong = strong_before(&items, &kept, pos);
        let dw = divider_width(pos, strong, collapsed);
        x = x.saturating_add(dw);
        let w = items[i].width(collapsed);
        out.push(FooterSlot {
            item: items[i],
            index: i,
            strong_divider: strong,
            div_before: dw,
            collapsed,
            rect: Rect {
                x,
                y: area.y,
                width: w,
                height: 1,
            },
        });
        x = x.saturating_add(w);
    }
    out
}

/// Whether the divider before the kept item at `pos` is the stronger nav/mode
/// boundary: true for the first non-nav item that follows a nav item.
pub fn strong_before<A: FooterRole>(items: &[FooterItem<A>], kept: &[usize], pos: usize) -> bool {
    if pos == 0 {
        return false;
    }
    let cur = &items[kept[pos]];
    let prev = &items[kept[pos - 1]];
    prev.is_nav() && !cur.is_nav()
}
