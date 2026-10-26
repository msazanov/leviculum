# lnmsg: keys, mouse and layout

Part of the [lnmsg design record](lnmsg.md). The second decision round on
the screen, after the input model of 2026-08-08 and the delivery display
of 2026-08-10 ([the user interface](lnmsg-ui.md)). Those stand: focus
follows the pane, a command palette, no modes, `Enter` in compose the one
keystroke that transmits. What this page adds is the full key table, the
mouse, and the layout, and the order they get built in.

**Status: proposal, 2026-10-26.** The principles in section 1 are Lew's
and decided. The table in section 3, the four changes to slice one it
asks for (section 3, "Four changes to what slice one does"), the layout
in section 5 and the slice order in section 7 are this page's proposal,
for Lew to confirm before any slice is ordered.

## 1. The principles (Lew, 2026-10-26)

`lnomad` is operable with the mouse like a classic browser, with the
browser's keyboard, and with the typical Emacs and vi commands, all at
once: one keymap whose rows carry the vi, Emacs and arrow spellings of the
same motion side by side (`SCROLL_KEYS`, `lnomad/src/app/event.rs:1983`;
the mechanism is described under [architecture](lnmsg-architecture.md),
section 1). `lnmsg` takes the principle over, with three substitutions:

- **Not the browser's keys, but the mail programs' and the chat
  programs'**, where a convention exists. A mutt user and a Signal user
  each arrive with habits, and the habits are the reference.
- **Emacs and vi keys are mandatory.** Where a chat convention and an
  Emacs or vi key want the same chord, Emacs or vi wins, and the chat
  action gets another key, a palette name, and a mouse target.
- **The mouse is a first-class citizen**, as the proposal on
  [the user interface](lnmsg-ui.md) page already asked: everything on
  the screen that looks clickable is.

And the layout: what Signal Desktop and WhatsApp Desktop look like and how
they work, rebuilt in text mode.

The test for the table is that a Signal user, a mutt user and a vi user
each find their habit on the first try, and that the one who does not
find it is told where it went.

## 2. The key families, surveyed

Each program's documented defaults, read from its source or its manual on
2026-10-26.

