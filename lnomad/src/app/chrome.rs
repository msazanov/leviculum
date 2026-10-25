//! The browser chrome laid out in character cells: the top bar, the footer,
//! the help text. Shared by the frontend that draws it and by the click and
//! hover hit-tests in [`update`], which is why it lives in the core.

use unicode_width::UnicodeWidthStr;

use super::event::*;
use super::geometry::Rect;
use super::input::KeyCode;
use super::model::*;

/// The ` · ` separator drawn between the title and the address on the top bar.
pub(crate) const TOPBAR_SEP: &str = " · ";

/// The resolved geometry of the one-row top bar: the title and address (each
/// already ellipsized to fit) and the right-aligned status cluster (bookmark
/// star, cache bolt, hop count). The renderer and the click / hint hit-testing
/// both derive from this one function, so they can never disagree on where a
/// span sits.
pub struct TopBar {
    /// The title text (label only), ellipsized to fit; empty when none.
    pub title: String,
    /// The column the title starts at.
    pub title_x: u16,
    /// The `·` separator's column, present only when both title and address show.
    pub sep_x: Option<u16>,
    /// The address text, ellipsized to fit; empty when it did not fit at all.
    pub address: String,
    /// The address slot's rect; a click here opens the address editor.
    pub address_rect: Rect,
    /// The bookmark star: its glyph (★ / ☆) and rect, absent when no page is
    /// loaded. A click toggles the bookmark.
    pub star: Option<(&'static str, Rect)>,
    /// The identity marker (key glyph + fingerprint prefix) and its column,
    /// present only while identifying to the current node. Anonymous — the
    /// default — shows nothing: the visible state is the one with consequences.
    pub identity: Option<(String, u16)>,
    /// The cache-bolt glyph's column, present only for a cached view.
    pub cached_x: Option<u16>,
    /// The hop-count text (`N hops` / `unknown`) and its column.
    pub hops: Option<(String, u16)>,
}

/// The bookmark-star glyph for the current page: filled when bookmarked, hollow
/// when not, or `None` when no page is loaded (nothing to bookmark).
pub(crate) fn star_glyph(model: &Model) -> Option<&'static str> {
    let url = current_url(model)?;
    Some(if model.bookmarks.contains(&url) {
        STAR_ON
    } else {
        STAR_OFF
    })
}

/// The identity-marker cluster text: the key glyph plus the first 8 hex chars
/// of the session's own fingerprint, or `None` when not identifying to the
/// current node (or the fingerprint is unset, as in a bare test model).
pub(crate) fn identity_text(model: &Model) -> Option<String> {
    if !model.identifying || model.fingerprint.is_empty() {
        return None;
    }
    let fp8: String = model.fingerprint.chars().take(8).collect();
    Some(format!("{IDENTIFY_GLYPH} {fp8}"))
}

/// The hop-count cluster text for the current destination: `N hops` when the
/// destination is a known announced node with a hop count, `unknown` when it is
/// not in the registry, or `None` when no page is loaded.
pub(crate) fn hops_text(model: &Model) -> Option<String> {
    let dest = model.current_dest?;
    Some(
        match model.node_registry.get_by_hash(&dest).and_then(|n| n.hops) {
            Some(h) => format!("{h} hops"),
            None => "unknown".to_string(),
        },
    )
}

