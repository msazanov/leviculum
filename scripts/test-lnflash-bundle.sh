#!/usr/bin/env bash
# Fixture test for scripts/lnflash-bundle.sh: which build a bundle says it is.
#
# PUB-0022: every nightly bundle was named after the bare Cargo version, so
# two nightlies of one development window were the same "lnflash-0.10.0-dev"
# on disk, the same UF2 names and the same manifest version, and the person
# holding one could not say which firmware they had flashed. The bundle now
# carries the leviculum .deb's own stamp, <version>~nightly.<UTCdate>.<sha7>,
# in three places: the tarball name (and the directory inside it), every UF2
# name together with the manifest that points at them, and the README. This
# runs the real script from a known commit and asserts all three.
#
# The script builds two firmware images and a musl binary; none of that is
# what is under test, so a PATH of stubs stands in for the toolchain: cargo
# answers `metadata` and writes placeholder artefacts for `build`, rustc
# names a compiler and writes a copying bin2uf2, llvm-objcopy copies, and the
# lnflash it "builds" accepts --check-bundle. ~1 s, no network, no build.
#
# LNFLASH_BUNDLE_SH overrides the script under test, which is how the
# pre-fix version is checked to be red:
#   LNFLASH_BUNDLE_SH=<old copy> bash scripts/test-lnflash-bundle.sh
#
# Usage: bash scripts/test-lnflash-bundle.sh

set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE_SH="${LNFLASH_BUNDLE_SH:-$REPO/scripts/lnflash-bundle.sh}"
[ -f "$BUNDLE_SH" ] || { echo "no script under test at $BUNDLE_SH"; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

failures=0
fail() { echo "  FAIL: $*"; failures=$((failures + 1)); }

# The board records of the script under test, so a board added there is a
# board this test stages a payload for and asserts an image of.
mapfile -t RECORDS < <(sed -n 's/^    "\([a-z0-9]*|[^"]*\)"$/\1/p' "$BUNDLE_SH")
[ "${#RECORDS[@]}" -gt 0 ] || { echo "no BOARDS records found in $BUNDLE_SH"; exit 1; }

# --- The toolchain stubs ----------------------------------------------------
STUBS="$WORK/stubs"
mkdir -p "$STUBS"
cat >"$STUBS/cargo" <<'EOF'
#!/usr/bin/env bash
# metadata: where the fixture's target directory is. build: a placeholder
# where cargo would have written the artefact.
case "$1" in
metadata) printf '{"target_directory":"%s"}\n' "$FIXTURE_TARGET" ;;
build)
    if [ "${3:-}" = lnflash ]; then
        out="$FIXTURE_TARGET/x86_64-unknown-linux-musl/release/lnflash"
        mkdir -p "$(dirname "$out")"
        printf '#!/usr/bin/env bash\nexit 0\n' >"$out"
        chmod +x "$out"
    fi
    ;;
*) echo "cargo stub: unexpected $*" >&2; exit 1 ;;
esac
EOF
cat >"$STUBS/rustc" <<'EOF'
#!/usr/bin/env bash
case "$1" in
--version) echo "rustc 9.9.9 (fixturec0de 2026-01-01)" ;;
--print) echo "$FIXTURE_TARGET/no-sysroot" ;;
-O)
    # rustc -O -o <out> <src>: a bin2uf2 that copies its input.
    printf '#!/usr/bin/env bash\ncp "${@: -2:1}" "${@: -1}"\n' >"$3"
    chmod +x "$3"
    ;;
