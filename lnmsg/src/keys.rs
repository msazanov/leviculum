//! lnmsg's keymap: its rows beside the shared scroll rows, and the four
//! readers of them (`docs/src/concepts/lnmsg-keys-and-layout.md`, sections 3
//! and 4).
//!
//! [`resolve`] is the key handler's lookup, [`help_groups`] the help
//! overlay, [`footer`] the footer strip and [`compose_hint`] the line on the
//! compose pane's border; the palette's names are rows too
//! ([`leviculum_ui::keymap::Row::palette`]). The tables are data, the per-pane
//! rules of section 3 ("How the collisions are settled") are each chord's
//! panes, and the tests at the end hold what the page asks of the table: no
//! chord reaches two actions in one pane, every row is in the help and every
//! palette name has a row, the quiet keyboard, a plain chord beside every
//! enhanced one, and `:` `q` `Enter` quits.
//!
//! Not here yet, because what they run is a later slice: new conversation
//! (`m`, `C`), search (`/`, `Ctrl-s`, `n`/`N`), hints (`f`), delete (`d`),
//! and `Ctrl-x Ctrl-c` (a two-key sequence, which the table does not
//! model). The mouse is not a chord: what it does is [`crate::ui::hit`],
//! and only its capture switch is a row, the palette's `mouse`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_ui::commands::Command;
use leviculum_ui::footer::FooterItem;
use leviculum_ui::help::{HelpEntry, HelpGroup};
use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use leviculum_ui::keymap::{self, Chord, Ctx, Mods, Panes, Row, ScrollCmd, MOVE, SCROLL_ROWS};

/// The conversation list.
pub const LIST: Panes = Panes(1 << 1);
/// The messages.
pub const MESSAGES: Panes = Panes(1 << 2);
/// The compose pane.
pub const COMPOSE: Panes = Panes(1 << 3);
/// An open overlay (the help, the palette): only what is bound here reaches
/// past it.
pub const OVERLAY: Panes = Panes(1 << 4);
/// The two command panes.
pub const COMMAND_PANES: Panes = Panes(LIST.0 | MESSAGES.0);
/// Every pane outside an overlay, the design page's **all**.
pub const EVERY: Panes = Panes(LIST.0 | MESSAGES.0 | COMPOSE.0);

/// `send_on_enter` is off: `Enter` breaks the line in compose and the
/// modifier chord sends.
pub const ENTER_BREAKS: Ctx = Ctx(1 << 1);

/// What a key runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// A palette command: the same thing the palette runs under its name.
    Run(Command),
    /// Move the focused list's cursor.
    Scroll(ScrollCmd),
    /// Move the messages' cursor from the compose pane.
    ScrollMessages(ScrollCmd),
    /// Break the compose line.
    Newline,
    /// Hand the key to the compose buffer's editing table.
    Edit,
    /// Clear compose into the kill buffer, back with `Ctrl-y`.
    ClearDraft,
    /// Back out: compose to messages to the list, and an overlay closes.
    Back,
    /// The next pane.
    NextPane,
    /// The previous pane.
    PreviousPane,
    /// Open the command palette.
    Palette,
    /// `Ctrl-c` outside compose: say what quits.
    QuitHint,
    /// Copy the drag's selection, or the message under the cursor.
    Copy,
}

const fn key(code: KeyCode, mods: Mods, action: Action, panes: Panes) -> Chord<Action> {
    Chord::new(code, mods, action).panes(panes)
}

const NONE: Mods = Mods::Exact(KeyModifiers::NONE);
const CTRL: Mods = Mods::Exact(KeyModifiers::CONTROL);
const ALT: Mods = Mods::Exact(KeyModifiers::ALT);
const SHIFT: Mods = Mods::Exact(KeyModifiers::SHIFT);
const ALT_SHIFT: Mods = Mods::Exact(KeyModifiers::from_bits_truncate(
    KeyModifiers::ALT.bits() | KeyModifiers::SHIFT.bits(),
));
const CTRL_SHIFT: Mods = Mods::Exact(KeyModifiers::from_bits_truncate(
    KeyModifiers::CONTROL.bits() | KeyModifiers::SHIFT.bits(),
));

const fn run(command: Command) -> Action {
    Action::Run(command)
}

