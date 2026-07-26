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

set -euo pipefail

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
run "baseline (target defaults)" "" "$@"

# No 256/512-bit instructions at all: exercises the narrowest codegen and
# the scalar fallbacks.
run "SIMD widening disabled" \
    "-C target-cpu=native -C target-feature=-avx2,-avx512f,-avx" "$@"

# AVX2 + FMA: the configuration most likely to perturb float results,
# because LLVM may pick different instruction sequences for the same
# arithmetic.
run "AVX2 + FMA" "-C target-feature=+avx2,+fma" "$@"

# Everything the host has. On a machine with AVX-512 this covers it; note
# that GitHub Actions runners generally do not offer AVX-512, so that leg
# only really runs locally.
run "target-cpu=native" "-C target-cpu=native" "$@"

echo
echo "All SIMD configurations passed."
