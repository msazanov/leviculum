# lnmsg(1)

## NAME

lnmsg -- terminal LXMF messenger for Reticulum

## SYNOPSIS

**lnmsg send** [*options*] *address* [*body*]
**lnmsg fetch** [*options*]
**lnmsg address**
**lnmsg tui** [**--to** *address*]... [*options*]

## DESCRIPTION

**lnmsg** sends and receives LXMF messages over Reticulum. It attaches to a shared instance that is already running -- **lnsd**(1), or Python's **rnsd** -- and runs its own LXMF router; it never starts a Reticulum stack of its own. Its address is a pure function of the identity in its state directory, minted on first use and never replaced silently.

**send** queues one message. The body is the argument, or standard input when the argument is missing or `-`, one trailing newline removed. A send that worked prints nothing and exits 0, which means the message was queued and handed to the network, not that it was delivered. **--via auto**, the default, tries a direct delivery link first and falls back to a propagation node when none comes up; exit 3 says a propagation node holds the message. A message that went onto the link and got no delivery proof in time is not sent again through a propagation node, which would deliver it twice; the run says so and exits 1.

**fetch** drains the propagation node's mailbox and prints what is new on standard output.

**address** prints this machine's LXMF address.

**tui** is the interactive messenger, built when **lnmsg** is compiled with its `tui` feature: conversations on the left, the open conversation's messages on the right, a compose line below. A message sent from it travels the way **send --via auto** sends it, and each sent message carries a delivery mark (see DELIVERY MARKS). lnmsg keeps no message history yet, so the screen holds the messages of this session only; **--to** lists a correspondent from the start.

## OPTIONS

**--instance** *name*
:   Shared-instance name to join, overriding the Reticulum config file's. All subcommands but **address**.

**--config** *dir*
:   Reticulum config directory; only its instance name and RPC key are read. Default: the platform default.

**--pn** *hash*
:   The propagation node to use, remembered as your choice. Without it the `propagation_node` key in lnmsg's config decides; without either, the mailbox an earlier run adopted, or a newly adopted one: the most recently heard node that does not run on this device. **send**, **fetch** and **tui**.

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
:   **tui** only: no colours.

**--script** *file*, **--frames** *file*, **--size** *W*x*H*
:   **tui** only: run a key script instead of reading the keyboard, and write each frame it asks for to **--frames** (default: the script's path with `.frames`), on a screen of **--size** (default 80x24). One step a line, `#` a comment: `key` *name* (`tab`, `backtab`, `enter`, `esc`, `up`, `down`, `home`, `end`, `backspace`, `ctrl-`*c*, or one character), `type` *text*, `tick` [*n*], `frame` [*label*], and `until ready`, `until delivery` *state*, `until received` *n*, each with an optional timeout in seconds (default 30). *state* is one of `queued`, `inflight`, `sent`, `mailbox`, `delivered`, `rejected`, `failed`. An `until` that does not hold in time draws a last frame and exits 1.

## KEYS

In **tui**, the focus follows the pane. `Tab` and `Shift-Tab` cycle the conversations, the messages and the compose line.

In the conversations and the messages: `j`/`k` or the arrows move, `g`/`G` or `Home`/`End` jump to the top or bottom, `Enter` opens the conversation or goes to the compose line, `?` shows the help, `:` opens the command palette, `q` quits.

In the compose line, keys type. `Enter` sends to the open conversation (an empty line sends nothing), `Alt-Enter` starts a new line, `Esc` goes back to the messages, `Ctrl-C` clears the line (`Ctrl-Y` brings it back).

Everywhere: `Ctrl-P` opens the command palette, which matches what you type against `send`, `fetch`, `quit`, `help` and `focus conversations|messages|compose`; `Ctrl-Q` quits.

## DELIVERY MARKS

`○` queued; a spinner while a link is built or the bytes go out; `◇` handed to the network, no proof back; `⌂` left in a propagation node's mailbox; `✓` a delivery proof came back; `✗` refused (red) or given up (dim). Only `✓` is green, and only a proof makes it. With **--ascii**: `.` `*` `o` `^` `v` `x`.

## EXIT STATUS

0 success; 1 operational failure; 2 argument error; 3 (**send**) a propagation node holds the message.

## ENVIRONMENT

**LNMSG_HOME**
:   The state directory (identity, mailbox choice, seen record, client storage). Default: `$XDG_CONFIG_HOME/lnmsg`, else `~/.config/lnmsg`.

**LNMSG_DISPLAY_NAME**
:   The sender name when **--from** is not given.

**LEVICULUM_EVENT_LOG**
:   Append the run's structured events, including the message id, to this file.

## FILES

`$LNMSG_HOME/identity` is your address; back it up. **tui** writes its warnings to `$LNMSG_HOME/tui.log` instead of the screen, with a line for each step of each send leg (`SEND`), each path answer (`PATH`) and each call to the daemon (`RPC`); **RUST_LOG** replaces its filter (default `warn,lnmsg=info`).

## SEE ALSO

**lnsd**(1), **lnpnd**(1), **lnomad**(1)
