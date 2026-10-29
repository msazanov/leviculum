# loquax for Android

The phone side of the loquax home node (`docs/src/concepts/loquax-home-node.md`):
an Android app with a Java UI on Android's own views and the loquax core
in a Rust library behind JNI. Why this shape and not another is the study
`docs/drafts/loquax-android-2026-10.md`.

Today it is the first build only: one screen that loads the library and
shows the loquax version and whether the app is a client of a home node.

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
make debug                               # android/bin/loquax-debug.apk
make debug ABIS="arm64-v8a armeabi-v7a"  # with the 32-bit library too
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
| `Cargo.toml`, `src/lib.rs` | the JNI library, a workspace member; `cargo test -p loquax-android` runs its tests on the host |
| `Makefile` | the template's, with the `native` step and the Android settings below |
| `android/AndroidManifest.xml` | with the template's build-time tokens |
| `android/src/network/leviculum/loquax/` | `MainActivity`, and `Native`, the Java face of the library |
| `android/res/` | layout and strings |
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
  and cargo home paths remapped, so neither path ends up in the library.
