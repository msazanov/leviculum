#!/usr/bin/env bash
#
# lnomad's browser core does not know it has a terminal (Codeberg #219,
# docs/src/concepts/lnmsg-architecture.md, "the core must not know it has a
# terminal"). The core is `lnomad/src/app/`; the terminal frontend is the
# `tui` cargo feature. Three checks, cheapest first; `--static` runs the
# first two (and the positive control), `--build` the third:
#
#   source  no terminal crate is named in lnomad/src/app/ outside comments,
#           and nothing there is gated on the `tui` feature. The second half
#           is what the build below cannot see: a terminal import behind
#           `cfg(feature = "tui")` compiles away exactly when it is checked.
#   graph   lnomad without default features has no terminal crate in its
#           resolved normal-dependency graph, whichever crate would pull it
#           (tui-input's default features pull both ratatui and crossterm).
#   build   that configuration compiles and lints clean: `cargo clippy -p
#           lnomad --lib --no-default-features -- -D warnings`. Lint-clean
#           matters as much as compiling: an item only the frontend reads is
#           dead code here, which is the sign it belongs in the frontend or in
#           the core's public frontend API, not in a `pub(crate)` corner.
#
# Positive control first, on a temp copy of the workspace manifests and Rust
# sources (~27 MB, 0.2 s): the unmodified copy passes source and graph, a
# `use ratatui::layout::Rect;` injected into the copy's core is refused by the
# source check, and tui-input with its default features is refused by the
# graph check, naming the crate.
#
# Usage: check-lnomad-core-no-tui.sh --static | --build

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAG="[lnomad-core-no-tui]"
FORBIDDEN=(ratatui crossterm termbg terminal_size ratatui-image)

# check_source <workspace-root>
check_source() {
    local core="$1/lnomad/src/app" hits
    # Comments may name the crates (the core documents what it mirrors);
    # code may not. `//` to end of line is dropped before matching.
    hits="$(grep -rn --include='*.rs' '' "$core" \
        | awk -F: '{line=$0; sub(/^[^:]*:[^:]*:/, "", line); sub(/\/\/.*/, "", line);
                    if (line ~ /(^|[^A-Za-z0-9_])(ratatui|crossterm|termbg|terminal_size|ratatui_image|tui_input::backend)::/ ||
                        line ~ /feature *= *"tui"/) print $1 ":" $2 ": " line}')"
    if [ -n "$hits" ]; then
        echo "$TAG FAIL the core names a terminal crate or the tui feature:"
        printf '%s\n' "$hits" | sed "s#^$1/##; s/^/  /"
        echo "$TAG The core takes AppEvent in and gives Effect out; terminal types"
        echo "$TAG belong in lnomad/src/tui.rs."
        return 1
    fi
    echo "$TAG ok   lnomad/src/app/ names no terminal crate and no tui gate"
}

# check_graph <workspace-root> <cargo lock flag>
check_graph() {
    local tree
    if ! tree="$(cd "$1" && cargo tree "$2" -p lnomad --no-default-features \
        -e normal --prefix none 2>&1)"; then
        echo "$TAG ERROR cargo tree failed:"
        printf '%s\n' "$tree" | sed 's/^/  /'
        return 2
    fi
    local hits=() name crate
    while read -r name _; do
        for crate in "${FORBIDDEN[@]}"; do
            if [ "$name" = "$crate" ] || [[ "$name" == "$crate-"* ]]; then
                hits+=("$name")
            fi
        done
    done <<< "$tree"
    if [ "${#hits[@]}" -gt 0 ]; then
        mapfile -t hits < <(printf '%s\n' "${hits[@]}" | sort -u)
        for name in "${hits[@]}"; do
            echo "$TAG FAIL lnomad without the tui feature carries $name"
            echo "$TAG   path: $(cd "$1" && cargo tree "$2" -p lnomad --no-default-features \
                -e normal -i "$name" --prefix none 2>/dev/null | tr '\n' ' ')"
        done
        echo "$TAG A terminal crate is an optional dependency behind the tui feature."
        return 1
    fi
    echo "$TAG ok   none of ${FORBIDDEN[*]} in lnomad's graph without the tui feature"
}

selftest() {
    local tmp out rc=0 rc2
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' RETURN
    (cd "$ROOT" && git ls-files -z Cargo.toml Cargo.lock '*/Cargo.toml' '*.rs' \
        | xargs -0 cp --parents -t "$tmp")
    # The core files may be new and not yet committed; the copy takes the
    # working tree's core, which is what the real check reads too.
    mkdir -p "$tmp/lnomad/src/app"
    cp "$ROOT"/lnomad/src/app/*.rs "$tmp/lnomad/src/app/"
    cp "$ROOT/lnomad/Cargo.toml" "$tmp/lnomad/Cargo.toml"
    if ! out="$(check_source "$tmp" && check_graph "$tmp" --offline)"; then
        # The copy carries the working tree's core and manifest, so this is
        # the tree itself failing; the selftest has nothing clean to inject
        # into, and the verdict below is the tree's.
        printf '%s\n' "$out"
        echo "$TAG FAIL before any injection: the working tree is refused (above)"
        return 1
    fi
    printf 'use ratatui::layout::Rect;\n' >> "$tmp/lnomad/src/app/model.rs"
    rc2=0
    out="$(check_source "$tmp")" || rc2=$?
    if [ "$rc2" -ne 1 ] || ! printf '%s\n' "$out" | grep -qF "use ratatui::layout::Rect"; then
        echo "$TAG SELFTEST FAIL: an injected ratatui import was not refused (exit $rc2)"
        printf '%s\n' "$out"
        rc=1
    fi
    sed -i -E 's/^tui-input = \{ version = "0.15", default-features = false \}$/tui-input = "0.15"/' \
        "$tmp/lnomad/Cargo.toml"
    rc2=0
    out="$(check_graph "$tmp" --offline)" || rc2=$?
    if [ "$rc2" -ne 1 ] || ! printf '%s\n' "$out" | grep -qF "carries ratatui"; then
        echo "$TAG SELFTEST FAIL: tui-input with default features was not refused (exit $rc2)"
        printf '%s\n' "$out"
        rc=1
    fi
    [ "$rc" -eq 0 ] && echo "$TAG selftest ok: an injected ratatui import and tui-input's default features are refused"
    return "$rc"
}

case "${1:-}" in
    --static)
        selftest
        check_source "$ROOT"
        check_graph "$ROOT" --frozen
        ;;
    --build)
        (cd "$ROOT" && cargo clippy --frozen -p lnomad --lib --no-default-features \
            --quiet -- -D warnings)
        echo "$TAG ok   lnomad's core builds and lints clean without the tui feature"
        ;;
    *)
        echo "usage: $0 --static | --build" >&2
        exit 2
        ;;
esac
