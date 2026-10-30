# loquax on Android and the phone's address book: a study

Order 1138, 2026-10-29, Refs #232. Lew's question of the same evening: how
should loquax on Android keep the people we talk to, using the phone's own
infrastructure and above all its address book; one contact may carry one or
several LXMF addresses; linking is half automatic; once linked, loquax shows
the contact's photo and name.

Constraints from the order: platform APIs only (no Gradle, no AndroidX, no
Google services; minSdk 26, target 35), and nothing leaves the phone.

Nothing here is built. Everything was read from public source, shallow
clones in the order's scratch directory:

| Source | Commit |
|---|---|
| AOSP `platform/frameworks/base` | tag `android-15.0.0_r1` |
| AOSP `platform/packages/apps/Contacts` | tag `android-15.0.0_r1` |
| AOSP `platform/packages/providers/ContactsProvider` | tag `android-15.0.0_r1` |
| Signal-Android (github.com/signalapp) | `6ef1b67b` |
| Conversations (codeberg.org/iNPUTmice) | `a4c98082` |
| Threema Android (github.com/threema-ch) | `d44a2b53` |
| Delta Chat Android (github.com/deltachat) | `0b9ba6d3` |
| Briar (code.briarproject.org) | `ae48a901` |

Citations below are `repo:path:line` against these commits. The platform
guides cited are developer.android.com's "Contacts Provider"
(`/identity/providers/contacts-provider`, read 2026-10-29) and "Common
intents" (`/guide/components/intents-common`, "Contacts/people app").
Paths in this tree marked *(homenode)* are on branch `homenode`, not yet on
master.

## 0. Verdict

**loquax's own table is the truth; the address book is a presentation
layer the user opts into, in three steps that each work without the next.**

1. **Own table (model c), from slice 1.** Every LXMF address loquax knows
   stays in loquax's store, as now (the contacts file of 1047,
   `loquax/src/names.rs:77`). A *link* is a second column on that row:
   the contact's `LOOKUP_KEY` and last `_ID`, plus a copy of the contact's
   name and thumbnail taken when it was linked. Linking needs no permission
   when the user picks the contact (`ACTION_PICK`); suggestions and live
   updates need `READ_CONTACTS`, asked only when the user asks for them.
2. **The address on the contact itself, through the user's own editor.**
   "Save to contact" opens the system editor (`ACTION_INSERT_OR_EDIT`, or
   `ACTION_EDIT` for an already linked contact) prefilled with a standard
   IM row: `Im.PROTOCOL_CUSTOM` with `CUSTOM_PROTOCOL = "LXMF"` and the
   address hex. The user sees which account it lands in and saves it
   themselves; loquax writes nothing into a contact it does not own, as the
   platform guide asks. That row travels with the contact to a new phone
   (when the contact syncs), the AOSP card shows it and taps it into
   `imto://lxmf/<hex>`, which loquax handles, and a later loquax with
   `READ_CONTACTS` re-links from it. This is how Conversations stores
   XMPP addresses (§1.d).