/// The help group titles, in the overlay's order. The first is the shared
/// rows' group.
pub const GROUPS: [&str; 5] = [
    MOVE,
    CONVERSATIONS,
    MESSAGES_GROUP,
    COMPOSE_GROUP,
    EVERYWHERE,
];
const CONVERSATIONS: &str = "Conversations";
const MESSAGES_GROUP: &str = "Messages";
const COMPOSE_GROUP: &str = "Compose";
const EVERYWHERE: &str = "Everywhere";

/// lnmsg's own rows, beside [`SCROLL_ROWS`], which apply in the two command
/// panes.
pub const ROWS: &[Row<Action>] = {
    use Action::*;
    use Command as C;
    use KeyCode::*;
    use ScrollCmd::{Bottom, PageDown as PgDn, PageUp as PgUp, Top};
    &[
        // Move: the one chord lnmsg adds to the shared rows, and paging the
        // messages while composing.
        Row::new(
            "top / bottom (Signal)",
            MOVE,
            &[&[
                key(Up, CTRL, Scroll(Top), COMMAND_PANES),
                key(Down, CTRL, Scroll(Bottom), COMMAND_PANES),
            ]],
        ),
        Row::new(
            "page the messages (compose)",
            MOVE,
            &[&[
                key(PageDown, Mods::Any, ScrollMessages(PgDn), COMPOSE),
                key(PageUp, Mods::Any, ScrollMessages(PgUp), COMPOSE),
            ]],
        ),
        // Conversations.
        Row::new(
            "open the selected conversation",
            CONVERSATIONS,
            &[&[key(Enter, NONE, run(C::FocusMessages), LIST)]],
        )
        .palette("focus messages")
        .footer("open"),
        // Messages; listed here so its footer leads the messages pane's.
        Row::new(
            "the message's details",
            MESSAGES_GROUP,
            &[&[key(Enter, NONE, run(C::Details), MESSAGES)]],
        )
        .palette("details")
        .footer("details")
        .note("Ctrl-D in Signal: here Enter"),
        Row::new(
            "copy the message's text, or what a drag selected",
            MESSAGES_GROUP,
            &[
                &[key(Char('y'), Mods::Plain, Copy, MESSAGES)],
                &[key(Char('w'), ALT, Copy, MESSAGES)],
            ],
        ),
        Row::new(
            "previous conversation",
            CONVERSATIONS,
            &[
                &[key(Up, ALT, run(C::PreviousConversation), EVERY)],
                &[key(BackTab, CTRL_SHIFT, run(C::PreviousConversation), EVERY).enhanced()],
            ],
        )
        .palette("previous conversation"),
        Row::new(
            "next conversation",
            CONVERSATIONS,
            &[
                &[key(Down, ALT, run(C::NextConversation), EVERY)],
                &[key(Tab, CTRL, run(C::NextConversation), EVERY).enhanced()],
            ],
        )
        .palette("next conversation"),
        Row::new(
            "previous unread",
            CONVERSATIONS,
            &[&[key(Up, ALT_SHIFT, run(C::PreviousUnread), EVERY)]],
        )
        .palette("previous unread"),
        Row::new(
            "next unread",
            CONVERSATIONS,
            &[&[key(Down, ALT_SHIFT, run(C::NextUnread), EVERY)]],
        )
        .palette("next unread")
        .note("Tab in mutt: here Alt-Shift-↓, Tab cycles the panes"),
        Row::new(
            "write in this conversation",
            CONVERSATIONS,
            &[
                &[
                    key(Char('i'), Mods::Plain, run(C::FocusCompose), COMMAND_PANES),
                    key(Char('a'), Mods::Plain, run(C::FocusCompose), COMMAND_PANES),
                ],
                &[key(
                    Char('r'),
                    Mods::Plain,
                    run(C::FocusCompose),
                    COMMAND_PANES,
                )],
                &[key(Char('T'), CTRL_SHIFT, run(C::FocusCompose), COMMAND_PANES).enhanced()],
            ],
        )
        .palette("focus compose")
        .footer_droppable("write", 3),
        Row::new("focus the conversation list", CONVERSATIONS, &[]).palette("focus conversations"),
        // Compose. The Enter rule: Enter sends and Shift-Enter (enhanced),
        // Alt-Enter and Ctrl-j break the line; Ctrl-Enter (enhanced) always
        // sends and never breaks. With send_on_enter off, Enter breaks and
        // Alt-Enter sends.
        Row::new(
            "send",
            COMPOSE_GROUP,
            &[
                &[key(Enter, NONE, run(C::Send), COMPOSE).unless(ENTER_BREAKS)],
                &[key(Enter, CTRL, run(C::Send), COMPOSE).enhanced()],
                &[key(Enter, ALT, run(C::Send), COMPOSE).when(ENTER_BREAKS)],
            ],
        )
        .palette("send")
        .footer("send")
        .note("with send on enter off, Enter breaks the line and the modifier chord sends")
        .tx(),
        Row::new(
            "line break",
            COMPOSE_GROUP,
            &[
                &[key(Enter, NONE, Newline, COMPOSE).when(ENTER_BREAKS)],
                &[key(Enter, SHIFT, Newline, COMPOSE).enhanced()],
                &[key(Enter, ALT, Newline, COMPOSE).unless(ENTER_BREAKS)],
                &[key(Char('j'), CTRL, Newline, COMPOSE)],
            ],
        ),
        Row::new(
            "line start / end",
            COMPOSE_GROUP,
            &[
                &[
                    key(Char('a'), CTRL, Edit, COMPOSE),
                    key(Char('e'), CTRL, Edit, COMPOSE),
                ],
                &[
                    key(Home, NONE, Edit, COMPOSE),
                    key(End, NONE, Edit, COMPOSE),
                ],
            ],
        ),
        Row::new(
            "line up / down",
            COMPOSE_GROUP,
            &[
                &[
                    key(Char('p'), CTRL, Edit, COMPOSE),
                    key(Char('n'), CTRL, Edit, COMPOSE),
                ],
                &[key(Up, NONE, Edit, COMPOSE), key(Down, NONE, Edit, COMPOSE)],
            ],
        ),
        Row::new(
            "body start / end",
            COMPOSE_GROUP,
            &[&[
                key(Char('<'), Mods::Alt, Edit, COMPOSE),
                key(Char('>'), Mods::Alt, Edit, COMPOSE),
            ]],
        ),
        Row::new(
            "character back / forward",
            COMPOSE_GROUP,
            &[
                &[
                    key(Char('b'), CTRL, Edit, COMPOSE),
                    key(Char('f'), CTRL, Edit, COMPOSE),
                ],
                &[
                    key(Left, NONE, Edit, COMPOSE),
                    key(Right, NONE, Edit, COMPOSE),
                ],
            ],
        ),
        Row::new(
            "word back / forward",
            COMPOSE_GROUP,
            &[
                &[
                    key(Char('b'), Mods::Alt, Edit, COMPOSE),
                    key(Char('f'), Mods::Alt, Edit, COMPOSE),
                ],
                &[
                    key(Left, CTRL, Edit, COMPOSE),
                    key(Right, CTRL, Edit, COMPOSE),
                ],
            ],
        ),
        Row::new(
            "delete forward",
            COMPOSE_GROUP,
            &[
                &[key(Char('d'), CTRL, Edit, COMPOSE)],
                &[key(Delete, NONE, Edit, COMPOSE)],
            ],
        ),
        Row::new(
            "delete a word back / forward",
            COMPOSE_GROUP,
            &[
                &[
                    key(Backspace, Mods::Alt, Edit, COMPOSE),
                    key(Char('d'), Mods::Alt, Edit, COMPOSE),
                ],
                &[key(Char('w'), CTRL, Edit, COMPOSE)],
            ],
        ),
        Row::new(
            "kill to the end, yank",
            COMPOSE_GROUP,
            &[&[
                key(Char('k'), CTRL, Edit, COMPOSE),
                key(Char('y'), CTRL, Edit, COMPOSE),
            ]],
        ),
        Row::new(
            "clear to a draft Ctrl-y brings back",
            COMPOSE_GROUP,
            &[&[key(Char('c'), CTRL, ClearDraft, COMPOSE)]],
        ),
        // Everywhere.
        Row::new(
            "next / previous pane",
            EVERYWHERE,
            &[&[
                key(Tab, Mods::Any, NextPane, EVERY),
                key(BackTab, Mods::Any, PreviousPane, EVERY),
            ]],
        )
        .footer("pane"),
        Row::new(
            "back out: compose, messages, list; close an overlay",
            EVERYWHERE,
            &[
                &[key(
                    Esc,
                    NONE,
                    Back,
                    Panes(MESSAGES.0 | COMPOSE.0 | OVERLAY.0),
                )],
                &[key(
                    Char('g'),
                    CTRL,
                    Back,
                    Panes(MESSAGES.0 | COMPOSE.0 | OVERLAY.0),
                )],
            ],
        )
        .footer_droppable("back", 5),
        Row::new(
            "command palette",
            EVERYWHERE,
            &[
                &[key(Char(':'), Mods::Plain, Palette, COMMAND_PANES)],
                &[key(Char('x'), ALT, Palette, EVERY)],
            ],
        )
        .footer_droppable("commands", 2)
        .note("Ctrl-P moves a line in every pane: the palette is : or Alt-x"),
        Row::new(
            "help",
            EVERYWHERE,
            &[
                &[key(Char('?'), Mods::Plain, run(C::Help), COMMAND_PANES)],
                &[key(F(1), Mods::Any, run(C::Help), EVERY)],
            ],
        )
        .palette("help")
        .footer_droppable("help", 1),
        Row::new(
            "fetch the mailbox",
            EVERYWHERE,
            &[
                &[key(F(5), Mods::Any, run(C::Fetch), EVERY)],
                &[key(Char('r'), CTRL, run(C::Fetch), COMMAND_PANES)],
            ],
        )
        .palette("fetch")
        .tx(),
        Row::new(
            "redraw",
            EVERYWHERE,
            &[&[key(Char('l'), CTRL, run(C::Redraw), EVERY)]],
        )
        .palette("redraw"),
        Row::new(
            "quit",
            EVERYWHERE,
            &[
                &[key(Char('q'), Mods::Plain, run(C::Quit), COMMAND_PANES)],
                &[key(
                    Char('q'),
                    CTRL,
                    run(C::Quit),
                    Panes(EVERY.0 | OVERLAY.0),
                )],
            ],
        )
        .palette("quit")
        .footer("quit"),
        Row::new(
            "says what quits (Ctrl-c quits nowhere)",
            EVERYWHERE,
            &[&[key(
                Char('c'),
                CTRL,
                QuitHint,
                Panes(LIST.0 | MESSAGES.0 | OVERLAY.0),
            )]],
        ),
        Row::new("Enter sends, or breaks the line", EVERYWHERE, &[]).palette("send on enter"),
        Row::new(
            "mouse capture off and on (--no-mouse starts it off)",
            EVERYWHERE,
            &[],
        )
        .palette("mouse"),
        Row::new(
            "light and dark colours swapped for the session (--theme, the theme key)",
            EVERYWHERE,
            &[],
        )
        .palette("theme"),
    ]
};