pub(crate) fn topbar_layout(model: &Model, area: Rect) -> TopBar {
    let y = area.y;
    let right = area.right();

    // Right-aligned status cluster: star, identity marker, cache bolt, hop
    // count, each present only when it applies, separated by a single column.
    let star = star_glyph(model);
    let identity = identity_text(model);
    let mut cached = model.cached_view;
    let mut hops = hops_text(model);
    let seg_w = |g: &str| UnicodeWidthStr::width(g) as u16;
    let cluster_width = |cached: bool, hops: &Option<String>| -> u16 {
        let mut w = 0u16;
        let mut segs = 0u16;
        if let Some(g) = star {
            w += seg_w(g);
            segs += 1;
        }
        if let Some(t) = &identity {
            w += seg_w(t);
            segs += 1;
        }
        if cached {
            w += seg_w(CACHED_GLYPH);
            segs += 1;
        }
        if let Some(h) = hops {
            w += seg_w(h);
            segs += 1;
        }
        w + segs.saturating_sub(1) // one blank column between segments
    };
    // When even the bare cluster cannot fit the bar, drop the least important
    // segments: the hop count first, then the cache bolt. The star and the
    // identity marker — the security-relevant state — always survive.
    if cluster_width(cached, &hops) > area.width && hops.is_some() {
        hops = None;
    }
    if cluster_width(cached, &hops) > area.width {
        cached = false;
    }
    let cluster_w = cluster_width(cached, &hops);
    let cluster_x = right.saturating_sub(cluster_w).max(area.x);

    // Place each cluster segment left to right from cluster_x.
    let mut cx = cluster_x;
    let star = star.map(|g| {
        let w = seg_w(g);
        let rect = Rect {
            x: cx,
            y,
            width: w,
            height: 1,
        };
        cx = cx.saturating_add(w).saturating_add(1);
        (g, rect)
    });
    let identity = identity.map(|t| {
        let x = cx;
        cx = cx.saturating_add(seg_w(&t)).saturating_add(1);
        (t, x)
    });
    let cached_x = if cached {
        let x = cx;
        cx = cx.saturating_add(seg_w(CACHED_GLYPH)).saturating_add(1);
        Some(x)
    } else {
        None
    };
    let hops = hops.map(|h| {
        let x = cx;
        (h, x)
    });

    // Left identity: title, separator, address. The cluster keeps its width; the
    // title comes next; the address ellipsizes; and, only when even the title
    // plus separator will not fit, the title ellipsizes too.
    let left_end = if cluster_w > 0 {
        cluster_x.saturating_sub(CTRL_GAP)
    } else {
        right
    };
    let left_w = left_end.saturating_sub(area.x);
    let title_full = model.title.trim().to_string();
    let addr_full = breadcrumb(model);
    let sep_w = UnicodeWidthStr::width(TOPBAR_SEP) as u16;
    let tw = UnicodeWidthStr::width(title_full.as_str()) as u16;

    // Decide the title and how much width the address slot gets. The cluster
    // already has its width; the title comes next; the address slot takes the
    // rest and ellipsizes; and only when even the title plus separator plus one
    // address column will not fit does the title ellipsize and the address drop.
    let (title, has_sep, addr_avail) = if title_full.is_empty() {
        (String::new(), false, left_w)
    } else if tw + sep_w + 1 > left_w {
        (truncate_to_cols(&title_full, left_w as usize), false, 0)
    } else {
        (title_full, true, left_w - tw - sep_w)
    };

    let title_x = area.x;
    let title_w = UnicodeWidthStr::width(title.as_str()) as u16;
    let (sep_x, addr_x) = if has_sep {
        (Some(area.x + title_w), area.x + title_w + sep_w)
    } else if title.is_empty() {
        (None, area.x)
    } else {
        (None, area.x + title_w)
    };
    // The address text ellipsizes to the slot; the slot itself is the full
    // remaining width so a click anywhere in it, and the live editor, both land.
    let address = truncate_to_cols(&addr_full, addr_avail as usize);
    let address_rect = Rect {
        x: addr_x,
        y,
        width: addr_avail,
        height: 1,
    };

    TopBar {
        title,
        title_x,
        sep_x,
        address,
        address_rect,
        star,
        identity,
        cached_x,
        hops,
    }
}

/// The clickable top-bar controls (the address slot and, when a page is loaded,
/// the bookmark star), derived from [`topbar_layout`] so clicks and hint labels
/// hit-test against exactly what the renderer draws.
pub(crate) fn top_bar_controls(model: &Model, area: Rect) -> Vec<ControlRect> {
    let tb = topbar_layout(model, area);
    let mut out = vec![ControlRect {
        control: Control::Address,
        rect: tb.address_rect,
    }];
    if let Some((_, rect)) = tb.star {
        out.push(ControlRect {
            control: Control::Bookmark,
            rect,
        });
    }
    out
}

