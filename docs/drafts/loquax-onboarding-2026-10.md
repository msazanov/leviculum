# Loquax and Loquax Mailbox: onboarding for people who are not technical

Order 1149, Refs #232. A concept, no code. It builds on what is built:
the home node (`docs/src/concepts/loquax-home-node.md`), the propagation
node rule (`docs/src/concepts/loquax-mailbox.md`), the Android study
(`docs/drafts/loquax-android-2026-10.md`) and the home node draft
(`docs/drafts/home-node-2026-10.md`). Where this page says "measured" it
names the measurement; everything else is reasoned or cited and marked so.

## 0. The recommendation in short

1. Keep **Loquax** (the app) and **Loquax Mailbox** (the always-on part).
   German UI: "dein Postfach", product name untranslated.
2. Today's "mailbox" (the propagation node) becomes **relay** in the UI
   (German "Relais"); "propagation node" stays in advanced settings and the
   man page. The one state file and one log word get a migration.
3. The first start asks one question: mailbox on this phone, or on a
   computer. "On this phone" is preselected and needs nothing else; it is
   also the way to start before the computer is ready.
4. The phone mailbox is the existing `loquax --daemon` role inside the
   app's foreground service. The screens talk to it as a client, over an
   in-process interface, with the same protocol as to an external one.
5. Nobody forwards a port. The mailbox and the phone both dial out to a
   public transport node (`leviculum.network`), and the phone finds the
   mailbox through it. This needs a phone-facing port in `access_point`
   mode there; unmeasured, the cell to measure it is in section 6.
6. The wake stays the held Reticulum link (1123). UnifiedPush is an option
   for later, not the default: it needs a second app and, with ntfy, is the
   same always-on connection in another process. A phone mailbox needs no
   wake at all.
7. Moving the mailbox is a hand-over: the old one stops before it sends,
   the new one starts only after it has everything. Mail that arrives in
   the gap waits at a relay.
8. The wizard on the computer ends with the QR code it already prints
   (1121), carrying more than one address: the LAN one and the rendezvous.
9. Local case: 5 steps, 1 decision. External case: 8 steps, 1 decision
   (section 7).

## 1. Names

### The always-on part

Lew's proposal is "Loquax Mailbox". Candidates weighed:

| Name | For | Against |
|---|---|---|
| **Loquax Mailbox** | says what it does: mail arrives there and waits; everybody knows a mailbox | clashes with today's UI word for the propagation node (below) |
| Loquax Home | the code says `home` already (`loquax home admit`, `loquax.home`) | "home" is also the home screen and `$LNMSG_HOME`; says nothing about mail |
| Loquax Station | short, sounds like hardware that stays on | says nothing about mail |
| Loquax Server | honest | scares exactly the users this page is for |

**Recommendation: Loquax Mailbox.** In German the UI says "dein Postfach"
in running text and keeps the product name "Loquax Mailbox" in headings,
package names and support texts, so a German user searching the web finds
the English documentation too. The code keeps `home` (the destination
`loquax.home`, the code prefix `lqxh1`, the config keys): they are on the
wire or in files and do not need to change for the user to read
"Mailbox". The CLI gets `loquax mailbox ...` as the documented spelling
and keeps `loquax home ...` and `--daemon` as aliases.

### The propagation node needs another word