3. **A local "loquax" account (model b), later and optional.** Only for
   the "Message on loquax" row with loquax's icon on the card, the way
   Signal and Threema do it: our own raw contact per linked address, joined
   to the user's contact by `AggregationExceptions.TYPE_KEEP_TOGETHER`, and
   a sync adapter that syncs nothing. It costs `WRITE_CONTACTS` and an
   entry under Settings > Accounts; it adds nothing that step 2's IM row
   does not already give on AOSP, so it waits until a phone measurement
   shows the IM row is not tappable in the contacts app people actually
   use (Google Contacts on the Pixel, the Fairphone's).

Model (a), our own MIME type inside the user's raw contacts, is rejected:
the AOSP contacts app hides it and its editor drops it (§1.a).

## 1. Storage models

### a. Custom MIME data row in the user's existing raw contacts

A row `vnd.android.cursor.item/vnd.network.leviculum.lxmf` with
`RAW_CONTACT_ID` of, say, the user's Google raw contact.

- **Permissions:** `WRITE_CONTACTS` to insert, `READ_CONTACTS` to read it
  back.
- **Shown on the card: no.** QuickContact builds its list per raw contact
  and asks the raw contact's *own* account type for the data kind; an
  unknown MIME type gets `null` and is skipped
  (`aosp-contacts:src/com/android/contacts/quickcontact/QuickContactActivity.java:1237-1242`,
  `aosp-contacts:src/com/android/contacts/model/AccountTypeManager.java:699-717`).
  Google's account type declares the standard kinds only
  (`aosp-contacts:src/com/android/contacts/model/account/GoogleAccountType.java:67`).
  A `contacts.xml` declaring our kind only applies to raw contacts of
  *our* account type: the contacts app loads it from the sync adapter
  service of the account type's own package
  (`aosp-contacts:src/com/android/contacts/model/account/ExternalAccountType.java:186-215`).
- **Through the editor: dropped.** `ACTION_INSERT` with an `Insert.DATA`
  row of a MIME type the target account does not know is logged and
  ignored ("Mimetype not supported for account type",
  `aosp-contacts:src/com/android/contacts/model/RawContactModifier.java:702-706`).
- **Sync and new phone:** the row belongs to another account's raw
  contact; that account's sync adapter has no reason to upload a kind it
  does not know. Not measured; either way it does not survive as data
  loquax can count on.
- **Against the guide:** "Change data only for those raw contacts that you
  own" (Contacts Provider guide, "Data integrity").
- **Verdict:** rejected. Invisible, not portable, and against the
  platform's own rule.

### b. Own account type with own raw contacts (Signal, Threema)

An account `network.leviculum.loquax` in `AccountManager`, one raw contact
of that account per linked address, carrying a StructuredName, a data row
of our MIME type, and an aggregation exception that keeps it with the
user's contact.

- **What it takes, all platform:** an `AbstractAccountAuthenticator`
  behind a bound service (intent `android.accounts.AccountAuthenticator`,
  `res/xml/authenticator.xml`), an `AbstractThreadedSyncAdapter` behind a
  second service (intent `android.content.SyncAdapter`,
  `res/xml/syncadapter.xml` with `contentAuthority="com.android.contacts"`),
  and on that service the `android.provider.CONTACTS_STRUCTURE` metadata
  pointing at `contacts.xml` with one `ContactsDataKind`. Signal's
  declarations: `signal:app/src/main/AndroidManifest.xml:1319-1343`,
  `signal:app/src/main/res/xml/syncadapter.xml`,
  `signal:app/src/main/res/xml/contactsformat.xml:3-8`; Threema's:
  `threema:app/src/main/AndroidManifest.xml:917-944`,
  `threema:app/src/main/res/xml/contacts.xml`. The classes are
  `android.accounts.AbstractAccountAuthenticator` and
  `android.content.AbstractThreadedSyncAdapter` in
  `frameworks/base/core/java/android/{accounts,content}/`; the guide
  ("Contacts Provider sync adapters", "Sync adapter classes and files")
  names no library. **Confirmed: no AndroidX needed.** Our make template
  already compiles `res/xml` through aapt2, so the two XML files are
  ordinary resources.
- **Creating the account:** `AccountManager.addAccountExplicitly`
  requires a signature match with the authenticator, which an app has
  with itself; `AUTHENTICATE_ACCOUNTS` is only for API 22 and older
  (`aosp-base:core/java/android/accounts/AccountManager.java:1002-1007`).
  Signal's call: `signal:lib/contacts/src/main/java/org/signal/contacts/SystemContactsRepository.kt:306-333`.
  A raw contact of an unregistered account is deleted by the provider
  (guide, "Raw contacts", Notes).
- **Joining:** `AggregationExceptions` with `TYPE_KEEP_TOGETHER`
  (`aosp-base:core/java/android/provider/ContactsContract.java:8268`,
  `:8309`), written with `CALLER_IS_SYNCADAPTER=true` (`:160`). Signal:
  `SystemContactsRepository.kt:624-699`, the exception at `:694-697`;
  Threema: `threema:app/src/main/java/ch/threema/app/utils/AndroidContactUtil.java:157-215`.
- **Permissions:** `READ_CONTACTS` and `WRITE_CONTACTS` (both run time,
  one "Contacts" group), plus the normal `READ_SYNC_SETTINGS` and
  `WRITE_SYNC_SETTINGS` for `setSyncAutomatically`. Signal and Threema
  also declare `GET_ACCOUNTS` and `AUTHENTICATE_ACCOUNTS`
  (`signal:app/src/main/AndroidManifest.xml:29,44,53,60,70,72`,
  `threema:app/src/main/AndroidManifest.xml:27-29,61-62,69-70`), which we
  do not need at minSdk 26 for our own account.
- **The card:** shows a row with our icon, `summaryColumn`/`detailColumn`
  from our data row, and tapping it sends `ACTION_VIEW` on the data row's
  URI with our MIME type; Signal's handler is an activity filtered on that
  MIME type (`signal:app/src/main/AndroidManifest.xml:711-719`).
- **Edits and merges:** the user can split or merge in the contacts app;
  the exception is a hint the user may override. Our raw contacts are
  ours, so a rebuild from loquax's table repairs them (Signal re-runs
  `addMessageAndCallLinksToContacts` from its sync adapter,
  `signal:app/src/main/java/org/thoughtcrime/securesms/contacts/ContactsSyncAdapter.java:38`).
