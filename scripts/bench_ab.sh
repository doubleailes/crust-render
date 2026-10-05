#!/usr/bin/env bash
# Interleaved A/B of two crust binaries.
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
#     cp target/release/crust /tmp/bin_before
#     ...make the change...
#     cargo build --release -p crust-render
#     cp target/release/crust /tmp/bin_after
#     scripts/bench_ab.sh -a /tmp/bin_before -b /tmp/bin_after cornellbox veach_mis
#
# Usage: scripts/bench_ab.sh -a <binA> -b <binB> [-n reps] [-p phase] [-x args] [scene ...]
#
#   -p phase  which `--stats` phase to time, by its report name (default
#             "Render"; e.g. "Parse USD stage" or "Traverse prims" to measure
#             an import). Durations above a minute (`03:29.3`) are converted.
#   -x args   extra renderer arguments for every run, e.g. a scene that needs a
#             frame and a camera:
#             -x "-f 1004 --camera /root/camera01/.../renderCam -s 1"

set -euo pipefail

BIN_A=""
BIN_B=""
REPS=6
PHASE="Render"
EXTRA=""

while getopts "a:b:n:p:x:h" opt; do
    case "$opt" in
        a) BIN_A="$OPTARG" ;;
        b) BIN_B="$OPTARG" ;;
        n) REPS="$OPTARG" ;;
        p) PHASE="$OPTARG" ;;
        x) EXTRA="$OPTARG" ;;
        h) sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 0 ;;
        *) exit 2 ;;
    esac
done
shift $((OPTIND - 1))

if [ ! -x "$BIN_A" ] || [ ! -x "$BIN_B" ]; then
    echo "error: -a and -b must both name executables" >&2
    exit 2
fi

# A binary from before the `render` subcommand takes the render flags bare;
# asking clap for the subcommand's help tells the two apart, so a new build
# can still be compared against an old one.
render_cmd() {
    if "$1" help render >/dev/null 2>&1; then echo render; fi
}
CMD_A="$(render_cmd "$BIN_A")"
CMD_B="$(render_cmd "$BIN_B")"

SCENES=("$@")
if [ ${#SCENES[@]} -eq 0 ]; then
    SCENES=(cornellbox openpbr_showcase veach_mis instancing nested_instancing)
fi

# The first report line naming $PHASE (the execution tree, which comes before
# the by-time table), as seconds: `41.453s` or `03:29.3`.
phase_seconds() {
    awk -v ph="$PHASE" '{
        l = $0; sub(/^ +/, "", l)
        if (index(l, ph "  ") == 1) {
            split(substr(l, length(ph) + 1), f, " ")
            t = f[1]; sub(/s$/, "", t)
            if (t ~ /:/) { split(t, m, ":"); t = m[1] * 60 + m[2] }
            print t; exit
        }
    }'
}

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
            bin="$BIN_A"; cmd="$CMD_A"
            [ "$side" = b ] && { bin="$BIN_B"; cmd="$CMD_B"; }
            # `|| true`: under `set -e` with `pipefail` a single transient
            # render failure in a 50-run sweep would otherwise abort the whole
            # comparison. Drop the sample and carry on instead.
            # shellcheck disable=SC2086  # EXTRA and cmd are deliberately word-split
            t="$("$bin" $cmd -i "$path" -o "$WORK/o.exr" --stats -l error $EXTRA 2>/dev/null \
                | phase_seconds || true)"
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
echo "$REPS interleaved reps per scene, timing \"$PHASE\"; negative deltas mean B is faster."
