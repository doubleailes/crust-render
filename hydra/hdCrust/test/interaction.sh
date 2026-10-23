#!/usr/bin/env bash
# The delegate's interaction (tasks 5.1-5.4): each scenario renders a sample
# through hdCrust to convergence, applies one change as a host would, and
# renders again. It checks:
#   - how crust's session took the change (`applied:` after the action):
#     in place for a camera move, a light, a setting, by an import otherwise;
#   - the final image against `crust render` of the layers as they are now
#     (`crust diff`: identical).
#
# Run as compare.sh is:
#   cd hydra && pixi run bash hdCrust/test/interaction.sh <install prefix> <work dir>

set -uo pipefail

PREFIX="${1:?install prefix of hdCrust}"
WORK="${2:?a work directory}"
# `pwd -W` (Git Bash) gives the path a native Windows program can open.
ROOT="$(cd "$(dirname "$0")/../../.." && (pwd -W 2>/dev/null || pwd))"
CRUST="$ROOT/target/release/crust"
TEST="$PREFIX/bin/hdCrustTest"
export PXR_PLUGINPATH_NAME="$PREFIX/plugin/usd/hdCrust/resources"
export MSYS_NO_PATHCONV=1

# name | sample | camera | expected class | extra hdCrustTest arguments
SCENARIOS=(
    "camera_move|subdivision_adaptive.usda|/World/Cam|settings|--move 0.3,0.1,-0.5"
    "light_intensity|cornellbox.usda|/scene/camera1|lights|--set /scene/Sky.inputs:intensity=0.5"
    "mesh_transform|cornellbox.usda|/scene/camera1|full|--set /scene/pCube2.xformOp:translate=0.6,0.5,0.3"
    "prim_removed|cornellbox.usda|/scene/camera1|full|--deactivate /scene/pSphere1"
    "frame_1|animation.usda|/World/Cam|full|--frame 1"
    "time_change|animation.usda|/World/Cam|full|--frame 1 --to-frame 10"
    # usdview's player: one redraw per frame, each must show its frame.
    "playback|animation.usda|/World/Cam|full|--frame 1 --playback 2,10"
    "spp_setting|cornellbox.usda|/scene/camera1|settings|--spp 8 --setting crust:samplesPerPixel=16"
    # An AOV the viewport binds (5.5): the CLI writes the delegate's product,
    # whose one var is compared with the delegate's buffer.
    "peye_aov|cornellbox.usda|/scene/camera1|full|--aov Peye"
    # Hydra's depth is converted to the host's normalized device depth (what
    # usdview composites with), so it is checked for range, not bitwise.
    "depth_aov|cornellbox.usda|/scene/camera1|full|--aov depth"
)

mkdir -p "$WORK"
fail=0
for entry in "${SCENARIOS[@]}"; do
    IFS='|' read -r name sample camera expected extra <<<"$entry"
    dir="$WORK/$name"
    rm -rf "$dir"
    mkdir -p "$dir"
    # shellcheck disable=SC2086 # the extra arguments split on purpose
    if ! "$TEST" "$ROOT/samples/$sample" "$dir/hd.exr" "$dir" --camera "$camera" \
            --width 160 --height 90 --spp 16 $extra >"$dir/hd.log" 2>&1; then
        printf '%-16s %s\n' "$name" "HYDRA RENDER FAILED (see $dir/hd.log)"
        fail=1
        continue
    fi
    if [[ "$extra" == *"--aov depth"* ]]; then
        printf '%-16s %s\n' "$name" "$(grep '^depth:' "$dir/hd.log")"
        continue
    fi
    applied="$(grep '^applied:' "$dir/hd.log" | tail -1 | cut -d' ' -f2)"
    if [ "$applied" != "$expected" ]; then
        printf '%-16s %s\n' "$name" "applied $applied, expected $expected"
        fail=1
        continue
    fi
    # The session's samples per pixel are the layer's own (`--setting` may
    # have changed them), so the CLI renders the layer as it is.
    if ! "$CRUST" render -i "$dir/edits.usda" -o "$dir/cli.exr" -l error >"$dir/cli.log" 2>&1; then
        printf '%-16s %s\n' "$name" "CLI RENDER FAILED (see $dir/cli.log)"
        fail=1
        continue
    fi
    # With an AOV bound, the layer has a product: `-o` names it, and it holds
    # that AOV alone.
    image="$dir/hd.exr"
    if [[ "$extra" =~ --aov\ ([A-Za-z]+) ]]; then
        image="$dir/hd.${BASH_REMATCH[1]}.exr"
    fi
    if "$CRUST" diff "$dir/cli.exr" "$image" >"$dir/diff.txt" 2>&1; then
        printf '%-16s %s\n' "$name" "applied $applied, identical"
    else
        printf '%-16s %s\n' "$name" "applied $applied, DIFFERS: $(head -1 "$dir/diff.txt")"
        fail=1
    fi
done
exit $fail