/// The address-slot breadcrumb for the current page, or a hint when nothing is
/// loaded yet.
pub(crate) fn breadcrumb(model: &Model) -> String {
    match model.history.current() {
        Some(target) => format!("{}:{}", short_hex(&target.dest_hash), target.path),
        None => "press : to enter a URL".to_string(),
    }
}

/// A short, glanceable form of a destination hash: the first 8 hex characters
/// followed by an ellipsis.
pub(crate) fn short_hex(dest: &[u8; 16]) -> String {
    let mut s = String::with_capacity(9);
    for byte in &dest[..4] {
        s.push_str(&format!("{byte:02x}"));
    }
    s.push('…');
    s
}

/// What a footer button activates when pressed or clicked. Mode-specific keys
/// (`Esc`, `Tab`, `Space`, `Enter`, `n`, `N`) route back through the key handler
/// so the click behaves exactly like the key press it mirrors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FooterAction {
    Back,
    Forward,
    Reload,
    /// Toggle identifying to the current page's node (same as the `i` key).
    Identify,
    Hint,
    Search,
    Places,
    Mark,
    Copy,
    Help,
    Quit,
    /// Synthesize a plain key press (the mode-action buttons).
    Key(KeyCode),
    /// A non-interactive sentence (hint mode's instruction).
    None,
}

/// One footer button: a key you can press and a label, which are the same thing
/// you can also click. The key cell renders bright + bold, the label muted.
#[derive(Clone, Copy, Debug)]
pub struct FooterItem {
    /// The pressable key (bright + bold), e.g. `Alt-←`, `f`, `Esc`.
    pub key: &'static str,
    /// The muted descriptive label, e.g. `back`, `hint` (empty for a sentence).
    pub label: &'static str,
    /// Whether the key renders before the label. Everything leads with its key
    /// (`Alt-← back`, `f hint`).
    pub key_first: bool,
    /// What a press or click runs.
    pub action: FooterAction,
    /// Truncation priority: `None` never drops; `Some(rank)` drops when the row
    /// is too narrow, lowest rank first.
    pub drop_rank: Option<u8>,
    /// Whether the control is currently usable; an unavailable nav item reads
    /// muted rather than active.
    pub available: bool,
}

