#!/usr/bin/env bash
# Run the test suite across SIMD instruction-set configurations.
#
# Why this exists: the kernel's correctness claims are about *float bit
# patterns*, not tolerances. `simd_matches_scalar_bitwise` asserts that the
# 4-wide packet intersector and the scalar one agree bit-for-bit, and
# watertightness depends on adjacent triangles computing exactly equal edge
# functions. Different codegen can break that — most obviously by
# contracting `a*b + c` into an FMA, which rounds once instead of twice.
# Rust does not contract by default, but that is a property worth
# re-verifying rather than assuming, and it is the kind of thing that
# changes quietly with a toolchain bump.
#
# Usage: scripts/test_simd_matrix.sh [extra cargo test args...]
#
# The codegen check at the end reads the IR of the crate a `-p <crate>` among
# the arguments names (crust-rt without one): `-p crust-nn` checks the
# network kit, whose bit-determinism rests on the same no-contraction rule.
# `-p crust-core` tests and checks the six engine crates as well: the facade
# holds no code of its own, so its unit tests and its IR alone would cover
# nothing but the integration tests.

set -euo pipefail

engine=(crust-base crust-shading crust-world crust-integrator crust-usd crust-tools)

# The crate the codegen check reads, and the test arguments with
# `-p crust-core` widened to the engine crates beside it.
crate=crust-rt
test_args=()
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
    case "${args[i]}" in
        -p | --package) crate="${args[i + 1]:-crust-rt}" ;;
        -p* ) crate="${args[i]#-p}" ;;
        --package=*) crate="${args[i]#--package=}" ;;
    esac
    test_args+=("${args[i]}")
    previous="${args[i - 1]:-}"
    if [ "${args[i]}" = -pcrust-core ] || [ "${args[i]}" = --package=crust-core ] ||
        { [ "${args[i]}" = crust-core ] && [ $i -gt 0 ] &&
            { [ "$previous" = -p ] || [ "$previous" = --package ]; }; }; then
        for c in "${engine[@]}"; do test_args+=(-p "$c"); done
    fi
done

run() {
    local label="$1"
    shift
    local flags="$1"
    shift
    echo
    echo "=== $label"
    echo "    RUSTFLAGS=\"$flags\""
    RUSTFLAGS="$flags" cargo test "$@" 2>&1 | grep -E "test result|FAILED|^error" || {
        echo "    FAILED under $label"
        exit 1
    }
}

# Baseline: whatever the target's default features are (x86-64 => SSE2).
run "baseline (target defaults)" "" "${test_args[@]}"

# No 256/512-bit instructions at all: exercises the narrowest codegen and
# the scalar fallbacks.
run "SIMD widening disabled" \
    "-C target-cpu=native -C target-feature=-avx2,-avx512f,-avx" "${test_args[@]}"

# AVX2 + FMA: the configuration most likely to perturb float results,
# because LLVM may pick different instruction sequences for the same
# arithmetic.
run "AVX2 + FMA" "-C target-feature=+avx2,+fma" "${test_args[@]}"

# Everything the host has. On a machine with AVX-512 this covers it; note
# that GitHub Actions runners generally do not offer AVX-512, so that leg
# only really runs locally.
run "target-cpu=native" "-C target-cpu=native" "${test_args[@]}"

# Codegen, not test results: does the compiler fuse crust's own arithmetic?
# Machine code cannot answer it — glam fuses `cross` on purpose under FMA
# (explicit `llvm.fma`), so `vfmadd` appears either way — but the IR can:
# contraction needs a `contract` / fast-math flag or `llvm.fmuladd`.
#
# For crust-nn the lib's IR holds `Adam::step` and the `f64` instantiation
# `grad_check` makes of the network; its `f32` one is the same generic source,
# and whether the compiler contracts is a property of the source, not of the
# type.
if [ "$crate" = crust-core ]; then
    crates=("${engine[@]}")
else
    crates=("$crate")
fi
ir_dir="${CARGO_TARGET_DIR:-target}/simd-matrix-ir"
# An instruction carrying a fusing fast-math flag, e.g. `fmul contract float`.
flagged='= (fadd|fsub|fmul|fdiv|fneg|frem|call)( [a-z]+)* (contract|fast|reassoc)\b'
for crate in "${crates[@]}"; do
    echo
    echo "=== codegen: no FMA contraction in $crate (IR, +avx2,+fma)"
    RUSTFLAGS="-C target-feature=+avx2,+fma" CARGO_TARGET_DIR="$ir_dir" \
        cargo rustc -q --release -p "$crate" --lib -- --emit=llvm-ir
    ir=$(ls -t "$ir_dir"/release/deps/"${crate//-/_}"-*.ll | head -n 1)
    contracted=$(grep -cE "$flagged|llvm\.fmuladd" "$ir" || true)
    explicit=$(grep -c "call .*@llvm\.fma" "$ir" || true)
    echo "    contraction-eligible: $contracted; explicit llvm.fma (glam intrinsics): $explicit"
    if [ "$contracted" != 0 ]; then
        echo "    FAILED: the compiler may fuse $crate arithmetic ($ir)"
        exit 1
    fi
done

echo
echo "All SIMD configurations passed."
