#!/usr/bin/env bash
# Build the lnflash Debian package from the bundle scripts/lnflash-bundle.sh
# just assembled.
#
# Why from the bundle and not from scratch: the bundle is what the tarball
# ships, firmware images, the vendored SoftDevice with its licence, and a
# manifest whose checksums cover all of them. The package installs that same
# firmware/ directory at /usr/share/lnflash, the last place lnflash looks
# (lnflash/src/manifest.rs, SYSTEM_BUNDLE), so `sudo lnflash` after an apt
# install finds exactly what `sudo ./lnflash` in the unpacked tarball finds.
# Building the images a second time here would be a second firmware build to
# keep in step with the first.
#
# Usage:
#   scripts/deb-stamp.sh && scripts/lnflash-bundle.sh && scripts/build-lnflash-deb.sh
#
# amd64 only, like the bundle: the images are architecture-independent, but
# the bundle builds the host binary for the runner's own triple.
#
# Requires cargo-deb, python3 (the manual page) and dpkg-deb. Produces
# target/debian/lnflash_<version>_amd64.deb under the cargo target directory.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# shellcheck source-path=SCRIPTDIR/..
# shellcheck source=scripts/cargo-target-dir.sh
source "$ROOT/scripts/cargo-target-dir.sh"
TARGET="$(cargo_target_dir "$ROOT")"
TRIPLE=x86_64-unknown-linux-musl
ARCH=amd64

say() { printf '[build-lnflash-deb] %s\n' "$*"; }

for stamp in .deb-version-lnflash .deb-version-leviculum-cli target/deb-changelog/lnflash; do
    [ -r "$stamp" ] || {
        echo "error: ${stamp} missing — run scripts/deb-stamp.sh first" >&2
        exit 1
    }
done
VERSION="$(cat .deb-version-lnflash)"

# The bundle's stage directory is named after the leviculum .deb's stamp, the
# same file scripts/lnflash-bundle.sh reads (PUB-0022). A bundle built with
# NRF_FEATURES carries a `+<feature>` suffix and is a rig arm, not a release,
# so it is never found here.
BUNDLE_STAMP="$(cat .deb-version-leviculum-cli)"
STAGE="target/lnflash/lnflash-${BUNDLE_STAMP}"
[ -r "$STAGE/firmware/manifest.toml" ] || {
    echo "error: no bundle at ${STAGE} — run scripts/lnflash-bundle.sh first" >&2
    exit 1
}

# The fixed path lnflash/Cargo.toml's asset glob reads. Replaced, not merged:
# a board dropped from the bundle must drop out of the package too.
PAYLOAD="target/lnflash/deb-payload"
rm -rf "$PAYLOAD"
cp -a "$STAGE/firmware" "$PAYLOAD"

python3 scripts/md2man.py --selftest
python3 scripts/md2man.py --outdir target/man docs/src/man/lnflash.1.md

# The binary cargo-deb packages is the one the bundle built and checked its
# own payload with; --no-build keeps it that way.
cargo deb -p lnflash --target "$TRIPLE" --no-build --no-strip --deb-version "$VERSION"
DEB="$TARGET/debian/lnflash_${VERSION}_${ARCH}.deb"
bash scripts/deb-finalize.sh "$DEB"

# Check the package as it installs, not the stage it came from: unpack it and
# let the packaged binary verify the packaged bundle at the packaged path. A
# file the asset glob missed, or one it flattened out of its board directory,
# fails the manifest's checksums here rather than at somebody's board.
unpacked="$(mktemp -d)"
trap 'rm -rf "$unpacked"' EXIT
dpkg-deb -x "$DEB" "$unpacked"
"$unpacked/usr/bin/lnflash" --bundle "$unpacked/usr/share/lnflash" --check-bundle

say "produced: ${DEB}"