/// The panes the chords of a pane are looked up in: the command panes take
/// the shared scroll rows too.
pub fn panes_of(pane: crate::ui::Pane) -> Panes {
    match pane {
        crate::ui::Pane::Conversations => LIST.with(Panes::SCROLL),
        crate::ui::Pane::Messages => MESSAGES.with(Panes::SCROLL),
        crate::ui::Pane::Compose => COMPOSE,
    }
}

/// What `key` runs in `pane` under `ctx`: the most specific chord across
/// lnmsg's rows and the shared scroll rows, lnmsg's first among equals.
pub fn resolve(key: &KeyEvent, pane: Panes, ctx: Ctx) -> Option<Action> {
    let own = keymap::best(ROWS, key, pane, ctx);
    let shared = keymap::best(SCROLL_ROWS, key, pane, ctx).map(|(r, c)| (r, Action::Scroll(c)));
    match (own, shared) {
        (Some(own), Some(shared)) if shared.0 > own.0 => Some(shared.1),
        (Some(own), _) => Some(own.1),
        (None, shared) => shared.map(|(_, action)| action),
    }
}

/// The row whose palette name is `name`.
pub fn row_named(name: &str) -> Option<&'static Row<Action>> {
    ROWS.iter().find(|r| r.palette == Some(name))
}