- **New phone, backup:** our raw contacts are local; the sync adapter
  uploads nothing, so they do not come back. They are rebuilt from
  loquax's own table, which is what must be backed up.
- **Other apps:** every app with `READ_CONTACTS` sees our raw contacts and
  our data rows, the LXMF address included. The account is listed under
  Settings > Accounts.
- **Effort:** two services, two XML files, an authenticator whose every
  method says "not supported", a sync adapter whose `onPerformSync`
  reconciles from our table, and the batch writer. About as much Java as
  the rest of the address book work together.

### c. Own table with an optional link (Delta Chat, Briar, and every app's base)

The address and everything loquax knows stays in loquax's store; the link
is a `LOOKUP_KEY` (and the last seen `_ID` as a fast path).

- **Permissions:** none to keep the link. `READ_CONTACTS` to refresh the
  copied name and photo or to suggest; without it the picked contact is
  readable once, right after the pick (§3).
- **Robust:** the lookup key is the platform's own answer: "you can use
  `LOOKUP_KEY` to maintain links to favorite contacts" even when
  aggregation or sync changes `_ID` (guide, "Contacts";
  `ContactsContract.java:1066-1069`). The provider resolves a stale key
  by the source id of each raw contact first, then by raw contact ids,
  then by display name
  (`aosp-cp:src/com/android/providers/contacts/ContactsProvider2.java:8150-8178`,
  the segment kinds at `ContactLookupKey.java:27-30`). A merge keeps the
  link working; a split keeps it on one of the parts; a deleted contact
  turns it into a stale link, which loquax shows as unlinked.
- **New phone:** a contact that syncs (Google, CardDAV) comes back with the
  same source id, so a restored lookup key usually resolves. A local-only
  contact gets new ids and falls back to the name segment. Loquax's table
  comes with loquax's own export (1103), not with the phone.
