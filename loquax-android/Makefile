# loquax for Android, built with make after android-make-template: no
# Gradle, no AndroidX, the SDK's command line tools, and the Rust library
# of this directory's crate cross-built by cargo with the NDK's linker.
# README.md says what to install; docs/drafts/loquax-android-2026-10.md why.

# --- Project ---
APP_MODULE=loquax
NAMESPACE=network.leviculum.loquax
VERSION_CODE?=1
VERSION_NAME?=0.1

# --- SDK / tools ---
SDK?=/usr/lib/android-sdk
PLATFORM?=android-35
BUILDTOOLS?=$(SDK)/build-tools/35.0.1
# 26 (Android 8): notification channels, which the foreground service of a
# later slice needs, and above the 21 that Rust's Android std requires.
MIN_API?=26
TARGET_SDK?=35
JV=-source 8 -target 8

ANDJAR=$(SDK)/platforms/$(PLATFORM)/android.jar
D8=$(BUILDTOOLS)/d8
AAPT=$(BUILDTOOLS)/aapt
APKSIGNER=$(BUILDTOOLS)/apksigner
ZIPALIGN=$(BUILDTOOLS)/zipalign
# A current adb from Google's platform-tools; on hamster Debian's freezes
# the host when a device connects (android-make-template's CLAUDE.md).
ADB?=adb

# --- Rust ---
# The NDK is only the linker and the Android libc it links against; the
# compiler is rustup's. r27 is the NDK's long-term-support line.
NDK?=$(SDK)/ndk/27.3.13750724
NDKBIN=$(NDK)/toolchains/llvm/prebuilt/linux-x86_64/bin
# The ABIs in the APK: arm64-v8a, and armeabi-v7a, the 32-bit one older
# phones need (Lew, 2026-10-29).
ABIS?=arm64-v8a armeabi-v7a
REPO=$(realpath $(PROJ)/../..)
CARGO_TARGET_DIR?=$(REPO)/target
CARGO_HOME?=$(HOME)/.cargo
LIBNAME=libloquax_android.so
TRIPLE_arm64-v8a=aarch64-linux-android
TRIPLE_armeabi-v7a=armv7-linux-androideabi
CLANG_arm64-v8a=aarch64-linux-android
CLANG_armeabi-v7a=armv7a-linux-androideabi
ENV_arm64-v8a=AARCH64_LINUX_ANDROID
ENV_armeabi-v7a=ARMV7_LINUX_ANDROIDEABI
# The cc crate's names for the C compiler, for SQLite inside the store.
CC_ENV_arm64-v8a=aarch64_linux_android
CC_ENV_armeabi-v7a=armv7_linux_androideabi
# 16 KiB pages: Android 15 devices may run with them; a library aligned to
# 4 KiB needs a compatibility mode there, and one whose RELRO segment ends
# off a 16 KiB boundary crashes. NDK r28 aligns by default, r27 needs both
# flags (developer.android.com/guide/practices/page-sizes). The path
# prefixes keep the checkout and the cargo home out of the library. The
# symbol table goes (the dynamic one, which JNI looks the native methods up
# in, stays): it was a third of the library.
RUSTFLAGS_ANDROID=-C link-arg=-Wl,-z,max-page-size=16384 \
	-C link-arg=-Wl,-z,common-page-size=16384 \
	-C strip=symbols \
	--remap-path-prefix=$(REPO)=leviculum \
	--remap-path-prefix=$(CARGO_HOME)=cargo

# --- Paths ---
PROJ=$(shell dirname $(realpath $(lastword $(MAKEFILE_LIST))))/android
BIN=$(PROJ)/bin
SRCPATH=$(PROJ)/src/$(subst .,/,$(NAMESPACE))
GENMANIFEST=$(BIN)/AndroidManifest.xml
NATIVE_LIBS=$(foreach abi,$(ABIS),$(BIN)/lib/$(abi)/$(LIBNAME))

