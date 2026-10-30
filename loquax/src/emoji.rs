//! The emoji a picker offers, and the ones the reader used last.
//!
//! The table is data generated from Unicode's `emoji-test.txt` and CLDR's
//! names (`scripts/gen-emoji-table.py`, `loquax/emoji/emoji.tsv`): every
//! fully-qualified emoji in CLDR order by category, its English and German
//! short name for a search, and the skin tone variants of one that has
//! them. The emoji are text; the frontend's emoji font draws them. The
//! Android app ships the file as an asset and reads it in the same format;
//! [`Table::parse`] is the reading a Rust frontend shares.
//!
//! The recents are the reader's last used emoji, newest first, from the
//! reactions they sent and what they picked into a message. The picker
//! shows them as its first row; the reaction row takes them later (1137).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// The categories, in the picker's order: the table's `@` lines.
pub const CATEGORIES: [&str; 9] = [
    "smileys",
    "people",
    "animals",
    "food",
    "travel",
    "activities",
    "objects",
    "symbols",
    "flags",
];

/// One emoji of the table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Emoji {
    /// The emoji itself, fully qualified.
    pub text: String,
    /// Its CLDR short name in English.
    pub en: String,
    /// Its CLDR short name in German, the English one where CLDR has none.
    pub de: String,
    /// The same emoji in the five skin tones, light to dark; empty for one
    /// without.
    pub variants: Vec<String>,
}

/// The table: each category with its emoji.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    pub categories: Vec<(String, Vec<Emoji>)>,
}

impl Table {
    /// Read the table's text. `Err` names the first line that is neither a
    /// comment, a category nor an emoji with both names, or an emoji before
    /// any category.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut table = Self::default();
        for (n, line) in text.lines().enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('@') {
                table.categories.push((name.to_string(), Vec::new()));
                continue;
            }
            let mut cols = line.split('\t');
            let (Some(text), Some(en), Some(de)) = (cols.next(), cols.next(), cols.next()) else {
                return Err(alloc::format!("emoji table line {}: not an emoji", n + 1));
            };
            let variants = cols
                .next()
                .map(|v| v.split(' ').map(str::to_string).collect())
                .unwrap_or_default();
            let Some((_, emoji)) = table.categories.last_mut() else {
                return Err(alloc::format!(
                    "emoji table line {}: before a category",
                    n + 1
                ));
            };
            emoji.push(Emoji {
                text: text.to_string(),
                en: en.to_string(),
                de: de.to_string(),
                variants,
            });
        }
        Ok(table)
    }

    /// Every emoji whose English or German name contains each of the words
    /// of `query`, case folded, in the table's order.
    pub fn search(&self, query: &str) -> Vec<&Emoji> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Vec::new();
        }
        self.categories
            .iter()
            .flat_map(|(_, emoji)| emoji.iter())
            .filter(|e| {
                let names = alloc::format!("{} {}", e.en, e.de).to_lowercase();
                words.iter().all(|w| names.contains(w.as_str()))
            })
            .collect()
    }
}

/// How many emoji the recents keep: the picker's first row, twice.
pub const RECENTS: usize = 16;

/// The reader's last used emoji, newest first, each once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recents(pub Vec<String>);

impl Recents {
    /// `emoji` was used: first now, and the oldest beyond [`RECENTS`] goes.
    /// An empty one, or one with a line break, which the file could not
    /// hold, changes nothing.
    pub fn used(&mut self, emoji: &str) {
        if emoji.is_empty() || emoji.contains('\n') {
            return;
        }
        self.0.retain(|e| e != emoji);
        self.0.insert(0, emoji.to_string());
        self.0.truncate(RECENTS);
    }

    /// The file's text: one emoji a line.
    pub fn encode(&self) -> String {
        let mut out = String::new();
        for emoji in &self.0 {
            out.push_str(emoji);
            out.push('\n');
        }
        out
    }

    /// Read the file's text; what does not fit the rules is dropped.
    pub fn decode(text: &str) -> Self {
        let mut recents = Self::default();
        for emoji in text.lines().rev() {
            recents.used(emoji.trim());
        }
        recents
    }
}

/// The recents' file in a loquax home.
#[cfg(feature = "std")]
pub const RECENTS_FILE: &str = "emoji-recents";

