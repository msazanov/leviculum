# loquax for Android

The phone side of the loquax home node (`docs/src/concepts/loquax-home-node.md`):
an Android app with a Java UI on Android's own views and the loquax core
in a Rust library behind JNI. Why this shape and not another is the study
`docs/drafts/loquax-android-2026-10.md`.

What it does today:

- **Join.** The first start asks for the home code (`lqxh1...`, printed by
  `loquax home admit` on the home node) and the home node's address,
  `host:port` of its lnsd TCP server, and joins. Both are kept in the app's
  files directory; later starts need neither. What went wrong is said in
  plain words, and the field it concerns is focused.
- **Conversations.** Names as every loquax screen gives them, the last
  message, the unread count, most recent first. An address typed below the
  list opens a new conversation.
- **Chat.** Bubbles, ours right and theirs left; each of ours says
  `queued`, `sending`, `sent`, `delivered` or `failed` from the home node's
  archive. Reactions show as chips under their message; a long press reacts
  with one of loquax's quick six. Send goes through the home node's
  `/send`. Opening the chat reads it (a read mark for the home's other
  devices) and pulls; the refresh button pulls; the sync schedule pulls
  while the app runs. Leaving the chat closes it, so what arrives then
  stays unread.

- **In the background.** After the start a foreground service
  (`LoquaxService`, type `remoteMessaging`) holds the session: the link to
  the home node and the sync schedule's quiet pulls go on with no screen in
  front. Its notification says the home node and the unread count and has
  a stop button, which ends the session and closes the app. A conversation
  with new messages gets a notification of its own, sender and last line,
  which opens the chat; none while a screen of the app is visible. When
  the system ends the process, the service comes back and starts the
  session from the saved code and address.
- **Asked once.** On the first conversation list: notifications (Android
  13 and later), and on a later look the battery optimisation exemption,
  with a plain reason. Neither is forced, neither is asked again.

Not yet: the battery measurement (study, section 3), UnifiedPush.

## How it is built

One Rust library holds the node, the engine, the store and loquax's screen
model; Java only draws. The library's node is a leaf: transport off, one
TCP client interface to the home node, no lnsd and no shared instance on
the phone (`loquax::engine::attach_leaf`). Its session
(`src/session.rs`) runs the model's effects through the client runners the
terminal's shell runs too (`loquax::home::runner`). Java calls five functions with msgpack
bytes in the versioned schema of `src/schema.rs`:

| Call | What |
|---|---|
| `start` | start, joining with the code on the first run |
| `command` | `open`, `close`, `send`, `react`, `pull` |
| `query` | `conversations`, `messages` of one peer |
| `nextEvent` | blocks on a Java thread: `changed`, `notice`, `sync`, `link`, `stopped` |
| `stop` | end the session |

## Build

The build follows android-make-template: make, the SDK's command line
tools and a JDK, no Gradle, no AndroidX, no Google services. On top of the
template, make cross-builds the crate in this directory with cargo and puts
`libloquax_android.so` into the APK.

Once per host:

```bash
sudo apt install default-jdk-headless unzip make
rustup target add aarch64-linux-android armv7-linux-androideabi
loquax-android/install-toolchain.sh      # build-tools 35.0.1, android-35, NDK r27d
```

Then, in this directory:

```bash
make debug                               # android/bin/loquax-debug.apk, both ABIs
make debug ABIS=arm64-v8a                # the 64-bit library only
make install-debug start-debug           # onto a connected phone
KSPASS=... make release                  # needs android/loquax.keystore
```

`make debug` makes `android/debug.keystore` on first use. No keystore goes
into git (`.gitignore`); the release key is made by hand:

```bash
keytool -genkeypair -keystore android/loquax.keystore \
    -alias release -keyalg RSA -keysize 2048 -validity 10000
```

## Layout

| Path | What |
|---|---|
| `Cargo.toml`, `src/lib.rs` | the JNI library, a workspace member; `cargo test -p loquax-android` runs its tests on the host, `tests/facade.rs` against an in-process home node |
| `src/schema.rs`, `src/session.rs` | the payloads, and the session behind them |
| `Makefile` | the template's, with the `native` step and the Android settings below |
| `android/AndroidManifest.xml` | with the template's build-time tokens |
| `android/src/network/leviculum/loquax/` | the three screens (`MainActivity` joins, `ConversationsActivity`, `ChatActivity`), `LoquaxService`, which holds the session, and `ServiceLink`, the screens' binding to it, `Asks`, the two questions asked once, `Native`, the Java face of the library, `Core` and `MsgPack`, which read it, and `Events` |
| `android/res/` | layouts, the bubbles' shapes, strings |
| `install-toolchain.sh` | the SDK and NDK pieces, checked by SHA-1 |

## Where it differs from the template

- `MIN_API` is 26, not 19: Rust's Android std needs 21, and 26 brings the
  notification channels a foreground service needs.
- `ADB` is `adb` from the PATH. On hamster use a current adb from
  Google's platform-tools; Debian's freezes that host when a phone
  connects.
- `pack` sorts the class files before `d8` and adds the native libraries
  after `classes.dex`.
- The library is linked with 16 KiB page alignment and with the checkout
  and cargo home paths remapped, so neither path ends up in the library,
  and without its symbol table (the dynamic one JNI needs stays).
- SQLite inside the store is C, compiled by the NDK's clang, which the
  Makefile names for the cc crate.
- The Java has no lambdas: javac compiles them against
  `LambdaMetafactory`, which `android.jar` does not have.
