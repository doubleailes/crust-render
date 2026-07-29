#!/usr/bin/env bash
# Re-render every sample scene and diff it against a recorded set of goldens.
# This is the gate for any change that claims to be a pure optimization.
#
# Rendered at `-s 16` on purpose. `min_samples_per_pixel` defaults to 32 and
# the adaptive early-stop needs `taken >= min_spp`, so at 16 spp adaptive
# sampling can never fire and every pixel takes exactly 16 samples. Above
# that threshold a single-ulp difference changes a pixel's sample budget,
# which then cascades — so a bit-identical change would look structural and
# a structural change could hide. Do not raise it without re-reading
# `render_pixel`'s early-stop condition.
#
# Usage:
#   scripts/check_images.sh record <dir>    # render the goldens
#   scripts/check_images.sh check  <dir>    # re-render and diff against them

set -euo pipefail

MODE="${1:-}"
DIR="${2:-}"
SPP=16

if [ "$MODE" != record ] && [ "$MODE" != check ]; then
    sed -n '2,16p' "$0"
    exit 2
fi
if [ -z "$DIR" ]; then
    echo "error: no golden directory given" >&2
    exit 2
fi

BIN=target/release/crust-render
if [ ! -x "$BIN" ]; then
    cargo build --release -p crust-render
fi
cargo build --release -q -p crust-render --example exr_diff

scene_paths() {
    for f in samples/*.usda; do
        printf '%s\t%s\n' "$(basename "$f" .usda)" "$f"
    done
    # The two Kitchen_set variants are the instancing A/B: identical geometry
    # authored with and without native instancing, so Stage-4-style changes to
    # how prototypes are grouped must keep both of them honest.
    printf 'kitchen\tsamples/Kitchen_set/Kitchen_set.usd\n'
    printf 'kitchen_instanced\tsamples/Kitchen_set/Kitchen_set_instanced.usd\n'
}

if [ "$MODE" = record ]; then
    mkdir -p "$DIR"
    while IFS=$'\t' read -r name path; do
        "$BIN" -i "$path" -o "$DIR/$name.exr" -s "$SPP" -l error >/dev/null 2>&1 \
            && echo "recorded $name" || echo "FAILED   $name"
    done < <(scene_paths)
    exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
fail=0

while IFS=$'\t' read -r name path; do
    golden="$DIR/$name.exr"
    if [ ! -f "$golden" ]; then
        printf '%-22s %s\n' "$name" "NO GOLDEN"
        continue
    fi
    if ! "$BIN" -i "$path" -o "$WORK/$name.exr" -s "$SPP" -l error >/dev/null 2>&1; then
        printf '%-22s %s\n' "$name" "RENDER FAILED"
        fail=1
        continue
    fi
    diff="$(target/release/examples/exr_diff "$golden" "$WORK/$name.exr" 2>&1)"
    # "640x360  differing pixels: 0/230400 (0.0000%)" -> the 0 before the slash
    n="$(printf '%s\n' "$diff" \
        | sed -n 's/.*differing pixels: \([0-9]*\)\/.*/\1/p')"
    if [ "$n" = 0 ]; then
        printf '%-22s identical\n' "$name"
    else
        printf '%-22s %s\n' "$name" "$(printf '%s\n' "$diff" | tr '\n' ' ')"
        fail=1
    fi
done < <(scene_paths)

echo
if [ "$fail" = 0 ]; then
    echo "All scenes bit-identical."
else
    echo "Some scenes differ — see above. Expected only for a change that says so."
fi
exit "$fail"
