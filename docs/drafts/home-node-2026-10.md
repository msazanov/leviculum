# Home node and light clients: research, critique, concept

Status: draft for Lew, order 1096, 2026-10-29. Refs #232. Research and
concept only; no product code is changed by this page.

References read: LXMF 1.1.0 at `reference/LXMF` (commit 795fdaa), RNS 1.3.5
at `reference/Reticulum` (upstream 422dc055 plus our fail-loudly patch
d5e62d4e, which touches only `RNS/Reticulum.py`), and leviculum at e9a72d1c.
Every path below is relative to the repository root. Prior art comes from
the web; each source has its URL and a tag: datasheet [DS], spec [SPEC],
project documentation [DOC], paper [PAPER], blog or forum claim [CLAIM].
Figures I worked out myself are marked [DERIVED].

## 0. Verdict

The idea holds, in a narrower form than Lew's version:

- A home node that holds the LXMF identity is the only design that gives
  senders a real "in the mailbox" proof. A blind mailbox that stores only
  ciphertext, the Briar model, cannot accept DIRECT delivery, because the
  link proof is signed by the destination's key.
- It solves problem 4 by construction and problem 2 for every sender that
  delivers directly. It turns problem 1 from silent loss into visible state.
  It eases problem 3 only as far as people adopt it.
- Problem 5 is mostly not the announce flood. Both stacks send a TCP
  keepalive probe after 5 s of idle, and an RNS link keepalive at mobile RTT
  comes every 10 to 20 s. A "quiet link" is therefore not quiet today, and
  making it quiet is interface work plus one per-link setting.
- The reviewer's proposed "leaf client" option already exists: it is
  `mode = access_point` on the home node's listener, in both stacks.
- The home node is `loquax` in daemon mode, the half that the 2026-08-08
  decision left open "to be decided empirically". Problem 1 is that
  evidence. It is not lnpnd and not lnsd.
- There is no Android client of ours yet, so the smartphone rungs of the
  wake ladder come late. The first clients are loquax on a laptop and,
  later, the M9.
- Wake-on-radio is premature. No board sleeps today, no board current has
  been measured, and the M9 and T1000-E carry an LR1110, not an SX1262.

## 1. Findings per problem

### Problem 1: messages disappear

**Refined.** "Disappear" covers at least seven distinct mechanisms in the
reference. Most of them hit hardest when two devices share an identity.

1. **First collector purges, and the setting does not stop it.**
   `retain_synced_on_node` defaults to false
   (`reference/LXMF/LXMF/LXMRouter.py:118`). Worse than the reviewer
   assumed: the setting only suppresses the "haves" list
   (`reference/LXMF/LXMF/LXMRouter.py:1581-1583`). After every download the
   client unconditionally sends `[None, haves]` for every message it
   fetched, under the comment "This deletes the messages on the propagation
   node. TODO: Add option to keep messages on node"
   (`reference/LXMF/LXMF/LXMRouter.py:1623-1638`). A second device on the
   same identity therefore never sees mail the first one collected,
   whatever either device has configured. Our own mailbox page
   (`docs/src/concepts/loquax-mailbox.md:274-281`) calls the default the
   wrong default for two clients, but treats it as a setting. In the
   reference it is not one.
2. **Purge after a failed decrypt (new finding).** The `haves` entry is
   appended whatever `lxmf_propagation` returned
   (`reference/LXMF/LXMF/LXMRouter.py:1624-1627`). If decryption fails,
   the message is still deleted on the node. Together with ratchets this is
   a concrete mechanism for problem 4 feeding problem 1:
   - every LXMF delivery destination has ratchets enabled
     (`reference/LXMF/LXMF/LXMRouter.py:357`);
   - each announce rotates the ratchet (`reference/Reticulum/RNS/Destination.py:283-287`);
   - a sender encrypts to the newest ratchet it holds
     (`reference/Reticulum/RNS/Destination.py:596-599`);
   - a ratchet-encrypted message cannot be decrypted with the base key.
   So device B fetches a message encrypted to device A's ratchet, fails to
   decrypt it, and tells the node to delete it. Nobody ever reads it.
