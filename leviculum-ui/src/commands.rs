//! The command palette's names: the one table a messenger frontend opens
//! with `:` or `Alt-x` and filters as the reader types
//! (`docs/src/concepts/lnmsg-ui.md`, section 1: "a command palette layered on
//! top, no modes").
//!
//! One table, so the terminal palette and the desktop GUI's offer the same
//! words for the same actions. The program maps a [`Command`] to what it
//! does; this module only names them and filters them. Each name is also a
//! row of the program's keymap ([`crate::keymap::Row::palette`]), which is
//! where its keys come from.
//!
//! `quit` is the only name with a `q` in it, so `:`, `q`, `Enter` quits as a
//! vi reader expects (`docs/src/concepts/lnmsg-keys-and-layout.md`, section
//! 3); a test holds it.

use alloc::vec::Vec;

/// What a palette entry runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    /// Send the compose line to the selected conversation.
    Send,
    /// Drain the propagation node's mailbox.
    Fetch,
    /// Leave the program.
    Quit,
    /// Show the keybindings.
    Help,
    /// Move the focus to the conversation list.
    FocusConversations,
    /// Move the focus to the message list.
    FocusMessages,
    /// Move the focus to the compose line.
    FocusCompose,
    /// Select the next conversation.
    NextConversation,
    /// Select the previous conversation.
    PreviousConversation,
    /// Select the next conversation with unread messages.
    NextUnread,
    /// Select the previous conversation with unread messages.
    PreviousUnread,
    /// What is known about the message under the cursor.
    Details,
    /// Draw the whole screen again.
    Redraw,
    /// Swap what `Enter` does in compose: send, or break the line.
    SendOnEnter,
}

/// One palette row: the words the reader filters on, and what it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaletteEntry {
    /// The name as typed and shown.
    pub name: &'static str,
    /// What it runs.
    pub command: Command,
}

/// The table, in the order the palette lists it unfiltered.
pub const COMMANDS: [PaletteEntry; 14] = [
    entry("send", Command::Send),
    entry("fetch", Command::Fetch),
    entry("quit", Command::Quit),
    entry("help", Command::Help),
    entry("focus conversations", Command::FocusConversations),
    entry("focus messages", Command::FocusMessages),
    entry("focus compose", Command::FocusCompose),
    entry("next conversation", Command::NextConversation),
    entry("previous conversation", Command::PreviousConversation),
    entry("next unread", Command::NextUnread),
    entry("previous unread", Command::PreviousUnread),
    entry("details", Command::Details),
    entry("redraw", Command::Redraw),
    entry("send on enter", Command::SendOnEnter),
];

/// The entry that runs `command`.
pub fn name_of(command: Command) -> Option<&'static str> {
    COMMANDS
        .iter()
        .find(|e| e.command == command)
        .map(|e| e.name)
}

const fn entry(name: &'static str, command: Command) -> PaletteEntry {
    PaletteEntry { name, command }
}

/// Whether `query` is a subsequence of `name`, ignoring ASCII case: the
/// palette's fuzzy match, so `fc` finds `focus compose`.
pub fn fuzzy_match(query: &str, name: &str) -> bool {
    let mut rest = name.chars();
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .all(|q| rest.any(|n| n.eq_ignore_ascii_case(&q)))
}

/// The entries `query` matches, in table order.
pub fn filter(query: &str) -> Vec<&'static PaletteEntry> {
    COMMANDS
        .iter()
        .filter(|e| fuzzy_match(query, e.name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_lists_the_whole_table_in_order() {
        let all: Vec<&str> = filter("").iter().map(|e| e.name).collect();
        assert_eq!(all, COMMANDS.map(|e| e.name));
    }

    #[test]
    fn a_subsequence_matches_and_a_reordering_does_not() {
        assert!(fuzzy_match("fc", "focus compose"));
        assert!(fuzzy_match("FOCO", "focus compose"));
        assert!(fuzzy_match("focus m", "focus messages"));
        assert!(!fuzzy_match("cf", "fetch"));
        assert!(
            fuzzy_match("fc", "fetch"),
            "a subsequence, not a word start"
        );
        let hits: Vec<Command> = filter("foc").iter().map(|e| e.command).collect();
        assert_eq!(
            hits,
            [
                Command::FocusConversations,
                Command::FocusMessages,
                Command::FocusCompose
            ]
        );
        assert_eq!(filter("qu")[0].command, Command::Quit);
    }

    /// `:q` then `Enter` runs the first match, and `quit` is the only name
    /// a `q` matches.
    #[test]
    fn q_alone_finds_quit_first() {
        let hits: Vec<Command> = filter("q").iter().map(|e| e.command).collect();
        assert_eq!(hits, [Command::Quit]);
    }
}