- **Other apps:** see nothing.
- **Who does this:** everyone keeps their own table and treats the link
  as a cache. Signal stores `system_contact_uri`, `system_photo_uri`,
  `system_joined_name`, `system_given_name`, `system_nickname` per
  recipient
  (`signal:app/src/main/java/org/thoughtcrime/securesms/database/RecipientTable.kt:160-168`);
  Threema stores the lookup key per contact
  (`threema:app/src/main/java/ch/threema/storage/factories/ContactModelFactory.java:52,136-142`)
  and copies the photo into its own file
  (`AndroidContactUtil.java:100-134`). Delta Chat declares no contacts
  permission at all and keeps a dead `systemContactPhoto` path that is
  always `null`
  (`deltachat:src/main/java/org/thoughtcrime/securesms/recipients/Recipient.java:114,232`);
  it exchanges contacts as vCards. Briar declares no contacts permission
  (`briar:briar-android/src/main/AndroidManifest.xml`, 18
  `uses-permission`, none for contacts).
- **Effort:** small. One column family in the store, a picker call, a
  query by lookup URI.

### d. Better: the standard IM row, written by the user's editor (Conversations)

`ContactsContract.CommonDataKinds.Im` is a kind every account type knows.
Its current form is `PROTOCOL = PROTOCOL_CUSTOM` (-1) with the protocol's
name in `CUSTOM_PROTOCOL`; the predefined protocols are deprecated
(`ContactsContract.java:6919,6932-6944`).

- **Conversations** reads XMPP addresses from exactly these rows,
  `PROTOCOL_JABBER` or `PROTOCOL_CUSTOM` with `custom_protocol` `xmpp`
  (`conversations:src/main/java/eu/siacs/conversations/android/JabberIdContact.java:20-43`),
  and writes them only through the system editor,
  `ACTION_INSERT_OR_EDIT` with `Insert.IM_HANDLE` and `Insert.IM_PROTOCOL`
  (`conversations:src/main/java/eu/siacs/conversations/ui/ContactDetailsActivity.java:180-190`).
- **The card:** AOSP renders a custom IM row as "IM" with the protocol as
  sub header and the address as text
  (`QuickContactActivity.java:1407-1425`), and its tap is
  `ACTION_SENDTO imto://<protocol lowercased>/<data>`
  (`aosp-contacts:src/com/android/contacts/ContactsUtils.java:162-180`).
  An intent filter for scheme `imto`, host `lxmf` makes that tap open
  loquax's chat. This gives the "Message on loquax" action of model (b)
  without an account, at least on AOSP; the contacts apps on Lew's phones
  are to be measured.
- **The editor keeps it:** Google's and the fallback account type both
  have the IM kind with the custom protocol column
  (`GoogleAccountType.java:67`, `FallbackAccountType.java:51`,
  `BaseAccountType.java:331-354`), and `Insert.IM_PROTOCOL` may carry a
  custom protocol string (`RawContactModifier.java:551,868-887`). The
  robust way to prefill is `Insert.DATA`, a list of complete
  `ContentValues` rows (`ContactsContract.java:10060-10066`): one IM row
  with `PROTOCOL=-1`, `CUSTOM_PROTOCOL="LXMF"`, `DATA=<32 hex>`, and for a
  new contact a StructuredName.
- **Permissions:** none for loquax to write (the editor writes, "You also
  don't have to request permission to read or write to the provider",
  guide, "Retrieval and modification with intents"). `READ_CONTACTS` to
  read the rows back for reverse lookup and re-linking.
- **Edits, merges, new phone:** the address is part of the contact. It
  survives merges and splits with the raw contact it is in, and moves to a
  new phone with that contact's account. The user can type one by hand in
  any contacts app: protocol "Custom", label LXMF.
- **Privacy, and this is the cost:** the row is in the user's contact, so
  it goes wherever that contact goes: to Google if it is a Google contact.
  loquax uploads nothing, but the user's own sync does. That is why loquax
  never writes it silently: the editor shows the account, and the
  explanation says it in one sentence (§8).

### Comparison