3. **A propagation node's own mailbox loses silently.** When a propagated
   message is addressed to the node's own delivery destination and
   decryption fails, the transient id is already marked processed, nothing
   is stored, and the function returns true
   (`reference/LXMF/LXMF/LXMRouter.py:2496-2525`).
4. **Eviction.** lxmd's storage limit defaults to 500 MB
   (`reference/LXMF/LXMF/Utilities/lxmd.py:159-163`). The weight is
   priority × age × size (`reference/LXMF/LXMF/LXMRouter.py:1056-1067`), and
   the heaviest go first (`reference/LXMF/LXMF/LXMRouter.py:1180-1218`). The
   limit is checked only in the store job, every 120 jobs of 4 s, about
   8 minutes.
5. **Expiry.** 30 days (`reference/LXMF/LXMF/LXMRouter.py:38`), counted
   from each node's own receive time.
6. **Undownloadable.** A message over the client's per-transfer limit is
   skipped on every fetch (`reference/LXMF/LXMF/LXMRouter.py:1546`) until it
   expires. A message over a peer's per-message limit is marked handled and
   never offered to that peer (`reference/LXMF/LXMF/LXMPeer.py:370-373`).
7. **False failure, not loss.**
   - A DIRECT message with no path fails after 5 attempts
     (`reference/LXMF/LXMF/LXMRouter.py:30-34`, `:2820-2842`), about 40 s.
     LXMF itself never falls back to propagation: `try_propagation_on_fail`
     occurs nowhere in `reference/`. Our `loquax --via auto` does the
     fallback in the app, as Columba does.
   - A propagated upload whose proof arrives late is torn down and retried,
     and becomes FAILED after 5 attempts
     (`reference/LXMF/LXMF/LXMRouter.py:2860-2899`) even though the node
     may hold it.

**What #450 is and is not.** `lxmf_pn_stock_client_upload_reference`
measures problem 1's last mechanism. Stock lxmd proves after the stamp
check and the store, in 15 to 18 ms, while the stock client's receipt
timeout is `max(rtt*6, …)`, about 11 to 14 ms on loopback. lnpnd in the
same seat proves in 2.6 to 4.8 ms (periculum
`regression/KNOWN-REDS.toml:164-179`). That makes it a sender-side false
failure, not a disappearance.

One thing to flag: the nightlies of 2026-10-26 and 2026-10-28 show
`sent=7 teardowns=6`, and the one of 2026-10-27 shows three teardowns
(`~/.local/state/leviculum/nightly/report-20261028.md:59-60`). The
registered evidence is `sent=10 teardowns=0`, and the register itself
says a teardown "is a defect, not this". The cell's attribution should be
re-checked. That is outside this order.

**Not yet measured.** Which of these mechanisms dominates in the real
network is unknown. Section 4 proposes the measurement.

### Problem 2: no proof for propagated mail

**Confirmed.** A propagated message ends in SENT through
`__mark_propagated` (`reference/LXMF/LXMF/LXMessage.py:573-575`). The
proof that sets it is the node's proof of the upload
(`reference/LXMF/LXMF/LXMRouter.py:2252-2255`). Neither
`message_get_request` nor `message_get_response`
(`reference/LXMF/LXMF/LXMRouter.py:1482-1644`) sends anything to the
sender, so SENT is final.

Corollary: OPPORTUNISTIC delivery is proven before the message is parsed
(`reference/LXMF/LXMF/LXMRouter.py:1926-1927`), so DELIVERED there means
"the destination received the packet", not "the message was accepted".

### Problem 3: propagation does not scale

**Confirmed.**
- Every stored message is queued to every peer except the one it came
  from, with no filter by destination
  (`reference/LXMF/LXMF/LXMRouter.py:2478-2485`).
- Defaults: up to 20 peers, auto-peering within 4 hops
  (`reference/LXMF/LXMF/LXMRouter.py:43-45`).
- Each node stores everything for everybody up to its limit. Past the
  limit, eviction (problem 1, item 4) takes over. So problem 3 feeds
  problem 1.
- Our board's PN is bounded to about 176 messages on purpose
  (`docs/src/concepts/propagation-node-on-a-board.md`, §3-4). That makes
  the board a cache, not a store.

### Problem 4: one identity on two devices

