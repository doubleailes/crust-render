#!/usr/bin/env bash
# The delegate's interaction (tasks 5.1-5.4): each scenario renders a sample
# through hdCrust to convergence, applies its changes as a host would, each
# rendered to convergence in turn. It checks:
#   - how crust's session took the last change (`applied:` after it):
#     in place for a camera move, a light, a setting, by an import otherwise;
#   - what the layers hold, where a bug would leave both renders agreeing
#     (a prim left inactive): the optional checks, `;`-separated,
#       edits:TEXT   edits.usda contains TEXT     edits!:TEXT  it does not
#       scene:TEXT   scene.usdc contains TEXT     log:TEXT     hdCrustTest's
#                                                              output does
#   - the final image against `crust render` of the layers as they are now
#     (`crust diff`: identical).
# A sample `@name` is one of the stages written below, into the work directory.
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

mkdir -p "$WORK/stages"
# A prim the stage authors inactive, so the first write lacks it.
cat >"$WORK/stages/added.usda" <<EOF
#usda 1.0
(
    subLayers = [@$ROOT/samples/cornellbox.usda@]
)
over "scene"
{
    def Sphere "Added" (
        active = false
    )
    {
        double radius = 0.3
    }
}
EOF
# The render settings name a camera other than the one viewed through.
cat >"$WORK/stages/cameras.usda" <<EOF
#usda 1.0
(
    subLayers = [@$ROOT/samples/cornellbox.usda@]
)
def Camera "Dicer"
{
    double3 xformOp:translate = (0, 1, 8)
    uniform token[] xformOpOrder = ["xformOp:translate"]
}
def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </Dicer>
    }
}
EOF
# A dome whose pole is +Z: DomeLight_1's poleAxis.
cat >"$WORK/stages/dome_z.usda" <<EOF
#usda 1.0
(
    upAxis = "Z"
)
def Camera "Cam"
{
    double3 xformOp:translate = (0, 0, 6)
    uniform token[] xformOpOrder = ["xformOp:translate"]
}
def DomeLight_1 "Sky"
{
    uniform token poleAxis = "Z"
    asset inputs:texture:file = @$ROOT/samples/sky_env.exr@
}
def Sphere "Ball"
{
}
EOF
# A stage root starting with `_`, as only the host's own prims used to.
cat >"$WORK/stages/underscore.usda" <<EOF
#usda 1.0
def Xform "_World"
{
    def Camera "Cam"
    {
        double3 xformOp:translate = (0, 0, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def DistantLight "Sun"
    {
    }
    def Sphere "Ball"
    {
    }
}
EOF
# Native instances nested ten deep: past what crust's import expands.
{
    printf '#usda 1.0\nclass Xform "L10"\n{\n    def Sphere "Ball"\n    {\n        double radius = 0.5\n    }\n}\n'
    for i in 9 8 7 6 5 4 3 2 1 0; do
        printf 'class Xform "L%d"\n{\n    def Xform "i" (\n        instanceable = true\n' "$i"
        printf '        references = </L%d>\n    )\n    {\n    }\n}\n' "$((i + 1))"
    done
    cat <<'EOF'
def Xform "World"
{
    def Xform "top" (
        instanceable = true
        references = </L0>
    )
    {
    }
    def Camera "Cam"
    {
        double3 xformOp:translate = (0, 0, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def DistantLight "Sun"
    {
    }
}
EOF
} >"$WORK/stages/nested.usda"

# What the layer at $1 holds, as text (scene.usdc is binary).
layer_text() {
    python -c 'import sys; from pxr import Sdf; print(Sdf.Layer.FindOrOpen(sys.argv[1]).ExportToString())' "$1"
}

# name | sample | camera | expected class | extra hdCrustTest arguments | checks
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
    # A prim removed or hidden, then back: no inactive opinion left.
    "prim_restored|cornellbox.usda|/scene/camera1|full|--deactivate /scene/pSphere1 --activate /scene/pSphere1|edits!:active = false"
    "prim_shown|cornellbox.usda|/scene/camera1|full|--set /scene/pSphere1.visibility=invisible --set /scene/pSphere1.visibility=inherited|edits!:active = false"
    # A prim added after the first write lives in the overs only; hidden,
    # it is made inactive there.
    "added_hidden|@added.usda|/scene/camera1|full|--activate /scene/Added --set /scene/Added.visibility=invisible|edits:active = false"
    # The stage's camera gone: the viewing camera dices, as a whole write
    # would have it, not a prim that is no longer there.
    "camera_removed|@cameras.usda|/scene/camera1|full|--deactivate /Dicer|edits!:dicingCamera = </Dicer>"
    # Written as the schema that reads the pole.
    "dome_z_pole|@dome_z.usda|/Cam|full||scene:DomeLight_1;scene:poleAxis = \"Z\""
    # The stage's own root, though it starts with `_`.
    "underscore_root|@underscore.usda|/_World/Cam|full||scene:def Sphere \"Ball\""
    # Expanded as deep as crust's import does, and reported past it.
    "nested_deep|@nested.usda|/World/Cam|full||log:nested more than 8 deep"
)

mkdir -p "$WORK"
fail=0
for entry in "${SCENARIOS[@]}"; do
    IFS='|' read -r name sample camera expected extra checks <<<"$entry"
    dir="$WORK/$name"
    rm -rf "$dir"
    mkdir -p "$dir"
    stage="$ROOT/samples/$sample"
    if [[ "$sample" == @* ]]; then
        stage="$WORK/stages/${sample#@}"
    fi
    # shellcheck disable=SC2086 # the extra arguments split on purpose
    if ! "$TEST" "$stage" "$dir/hd.exr" "$dir" --camera "$camera" \
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
    failed_check=""
    IFS=';' read -ra items <<<"${checks:-}"
    for check in "${items[@]}"; do
        what="${check%%:*}"
        text="${check#*:}"
        case "$what" in
            edits) grep -qF -- "$text" "$dir/edits.usda" || failed_check="edits.usda lacks '$text'" ;;
            'edits!') ! grep -qF -- "$text" "$dir/edits.usda" || failed_check="edits.usda has '$text'" ;;
            scene) layer_text "$dir/scene.usdc" | grep -qF -- "$text" || failed_check="scene.usdc lacks '$text'" ;;
            log) grep -qF -- "$text" "$dir/hd.log" || failed_check="hdCrustTest did not say '$text'" ;;
        esac
        [ -n "$failed_check" ] && break
    done
    if [ -n "$failed_check" ]; then
        printf '%-16s %s\n' "$name" "applied $applied, $failed_check"
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
