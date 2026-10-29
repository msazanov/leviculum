# loquax: the home node

Status: slices 1 and 2 built (orders 1101 and 1106, Refs #232). The research and the
critique behind it are the draft `docs/drafts/home-node-2026-10.md` (order
1096); this page keeps what is decided and what is built.

## Why

Two devices on one LXMF identity lose mail. The reference client deletes
every message it fetched from the propagation node, whatever it is
configured to do (`reference/LXMF/LXMF/LXMRouter.py:1623-1638`), so the
second device never sees what the first collected. Ratchets make it worse:
a message encrypted to one device's newest ratchet cannot be read on the
other, and is deleted there all the same. The draft lists seven ways a
message disappears; most of them hit hardest when two devices share an
identity.

The home node ends the sharing. One always-on loquax holds the identity and
keeps every message; the user's other devices read from it.

## Roles and keys

| Role | Holds | Runs |
|---|---|---|
| Home node | the LXMF identity, the archive, the admitted devices | `lnsd` plus `loquax --daemon` |
| Client | its own RNS device identity, a copy of what it pulled | `loquax` with a home code |
| Senders | nothing new | stock LXMF |

Decided by Lew on 2026-10-29:

- **Only the home node holds the LXMF identity.** Each client has its own
  RNS identity (`$LNMSG_HOME/device-identity`), admitted once and revocable.
  A client never registers a delivery destination at all, so in particular
  never one for the home identity, whose private key it never holds. This is
  enforced in code: a client's engine starts in a state that has no identity
  to register (`Engine::client` in `loquax/src/engine.rs`), and the test
  `a_client_engine_registers_no_delivery_destination` runs the same probe on
  a messenger engine as its positive control.
- **The default home node is the user's own device**: a Raspberry Pi, a
  home PC, anything Linux that stays on. A VPS may run it too, and works
  without NAT trouble, but it puts the identity and every message in
  plaintext on rented hardware. That is a conscious choice for the user to
  make, not a default.
- **The archive is kept forever, under a disk cap.** When the cap is
  reached the oldest entries go. See "The archive".
- **A public propagation node on the home host runs under an identity of
  its own**, never the user's. Not part of slice 1.
- **The phone comes later**, as our own Android app. The clients of slice 1
  are loquax on a laptop or a desktop.

Losing a client: revoke it, the identity is untouched. Losing the home node
loses the identity, since LXMF has one key per identity; keep an offline
backup of `$LNMSG_HOME/identity`, made at setup.

## Admission and revocation

On the home node, `loquax home admit --name laptop` opens one single-use
token and prints a code. The code carries the home identity's public key
and the token, with four bytes of SHA-256 over both so a mistyped code is
refused before anything goes on the wire, in lower-case base32 behind the
prefix `lqxh1` (140 characters; slice 5 adds a QR code for it). The home
destination and the LXMF address both derive from the public key, so the
code needs nothing else.

On the new device, `loquax home join <code>` creates the device identity,
asks for the path to the home destination, opens a link to it, identifies
with the device identity and sends the token. The home node admits the
identity under the name given at `admit`, records when, and deletes the
token, all in one transaction. A token used once is refused the second
time, and so is one older than `home_token_hours` in the config (24 by
default: time to carry the code to the other device, short enough that a
code left in a chat log or a screenshot is dead by the next day). `loquax
home tokens` lists the codes still open, with when each expires.

`loquax home devices` lists the admitted devices, `loquax home revoke
<device>` removes one, by its name or by its hash (four hex characters are
enough when unique). Every request checks membership again, as lxmd's
control handlers check their allow list on every call
(`reference/LXMF/LXMF/LXMRouter.py:838-845`), so a device revoked while its
link is up is refused on its next request.

## The home destination

`loquax.home`, a SINGLE destination of the home identity. It is never
announced: a client learns its key from the code and asks for its path, and
the home node answers that path request because the destination is
registered locally, as the reference answers for any local destination
(`reference/Reticulum/RNS/Transport.py:2936-2941`). Peers other than the
user's own devices never see it.

Its three request handlers admit every link at the RNS level; the home node
reads the identity the link was identified with and decides itself. A
request on a link that was not identified is answered with a refusal.

## The client protocol

Link requests on `loquax.home`, msgpack on the wire:

```text
/join  [token: bin16, name: str]   -> [status]
/sync  [cursor: uint]              -> [status, first: uint, more: bool,
                                       [[seq: uint, kind: uint, at: int, packed: bin], ...]]
/send  [client_id: bin16, destination: bin16, title: bin, content: bin,
        [[key: int, value: bin], ...]] -> [status, id: bin32]
```

The status comes first in every answer: 0 ok, 1 not identified, 2 not
admitted, 3 unknown or used token, 4 malformed request, 5 the home node's
store failed. A `/sync` answer carries the entries after the cursor, oldest
first, up to 64 KiB of messages (one larger entry still goes, alone), and
`more` when others follow; the client asks again from the last sequence it
got, over the same link. An answer too large for one packet goes as a
response resource. `first` is the oldest sequence the archive still holds,
so a client whose cursor fell behind an eviction says so in its log.

- **Fetching never deletes.** Only a later slice's `/mark` changes state.
- **Idempotent.** The same cursor gets the same entries. A client keeps its
  cursor in `$LNMSG_HOME/home-cursor` and moves it only after what it pulled
  is kept: the pull hands the entries to the engine with a fence behind them
  (`Command::Fence`), and once the fence is answered every event the entries
  caused has gone out before it. The screen saves the cursor when it takes
  the pull's end, after its store wrote those events; `fetch` saves it
  after its seen file. A crash in between pulls the same entries again, and
  the screen and `fetch` drop a message they already hold.
- **Each client has its own cursor**, so any number of them read the same
  archive.
- **The client checks authorship itself.** An entry is the LXMF message as
  the home node received it, signature and all. The client checks the
  signature against the sender's key when it knows it, and otherwise keeps
  the message unverified until the key arrives, as for any received message
  (leviculum 1079). A message addressed to anyone but the home identity, or
  whose signature does not hold, is dropped.

The later slices add `/mark` (slice 4) and `/register` for wake paths
(slice 3).

## Sending through the home node

Slice 2. A client hands each message to the home node with `/send`: a
16-byte id of its own making, the destination, title, content and fields
(each field's value as its raw msgpack, so the home node signs what the
client composed). The home node signs the message once with the identity,
puts it in its outbox and answers with the message id. The client is done
then and may sleep.

- **Idempotent by the client's id.** The same id handed in again is
  answered from the outbox with the message it made the first time, also
  after a restart; nothing is signed twice. A client whose answer was lost
  asks again with the same id, up to three times
  (`home::client::send_retrying`).
- **The outbox is a table** (`home_outbox`, schema 5 of the store), one row
  a message: the client's id, the message id, the destination, the packed
  bytes, when it came, whether it is still retried, and whether a
  propagation node took a copy. A restart goes on with every message still
  retried, each due direct at once.
- **The policy**, a pure state machine in `loquax/src/home/outgoing.rs` with
  its tests beside it: direct over a link at once, so a recipient that is
  not there fails crisply. When the router's stock attempts end without a
  proof, the same packed message goes to the propagation node (the one
  `--pn`, the config or the last collection selected; with none, the
  message waits for the direct retries alone), and direct delivery is tried
  again on every announce of the recipient and every `outbox_retry_secs`
  (3600 by default), for `outbox_days` (7 by default, Lew's decision of
  2026-10-29). The announce is the retry that matters, since it says the
  recipient is there; the cadence covers an announce the home node missed,
  at one link attempt an hour. The router holds one entry per message id,
  so a message is never on two legs at once.
- **DELIVERED when a direct proof arrives**, even after the propagation node
  took a copy. Both legs carry the same bytes, so the same hash, and the
  recipient keeps one message per hash
  (`reference/LXMF/LXMF/LXMRouter.py:1906-1908`): the copy it later collects
  is dropped there.
- **Every state change is an archive entry**, appended in the same
  transaction as the outbox row changes: `sent` (kind 1, the packed message
  as signed), `propagated` (2), `delivered` (3), `failed` (4: the seven days
  ended with no propagation node ever taking it) and `retries ended` (5: the
  direct retries ended while a copy waits on the propagation node). Kinds 2
  to 5 carry the message id as their bytes.

On the client the compose line and `loquax send` go through `/send`. The
screen names the bubble after the id the home node answered with
(`Command::HomeAccepted`, reported as the router's `Queued` would be), and
each pull turns the state entries into the bubble's state
(`Command::HomeState`): propagated shows as in the mailbox, delivered as
delivered, failed as failed. `loquax fetch` prints a line `message <id>
<state>` for each. Read and sent state on every client, including the
sent messages another client wrote, is slice 4.

## The archive

A table of the loquax store (`archive`, schema 4 of
`docs/src/concepts/loquax-storage.md`'s store): one entry a received
message, with a sequence that only grows (`AUTOINCREMENT`, so a number is
never handed out twice, even after the newest entry was evicted), the kind
of entry, when it was recorded and the packed LXMF bytes. A message that
arrives twice, direct and again from the propagation node, is one entry.

**The cap.** 1 GiB by default, `archive_cap_mb` in `$LNMSG_HOME/config`.
That is twice what a stock lxmd propagation node keeps by default
(`reference/LXMF/LXMF/Utilities/lxmd.py:159-163`, 500 MB) for everybody's
mail, here for one person's. A text message packs to well under a
kilobyte, so the default holds about a million of them, and a home host
has the gigabyte to spare. The sent messages and their state changes go into the same log (see
"Sending through the home node"). When an append takes the archive over the cap,
the oldest entries go until it fits again; the entry just appended stays,
even when it alone is larger than the cap.

## Inbound on the home node

To every sender the home node is an ordinary `lxmf.delivery` destination:
senders deliver DIRECT and get their proof. It announces at start and every
360 minutes, the interval lxmd's config template offers for the same
announce (`reference/LXMF/LXMF/Utilities/lxmd.py:1140`). It also collects
from its propagation node on the messenger's own schedule
(`loquax-mailbox.md`, "When to sync"), so mail that senders propagated
arrives too; those senders get no direct proof from it.

## Running it

The home node is `loquax --daemon` beside a running `lnsd`. It takes the
messenger's `--instance`, `--config`, `--pn`, `--sync-interval` and
`--timeout`. It logs to stderr (`warn,loquax=info`, `RUST_LOG` replaces
it), one `HOME` line for each archived message, admission and refusal, and
never logs a message's text. A systemd user unit:

```ini
# ~/.config/systemd/user/loquax-home.service
[Unit]
Description=loquax home node

[Service]
ExecStart=/usr/bin/loquax --daemon
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
```

`systemctl --user enable --now loquax-home`, and `loginctl enable-linger
$USER` so it runs without a login. The package's `lnsd` is a system
service, which a user unit cannot be ordered after; a home node started
before `lnsd` exits 1 with "could not join the Reticulum shared instance",
and `Restart=on-failure` tries again ten seconds later. The daemon and the
messenger never run on one home at once: both would register the same
delivery destination, so each takes an `flock` on
`$LNMSG_HOME/delivery.lock` first and the second refuses to start, naming
the first (`loquax/src/home/lock.rs`). The kernel lets go with the
process, so a crash leaves nothing to clean up.

A client is a laptop that ran `loquax home join`. Its messenger pulls at
start and on the mailbox schedule (`F5` and `Ctrl-r` too) and shows the
home node's address as its own, as does `loquax address`; `loquax fetch`
pulls and prints. It sends through the home node (see "Sending through the
home node").

## Compatibility with Python peers

No wire change. Senders see an ordinary LXMF destination that announces
and accepts DIRECT, OPPORTUNISTIC and propagated-and-collected mail.
`loquax.home` is a new aspect that is never announced. The end-to-end tests in
`loquax/tests/home_node.rs` run stock Python LXMF peers against the
shipped binary. Slice 1: DIRECT delivery and DELIVERED, two clients pulling
the same message, a reused code refused, a revoked client getting nothing.
Slice 2: a client sends through the home node to a Python recipient that
is offline and exits; the direct leg fails, the recipient comes up later
and announces, the message arrives DIRECT and once, and the client's next
fetch shows it delivered; a second daemon on the same home is refused.

## Slices

1. Home archive and pull. Built.
2. Send through the home node: `/send`, the outbox, direct with a
   propagated copy and seven days of direct retries. Built, with the three
   limits slice 1 left: the delivery lock, the cursor after the store, and
   token expiry.
3. Quiet link: TCP liveness in lnsd, the per-link keepalive, AP mode, a
   wake push. After the battery measurement.
4. Multi-client state: `/mark`, read and sent state on every client.
5. Pairing UX: the QR code in the GUI, the text code in the TUI, hand-over
   from a one-device setup.

The rest of the plan (the M9 as a client, board sleep, Android) is in the
draft's section 5.