**Confirmed, and worse than random.**
- Path choice in Transport replaces a path when an announce is strictly
  newer in emission time and its random blob is unseen, at any hop count
  (`reference/Reticulum/RNS/Transport.py:1762-1815`). With two devices
  announcing, every node follows whichever announced last, at
  one-second resolution.
- An existing link stays pinned to its interface and dies by timeout
  rather than following the path.
- LXMF keeps no per-origin state. The stamp cost cache is last write wins
  (`reference/LXMF/LXMF/LXMRouter.py:1027-1029`).
- The ratchets from problem 1, item 2, turn this from "random device" into
  "neither device".
- The custody draft already calls copying an identity to two devices
  "unsupported and documented as broken"
  (`docs/drafts/custody-propagation-concept-v3.md`, §6).

### Problem 5: always online drains the battery

**Half true, for a different reason than assumed.** The announce flood is
real, but it is not the main cost on a single TCP connection.

1. **TCP liveness, both stacks.**
   - Python sets keepalive idle 5 s, interval 2 s, 12 probes, and
     `TCP_USER_TIMEOUT` 24 s
     (`reference/Reticulum/RNS/Interfaces/TCPInterface.py:84-87`,
     `:181-187`), on outgoing and on accepted sockets (`:143-147`).
   - lnsd mirrors this by design (`leviculum-std/src/interfaces/tcp.rs:62-78`)
     on both sides (`leviculum-std/src/interfaces/tcp.rs:482`, `:907`).
   - An idle connection therefore exchanges a probe every 5 s, which keeps
     a cellular modem out of its idle state permanently. This alone defeats
     a quiet phone.
2. **Link keepalive scales with RTT.**
   `keepalive = clamp(rtt × 360/1.75, 5, 360)` s
   (`reference/Reticulum/RNS/Link.py:844-846`; ours at
   `leviculum-core/src/link/mod.rs:1504-1512`). At a mobile RTT of 50 to
   100 ms that is 10 to 21 s. Only an RTT of 1.75 s or more reaches the
   360 s maximum.
   - The initiator sends keepalives. The responder only tears down after
     `stale_time = 2 × keepalive`.
   - We already have a per-link override
     (`leviculum-core/src/link/mod.rs:1531-1546`), threaded node-wide from
     `TransportConfig::link_keepalive_secs`
     (`leviculum-core/src/transport.rs:1056`). It has no upper clamp.
3. **Announce forwarding.** On a transport node every broadcast announce
   goes out on every interface that has OUT set, including the one it came
   in on (`reference/Reticulum/RNS/Transport.py:1180-1326`), at most twice.
   - **Correction to the reviewer:** `MODE_ACCESS_POINT` blocks every
     broadcast announce on that interface, the node's own included
     (`reference/Reticulum/RNS/Transport.py:1194-1197`; ours at
     `leviculum-core/src/transport.rs:10318-10319`).
   - Path responses still go out, because they carry an attached interface.
   - AP is in `DISCOVER_PATHS_FOR`, so a leaf's path request for an unknown
     destination gets recursive discovery on its behalf.
   - TCP server children inherit the listener's mode
     (`leviculum-std/src/driver/interface_build/tcp.rs:116-124`).
   - So `mode = access_point` on the home node's listener for clients *is*
     the leaf option, and it needs no new code.
4. **Android Doze** cuts network access for apps that are not exempt from
   battery optimisation. A foreground service alone does not lift that cut
   [DOC] https://developer.android.com/training/monitoring-device-state/doze-standby.
   Any always-reachable design on Android needs the exemption. Conversations
   and ntfy use one [DOC]
   https://codeberg.org/iNPUTmice/Conversations/raw/branch/master/README.md,
   https://docs.ntfy.sh/subscribe/phone/.

Ingress control does not help a client; it holds inbound announces at the
node. The announce cap delays and reorders, but does not reduce the count
per destination much. Rate targets suppress only rebroadcasts of
fast-announcing destinations (`reference/Reticulum/RNS/Transport.py:1838-1889`).

## 2. Critique

### Of Lew's premises

- **"Preferably over a link rather than via the propagation network":**
  right, and this is the whole value. LXMF's default is DIRECT and it falls
  back only in apps, after about 40 s. An always-reachable recipient means
  the fallback never triggers. No sender changes are needed.