/// The chord the palette shows beside `name`: the row's first one this
/// terminal can send.
pub fn palette_key(name: &str, ctx: Ctx) -> Option<&'static str> {
    row_named(name)?
        .chords()
        .find(|c| c.live(Panes::ALL, ctx))
        .map(|c| c.label())
}

/// The help overlay, every row of both tables in the groups of
/// [`GROUPS`]; a row's lost-habit note is the line under it.
pub fn help_groups() -> Vec<HelpGroup> {
    let entry = |keys, desc| HelpEntry { keys, desc };
    GROUPS
        .iter()
        .map(|&title| {
            let mut entries = Vec::new();
            if title == MOVE {
                entries.extend(SCROLL_ROWS.iter().map(|r| entry(r.keys(), r.desc)));
            }
            for row in ROWS.iter().filter(|r| r.group == title) {
                entries.push(entry(row.keys(), row.desc));
                if let Some(note) = row.note {
                    entries.push(entry("", note));
                }
            }
            HelpGroup { title, entries }
        })
        .collect()
}

/// What a footer button runs when clicked: the action of the chord it
/// shows. Nothing for a row that transmits: the mouse sends with the word
/// `send` on the compose box's border and fetches with the mailbox state,
/// and with nothing else (section 5, "What the mouse does").
pub fn footer_action(row: &Row<Action>, chord: &Chord<Action>) -> Option<Action> {
    (!row.tx).then_some(chord.action)
}