The UI says "mailbox" for the propagation node today. With "Loquax
Mailbox" for the user's own device, a status line "mailbox 9c31e0a2
adopted" would name somebody else's node as the user's mailbox. That is
not a cosmetic clash: the mailbox page argues at length that the
propagation node sees the envelope of every message and is chosen, not
trusted (`docs/src/concepts/loquax-mailbox.md`, "Which node, and the trust
question").

**Proposal: "relay"** (German "Relais"), and "propagation node" only in
advanced settings, the man page and the logs. A relay is somebody else's
machine that passes mail on, which is what it is. It also reads right in
the delivery state: "waiting at a relay" instead of "in the mailbox".
Alternatives: "drop box" (implies it is yours), "post office" (long),
"Zwischenspeicher" (German only, technical).

### Inventory of the user-visible word today

On master at 4d5fac94, `grep -rn -i '"[^"]*mailbox[^"]*"'` over `loquax`
and `loquax-android`, test lines dropped, plus the docs. The Android app
shows none: it maps the state to "sent" (`loquax-android/src/session.rs:1245`).

| Where | What the user sees | New text |
|---|---|---|
| `loquax/src/ui.rs:2513` | `via mailbox` on a bubble | `via relay` |
| `loquax/src/ui.rs:2534` | `in the mailbox` (state) | `waiting at a relay` |
| `loquax/src/ui.rs:2574` | toast `fetching the mailbox` | `checking the relay` |
| `loquax/src/ui.rs:3514-3543` | title `no mailbox`, `mailbox 9c31e0a2 adopted · synced 14:05 · next 14:20` and its shorter forms | `no relay`, `relay 9c31e0a2 ...` |
| `loquax/src/keys.rs:687` | help line `fetch the mailbox` | `check the relay` |
| `loquax/src/mailbox.rs:91-127` | the reasons an adoption gives (`no mailbox was selected ...`, `no mailbox chosen: ...`) | `relay` for `mailbox` |
| `loquax/src/mailbox_state.rs:164-174` | `mailbox: 9c31e0a2 kept`, `... adopted: ...` on stderr | `relay: ...` |
| `loquax/src/mailbox_state.rs:227` | `could not record the mailbox: ...` | `... the relay: ...` |
| `loquax/src/shell.rs:751` | toast `mailbox <new> adopted, <old> did not answer` | `relay ...` |
| `loquax/src/fetch.rs:131` | `the mailbox sync failed: ...` | `checking the relay failed: ...` |
| `loquax/src/tui.rs:1849` | `no mailbox chosen: no propagation node has been heard` | `no relay chosen: ...` |
| `loquax/src/tui.rs:797` | key script word `until delivery mailbox` | accept `relay` and keep `mailbox` |
| `loquax/src/main.rs:496` | source word `mailbox` in the `LNMSG_PN` line | `relay`; the event catalogue (`leviculum-std/src/event_log.rs:783`) names the word |
| `docs/src/man/loquax.1.md` | section MAILBOX and 16 lines | section RELAY; MAILBOX becomes the home node's section |
| `docs/src/concepts/` | `loquax-mailbox.md` (29), `loquax-keys-and-layout.md` (21), `loquax-architecture.md` (16), `loquax-ui.md` (15), `loquax.md` (6), `zero-config.md` (3), `loquax-storage.md` (2), `loquax-map.md` (2), `handheld-messenger.md` (1) occurrences | rename `loquax-mailbox.md` to `loquax-relay.md`, with the old name kept as a redirect line in `SUMMARY.md` |

Not user-visible to a Loquax user and left alone: lnpnd's own wording
(`docs/src/man/lnpnd.1.md`, `packaging/lnpnd/lnpnd.service:2`), which
describes a propagation node to its operator; the LXMF appendix; the
structured events `MAILBOX unanswered` (`loquax/src/shell.rs:722`, `:733`)
and `PN_MAILBOX`, which are grep keys with stable names; the type
`DeliveryState::InMailbox`.

**Config keys and files.** The keys are already neutral: `propagation_node`
and `sync_interval` in `$LNMSG_HOME/config`, `--pn`, `--sync-interval`.
They stay. One file carries the word: `$LNMSG_HOME/mailbox`, the persisted
selection (`loquax/src/mailbox_state.rs:32`). Migration: read `relay`,
else `mailbox`; write only `relay` and remove `mailbox` after the write
succeeded; an older Loquax that finds no `mailbox` adopts again, which is
the documented behaviour for a missing file and loses nothing but the
adoption. The free name `mailbox` is then not reused for a year, so a
downgrade never reads a relay selection as something else.

**The home node strings on the phone** become Mailbox in the same pass:
`res/values/strings.xml` says "home node", the German file mixes
"Home-Node" and "Heimknoten" (`values-de/strings.xml:54`, `:127`). All
become "Mailbox" / "Postfach".

## 2. The first start, screen by screen

Rules: one question per screen, the recommended answer preselected, no
word from this page's technical vocabulary on a screen (no node, link,
identity, propagation, server). Texts are final drafts, EN then DE.