- **"Solves all five problems at once":** too strong.
  - Problem 3 only by adoption.
  - Problem 1 for mail to home-node users only. Mail to everybody else
    still goes through the same propagation network.
  - Problem 5 is solved only once the TCP and link keepalives are fixed,
    which the premise did not see.
- **"The phone does not take part in Reticulum normally":** right as a
  goal, but there is no Android client of ours. The phone in the example
  would be Columba (foreign) or a future app over `leviculum-ffi`
  (`docs/src/concepts/handheld-messenger.md:75`, `:107`). Our near-term
  clients are loquax on a laptop and the M9.
- **"An embedded node, always on, connected to the Internet":** none of our
  nRF boards has an IP uplink. The only ESP32 board, the M9, is the
  handheld. Realistically the first home node is a Linux host: Pi, home PC
  or VPS.

### Of the reviewer's additions

- **"2, 4 and 5 solved by construction":** 2 and 4 yes. 5 no; see problem 5
  above.
- **"Leaf client option in lnsd":** unnecessary; `mode = access_point`
  exists.
- **"The home node sees plaintext; acceptable":** agreed for a host the
  user owns. Two caveats:
  - a VPS home node puts identity and plaintext on rented hardware; that
    should be a conscious choice, not a default;
  - a stolen home node leaks the archive as well as the identity. No board
    or host of ours encrypts its store at rest.
- **Retry for days for outgoing mail:** the strongest unmentioned gain;
  agreed. One addition: the home node can propagate *and* keep retrying
  direct with the same packed message. The recipient drops the second copy
  by message hash (`reference/LXMF/LXMF/LXMRouter.py:1906-1908`), so
  nothing is duplicated, and the sender still gets DELIVERED when the
  direct attempt succeeds.
- **Wake-on-radio on "M9, Pocket, T1000-E" with the SX1262:** only the
  Pocket V2 (RAK4631) has an SX1262. The M9 and T1000-E have an LR1110
  (`docs/src/concepts/thinknode-m9.md:17`).
  - Neither driver has `SetRxDutyCycle`. `leviculum-nrf/src/sx1262.rs:16-43`
    lists the opcodes, and `leviculum-lr11xx/src/cmd.rs` has `SET_CAD` but
    no duty-cycle command.
  - Neither driver ever sleeps the radio (`leviculum-nrf/src/sx1262.rs:557-559`).
  - The MCU never sleeps either: "Nothing ever sleeps" in Mesh mode
    (`docs/src/concepts/operating-modes.md:31`).
  - No board current has been measured
    (`docs/src/concepts/operating-modes.md:120-125`).
  - Radio RX at 4.6 mA [DS] is not the dominant drain while an ESP32-S3
    or an nRF52 runs. Duty-cycled RX pays off only after MCU sleep exists.
  - A 1 s wake preamble also occupies the channel for 1 s for every
    neighbour in range: CSMA sees carrier and receivers lock on. That is
    a cost to the mesh, and it is paid per wake.
- **Pager board for the phone, CompanionDeviceManager:** CDM binds the
  app's service when the companion appears over BLE [DOC]
  https://developer.android.com/reference/android/companion/CompanionDeviceService.
  It is a presence hook, not a push channel. It is sound but needs our own
  Android app. It also costs a phone-side BLE connection whose power we
  have not measured.
- **UnifiedPush:** for a user whose only push app is ours, a
  self-hosted distributor is the same persistent connection with
  keepalives, in another app. It wins only when the user already runs
  ntfy for other apps and the connection is shared. Keep it as a rung, but
  after the quiet link, not instead of it.

### Alternatives found

1. **Blind mailbox (Briar model):** a PN that retains the owner's
   ciphertext, while clients hold copies of the identity.
   - Cheapest by far: the wire protocol is the stock `/get`, so Columba and
     Sideband could be clients today. Server side, only "do not purge the
     owner's messages on a haves list" is new.
   - Lost on three counts: no delivery proof (problem 2 stays); a lost
     phone loses the identity; and problem 4 returns the moment any client
     announces, which stock clients do by default.
   - Verdict: a compatibility mode for Columba users, not the design.
