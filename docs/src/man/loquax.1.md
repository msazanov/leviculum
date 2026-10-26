# loquax(1)

## NAME

loquax -- terminal LXMF messenger for Reticulum

## SYNOPSIS

**loquax** [**--to** *address*]... [*options*]
**loquax send** [*options*] *address* [*body*]
**loquax fetch** [*options*]
**loquax address**
**loquax tui** [**--to** *address*]... [*options*]

## DESCRIPTION

**loquax** is the Leviculum messenger: with no subcommand it opens the conversation view, and **send**, **fetch** and **address** are the commands for scripts. It sends and receives LXMF messages over Reticulum, attaches to a shared instance that is already running -- **lnsd**(1), or Python's **rnsd** -- and runs its own LXMF router; it never starts a Reticulum stack of its own. Its address is a pure function of the identity in its state directory, minted on first use and never replaced silently.

**send** queues one message. The body is the argument, or standard input when the argument is missing or `-`, one trailing newline removed. A send that worked prints nothing and exits 0, which means the message was queued and handed to the network, not that it was delivered. **--via auto**, the default, tries a direct delivery link first and falls back to a propagation node when none comes up; exit 3 says a propagation node holds the message. A message that went onto the link and got no delivery proof in time is not sent again through a propagation node, which would deliver it twice; the run says so and exits 1.

**fetch** drains the propagation node's mailbox and prints what is new on standard output.

**address** prints this machine's LXMF address.

**loquax** without a subcommand, or **loquax tui** under its old name, is the interactive messenger: conversations on the left, the open conversation on the right with a compose box under it (see THE SCREEN). A message sent from it travels the way **send --via auto** sends it, and each sent message carries a delivery mark (see DELIVERY MARKS). loquax keeps no message history yet, so the screen holds the messages of this session only; **--to** lists a correspondent from the start. The packages ship it; a build from source without the `tui` cargo feature has only the script commands, and a plain **loquax** there says so and exits 2.

## OPTIONS

**--instance** *name*
:   Shared-instance name to join, overriding the Reticulum config file's. All subcommands but **address**.

**--config** *dir*
:   Reticulum config directory; only its instance name and RPC key are read. Default: the platform default.

**--pn** *hash*
:   The propagation node to use, remembered as your choice. Without it the `propagation_node` key in loquax's config decides; without either, the mailbox an earlier run adopted, or a newly adopted one: the most recently heard node that does not run on this device. **send**, **fetch** and **tui**.

**--timeout** *secs*
:   **send**: seconds to get the message onto the network, per leg of **--via auto** (default 30). **fetch**: seconds for the whole drain (default 60). **tui**: per leg of each send, a fetch getting twice that.

**--via** **auto**|**direct**|**propagated**
:   **send** only: how the message travels.

**--title** *text*, **--from** *name*
:   **send** only: the LXMF title, and the name recipients see as the sender (default: `LNMSG_DISPLAY_NAME`, then the account name).

**--to** *address*
:   **tui** only, repeatable: a correspondent to list from the start.

**--ascii**
:   **tui** only: draw the delivery marks in ASCII.

**--no-color**
:   **tui** only: no colours. Alignment, the glyphs and reverse video for the selection, the badge and the toast carry everything, as they do in every colour tier too. **NO_COLOR** does the same.

**--color** `auto`|`truecolor`|`256`|`16`
:   **tui** only: the colour tier. `auto` (the default) takes true colour when **COLORTERM** is `truecolor` or `24bit`, the 256-colour palette when **TERM** contains `256color`, no colour when **TERM** is `dumb` or unset, and the sixteen ANSI colours otherwise. **--no-color** and **NO_COLOR** win over it.

**--theme** `auto`|`dark`|`light`
:   **tui** only: dark or light colours. `auto` asks the terminal for its background and takes dark when it does not answer within 100 ms. Over the config file's `theme` key; the palette's `theme` swaps the two for the session.