*) echo "rustc stub: unexpected $*" >&2; exit 1 ;;
esac
EOF
cat >"$STUBS/llvm-objcopy" <<'EOF'
#!/usr/bin/env bash
cp "${@: -2:1}" "${@: -1}"
EOF
chmod +x "$STUBS"/*

# --- The fixture tree -------------------------------------------------------
VERSION="0.10.0-dev"

setup() { # <case>
    TREE="$WORK/$1/tree"
    FIXTURE_TARGET="$WORK/$1/cargo-target"
    mkdir -p "$TREE/scripts" "$TREE/leviculum-nrf/tools" "$TREE/lnflash/payload" \
        "$FIXTURE_TARGET/thumbv7em-none-eabihf/release"
    cp "$BUNDLE_SH" "$TREE/scripts/lnflash-bundle.sh"
    cp "$REPO/scripts/cargo-target-dir.sh" "$TREE/scripts/"
    printf '[workspace.package]\nversion = "%s"\n' "$VERSION" >"$TREE/Cargo.toml"
    printf '/target/\n/.deb-version-*\n' >"$TREE/.gitignore"
    echo "fixture bin2uf2 source" >"$TREE/leviculum-nrf/tools/bin2uf2.rs"
    echo "fixture README" >"$TREE/lnflash/payload/README-bundle.md"
    echo "fixture LICENSE" >"$TREE/LICENSE"
    printf 'PART 1 — host binaries\nPART 2 — LNode firmware image\n' \
        >"$TREE/THIRD-PARTY-NOTICES"
    local record name bin stem
    for record in "${RECORDS[@]}"; do
        IFS='|' read -r name bin _ stem <<<"$record"
        echo "fixture $name firmware" >"$FIXTURE_TARGET/thumbv7em-none-eabihf/release/$bin"
        [ -n "$stem" ] || continue
        mkdir -p "$TREE/lnflash/payload/$name"
        echo "fixture softdevice" >"$TREE/lnflash/payload/$name/${stem}_softdevice.hex"
        echo "fixture licence" >"$TREE/lnflash/payload/$name/${stem}_license-agreement.txt"
    done
    (cd "$TREE" &&
        git init -q . &&
        git add -A &&
        git -c user.name=fixture -c user.email=fixture@example.invalid \
            commit -q -m fixture) >/dev/null 2>&1 ||
        { echo "could not build the fixture git repo"; exit 1; }
    SHA7="$(git -C "$TREE" rev-parse HEAD | cut -c1-7)"
}

run_bundle() {
    (cd "$TREE" && env -u CI_COMMIT_SHA -u CARGO_TARGET_DIR \
        PATH="$STUBS:$PATH" FIXTURE_TARGET="$FIXTURE_TARGET" SKIP_FIRMWARE=1 \
        bash scripts/lnflash-bundle.sh) >"$TREE/../out" 2>&1
    echo $? >"$TREE/../rc"
}
rc() { cat "$TREE/../rc"; }
dumpout() { echo "  --- script output ---"; sed 's/^/    /' "$TREE/../out"; }

# The three places, for a bundle that must be called <stamp>.
assert_stamped() { # <stamp>
    local stamp="$1" tarball listing unpack name image
    tarball="$TREE/target/lnflash/lnflash-$stamp.tar.gz"
    [ "$(rc)" = 0 ] || { fail "the bundle script exited $(rc)"; return; }
    # 1. The tarball, and the one directory inside it that `cd lnflash-*`
    #    (publish-nightly.sh, README, flashing.md) enters.
    [ -f "$tarball" ] || {
        fail "no tarball named lnflash-$stamp.tar.gz; target/lnflash holds: $(find "$TREE/target/lnflash" -mindepth 1 -maxdepth 1 -printf '%f ')"
        return
    }
    listing="$(tar -tzf "$tarball")"
    [ "$(printf '%s\n' "$listing" | cut -d/ -f1 | sort -u)" = "lnflash-$stamp" ] ||
        fail "the tarball's top directory is not lnflash-$stamp: $(printf '%s\n' "$listing" | cut -d/ -f1 | sort -u | tr '\n' ' ')"
    unpack="$TREE/../unpack"
    rm -rf "$unpack" && mkdir -p "$unpack" && tar -xzf "$tarball" -C "$unpack"
    local dir="$unpack/lnflash-$stamp"
    # 2. Every UF2 is named after the stamp, the manifest's version is the
    #    stamp, and every image the manifest names is in the archive.
    grep -qxF "version = \"$stamp\"" "$dir/firmware/manifest.toml" 2>/dev/null ||
        fail "manifest [bundle] version is not \"$stamp\": $(grep '^version' "$dir/firmware/manifest.toml" 2>/dev/null)"
    for record in "${RECORDS[@]}"; do
        IFS='|' read -r name _ <<<"$record"
        image="$name/leviculum-$name-$stamp.uf2"
        [ -f "$dir/firmware/$image" ] || fail "no image firmware/$image"
        grep -qF "file    = \"$image\"" "$dir/firmware/manifest.toml" 2>/dev/null ||
            fail "the manifest does not point at $image"
    done
    # 3. The README a person opens names the build.
    grep -qxF "    $stamp" "$dir/README.md" 2>/dev/null ||
        fail "README.md does not state the stamp $stamp"
}

# --- Case: the stamp deb-stamp.sh wrote for this commit is used verbatim ----
#
# A date no clock produces today, so the assertion can tell "read the file"
# from "derived it again" apart.
echo "[case] stamp-file"
before=$failures
setup stamp-file
STAMP="0.10.0~dev~nightly.19991231.$SHA7"
echo "$STAMP" >"$TREE/.deb-version-leviculum-cli"
run_bundle
assert_stamped "$STAMP"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: no stamp file (a developer's `just lnflash-bundle`) --------------
echo "[case] no-stamp-file"
before=$failures
setup no-stamp-file
day_before="$(date -u +%Y%m%d)"
run_bundle
day_after="$(date -u +%Y%m%d)"
stamp="0.10.0~dev~nightly.$day_before.$SHA7"
[ -f "$TREE/target/lnflash/lnflash-$stamp.tar.gz" ] ||
    stamp="0.10.0~dev~nightly.$day_after.$SHA7"
assert_stamped "$stamp"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: a stamp file left by another commit is not believed -------------
echo "[case] stale-stamp-file"
before=$failures
setup stale-stamp-file
echo "0.10.0~dev~nightly.19991231.0000000" >"$TREE/.deb-version-leviculum-cli"
day_before="$(date -u +%Y%m%d)"
run_bundle
day_after="$(date -u +%Y%m%d)"
stamp="0.10.0~dev~nightly.$day_before.$SHA7"
[ -f "$TREE/target/lnflash/lnflash-$stamp.tar.gz" ] ||
    stamp="0.10.0~dev~nightly.$day_after.$SHA7"
assert_stamped "$stamp"
grep -q "ignoring .deb-version-leviculum-cli" "$TREE/../out" ||
    fail "a stale stamp file was dropped without saying so"
[ "$failures" -eq "$before" ] || dumpout

echo
if [ "$failures" -eq 0 ]; then
    echo "test-lnflash-bundle: all cases passed"
else
    echo "test-lnflash-bundle: FAILED ($failures assertion(s))"
    exit 1
fi