2. **End-to-end receipts and sender custody** (the parked custody draft,
   `docs/drafts/custody-propagation-concept-v3.md`): it fixes problem 2 for
   every pair that implements it, but only between our clients, and it
   leaves problem 4. The home node delivers the same custody property
   ("the sender keeps the message until proven") for the user's own
   outgoing mail without any new field on the wire. The two are compatible.
   The draft's attested device list (§6, stage 2) is a heavier form of the
   device admission below.
3. **Sharding the PN store by recipient hash:** this breaks peering with
   Python nodes, as the reviewer said. Not pursued.
4. **Per-owner priority on a public PN:** lxmd's `prioritise_destinations`
   (`reference/LXMF/LXMF/Utilities/lxmd.py:214-215`) already gives one
   operator's own destinations a tenth of the eviction weight. A user who
   runs a PN can use it today against problem 1, item 4, without any home
   node. lnpnd accepts the key but ignores it (`lnpnd/src/config.rs:209-227`, the inert keys).
   That is a small, separate fix.

## 3. Concept

### 3.1 Roles and keys

| Role | Holds | Runs |
|---|---|---|
| Home node | the LXMF identity, its ratchets, the mail archive, the admitted device list, the outbox | `lnsd` plus `loquax --daemon` |
| Client device | its own device identity (an RNS identity, never announced as `lxmf.delivery`), a cache of the archive | `loquax` in client mode |
| Senders anywhere | nothing new | stock LXMF |

- **Identity.** Only the home node holds it and announces `lxmf.delivery`.
  This is the property that makes problem 4 go away; it must be enforced,
  not documented. A client in home mode never registers a delivery
  destination for the home identity.
- **Device keys.** Each client has its own RNS identity. The home node
  keeps an admitted list: device identity hash, a name, the wake paths it
  registered, and when it was admitted. The pattern is lxmd's `allowed`
  file (`reference/LXMF/LXMF/Utilities/lxmd.py:268-288`, `:336`), extended
  with metadata.
