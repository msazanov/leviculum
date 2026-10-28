# loquax: importing identities and histories from other LXMF messengers

Draft, leviculum 1091, 2026-10-28. Refs #232. Concept only, no code.

Lew's idea (2026-10-28): on the very first start loquax offers to bring
over the user's identity and chats from the messenger they used before;
later the same tool is the palette's `:import`. Desktop first (NomadNet,
MeshChat and MeshChatX, Ratspeak desktop), then the phone apps (Columba,
Sideband, Ratspeak Android), where Lew suspected the sandbox makes it
impossible.

Everything below is read from public source only. No user data directory
of any messenger was opened on any host.

## Sources

Shallow clones under `/home/lew/scratch/1091/`, read 2026-10-28. Citations
below are `<repo> path:line` against these commits.

| Repo | Upstream | Commit | Licence (LICENSE head) |
|---|---|---|---|
| NomadNet | github.com/markqvist/NomadNet | b2f852c | GPLv3 |
| LXMF | github.com/markqvist/LXMF | c3ff2d6 | Reticulum License |
| RNS | github.com/markqvist/Reticulum | e40191b | Reticulum License |
| Sideband | github.com/markqvist/Sideband | ef572b5 | CC BY-NC-SA 4.0 (some vendored libs MIT) |
| MeshChat | github.com/liamcottle/reticulum-meshchat | a9f9259 | MIT |
| MeshChatX | github.com/Quad4-Software/MeshChatX (mirror of git.quad4.io/RNS-Things/MeshChatX) | 4378392 | 0BSD for Quad4's part, MIT upstream part |
| Ratspeak | github.com/ratspeak/Ratspeak | 7da1fe3 | AGPL-3.0-or-later |
| rsReticulum | github.com/ratspeak/rsReticulum | bf7d16f | AGPL-3.0-or-later |
| rsLXMF | github.com/ratspeak/rsLXMF | b5c3365 | AGPL-3.0-or-later |
| Columba | github.com/torlando-tech/columba | 9222e380 | MPL 2.0 |

MeshChatX paths are under `meshchatx/` (backend files under
`meshchatx/src/backend/`) unless they start with `electron/`. Columba
paths written with `.../` abbreviate `app/src/main/java/network/columba/app/`
or `data/src/main/java/network/columba/app/data/`; `find -name` resolves
them uniquely.

Every source was reachable; nothing needs fetching by hand. The
git.quad4.io URL refused the clone ("Not found"), the GitHub mirror the
MeshChatX README names worked.

On licences: we read formats and write our own readers, we copy no code.
A file format is not covered by copyright, so none of these licences binds
an importer, NomadNet's GPLv3, Sideband's NonCommercial clause and
Ratspeak's AGPL included. Two notes. The Reticulum License (LXMF and RNS,
`LXMF LICENSE:1-29`) is MIT-style plus two use restrictions (harm, AI
training data, `:12-18`); we already live with it as the reference. And
Ratspeak's vault (below) is a KDF and cipher composition from public
primitives, which we reimplement from its description, never from its
source text.

## The common ground: one identity format

Every app, all six, stores or exports the user's LXMF identity as the
same 64 bytes: the X25519 private key, then the Ed25519 private key
(`RNS RNS/Identity.py:724-729`, written raw by `to_file`, `:655-666`).
No header, no version. Every app also has the 16-byte delivery hash as
its address, so importing these 64 bytes gives loquax the same address
the user had. loquax's own `$LNMSG_HOME/identity` is a different
envelope around the same key (magic, version, checksum,
`leviculum-core/src/identity_store.rs:38-48`), so adopting an identity is
`Identity::from_private_key_bytes` then `encode_identity`.

Four apps (Sideband, Columba, MeshChatX, Ratspeak) also export that key
as Base32 text (RFC 4648 of the 64 bytes): `Sideband sbapp/ui/keys.py:79`,
`Columba app/src/main/java/.../IdentityManagerViewModel.kt:502-530`,
`MeshChatX meshchatx/src/backend/http/routes/identities.py:38-62`,
`Ratspeak crates/ratspeak-tauri/src/commands/identity.rs:1262-1283`.
That makes Base32 paste the de facto identity interchange of the
ecosystem, and the cheapest import of all: it reaches a phone without a
cable.

