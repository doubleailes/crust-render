#!/usr/bin/env bash
# The C-side ABI guard for crust-capi: compiles the real crust.h from real C
# (-std=c11 -Wall -Wextra -Werror, so a drifted declaration fails the build),
# links the release cdylib, and runs tests/smoke.c — which exercises every
# exported symbol and asserts deterministic, nonzero pixels.
#
# A shell script rather than a #[test] shelling out to cargo: a nested
# `cargo build` under `cargo test` contends the build-dir lock.
#
# Usage: scripts/test_capi_c.sh   (from the repo root)

set -euo pipefail
cd "$(dirname "$0")/.."

CC="${CC:-gcc}"

cargo build --release -p crust-capi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

"$CC" -std=c11 -Wall -Wextra -Werror \
    crates/crust-capi/tests/smoke.c \
    -Icrates/crust-capi/include \
    -Ltarget/release -lcrust_capi -lm \
    -o "$tmp/smoke"

LD_LIBRARY_PATH="target/release${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" "$tmp/smoke"
echo "test_capi_c.sh: OK"