| | a. custom MIME in user's contact | b. own account | c. own table + link | d. IM row via editor |
|---|---|---|---|---|
| Permissions | READ+WRITE_CONTACTS | READ+WRITE_CONTACTS, sync settings (normal) | none; READ_CONTACTS for suggestions and live photo | none to write; READ_CONTACTS to read back |
| Platform only, no AndroidX | yes | yes (authenticator + sync adapter services, 3 XML files) | yes | yes |
| Shown on the card | no (hidden, §1.a) | yes, our icon, our label | no | yes as "IM: LXMF <hex>", tap to `imto://lxmf/` on AOSP |
| Survives user edit/merge | unclear | ours, rebuilt from our table | lookup key resolves | part of the contact |
| New phone | no | no, rebuilt from our table | lookup key mostly resolves; table via our export | yes, with the contact's sync |
| Other apps see | the address | the address, our account | nothing | the address |
| Leaves the phone | maybe | no | no | yes if the contact syncs, user's choice |
| Effort | small | large | small | small |
| Verdict | rejected | later, optional | base | yes, user initiated |

## 2. Several addresses per person

- **c:** the table is keyed by address; a link column on each row. Two
  addresses linked to the same lookup key are one person; the UI groups
  by lookup key. Reverse lookup is the primary key.
- **d:** one IM row per address, any number per contact; the kind is
  multi valued in every account type (`BaseAccountType.java:331-354`).
- **b:** one raw contact per address or one per person with several data
  rows; per person is cleaner on the card. Signal makes one per phone
  number (`SystemContactsRepository.kt:624-699`).
- **Reverse lookup on every incoming message:** never a provider query in
  the message path. loquax resolves address to name and avatar from its own
  in-memory table, as `Names::label` does today
  (`loquax/src/names.rs:571`): a map lookup. The link's copied name and
  thumbnail live in that table. The provider is queried when the
  address book changes: a `ContentObserver` on `Contacts.CONTENT_URI`
  (Threema: `threema:app/src/main/java/ch/threema/app/androidcontactsync/AndroidContactChangeMonitor.kt:103-108`),
  debounced, then one query of the linked contacts by lookup URI
  (`Contacts.lookupContact`, `ContactsContract.java:1716`) and one query of
  all `Im` rows with `CUSTOM_PROTOCOL` `LXMF` (Conversations' selection,
  `JabberIdContact.java:28-43`), both only with `READ_CONTACTS`. A few
  hundred linked contacts are one cursor each, milliseconds; nothing runs
  per message.
- **Conflicts:** an address on two contacts' IM rows is linked to neither
  automatically; loquax asks. Threema takes the same line for its
  automatic matches: "Only 1:1 matches are returned"
  (`threema:app/src/main/java/ch/threema/app/androidcontactsync/synchronization/AndroidContactMatcher.kt:37`).

## 3. The half automatic link flow

### The platform pieces

- **Pick:** `ACTION_PICK` with `Contacts.CONTENT_TYPE`; the result URI
  "grants your app temporary permissions to read that contact ... even if
  your app doesn't include the READ_CONTACTS permission" (Common intents,
  "Select a contact"). AOSP's picker sets
  `FLAG_GRANT_READ_URI_PERMISSION` on the result
  (`aosp-contacts:src/com/android/contacts/activities/ContactSelectionActivity.java:646-647`).
  The guide promises nothing past that moment, so loquax reads the lookup
  key, the name and the thumbnail right then and stores them.
- **Create:** `ACTION_INSERT` on `Contacts.CONTENT_URI` with
  `Insert.NAME` (`ContactsContract.java:9867`) and `Insert.DATA` (the IM
  row). No permission.
- **Add to existing:** `ACTION_INSERT_OR_EDIT` with type
  `Contacts.CONTENT_ITEM_TYPE` and the same extras opens the system's own
  "pick a contact to add this to" (Conversations,
  `ContactDetailsActivity.java:180-190`). No permission. With
  `"finishActivityOnSaveCompleted", true` the editor returns to loquax.