impl FooterItem {
    /// A never-dropped, always-available mode-action button.
    pub(crate) const fn action(key: &'static str, label: &'static str, act: FooterAction) -> Self {
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
    pub(crate) const fn droppable(
        key: &'static str,
        label: &'static str,
        act: FooterAction,
        r: u8,
    ) -> Self {
        FooterItem {
            drop_rank: Some(r),
            ..FooterItem::action(key, label, act)
        }
    }

    /// This item's rendered width, with labels (`false`) or collapsed to the key
    /// glyph only (`true`).
    pub(crate) fn width(&self, collapsed: bool) -> u16 {
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

    /// Whether this item is part of the navigation trio (rendered before the
    /// stronger divider).
    pub(crate) fn is_nav(&self) -> bool {
        matches!(
            self.action,
            FooterAction::Back | FooterAction::Forward | FooterAction::Reload
        )
    }
}

/// The footer's navigation trio, always first: back, forward, reload. They are
/// ordinary `key + label` items like the rest of the strip, spelling their keys
/// (`Alt-←`, `Alt-→`, `R`) rather than using glyphs. Their availability tracks
/// the history so an impossible motion reads muted.
pub(crate) fn footer_nav(model: &Model) -> [FooterItem; 3] {
    [
        FooterItem {
            available: model.history.can_back(),
            ..FooterItem::action("Alt-←", "back", FooterAction::Back)
        },
        FooterItem {
            available: model.history.can_forward(),
            ..FooterItem::action("Alt-→", "forward", FooterAction::Forward)
        },
        FooterItem {
            available: model.history.current().is_some(),
            ..FooterItem::action("R", "reload", FooterAction::Reload)
        },
    ]
}

/// The footer button list for the current mode: the nav trio, then the
/// mode-dependent action set. The first non-nav item carries the stronger
/// divider before it.
pub(crate) fn footer_items(model: &Model) -> Vec<FooterItem> {
    let mut items: Vec<FooterItem> = footer_nav(model).to_vec();
    match model.mode {
        Mode::Browse => {
            // The identify toggle, only when there is a page to identify to. A
            // state action, so under truncation it outlives the hint/search/
            // places/mark/copy conveniences (highest drop rank, right below the
            // never-dropped reload).
            if model.current_dest.is_some() {
                let label = if model.identifying {
                    "anonymous"
                } else {
                    "identify"
                };
                items.push(FooterItem::droppable("i", label, FooterAction::Identify, 6));
            }
            items.extend_from_slice(&[
                FooterItem::droppable("f", "hint", FooterAction::Hint, 5),
                FooterItem::droppable("/", "search", FooterAction::Search, 4),
                FooterItem::droppable("d", "places", FooterAction::Places, 3),
                FooterItem::droppable("m", "mark", FooterAction::Mark, 2),
                FooterItem::droppable("y", "copy", FooterAction::Copy, 1),
                FooterItem::action("?", "help", FooterAction::Help),
                FooterItem::action("q", "quit", FooterAction::Quit),
            ]);
        }
        Mode::Field => items.extend_from_slice(&[
            FooterItem::action("Esc", "done", FooterAction::Key(KeyCode::Esc)),
            FooterItem::droppable("Tab", "next field", FooterAction::Key(KeyCode::Tab), 1),
            FooterItem::droppable("Space", "toggle", FooterAction::Key(KeyCode::Char(' ')), 2),
            FooterItem::action("?", "help", FooterAction::Help),
        ]),
        Mode::Search => items.extend_from_slice(&[
            FooterItem::action("Enter", "go", FooterAction::Key(KeyCode::Enter)),
            FooterItem::droppable("n", "next", FooterAction::Key(KeyCode::Char('n')), 2),
            FooterItem::droppable("N", "prev", FooterAction::Key(KeyCode::Char('N')), 3),
            FooterItem::action("Esc", "cancel", FooterAction::Key(KeyCode::Esc)),
            FooterItem::droppable("?", "help", FooterAction::Help, 1),
        ]),
        Mode::Address => items.extend_from_slice(&[
            FooterItem::action("Enter", "go", FooterAction::Key(KeyCode::Enter)),
            FooterItem::action("Esc", "cancel", FooterAction::Key(KeyCode::Esc)),
            FooterItem::droppable("?", "help", FooterAction::Help, 1),
        ]),
        Mode::Hint => items.extend_from_slice(&[
            FooterItem {
                key: "",
                label: HINT_SENTENCE,
                key_first: true,
                action: FooterAction::None,
                drop_rank: None,
                available: true,
            },
            FooterItem::action("Esc", "cancel", FooterAction::Key(KeyCode::Esc)),
        ]),
    }
    items
}

/// The width of the divider drawn before the item at `kept`-position `pos`: a
/// ` │ ` between items (a single space in collapsed mode, except at the stronger
/// nav/mode boundary). Position 0 has no leading divider.
pub(crate) fn footer_divider_width(pos: usize, is_strong: bool, collapsed: bool) -> u16 {
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
pub struct FooterSlot {
    pub item: FooterItem,
    /// Index into `footer_items` for the current mode (stable per mode), so a
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
pub(crate) fn footer_layout(model: &Model, area: Rect) -> Vec<FooterSlot> {
    let items = footer_items(model);

    // Total width of a kept subset, laid with the given collapse.
    let total_width = |kept: &[usize], collapsed: bool| -> u16 {
        let mut w = 0u16;
        for (pos, &i) in kept.iter().enumerate() {
            let strong = footer_strong_before(&items, kept, pos);
            w = w.saturating_add(footer_divider_width(pos, strong, collapsed));
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
        let strong = footer_strong_before(&items, &kept, pos);
        let dw = footer_divider_width(pos, strong, collapsed);
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
pub(crate) fn footer_strong_before(items: &[FooterItem], kept: &[usize], pos: usize) -> bool {
    if pos == 0 {
        return false;
    }
    let cur = &items[kept[pos]];
    let prev = &items[kept[pos - 1]];
    prev.is_nav() && !cur.is_nav()
}

/// Whether the footer currently shows its button strip. It now always does,
/// except in search mode where the status bar becomes the `/<query>` editor: the
/// loading spinner and a focused/hovered link's target float in the bottom-left
/// [`render_status_chip`] over the content instead of covering the buttons.
pub(crate) fn footer_shows_buttons(model: &Model) -> bool {
    model.mode != Mode::Search
}

/// The status-bar text for the focused-or-hovered link's target, if any: the
/// current node's short hash and the link's path (`node · :/page/x.mu`).
pub fn focused_link_target(model: &Model) -> Option<String> {
    let idx = model.focus.or(model.hover)?;
    let link = model.links.iter().find(|l| l.index == idx)?;
    // A same-destination link's target begins with ':'; prefix the current node
    // so the reader sees which node it resolves against.
    match model.current_dest {
        Some(dest) if link.target.starts_with(':') => {
            Some(format!("{} · {}", short_hex(&dest), link.target))
        }
        _ => Some(link.target.clone()),
    }
}

/// One keybinding shown in the help overlay: the keys and their description. The
/// description column is COMPUTED from the widest `keys` across all groups (see
/// [`help_lines`]), never hand-padded, so it can never misalign.
pub(crate) struct HelpEntry {
    pub(crate) keys: &'static str,
    pub(crate) desc: &'static str,
}

/// A titled group of [`HelpEntry`]s in the help overlay.
pub(crate) struct HelpGroup {
    pub(crate) title: &'static str,
    pub(crate) entries: Vec<HelpEntry>,
}

/// The indent applied to every entry (its keys and description) under a group.
pub(crate) const HELP_INDENT: &str = "  ";
/// The gap between the (padded) keys column and the description column.
pub(crate) const HELP_GAP: usize = 2;

/// The keybinding groups shown in the help overlay. The Move group's scroll rows
/// are pulled straight from [`SCROLL_KEYS`], so the overlay can never list a
/// scroll binding that the key handler does not honour, nor omit one it does.
pub(crate) fn help_groups() -> Vec<HelpGroup> {
    // The scroll rows come straight from the keymap table; the mouse wheel is the
    // one Move affordance that is not a key chord, so it is appended here.
    let mut move_entries: Vec<HelpEntry> = SCROLL_KEYS
        .iter()
        .map(|s| HelpEntry {
            keys: s.keys,
            desc: s.desc,
        })
        .collect();
    move_entries.push(HelpEntry {
        keys: "wheel",
        desc: "scroll",
    });
    vec![
        HelpGroup {
            title: "Move",
            entries: move_entries,
        },
        HelpGroup {
            title: "Follow",
            entries: vec![
                HelpEntry {
                    keys: "Tab / Shift-Tab",
                    desc: "focus a link or field",
                },
                HelpEntry {
                    keys: "Enter / click",
                    desc: "follow a link or footer button",
                },
                HelpEntry {
                    keys: "f",
                    desc: "hint — type a label or the link text",
                },
                HelpEntry {
                    keys: "Alt-← / Alt-→   mouse side buttons",
                    desc: "back / forward",
                },
                HelpEntry {
                    keys: "hover a link",
                    desc: "show its URL",
                },
            ],
        },
        HelpGroup {
            title: "Page",
            entries: vec![
                HelpEntry {
                    keys: ": / click the URL",
                    desc: "enter a URL",
                },
                HelpEntry {
                    keys: "R / Ctrl-R / F5",
                    desc: "reload (always refetches)",
                },
                HelpEntry {
                    keys: "Esc / Ctrl-g",
                    desc: "cancel a load",
                },
                HelpEntry {
                    keys: "m / click star",
                    desc: "bookmark this page",
                },
                HelpEntry {
                    keys: "y",
                    desc: "copy the link's / page's URL",
                },
                HelpEntry {
                    keys: "d / click a place",
                    desc: "places panel — click an entry to open it",
                },
                HelpEntry {
                    keys: "i",
                    desc: "identify / go anonymous to this node",
                },
                HelpEntry {
                    keys: "",
                    desc: "default anonymous; identifying lets this node attribute your posts",
                },
                HelpEntry {
                    keys: "/ n N",
                    desc: "search / next / prev",
                },
            ],
        },
        HelpGroup {
            title: "Images",
            entries: vec![
                HelpEntry {
                    keys: "Enter",
                    desc: "save the focused picture (no second transfer)",
                },
                HelpEntry {
                    keys: "o",
                    desc: "open it in the system viewer",
                },
                HelpEntry {
                    keys: "",
                    desc: "start with --images off to leave them as plain links",
                },
            ],
        },
        HelpGroup {
            title: "Fields",
            entries: vec![HelpEntry {
                keys: "type / Space / Esc",
                desc: "edit / toggle / leave",
            }],
        },
        HelpGroup {
            title: "App",
            entries: vec![
                HelpEntry {
                    keys: "t",
                    desc: "light / dark theme",
                },
                HelpEntry {
                    keys: "? / Esc / click",
                    desc: "close this help",
                },
                HelpEntry {
                    keys: "q / Ctrl-c",
                    desc: "quit",
                },
            ],
        },
    ]
}

/// The widest `keys` display width across every entry of every group: the width
/// the keys column is padded to so all descriptions align.
pub(crate) fn help_key_col(groups: &[HelpGroup]) -> usize {
    groups
        .iter()
        .flat_map(|g| g.entries.iter())
        .map(|e| UnicodeWidthStr::width(e.keys))
        .max()
        .unwrap_or(0)
}

/// The plain-text lines of the help overlay: each group's title, then its
/// entries with every description aligned to the computed keys column, with a
/// blank line between groups. The renderer and the layout both derive from this,
/// so widths can never disagree and no padding is hand-counted. The block's own
/// border title carries the heading, so no heading line lives in the body.
pub(crate) fn help_lines() -> Vec<String> {
    let groups = help_groups();
    let col = help_key_col(&groups);
    let mut lines = Vec::new();
    for (gi, group) in groups.iter().enumerate() {
        if gi > 0 {
            lines.push(String::new());
        }
        lines.push(group.title.to_string());
        for e in &group.entries {
            let pad = col.saturating_sub(UnicodeWidthStr::width(e.keys)) + HELP_GAP;
            lines.push(format!(
                "{HELP_INDENT}{}{}{}",
                e.keys,
                " ".repeat(pad),
                e.desc
            ));
        }
    }
    lines
}

/// The help overlay's scroll geometry for the current terminal size: the number
/// of help lines that fit at once (the viewport) and the largest valid
/// [`Model::help_scroll`] (`max`). Shared by the key/wheel handler (to clamp the
/// offset via [`scrolled`]) and the renderer (to slice the visible lines), so
/// the two can never disagree on the bounds. Mirrors `render_help`'s sizing: the
/// overlay is at most the full terminal height, two rows go to the border.
pub(crate) fn help_scroll_geometry(model: &Model) -> (usize, usize) {
    let total = help_lines().len();
    let rows = model.size.1 as usize;
    let height = (total + 2).min(rows);
    let viewport = height.saturating_sub(2);
    (viewport, total.saturating_sub(viewport))
}