None of the apps uses its delivery identity as the Reticulum transport
identity. NomadNet, Sideband and the MeshChats leave transport to RNS
(`<rnsdir>/storage/transport_identity`, `RNS RNS/Transport.py:320-327`);
Ratspeak keeps one per profile in `identities/<h>/reticulum/`
(`Ratspeak crates/ratspeak-runtime/src/lib.rs:1867-1870`); Columba's kt
backend mints a fresh one in memory at every start
(`Columba rns-backend-kt/.../NativeRnsBackendImpl.kt:676-686`). loquax
never owns a transport identity either (it attaches to lnsd or rnsd), so
**transport identities are never imported**.

## Per-app findings

### Summary table

| App | Identity at rest | Encrypted? | History store | Raw LXMF kept? | Export file | Import verdict |
|---|---|---|---|---|---|---|
| NomadNet | `<cfg>/storage/identity`, raw 64 B | no | a file per message, msgpack container with `lxmf_bytes` | yes, but attachments stripped | none | full, from the data dir |
| MeshChat | `<storage>/identity`, raw 64 B | no | SQLite (peewee), `lxmf_messages` | no, columns plus JSON fields | none | full minus signatures, from the data dir |
| MeshChatX | same, several profiles | no (mode 0600) | SQLite, `lxmf_messages` (schema 59) | no | messages JSON v2, backup zip, identity zip | full minus signatures, data dir or export |
| Ratspeak desktop | `identity` raw, or `identity.enc` vault, or a hardware key | optional passcode vault | SQLite `ratspeak.db` (schema 45) | no, no fields column | identity only (raw, Base32, `.rsi`) | identity yes; history lossy, data dir only |
| Sideband desktop | `app_storage/primary_identity`, raw 64 B | no | SQLite `sideband.db`, `lxm.data` = packed LXMF | **yes, whole** | profile `.tar.gz` | full, best fidelity |
| Sideband Android | same, app private | no | same | yes | profile `.tar.gz` to Downloads | **full, via the tarball** |
| Columba (Android) | Room row, Keystore wrapped | yes (device key) | Room `messages` (version 10) | no, `fieldsJson` | `.columba` (password, AES-GCM), identity file, Base32 | full minus signatures, via `.columba` |
| Ratspeak Android | as desktop, app private | optional vault | as desktop | no | identity only | identity only |

"Raw LXMF kept" decides fidelity: where the signed wire bytes are kept,
loquax can check the signature itself and round-trip every field
(`docs/src/concepts/loquax-storage.md`, "fields stored as raw msgpack");
where they are not, the message is rebuilt from columns and its signature
is gone for good.

### NomadNet (1.4.4)

- **Where.** `/etc/nomadnetwork` if it holds a `config`, else
  `~/.config/nomadnetwork` if it holds one, else `~/.nomadnetwork`
  (`NomadNet nomadnet/NomadNetworkApp.py:29-35`). No per-OS branch, so
  macOS and Windows use `~/.nomadnetwork` under the user's home (inferred
  from `expanduser`).
- **Identity.** `storage/identity`, raw 64 bytes, unencrypted
  (`NomadNetworkApp.py:113, 244, 256-258`). It is the LXMF delivery
  identity (`:373-384`).