- **Suggest:** needs `READ_CONTACTS`. The provider's own filter
  (`Contacts.CONTENT_FILTER_URI`, `ContactsContract.java:1765`) matches on
  a primary strength collation key, which drops accents
  (`aosp-cp:src/com/android/providers/contacts/NameNormalizer.java:49-51,75-78`):
  `Müller` finds `Muller` but not `Mueller`. Good as a first cut, not
  enough for German names; loquax ranks itself.

### Matching the announced name

The announced name is sanitised already (`names::sanitise`,
`loquax/src/names.rs:105`). Against each contact: `DISPLAY_NAME_PRIMARY`,
StructuredName `GIVEN_NAME`, `FAMILY_NAME`, `MIDDLE_NAME`
(`ContactsContract.java:6029-6047`) and every `Nickname` row (`:6151`).

Folding, applied to both sides: lower case; NFKD and combining marks
removed (`java.text.Normalizer` is platform, the same in Rust with
`unicode-normalization`); `ä ö ü` tried both as `a o u` and as `ae oe ue`,
`ß` as `ss`; punctuation as space; the look-alike fold of 1047's
`skeleton` (`names.rs:183`) last, so `Аlice` in Cyrillic ranks like
`Alice`.

Score, highest wins, ties by most recently contacted contact:

| Match | Score |
|---|---|
| whole announced name equals display name or a nickname | 100 |
| token set equals given + family | 90 |
| announced is one token and equals a nickname | 85 |
| announced is one token and equals the given name (first name only) | 60 |
| all announced tokens appear among the contact's tokens (`Lew DL1ABC` vs `Lew Palm`) | 50 |
| given name prefix of 3+ letters, or initial plus family name | 30 |

Show at most five candidates with 30 or more. Never link without a tap.
The matcher belongs in Rust beside `names.rs`, as a pure function over
rows Java hands it (name fields only, never leaving the process), so the
desktop gets the same ranking and it is unit tested without a phone.

### Screen by screen

1. **Entry points.** In a chat, the peer header's menu: "Add to contacts".
   On an unknown sender's first message, a quiet one line banner: "Not in
   your contacts. Add?" with "Add" and "Not now". "Not now" hides it for
   that address for good (a flag on the row); the menu entry stays.
2. **Choose.** A sheet titled with the announced name and the short hash
   (`Alice ·4f2a`):
   - *With `READ_CONTACTS`:* up to five suggestions, each with photo,
     display name and the field that matched ("nickname Ali"), then
     "Another contact…", "New contact", "Not now".
   - *Without it:* "Another contact…", "New contact", "Not now", and one
     line "Allow contacts to see suggestions" which asks the permission
     there and then. Refused, the line goes away; asking again is in the
     settings.
3. **Picked or suggested contact.** Confirmation: "Link Alice ·4f2a to
   Alice Schmidt?" with the contact's photo. Below, a switch "Also save the
   address in the contact" (off by default; §8 says why). "Link".
   - Link writes loquax's row: lookup key, `_ID`, copied name, copied
     thumbnail.
   - With the switch on, loquax then opens `ACTION_EDIT` on that contact
     with the IM row prefilled; the user saves or backs out, and the link
     stands either way.
4. **New contact.** `ACTION_INSERT` with `Insert.NAME` = the announced
   name and, if the switch was on, the IM row; the editor shows the
   account. On return with a result URI, loquax links it as in 3. Backed
   out, nothing changes.
5. **Linked.** The chat header and the list show the contact's photo and
   name (§4). The header menu gains "Open contact" (`ACTION_VIEW` on the
   lookup URI) and "Unlink".
6. **Several addresses.** Linking a second address to a contact that has
   one already says so: "Alice Schmidt has 1 other address, ·9c31". The
   contact's detail screen in loquax lists all, each with its own chat.
7. **Re-link after a new phone** (only with `READ_CONTACTS`): IM rows with
   protocol LXMF whose address loquax knows and which are not linked yet
   become links on their own: this is not a guess but the user's own
   earlier statement in their own address book. An address on two
   contacts goes to step 2 instead.

