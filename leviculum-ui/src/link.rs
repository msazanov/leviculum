//! Links: how a followed link target is handled, and where the links are in
//! a message's plain text.
//!
//! [`classify_link`] is lnomad's rule for a NomadNet page's links, a security
//! boundary as much as a parser: a link comes from an untrusted node or
//! sender, so only a whitelisted scheme is ever handed to a system handler.
//! lnmsg follows a link in a message body by the same rule
//! (`docs/src/concepts/lnmsg-keys-and-layout.md`, section 5, "What the mouse
//! does"); [`find_links`] is what finds them there, since a message body is
//! text with no link markup.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Length in hex characters of a Reticulum truncated destination hash
/// (`RNS.Reticulum.TRUNCATED_HASHLENGTH // 8 * 2` = 16 bytes = 32 chars).
pub const TRUNCATED_HASH_HEX_LEN: usize = 32;

/// How a followed link target should be handled.
///
/// A NomadNet page can carry three kinds of link: an in-mesh RNS destination we
/// fetch ourselves, an external URL we hand to the system's default handler, and
/// a URL whose scheme we refuse to open. The split is a security boundary: a
/// page comes from an untrusted node, so only a whitelisted scheme is ever
/// passed to a system handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkKind {
    /// An RNS/NomadNet destination (`<hash>:/page/...`, `<hash>`, or the
    /// same-destination `:/...` form). Followed by fetching it in-mesh.
    Rns,
    /// A NomadNet `lxmf@<hash>` target: an address to send a message to,
    /// not a page to fetch. Carries the 32-hex destination hash.
    ///
    /// NomadNet opens a conversation with it. We have no message composer,
    /// so the address is shown and copied instead; either way it must not be
    /// mistaken for a page, which is what produced a spurious "malformed URL"
    /// before this variant existed.
    Lxmf(String),
    /// An external URL with a safe scheme (`http`/`https`/`mailto`), openable in
    /// the user's default handler. Carries the original URL verbatim.
    External(String),
    /// A URL with a scheme we will not hand to a system handler (`file`,
    /// `javascript`, any custom scheme). Carries the offending scheme, lowercased.
    Unsafe(String),
}

/// Classify a link target so the browser knows whether to fetch it in-mesh, open
/// it externally, or refuse it. A pure function over the target string.
///
/// RNS targets keep NomadNet's colon grammar (`<32-hex>:/page/...`, a bare
/// 32-hex hash, or the same-destination `:/...`). An external URL is recognised
/// by a URI scheme (`<alpha><alnum|+-.>* :`); only `http`, `https` and `mailto`
/// are safe to open, and every other scheme is refused. A 32-hex destination
/// prefix is always read as an RNS hash, never a scheme, so a hash that happens
/// to start with a letter is not mistaken for an external URL.
pub fn classify_link(target: &str) -> LinkKind {
    // The same-destination form starts with the colon and never has a scheme.
    if target.starts_with(':') {
        return LinkKind::Rns;
    }
    if let Some(hash) = lxmf_target(target) {
        return LinkKind::Lxmf(hash);
    }
    match uri_scheme(target) {
        Some(scheme) => {
            // A full-length hex destination prefix is an RNS hash, not a scheme.
            if scheme.len() == TRUNCATED_HASH_HEX_LEN
                && scheme.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return LinkKind::Rns;
            }
            match scheme.to_ascii_lowercase().as_str() {
                "http" | "https" | "mailto" => LinkKind::External(target.to_string()),
                other => LinkKind::Unsafe(other.to_string()),
            }
        }
        // No scheme and no leading colon: a bare hash or relative page path.
        None => LinkKind::Rns,
    }
}

/// The destination hash of an `lxmf@<hash>` target, lowercased.
///
/// NomadNet's link grammar is `<destination-type>@<target>`, with `lxmf` a
/// shorthand for the `lxmf.delivery` aspect (NomadNet `Browser.py`,
/// `expand_shorthands` and `handle_link`). It accepts the target only as
/// exactly [`TRUNCATED_HASH_HEX_LEN`] hex characters, so anything else is not
/// an LXMF link and is left to the other rules.
///
/// Matching on the prefix rather than merely on the presence of `@` keeps a
/// page path that happens to contain one (`<hash>:/page/a@b.mu`) a page path.
fn lxmf_target(target: &str) -> Option<String> {
    let (kind, hash) = target.split_once('@')?;
    let kind = kind.to_ascii_lowercase();
    if kind != "lxmf" && kind != "lxmf.delivery" {
        return None;
    }
    if hash.len() != TRUNCATED_HASH_HEX_LEN || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(hash.to_ascii_lowercase())
}

