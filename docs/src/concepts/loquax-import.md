# Loquax: import and export

Status: slice 1 built (order 1103, Refs #232). The survey of the other
messengers' formats, read from their public source only, is the draft
`docs/drafts/loquax-import-2026-10.md` (order 1091); this page keeps what
is decided and what is built. The user's side is the manual,
`loquax(1)`, "IMPORT AND EXPORT".

## Why

A user switching messengers loses everything today: the address their
correspondents wrote down, the names they gave people, the conversations.
The first start is when a messenger wins or loses a user. Every LXMF app
stores the identity as the same 64 bytes (the X25519 private key, then the
Ed25519 one), so taking the address over is a copy, not a format problem.

## Decided

Lew, 2026-10-29:

- **Adopting a foreign identity** when Loquax already has its own: the old
  identity and its history are set aside, kept and recoverable, and Loquax
  switches. Several identities at once come later.
- **Ratspeak**: identity only, never history.
- **`retain_synced_on_node`** is not offered at import. Loquax only warns
  not to run both apps on one identity at the same time.
- **Attachments** are reported as skipped until Loquax has an attachment
  store.
- **The export** (NomadNet's container shape) is part of slice 1 and is the
  round trip test of the import.

From the draft, kept:

- Transport identities are never imported; Loquax attaches to a daemon and
  owns none.
- The scan looks only at the documented default places, one level deep,
  and reads no more than the line it shows. A path the user names is read
  as whatever it is. Phone data directories are never read; a phone app's
  export file, carried over by hand, is.
- Three questions, in this order: contacts (default yes), history (default
  yes), identity (default no, a typed `yes` after the warning).
- Dedup on the LXMF message id, 1070's rule, so a re-run is idempotent and
  the same history from two apps merges.
- Never key material or message text in a log.

## Built in slice 1

| Source | What comes over |
|---|---|
| NomadNet tree | identity, named peers as petnames, `ignored` as blocks, the propagation node, the history |
| Loquax export | the same, through the same reader |
| raw 64-byte identity file (NomadNet, MeshChat, MeshChatX, Sideband, Ratspeak unsealed) | identity |
| pasted Base32 key, or a file with one | identity |
| Ratspeak `identity.enc` | listed as protected, not read |

The code is `loquax/src/import/` and `loquax/src/export.rs`, behind the
`history` feature; the core sees neither files nor formats.

### Where the history goes

Rows of the store are scoped by our own address
(`docs/src/concepts/loquax-storage.md`). Imported history is written under
the address it belongs to, the source's identity. Once that identity is
Loquax's, it is the live history; until then it waits in the store. Showing
a foreign address's history beside the current one belongs with several
identities, which come later.

### Order

The store orders a conversation by `seq`. Imported history is older than
what Loquax wrote, so a conversation's imported rows get keys below the
store's lowest (negative ones), in the source's time order. A second import
into the same conversation lays its whole imported block out again below
the lowest key, in one transaction, so older messages that arrive later
still go first and Loquax's own rows keep their keys. The block is deleted
and inserted, not renumbered, so the text index (keyed by `seq`) follows
through its triggers. The screen's model counts keys unsigned; the store
hands it `seq` with the sign bit flipped, which keeps the order and leaves
every older store's rows where they were.

Schema 6 adds two columns to `messages`: `origin` (the source's tag,
`nomadnet` or `loquax`, NULL for Loquax's own rows) and `origin_verdict`
(what the source said of the signature).

### Verification

Loquax's own verdict comes only from checking the signature itself:

- A message sent by the history's identity is checked against that
  identity's key, which the source tree holds.
- A received message whose bytes are intact, which is when they hash to
  the id the file is named by, goes into `unverified` with its bytes, and
  is settled when its sender's key arrives, as any received message is.
- NomadNet rewrites a message without its attachments and keeps its name,
  so its bytes no longer hash to its id. Such a message is unverified, and
  its ledger says `imported from NomadNet` and `NomadNet said: signature
  verified` (from NomadNet's `.index`): the source's word, never ours, and
  never "invalid" either, since the change is NomadNet's, not a forgery's.

### The identity

The 64 bytes go into Loquax's own envelope
(`leviculum-core/src/identity_store.rs`), written under a temporary name
with mode 0600 and moved into place. An identity already there with
another address is renamed to `identity.<address>.bak`, never deleted; its
history stays under its address, and moving the file back switches back.
On a client of a home node, adoption is refused: the client answers on the
home node's address.

### The export

`loquax export <dir>` writes NomadNet's tree with LXMF's
`packed_container` per message, the raw identity (mode 0600), the
contacts as NomadNet's directory and `ignored`, and the propagation node.
A marker file `storage/loquax-export` tells the import the tree is
Loquax's.

The store keeps a message's text and fields, not the bytes it travelled
as, except while a received message waits on its sender's key. So the
export writes those bytes where it has them, signs Loquax's own sent
messages again (their id then differs from the travelled one, and the file
keeps the old id as its name), and gives any other received message an
empty signature plus Loquax's verdict in an extra container key,
`loquax_verification`, which NomadNet ignores.

The round trip, export then import into an empty Loquax with the identity
adopted, brings back the same messages under the same ids: sent messages
verified, held messages held with their bytes, and messages Loquax had
verified on arrival unverified with `Loquax said: signature verified` in
their detail view. That last line is the one place the round trip is not
exact, and the cure is the next item below.

## Next

- Keep the signed bytes of every received message in the store, so the
  export carries them and the round trip is exact.
- Sideband, directory and profile tarball (the phone path; a tar reader).
- MeshChat and MeshChatX, the shared tables and the MeshChatX JSON export.
- Columba's `.columba` (AES-GCM, PBKDF2), with the id check the draft
  names.
- Ratspeak `identity.enc` and `.rsi` (Argon2id).
- The palette's `import` as a screen with the three questions and a
  pasted key, instead of contacts and history only.