## 4. Showing photo and name

- **Photo:** `Contacts.openContactPhotoInputStream(cr, lookupUri, false)`
  gives the thumbnail, `true` the display photo when there is one
  (`ContactsContract.java:2208-2257`); the thumbnail is the size the
  provider keeps for lists, which is what the 1129 avatar circle and the
  notification's large icon draw (its pixel size is device dependent and
  to be measured). Threema
  uses the same call (`threema:app/src/main/java/ch/threema/app/utils/AvatarConverterUtil.java:67`).
  `PHOTO_THUMBNAIL_URI` (`ContactsContract.java:1038`) is the cheaper
  column to check first. loquax keeps its own copy of the thumbnail on
  the row: the notification must not depend on a permission that may be
  revoked, and without the permission the copy from the pick is all there
  is. Refreshed by the observer of §2 when the permission is there.
- **Precedence of the name:** petname, then address book name, then the
  announced name. The petname is the reader's explicit word inside loquax;
  the address book name is the reader's word outside it; the announced
  name is the peer's claim. Linking sets no petname; "Rename" in loquax
  still overrides.
- **The short hash stays.** 1047's rule that the hash is shortened, never
  suppressed (`names.rs:7-12`), holds for the address book name too:
  `Alice Schmidt ·4f2a`. It is what keeps a second, unlinked "Alice
  Schmidt" apart.
- **The collision mark:** a linked peer is `known`, as one with a petname
  (`names.rs` "Trust"). The collision `≈` (`names.rs:71`) is computed
  over the *shown* names, so a linked name colliding with another peer's
  announced name still marks both. Two addresses linked to the same
  contact are not a collision: same person, the reader said so; the
  header names the other address instead.
- **The avatar of 1129:** photo when linked and present, else the
  initials circle from the shown name, with the peer's icon appearance
  colours or the address colour as now
  (`loquax-android/android/src/network/leviculum/loquax/AvatarView.java`
  *(homenode)*). The facade's `ConversationRow`
  (`loquax-android/src/schema.rs:157-180` *(homenode)*) gains a photo
  path or none; the bitmap is loaded on the Java side.

## 5. "Message on loquax" from the contact card

- **Without an account:** model (a)'s row is hidden (§1.a). The IM row of
  §1.d shows as "IM · LXMF" on AOSP and taps to `ACTION_SENDTO
  imto://lxmf/<hex>`. loquax declares on ChatActivity (exported, or a
  small trampoline activity):

  ```xml
  <intent-filter>
    <action a:name='android.intent.action.SENDTO'/>
    <category a:name='android.intent.category.DEFAULT'/>
    <data a:scheme='imto' a:host='lxmf'/>
  </intent-filter>
  ```

  The handler takes the last path segment, checks it is 32 hex digits and
  opens the chat; anything else opens the conversation list. No
  permission. Google Contacts and the Fairphone's contacts app are not
  AOSP; whether they render a custom IM row as tappable is the first
  measurement (open question 1).
- **With the account (b):** our own data kind with our icon and label
  ("Message Alice ·4f2a"), `ACTION_VIEW` with our MIME type into loquax
  (Signal: `AndroidManifest.xml:711-719`). Any contacts app that honours
  `CONTACTS_STRUCTURE` shows it.

## 6. Home node and several devices

The address book is per phone and the link is presentation: which photo and
which name *this* phone shows. Nothing of it goes to the home node.

- The home node's names archive (1132, kind 7,
  `docs/src/concepts/loquax-home-node.md` "Names the home node heard"
  *(homenode)*) carries *announced* names, so a client learns them; the
  link sits above it and changes nothing there.
- Petnames are per device today (the contacts file of 1047 on each
  device). A desktop loquax has no address book; it shows petname or
  announced name. If the reader wants one name everywhere, that is a
  petname entry in the home node's archive, a separate decision (open
  question 3), and the address book name should then *not* be copied into
  it automatically: it would carry address book data off the phone.
