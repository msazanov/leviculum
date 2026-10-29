# loquax: the home node

Status: slice 1 built (order 1101, Refs #232). The research and the
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
time.

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

Its two request handlers admit every link at the RNS level; the home node
reads the identity the link was identified with and decides itself. A
request on a link that was not identified is answered with a refusal.

## The client protocol

Link requests on `loquax.home`, msgpack on the wire:

```text
/join  [token: bin16, name: str]   -> [status]
/sync  [cursor: uint]              -> [status, first: uint, more: bool,
                                       [[seq: uint, kind: uint, at: int, packed: bin], ...]]
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
  cursor in `$LNMSG_HOME/home-cursor` and moves it only after every entry was
  handed on, so a pull cut short is pulled again; the screen and `fetch`
  drop a message they already hold.
- **Each client has its own cursor**, so any number of them read the same
  archive.
- **The client checks authorship itself.** An entry is the LXMF message as
  the home node received it, signature and all. The client checks the
  signature against the sender's key when it knows it, and otherwise keeps
  the message unverified until the key arrives, as for any received message
  (leviculum 1079). A message addressed to anyone but the home identity, or
  whose signature does not hold, is dropped.

The later slices add `/send` (slice 2, with an outbox that retries direct
delivery for 7 days), `/mark` (slice 4) and `/register` for wake paths
(slice 3).

## The archive

A table of the loquax store (`archive`, schema 4 of
`docs/src/concepts/loquax-storage.md`'s store): one entry a received
message, with a sequence that only grows (`AUTOINCREMENT`, so a number is
never handed out twice, even after the newest entry was evicted), the kind
of entry, when it was recorded and the packed LXMF bytes. A message that
arrives twice, direct and again from the propagation node, is one entry.
The later slices append sent, delivered and read entries to the same log.

**The cap.** 1 GiB by default, `archive_cap_mb` in `$LNMSG_HOME/config`.
That is twice what a stock lxmd propagation node keeps by default
(`reference/LXMF/LXMF/Utilities/lxmd.py:159-163`, 500 MB) for everybody's
mail, here for one person's. A text message packs to well under a
kilobyte, so the default holds about a million of them, and a home host
has the gigabyte to spare. When an append takes the archive over the cap,
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
and `Restart=on-failure` tries again ten seconds later. Do not open the messenger on the home
node while the daemon runs: both would register the same delivery
destination.

A client is a laptop that ran `loquax home join`. Its messenger pulls at
start and on the mailbox schedule (`F5` and `Ctrl-r` too) and shows the
home node's address as its own, as does `loquax address`; `loquax fetch`
pulls and prints. Sending from a client is refused until slice 2.

## Compatibility with Python peers

No wire change. Senders see an ordinary LXMF destination that announces
and accepts DIRECT, OPPORTUNISTIC and propagated-and-collected mail.
`loquax.home` is a new aspect that is never announced. The end-to-end test
`loquax/tests/home_node.rs` runs a stock Python LXMF sender against the
shipped binary: DIRECT delivery and DELIVERED, two clients pulling the same
message, a reused code refused, a revoked client getting nothing.

## Slices

1. Home archive and pull: this page. Built.
2. Send through the home node: `/send`, the outbox, direct with a
   propagated copy and seven days of direct retries.
3. Quiet link: TCP liveness in lnsd, the per-link keepalive, AP mode, a
   wake push. After the battery measurement.
4. Multi-client state: `/mark`, read and sent state on every client.
5. Pairing UX: the QR code in the GUI, the text code in the TUI, hand-over
   from a one-device setup.

The rest of the plan (the M9 as a client, board sleep, Android) is in the
draft's section 5.