- **Messages.** `storage/conversations/<peer hex>/<64 hex msg hash>`
  (`NomadNetworkApp.py:119`; `Conversation.py:58-66, 249`). Each file is
  LXMF's own `packed_container()`: msgpack `{state, lxmf_bytes,
  transport_encrypted, transport_encryption, method}`
  (`LXMF LXMF/LXMessage.py:667-679`), written temp plus rename
  (`:681-692`). `lxmf_bytes` is dest(16), source(16), signature(64),
  msgpack `[timestamp, title, content, fields, (stamp)]` (`:360-391`).
  Direction is not stored: a message is outbound iff its source is our
  own delivery hash (`NomadNet nomadnet/ui/textui/Conversations.py:2339`).
- **The catch.** On arrival NomadNet moves image, audio and file fields
  out to `storage/attachments/<msg hash>/file_N` with a msgpack
  `manifest` and rewrites `lxmf_bytes` without them
  (`Conversation.py:806-865, 946-992`). The stored signature then no
  longer verifies; the file keeps the original hash as its name. The
  `.index` sidecar per conversation (msgpack, `Conversation.py:997-1067`)
  holds NomadNet's own verdict (`signature_validated`), RSSI, SNR and Q,
  which exist nowhere else.
- **Contacts.** `storage/directory`, msgpack `{entry_list,
  announce_stream}`; an entry is `(hash, display_name, trust_level,
  hosts_node, preferred_delivery, identify, sort_rank, notes)`, shorter
  in older files (`Directory.py:91-147`). Trust: WARNING 0, UNTRUSTED 1,
  UNKNOWN 2, TRUSTED 0xFF (`:413-416`). Node entries are keyed by the
  `nomadnetwork.node` hash, not the LXMF one (`:393`). Blocked peers:
  `<cfg>/ignored`, one hex hash a line (`NomadNetworkApp.py:108, 332-349`).
- **Own name, propagation node.** `storage/peersettings`, msgpack:
  `display_name`, `propagation_node` (16 bytes or None)
  (`NomadNetworkApp.py:121, 265-318`).
- **Versions.** No version marker anywhere; every reader is tolerant
  (optional tuple fields, `if key in container`). An importer treats
  every field as optional. No pickle anywhere.

### MeshChat (Liam Cottle)

- **Where.** Electron: `~/.reticulum-meshchat` on every OS, portable
  Windows build next to the exe (`MeshChat electron/main.js:95-101,
  205-206`). pip/CLI: `./storage` unless `--storage-dir`
  (`meshchat.py:3611-3616, 3659`).
- **Identity.** `<storage>/identity`, raw 64 bytes
  (`meshchat.py:3667-3669`). One per storage dir.
- **Messages.** `<storage>/identities/<identity hex>/database.db`,
  SQLite via peewee (`meshchat.py:52-68`). `lxmf_messages`: `hash`
  (the LXMF id, hex), `source_hash`, `destination_hash`, `state` and
  `method` as strings, `is_incoming`, `title`, `content`, `fields` as
  JSON, `timestamp` (sender clock), `rssi`, `snr`, `quality`,
  `created_at` (`database.py:113-137`). Only image, audio and file
  attachments survive, inline as Base64 in the JSON; telemetry and other
  fields are dropped (`meshchat.py:2454-2497`). No signature column.
- **Contacts, own settings.** `custom_destination_display_names`,
  `announces` (with the peer's public key, Base64), `favourite_destinations`
  (NomadNet nodes) (`database.py:64-110`). Config table:
  `display_name`, `lxmf_preferred_propagation_node_destination_hash`
  (`meshchat.py:3365-3427`). No blocklist, no trust.
- **Versions.** `database_version` config key, 5 at this commit
  (`database.py:6-43`).

### MeshChatX (Quad4 fork)

- **Where.** Electron: `~/.reticulum-meshchatx`
  (`MeshChatX electron/mainHelpers.js:121-151`); `--data-dir` or
  `MESHCHAT_DATA_DIR` (`src/path_utils.py:539-560`). It already imports
  MeshChat's directory on first start
  (`src/backend/legacy_migrator.py:65-129`), so one reader for the
  shared tables covers both.
- **Identity.** Several profiles, `identities/<h>/identity` (raw 64 B,
  mode 0600) plus `metadata.json` with the display name
  (`identity_manager.py:171-217`).
- **Messages.** Raw sqlite3, WAL; `lxmf_messages` as MeshChat plus
  `peer_hash`, `reply_to_hash`, `has_telemetry`, `attachments_stripped`
  and more (`database/schema.py:242-275`); fields JSON grows telemetry,
  reactions and replies as decoded values (`lxmf_utils.py:712-858`).
  `database_version`, `LATEST_VERSION = 59` (`schema.py:50`): it moves
  fast.
- **Contacts.** `contacts` (name, identity hash, LXMF address,
  `is_telemetry_trusted`), `blocked_destinations`
  (`schema.py:298, 462-472`).
- **Exports.** Messages JSON `{"format": "meshchatx/messages/v2",
  messages, contacts, display_names, ...}`
  (`message_export_bundle.py:11, 87-111`); a backup zip with the database
  and the identity tree and `backup-manifest.json`
  (`database/__init__.py:811-912`); identity as `identity.bin`, Base32 or
  a zip of all profiles (`http/routes/identities.py:38-62, 136-155`).
  The JSON export carries a format name and version and is the stablest
  thing to read; the live database is not.

### Ratspeak (desktop and Android, alpha, 1.0.35)

- **Where.** Tauri `app_data_dir()` for `org.ratspeak.desktop`
  (`Ratspeak src-tauri/src/paths.rs:7-20`, `tauri.conf.json:5`), then
  `.ratspeak/` (`crates/ratspeak-core/src/config.rs:20-76`). On Linux
  that is `~/.local/share/org.ratspeak.desktop/.ratspeak/` by Tauri's
  convention (not checked in Tauri's source). `RATSPEAK_DATA_DIR`
  overrides.
- **Identity.** Per profile `identities/<h>/`, exactly one of three
  (`crates/ratspeak-runtime/src/lxmf.rs:2098-2112`):
  - `identity`: raw 64 bytes, the default for a new profile, next to
    `identity.seed`, a plaintext 12-word phrase (`vault.rs:28, 762-772`).
  - `identity.enc`: the passcode vault Lew met in October. JSON
    `{version: 1, kdf: "argon2id", m_cost, t_cost, p_cost, salt, token}`;
    Argon2id, then HKDF-SHA256 to 64 bytes, then the RNS Token scheme
    (AES-256-CBC plus HMAC-SHA256) over the 64-byte key
    (`vault.rs:182, 310-366`; `rsReticulum crates/rns-crypto/src/token.rs:47-77`).
    Enabling it deletes the plaintext files (`vault.rs:522-563`). The
    passcode is typed by the user, never stored.
  - `identity.hwid`: public metadata only, the key on a PIV token
    (`rsReticulum crates/rns-ratkey/src/hwid.rs:1-2`). Not importable,
    by design.
- **Messages.** `ratspeak.db`, plain SQLite (rusqlite bundled, no
  SQLCipher), `schema_version` 45, newer refused
  (`crates/ratspeak-db/src/db.rs:11, 139`). `messages`: `id` (LXMF hash,
  hex), `source`, `destination`, `identity_id`, `title`, `content`,
  `timestamp` (local observed time, not the wire time), `state`,
  `direction`, `delivery_method` (`db.rs:268-296`). No fields column, no
  signature column; only the first file attachment is kept
  (`crates/ratspeak-runtime/src/lib.rs:4340`), media under
  `identities/<h>/files/`. Channels are Reticulum Relay Chat, not LXMF
  (`rrc.rs:1-14`), and are out of scope.
- **Contacts.** `contacts` (display name, public key, `trust`),
  `blocked_contacts`; own name and propagation node in the `identities`
  row (`db.rs:241-320`).
- **Exports.** Identity only: raw file `reticulum.raw-private-key`,
  Base32, or an encrypted `.rsi` (`{"format": "ratspeak.identity.v2",
  vault: <as above>, display_name, ...}`, sealed with a separate backup
  PIN) (`crates/ratspeak-tauri/src/commands/identity.rs:29, 1240-1363`).
  No message or contact export. Ratspeak itself imports raw RNS identity
  files (`:236-372`), so the raw file is interchange in both directions.

### Sideband (desktop and Android)

- **Where.** `~/.config/sideband` on every desktop OS (via plyer's home
  dir; `Sideband sbapp/sideband/core.py:209`), `--config` overrides;
  Android `.../io.unsigned.sideband/files` (`core.py:223`).
- **Identity.** `app_storage/primary_identity`, raw 64 bytes
  (`core.py:261-267, 455-459`).
- **Messages.** `app_storage/sideband.db`. Table `lxm`: `lxm_hash`,
  `dest`, `source`, `title`, `tx_ts`, `rx_ts`, `state`, `method`, `data`,
  `extra` (`core.py:2085`). **`data` is `lxm.packed`, the whole signed
  wire message** (paper messages: msgpack `[packed, paper_packed]`,
  `core.py:3125-3128`); `extra` is msgpack (RSSI, SNR, stamps, telemetry,
  `core.py:3130-3148`). Table `conv`: `dest_context`, `unread`, `trust`
  (0 or 1), `name` (UTF-8 bytes), `type` (1 P2P, 2 group, 3 broadcast,
  4 voice) (`core.py:105-108, 2088`). No version marker; one ad hoc
  `ALTER TABLE` (`core.py:2120-2128`). No pickle.
- **Settings.** `app_storage/sideband_config`, msgpack: `display_name`,
  `lxmf_propagation_node` (`core.py:464-479`).
- **Exports.** "Backup Sideband Profile": a `.tar.gz` of all of
  `app_storage` under the arcname `Sideband Backup`, written to the
  Downloads folder (`sbapp/ui/keys.py:45-55`). It holds identity,
  database and settings: **everything an importer needs, on Android
  too**. Its restore button is disabled (`keys.py:203-206`). Identity:
  Base32 export and import (`keys.py:79-130`).

### Columba (Android)

- **Identity.** Room table `local_identities`, several profiles, the key
  AES-256-GCM under an Android Keystore key
  (`Columba .../entity/LocalIdentityEntity.kt:15-48`;
  `IdentityKeyEncryptor.kt:39, 94`). An Android backup of the app is
  therefore useless off the device even though `allowBackup="true"`
  (`app/src/main/AndroidManifest.xml:100`): the wrapping key never leaves
  the Keystore.
- **Messages.** Room, version 10 with exported schemas
  (`.../db/ColumbaDatabase.kt:73-74`). `messages`: `id`, `identityHash`,
  `conversationHash`, `content`, `timestamp`, `receivedAt`, `isFromMe`,
  `status`, `fieldsJson` (field number to hex), `signatureVerified`
  (`entity/MessageEntity.kt:8-86`). No title, no packed LXMF. This is
  the schema loquax's own store started from
  (`docs/src/concepts/loquax-storage.md`).
- **Exports.** The migration file `.columba`: a ZIP of `manifest.json`
  (kotlinx `MigrationBundle`, version 8) plus attachments
  (`.../migration/MigrationExporter.kt:34-36, 485-510`;
  `MigrationData.kt:12-35`), carrying identities (Base64 of the 64
  bytes), conversations, messages, contacts, settings including the
  propagation node (`MigrationData.kt:322-328`). The UI always encrypts
  the whole file with a password: byte `0x03`, PBKDF2-HMAC-SHA256 600 000
  iterations, AES-256-GCM in 8 MiB chunks with per-chunk IV and AAD
  (`MigrationCrypto.kt:4-84, 434-461`). Identity alone: a raw
  `.rnsidentity` file or Base32 (`IdentityManagerViewModel.kt:372-530`).
  Columba already imports Sideband's profile tarball, identity only
  (`IdentityManagerViewModel.kt:547-561`).

## Android: not out of reach

Lew's suspicion holds for data directories and fails for the apps. The
sandbox keeps loquax out of `/data/data/...`, and `adb backup` is no way
round it (Columba's keys are Keystore wrapped, see above, so even a
working backup would carry an unreadable key). But three of the four
phone apps write an export file the user can carry over by hand:

| Phone app | Export | Gets loquax |
|---|---|---|
| Sideband | profile `.tar.gz` in Downloads | identity, all messages with signatures, names, PN |
| Columba | `.columba` with its password | identity, messages (no signatures), contacts, PN |
| MeshChatX (Android build) | messages JSON, backup zip | identity, messages (no signatures), contacts |
| Ratspeak | raw identity file, Base32, `.rsi` | identity only |

So the desktop importer reads export files, not phone directories: the
user copies the file over (cable, Syncthing, mail to self) and points
`:import` at it, or loquax finds it in `~/Downloads`. Base32 paste covers
the identity for every app with nothing to copy at all.

## Verdict

**Partly yes.** Worth building for identity, contacts and history from
the four sources that keep their data readable (NomadNet, Sideband,
MeshChat and MeshChatX, Columba's export), with identity adoption as a
separate, warned step. Not worth building: Ratspeak history (lossy, no
export, schema moving every week of an alpha), transport identities,
Ratspeak channels (not LXMF), any reading of phone data directories.

Reasons for:

- Every source has the same identity bytes, so adopting an address is a
  64-byte copy, not a format problem.
- Two sources keep the signed wire bytes (Sideband whole, NomadNet minus
  attachments), so for them the import is exact: loquax stores the
  LXMF message as it travelled.
- The formats are plain (msgpack, SQLite, JSON, ZIP). No pickle, no
  SQLCipher. The only cryptography is in two optional containers
  (Ratspeak vault, Columba export), both built from standard primitives.
- A switcher today loses everything; the first start is exactly when a
  messenger wins or loses a user.

Reasons against, and their answers in the design below:

- **Running both apps on one identity** breaks delivery in ways a user
  will not understand (next section). Answer: identity adoption is never
  the default and says so before it happens.
- **The formats are not contracts.** NomadNet and Sideband have no
  version marker at all; MeshChatX is at schema 59, Ratspeak at 45.
  Answer: readers are tolerant (every field optional), refuse what they
  do not recognise with the reason, prefer versioned exports
  (`meshchatx/messages/v2`, `MigrationBundle` 8, `ratspeak.identity.v2`)
  over live databases, and each importer pins the schema versions it was
  tested against.
- **Rebuilt messages lie a little.** From MeshChat, Ratspeak and Columba
  there is no signature to check and some fields are gone. Answer: such
  rows say where they came from and are never shown as verified by us.

## The risks, and how the design handles them

### One identity in two apps

If the user adopts the identity and keeps the old app running:

- Both announce the same delivery destination with different app data
  (names), so peers see the name flap and their path tables point at
  whichever announced last. Direct and opportunistic delivery reaches one
  of the two; the other never sees those messages.
- Both sync from the same propagation node. loquax tells the node to
  delete what it collected (`retain_synced_on_node` false,
  `leviculum-lxmf/src/router/propagation_runtime.rs:73`;
  `docs/src/concepts/loquax-mailbox.md`, "The purge default"), and the
  other apps do the same by default, so the first to sync takes the mail.
- Both may answer the same message with a delivery proof; harmless on
  the wire, but the sender's view of "delivered to whom" is wrong.

Handling: identity adoption is a separate question after the history
question, default **no**, and requires typing `yes` after a warning that
names the old app and these three consequences. After adoption loquax
offers (does not force) `retain_synced_on_node = true` for as long as the
user runs both, with the privacy cost stated. A running source app
cannot be detected reliably across six apps, so loquax does not pretend
to; the warning is the mechanism.

### Private keys copied

- The 64 bytes go straight from the source file into loquax's
  `encode_identity` envelope, written temp plus rename with mode 0600;
  the importer never writes the raw key anywhere else and never logs it
  (the log line names the source path and the resulting address only).
- Adoption never overwrites an existing loquax identity silently: the
  current `$LNMSG_HOME/identity` is renamed to `identity.<address>.bak`,
  not deleted, and only when its address holds no history; with history
  the user is told that the old address's conversations stay in the
  store under the old address (rows are identity scoped,
  `loquax-storage.md`, "Every row scoped by local identity") and can be
  reached by switching back. Whether loquax should hold several local
  identities as a feature is an open question for Lew.
- Vault and `.columba` passwords are asked for, used, and dropped; never
  stored, never logged.

### Legal and ethical

The importer reads the user's own data from their own account on their
own machine, or a file they exported. It never reads another OS user's
home, never scans outside the documented locations, and never phones
home. That is the whole boundary; it needs stating in the manual, not
a mechanism.

## The flow

### When it runs

- **First start** (no `$LNMSG_HOME/identity` and no history): before
  minting, loquax scans the known locations (list below). Found nothing:
  mint as today, no screen. Found something: the import screen. The user
  can skip it, and skipping is one key.
- **`:import`** in the palette, any time; also `loquax import [--dry-run]
  [PATH]` on the command line for scripts and for files, matching
  `send`, `fetch` and `address`.

### What is scanned

Only documented default locations, no recursive search:
`~/.nomadnetwork`, `~/.config/nomadnetwork`, `/etc/nomadnetwork`;
`~/.config/sideband`; `~/.reticulum-meshchat`, `~/.reticulum-meshchatx`;
`~/.local/share/org.ratspeak.desktop` (and the macOS and Windows
equivalents); plus files in `~/Downloads` matching
`sideband_backup_*.tar.gz`, `*.columba`, `*.rnsidentity`,
`*-reticulum-identity.identity`, `*.rsi`, MeshChatX export names. A PATH
given to `:import` or `loquax import` is read as whatever it turns out to
be (directory or file, sniffed by content).

### What is shown

One line per source found, before anything is read beyond what
identification needs:

```
NomadNet  ~/.nomadnetwork        address <9c31e0a2…>  412 messages, 23 contacts
Sideband  ~/Downloads/sideband_backup_2026-10-01.tar.gz
                                 address <4f2a77b0…>  1 830 messages, 41 contacts
