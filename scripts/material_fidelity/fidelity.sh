#!/usr/bin/env bash
# Run Ben Houston's Material Fidelity suite through crust, end to end:
# fetch the suite at a pinned revision, build crust, render and score all of its
# materials, write the report and gate the result against the recorded baseline.
#
#   https://github.com/bhouston/material-fidelity
#
# Usage: scripts/material_fidelity/fidelity.sh [command] [args...]
#
#   all        init, build, run, report, check (the default)
#   init       clone the suite + its sample library (https, pinned), create the venv
#   build      cargo build --release
#   run ...    render and score every material; args go to run.py
#              (e.g. --materials noise3d --materials re:^input_ --jobs 2 --threads 2)
#   report     results.json -> report.md (summarize.py)
#   check ...  gate results.json against baseline.json; args go to check.py
#   baseline   accept this run: rewrite baseline.json from results.json (refused
#              for a --materials run unless given --partial, which merges it in)
#   goldeneye  export a Goldeneye project (goldeneye_suite.py), install Goldeneye
#              into the venv and run it with the crust profile; args go to pytest
#              (e.g. -k noise3d); `goldeneye view` in $FIDELITY_ROOT/goldeneye serves the report
#   goldeneye-accept
#              record the latest Goldeneye run's failures as expected failures
#
# Environment:
#   FIDELITY_ROOT   work directory (default: <repo>/.fidelity, gitignored)
#   SUITE_REV       material-fidelity revision (default: the pinned one below)
#   PYTHON          interpreter that already has the dependencies (skips the venv)
#   CARGO           cargo command (default: cargo)
#
# A full run renders 826 materials at 64 spp, about 6 s each on 4 cores: ~80 min.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
ROOT="${FIDELITY_ROOT:-$REPO/.fidelity}"
SUITE="$ROOT/material-fidelity"
OUT="$ROOT/out"
VENV="$ROOT/venv"
SUITE_URL=https://github.com/bhouston/material-fidelity.git
# The suite's .gitmodules uses SSH URLs; clone the one submodule crust needs over https.
SAMPLES_SUBMODULE=third_party/material-samples
SAMPLES_PATH=submodules/mtlx-sample-library
SAMPLES_URL=https://github.com/bhouston/mtlx-sample-library.git
# Pinned so a baseline means something: bump it together with baseline.json.
PINNED_SUITE_REV=fe4de1e779d2969f983bddfc5fb1927b4a5e3553
SUITE_REV="${SUITE_REV:-$PINNED_SUITE_REV}"
PY_DEPS=(numpy "pillow>=11.3" pygltflib OpenEXR)
GOLDENEYE_URL=git+https://github.com/anderslanglands/goldeneye.git@db6067d14c112b89a6fba3b1243cf5ce0c378cae

log() { printf '\033[1m== %s\033[0m\n' "$*" >&2; }

python_bin() {
    if [ -n "${PYTHON:-}" ]; then echo "$PYTHON"
    elif [ -x "$VENV/bin/python" ]; then echo "$VENV/bin/python"
    else echo python3
    fi
}

cmd_init() {
    mkdir -p "$ROOT"
    if [ ! -d "$SUITE/.git" ]; then
        log "cloning material-fidelity"
        git clone --quiet --no-checkout "$SUITE_URL" "$SUITE"
    fi
    if ! git -C "$SUITE" cat-file -e "$SUITE_REV^{commit}" 2>/dev/null; then
        git -C "$SUITE" fetch --quiet origin
    fi
    git -C "$SUITE" -c advice.detachedHead=false checkout --quiet "$SUITE_REV"
    log "material-fidelity at $(git -C "$SUITE" rev-parse --short HEAD)"
    # The gitlink in the suite pins the sample library; `submodule update` honours it.
    git -C "$SUITE" config "submodule.$SAMPLES_SUBMODULE.url" "$SAMPLES_URL"
    log "sample library (~900 MB: materials, textures and every renderer's images)"
    git -C "$SUITE" submodule --quiet update --init "$SAMPLES_PATH"
    log "mtlx-sample-library at $(git -C "$SUITE/$SAMPLES_PATH" rev-parse --short HEAD)"

    if [ -z "${PYTHON:-}" ]; then
        if [ ! -x "$VENV/bin/python" ]; then
            log "python venv at $VENV"
            python3 -m venv "$VENV"
        fi
        "$VENV/bin/python" -m pip install --quiet --upgrade pip
        "$VENV/bin/python" -m pip install --quiet "${PY_DEPS[@]}"
    fi
    "$(python_bin)" - <<'EOF'
from PIL import features
import numpy, pygltflib, OpenEXR  # noqa: F401
if not features.check("avif"):
    raise SystemExit("Pillow has no AVIF codec: the suite's images are AVIF (pip install 'pillow>=11.3')")
EOF
}

cmd_build() {
    log "building crust"
    (cd "$REPO" && ${CARGO:-cargo} build --release -p crust-render)
}

cmd_run() {
    log "rendering the suite -> $OUT"
    "$(python_bin)" "$HERE/run.py" --suite "$SUITE" --out "$OUT" "$@"
}

cmd_report() {
    "$(python_bin)" "$HERE/summarize.py" --suite "$SUITE" "$OUT/results.json" > "$OUT/report.md"
    log "report -> $OUT/report.md"
    sed -n '1,/^$/p;/^| group/,/^$/p' "$OUT/report.md" | head -30
}

cmd_check() {
    "$(python_bin)" "$HERE/check.py" "$OUT/results.json" "$@"
}

cmd_baseline() {
    "$(python_bin)" "$HERE/check.py" "$OUT/results.json" --update \
        --suite-rev "$(git -C "$SUITE" rev-parse --short HEAD)" "$@"
}

cmd_goldeneye() {
    local py proj="$ROOT/goldeneye"
    py="$(python_bin)"
    if ! "$py" -c "import goldeneye" 2>/dev/null; then
        log "installing Goldeneye"
        # --no-deps: its conda package pulls in openusd-typhoon, which crust does not need.
        "$py" -m pip install --quiet "flip-evaluator==1.7" pytest
        "$py" -m pip install --quiet --no-deps "$GOLDENEYE_URL"
    fi
    "$py" "$HERE/goldeneye_suite.py" --suite "$SUITE" --out "$proj"
    log "goldeneye: pytest material-fidelity $*"
    (cd "$proj" && "$py" -m pytest material-fidelity "$@")
}

cmd_goldeneye_accept() {
    local proj="$ROOT/goldeneye" report
    report="$(ls -d "$proj"/_output/run-*/goldeneye-report.json 2>/dev/null | sort | tail -1)"
    [ -n "$report" ] || { echo "no Goldeneye run under $proj/_output" >&2; exit 2; }
    "$(python_bin)" "$HERE/goldeneye_suite.py" --suite "$SUITE" --out "$proj" --expect-failures "$report"
}

cmd_all() {
    cmd_init
    cmd_build
    cmd_run "$@"
    cmd_report
    cmd_check
}

command="${1:-all}"
[ $# -gt 0 ] && shift
case "$command" in
    all|init|build|run|report|check|baseline|goldeneye) "cmd_$command" "$@" ;;
    goldeneye-accept) cmd_goldeneye_accept ;;
    -h|--help|help) sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//' ;;
    *) echo "unknown command: $command (try --help)" >&2; exit 2 ;;
esac
