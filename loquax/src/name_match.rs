//! Which contacts of the phone's address book a peer's announced name may
//! be (1139; `docs/drafts/loquax-android-contacts-2026-10.md`, section 3,
//! "Matching the announced name"). A pure function over the name fields
//! the app hands it, so the desktop gets the same ranking and the table is
//! tested without a phone. It only ranks: nothing is linked without the
//! reader's tap.
//!
//! # Folding
//!
//! Both sides alike: lower case; a letter with an accent as the letter
//! without it; `ä ö ü` both as `a o u` and as `ae oe ue`, so `Müller`
//! finds `Muller` and `Mueller` alike; `ß` as `ss`; punctuation as space;
//! then the look-alike fold of [`crate::names::skeleton`], so a Cyrillic
//! `Аlice` ranks as `Alice`. The study names Unicode normalisation (NFKD);
//! this crate carries no normalisation table, so the accents of the Latin
//! scripts are folded by a table of their own and a combining mark after a
//! letter is dropped, `u` and U+0308 being `ü`.
//!
//! # Score
//!
//! | Match | Score |
//! |---|---|
//! | the whole announced name is the display name or a nickname | 100 |
//! | its words are the given and the family name | 90 |
//! | one word, a nickname | 85 |
//! | one word, the given name | 60 |
//! | every word but a call sign is among the contact's words (`Lew DL1ABC`, `Lew Palm`) | 50 |
//! | a prefix of three or more letters of the given name, or an initial and the family name | 30 |
//!
//! A word with letters and digits both is a call sign. At most
//! [`SHOWN`] candidates of [`FLOOR`] or more, highest first, ties by the
//! contact the reader contacted most recently.

use alloc::string::String;
use alloc::vec::Vec;

use crate::names::skeleton;

/// The most suggestions shown.
pub const SHOWN: usize = 5;
/// The lowest score shown.
pub const FLOOR: u8 = 30;

/// One contact's name fields, as the app read them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Candidate {
    /// The app's handle on the contact, handed back with the suggestion.
    pub key: String,
    pub display: String,
    pub given: String,
    pub middle: String,
    pub family: String,
    pub nicknames: Vec<String>,
    /// When the reader last contacted it, any clock; 0 for never.
    pub last_contacted: i64,
}

/// Which field matched, for the sheet's "nickname Ali".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    Display,
    Nickname(String),
    Name,
    Given,
    Words,
    Prefix,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub key: String,
    pub score: u8,
    pub field: Field,
}

/// The candidates `announced` may be, best first.
pub fn rank(announced: &str, candidates: &[Candidate]) -> Vec<Suggestion> {
    let mut found: Vec<(Suggestion, i64)> = candidates
        .iter()
        .filter_map(|c| score(announced, c).map(|s| (s, c.last_contacted)))
        .filter(|(s, _)| s.score >= FLOOR)
        .collect();
    found.sort_by(|(a, at), (b, bt)| b.score.cmp(&a.score).then(bt.cmp(at)));
    found.truncate(SHOWN);
    found.into_iter().map(|(s, _)| s).collect()
}

/// The best rule `candidate` meets for `announced`, if any.
pub fn score(announced: &str, candidate: &Candidate) -> Option<Suggestion> {
    let whole = forms(announced);
    if whole[0].is_empty() {
        return None;
    }
    let words = words_of(announced);
    let hit = |score, field| {
        Some(Suggestion {
            key: candidate.key.clone(),
            score,
            field,
        })
    };
    if same(&whole, &forms(&candidate.display)) {
        return hit(100, Field::Display);
    }
    if let Some(nick) = candidate
        .nicknames
        .iter()
        .find(|nick| same(&whole, &forms(nick)))
    {
        return hit(100, Field::Nickname(nick.clone()));
    }
    let mut named = words_of(&candidate.given);
    named.extend(words_of(&candidate.family));
    if words.len() >= 2 && same_set(&words, &named) {
        return hit(90, Field::Name);
    }
    // One word: the nickname rule above already took a whole match, so a
    // nickname here is one of several words of a nickname.
    if words.len() == 1 {
        if let Some(nick) = candidate
            .nicknames
            .iter()
            .find(|nick| words_of(nick).iter().any(|w| same(w, &words[0])))
        {
            return hit(85, Field::Nickname(nick.clone()));
        }
        let given = words_of(&candidate.given);
        if given.len() == 1 && same(&given[0], &words[0]) {
            return hit(60, Field::Given);
        }
    }
    let theirs = all_words(candidate);
    let calls = call_signs(announced);
    let plain: Vec<&[String; 2]> = words
        .iter()
        .zip(calls.iter().chain(core::iter::repeat(&false)))
        .filter(|(_, call)| !**call)
        .map(|(w, _)| w)
        .collect();
    if words.len() >= 2
        && !plain.is_empty()
        && plain.iter().all(|w| theirs.iter().any(|t| same(w, t)))
    {
        return hit(50, Field::Words);
    }
    if prefix_of_given(&words, candidate) || initial_and_family(&words, candidate) {
        return hit(30, Field::Prefix);
    }
    None
}

