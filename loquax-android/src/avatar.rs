//! A peer's avatar as the app draws it (1129): a circle with initials,
//! since loquax has no photos. Its colours are the peer's own when it sent
//! an LXMF icon appearance, and otherwise the app picks one from the
//! address (Java, by the theme).

use leviculum_lxmf::constants::FIELD_ICON_APPEARANCE;
use leviculum_lxmf::msgpack;
use leviculum_lxmf::Field;
use loquax::ui::Message;

/// The initials for a peer: the first letter of the name's first word and
/// of its last, one letter for a name of one word, and the address's first
/// two hex digits for a peer without a name.
pub fn initials(name: Option<&str>, peer: &[u8; 16]) -> String {
    let first_letter = |word: &str| {
        word.chars()
            .find(|c| c.is_alphanumeric())
            .or_else(|| word.chars().next())
    };
    let words: Vec<&str> = name
        .map(|name| name.split_whitespace().collect())
        .unwrap_or_default();
    let letters: Vec<char> = match words.as_slice() {
        [] => Vec::new(),
        [only] => first_letter(only).into_iter().collect(),
        [first, .., last] => first_letter(first)
            .into_iter()
            .chain(first_letter(last))
            .collect(),
    };
    if letters.is_empty() {
        return loquax::address::to_hex(&peer[..1]).to_uppercase();
    }
    letters.into_iter().flat_map(char::to_uppercase).collect()
}

/// The foreground and background of an LXMF icon appearance in `fields`,
/// as `rrggbb`: Sideband's and Columba's `[icon name, fg, bg]` with three
/// bytes each. None for no such field or one of another shape.
pub fn appearance(fields: &[Field]) -> Option<(String, String)> {
    let (_, value) = fields.iter().find(|(k, _)| *k == FIELD_ICON_APPEARANCE)?;
    let mut p = 0;
    if msgpack::array_len(value, &mut p).ok()? < 3 {
        return None;
    }
    msgpack::skip(value, &mut p).ok()?;
    let mut colour = || {
        let bytes = msgpack::read_bin(value, &mut p).ok()?;
        (bytes.len() == 3).then(|| loquax::address::to_hex(bytes))
    };
    let fg = colour()?;
    let bg = colour()?;
    Some((fg, bg))
}

/// The newest icon appearance among a conversation's received messages.
pub fn latest_appearance(messages: &[Message]) -> Option<(String, String)> {
    messages
        .iter()
        .rev()
        .filter(|message| !message.outbound)
        .find_map(|message| appearance(&message.fields))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER: [u8; 16] = [0x3f; 16];

    #[test]
    fn initials_are_first_and_last_word() {
        assert_eq!(initials(Some("Jörn Brunke"), &PEER), "JB");
        assert_eq!(initials(Some("anna maria schulz"), &PEER), "AS");
        assert_eq!(initials(Some("lew"), &PEER), "L");
        assert_eq!(initials(Some("  (Basis)  Nord "), &PEER), "BN");
        assert_eq!(initials(Some("🦊"), &PEER), "🦊");
        assert_eq!(initials(Some("ß"), &PEER), "SS");
    }

    #[test]
    fn a_peer_without_a_name_shows_its_address() {
        assert_eq!(initials(None, &PEER), "3F");
        assert_eq!(initials(Some("   "), &PEER), "3F");
    }

    fn icon(name: &str, fg: &[u8], bg: &[u8]) -> Field {
        let mut value = Vec::new();
        msgpack::array(&mut value, 3);
        msgpack::string(&mut value, name);
        msgpack::bin(&mut value, fg);
        msgpack::bin(&mut value, bg);
        (FIELD_ICON_APPEARANCE, value)
    }

    #[test]
    fn an_icon_appearance_gives_its_colours() {
        let fields = vec![
            (0x02, vec![0xc0]),
            icon("account", &[255, 255, 255], &[0x1e, 0x88, 0xe5]),
        ];
        assert_eq!(
            appearance(&fields),
            Some(("ffffff".to_string(), "1e88e5".to_string()))
        );
    }

    #[test]
    fn a_malformed_icon_appearance_gives_none() {
        assert_eq!(appearance(&[]), None);
        assert_eq!(appearance(&[icon("x", &[1, 2], &[3, 4, 5])]), None);
        assert_eq!(
            appearance(&[(FIELD_ICON_APPEARANCE, vec![0x92, 0xa1, b'x', 0xc0])]),
            None
        );
        assert_eq!(
            appearance(&[(FIELD_ICON_APPEARANCE, vec![0xa1, b'x'])]),
            None
        );
        assert_eq!(appearance(&[(FIELD_ICON_APPEARANCE, Vec::new())]), None);
    }
}