- **Admission.** On the home node, `loquax home admit` shows a QR code and
  a short text code. The code carries the home node's destination hash, its
  identity public key and a single-use token, the way Briar pairs its
  mailbox (`PUT /setup` with a single-use token from the QR code [DOC]
  https://raw.githubusercontent.com/briar/briar-mailbox/main/API.md). The
  client opens a link, identifies with its device key, and presents the
  token. The home node adds the device and invalidates the token.
- **Revocation.** `loquax home revoke <device>` removes it from the list.
  Every request re-checks membership, as lxmd's control handlers do
  (`reference/LXMF/LXMF/LXMRouter.py:838-865`).
- **Loss of a client:** revoke the device. The identity is untouched.
- **Theft of the home node:** the identity is gone. LXMF has one key per
  identity, so this cannot be split. The archive is exposed as well.
  Mitigation: an identity backup, kept offline and printable, made at setup.
- **Plaintext on the home node:** accepted, with the VPS caveat in §2.

### 3.2 The client protocol

The protocol is ours. It is used only between a user's own devices: RNS
link requests and resources on a home destination (`loquax.home`, a SINGLE
destination of the home identity). The responder answers only links
identified by an admitted device key.

| Request | Payload | Answer |
|---|---|---|
| `/sync` | the client's cursor (the last archive sequence it holds) | new archive entries since the cursor (messages in and out, state changes), as a resource when large |
| `/send` | a client-generated id plus destination, title, content and fields | the accepted id; the home node signs, stores in the outbox and delivers |
| `/mark` | message ids plus a flag (read, deleted) | the new archive sequence |
| `/register` | wake paths (§3.3) | ok |

- **Archive with a sequence.** Every change (received, sent, delivered,
  read, deleted) appends one entry with a monotonic sequence. That is the
  IMAP model reduced to one append-only log, and the loquax SQLite store
  (`loquax/src/store.rs`, the `ledger` table) is the natural home for it.
- **Idempotence.** `/send` is keyed by the client's id, so a retried
  submission returns the same outbox entry. `/sync` with the same cursor
  returns the same entries. A client deletes nothing on the home node by
  fetching; only `/mark` changes state. That fixes the purge problem at
  the root.
- **Several clients.** Each keeps its own cursor. Read and sent state
  arrive at every client as archive entries.
- **What the client sees of the original message.** The packed LXMF
  message as received: the sender's signature and the sender's public key
  are included, so a client verifies the signature itself and does not have
  to trust the home node for authorship.
- **Send through the home node.** The client is done once `/send`
  answers, and may sleep. Outbox policy on the home node:
  1. direct delivery at once;
  2. on direct failure (the stock 5 attempts), propagate the same packed
     message to the configured PN and keep retrying direct on every
     announce of the recipient and at a slow cadence, for 7 days;
  3. DELIVERED when a direct proof arrives, even after propagation. The
     recipient deduplicates by message hash
     (`reference/LXMF/LXMF/LXMRouter.py:1906-1908`).
- **Inbound for the home identity.** Senders deliver DIRECT to the home
  node and get DELIVERED. The home node also collects from its propagation
  node on loquax's existing schedule (`loquax/src/sync.rs`), so mail that
  senders propagated still arrives. It does not yet give those senders a
  proof.

### 3.3 The wake ladder

The wake signal says only "something is waiting". The content always comes
over `/sync`.

| Rung | Platforms | Power at the client | Latency | Effort | When |
|---|---|---|---|---|---|
| Pull: on open, on a key, on loquax's timer | all | nothing beyond the fetch | up to the timer (loquax: 2 min after activity, 15 min hard bound) | exists as a schedule (`loquax/src/sync.rs`); new target `/sync` | slice 1 |
| Quiet link: one TCP connection to the home node, AP mode at the node, TCP keepalive and link keepalive at the NAT-safe interval, home node pushes "wake" over the link | laptop, Android (exempt from battery optimisation), M9 over WLAN | unmeasured. Literature: 11 of 73 carriers drop idle TCP under 10 min [PAPER] https://web.eecs.umich.edu/~zmao/Papers/netpiculet.pdf, and ntfy claims 0 to 1 % per 17 h [CLAIM] https://docs.ntfy.sh/faq/ | seconds | interface knob plus per-link keepalive plus a `/wake` push; small | slice 3 |
| Home WLAN | same, at home | as quiet link, no NAT, so the keepalive can be long | seconds | discovery: AutoInterface already finds the home node on the LAN (`docs/src/concepts/zero-config.md`) | slice 3 |
| UnifiedPush with self-hosted ntfy | Android | shared with other UP apps; otherwise no better than the quiet link | seconds | home node POSTs an empty message; Android app registers as a UP app | with the Android app |
| Pager board over BLE (Pocket or T1000-E listens, phone woken by notify, CDM presence) | Android | board: today's idle draw, unmeasured. Phone: one long-interval BLE connection, unmeasured | seconds | Android app plus a GATT wake notify; the BLE stack exists | with the Android app |
| Scheduled windows (GNSS or last-contact clock, e.g. 200 ms every 2 min) | own handhelds | [DERIVED] at 4.6 mA RX [DS], 0.2 s per 120 s ≈ 8 µA radio average, plus MCU wake cost (unmeasured) | up to the period | MCU and radio sleep, a clock-sync rule, a home-node send schedule; large | after a PPK2 baseline and sleep support |
| Wake-on-radio (SetRxDutyCycle, long preamble) | Pocket V2 (SX1262); LR1110 to be verified | [DERIVED] 2 symbols every 1 s at SF8 ≈ 25 µA plus the 1.2 µA warm-sleep floor [DS]. Costs the mesh 1 s of channel per wake | under the period | radio sleep, duty-cycle command, per-peer preamble in the LoRa interface; large and rig-heavy | research only |

SX1262 figures: datasheet Rev 1.2, Table 3-5. Sleep is 160 nA cold,
600 nA warm and 1.2 µA warm with RC64k; RX is 4.6 mA (DC-DC, 125 kHz).
§13.1.7 gives `Tpreamble + Theader ≤ 2 × rxPeriod + sleepPeriod` [DS]
https://cdn.sparkfun.com/assets/6/b/5/1/4/SX1262_datasheet.pdf. The lower
bound on the preamble needed to be caught is in AN1200.36, which we could
not obtain, so it has to be measured.

**Interface isolation.** Every medium-specific part lives in an interface
or in loquax:
- TCP keepalive tuning is a per-interface option of the TCP interface;
- the preamble for a sleeping peer is LoRa interface logic;
- AP mode is an existing per-interface mode.

What the home node knows about wake paths is application state in loquax,
not in `transport.rs` or `node/`. The per-link keepalive is a Link
property that already exists. Exposing it per link to the caller who opens
the link is a core API change, not medium awareness.

### 3.4 Home node hardware

| Option | Reach while travelling | Notes |
|---|---|---|
| Home PC or Pi with lnsd and loquax | yes, via one inbound TCP port, or via a public hub with `mode = access_point` toward the client | first target; everything runs on it today except the client protocol |
| VPS | yes, no NAT at the home side | identity and plaintext on rented hardware; a conscious choice |
| Router (OpenWrt) | yes | lnsd is a static musl binary; loquax daemon size and storage need checking |
| ESP32 board with WLAN | yes | no such home-node-class board of ours yet |
| nRF board, LoRa and BLE only | no, only within radio range | fine as a neighbourhood home node; not for Berlin while it stands in Ottersberg |

### 3.5 Fallback to the propagation network

- **Home node down, inbound:** senders' direct attempts fail and their apps
  propagate. The home node collects when it comes back (loquax schedule).
  Nothing is worse than today.
- **Home node down, outbound:** the client cannot send through it. A client
  that holds only a device key cannot sign as the identity. Options: the
  client queues and waits (the default), or the user accepts identity
  copies (the blind-mailbox compatibility mode).
- **Client down:** the archive waits; this is the normal case.

### 3.6 Zero-config and the step to two devices

- One device stays exactly as today: loquax holds the identity, uses PNs,
  and nothing about the home node is visible.
- Moving to two devices: on the always-on machine, `loquax home init`
  takes over the existing identity (its file is moved, not copied; the old
  device becomes the first client and gets a device key) and shows the
  admission QR code. Every further device scans or types the code.
- The identity must be moved, never copied. The tool deletes it from the
  old device after a successful hand-over and says so.
- The home node's listener for clients is configured with
  `mode = access_point` by `loquax home init`, or the defaults of a shipped
  home profile set it.

### 3.7 What changes where

| Component | Change |
|---|---|
| loquax | daemon mode (decided 2026-08-08 as option C, `docs/src/concepts/loquax-architecture.md:536-545`); home destination and protocol; archive sequence; client mode; admission and revocation; outbox with a 7-day policy |
| lnsd (leviculum-std) | a per-TCP-interface liveness option (keepalive idle, interval, user timeout); default unchanged, Python parity kept |
| leviculum-core | expose the per-link keepalive override to the caller who opens the link (it exists, `leviculum-core/src/link/mod.rs:1531`) |
| leviculum-lxmf | nothing required; optional: a server-side retain policy for the blind-mailbox compatibility mode |
| lnpnd | nothing required; separately, honour `prioritise_destinations` |
| firmware | nothing for slices 1 to 4; M9 client mode later; sleep and wake-on-radio only after measurement |

### 3.8 Compatibility with Python peers

- To every peer, the home node is an ordinary `lxmf.delivery` destination
  that announces at the usual cadence and accepts DIRECT, OPPORTUNISTIC and
  PROPAGATED-collected mail. No wire change.
- Outbound, the home node is an ordinary LXMF sender. Propagating and
  retrying direct with the same packed message is something a stock sender
  can also produce; the recipient dedups.
- The home destination `loquax.home` is a new aspect. Peers only see its
  announce, if it is announced at all. It need not be: clients learn it at
  admission, and the home node answers their path requests itself
  (`reference/Reticulum/RNS/Transport.py:2939-2940`).
- AP mode and a different TCP keepalive on our own listener change nothing
  that Python expects from a neighbour: AP is a reference mode, and TCP
  keepalive is a socket option.
- The deviation rule is not touched.

## 4. Measurements proposed

1. **Problem 1 as a number.** A periculum soak cell:
   - Setup: two peered stock lxmd PNs; four stock clients on three
     identities, one identity on two devices.
   - Traffic: 200 propagated messages over 2 h; recipients fetch at
     staggered intervals; one PN given a small storage limit for one run.
   - Count, per transient id: uploaded, SENT, stored at each PN, offered to
     the peer, listed to a client, downloaded, decrypted, purged.
   - Attribute every missing message to one of the seven mechanisms in §1.
     The `periculum trace` tool follows a transient id through all logs.
   - Run the same cell with lnpnd in the PN seats. The client driver must
     be the same code (drop-in rule).
   - Acceptance: a table of losses per mechanism, with counts.
2. **Phone battery, quiet link versus none.** No Android client exists, so
   the first measurement uses lnsd under Termux, which the 32-bit Android
   target already builds for.
   - Phone: Fairphone or Pixel, SIM, WLAN off.
   - Four runs of 24 h each, done twice: (a) no Reticulum; (b) one TCP
     interface to a home node with today's keepalives; (c) the same with
     TCP keepalive at 4.5 min and the link keepalive at 4.5 min; (d) ntfy
     instant delivery as the reference.
   - Read `dumpsys batterystats` and the battery percentage, and count
     wakeups.
   - In parallel, a NAT timeout probe on Lew's carrier: idle TCP intervals
     of 2, 5, 10, 20 and 30 min, counting which ones survive.
3. **Board baseline, then wake-on-radio.**
   - PPK2 on the Pocket V2 (SX1262), as operating-modes already asks.
     Measure idle today, then MCU sleep with the radio in continuous RX,
     then `SetRxDutyCycle` at periods of 0.5, 1, 2 and 4 s.
   - Per setting: average current; 100 wakes from a home-node test sender,
     counting how many are caught and the preamble each needs; and an
     RNode neighbour's delivery during the wake preambles, counting losses.
   - The LR1110 (M9, T1000-E) after it.

## 5. Slice plan

1. **Home archive and pull (loquax).** Daemon mode on an always-on Linux
   host, the home destination, admission with a token, `/sync` with a
   cursor, and loquax client mode on a laptop pulling on its existing
   timer.
   - Acceptance: a stock Python sender delivers DIRECT to the home node and
     gets DELIVERED; two loquax clients both see the message; neither
     deletes it.
2. **Send through the home node.** `/send`, the outbox with direct,
   propagate-and-keep-trying, and a 7-day horizon.
   - Acceptance: the recipient is offline for 2 h, comes back and
     announces; the message arrives direct with a proof; the client was
     asleep the whole time.
3. **Quiet link.** The TCP liveness option in lnsd, the per-link keepalive
   for the home link, AP mode in the home profile, and a `/wake` push.
   Gated on measurement 2.
4. **Multi-client state.** `/mark`, read and sent state on every client.
5. **Pairing UX.** QR code in the loquax GUI, text code in the TUI,
   revocation, identity hand-over from a one-device setup.
6. **Separate small fix, any time.** lnpnd honours
   `prioritise_destinations`.
7. **M9 as a client** over WLAN and LoRa, with pull and quiet link.
8. **Board sleep and wake-on-radio:** after measurement 3, and only if it
   pays.
9. **Android:** with whatever Android client exists; UnifiedPush and the
   pager rungs there.

Measurement 1 runs alongside slice 1. It does not block it.

## 6. Open questions for Lew

1. **Device keys or identity copies?** Device keys (recommended) mean only
   our clients can use a home node. Identity copies would let Columba act
   as a client today, but give up the delivery proof and the protection
   against a lost phone. Do you want the blind-mailbox compatibility mode
   as well?
2. **A VPS home node:** acceptable as a supported setup, or only a PC or Pi
   at home?
3. **Archive retention on the home node:** forever, like a mail archive, or
   purge once every admitted device has synced?
4. **One host as public PN and home node:** with lxmd's model, the PN and
   the delivery identity are the same key. Should a home node also offer a
   public PN under a *separate* identity, so the user's address is not
   tied to a public service?
5. **Outbox horizon:** 7 days of direct retries after the first
   propagation, or a different number?
6. **Phone client:** a Columba compatibility mode first, or wait for our
   own Android app over `leviculum-ffi`? This decides when the smartphone
   rungs happen.
7. **Priority:** this serves the second half of the Leitstern (carrying
   messages reliably). Does slice 1 queue behind the open BLE and
   propagation findings, or run alongside them in lane 2?