/// Every word of the contact's fields.
fn all_words(candidate: &Candidate) -> Vec<[String; 2]> {
    let mut out = words_of(&candidate.display);
    for field in [&candidate.given, &candidate.middle, &candidate.family] {
        out.extend(words_of(field));
    }
    for nick in &candidate.nicknames {
        out.extend(words_of(nick));
    }
    out
}

/// One word of three letters or more that begins the given name.
fn prefix_of_given(words: &[[String; 2]], candidate: &Candidate) -> bool {
    let [word] = words else {
        return false;
    };
    let given = forms(&candidate.given);
    word[0].chars().count() >= 3
        && word
            .iter()
            .any(|w| given.iter().any(|g| g.starts_with(w.as_str()) && g != w))
}

/// Two words: an initial of the given name and the family name, either
/// order (`A Schmidt`, `Schmidt A`).
fn initial_and_family(words: &[[String; 2]], candidate: &Candidate) -> bool {
    let [a, b] = words else {
        return false;
    };
    let given = forms(&candidate.given);
    let family = forms(&candidate.family);
    let Some(first) = given[0].chars().next() else {
        return false;
    };
    let initial = |w: &[String; 2]| w[0].chars().count() == 1 && w[0].starts_with(first);
    (initial(a) && same(b, &family)) || (initial(b) && same(a, &family))
}

/// For each word of `name`, as [`forms`] splits it, whether it holds
/// letters and digits both: a call sign, which no address book name holds.
/// Read before the look-alike fold, which takes `1` for `l`.
fn call_signs(name: &str) -> Vec<bool> {
    let [plain, _] = folded(name);
    plain
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(|w| w.chars().any(|c| c.is_ascii_digit()) && w.chars().any(char::is_alphabetic))
        .collect()
}

/// Whether two names' forms meet.
fn same(a: &[String; 2], b: &[String; 2]) -> bool {
    !a[0].is_empty() && a.iter().any(|x| b.iter().any(|y| x == y))
}

/// Whether two word lists hold the same words, each matched once.
fn same_set(a: &[[String; 2]], b: &[[String; 2]]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut left: Vec<&[String; 2]> = b.iter().collect();
    for word in a {
        match left.iter().position(|other| same(word, other)) {
            Some(at) => {
                left.swap_remove(at);
            }
            None => return false,
        }
    }
    true
}

/// The words of a name, each folded both ways.
fn words_of(name: &str) -> Vec<[String; 2]> {
    let [plain, spelled] = forms(name);
    plain
        .split(' ')
        .zip(spelled.split(' '))
        .filter(|(p, _)| !p.is_empty())
        .map(|(p, s)| [String::from(p), String::from(s)])
        .collect()
}

/// A name folded the two ways: the umlauts as `a o u`, and as `ae oe ue`.
/// The words line up in both, one space between them.
pub fn forms(name: &str) -> [String; 2] {
    folded(name).map(|form| skeleton(&form))
}

/// [`forms`] before the look-alike fold.
fn folded(name: &str) -> [String; 2] {
    let mut plain = String::new();
    let mut spelled = String::new();
    let mut chars = name.chars().flat_map(char::to_lowercase).peekable();
    while let Some(mut ch) = chars.next() {
        // A letter and a combining diaeresis are the umlaut.
        if chars.peek() == Some(&'\u{308}') && matches!(ch, 'a' | 'o' | 'u') {
            chars.next();
            ch = match ch {
                'a' => 'ä',
                'o' => 'ö',
                _ => 'ü',
            };
        }
        if ('\u{300}'..='\u{36f}').contains(&ch) {
            continue;
        }
        let (p, s): (&str, &str) = match ch {
            'ä' => ("a", "ae"),
            'ö' => ("o", "oe"),
            'ü' => ("u", "ue"),
            'ß' => ("ss", "ss"),
            'æ' => ("ae", "ae"),
            'œ' => ("oe", "oe"),
            'þ' => ("th", "th"),
            _ if ch.is_alphanumeric() => {
                let base = base(ch);
                plain.push(base);
                spelled.push(base);
                continue;
            }
            _ => (" ", " "),
        };
        plain.push_str(p);
        spelled.push_str(s);
    }
    [tidy(&plain), tidy(&spelled)]
}

/// One space between words, none around them.
fn tidy(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    words.join(" ")
}