- The IM row (§1.d) is the only route by which an address reaches another
  device through the address book, and only by the user's own contact
  sync. The desktop does not read it.

## 7. Import from other LXMF apps (1091, 1103)

Import brings named peers as petnames and blocks as blocks
(`docs/src/concepts/loquax-import.md:46-52`). On the phone they land in
loquax's table like any other peer, unlinked. After an import with
`READ_CONTACTS` granted, the suggestion pass of §3 runs once over the
imported names and offers a review list "12 of your imported people look
like contacts": each row the import's petname, the best candidate, a tick
box, all unticked. Nothing is linked unasked, and no IM row is written by
import.

Sideband's and Columba's exports carry names and contacts as their own
records (1091's survey, `docs/drafts/loquax-import-2026-10.md:305-306`),
not address book links, so nothing of the phone's address book comes over.
Whether either app reads the address book itself was not checked here.

## 8. The permission story in plain words

- **No permission at all:** you can link a person to a contact by picking
  it. loquax copies that contact's name and small photo once. If you change
  the photo in your contacts later, loquax keeps the old one until you
  pick again.
- **"Allow loquax to access your contacts" (`READ_CONTACTS`):** loquax can
  suggest contacts by name, keep names and photos up to date, and find
  LXMF addresses you saved in your contacts. Android shows this as access to
  all your contacts; up to Android 15 there is no "only some" choice. loquax reads them on
  this phone and sends nothing anywhere: it has no server, and its one
  connection is to your own home node, which never gets your contacts.
- **"Also save the address in the contact":** your contacts app saves it.
  If that contact is synced with Google or another account, the address
  goes there with it, as a phone number would. Leave it off to keep the
  address only in loquax.
- **`WRITE_CONTACTS`** only if the loquax account of model (b) is ever
  built; not asked before.

## 9. Slice plan

1. **The link, no permission.** Store: link columns (lookup key, contact
   id, copied name, copied thumbnail path, "not now" flag). Facade: link,
   unlink, the row's photo. Java: `ACTION_PICK`, the confirmation, photo in
   the avatar and the notification. Precedence rule in `names.rs` with
   tests. Phone test: link, rename the contact, rotate, restart.
2. **Create and save.** `ACTION_INSERT` and `ACTION_INSERT_OR_EDIT` with
   the IM row; the `imto://lxmf/` intent filter. Phone measurement on the
   Pixel and the Fairphone: does the card show the row, does a tap reach
   loquax, does it sync and come back on a factory reset phone.
3. **Suggestions.** The Rust matcher with a table of name pairs (umlauts,
   first names, call signs, Cyrillic look-alikes); the permission ask on
   the sheet; the `ContentObserver` refresh; re-link from IM rows.
4. **Import review list** (§7).
5. **Optional: the loquax account (b)**, only if slice 2's measurement
   says the IM row is not usable on the phones people have.

Each slice runs the facade tests on the host; the flows are phone tests
with Lew, asked for ahead of time.

## 10. Open questions for Lew

1. Which contacts app is the target: AOSP's, Google Contacts on the Pixel,
   the Fairphone's? Slice 2's measurement decides whether model (b) is
   ever needed.
2. "Also save the address in the contact": default off as proposed, or
   on? On is friendlier for a new phone, off is the privacy line of the
   order.
3. Should petnames move through the home node so every device shows the
   same name (§6)? Separate from this order; the address book name would
   not travel either way.
4. Precedence: petname above address book name, as proposed, or should
   linking offer to clear an existing petname?
5. The protocol label in the IM row: `LXMF` (what the address is) or
   `Reticulum`? `LXMF` is proposed; other LXMF apps could then read the
   same row.
6. The order names 1136 beside 1132 for the names archive; this study
   found 1132's name entries in the tree and nothing under 1136. If 1136
   carries more (petnames?), §6 changes.
