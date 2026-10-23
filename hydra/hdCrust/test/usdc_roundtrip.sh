#!/usr/bin/env bash
# Pixar-written USDC read by crust (openusd) as the USDA it came from (task
# 4.6, design Risks): hdCrust's scene.usdc is written by Pixar's SdfLayer, so
# every sample is exported with `Sdf.Layer.Export` to USDC, and both files are
# rendered by `crust render` at 16 spp; `crust diff` must find them identical.
#
# The export goes beside its sample (`samples/_rt_<name>.usdc`, removed after)
# so relative asset paths and sublayers resolve the same. Run in the pixi
# environment, whose Python has `pxr`:
#   cd hydra && pixi run bash hdCrust/test/usdc_roundtrip.sh <work dir>

set -uo pipefail

WORK="${1:?a work directory}"
ROOT="$(cd "$(dirname "$0")/../../.." && (pwd -W 2>/dev/null || pwd))"
CRUST="$ROOT/target/release/crust"
export MSYS_NO_PATHCONV=1

mkdir -p "$WORK"
fail=0
for usda in "$ROOT"/samples/*.usda; do
    name="$(basename "$usda" .usda)"
    usdc="$ROOT/samples/_rt_$name.usdc"
    dir="$WORK/$name"
    rm -rf "$dir"
    mkdir -p "$dir/a" "$dir/c"
    if ! python -c "import sys; from pxr import Sdf; \
sys.exit(0 if Sdf.Layer.FindOrOpen(sys.argv[1]).Export(sys.argv[2]) else 1)" \
            "$usda" "$usdc" >"$dir/export.log" 2>&1; then
        printf '%-28s %s\n' "$name" "EXPORT FAILED (see $dir/export.log)"
        fail=1
        continue
    fi
    (cd "$dir/a" && "$CRUST" render -i "$usda" -o "$dir/a/out.exr" -s 16 -l error) >"$dir/a.log" 2>&1
    a=$?
    (cd "$dir/c" && "$CRUST" render -i "$usdc" -o "$dir/c/out.exr" -s 16 -l error) >"$dir/c.log" 2>&1
    c=$?
    rm -f "$usdc"
    if [ $a -ne 0 ] || [ $c -ne 0 ]; then
        printf '%-28s %s\n' "$name" "RENDER FAILED (usda $a, usdc $c; see $dir)"
        fail=1
        continue
    fi
    if "$CRUST" diff "$dir/a/out.exr" "$dir/c/out.exr" >"$dir/diff.txt" 2>&1; then
        printf '%-28s %s\n' "$name" "identical"
    else
        printf '%-28s %s\n' "$name" "DIFFERS: $(head -1 "$dir/diff.txt")"
        fail=1
    fi
done
exit $fail