/// The footer buttons for `pane` under `ctx`: every row with a footer
/// label and a chord this terminal can send there, its key that chord.
pub fn footer<A: Copy>(
    pane: Panes,
    ctx: Ctx,
    act: impl Fn(Option<Action>) -> A,
) -> Vec<FooterItem<A>> {
    ROWS.iter()
        .filter_map(|row| {
            let spec = row.footer?;
            let chord = row.live_chord(pane, ctx)?;
            let action = act(footer_action(row, chord));
            Some(match spec.drop_rank {
                None => FooterItem::action(chord.label(), spec.label, action),
                Some(rank) => FooterItem::droppable(chord.label(), spec.label, action, rank),
            })
        })
        .collect()
}

/// The chord that does `what` in compose right now.
fn live(what: Action, ctx: Ctx) -> &'static str {
    ROWS.iter()
        .flat_map(|r| r.chords())
        .find(|c| c.action == what && c.live(COMPOSE, ctx))
        .map(|c| c.label())
        .unwrap_or("")
}

/// The chord that sends from compose in this terminal and setting.
pub fn send_chord(ctx: Ctx) -> &'static str {
    live(Action::Run(Command::Send), ctx)
}

/// The chord that breaks the line in compose in this terminal and setting.
pub fn newline_chord(ctx: Ctx) -> &'static str {
    live(Action::Newline, ctx)
}