/// A Latin letter with an accent as the letter without it.
fn base(ch: char) -> char {
    match ch {
        'à' | 'á' | 'â' | 'ã' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' | 'đ' | 'ð' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' | 'ħ' => 'h',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'ĵ' => 'j',
        'ķ' => 'k',
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => 's',
        'ţ' | 'ť' | 'ŧ' | 'ț' => 't',
        'ù' | 'ú' | 'û' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ŵ' => 'w',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn contact(key: &str, display: &str, given: &str, family: &str) -> Candidate {
        Candidate {
            key: key.to_string(),
            display: display.to_string(),
            given: given.to_string(),
            family: family.to_string(),
            ..Candidate::default()
        }
    }

    fn scored(announced: &str, candidate: &Candidate) -> u8 {
        score(announced, candidate).map_or(0, |s| s.score)
    }

    /// The study's table, row by row, with a case that must not match
    /// beside each.
    #[test]
    fn the_ranking_table() {
        let alice = Candidate {
            nicknames: vec!["Ali".to_string()],
            ..contact("1", "Alice Schmidt", "Alice", "Schmidt")
        };
        // 100: the display name, or a whole nickname.
        assert_eq!(scored("Alice Schmidt", &alice), 100);
        assert_eq!(scored("alice  schmidt!", &alice), 100);
        assert_eq!(scored("Ali", &alice), 100);
        assert_eq!(
            score("Ali", &alice).map(|s| s.field),
            Some(Field::Nickname("Ali".to_string()))
        );
        // 90: given and family, in either order.
        let reversed = contact("2", "Dr. A. Schmidt", "Alice", "Schmidt");
        assert_eq!(scored("Schmidt Alice", &reversed), 90);
        assert_eq!(scored("Alice Schmidt", &reversed), 90);
        // 85: one word of a nickname of several.
        let field = Candidate {
            nicknames: vec!["Funker Lew".to_string()],
            ..contact("3", "Lew Palm", "Lew", "Palm")
        };
        assert_eq!(scored("Funker", &field), 85);
        // 60: one word, the given name.
        assert_eq!(scored("Alice", &alice), 60);
        assert_eq!(scored("Schmidt", &alice), 0, "the family name alone");
        // 50: the words but a call sign among the contact's.
        assert_eq!(scored("Lew DL1ABC", &field), 50);
        assert_eq!(scored("Bob DL1ABC", &field), 0);
        // 30: a prefix of the given name, or an initial and the family name.
        assert_eq!(scored("Alic", &alice), 30);
        assert_eq!(scored("Al", &alice), 0, "two letters are no prefix");
        assert_eq!(scored("A Schmidt", &alice), 30);
        assert_eq!(scored("A. Schmidt", &alice), 30);
        assert_eq!(scored("B Schmidt", &alice), 0);
        // Nothing for a name that is none of these.
        assert_eq!(scored("Mallory", &alice), 0);
        assert_eq!(scored("", &alice), 0);
        assert_eq!(scored("…", &alice), 0);
    }

    /// Umlauts both ways, accents and ß folded; a Cyrillic look-alike is
    /// the Latin letter.
    #[test]
    fn umlauts_accents_and_look_alikes_fold() {
        let mueller = contact("1", "Jörg Müller", "Jörg", "Müller");
        assert_eq!(scored("Joerg Mueller", &mueller), 100);
        assert_eq!(scored("Jorg Muller", &mueller), 100);
        assert_eq!(scored("JÖRG MÜLLER", &mueller), 100);
        // Decomposed: o and u with a combining diaeresis.
        assert_eq!(scored("Jo\u{308}rg Mu\u{308}ller", &mueller), 100);
        let written = contact("2", "Joerg Mueller", "Joerg", "Mueller");
        assert_eq!(scored("Jörg Müller", &written), 100);
        // Muller and Mueller are two spellings nobody folds into another.
        let plain = contact("3", "Muller", "", "");
        assert_eq!(scored("Mueller", &plain), 0);
        assert_eq!(scored("Müller", &plain), 100);
        let strasse = contact("4", "Strasse", "", "");
        assert_eq!(scored("Straße", &strasse), 100);
        let rene = contact("5", "René Čapek", "René", "Čapek");
        assert_eq!(scored("Rene Capek", &rene), 100);
        let alice = contact("6", "Alice", "Alice", "");
        assert_eq!(scored("\u{410}lice", &alice), 100);
    }

    /// Best first, ties by the most recent contact, five at most, nothing
    /// under the floor.
    #[test]
    fn the_ranking_orders_cuts_and_floors() {
        let mut candidates = vec![
            contact("prefix", "Alicia Keys", "Alicia", "Keys"),
            contact("given-old", "Alice Old", "Alice", "Old"),
            contact("display", "Alice", "", ""),
            contact("none", "Bob", "Bob", ""),
        ];
        candidates.push(Candidate {
            last_contacted: 9,
            ..contact("given-new", "Alice New", "Alice", "New")
        });
        let keys: Vec<String> = rank("Alice", &candidates)
            .into_iter()
            .map(|s| s.key)
            .collect();
        assert_eq!(keys, ["display", "given-new", "given-old"]);
        // "Ali" is a prefix of the given names, and of none else.
        let keys: Vec<String> = rank("Ali", &candidates)
            .into_iter()
            .map(|s| s.key)
            .collect();
        assert_eq!(keys, ["given-new", "prefix", "given-old"]);
        let many: Vec<Candidate> = (0..9)
            .map(|i| contact(&i.to_string(), "Alice", "", ""))
            .collect();
        assert_eq!(rank("Alice", &many).len(), SHOWN);
        assert!(rank("Zed", &many).is_empty());
    }
}