Ratspeak  ~/.local/share/…       address protected by a passcode
```

Then three separate questions, in this order:

1. **Contacts and names** (petnames into `$LNMSG_HOME/contacts`, blocks,
   the announced names into `names`): default yes.
2. **History**: default yes. History without the identity is useful: the
   conversations are readable and searchable, just under the old address,
   which is then shown as "imported from NomadNet, address <…>" in the
   conversation list.
3. **The identity**: default no, the warning above, typed `yes`.

`--dry-run` (and the screen's `d`) lists what would be written, per
conversation, with counts of new, already held, and unreadable messages,
and writes nothing.

### Writing

- **Dedup** on the LXMF message id, 1070's rule (`loquax-storage.md`,
  "One row per received message"): a message whose id the store holds for
  the same local identity and direction is skipped. Ids come from the
  source: the file name (NomadNet), `lxm_hash` (Sideband), `hash`
  (MeshChat), `id` (Ratspeak); Columba's `id` is assumed to be the LXMF
  hash and must be checked against its exporter before that importer
  lands. A re-run is therefore idempotent, and importing the same history
  from two apps (a user who used NomadNet and Sideband on one identity)
  merges.
- **Order.** The store orders a conversation by `seq`, the screen's own
  counter (`loquax-storage.md`, "The key"). Imported history is older
  than what loquax holds, so imported rows get `seq` values below the
  current minimum (SQLite rowids may be negative), in the source's time
  order. A second import into a conversation that already holds imported
  rows rewrites that conversation's imported block in one transaction, so
  order stays right without renumbering loquax's own rows.
- **Provenance.** A new column `origin` (NULL for loquax's own rows, a
  short tag like `nomadnet` otherwise) and schema 4. The detail view
  shows it.
- **Verification.** Where the signed bytes are intact (Sideband;
  NomadNet messages without attachments), loquax checks the signature
  itself: valid is 1, invalid is 2, sender key unknown goes into the
  `unverified` table with its bytes like any received message
  (`loquax-storage.md`, "Unverified messages keep their bytes"). Where
  the bytes are gone, the row is 0 (unverified) whatever the source app
  claimed; the source's verdict is shown as "NomadNet said: verified",
  never as ours.
- **Timestamps.** `timestamp` gets the sender's clock where the source
  has it; `received_at` gets the source's receive time (Sideband
  `rx_ts`, Columba `receivedAt`, NomadNet `.index`), else NULL.
- **Fields.** Raw msgpack where the packed message exists. Rebuilt
  sources (MeshChat JSON, Columba hex map) are re-encoded to msgpack for
  the fields they kept; attachments go to the content-addressed
  `attachments/` directory once that exists (it does not yet,
  `loquax-storage.md`, "Retention"), until then they are counted and
  named in the import log as not imported.
- **One transaction per conversation**, `BEGIN IMMEDIATE` as every other
  write (`loquax-storage.md`, "Writes"). An interrupted import leaves
  whole conversations or none, and a re-run fills the rest.
- **Log.** `$LNMSG_HOME/import.log` and the tui log: one `IMPORT` line
  per source and per conversation with counts (`IMPORT source=nomadnet
  conv=<hex> new=12 held=400 unreadable=0`), one per refused file with
  the reason. Never key material, never message content.
- **Settings.** The source's propagation node becomes loquax's
  `propagation_node` only if loquax has none; the own display name is
  offered, not taken.

## Where it lives

- **Shell, not core.** File access, SQLite and archive readers are std
  only. A module `loquax/src/import/` behind the `history` feature, one
  file per source, all behind one trait:

  ```
  trait Source {
      fn probe(&self) -> Option<Found>;          // cheap: name, path, address, counts
      fn identity(&self, secret: Option<&str>) -> Result<Option<[u8; 64]>, ImportError>;
      fn contacts(&self) -> Result<Vec<Contact>, ImportError>;
      fn messages(&self) -> Result<Box<dyn Iterator<Item = Result<Imported, ImportError>>>, ImportError>;
  }
  ```

  `Imported` carries either the packed LXMF bytes (and the engine
  unpacks and checks them with the library we already have,
  `leviculum-lxmf`) or the rebuilt columns. The core sees neither files
  nor formats.
- **No foreign code.** Every reader is written from the formats above.
  The MeshChat and Columba schemas are read by column name with
  `rusqlite`, which loquax already links.
- **New dependencies**, each a `just notices-guard` matter: a tar reader
  (flate2 is already in `Cargo.lock`) for Sideband; `zip`, `aes-gcm` and
  `pbkdf2` for Columba; `argon2` for the Ratspeak vault (HKDF, AES-CBC
  and HMAC are already there for the RNS token). Each slice adds only
  its own.

## Better ideas, and what is nonsense

- **Base32 paste is the best single feature.** Four apps export it, it
  works from any phone with no file transfer, and it is a dozen lines.
  It belongs in slice 1, as part of the identity question ("or paste a
  key").
- **Contacts only** is a real use case (a user trying loquax beside their
  old app, deliberately not adopting the identity) and the cheapest
  importer step. It should not wait for history.
- **An export from loquax** in the shape the ecosystem already shares:
  LXMF's own `packed_container` msgpack, one file per message, as
  NomadNet stores it (`LXMF LXMF/LXMessage.py:667-692`), plus the raw
  64-byte identity. That is the closest thing to a standard the
  ecosystem has; writing it costs little, makes loquax a good citizen
  (a user can leave as easily as they came), and gives our own importer
  its round-trip test. A new cross-app format (a "lxmf-archive" spec)
  is a fine idea for the ecosystem and not ours to lead now.
- **Sharing the identity through the daemon instead of copying keys** is
  nonsense here: every one of these apps runs its own LXMF router
  inside the app, the shared instance carries transport only, so there
  is nothing to share through. The real cure for the two-apps problem is
  not technical; it is telling the user.
- **Reading phone data directories** (root, `adb backup`, `run-as`) is
  nonsense: unreliable, wrong for keys wrapped in a Keystore, and it
  teaches users to root phones for a messenger.

## Slice plan

Each slice is one batch with its own tests, fixtures built from the
formats above (never from real user data), and a page in the manual.

1. **Identity and contacts, NomadNet and raw files.** The `Source`
   trait, the first-start probe, the three-question screen, `:import`,
   `loquax import --dry-run`, Base32 paste, raw 64-byte files from any
   app, NomadNet `directory`, `ignored` and `peersettings`. Adoption
   with the warning and the `.bak` rename. Fixtures: a NomadNet tree and
   raw identity files written by the test.
2. **History, NomadNet.** Schema 4 (`origin`), negative `seq` block,
   dedup, verification on intact bytes, `.index` for receive time and
   the source's verdict, the import log. Positive control: a fixture
   message signed by a test identity verifies; the same with an
   attachment stripped is stored unverified with NomadNet's verdict
   shown.
3. **Sideband**, directory and profile tarball (same reader). Highest
   fidelity, and the phone path.
4. **MeshChat and MeshChatX**: the shared tables, then the MeshChatX JSON
   export.
5. **Columba `.columba`**: the chunked AES-GCM container, the bundle,
   the id check named above.
6. **Ratspeak identity**: raw file and Base32 come free with slice 1;
   this adds `identity.enc` and `.rsi` (Argon2id). History only if Lew
   wants it after seeing the loss list above, and only against a pinned
   schema version.
7. **Export** in NomadNet's container shape, with an import of loquax's
   own export as the round-trip test.

Slices 1 and 2 deliver the desktop story for the oldest user base; 3 and
5 deliver the phone story; 4 and 6 are order-free after 2.

## Open questions for Lew

1. Identity adoption when loquax already has history under its own
   address: rename aside and switch (as above), or refuse until the user
   chooses, or make several local identities a real loquax feature now?
2. Ratspeak history: worth a lossy importer for an alpha whose schema
   moves weekly, or identity only?
3. Should loquax offer `retain_synced_on_node = true` after adoption, or
   only print the warning?
4. Attachments: import them only after the `attachments/` store exists
   (then slice 2 counts them as skipped), or build that store as part of
   the import work?
5. Export (slice 7): wanted now, or later?
