# loquax on Android without Gradle: a study and a first build

Order 1111, 2026-10-29, Refs #232. Lew's decision of the same day: the
phone side of the home node (`docs/src/concepts/loquax-home-node.md`) is our
own Android app, built with make after his `android-make-template`: make,
Debian packages and the SDK's command line tools, no Gradle, no AndroidX,
no AAR, no Google services.

The first build exists: `loquax-android/` in the repo, a Java Activity that
loads a Rust library over JNI and shows the loquax version and the home
node client state. Built twice from clean, byte-identical. Numbers below
are measured on schneckenschreck unless marked otherwise.

## 0. Verdict

**Option A with the boundary on the shell, not on the screen model.**
Java draws with Android's own views and runs the Android plumbing
(Activity, the foreground service, the notification, the battery
exemption intent). One Rust library holds everything else: an in-process
Reticulum node with one TCP interface to the home node, the client engine,
the home client (`/join`, `/sync`, `/send`, `/mark`), the store and the
pull schedule. The JNI surface is a handful of calls that move commands in
and events out as bytes, the same seam the CLI and the terminal use
(`loquax::outbox::Outbox`). No lnsd on the phone, no shared instance.

Iced on Android (option B) does not start today without patching iced's
winit layer, and would still need Java for the service and the
notification. Termux (C1) is the free interim for testing the client role.

## 1. Architecture options

### A. Java UI with Android views, Rust library over JNI

What the first build does, grown up.

- **Where the boundary sits.** Three layers are candidates:
  - *The screen model* (`loquax::ui`: `UiModel`, `update`, `view`,
    TEA). It is a pane and key model, three panes with focus and a keymap
    (`loquax/src/ui.rs`, module doc). An Android screen is lists, touch and
    the system back button; Java would end up rendering a terminal's panes.
    Not this layer.
  - *The engine.* It runs under the core lock and may not wait
    (`loquax/src/home/client.rs`, module doc), so Java must never call into
    it directly. Not this layer.
  - *The shell*: the `Outbox` seam (`Command` in, `OutboxEvent` out,
    `loquax/src/outbox.rs`), the home client's `pull_into`,
    `send_retrying` and the mark, the store, and the sync schedule
    (`loquax/src/sync.rs`). This is the layer the CLI and the terminal both
    stand on. **The JNI boundary goes here**, as a small facade in
    `loquax-android`: `start(home, config)`, `command(bytes)`,
    `nextEvent() -> bytes` (blocking, on a Java thread), `stop()`, and
    queries for what a list shows (conversations, the messages of one).
    Payloads are msgpack, which the crate already speaks
    (`rmp-serde` is in its graph).
- **How it connects.** An in-process `ReticulumNode` with
  `enable_transport(false)` and one `add_tcp_client`
  (`leviculum-std/src/driver/builder.rs:285`); a host name goes through
  `host_resolver` (`:703`); the quiet link's keepalive is the builder's
  `link_keepalive` (`:722`). Today the engine's shell only attaches to a
  shared instance (`loquax/src/engine/shell.rs:316-318`), so the one change
  in loquax is an attach that brings its own interfaces. The phone is a
  leaf, never a transport node, and runs no lnsd.
- **What it reuses.** All of loquax's std shell and core, leviculum-std's
  node and TCP interface, the store (`history`, bundled SQLite) unchanged.
  Nothing of the terminal or the iced GUI.
- **Fit with the template.** Exact: Java sources and resources as the
  template has them, one extra make step for the library.