**--no-mouse**
:   **tui** only: leave the mouse to the terminal. Its own selection and copy then work as in any other program, and loquax sees no click, wheel or drag; the palette's `mouse` captures it again, and turns it off again.

**--script** *file*, **--frames** *file*, **--size** *W*x*H*
:   **tui** only: run a key script instead of reading the keyboard, and write each frame it asks for to **--frames** (default: the script's path with `.frames`), on a screen of **--size** (default 80x24). One step a line, `#` a comment: `key` *name* (`tab`, `backtab`, `enter`, `esc`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `backspace`, `delete`, `space`, `f1` to `f12`, or one character, after any of the prefixes `ctrl-`, `alt-` and `shift-`, so `shift-enter` and `ctrl-enter` too), `type` *text*, `newline` (Alt-Enter, a line break in compose), `keyboard` `enhanced`|`plain` (the keyboard the screen is told it has: its hint offers `Shift-Enter` and `Ctrl-Enter` only when enhanced; plain until said), `tick` [*n*], `sleep` *ms* (up to 60000: read the engine as the idle screen does and tick once per 250 ms of *ms*), `frame` [*label*], and `until ready`, `until delivery` *state*, `until received` *n*, `until named` *n* (*n* peers have announced a name), `until leg` `direct`|`propagated` (the send runner reported that leg for the newest sent message), each with an optional timeout in seconds (default 30), and `clock` *YYYY-MM-DD* *HH:MM* (the time of day from then on, at UTC; a script starts at 2026-10-26 12:00 and never reads the wall clock, so its frames are the same on every run). A script draws without colour and dark, whatever the terminal it runs in, until `tier` `mono`|`16`|`256`|`truecolor` and `theme` `dark`|`light` say otherwise; `styles` *name* writes the screen's colours as a map to the frames file's path with `.styles` added, one letter a cell: `.` your block, `,` theirs, `=` the date pill, `!` the `new` rule, `^` a selected block or reverse video, `t` the toast, `e` the error colour, blank the terminal's own. The mouse steps take screen cells counted from 1 at the top left, as the frames' rows and columns are: `click` *col* *row* (the left button down and up), `wheel` `up`|`down` *col* *row* [*n*] (*n* notches, 1 by default) and `drag` *col* *row* *col* *row* (down at the first cell, moved to the second and up there). What the screen asks of the terminal goes into the frames where it happens: `--- osc52` *sequence* `---` for a copy, the escape character written `\e`, and `--- open` *url* `---` for a link, which a script never launches. *state* is one of `queued`, `inflight`, `sent`, `mailbox`, `delivered`, `rejected`, `failed`. An `until` that does not hold in time draws a last frame and exits 1.

## KEYS

In **tui**, the focus follows the pane. `Tab` and `Shift-Tab` cycle the conversations, the messages and the compose line. `?` (or `F1` from any pane) shows every key; the help, the footer and the palette are built from the same table the keys are.

In the messages, `y` or `Alt-w` copies what a drag selected (see MOUSE), or else the message under the cursor whole. `d` deletes the message under the cursor from this device after asking: `y` deletes it, any other key keeps it (see HISTORY).

In the conversations and the messages: `j`/`k`, the arrows or `Ctrl-n`/`Ctrl-p` move, `Ctrl-f`/`Ctrl-b`, `Space` and `PageDown`/`PageUp` page, `Ctrl-d`/`Ctrl-u` move half a page, `g`/`G`, `Home`/`End`, `Alt-<`/`Alt->` or `Ctrl-↑`/`Ctrl-↓` jump to the top or bottom. `Enter` opens the selected conversation; on a message it opens the message's detail view (see THE SCREEN). `i`, `a` or `r` start writing. `:` opens the command palette (`:q` and `Enter` quit), `Ctrl-r` fetches the mailbox outside a search, `q` quits. `/` searches (see SEARCH).

In the compose line, keys type. `Enter` sends to the open conversation (an empty line sends nothing). `Alt-Enter` and `Ctrl-j` break the line; where the terminal speaks the keyboard enhancement protocol, `Shift-Enter` breaks it too and `Ctrl-Enter` sends, and never breaks it. The compose pane's border names the two chords live in this terminal. The Emacs keys edit: `Ctrl-a`/`Ctrl-e` and `Home`/`End` the line's start and end, `Alt-<`/`Alt->` the body's, `Ctrl-p`/`Ctrl-n` and the arrows move a line, `Ctrl-b`/`Ctrl-f` a character, `Alt-b`/`Alt-f` a word, `Ctrl-d` deletes forward, `Alt-d` and `Alt-Backspace` delete a word, `Ctrl-k` kills to the end and `Ctrl-y` yanks. `Ctrl-c` clears the line into the kill buffer (`Ctrl-y` brings it back). `Esc` or `Ctrl-g` go back to the messages, and from there to the conversations. `PageDown`/`PageUp` page the messages.

**send_on_enter**: with `send_on_enter = false` in `$LNMSG_HOME/config`, or after the palette's `send on enter`, `Enter` breaks the line and `Alt-Enter` sends (`Ctrl-Enter` too, where the terminal can send it).

Everywhere: `Alt-x` opens the command palette, which matches what you type against the actions' names (`send`, `fetch`, `quit`, `help`, `details`, `next conversation`, `send on enter`, `mouse`, `theme`, `search`, `delete message`, `delete conversation`, `pin`, ...); a name typed whole, or its start, is offered first. `Alt-↓`/`Alt-↑` select the next or previous conversation, `Alt-Shift-↓`/`Alt-Shift-↑` the next or previous one with unread messages. `Ctrl-s` opens the search, or goes back to its field. `F5` fetches the mailbox, `Ctrl-l` redraws the screen, `Ctrl-q` quits. `Ctrl-c` quits nowhere; outside compose it says that `Ctrl-q` does.

## THE SCREEN

**The list.** Two rows a conversation, the most recent first: its name (see NAMES), the time of its last message (`14:05` today, the weekday this week, else the date), then the last line, `you:` before it when it was yours, and in brackets the count of messages not yet read. The selected conversation has a bar in the margin, reversed while the list has the focus. Pinned conversations (see HISTORY) stand first, above a dotted rule. The conversation's title says the peer, its trust in words (see NAMES) and the mailbox: the selected node, how it was chosen and when this session last fetched from it.

**A message** is a block: yours flush right, theirs flush left, at most three quarters of the pane wide, the text wrapped at a space and a line break kept, and the time closing the last line, for yours with its delivery mark (see DELIVERY MARKS). A centred date separates the days, and the top of the conversation says `nothing earlier on this device` where the history begins, or how many older messages it holds above what is loaded (see HISTORY). On a pane narrower than 40 cells yours are drawn flush left too, with `you` before the time. A message is drawn selected behind a bar while the conversation has the focus and the cursor is on it.

**Colour.** In colour your blocks are tinted blue and theirs grey, each a rectangle of its text with a cell of margin either side; the time takes a softer colour of its block, the date sits on a pill of its own between dashes, and the `new` rule takes an accent. A selected block takes a stronger tint; in 16 colours and without colour it is reversed. On a dark terminal in 16 colours their blocks are not tinted. Colour only reinforces: the side is where the block sits, the delivery state is the glyph, the unread count is a word.

**The `new` rule.** Where the messages begin that arrived while the conversation was not open, or while you were reading earlier ones, a full-width rule says how many (`2 new`). It stays until you leave the conversation. A message that arrives while you have scrolled up draws the rule above it and does not move what you are reading; `G` or `End` goes back to the newest, and from there the conversation follows what arrives.

**The detail view.** `Enter` on a message shows who wrote it to whom, the peer's address, the message's id in full, its size and its state, and what this device saw happen to it, across restarts, each with its time: queued, the leg tried, the router's states, the delivery proof, or the reason it failed. `Esc` or `q` closes it.

## NAMES

A peer is named the way its own client announces itself, followed by the first four hex digits of its address: `Alice ·4f2a`. Your own name for a peer, a petname from `$LNMSG_HOME/contacts`, wins over the announced one. A peer that has announced no name and has no petname is its address's first eight hex digits in angle brackets, `<9c31e0a2…>`. A name heard is kept in the history (see HISTORY) and names the peer after a restart, until it announces again.

A name off the network is drawn sanitised: control characters, direction overrides and invisible characters are removed, white space is one space, a glyph the screen uses for a meaning of its own (a delivery mark, `·`, `…`, `«`, `»`, `≈`, box drawing) is shown as `?`, and it is cut with `…` at 32 cells, a wide character counting two. What is forwarded or stored is not changed.

Two peers whose names look alike (the same letters ignoring case, full-width forms, and the Cyrillic, Greek and digit letters that look like Latin ones) both carry `≈` after the hash, and the title of an open one says `same name as ·b7d0`. A peer that announces a different name than before says so once in its conversation: `── was Alice, now announces Ally ──`.

The title's trust state is one of three words: `known`, a peer you named in `contacts`; `blocked`, one you blocked there; else `unknown`. Beside a petname the title also says what the peer announces (`known · announces Alice`). A blocked peer's messages are still shown; blocking is a state the title says, not yet a filter.

## SEARCH

`/` in the conversations or the messages, or `Ctrl-s` anywhere (`Ctrl-Shift-F` too, where the terminal speaks the keyboard enhancement protocol), turns the list into the search: its title says `Search` and `esc closes`, and the cursor is in its field at the top. As you type, the list under the field becomes the results, ignoring case: first the conversations whose name matches (the name as the list shows it, with its short hash, or the address for a peer with no name), then every message in the history whose text matches, each group with its count and the most recent first. A result is two rows, who and when over a preview, with the first match between `«` and `»`; a message you wrote reads `you to` its conversation. `no matches` says the query found nothing, and an empty field shows the plain list. Messages older than what a conversation has loaded are found in the history's index and listed among the others; opening one loads its conversation down to it.

`Tab` goes from the field to the results, where the moving keys and `n`/`N` step through them; `Ctrl-s`/`Ctrl-r` step them from the field too. `Enter` opens the selected result: a conversation at its newest message, a message's conversation at that message, selected, with the match marked in its block. The search stays open, every matching block of the open conversation is marked, and there `n`/`N` and `Ctrl-s`/`Ctrl-r` move to the next or previous matching message, round to the other end past the last. `Esc` in the conversation goes back to the results, and `Esc` (or `Ctrl-g`) in the results or the field closes the search: the list comes back, and the conversation keeps what it shows. While a search is open `Ctrl-r` is the previous match, not a fetch; `F5` fetches.

## HISTORY

**tui** keeps every conversation in `$LNMSG_HOME/history.sqlite3`: the messages both ways with their delivery marks and what their detail view lists, which conversations are pinned and how many messages are unread, and the names peers announced. A restart shows it all again, under the same address; another identity in the same directory has a history of its own.

A conversation opens with its newest 200 messages. Scrolling to the top loads 200 more, and the top line says how many the history still holds (`180 earlier on this device, scroll up for them`) until it reaches the first: `nothing earlier on this device`.

A message you sent that was still on its way when loquax ended is `○` again after the restart, with `loquax restarted: no longer being sent` in its detail view. Nothing sends it again; send it anew if it matters.

Message text is kept until you delete it. `d` on a message, then `y`, deletes it from this device; any other key keeps it, and a message still being sent cannot be deleted until it settles. The palette's `delete conversation` deletes the open conversation and its messages the same way, after asking, and `pin` puts the open conversation above the others, under a dotted rule, or takes it back down. Deleting is local: the peer keeps what you sent and what they sent you.

What **send** and **fetch** handle on the command line is not written to the history.

## MOUSE

In **tui** the mouse is captured unless **--no-mouse** says otherwise, and the palette's `mouse` switches it off and on. A click on a pane focuses it, and the wheel scrolls the pane under the pointer, whichever has the focus: the list a conversation a notch, the conversation three rows, the compose box a line. A click on a conversation's rows opens it; on its unread count, it opens at the `new` rule. A click on a message selects it where it is, without scrolling; on its delivery mark, it opens the detail view. A click in the compose box puts the cursor there. A click on `/ search` on the list's bottom border opens the search; a click on a result opens it, and one on the field puts the cursor back there.

Two clicks transmit, and they are words: `send` on the compose box's border sends, as `Enter` does, and the mailbox state in the conversation's title (or in the row under it on a wide screen) fetches, as `F5` does. Nothing else the mouse can reach transmits; the footer's `send` is a key to press, not a button.

A footer item runs its key's action, and the pointer over it highlights it. A click outside the palette or a detail view closes it; any click closes the help.

Drag across the conversation to select text, across lines and messages: the release copies the selected text, without the times and the marks, to the clipboard by OSC 52, which works over SSH where the terminal allows it, and says `copied N chars`. `y` copies the selection again.

A link in a message (`http`, `https` and `mailto` URLs, `lxmf@`*address*, a NomadNet page *address*`:/page/...`) is underlined. A click follows it as lnomad does: a web or mail link goes to the system's handler for it, an `lxmf@` address opens its conversation for writing, and a NomadNet page is copied, to open in lnomad. Any other scheme is refused.

Without capture the terminal's own selection works as before: drag to select, and the terminal's copy key copies.

## DELIVERY MARKS

`○` queued; a spinner while a link is built or the bytes go out; `◇` handed to the network, no proof back; `⌂` left in a propagation node's mailbox; `✓` a delivery proof came back; `✗` refused (red) or given up (dim). Only `✓` is green, and only a proof makes it. Each tone has a value per theme and colour tier; in 16 colours a mark takes its block's text colour, and without colour none. With **--ascii**: `.` `*` `o` `^` `v` `x`.

## EXIT STATUS

0 success; 1 operational failure; 2 argument error; 3 (**send**) a propagation node holds the message.

## ENVIRONMENT

**LNMSG_HOME**
:   The state directory (identity, history, mailbox choice, seen record, client storage). Default: `$XDG_CONFIG_HOME/loquax`, else `~/.config/loquax`. The program was called lnmsg before; when only the old `lnmsg` directory exists there, the first start moves it to `loquax` and says so on stderr. Setting this variable skips that move.

**LNMSG_DISPLAY_NAME**
:   The sender name when **--from** is not given.

**LEVICULUM_EVENT_LOG**
:   Append the run's structured events, including the message id, to this file.

**NO_COLOR**
:   Set and not empty: **tui** draws without colour, as **--no-color**.

**COLORTERM**, **TERM**
:   Read by **--color** `auto`.

## FILES

`$LNMSG_HOME/identity` is your address; back it up. `$LNMSG_HOME/history.sqlite3` is the history (see HISTORY), an SQLite file that `sqlite3` reads; a backup of it is a copy taken while loquax is not running. **tui** refuses to start over a history it cannot read whole (cut short, not SQLite, or written by a later loquax) and names the file: move it aside to start with an empty history, or put a backup in its place. It never starts empty over one. **tui** writes its warnings to `$LNMSG_HOME/tui.log` instead of the screen, with a line for each step of each send leg (`SEND`), each path answer (`PATH`) and each call to the daemon (`RPC`); and once at start a `KEYS` line naming the keyboard and its live chords (`KEYS enhancement=no newline=Alt-Enter send=Enter`); **RUST_LOG** replaces its filter (default `warn,loquax=info`). `$LNMSG_HOME/config` holds `key = value` lines: `propagation_node` (see **--pn**), `send_on_enter` (`true`, the default, or `false`) and `theme` (`auto`, the default, `dark` or `light`; see **--theme**). `$LNMSG_HOME/contacts` holds your own word on peers, one a line, `#` a comment: `name` *address* *petname* (the rest of the line is the name) and `block` *address*; a line that is neither stops **tui** with the file and line named. See NAMES.

## SEE ALSO

**lnsd**(1), **lnpnd**(1), **lnomad**(1)
