#!/usr/bin/env bash
# Interleaved A/B of two crust-render binaries.
#
# Why this exists rather than "run bench_scenes.sh, change the code, run it
# again": on a shared or busy machine that method is simply wrong. Measuring
# one optimization twice an hour apart produced a *12% apparent regression*
# for a change that this script showed to be a 4-5% improvement — the
# difference was background load, not the code. Sequential measurement
# cannot distinguish the two.
#
# So: alternate A, B, A, B, ... within the same few seconds. Whatever else
# the machine is doing lands on both binaries roughly equally, and the
# comparison survives it. Reports both the minimum (the run that got closest
# to the machine's capability) and the mean (which is what actually moves
# when load is symmetric).
#
# Build the two binaries with e.g.:
#     cp target/release/crust-render /tmp/bin_before
#     ...make the change...
#     cargo build --release -p crust-render
#     cp target/release/crust-render /tmp/bin_after
#     scripts/bench_ab.sh -a /tmp/bin_before -b /tmp/bin_after cornellbox veach_mis
#
# Usage: scripts/bench_ab.sh -a <binA> -b <binB> [-n reps] [scene ...]

set -euo pipefail

BIN_A=""
BIN_B=""
REPS=6

while getopts "a:b:n:h" opt; do
    case "$opt" in
        a) BIN_A="$OPTARG" ;;
        b) BIN_B="$OPTARG" ;;
        n) REPS="$OPTARG" ;;
        h) sed -n '2,26p' "$0"; exit 0 ;;
        *) exit 2 ;;
    esac
done
shift $((OPTIND - 1))

if [ ! -x "$BIN_A" ] || [ ! -x "$BIN_B" ]; then
    echo "error: -a and -b must both name executables" >&2
    exit 2
fi

SCENES=("$@")
if [ ${#SCENES[@]} -eq 0 ]; then
    SCENES=(cornellbox openpbr_showcase veach_mis instancing nested_instancing)
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

printf '%-22s %18s %18s %10s %10s\n' scene "A min/mean" "B min/mean" "d(min)" "d(mean)"
printf '%s\n' "------------------------------------------------------------------------------------"

for scene in "${SCENES[@]}"; do
    path="$scene"
    if [ ! -e "$path" ]; then
        path="samples/$scene"
        [ -e "$path" ] || path="samples/$scene.usda"
    fi
    [ -e "$path" ] || { printf '%-22s MISSING\n' "$scene"; continue; }

    a_times=()
    b_times=()
    for _ in $(seq "$REPS"); do
        # A then B, back to back, so a load spike hits both.
        for side in a b; do
            bin="$BIN_A"; [ "$side" = b ] && bin="$BIN_B"
            # `|| true`: under `set -e` with `pipefail` a single transient
            # render failure in a 50-run sweep would otherwise abort the whole
            # comparison. Drop the sample and carry on instead.
            t="$("$bin" -i "$path" -o "$WORK/o.exr" --stats -l error 2>/dev/null \
                | awk '/^  Render /{gsub(/s$/,"",$2); print $2; exit}' || true)"
            [ -n "$t" ] || continue
            if [ "$side" = a ]; then a_times+=("$t"); else b_times+=("$t"); fi
        done
    done

    if [ ${#a_times[@]} -eq 0 ] || [ ${#b_times[@]} -eq 0 ]; then
        printf '%-22s FAILED\n' "$(basename "$scene" .usda)"
        continue
    fi

    stats() { printf '%s\n' "$@" | awk '
        NR==1{min=$1} {s+=$1; if($1<min)min=$1; n++}
        END{printf "%.3f %.3f", min, s/n}'; }
    read -r a_min a_mean <<<"$(stats "${a_times[@]}")"
    read -r b_min b_mean <<<"$(stats "${b_times[@]}")"

    d_min="$(awk "BEGIN{printf \"%+.1f%%\", ($b_min-$a_min)/$a_min*100}")"
    d_mean="$(awk "BEGIN{printf \"%+.1f%%\", ($b_mean-$a_mean)/$a_mean*100}")"

    printf '%-22s %8s %8s %8s %8s %10s %10s\n' \
        "$(basename "$scene" .usda)" "$a_min" "$a_mean" "$b_min" "$b_mean" "$d_min" "$d_mean"
done

echo
echo "$REPS interleaved reps per scene; negative deltas mean B is faster."