/// The URI scheme of `s` (the run before the first `:`), if it is a well-formed
/// scheme name: a leading ASCII letter followed by letters, digits, `+`, `-` or
/// `.` (RFC 3986). Returns `None` when there is no colon or the prefix is not a
/// valid scheme (e.g. it starts with a digit, as a numeric destination hash does).
fn uri_scheme(s: &str) -> Option<&str> {
    let idx = s.find(':')?;
    let scheme = &s[..idx];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        Some(scheme)
    } else {
        None
    }
}

/// A link found in plain text: where it is, in characters, and its target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundLink {
    /// The first character of the link.
    pub start: usize,
    /// The first character after it.
    pub end: usize,
    /// The link's text, which is its target.
    pub target: String,
}

/// The links in `text`: each whitespace-separated word that is an `http`,
/// `https` or `mailto` URL, an `lxmf@<address>` target, or a NomadNet page
/// (`<address>:/page/...`), with the brackets and the sentence punctuation
/// around it left out. A word of any other shape is text, so a sentence that
/// happens to hold a colon is not a link.
pub fn find_links(text: &str) -> Vec<FoundLink> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        if chars[at].is_whitespace() {
            at += 1;
            continue;
        }
        let word_end = chars[at..]
            .iter()
            .position(|c| c.is_whitespace())
            .map_or(chars.len(), |n| at + n);
        let mut start = at;
        let mut end = word_end;
        while start < end && matches!(chars[start], '(' | '<' | '[' | '"' | '\'') {
            start += 1;
        }
        while end > start
            && matches!(
                chars[end - 1],
                '.' | ',' | ';' | ':' | '!' | '?' | ')' | '>' | ']' | '"' | '\''
            )
        {
            end -= 1;
        }
        let word: String = chars[start..end].iter().collect();
        if is_link(&word) {
            found.push(FoundLink {
                start,
                end,
                target: word,
            });
        }
        at = word_end;
    }
    found
}

/// Whether `word` is one of the link shapes [`find_links`] takes.
fn is_link(word: &str) -> bool {
    match classify_link(word) {
        LinkKind::External(_) => word
            .split_once(':')
            .is_some_and(|(_, rest)| !rest.is_empty()),
        LinkKind::Lxmf(_) => true,
        LinkKind::Rns => word.split_once(':').is_some_and(|(hash, path)| {
            hash.len() == TRUNCATED_HASH_HEX_LEN
                && hash.bytes().all(|b| b.is_ascii_hexdigit())
                && path.starts_with('/')
        }),
        LinkKind::Unsafe(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef";

    fn targets(text: &str) -> Vec<String> {
        find_links(text).into_iter().map(|l| l.target).collect()
    }

    #[test]
    fn the_three_shapes_are_found_and_their_punctuation_left_out() {
        let text = alloc::format!(
            "see https://example.org/x. or (mailto:a@b), lxmf@{HASH} and {HASH}:/page/index.mu!"
        );
        assert_eq!(
            targets(&text),
            [
                "https://example.org/x".to_string(),
                "mailto:a@b".to_string(),
                alloc::format!("lxmf@{HASH}"),
                alloc::format!("{HASH}:/page/index.mu"),
            ]
        );
        let first = &find_links(&text)[0];
        assert_eq!((first.start, first.end), (4, 25));
    }

    #[test]
    fn a_colon_in_a_sentence_and_an_unsafe_scheme_are_text() {
        assert!(targets("note: the time is 12:00").is_empty());
        assert!(targets("javascript:alert(1) file:///etc/passwd").is_empty());
        assert!(targets(&alloc::format!("{HASH} alone")).is_empty());
        assert!(targets("https:").is_empty());
    }

    #[test]
    fn positions_count_characters_not_bytes() {
        let found = find_links("größe https://ä.example");
        assert_eq!(found[0].start, 6);
        assert_eq!(found[0].end, 23);
    }
}