/// The compose pane's border hint: the live send and line-break chords,
/// `Enter send · Alt-Enter newline` in a plain terminal.
pub fn compose_hint(ctx: Ctx) -> String {
    format!("{} send · {} newline", send_chord(ctx), newline_chord(ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_ui::commands::{filter, COMMANDS};
    use leviculum_ui::keymap::{all_best, canonical};

    const PANES: [Panes; 4] = [
        Panes(LIST.0 | Panes::SCROLL.0),
        Panes(MESSAGES.0 | Panes::SCROLL.0),
        COMPOSE,
        OVERLAY,
    ];
    const CTXS: [Ctx; 4] = [
        Ctx::NONE,
        Ctx::ENHANCED,
        ENTER_BREAKS,
        Ctx(Ctx::ENHANCED.0 | ENTER_BREAKS.0),
    ];

    /// Every action `key` reaches at the winning rank across both tables.
    fn hits(key: &KeyEvent, pane: Panes, ctx: Ctx) -> Vec<Action> {
        let own = all_best(ROWS, key, pane, ctx);
        let shared: Vec<(u8, Action)> = all_best(SCROLL_ROWS, key, pane, ctx)
            .into_iter()
            .map(|(r, c)| (r, Action::Scroll(c)))
            .collect();
        let top = own.iter().chain(&shared).map(|(r, _)| *r).max();
        let mut out = Vec::new();
        for (rank, action) in own.into_iter().chain(shared) {
            if Some(rank) == top && !out.contains(&action) {
                out.push(action);
            }
        }
        out
    }

    fn every_chord() -> impl Iterator<Item = (Action, KeyEvent, &'static str, Panes, Ctx)> {
        let own = ROWS
            .iter()
            .flat_map(|r| r.chords())
            .map(|c| (c.action, canonical(c), c.label(), c.panes, c.when, c.unless));
        let shared = SCROLL_ROWS.iter().flat_map(|r| r.chords()).map(|c| {
            (
                Action::Scroll(c.action),
                canonical(c),
                c.label(),
                c.panes,
                c.when,
                c.unless,
            )
        });
        own.chain(shared)
            .flat_map(|(action, event, label, panes, when, unless)| {
                PANES.into_iter().flat_map(move |pane| {
                    CTXS.into_iter().filter_map(move |ctx| {
                        let in_force =
                            panes.meets(pane) && ctx.contains(when) && !ctx.meets(unless);
                        in_force.then_some((action, event, label, pane, ctx))
                    })
                })
            })
    }

    /// No chord reaches two actions in one pane: each chord's own key, in
    /// every pane and setting it is in force, resolves to its own row's
    /// action and to nothing else. lnomad's first-match scan could not see a
    /// shadowed chord; this can.
    #[test]
    fn no_chord_resolves_to_two_actions_in_one_pane() {
        for (action, event, label, pane, ctx) in every_chord() {
            let got = hits(&event, pane, ctx);
            assert_eq!(got, [action], "{label} in {pane:?} under {ctx:?}");
            assert_eq!(resolve(&event, pane, ctx), Some(action), "{label}");
        }
    }

    /// Every row is in the help with its derived keys, and every palette
    /// name has a row whose chords run that very command.
    #[test]
    fn every_row_is_in_help_and_every_palette_name_has_a_row() {
        let groups = help_groups();
        let shown: Vec<(&str, &str)> = groups
            .iter()
            .flat_map(|g| g.entries.iter().map(|e| (e.keys, e.desc)))
            .collect();
        for row in ROWS {
            assert!(
                shown.contains(&(row.keys(), row.desc)),
                "{:?} missing from help",
                row.desc
            );
            assert!(GROUPS.contains(&row.group), "{:?}", row.group);
        }
        for row in SCROLL_ROWS {
            assert!(shown.contains(&(row.keys(), row.desc)), "{:?}", row.desc);
        }
        for entry in COMMANDS {
            let row = row_named(entry.name)
                .unwrap_or_else(|| panic!("palette name {:?} has no row", entry.name));
            for chord in row.chords() {
                assert_eq!(chord.action, Action::Run(entry.command), "{}", entry.name);
            }
        }
        for row in ROWS {
            if let Some(name) = row.palette {
                assert!(
                    COMMANDS.iter().any(|e| e.name == name),
                    "row {:?} names {name:?}, not in the palette",
                    row.desc
                );
            }
        }
    }

    /// The quiet keyboard: the one transmitting chord a reader can type by
    /// accident is `Enter` in compose.
    #[test]
    fn the_only_single_keystroke_that_transmits_is_send_in_compose() {
        let mut single = Vec::new();
        for row in ROWS.iter().filter(|r| r.tx) {
            for chord in row.chords().filter(|c| c.is_single_keystroke()) {
                single.push((chord.label(), chord.panes));
            }
        }
        assert_eq!(single, [("Enter", COMPOSE)]);
    }

    /// A chord only an enhanced terminal sends always has a sibling every
    /// terminal sends, in the same row.
    #[test]
    fn every_enhanced_chord_has_a_plain_one_beside_it() {
        for row in ROWS {
            if row.chords().any(|c| c.enhanced) {
                assert!(
                    row.chords().any(|c| !c.enhanced),
                    "{:?} only has enhanced chords",
                    row.desc
                );
            }
        }
    }

    /// `:` opens the palette in a command pane, `q` narrows it to `quit`
    /// alone, so `Enter` quits.
    #[test]
    fn colon_q_enter_quits() {
        let colon = KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE);
        assert_eq!(resolve(&colon, PANES[0], Ctx::NONE), Some(Action::Palette));
        assert_eq!(filter("q")[0].command, Command::Quit);
    }

    /// The hint names the chord live in this terminal and setting, never
    /// both worlds.
    #[test]
    fn the_compose_hint_follows_the_terminal_and_the_switch() {
        assert_eq!(compose_hint(Ctx::NONE), "Enter send · Alt-Enter newline");
        assert_eq!(
            compose_hint(Ctx::ENHANCED),
            "Enter send · Shift-Enter newline"
        );
        assert_eq!(compose_hint(ENTER_BREAKS), "Alt-Enter send · Enter newline");
        assert_eq!(
            compose_hint(Ctx(Ctx::ENHANCED.0 | ENTER_BREAKS.0)),
            "Ctrl-Enter send · Enter newline"
        );
    }

    /// `Ctrl-Enter` sends in every setting and never breaks the line.
    #[test]
    fn ctrl_enter_always_sends() {
        let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        for ctx in CTXS {
            assert_eq!(
                resolve(&ctrl_enter, COMPOSE, ctx),
                Some(Action::Run(Command::Send)),
                "{ctx:?}"
            );
        }
    }
}
