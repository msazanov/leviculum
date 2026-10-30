//! Which announced names the home node archives (1136): only those of a
//! peer the user has a relation to. The home node hangs on a transport that
//! hears the whole network's LXMF announces; archiving each of them grew
//! the archive by one entry an announce and handed every client names of
//! strangers. A relation is a conversation in the archive (a message from
//! the peer, or one sent to it), a `/send` an admitted device handed in for
//! it, or a petname in the home node's contacts file.
//!
//! Every name heard is still remembered, so a conversation that starts with
//! a peer heard before archives its name at once. Pure: the daemon
//! (`super::daemon`) feeds it the announces and the relations and appends
//! what it names.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;

/// The peers the user has a relation to, and the newest name heard from
/// each peer, related or not.
#[derive(Debug, Clone, Default)]
pub struct Relations {
    related: BTreeSet<[u8; 16]>,
    heard: BTreeMap<[u8; 16], String>,
}

impl Relations {
    /// The relations the store and the contacts file name at start.
    pub fn new(related: impl IntoIterator<Item = [u8; 16]>) -> Self {
        Self {
            related: related.into_iter().collect(),
            heard: BTreeMap::new(),
        }
    }

    /// `peer` announced `name`: the name to archive when the user has a
    /// relation to the peer, `None` for a stranger, whose name is only
    /// remembered.
    pub fn announced(&mut self, peer: [u8; 16], name: &str) -> Option<String> {
        self.heard.insert(peer, String::from(name));
        self.related.contains(&peer).then(|| String::from(name))
    }

    /// A conversation with `peer` began or went on: the newest name heard
    /// from it, to archive at once, when the relation is new.
    pub fn relate(&mut self, peer: [u8; 16]) -> Option<String> {
        if !self.related.insert(peer) {
            return None;
        }
        self.heard.get(&peer).cloned()
    }

    /// Whether the user has a relation to `peer`.
    pub fn is_related(&self, peer: &[u8; 16]) -> bool {
        self.related.contains(peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRIEND: [u8; 16] = [1; 16];
    const STRANGER: [u8; 16] = [2; 16];

    #[test]
    fn a_stranger_s_name_is_remembered_and_archived_only_once_a_conversation_starts() {
        let mut relations = Relations::new([FRIEND]);
        assert_eq!(
            relations.announced(FRIEND, "Friend").as_deref(),
            Some("Friend")
        );
        assert_eq!(relations.announced(STRANGER, "Stranger"), None);
        assert_eq!(relations.announced(STRANGER, "Stranger 2"), None);
        assert!(!relations.is_related(&STRANGER));
        // The first message: the newest name heard, at once.
        assert_eq!(relations.relate(STRANGER).as_deref(), Some("Stranger 2"));
        assert_eq!(relations.relate(STRANGER), None, "only the first time");
        assert_eq!(
            relations.announced(STRANGER, "Known").as_deref(),
            Some("Known")
        );
    }

    #[test]
    fn a_new_relation_to_a_peer_never_heard_archives_nothing_yet() {
        let mut relations = Relations::default();
        assert_eq!(relations.relate(FRIEND), None);
        assert!(relations.is_related(&FRIEND));
    }
}