/// The recents kept in `home`; none when the file is missing or unreadable.
#[cfg(feature = "std")]
pub fn load_recents(home: &std::path::Path) -> Recents {
    std::fs::read_to_string(home.join(RECENTS_FILE))
        .map(|text| Recents::decode(&text))
        .unwrap_or_default()
}

/// Keep `recents` in `home`.
#[cfg(feature = "std")]
pub fn save_recents(home: &std::path::Path, recents: &Recents) -> Result<(), String> {
    std::fs::write(home.join(RECENTS_FILE), recents.encode())
        .map_err(|error| alloc::format!("{RECENTS_FILE}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_segmentation::UnicodeSegmentation;

    const TABLE: &str = include_str!("../emoji/emoji.tsv");

    #[test]
    fn the_table_loads_with_every_category_in_order() {
        let table = Table::parse(TABLE).expect("table");
        let names: Vec<&str> = table.categories.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, CATEGORIES);
        assert!(table.categories.iter().all(|(_, e)| !e.is_empty()));
        let all: usize = table.categories.iter().map(|(_, e)| e.len()).sum();
        assert!(all > 1800, "{all} emoji");
    }

    /// One grapheme each, so a picker cell, a reaction and a cursor step
    /// are one emoji, variants as well; and every variant has its tone.
    #[test]
    fn every_entry_is_one_grapheme() {
        let table = Table::parse(TABLE).expect("table");
        for (_, emoji) in &table.categories {
            for e in emoji {
                assert_eq!(e.text.graphemes(true).count(), 1, "{} {}", e.text, e.en);
                assert!(!e.en.is_empty() && !e.de.is_empty(), "{}", e.text);
                assert!(e.variants.is_empty() || e.variants.len() == 5, "{}", e.en);
                for v in &e.variants {
                    assert_eq!(v.graphemes(true).count(), 1, "{v} of {}", e.en);
                    assert!(v.chars().any(|c| ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)));
                }
            }
        }
    }

    /// The quick reactions are in the table as written, with U+FE0F where
    /// they carry one, so a picked heart and a quick heart count together.
    #[test]
    fn the_quick_reactions_are_in_the_table_as_written() {
        let table = Table::parse(TABLE).expect("table");
        let all: Vec<&str> = table
            .categories
            .iter()
            .flat_map(|(_, e)| e.iter().map(|e| e.text.as_str()))
            .collect();
        for quick in crate::ui::reactions::QUICK
            .iter()
            .chain(crate::ui::reactions::SIGNAL_QUICK.iter())
        {
            assert!(all.contains(quick), "{quick:?}");
        }
    }

    #[test]
    fn a_search_finds_by_either_name() {
        let table = Table::parse(TABLE).expect("table");
        let thumbs = |q: &str| table.search(q).iter().any(|e| e.text == "\u{1F44D}");
        assert!(thumbs("thumbs up"));
        assert!(thumbs("Daumen HOCH"));
        assert!(!thumbs("Daumen runter"));
        assert!(table.search("  ").is_empty());
    }

    #[test]
    fn a_damaged_table_names_its_line() {
        assert!(Table::parse("@smileys\nx\n")
            .unwrap_err()
            .contains("line 2"));
        assert!(Table::parse("x\ty\tz\n")
            .unwrap_err()
            .contains("before a category"));
    }

    #[test]
    fn the_recents_keep_the_newest_first_and_each_once() {
        let mut recents = Recents::default();
        for e in ["a", "b", "a", "", "x\ny"] {
            recents.used(e);
        }
        assert_eq!(recents.0, ["a", "b"]);
        for i in 0..RECENTS + 4 {
            recents.used(&alloc::format!("{i}"));
        }
        assert_eq!(recents.0.len(), RECENTS);
        assert_eq!(recents.0[0], alloc::format!("{}", RECENTS + 3));
        assert_eq!(Recents::decode(&recents.encode()), recents);
    }

    #[cfg(feature = "std")]
    #[test]
    fn the_recents_survive_in_the_home() {
        let home = tempfile::tempdir().expect("tempdir");
        assert_eq!(load_recents(home.path()), Recents::default());
        let mut recents = Recents::default();
        recents.used("\u{2764}\u{FE0F}");
        save_recents(home.path(), &recents).expect("save");
        assert_eq!(load_recents(home.path()), recents);
    }
}