### Screen 1: welcome

> **Loquax**
> Messages that do not depend on any company.
> [Start]

> **Loquax**
> Nachrichten, die von keiner Firma abhängen.
> [Los geht's]

No settings, no terms. One screen because Android wants the app opened
before it asks for anything.

### Screen 2: where your mailbox lives

> **Where should your mailbox be?**
> Your mailbox receives your messages and keeps them for you.
>
> (•) **On this phone**
>     Ready now. Messages reach you while the phone is on and online.
> ( ) **On a computer at home**
>     Messages arrive even when your phone is off. You need a computer or
>     a Raspberry Pi that stays on.
>
> You can move your mailbox later without losing anything.
> [Continue]

> **Wo soll dein Postfach sein?**
> Dein Postfach empfängt deine Nachrichten und bewahrt sie für dich auf.
>
> (•) **Auf diesem Telefon**
>     Sofort startklar. Nachrichten erreichen dich, solange das Telefon an
>     und online ist.
> ( ) **Auf einem Computer zu Hause**
>     Nachrichten kommen auch an, wenn dein Telefon aus ist. Du brauchst
>     einen Computer oder Raspberry Pi, der eingeschaltet bleibt.
>
> Du kannst dein Postfach später umziehen, ohne etwas zu verlieren.
> [Weiter]

The last line matters most: it takes the weight off the only decision.

### Screen 3a: on this phone, what that means

> **Your mailbox on this phone**
> - Loquax keeps running in the background. It uses some battery; we will
>   show you how much.
> - When the phone is off or has no internet, messages wait at a relay in
>   the network and arrive when you are back. The sender sees "delivered"
>   only then.
> - Your address lives on this phone. Save a backup, or a lost phone means
>   a new address.
> [Use this phone] [Back]

> **Dein Postfach auf diesem Telefon**
> - Loquax läuft im Hintergrund weiter. Das kostet etwas Akku; wir zeigen
>   dir, wie viel.
> - Ist das Telefon aus oder offline, warten Nachrichten bei einem Relais
>   im Netz und kommen an, sobald du wieder da bist. Der Absender sieht
>   "zugestellt" erst dann.
> - Deine Adresse wohnt auf diesem Telefon. Sichere sie, sonst bedeutet
>   ein verlorenes Telefon eine neue Adresse.
> [Dieses Telefon nehmen] [Zurück]

"We will show you how much" commits us to the battery line in the
persistent notification (section 3). Without the measurement the sentence
is dropped, not softened.

### Screen 3b: on a computer

> **Set up your mailbox on the computer**
> On the computer, open **leviculum.network/mailbox** and follow the
> steps. At the end it shows a code. Scan it here.
> [Scan the code] [Paste a code]
> [Use this phone until then]

> **Richte dein Postfach auf dem Computer ein**
> Öffne auf dem Computer **leviculum.network/mailbox** und folge den
> Schritten. Am Ende zeigt er einen Code. Scanne ihn hier.
> [Code scannen] [Code einfügen]
> [Bis dahin dieses Telefon nehmen]

"Use this phone until then" makes the phone the mailbox at once and turns
the later scan into a hand-over (section 4), so the user can write the
first message before the computer exists.

After the scan:

> **Connect to your mailbox?**
> Mailbox "Lew's Pi" at 192.168.1.20
> [Connect]

> **Mit deinem Postfach verbinden?**
> Postfach "Lews Pi" unter 192.168.1.20
> [Verbinden]

then "Connecting...", "Verbinde..." until the first pull ends.

### Screen 4: background permission

Both paths need it: the phone mailbox to stay reachable, the client to
hold the wake link. Asked once, before the system dialog
(`ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`), with the notification
permission (Android 13 and later) on the same screen.

> **Let Loquax run in the background**
> Otherwise Android stops it when the phone lies still, and messages are
> late. Android will ask you twice now: allow both.
> [Continue]

> **Lass Loquax im Hintergrund laufen**
> Sonst hält Android es an, wenn das Telefon still liegt, und Nachrichten
> kommen zu spät. Android fragt dich gleich zweimal: erlaube beides.
> [Weiter]

A refusal is accepted; the persistent notification then says "Messages may
be late: background use not allowed" with a button to ask again.

### Screen 5: done

> **Your address**
> [QR code]  `a1b2 c3d4 ...`
> Show this to people who should write to you.
> [Share] [Write a message]
> Your name for others: [__________] (optional)
> (phone mailbox only) [Save a backup of your address]

> **Deine Adresse**
> [QR-Code]  `a1b2 c3d4 ...`
> Zeig das den Leuten, die dir schreiben sollen.
> [Teilen] [Nachricht schreiben]
> Dein Name für andere: [__________] (freiwillig)
> (nur Postfach auf dem Telefon) [Adresse sichern]

The display name is optional here and asked again at the first send if
empty; it is not a gate. The backup is offered here and again after the
first received message, as a notification, once.

## 3. A mailbox on the phone

### What runs where

| Part | Today (client phone) | Phone mailbox |
|---|---|---|
| Process | `LoquaxService` (`remoteMessaging`), the session | the same service |
| Node | in-process leviculum-std node, one TCP client to the home node's lnsd | in-process node, TCP client to the entry servers, AutoInterface on WLAN |
| Identity | device identity only | the LXMF identity, plus the device identity for the screens |
| Engine | `Engine::client` (registers no delivery destination) | the home node engine of `loquax --daemon`: `lxmf.delivery`, archive, outbox, relay collection |
| Screens | client of `loquax.home` over TCP | client of `loquax.home` over an in-process interface |
| Wake | `/watch` over the held link (1123) | `/watch` over the in-process link: free |

The screens stay a client in both cases. That is what makes the move
cheap: the client code, the cursor, the marks, the names are one code path
whether the mailbox is in the same process or on a Pi. The delivery lock
(`loquax/src/home/lock.rs`) has its analogue: the service holds the
mailbox engine once, and a second start refuses.

### Battery, what is known and what is not

Nothing on the phone has been measured; the measurement of the Android
study's section 3 is still open. Known costs, from code and reference:

- **TCP keepalive every 5 s** on an idle connection, mirrored from Python
  (`leviculum-std/src/interfaces/tcp.rs:62-78`; the home node draft,
  problem 5). This keeps the modem awake and is the first knob, for the
  client as much as for the phone mailbox. The phone mailbox has no link
  of its own to keep alive: senders open links to it.
- **The announce stream.** A TCP connection to a transport node in full
  mode gets every announce that node forwards. The home node on hamster
  heard enough strangers to archive 184 name entries in nine minutes before
  1136 filtered them (`docs/src/concepts/loquax-home-node.md`, "Names the
  home node heard"). An `access_point` interface on the server side sends
  no broadcast announces at all (`reference/Reticulum/RNS/Transport.py:1194-1197`),
  which is the reason for the phone port of section 6.
- **Own announces** every 360 minutes, and the relay check on the
  schedule (15 minutes bound). Small.

What the user is told: the persistent notification carries a line
"Battery: about N % a day" once N is measured on the device. Until then
it says nothing about battery, and screen 3a drops "we will show you how
much".

### Reachability

- **Online, app running:** senders deliver direct over the entry server,
  with a proof, as to any LXMF destination.
- **Offline, off, or in Doze without the exemption:** senders' direct
  attempts fail and their clients fall back to the relay (LXMF's fallback
  lives in the sending app, after about 40 s, home node draft section 2);
  the phone collects on reconnect. Senders without a relay keep retrying
  or give up, which is the cost the warning names.
- **Outgoing:** the outbox retries direct on every announce of the
  recipient and hourly for seven days, and propagates a copy
  (`docs/src/concepts/loquax-home-node.md`, "Sending through the home
  node"), but only while the phone is online. A phone off for a week sends
  nothing for a week.

The phone mailbox is good for a start and for a user who is online most of
the time. It is not good for somebody whose phone is often off; screen 2
says so in one line.

## 4. Moving the mailbox

The rule from the home node draft, problem 4: one identity never answers
on two devices at once. Ratchets make a double answer lose mail on both.

### Phone to computer

1. The wizard on the computer, fresh, has no identity yet. It shows its
   QR code: a hand-over-capable join code with the computer's temporary
   destination and its addresses (section 6).
2. The phone scans it. The app sees that it holds a mailbox itself and
   asks: "Move your mailbox to Lew's Pi? Your phone becomes a reader of it.
   Takes about a minute." / "Dein Postfach auf Lews Pi umziehen? Dein
   Telefon liest dann dort mit. Dauert etwa eine Minute."
3. The phone **stops being the mailbox first**: it deregisters
   `lxmf.delivery`, stops the outbox, waits until its in-flight sends ended
   or timed out, and writes a state `handing-over` to its store.
4. It sends one bundle as a resource over a link to the computer: the
   identity, the destination's ratchet keys (where leviculum-std keeps them,
   `leviculum-std/src/storage.rs:310`; the slice locates them for the
   in-process and the shared-instance case), the archive, the outbox, the
   contacts, the device list with the phone's own device identity admitted,
   the relay selection and the config keys that apply. The bundle carries
   its SHA-256.
5. The computer checks the hash, writes everything in one transaction,
   answers `taken`, and only then starts: registers, announces, collects
   from the relay.
6. The phone, on `taken`, deletes the identity and the ratchet keys, keeps
   its history as a client copy with the archive cursor of the bundle, and
   is a client from then on.

Mail in the gap (from step 3 to the first announce in step 5) finds no
destination; senders fall back to the relay, and the computer collects it
in step 5. Nothing is lost that a sender propagated; a sender with no
relay retries as it would for any offline recipient.

**Failure.** Before `taken`, the phone may take its mailbox back: the link
broke or the computer refused, the phone still has everything, and it
resumes. After the bundle went out and no `taken` came, the phone does
**not** resume by itself, since the computer may have started. It asks the
computer over a fresh link (`/handover-status`) and resumes only on an
answer "not taken"; with no answer it stays stopped and says so: "Move not
confirmed. Is Lew's Pi on?" with [Try again] and, behind a confirmation
that names the risk, [Take my mailbox back].

### Computer to phone

The phone is already an admitted client. The authority to give the
identity away must come from the mailbox side, not from a client, since a
stolen admitted phone must not be able to pull the identity off the Pi.

1. On the computer: `loquax mailbox hand-over --to <device>`, or the
   button in the GUI wizard. It opens a single-use token for that device,
   valid ten minutes.
2. The phone gets a wake, shows "Lew's Pi wants to move your mailbox to
   this phone. Accept?" and, on accept, asks `/handover` with the token.
3. The computer runs steps 3 to 6 above in the giving role; the phone runs
   them in the taking role and starts the in-process mailbox. The computer
   ends as a stopped mailbox with no identity, and the wizard offers to
   uninstall or to stay a client.

The same bundle format serves both directions and a third case: the backup
of screen 5 is the bundle written to a file, encrypted with a passphrase,
and restoring it is the taking role from a file.

## 5. App to mailbox: the link wake or UnifiedPush

**Recommendation confirmed: the link wake first, UnifiedPush as an
option.** The facts:

- **UnifiedPush is a wake, not a transport.** A message is 1 to 4096 bytes
  encrypted (the Android spec, https://unifiedpush.org/developers/spec/android/,
  read 2026-10-30). Loquax would send an empty wake and pull over
  Reticulum as now.
- **It needs a distributor.** The spec defines the distributor as its own
  component with its own receivers; in practice it is a second app the user
  installs and chooses (ntfy, or one bound to Google's FCM). That is one
  more install and one more decision, against section 7's count.
- **ntfy is an always-on connection too.** ntfy's own documentation says
  instant delivery "is achieved with a foreground service, which you'll see
  as a permanent notification", and that it works "even when your phone is
  in doze mode" (https://docs.ntfy.sh/subscribe/phone/, read 2026-10-30).
  So for a user whose only push app is Loquax, ntfy replaces our
  connection by its own, plus an ntfy server the mailbox must reach. It
  wins only when ntfy is already installed for other apps and the one
  connection is shared.
- **Doze.** The spec has the distributor raise the app to foreground
  importance for 5 s with each message, or bind its
  `RAISE_TO_FOREGROUND` service, which lets the app start a foreground
  service from the background. 5 s is short for a Reticulum pull over
  mobile (path, link, `/sync`), so the app would start its service on a
  wake and pull from there. Delivery also follows RFC 8030 urgency: on
  battery without WLAN only `normal` and above get through, on low battery
  only `high`.
- **The one case UnifiedPush wins outright** is an FCM-backed distributor
  on a phone with Google services: Google's connection is there anyway and
  Doze lets it through. Not checked here which distributors exist for that
  and how they behave; it is the reason to keep the option.
- **Our own link** needs the battery exemption and the foreground service,
  both already built (1116), and its cost is the open measurement. Its two
  known knobs (TCP keepalive, link keepalive) are in our hands.

**A phone mailbox needs no wake at all**: the screens and the mailbox are
one process, and `/watch` over the in-process link costs nothing.

## 6. The external mailbox

### Reachability without touching the router

| Option | Works without router changes | Cost | Verdict |
|---|---|---|---|
| **Rendezvous over a public transport** | yes: both sides dial out | depends on our server; it sees encrypted link traffic and timing | **default** |
| Same LAN (AutoInterface, or the LAN address in the code) | yes, at home only | none | always on, tried first |
| IPv6 direct | only where the router's firewall lets inbound IPv6 through, which home routers usually block by default (not measured here) | a router rule | advanced only |
| UPnP / NAT-PMP / PCP port mapping | only where the router has it on; no use behind CGNAT or DS-Lite, where the home has no public IPv4 (RFC 6333) | a protocol client in lnsd | not now |
| A relay we host for TCP | same as the rendezvous, but a new service to run | operating duty | the rendezvous already is one |
| Yggdrasil | needs a daemon on both ends; on Android a VPN slot | an install per device | no |
| I2P | Reticulum has an I2P interface, but needs i2pd and minutes to build tunnels | slow, an install | no |

**How the rendezvous works, and its one gap.** The mailbox's lnsd keeps a
TCP client to `leviculum.network` (the first entry of the zero-config
server list, `docs/src/concepts/zero-config.md`, "The server list"; not
shipped in code yet). The phone's node does the same. The phone asks for
the path to `loquax.home`, which is never announced. The transport node
answers or forwards an unknown path request only when the interface it
arrived on is in a discovering mode, access point, gateway or roaming
(`reference/Reticulum/RNS/Transport.py:2917-2918`,
`reference/Reticulum/RNS/Interfaces/Interface.py:54`; ours matches,
`leviculum-core/src/transport.rs:12450-12451`). It then forwards the
request on every interface, including the mailbox's TCP connection; the
mailbox's lnsd forwards it to its local clients, transport on or off
(`reference/Reticulum/RNS/Transport.py:3042-3047`), and Loquax answers for
its local destination. In full mode the request is dropped as "no path
known".

So the server needs a **phone port in `access_point` mode**: it discovers
paths for unknown destinations and sends phones no broadcast announces
(the battery point of section 3). The mailbox itself dials the normal full
port, since it wants the network's announces (names, recipients' keys).
Proposal: `leviculum.network:4242` full, a second port, for instance
`:4243`, access point. The alternative, the mailbox announcing
`loquax.home`, works on any port but tells the whole network which
addresses use a Loquax Mailbox; rejected.

**Unmeasured.** The cell to measure it: three containers, a transport
node with one full and one access point TCP server, a mailbox on the full
one, a client on the access point one; the client joins with a code that
carries only the rendezvous and pulls one message. Positive control: the
client on the full port fails with the path request ignored. Then on the
real `leviculum.network` once the operating duties of the zero-config page
stand (monitoring, a privacy statement).

A side effect worth keeping: the client reaches the mailbox by its
destination, not by an IP address. A laptop can be a client of a **phone**
mailbox through the rendezvous the same way, with no server on the phone.

### What the QR code carries

The 1121 code, extended:

```text
lqxh1<base32 code>@192.168.1.20:4242,leviculum.network:4243#Lew's Pi
```

- the code as today: the mailbox identity's public key, the single-use
  token, the checksum; base32 in the QR alphanumeric segment as now
  (`loquax/src/home/qr.rs:30-32`);
- behind `@`, a comma-separated list of addresses in the byte segment, the
  app trying all at once, the LAN one first; today's single address is the
  list of one, so old codes read unchanged;
- behind `#`, the mailbox's name for the confirm screen (screen 3b),
  percent-encoded, optional.

Neither `@`, `,` nor `#` is in the base32 alphabet, and none occurs in a
host:port except inside brackets, which the IPv6 form already has. The
size grows from version 6 to about version 8 at level M, still easy to
scan from a screen. A fresh mailbox's code is also its hand-over code: it
has no identity yet and takes one from the first scan (section 4), or
makes one when the first scanning phone has none to give.

### The wizard

One program, `loquax mailbox setup`, text by default, windows when a
display is there (`WAYLAND_DISPLAY` or `DISPLAY`), the same steps:

1. **Check.** lnsd runs, the shared instance answers, the disk has room for
   the archive cap. A failure says what to do in one line.
2. **Start on boot.** Install and enable the service. Proposal: the package
   ships a system unit `loquax-mailbox.service` with its own user, ordered
   after `lnsd.service`, instead of today's user unit with `loginctl
   enable-linger` and the "could not join the shared instance" retry
   (`docs/src/concepts/loquax-home-node.md`, "Running it"). One question
   fewer, and no ordering race.
3. **Reachability.** Test what works, say what was found: "At home: yes
   (192.168.1.20). From anywhere: yes, through leviculum.network." No
   choice unless the test fails.
4. **Name.** "What should this mailbox be called on your phone?" prefilled
   with the hostname.
5. **The code.** QR code and text, valid 24 hours (`home_token_hours`),
   with "Waiting for your phone..." until a device joins, then "Your phone
   is connected." The wizard stays open until then.

The identity backup is not a wizard step: it is offered on the phone,
which has a share sheet, and on the computer by `loquax mailbox backup`.

The URL on screen 3b (`leviculum.network/mailbox`) is a page with one
download per platform and one command line for the shell; for a Debian
or Raspberry Pi OS user, one line that adds the repository and installs
`loquax`, which pulls in `lnsd`.

### Platforms

| Platform | Package today | Wizard | Notes |
|---|---|---|---|
| Debian, Ubuntu, Mint on amd64 | `.deb`, musl static (`scripts/build-deb.sh:58-61`) | text and GUI | the reference case |
| Raspberry Pi OS 64-bit, Pi 3/4/5 | `.deb` arm64 | text over SSH, GUI with a desktop | the recommended always-on box |
| Raspberry Pi OS 32-bit, Pi Zero / Pi 2 | none (no armhf build) | none | a build target to add, or "use the 64-bit image" |
| VPS | the `.deb`s | text | allowed (decided 2026-10-29); the identity and plaintext on rented hardware, the wizard says so once |
| Other Linux | the static binaries in a tarball | text | no service file of theirs; documented by hand |
| Router (OpenWrt) | none | none | a static aarch64 binary may run; flash and RAM limit the archive; not now |
| Windows, macOS | none | none | an always-on home PC on Windows is common; open question |
| Our boards | none | none | no nRF board has IP; the M9 is the handheld, not the mailbox (home node draft, section 2) |

## 7. The ten-minute script

Counted from "found Loquax" to "first message received". A step is one
action of the user, a decision is a step where the user must choose.

### Local

1. Install Loquax from F-Droid or the APK link.
2. Open it, tap Start (screen 1).
3. Keep "On this phone", Continue (screen 2): **the decision**.
4. Use this phone (screen 3a).
5. Continue, allow twice in Android's dialogs (screen 4).
6. Share the address with a friend; the friend writes; the message
   arrives.

Six steps, one decision. Cut: screen 1 and screen 3a can be one screen
(the warning below the choice, shown when "On this phone" is selected),
which makes **five**. The name and the backup are on screen 5 and gate
nothing.

### External

On the computer:

1. Open `leviculum.network/mailbox`, run the one command (or install the
   `.deb`).
2. The wizard runs: check and start on boot need nothing.
3. Confirm the name.
4. The QR code appears.

On the phone:

5. Install Loquax.
6. Open it, Start.
7. "On a computer at home", Continue: **the decision**.
8. Scan the code, Connect.
9. Continue, allow twice.
10. Share the address; the friend writes; the message arrives.

Ten steps, one decision. Cut: the name step goes (the hostname is the
default, rename later) and screens 1 and 2 merge as above, which makes
**eight**. The one command is the step that is hardest for a
non-technical user and the one a GUI installer per platform would cut;
until then a user who cannot do it presses "Use this phone until then" and
is in the local case.

Not counted and not removable: Android's install from unknown sources if
the APK is not from a store, and the camera permission at the scan.

## 8. Slices and open questions

### Slices

1. **Words.** relay for the propagation node, Mailbox / Postfach for the
   home node, in UI, man page and docs; the `mailbox` file migration; the
   key script word; the CLI aliases. Small, no design left.
2. **The rendezvous cell.** The three-container measurement of section 6,
   then the access point port on `leviculum.network` and multi-address
   codes. It decides whether the external case works without a router.
3. **The phone mailbox.** The daemon role in `LoquaxService`, the
   in-process interface for the screens, the entry servers as the node's
   interfaces.
4. **The first start**, screens 1 to 5 in EN and DE, and the camera scan
   (Camera2 and a Rust QR decoder in the native library, no AndroidX).
5. **The hand-over**: the bundle, both directions, the failure states,
   the encrypted backup file. With a two-device end-to-end test and the
   ratchet case (a message to the newest ratchet sent during the move).
6. **The wizard**, text first, and the system unit in the package.
7. **The battery measurement** on the phone, both roles, and the keepalive
   tuning it drives; the battery line in the notification.
8. **GUI wizard** and the download page. UnifiedPush only after 7, and
   only if the measurement says the own link is too expensive.

### Open questions for Lew

1. **Names.** Loquax Mailbox, with "Postfach" in German running text? And
   "relay" / "Relais" for the propagation node, or another word?
2. **The phone port on leviculum.network.** A second TCP port in access
   point mode, for every Loquax user: it makes our server part of the
   delivery path for external mailboxes, with the operating duties the
   zero-config page lists. Agreed?
3. **Preselection.** "On this phone" preselected on screen 2, as proposed,
   or no preselection?
4. **The camera.** A scanner in the app (Camera2, a Rust decoder, about a
   day of work, no Google) or the share-from-a-QR-reader path of 1121
   only?
5. **The service.** A system unit with its own user in the package,
   replacing the user unit and linger?
6. **Backups.** Passphrase-encrypted file through the share sheet, or also
   a printable paper code (the identity is 64 bytes, a QR code fits)?
7. **Windows and macOS** as mailbox hosts: wanted, and when?
8. **The VPS case.** One warning in the wizard and nothing else, or a
   separate path in the download page?