- **Size, measured.**

  | Build | arm64-v8a | armeabi-v7a |
  |---|---|---|
  | First build's library (`describe` only; the linker drops the rest) | 1 638 064 B | 601 952 B |
  | First build's APK, debug, arm64 only | 528 941 B | |
  | APK with both ABIs | 795 263 B | |
  | `loquax` CLI for aarch64-linux-android (std: node, engine, home client), a stand-in for the full library | 12 936 744 B, 4 320 950 B gzip -9 | |
  | the same with `history` (SQLite with FTS5, bundled C, built by the NDK's clang) | 15 334 448 B | |

  A full client library should land near the CLI's size, so an APK of
  about 5 MB per ABI, compressed.
- **Effort.** Small to medium, in this order: the attach with its own
  TCP interface (loquax shell), the facade and its event encoding
  (`loquax-android`), then the Java screens (conversations, one
  conversation, compose, the join screen with the `lqxh1` code), then the
  service (section 3). Each is a pass of the usual size; the Java screens
  are the only part without a Rust test, and their logic stays thin.

### B. A Rust UI on Android (iced through android-activity)

loquax already has an iced window (`gui` feature,
`docs/src/concepts/gui-toolkit.md`). Probed here:

- `cargo check -p loquax --features gui --target aarch64-linux-android`
  fails: android-activity 0.6.1 needs `native-activity` or
  `game-activity` chosen, which winit's `android-native-activity` feature
  would do.
- With it, the program would still not start. iced_winit 0.14.1 builds
  its event loop without an `AndroidApp` (`src/lib.rs` line 79 of that
  crate), and winit 0.30.13 requires one on Android ("An `AndroidApp` as
  passed to android_main() is required", `src/platform_impl/android/mod.rs`
  line 167). iced_winit also implements no `suspended` handler; Android
  destroys the window's surface whenever the app goes to the background.
- Beyond starting: the soft keyboard and IME, the back button, the
  notification and the foreground service. The last two are Java (or JNI
  calls into Java) in any case, so B does not remove Java, it adds a
  forked iced on top.
- **Fit with the template.** NativeActivity works without Java code and
  make could package it, but any service brings Java back.
- **Size.** Not measured: it does not link. Expect A's library plus iced
  and tiny-skia.
- **Effort.** Large, and it is maintenance of an iced fork.

### C. Other shapes found

1. **Termux.** loquax's terminal frontend already knows Termux
   (`TERMUX_VERSION`, `docs/src/man/loquax.1.md`, ENVIRONMENT). Static
   aarch64 builds of lnsd and loquax in Termux should make the phone a
   client of the home node today, as on a laptop, with no background
   service and no notification. No code; the cheap way to try the client
   role on the phone before the app has screens. Not tried on a device in
   this pass.
2. **lnsd as a second process.** Ship lnsd as `liblnsd.so` in the APK's
   native library directory (the one place an app may execute from) and
   attach loquax to it as on a desktop. Two processes, a shared instance
   socket on Android, and the service still Java. More moving parts than
   A for no gain: the phone is not a transport node.
3. **A WebView UI** over the Rust library. Gives up Android's views for
   HTML, keeps all the Java plumbing. No gain over A.

## 2. Toolchain on this host

| Piece | From | Installed | Disk |
|---|---|---|---|
| JDK 21 (`javac`, `keytool`) | Debian `default-jdk-headless` | apt | Debian's usual |
| build-tools 35.0.1 (`aapt`, `d8`, `zipalign`, `apksigner`) | Google, `build-tools_r35.0.1_linux.zip` | `/usr/lib/android-sdk/build-tools/35.0.1` | 147 MB |
| platform android-35 (`android.jar`) | Google, `platform-35_r02.zip` | `/usr/lib/android-sdk/platforms/android-35` | 142 MB |
| NDK r27d (27.3.13750724) | Google, `android-ndk-r27d-linux.zip`, 664 MB | `/usr/lib/android-sdk/ndk/27.3.13750724` | 2.0 GB |
| Rust std for `aarch64-linux-android`, `armv7-linux-androideabi` | rustup | toolchain 1.97.1 | small |

- **Debian packages.** The template's apt line names
  `google-android-build-tools-35.0.1-installer` and
  `google-android-platform-35-installer`. trixie's contrib has no
  `google-android-*` package at all (contrib enabled, `apt-cache search`
  empty, 2026-10-29), so `loquax-android/install-toolchain.sh` fetches the
  same zips those installers fetch, checks each against the SHA-1 in
  Google's repository index, and unpacks them into the installers' layout.
  A fresh run into an empty directory gave a tree identical to the one
  installed here (`diff -r`); a wrong SHA-1 stops it before anything of
  that piece is installed.
- **The NDK question.** Rust's compiler and std come from rustup; what the
  NDK gives is the linker driver (`aarch64-linux-android26-clang`) and
  bionic's sysroot it links against, plus the C compiler for crates with
  C inside (SQLite in `history`). cargo-ndk is not needed: two
  environment variables in the Makefile do its job.
  - *zig* ships no bionic, so it needs the NDK's sysroot anyway.
  - *Debian* has no Android sysroot package.
  - *A sysroot-only extract* is possible: the parts used are the sysroot
    (144 MB) and the clang wrappers; most of the 2.0 GB is clang's runtime
    libraries for every target (363 MB) and the toolchain itself. Not worth
    the custom packaging while disk is plenty.
- **16 KiB pages.** Android 15 supports devices with 16 KiB pages; a
  library aligned for 4 KiB runs there only in a compatibility mode, and
  one whose RELRO segment is misaligned crashes
  (https://developer.android.com/guide/practices/page-sizes). NDK r28
  aligns by default; with r27 the Makefile passes
  `-z max-page-size=16384` and `-z common-page-size=16384`. Measured: with
  the first flag alone RELRO ended at 0xde000, with both at 0xe0000.
- **Package name.** `network.leviculum.loquax` as ordered. A reverse-DNS
  name should be a domain we control; see the open questions.

## 3. Background and battery, without Google

- **Doze** suspends network access, ignores wake locks and defers alarms
  to maintenance windows
  (https://developer.android.com/training/monitoring-device-state/doze-standby).
  An app that is exempt from battery optimisation "can use the network and
  hold partial wake locks during Doze and App Standby" (same page). A
  foreground service alone keeps the app out of App Standby, but the page
  does not say it lifts Doze's network cut; the home-node draft (section
  1, problem 5) reads it the same way.
- **The exemption.** The app asks with
  `ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`. Google's table calls it
  acceptable for a messenger that cannot use FCM; we use no FCM by
  decision, and the Play policy is not ours to meet anyway.
- **The foreground service** with a persistent notification holds the
  quiet link. Since Android 14 every foreground service declares a type.
  `remoteMessaging` ("transfer text messages from one device to another")
  fits and has no time limit; `dataSync` is capped at 6 hours in 24 from
  Android 15 and may not start from `BOOT_COMPLETED`
  (https://developer.android.com/about/versions/15/behavior-changes-15,
  https://developer.android.com/develop/background-work/services/fgs/service-types).
  `minSdk` 26 brings the notification channels it needs.
- **How the wake ladder maps** (home-node draft, section 3.3):
  - *Pull*: as on the desktop, on open, on a key, on loquax's timer
    (`loquax/src/sync.rs`). Without the service, Android may stop the app
    any time after it leaves the screen; the timer runs only while it is
    open. Needs nothing beyond A.
  - *Quiet link*: the service keeps the node and its one TCP connection
    to the home node up, with the exemption. TCP keepalive at the NAT-safe
    interval and the link keepalive (`link_keepalive`) are slice 3's knobs;
    the home node's wake push arrives over the link and the app pulls.
    Untouched by Google either way. The battery cost is the open
    measurement of the draft.
  - *Home WLAN*: AutoInterface finds the home node on the LAN; on Android
    that needs a multicast lock (as Columba found), and is a later step.
  - *UnifiedPush with a self-hosted ntfy*: later. The home node posts an
    empty message to the user's ntfy, the distributor app on the phone
    wakes loquax, loquax pulls. It replaces the always-on service for
    users who run a distributor anyway. The connector library is published
    as an AAR, which the template does not take; the Android side of the
    protocol is a set of broadcast intents
    (https://unifiedpush.org/developers/spec/android/, not checked in
    detail here), so a Java implementation without the AAR is the likely
    path.

## 4. Signing and distribution

- **Debug.** As the template: `make debug` makes `android/debug.keystore`
  on first use, password `android`. The keystore is in `.gitignore`, so
  every checkout has its own debug key, and a debug APK from another
  checkout does not install over this one's.
- **Release.** `android/loquax.keystore`, made by hand, `KSPASS` from the
  environment. Not made in this pass.
- **Reproducibility.** The template exports `SOURCE_DATE_EPOCH` from the
  last commit; aapt wrote 1980-01-01 into every zip entry, so the date
  plays no part. Two `make debug` runs from `make clean`, the second with a
  fresh cargo target directory and sccache off, gave the same APK and the
  same library (sha256 in the report of 1111). Untested: a second
  checkout path (the Makefile remaps it out of the library) and a second
  host.
- **F-Droid.** F-Droid builds from source and signs with its own key
  unless the app's build is reproducible, in which case it can ship the
  developer's signature. The make flow is unusual for its build server,
  which expects Gradle; its recipes allow custom build commands, but the
  NDK and the Rust toolchain have to be named in the recipe. A question
  for when there is something to publish.

## 5. What the first build is

- `loquax-android/Cargo.toml`, `src/lib.rs`: a workspace member, not a
  default one (cdylib, like leviculum-ffi). `describe(home)` and the JNI
  export `Java_network_leviculum_loquax_Native_describe`. Three host tests.
- `loquax-android/Makefile`: the template's, plus `native`.
- `loquax-android/android/`: manifest, `MainActivity`, `Native`, layout.
- The screen shows `loquax 0.1.0 (<commit>)` and either "Not a client of
  a home node." or the home node's LXMF address and the cursor. The app's
  files directory is its loquax home. Nothing on the phone can make it a
  client yet; that is the join screen.
- Not installed on any device.

## 6. Open questions for Lew

1. **Package name.** `network.leviculum.loquax` assumes the domain
   `leviculum.network`. If we do not hold it, `de.lew_palm.loquax` (your
   domain, the template's example) is the safe choice; the name cannot
   change after the first release without losing updates.
2. **ABIs.** arm64-v8a only, or armeabi-v7a too (0.37 of the arm64 library's
   size here) for older phones?
3. **minSdk 26** (Android 8, 2017): fine, or must older phones run it?
4. **First screens after the facade**: join by code (typed or pasted;
   QR needs a camera and is slice 5), conversations, one conversation,
   compose. Or the service first, to measure the quiet link's battery
   cost before there are screens?
5. **Termux as the interim**: should a static aarch64 loquax be in the
   nightly's packages so you can try the client role on the phone now?
