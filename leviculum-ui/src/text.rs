//! Fitting text into a cell budget: the width a terminal paints, and
//! truncation with an ellipsis that never spills over a border.

use alloc::string::{String, ToString};

use unicode_width::UnicodeWidthChar;

/// The column width a terminal actually PAINTS for `s`, used to fit text into a
/// box. This is `unicode-width` PLUS one column for every `U+FE0F` VARIATION
/// SELECTOR-16: `unicode-width` scores VS16 as 0 and a text-default base (e.g.
/// `U+262F ☯`) as 1, but a terminal draws the resulting emoji-presentation
/// sequence 2 columns wide. Over-counting is safe when fitting — we truncate a
/// column early; under-counting overflows a border. ZWJ sequences are already
/// over-counted by `unicode-width`, which is likewise safe, so they are left be.
///
/// This is deliberately a DIFFERENT notion of width from the plain
/// `unicode-width` used for cell POSITIONS (lnomad's `visible_links`,
/// `visible_fields`, the search-match columns): those must keep agreeing with
/// ratatui's own `unicode-width` cell layout for hit-testing. `fit_width` measures what the
/// TERMINAL paints; the plain width measures what ratatui ADDRESSES.
pub fn fit_width(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0) + usize::from(c == '\u{FE0F}'))
        .sum()
}

/// Truncate `s` to at most `cols` display columns, ending with a `…` when
/// anything is cut. Uses [`fit_width`] so an emoji-presentation sequence counts
/// as the two columns a terminal paints, so the returned string never spills
/// over a `cols`-wide overlay border. A zero budget yields an empty string.
pub fn truncate_to_cols(s: &str, cols: usize) -> String {
    if cols == 0 {
        return String::new();
    }
    if fit_width(s) <= cols {
        return s.to_string();
    }
    // Reserve one column for the ellipsis, then take as many full characters as
    // fit. A wide char that would straddle the budget is dropped whole, so the
    // result never exceeds `cols`.
    let budget = cols - 1;
    let mut out = String::new();
    let mut width = 0usize;
    for c in s.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0) + usize::from(c == '\u{FE0F}');
        if width + w > budget {
            break;
        }
        out.push(c);
        width += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn fit_width_counts_vs16_presentation_as_wide() {
        assert_eq!(fit_width(""), 0);
        assert_eq!(fit_width("a"), 1);
        // Ordinary wide emoji: unicode-width already says 2.
        assert_eq!(fit_width("🌐"), 2);
        assert_eq!(fit_width("📰"), 2);
        // Emoji-presentation sequences (base + U+FE0F): unicode-width scores
        // these as 1, but a terminal paints them 2 wide.
        assert_eq!(fit_width("☯️"), 2);
        assert_eq!(fit_width("☸️"), 2);
        // The bare base char without VS16 keeps its text-default single column.
        assert_eq!(fit_width("☯"), 1);
    }

    #[test]
    fn truncate_to_cols_is_width_aware() {
        assert_eq!(truncate_to_cols("hello", 10), "hello");
        assert_eq!(truncate_to_cols("hello", 5), "hello");
        // Cut to a budget: the ellipsis takes the last column.
        assert_eq!(truncate_to_cols("hello world", 5), "hell…");
        assert_eq!(
            UnicodeWidthStr::width(truncate_to_cols("hello world", 5).as_str()),
            5
        );
        // A wide char that would straddle the budget is dropped whole, so the
        // result never exceeds the requested width.
        let cut = truncate_to_cols("🚀ab", 2);
        assert!(
            UnicodeWidthStr::width(cut.as_str()) <= 2,
            "over budget: {cut:?}"
        );
        assert_eq!(truncate_to_cols("anything", 0), "");
    }
}
