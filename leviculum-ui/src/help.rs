//! The help overlay's model: titled groups of keybindings, and their plain-text
//! projection with every description aligned to one computed column.
//!
//! A program supplies its groups; `leviculum-ui-ratatui` draws them. Both the
//! drawing and the overlay's sizing derive from [`lines`], so they can never
//! disagree and no padding is hand-counted.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use unicode_width::UnicodeWidthStr;

/// One keybinding shown in the help overlay: the keys and their description. The
/// description column is COMPUTED from the widest `keys` across all groups (see
/// [`lines`]), never hand-padded, so it can never misalign.
pub struct HelpEntry {
    /// The keys, as the reader presses them (`Tab / S-Tab`).
    pub keys: &'static str,
    /// What they do.
    pub desc: &'static str,
}

/// A titled group of [`HelpEntry`]s in the help overlay.
pub struct HelpGroup {
    /// The group's title line.
    pub title: &'static str,
    /// Its bindings, in display order.
    pub entries: Vec<HelpEntry>,
}

/// The indent applied to every entry (its keys and description) under a group.
pub const HELP_INDENT: &str = "  ";
/// The gap between the (padded) keys column and the description column.
pub const HELP_GAP: usize = 2;

/// The widest `keys` display width across every entry of every group: the width
/// the keys column is padded to so all descriptions align.
pub fn key_col(groups: &[HelpGroup]) -> usize {
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
pub fn lines(groups: &[HelpGroup]) -> Vec<String> {
    let col = key_col(groups);
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
