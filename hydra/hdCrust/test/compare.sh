#!/usr/bin/env bash
# The delegate's image against `crust render` of the layers it wrote (spec
# "Agreeing with the CLI"): each sample rendered headless through hdCrust by
# hdCrustTest at 16 spp, then the session's `edits.usda` rendered by the CLI
# at 16 spp, and `crust diff` must report the two EXRs identical.
#
# That cannot see what the writing loses, which is lost on both sides alike,
# so a second part renders the *original* stage with the CLI and requires the
# delegate's image to match it within noise (relmse below 0.01; the layers'
# structure differs, so the samples land differently). A material binding
# the writer dropped renders the default grey there and fails it.
#
# Run from the repository root, in the pixi environment that built hdCrust:
#   cd hydra && pixi run bash hdCrust/test/compare.sh <install prefix> <work dir>
# with the release `crust` built (target/release/crust). Exits 1 on any
# difference or failure.

set -uo pipefail

PREFIX="${1:?install prefix of hdCrust (cmake --install ... --prefix)}"
WORK="${2:?a work directory}"
# `pwd -W` (Git Bash) gives the path a native Windows program can open.
ROOT="$(cd "$(dirname "$0")/../../.." && (pwd -W 2>/dev/null || pwd))"
CRUST="$ROOT/target/release/crust"
TEST="$PREFIX/bin/hdCrustTest"
export PXR_PLUGINPATH_NAME="$PREFIX/plugin/usd/hdCrust/resources"
# Git Bash on Windows: leave USD prim paths (`/World/Cam`) alone.
export MSYS_NO_PATHCONV=1

# sample, camera: stages with no RenderProducts (their beauty is the one EXR
# both sides write), covering meshes, materials, lights, curves, implicit
# shapes, instancing and crust's own prim attributes.
SAMPLES=(
    "cornellbox.usda /scene/camera1"
    "materialx_basic.usda /World/ShotCam"
    "domelight.usda /World/Cam"
    "curves.usda /World/Cam"
    "implicit_shapes.usda /World/Cam"
    "instancing.usda /World/Cam"
    "fog.usda /World/Cam"
    "purpose_bindings.usda /World/Cam"
)

mkdir -p "$WORK"
fail=0
for entry in "${SAMPLES[@]}"; do
    read -r sample camera <<<"$entry"
    name="${sample%.*}"
    dir="$WORK/$name"
    rm -rf "$dir"
    mkdir -p "$dir"
    if ! "$TEST" "$ROOT/samples/$sample" "$dir/hd.exr" "$dir" --camera "$camera" \
            --width 160 --height 90 --spp 16 >"$dir/hd.log" 2>&1; then
        printf '%-22s %s\n' "$name" "HYDRA RENDER FAILED (see $dir/hd.log)"
        fail=1
        continue
    fi
    if ! "$CRUST" render -i "$dir/edits.usda" -o "$dir/cli.exr" -s 16 -l error >"$dir/cli.log" 2>&1; then
        printf '%-22s %s\n' "$name" "CLI RENDER FAILED (see $dir/cli.log)"
        fail=1
        continue
    fi
    if "$CRUST" diff "$dir/cli.exr" "$dir/hd.exr" >"$dir/diff.txt" 2>&1; then
        printf '%-22s %s\n' "$name" "identical"
    else
        printf '%-22s %s\n' "$name" "DIFFERS: $(head -1 "$dir/diff.txt")"
        fail=1
    fi
done

# Against the original stage: sample, camera, and the stage's own resolution
# (the CLI renders the original at it). The camera's aperture must have the
# image's aspect, or Hydra and crust conform the window differently.
FIDELITY=(
    "purpose_bindings.usda /World/Cam 640 360"
)
for entry in "${FIDELITY[@]}"; do
    read -r sample camera width height <<<"$entry"
    name="${sample%.*}_original"
    dir="$WORK/$name"
    rm -rf "$dir"
    mkdir -p "$dir"
    if ! "$TEST" "$ROOT/samples/$sample" "$dir/hd.exr" "$dir" --camera "$camera" \
            --width "$width" --height "$height" --spp 16 >"$dir/hd.log" 2>&1; then
        printf '%-22s %s\n' "$name" "HYDRA RENDER FAILED (see $dir/hd.log)"
        fail=1
        continue
    fi
    # With the sampler the delegate writes by default (its viewport default).
    if ! "$CRUST" render -i "$ROOT/samples/$sample" --camera "$camera" --sampler pmjBn \
            -o "$dir/orig.exr" -s 16 \
            -l error >"$dir/cli.log" 2>&1; then
        printf '%-22s %s\n' "$name" "CLI RENDER FAILED (see $dir/cli.log)"
        fail=1
        continue
    fi
    "$CRUST" diff "$dir/orig.exr" "$dir/hd.exr" --json "$dir/diff.json" >"$dir/diff.txt" 2>&1
    relmse="$(sed -n 's/.*"relmse": *\([^,]*\),.*/\1/p' "$dir/diff.json" | head -1)"
    if awk -v r="$relmse" 'BEGIN { exit !(r != "" && r < 0.01) }'; then
        printf '%-22s %s\n' "$name" "within noise (relmse $relmse)"
    else
        printf '%-22s %s\n' "$name" "DIFFERS from the original stage (relmse ${relmse:-?}, see $dir)"
        fail=1
    fi
done
exit $fail
