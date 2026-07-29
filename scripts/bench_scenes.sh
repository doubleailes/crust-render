#!/usr/bin/env bash
# Min-of-N render timings across the checked-in sample scenes.
#
# Why min-of-N and not a mean: the numbers this reports are used to accept or
# reject optimizations worth 1-5% each, and a mean drifts with whatever else
# the machine is doing. The minimum is the run that got closest to the
# machine's actual capability, which is the quantity being changed. Same
# convention as the `ray_throughput` probe.
#
# Two things this script deliberately does NOT do:
#
#   - It does not compare images. Use `exr_diff` for that, and always render
#     the comparison at `-s 16`: `min_samples_per_pixel` defaults to 32 and
#     the adaptive early-stop needs `taken >= min_spp`, so at 16 spp adaptive
#     sampling can never fire and the sample count is fixed. Above it, a
#     1-ulp difference changes a pixel's sample budget and a bit-identical
#     change looks structural.
#
#   - It does not attribute time within the render. For that, count
#     instructions rather than seconds — several of the steps this measures
#     are smaller than the run-to-run spread:
#
#       RAYON_NUM_THREADS=1 valgrind --tool=callgrind --cache-sim=no \
#           --branch-sim=no target/release/crust-render \
#           -i samples/cornellbox.usda -o /tmp/cg.exr -s 2
#       callgrind_annotate --inclusive=no callgrind.out.<pid>
#
# Usage: scripts/bench_scenes.sh [-n runs] [-o outdir] [scene ...]
#   scene: a path, or a bare name resolved against samples/ (default: all)

set -euo pipefail

RUNS=3
OUTDIR=""

while getopts "n:o:h" opt; do
    case "$opt" in
        n) RUNS="$OPTARG" ;;
        o) OUTDIR="$OPTARG" ;;
        h) sed -n '2,30p' "$0"; exit 0 ;;
        *) exit 2 ;;
    esac
done
shift $((OPTIND - 1))

# `curves` is deliberately absent: it renders in ~10 ms, which is all
# process startup and BVH build, so its "render time" measures nothing.
DEFAULT_SCENES=(
    cornellbox
    openpbr_showcase
    veach_mis
    instancing
    nested_instancing
    motionblur
    smoke
    fog
    Kitchen_set/Kitchen_set.usd
    Kitchen_set/Kitchen_set_instanced.usd
)

SCENES=("$@")
if [ ${#SCENES[@]} -eq 0 ]; then
    SCENES=("${DEFAULT_SCENES[@]}")
fi

BIN=target/release/crust-render
if [ ! -x "$BIN" ]; then
    echo "building $BIN"
    cargo build --release -p crust-render
fi

WORK="${OUTDIR:-$(mktemp -d)}"
mkdir -p "$WORK"

printf '%-34s %10s %12s %10s\n' scene "render(s)" "throughput" "spread"
printf '%s\n' "--------------------------------------------------------------------"

for scene in "${SCENES[@]}"; do
    # Bare names resolve against samples/ with a .usda suffix; anything
    # containing a slash or a suffix is taken as given.
    path="$scene"
    if [ ! -e "$path" ]; then
        path="samples/$scene"
        [ -e "$path" ] || path="samples/$scene.usda"
    fi
    if [ ! -e "$path" ]; then
        printf '%-34s %10s\n' "$scene" "MISSING"
        continue
    fi

    label="$(basename "$scene" .usda)"
    best=""
    worst=""
    tput=""

    for _ in $(seq "$RUNS"); do
        out="$("$BIN" -i "$path" -o "$WORK/$label.exr" --stats -l error 2>/dev/null)"
        # "  Render      1.412s   96.6% ..." -> 1.412
        secs="$(printf '%s\n' "$out" | awk '/^  Render /{gsub(/s$/,"",$2); print $2; exit}')"
        [ -n "$secs" ] || continue
        if [ -z "$best" ] || awk "BEGIN{exit !($secs < $best)}"; then
            best="$secs"
            # Report the throughput of the run being reported, not an average
            # of runs, so the two columns describe the same render.
            tput="$(printf '%s\n' "$out" \
                | awk '/^  throughput /{print $2" "$3; exit}')"
        fi
        if [ -z "$worst" ] || awk "BEGIN{exit !($secs > $worst)}"; then
            worst="$secs"
        fi
    done

    if [ -z "$best" ]; then
        printf '%-34s %10s\n' "$label" "FAILED"
        continue
    fi

    spread="$(awk "BEGIN{printf \"%.1f%%\", ($worst-$best)/$best*100}")"
    printf '%-34s %10s %12s %10s\n' "$label" "$best" "$tput" "$spread"
done

echo
echo "min of $RUNS runs; images in $WORK"