# --- Optional deps: drop *.jar into lib/, .kt files anywhere in src/ ---
LIB_DIR=$(PROJ)/lib
JARS=$(wildcard $(LIB_DIR)/*.jar)
KT_SOURCES=$(shell find $(PROJ)/src -name '*.kt' 2>/dev/null)

empty:=
space:=$(empty) $(empty)
CLASSPATH=$(subst $(space),:,$(strip $(PROJ)/src $(JARS)))

# --- Reproducible builds (F-Droid convention) ---
SOURCE_DATE_EPOCH?=$(shell git log -1 --format=%ct 2>/dev/null || echo 0)
export SOURCE_DATE_EPOCH

# --- Build-type defaults (debug target overrides these) ---
KSPASS_ARG?=env:KSPASS
KEYSTORE?=$(PROJ)/$(APP_MODULE).keystore
APK_SUFFIX?=
AAPT_EXTRA_FLAGS?=
PACKAGE_NAME?=$(NAMESPACE)

PACKAGE=$(BIN)/$(APP_MODULE)$(APK_SUFFIX).apk
UNALIGNED_PACKAGE=$(BIN)/$(APP_MODULE)$(APK_SUFFIX).unaligned.apk

.PHONY: all release debug build install install-debug uninstall uninstall-debug start start-debug logcat clean compile compile_android manifest native pack check-kspass FORCE

all: release

release: check-kspass build

check-kspass:
	@if [ "$(KSPASS_ARG)" = "env:KSPASS" ] && [ -z "$$KSPASS" ]; then \
		printf '\nKSPASS is not set: a release build needs the keystore password.\n\n  KSPASS=... make release        # signed release build\n  make debug                     # debug build, no password\n\n' >&2; \
		exit 1; \
	fi

debug: KSPASS_ARG=pass:android
debug: KEYSTORE=$(PROJ)/debug.keystore
debug: APK_SUFFIX=-debug
debug: AAPT_EXTRA_FLAGS=--debug-mode
debug: PACKAGE_NAME=$(NAMESPACE).debug
debug: $(PROJ)/debug.keystore build

build: compile native pack

$(PROJ)/debug.keystore:
	keytool -genkeypair -keystore $@ -storepass android -keypass android \
		-alias androiddebugkey -dname "CN=Android Debug,O=Android,C=US" \
		-keyalg RSA -keysize 2048 -validity 10000

$(BIN):
	mkdir -p $(BIN)

manifest: | $(BIN)
	sed -e 's|{{VERSION_CODE}}|$(VERSION_CODE)|g' \
	    -e 's|{{VERSION_NAME}}|$(VERSION_NAME)|g' \
	    -e 's|{{PACKAGE_NAME}}|$(PACKAGE_NAME)|g' \
	    -e 's|{{MIN_SDK}}|$(MIN_API)|g' \
	    -e 's|{{TARGET_SDK}}|$(TARGET_SDK)|g' \
	    $(PROJ)/AndroidManifest.xml > $(GENMANIFEST)
	$(AAPT) package -f -m --custom-package $(NAMESPACE) -J $(PROJ)/src -M $(GENMANIFEST) -S $(PROJ)/res -I $(ANDJAR)

compile: compile_android

compile_android: manifest
ifneq ($(strip $(KT_SOURCES)),)
	kotlinc -jvm-target 1.8 -cp $(ANDJAR):$(CLASSPATH) -d $(PROJ)/obj $(KT_SOURCES)
endif
	javac $(JV) -d $(PROJ)/obj -classpath $(CLASSPATH):$(PROJ)/obj -bootclasspath $(ANDJAR) $$(find $(SRCPATH) -name '*.java')

native: $(NATIVE_LIBS)

# cargo knows when the library is current, so make always asks it.
$(BIN)/lib/%/$(LIBNAME): FORCE
	CARGO_TARGET_$(ENV_$*)_LINKER=$(NDKBIN)/$(CLANG_$*)$(MIN_API)-clang \
	CC_$(CC_ENV_$*)=$(NDKBIN)/$(CLANG_$*)$(MIN_API)-clang \
	AR_$(CC_ENV_$*)=$(NDKBIN)/llvm-ar \
	CARGO_TARGET_$(ENV_$*)_RUSTFLAGS="$(RUSTFLAGS_ANDROID)" \
	CARGO_TARGET_DIR=$(CARGO_TARGET_DIR) \
	cargo build --manifest-path $(REPO)/Cargo.toml -p loquax-android \
		--release --locked --target $(TRIPLE_$*)
	mkdir -p $(@D)
	cp $(CARGO_TARGET_DIR)/$(TRIPLE_$*)/release/$(LIBNAME) $@

FORCE:

pack:
	cd $(PROJ)/obj && $(D8) --output $(BIN) --lib $(ANDJAR) --min-api $(MIN_API) $$(find . -name '*.class' | LC_ALL=C sort) $(JARS)
	$(AAPT) package -f -m $(AAPT_EXTRA_FLAGS) -F $(UNALIGNED_PACKAGE) -M $(GENMANIFEST) -S $(PROJ)/res -I $(ANDJAR)
	cd $(BIN) && $(AAPT) add $(UNALIGNED_PACKAGE) classes.dex
	cd $(BIN) && $(AAPT) add $(UNALIGNED_PACKAGE) $(foreach abi,$(ABIS),lib/$(abi)/$(LIBNAME))
	$(ZIPALIGN) -f 4 $(UNALIGNED_PACKAGE) $(PACKAGE)
	$(APKSIGNER) sign --ks $(KEYSTORE) --ks-pass $(KSPASS_ARG) $(PACKAGE)

uninstall:
	$(ADB) uninstall $(NAMESPACE)

uninstall-debug:
	$(ADB) uninstall $(NAMESPACE).debug

install: release
	$(ADB) install $(PACKAGE)

install-debug: KSPASS_ARG=pass:android
install-debug: KEYSTORE=$(PROJ)/debug.keystore
install-debug: APK_SUFFIX=-debug
install-debug: AAPT_EXTRA_FLAGS=--debug-mode
install-debug: PACKAGE_NAME=$(NAMESPACE).debug
install-debug: debug
	$(ADB) install $(PACKAGE)

start:
	$(ADB) shell am start -n $(NAMESPACE)/$(NAMESPACE).MainActivity

start-debug:
	$(ADB) shell am start -n $(NAMESPACE).debug/$(NAMESPACE).MainActivity

logcat:
	$(ADB) shell logcat | grep Runtime

clean:
	rm -rf $(BIN) $(PROJ)/obj
	rm -f $(SRCPATH)/R.java