**mutt**, the default bindings in its `functions.h`
(<https://gitlab.com/muttmua/mutt/-/raw/master/functions.h>, master):
index `j`/`k` and `↓`/`↑` next and previous message, `Enter` (also
`Space`) display, `r` reply, `g` group reply, `L` list reply, `m` mail,
`f` forward, `d` delete, `u` undelete, `s` save, `c` change folder, `/`
search and `n` search next, `Esc /` reverse search, `Home`/`=` and
`End`/`*` first and last, `z`/`Z` page, `Tab` next new-then-unread, `t`
tag, `:` command line, `?` help, `q` quit, `Ctrl-L` redraw. Its line
editor is Emacs-shaped: `Ctrl-A`/`Ctrl-E`, `Ctrl-B`/`Ctrl-F`,
`Alt-B`/`Alt-F`, `Ctrl-D` delete forward, `Ctrl-K` kill to end.

**aerc**, the shipped `config/binds.conf`
(<https://github.com/rjarry/aerc/blob/master/config/binds.conf>, master):
messages `j`/`k`, `Ctrl-D`/`Ctrl-U` half and `Ctrl-F`/`Ctrl-B` full page,
`g`/`G` top and bottom, `Enter` view, `rr` reply all and `Rr` reply,
`C` and `m` compose, `/` search with `n`/`N`, `d` delete after a
confirmation and `D` without, `a` archive, `q` quit, `Ctrl-N`/`Ctrl-P`
next and previous tab.

**Signal Desktop**, the shortcut guide's own table
(`ts/components/ShortcutGuide.dom.tsx` in
<https://github.com/signalapp/Signal-Desktop>, main, last changed
2026-10-13): `Alt-↑`/`Alt-↓` and `Ctrl-Shift-Tab`/`Ctrl-Tab` previous and
next conversation, `Alt-Shift-↑`/`Alt-Shift-↓` previous and next unread,
`Ctrl-1`..`9` conversation by index, `Ctrl-N` new conversation, `Ctrl-F`
search, `Ctrl-Shift-F` search in the conversation, `Ctrl-Shift-T` focus
the composer, `Ctrl-↑`/`Ctrl-↓` scroll to top and bottom, `Enter` the
selected message's default action, `Ctrl-D` its details, `Ctrl-Shift-R`
reply to it, `Ctrl-Shift-D` delete, `Shift-Enter` newline, `Ctrl-Enter`
send in the expanded composer, `Ctrl-U` attach a file, `Ctrl-,`
preferences.

**WhatsApp Desktop** publishes no source, and its help page on
shortcuts did not resolve on 2026-10-26, so it is read from secondary
lists (XDA Developers, "WhatsApp keyboard shortcuts", updated
2023-02-15, <https://www.xda-developers.com/whatsapp-keyboard-shortcuts/>),
and only what those agree on and Signal shares is taken: `Ctrl-N` new
chat, `Ctrl-Tab`/`Ctrl-Shift-Tab` next and previous chat, `Ctrl-F`
search, `Ctrl-Shift-F` search in the chat, `Esc` closes the chat, `Enter`
sends, `Shift-Enter` breaks the line.

**Emacs**, the GNU Emacs reference card
(`etc/refcards/refcard.tex` in
<https://git.savannah.gnu.org/cgit/emacs.git>): `Ctrl-N`/`Ctrl-P` line,
`Ctrl-F`/`Ctrl-B` character, `Alt-F`/`Alt-B` word, `Ctrl-A`/`Ctrl-E` line
start and end, `Alt-<`/`Alt->` buffer start and end, `Ctrl-V`/`Alt-V`
page, `Ctrl-S`/`Ctrl-R` incremental search forward and back, `Ctrl-G`
abort, `Ctrl-L` redraw, `Ctrl-D` delete forward, `Alt-DEL`/`Alt-D` kill
word back and forward, `Ctrl-K` kill to end of line, `Ctrl-Y` yank,
`Alt-W` copy, `Alt-X` run a command by name, `Ctrl-X Ctrl-C` exit.

**vi**, the POSIX specification
(<https://pubs.opengroup.org/onlinepubs/9799919799/utilities/vi.html>):
`j`/`k` line, `Ctrl-F`/`Ctrl-B` page, `Ctrl-D`/`Ctrl-U` half page, `G`
last line, `/` and `?` search forward and back, `n`/`N` repeat in the
same and the opposite direction, `i`/`a` insert and append, `y` yank,
`ZZ` exit, `:` the ex command line. `gg` for the first line is Vim's, not
POSIX's; `lnomad` already binds single `g` for it.

The families agree more than they differ: `j`/`k`, `/`, `n`, `Enter`,
`q`, `?` and `:` mean the same thing in mutt, aerc, vi and `lnomad`, and
the Emacs motions are what mutt's line editor and every readline already
do. The real collisions are between the chat programs and Emacs, because
both want `Ctrl-` letters: `Ctrl-N` (new conversation against next line),
`Ctrl-F` (search against forward), `Ctrl-D` (details against delete or
half page), and between vi's `?` (search back) and everybody else's `?`
(help).

## 3. The lnmsg keymap

One row per action. The panes are **L** the conversation list, **M** the
messages, **C** compose, **S** the search field, and **all** means every
pane outside an open overlay (help, palette, detail view). `Alt` is
Emacs's `M-`. A chord marked *(enh.)* exists only where the terminal
speaks the keyboard enhancement protocol, because a legacy terminal sends
the same byte for it as for the plain key (`Shift-Enter` and `Enter`,
`Ctrl-Tab` and `Tab`, `Ctrl-Shift-F` and `Ctrl-F`); every such row has a
spelling that works everywhere. **TX** marks an action that puts bytes
on the air.

### Moving

| Action | vi | Emacs | mail, chat | mouse | where |
|---|---|---|---|---|---|
| next / previous item | `j` / `k` | `Ctrl-n` / `Ctrl-p` | `↓` / `↑` (mutt, aerc `j`/`k`) | wheel | L M; in C the same chords move a line of the body, and `j`/`k` type |
| page down / up | `Ctrl-f` / `Ctrl-b`, `Space` | `Ctrl-v` / `Alt-v` | `PageDown` / `PageUp` (aerc `Ctrl-f`/`Ctrl-b`) | wheel | L M; `PageDown`/`PageUp` also from C, scrolling M |
| half a page | `Ctrl-d` / `Ctrl-u` | | (aerc the same) | | L M |
| first / last | `g` / `G` | `Alt-<` / `Alt->` | `Home` / `End` (mutt); `Ctrl-↑` / `Ctrl-↓` (Signal) | | L M; in C `Alt-<`/`Alt->` are body start and end |
| jump to a hint or a name | `f`, then the label or part of the name | | | | L M (`lnomad`'s hint mode) |

The first four rows are `lnomad`'s `SCROLL_KEYS` rows unchanged, read from
the shared table (section 4); the `Ctrl-↑`/`Ctrl-↓` spelling is the one
chord `lnmsg` adds to them.

### Conversations

| Action | vi | Emacs | mail, chat | mouse | where |
|---|---|---|---|---|---|
| open the selected conversation | `Enter` | | `Enter` (mutt, aerc, Signal) | click the row | L |
| previous / next conversation | | | `Alt-↑` / `Alt-↓` (Signal); `Ctrl-Shift-Tab` / `Ctrl-Tab` *(enh.)* (Signal, WhatsApp) | click a row | all |
| previous / next unread | | | `Alt-Shift-↑` / `Alt-Shift-↓` (Signal) | click an unread badge | all |
| new conversation | | | `m`, `C` (mutt, aerc) | click `+` in the list's title | L M |
| write in this conversation | `i`, `a` | | `r` (mutt, aerc reply); `Ctrl-Shift-T` *(enh.)* (Signal) | click the compose box | L M |
| search | `/` | `Ctrl-s` | `Ctrl-Shift-f` *(enh.)* (Signal, WhatsApp) | click `/ search` | `/` L M; `Ctrl-s` all |
| next / previous match | `n` / `N` | `Ctrl-s` / `Ctrl-r` | (mutt, aerc `n`) | click a result | S, and L M while a search is open |

### Messages

| Action | vi | Emacs | mail, chat | mouse | where |
|---|---|---|---|---|---|
| the message's detail view and ledger | `Enter` | | `Enter` (mutt display, Signal default action) | click its delivery mark | M |
| copy the message's text | `y` | `Alt-w` | | drag selects, release copies (OSC 52) | M |
| follow a link in the body | `f`, then its label | | | click the link, as in `lnomad` | M |
| delete from this device | `d`, then confirm | | `d` (mutt; aerc asks first) | | M, from the history slice on |

### Compose

| Action | vi | Emacs | mail, chat | mouse | where |
|---|---|---|---|---|---|
| send **TX** | | | `Enter` (Signal, WhatsApp); `Ctrl-Enter` *(enh.)* (Signal) | click `send` on the box's border | C |
| line break | | `Ctrl-j` | `Alt-Enter`; `Shift-Enter` *(enh.)* (Signal, WhatsApp) | | C |
| line start / end | | `Ctrl-a` / `Ctrl-e` | `Home` / `End` | click places the cursor | C |
| character, word | | `Ctrl-f` / `Ctrl-b`, `Alt-f` / `Alt-b` | `→` / `←`, `Ctrl-→` / `Ctrl-←` | | C |
| delete forward, word back, word forward | | `Ctrl-d`, `Alt-Backspace`, `Alt-d` | `Delete`, `Ctrl-w` (readline) | | C |
| kill to end, yank | | `Ctrl-k`, `Ctrl-y` | | | C |
| clear to a recoverable draft | | | `Ctrl-c` (decided 2026-08-08; `Ctrl-y` brings it back) | | C |
| leave compose | `Esc` | `Ctrl-g` | `Esc` | click another pane | C |

### Everywhere

| Action | vi | Emacs | mail, chat | mouse | where |
|---|---|---|---|---|---|
| next / previous pane | | | `Tab` / `Shift-Tab` | click the pane | all |
| back out, cancel, close | `Esc` | `Ctrl-g` | `Esc` (WhatsApp closes the chat) | click outside the overlay | all: C to M to L, and any overlay closes |
| command palette | `:` | `Alt-x` | `Ctrl-p` (the editors' palette, not a chat habit) | | `:` and `Ctrl-p` L M; `Alt-x` all |
| help | `?` | | `F1` | click `? help` in the footer | `?` L M; `F1` all |
| fetch the mailbox **TX** | | | `F5`; `Ctrl-r` (`lnomad`'s reload) | click the mailbox state in the header | `F5` all; `Ctrl-r` L M |
| redraw | | `Ctrl-l` | (mutt `Ctrl-l`) | | all |
| quit | `:q`, `q` | `Ctrl-x Ctrl-c` | `Ctrl-q` | click `quit` in the footer | `q` L M; the rest all |
| mouse capture off and on | | | palette `mouse`, `--no-mouse` | | all |

### How the collisions are settled

Per pane, in this order, and every loser keeps a palette name and a
mouse target:

- **In compose, plain keys type**, and so do `Shift`-letters. Every
  single-letter row above is L and M only.
- **In compose, Emacs editing wins every `Ctrl` letter it has**: `Ctrl-n`
  and `Ctrl-p` move a line, `Ctrl-f`/`Ctrl-b` a character, `Ctrl-d`
  deletes, `Ctrl-k`/`Ctrl-y` kill and yank, as 994 built the last two. The
  palette from compose is `Alt-x`, or `Esc` and then `:`.
- **In the command panes, the shared scroll rows win**, because they are
  `lnomad`'s and a reader of both programs must not learn two meanings:
  `Ctrl-f` pages and `Ctrl-d` half-pages there, as in vi and aerc.
- **The chat programs' `Ctrl` letters lose to both**: Signal's and
  WhatsApp's `Ctrl-N` (new conversation) is Emacs's next line, their
  `Ctrl-F` (search) is a page in L M and a character in C, Signal's
  `Ctrl-D` (details) is vi's half page. They go to `m`/`C`, `/`/`Ctrl-s`
  and `Enter`. The chords that do not collide are taken as they are:
  `Alt-↑`/`Alt-↓`, `Alt-Shift-↑`/`Alt-Shift-↓`, `Ctrl-↑`/`Ctrl-↓`,
  `Shift-Enter`, `Ctrl-Enter`, `Ctrl-Tab`, `Ctrl-Shift-F`, `Esc`.
- **vi's `?` is help**, as in `lnomad`, mutt and aerc; backward search is
  `/` and then `N`.
- **mutt's `Tab`** (next unread) stays the pane cycle decided on
  2026-08-08; next unread is Signal's `Alt-Shift-↓`. mutt's `g` (group
  reply) and `c` (change folder) have no counterpart, since LXMF has no
  groups and `lnmsg` no folders, so `g` stays vi's top.
- **The quiet keyboard holds**: `Enter` in compose is the only single
  keystroke marked TX. Fetching needs `F5`, a `Ctrl` chord or a click, and
  `lnomad`'s bare `R` reload is deliberately not taken over, because in
  `lnmsg` it would be a single keystroke that transmits.

A lost habit is not silent: the help overlay lists, under each row, the
chat program's key that went elsewhere ("`Ctrl-N` in Signal: here `m`"),
and the palette finds the action by the chat program's word too (`new
chat`, `find`).

**`:q` quits for free.** The palette's fuzzy match is a subsequence test
(`fuzzy_match`, `leviculum-ui/src/commands.rs:57`), and `quit` is the only
palette name containing a `q`, so `:`, `q`, `Enter` quits as a vi user
expects. That stays true only while it is tested, so the keymap slice
pins it.

### Four changes to what slice one does

The table asks four things of slice one (lnmsg-ui.md, "Built 2026-10-25,
slice one") that Lew decides before the keymap slice is ordered:

1. **`Enter` on a message opens its detail view**, as in mutt and Signal;
   today it moves to compose (`command_pane_key`, `lnmsg/src/ui.rs:450`).
   Compose is then `i`, `a`, `r`, `Tab` or a click.
2. **`Ctrl-p` and `Ctrl-n` in compose become Emacs line motions**, now
   that a body has lines (1002); today `Ctrl-P` opens the palette from
   every pane (`lnmsg/src/ui.rs:407`). The palette from compose
   is `Alt-x`.
3. **`Ctrl-c` quits nowhere.** Today it quits outside compose
   (`lnmsg/src/ui.rs:365-372`); the reflex that throws a draft away in
   compose copies or quits elsewhere by habit, and a messenger that
   vanishes on a copy reflex loses the screen. Pressed outside compose it
   shows a toast, `Ctrl-q quits`.
4. **The `send_on_enter` switch's email style swaps `Enter` and
   `Alt-Enter`** (`Enter` breaks the line, `Alt-Enter` sends) instead of
   sending on `Ctrl-D`, which Emacs needs as delete forward.

### What the terminal can and cannot send

- `Ctrl-j` is the line break every terminal can send: in raw mode a
  terminal's `0x0A` reaches crossterm 0.29 as `Ctrl-J`, not as `Enter`
  (`src/event/sys/unix/parse.rs` in crossterm, branch 0.29: the `b'\n'`
  arm applies only outside raw mode), so it needs no enhancement.
- `Alt-f`, `Alt-b` and `Alt-d` do not edit today. The editing table
  (`input_request`, `leviculum-ui/src/input.rs:288`) accepts them with
  `META` only, which a terminal reports under the enhancement protocol and
  not otherwise, while the frontend passes crossterm's `ALT` through as
  `ALT`; and it maps `Meta-d` to deleting the word *back*, where Emacs
  kills the word forward. `Ctrl-d` (delete forward) is not in it at all.
  The keymap slice fixes the three, in the shared table, so `lnomad`'s
  address bar gets them too.
- `Ctrl-a` and `Home` go to the start of the whole body, not of its line,
  since the body became multi-line (`GoToStart` over one buffer); the
  keymap slice makes them line motions and `Alt-<` the body's start.
- `Ctrl-s` reaches the program: raw mode clears `IXON`, so the terminal
  does not take it as flow control.
- The enhancement rows need `PushKeyboardEnhancementFlags` where
  crossterm reports support; `lnmsg tui` does not push it yet
  (lnmsg-ui.md, slice one). The keymap slice does, and pops it on exit.

## 4. One table, four readers

The decision of 2026-08-08 asked for the keymap as one table read by the
key handler, the help overlay, the palette and the footer hints. Today it
is not: `lnmsg`'s keys are a `match` in `on_key` (`lnmsg/src/ui.rs:357`)
and `command_pane_key` (`lnmsg/src/ui.rs:450`) beside a hand-written help text, and `lnomad`'s
`SCROLL_KEYS` is private to `lnomad` (`pub(crate)`,
`lnomad/src/app/event.rs:1983`) and typed over crossterm's `KeyCode`, so
`lnmsg` cannot read it. Its rows also carry the keys twice, once as the
displayed `keys` string and once as the `chords`, so the help can still
drift from the handler inside one row.

The keymap slice therefore does three things in `leviculum-ui`, which is
terminal-free and already owns the key types (`leviculum-ui/src/input.rs`)
and the palette names (`leviculum-ui/src/commands.rs`):

1. **The scroll rows move there**, over `leviculum_ui::input::KeyCode`, and
   `lnomad` reads them from there; its help overlay and its key handler do
   not change behaviour, which `lnomad`'s own tests and its golden files
   hold.
2. **A row is one action with its chords, and the displayed keys are
   derived from the chords**, never typed beside them. A chord carries the
   panes it applies in, so the per-pane rules of section 3 are data and
   not `if` order.
3. **`lnmsg` adds its rows** beside the shared ones: the conversation,
   message, compose and global rows of section 3, each with its palette
   name (the `Command` enum and `COMMANDS` grow to the full list), its
   footer label if it has one (`FooterItem`, `leviculum-ui/src/footer.rs`),
   its help group, the lost-habit note, and the TX flag.

Readers: the handler resolves a key by scanning the rows for the focused
pane; the help overlay is built from the rows (`HelpGroup`,
`leviculum-ui/src/help.rs`); the footer takes the rows that carry a label
for the focused pane; the palette lists every row's name and shows its
first chord. The desktop GUI reads the same table.
The keymap and the palette are shared with lnflash's GUI once it exists
(Codeberg #476), so a flashing action is a palette entry there, not a
new grammar.

What makes it testable, and what the slice's unit tests hold:

- no chord resolves to two actions in one pane (no shadowing, which
  `lnomad`'s first-match scan cannot see);
- every row appears in help, and every palette name belongs to a row;
- the quiet keyboard: the only row with the TX flag and a single
  keystroke is send-in-compose (the `Enter` chord);
- every *(enh.)* chord's row has a chord that is not *(enh.)*;
- `:` `q` `Enter` quits.

## 5. The layout: a modern chat app in text mode

Signal Desktop and WhatsApp Desktop share one shape: a conversation list
on the left with name, last-line preview, time and an unread badge, the
open conversation on the right with a header naming the peer, messages as
blocks with ours on the right and theirs on the left, a time and a
delivery mark on each, a date between days, a "new messages" line where
the unread begins, and a compose box at the bottom that grows with the
text. Here it is in cells.

### 80 x 24

```text
┌─ Chats ────────────────┐┌─ Alice ·4f2a  known ───────────── mailbox ⌂ 4 min ─┐
│/ search                ││                                                    │
│                        ││                                                    │
│▌Alice ·4f2a      14:05 ││             ── Saturday 24 October ──              │
│▌ok, bring the T114.    ││ are you at the field site tomorrow?  13:58         │
│ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ││                                                    │
│ Bob ·b7d0        12:40 ││                yes, from ten. Shall I bring the    │
│ you: the antenna is…   ││                T114 and the long antenna?  14:02 ✓ │
│                        ││                                                    │
│ <9c31e0a2…>        Fri ││                    ── Today ──                     │
│ hello? is this th… [1] ││                                 on my way  09:12 ⌂ │
│                        ││                                                    │
│ Carol ·17ee     22 Oct ││ ──────────────────── 2 new ─────────────────────── │
│ you: thanks, merged    ││ ok, bring the T114.  14:04                         │
│                        ││                                                    │
│                        ││ and the small tripod                               │
│                        ││ if it fits in the bag  14:05                       │
│                        ││                                                    │
│                        ││                           the tripod fits  14:06 ⠹ │
│                        │├─ to Alice ──────── Enter send · Alt-Enter newline ─┤
│                        ││ sure, and I will charge                            │
│                        ││ the batteries tonight▏                             │
└────────────────────────┘└────────────────────────────────────────────────────┘
 Tab focus  Alt-↑↓ chat  / search  : commands  ? help  Ctrl-Q quit              
```

### 120 x 40, with a search open

```text
┌─ Search ──────────── esc closes ─┐┌─ Alice ·4f2a  known ────────────────────────── 4f2a91c03be8d7e15a60f2b44c9d0e71 ─┐
│ / tripod▏                        ││ direct, 2 hops · heard 14:05             mailbox 9c31e0a2 adopted · synced 14:03 │
│                                  ││ ──────────────────────────────────────────────────────────────────────────────── │
│ CONVERSATIONS                  2 ││                                                                                  │
│  Alice ·4f2a               14:06 ││                                                                                  │
│  you: the «tripod» fits          ││                                                                                  │
│                                  ││                                                                                  │
│  Bob ·b7d0                 3 Oct ││                                                                                  │
│  «tripod» stays in the van       ││                                                                                  │
│                                  ││                                                                                  │
│ MESSAGES                       3 ││                          nothing earlier on this device                          │
│ ▌Alice                     14:05 ││                             ── Friday 23 October ──                              │
│ ▌and the small «tripod» if it…   ││ did the board come back from the soak?  19:40                                    │
│                                  ││                          yes, 100 % over the night, log is on the share  19:52 ✓ │
│  you to Alice              14:06 ││                                                                                  │
│  the «tripod» fits               ││                            ── Saturday 24 October ──                             │
│                                  ││ are you at the field site tomorrow? Dana says the north                          │
│  Bob                       3 Oct ││ gate is locked after six, so the car park by the road it is.  13:58              │
│  the «tripod» stays in the van,  ││                                                                                  │
│                                  ││                           yes, from ten. Shall I bring the T114 and the long     │
│                                  ││                           antenna, or is the stick enough for the hill?  14:02 ✓ │
│                                  ││                                                                                  │
│                                  ││ long one, the hill eats signal  14:03                                            │
│                                  ││                                                                                  │
│                                  ││                                   ── Today ──                                    │
│                                  ││                                                               on my way  09:12 ⌂ │
│                                  ││                                             stuck at the level crossing  09:31 ◇ │
│                                  ││                                                                                  │
│                                  ││ ────────────────────────────── 2 new ─────────────────────────────────────────── │
│                                  ││ ok, bring the T114.  14:04                                                       │
│                                  ││                                                                                  │
│                                  ││ and the small «tripod»                                                           │
│                                  ││ if it fits in the bag  14:05                                                     │
│                                  ││                                                                                  │
│                                  ││                                                       the «tripod» fits  14:06 ✓ │
│                                  ││                                                          leaving in ten  14:07 ○ │
│                                  │├─ to Alice ─────────────────────── Enter send · Alt-Enter newline · M-x commands ─┤
│                                  ││ ▏                                                                                │
└──────────────────────────────────┘└──────────────────────────────────────────────────────────────────────────────────┘
 Enter open  n / N next, previous  Ctrl-S / Ctrl-R next, previous  Tab to the results  Esc close search                 
```

### The elements

**The conversation list.** Two rows a conversation: the name with its
short hash and the time of the last message, then the last line's preview,
prefixed `you:` when it was ours, and an unread count in brackets, drawn
reversed. The name follows the display rule of
[the mailbox page](lnmsg-mailbox.md) (petname, else announced name,
always with a short hash); until names arrive it is the address in angle
brackets. Most recent first. Pinned conversations, if we want them, sit
on top above a dotted rule; pinning is a palette action (`pin`), stored
with the conversation, and needs the history slice. The selected row has
the bar in the left margin; under focus it is drawn reversed.

**The header** is the open conversation's title line: the peer's name and
short hash, its trust state in words (`known`, `unknown`, `blocked`, the
three of the mailbox page, never a glyph alone), and on the right the
mailbox state, the same words the `mailbox:` line prints
(`lnmsg/src/mailbox_state.rs`). At 120 columns it gains the full address
in the title and a second row: how the peer is reached (`direct, 2 hops`,
from the path table) and when it was last heard, and the mailbox's node,
selection and last sync.

**The conversation pane.** Ours are blocks set flush right, theirs flush
left, a block at most three quarters of the pane wide so the two sides
read apart; the text inside a block is left-aligned and wrapped at a
space. A multi-line body is drawn as its lines, each wrapped, as one
block. The time and, for ours, the delivery glyph of
[the delivery table](lnmsg-ui.md) close the block's last line, so a mark
always sits at the same place relative to its text. Between days a
centred date (`Today`, then weekday and date). Where the unread begins, a
full-width rule with the count (`2 new`), which stays until the
conversation is left. Below a pane width of 40 cells the right alignment
stops paying for itself, and ours are drawn flush left with `you` before
the time instead. The top of what the device holds says so (`nothing
earlier on this device`), because until the history slice that is the
start of this session.

**The compose box** sits under the messages with the conversation's name
on its border and the hint for send and line break on the right. It shows
as many rows as the body has lines, up to five, then scrolls (1002). The
attachment and the QR code of the paper message are palette actions
(`attach`, `qr`) until they earn a key.

**Search** is the list pane's top row. `/` or `Ctrl-s` puts the cursor
there; as the query is typed the list becomes the results, conversations
whose name matches and then messages whose text does, each with the match
marked. Moving through the results opens the conversation on the right,
scrolled to the hit, with the hit marked in the block; `Enter` keeps it
open and closes the search, `Esc` closes the search and returns to where
the reader was. Until the history slice it searches what this session
holds; with history it is the FTS5 index of [storage](lnmsg-storage.md).

**The footer** is the shared footer strip (`leviculum-ui/src/footer.rs`),
filled from the keymap rows that carry a label for the focused pane, so
it changes with the focus and drops low-priority hints first on a narrow
screen.

### What the mouse does

| Element | Click | Wheel | Other |
|---|---|---|---|
| a pane | focuses it | scrolls the pane under the pointer, not the focused one | |
| a conversation row | selects and opens it | | |
| an unread badge | opens the conversation at its `new` line | | |
| `+` in the list's title | new conversation | | |
| `/ search` | focuses the search field | | |
| a message | selects it | | drag selects text across lines and blocks, release copies by OSC 52 (lnmsg-ui.md, "The mouse as a first-class citizen") |
| a delivery mark | opens the message's detail view and ledger | | |
| a link in a body | follows it, as in `lnomad` | | |
| the compose box | focuses it and places the cursor | scrolls the body | |
| `send` on the box's border | sends (TX) | | |
| the mailbox state in the header | fetches (TX) | | |
| a footer item | runs its row | | hover highlights, as in `lnomad` |
| outside an overlay | closes it | | |

The two TX targets are labelled with their word, never a bare glyph, so
a click that transmits is as deliberate as the `Enter` it stands for.

## 6. What is deliberately not copied from the phone apps

- **Read receipts** beyond the LXMF proof. The proof says the bytes
  reached the destination identity; nothing on the wire says a human read
  them (`leviculum-lxmf/src/constants.rs:60-86` has no such field), and
  the delivery decision of 2026-08-10 already forbids implying one. No
  second check mark, no blue ticks.
- **Typing indicators.** No LXMF field carries one, and sending one would
  be a transmission per keystroke, which the quiet keyboard rules out on
  its own.
- **Reactions.** No field the deployed clients agree on carries them;
  a reaction would be a whole message that peers render as text. If
  Sideband, columba or NomadNet settle on a field, it is a row in the
  table then, not a guess now.
- **Online and last-seen presence.** What the mesh knows is the last
  announce and the path, and the header says exactly that ("heard 14:05",
  "direct, 2 hops"); "online" would round an announce up to a person.
- **Edit and delete for everyone.** Once sent, an LXMF message is the
  peer's; a local delete is offered, a remote one would be a promise the
  protocol cannot keep.
- **Avatars and emoji pickers.** Cells are better spent on text; emoji
  arrive and are drawn as the terminal draws them.

## 7. The slices after 1002

Dated 2026-10-26. Order and dependencies only. Each has its acceptance,
and where it changes what goes over the wire or what the screen shows of
a real exchange, a phase in the 1001 cell (periculum's
`regression/lnmsg_tui_chat_session.toml`, the `lnmsg_tui_chat` step: `lnmsg
tui` against the stock LXMF client on one `lnsd`). The order differs from
the one in the order text in one place: the mouse comes after the layout,
because its targets (the delivery mark, the badge, the blocks) are the
layout's.

1. **The keymap table and help from it.** Section 4: the shared rows move
   to `leviculum-ui`, `lnomad` reads them, `lnmsg`'s rows join, help,
   footer and palette derive from the table; the four changes of section
   3 if Lew takes them; `Alt-f`/`Alt-b`/`Alt-d`/`Ctrl-d`/`Ctrl-j` in the
   shared editing table; line-wise `Ctrl-a`/`Ctrl-e`; the keyboard
   enhancement pushed where supported. Depends on nothing open.
   *Acceptance:* the unit tests listed in section 4; `lnomad`'s tests and
   golden files unchanged; `tui_snapshot` gains a session driven only by
   vi spellings and one only by Emacs spellings that reach the same
   frames. *1001 phase:* Alice's own three-line body, typed with
   `Ctrl-j`, arrives at Bob as three lines.
2. **The chat layout.** Section 5 without the mouse: list rows with
   preview, time and badge; blocks aligned by side, date separators, the
   `new` line, the narrow fallback; the header with trust (every peer
   `unknown` until slice 5) and mailbox state; the per-message detail
   view with this session's ledger, behind `Enter` on a message. Depends
   on 1 (the `Enter` row). *Acceptance:* snapshots at 80 x 24 and 120 x 40
   whose structure is the mockups'; unit tests over `view` for alignment,
   wrapping and the `new` line. *1001 phase:* Bob's reply arriving while
   Alice reads earlier messages draws the `new` line above it, and her
   detail view of the first message lists the proof.
3. **The mouse.** The table of section 5; drag selection and OSC 52;
   `--no-mouse` and the palette toggle. Depends on 2. *Acceptance:* unit
   tests feeding mouse events to the model; the key script language gains
   `click` and `wheel` steps and `tui_snapshot` pins a session clicking a
   conversation, a mark and `send`. *1001 phase:* none of its own; the
   send by click is the same send, so a script variant of phase 1 that
   sends with `click` on `send` is enough.
4. **Search.** The search field, results over conversations and this
   session's messages, `n`/`N`, `Ctrl-s`/`Ctrl-r`, the hit marked in
   place. Depends on 2 (the results take the list pane). *Acceptance:* unit
   tests over the matcher and the result order; a snapshot of the 120 x 40
   search mockup's structure. *1001 phase:* Alice searches a word of Bob's
   reply and lands on that message.
5. **Names from announces.** The display rule, sanitising, the collision
   and name-change marks, petnames, and with them the three trust states
   in the header. Depends on 2 (where names are drawn); the library side
   is there, `RouterEvent::PeerAnnounced` closed gap 1 of the triage on
   [architecture](lnmsg-architecture.md) on 2026-08-11, and `lnmsg`
   already announces its own name (`lnmsg/src/display_name.rs`). *Acceptance:* unit tests for
   sanitising (control characters, glyph look-alikes, width cap) and the
   collision mark. *1001 phase:* Bob's client announces a display name and
   Alice's list and header carry it with its short hash.
6. **History.** The SQLite store of [storage](lnmsg-storage.md),
   conversations and messages across restarts, the ledger persisted,
   search over FTS5, local delete, pinning, and the restart discontinuity
   shown honestly (a message queued at exit is `○` again after restart).
   Depends on the storage decision's implementation, on 4 for FTS5, and
   on 2 for where history is drawn. *Acceptance:* tests over the store's
   durability rule (atomic writes, corrupt is not missing); a restart in
   `tui_snapshot`. *1001 phase:* Alice's `lnmsg tui` restarted mid-session
   shows the earlier exchange with its marks.
