# Reference generator

`ref_gen.cpp` regenerates the golden vectors in `../tests/golden_upstream.rs`
directly from the upstream C++ headers, so the Rust port can be checked
bit-for-bit against the original.

## Regenerating

Clone the upstream headers and compile against them (scalar path, so the values
match the portable Rust implementation):

```bash
git clone --depth 1 https://github.com/AcademySoftwareFoundation/openqmc.git
g++ -std=c++14 -O2 -DOQMC_FORCE_SCALAR -I openqmc/include \
    ref_gen.cpp -o ref_gen
./ref_gen > ../tests/golden_upstream.rs
```

Then `cargo test -p openqmc-rs --test golden_upstream` must pass. The generator
emits Rust `#[test]`s whose expected values are the upstream outputs; the test
bodies recompute the same quantities through this crate's public API and assert
equality. It covers `pcg`, `owen`/`rank1` cores, `State64Bit`, the PMJ table,
and full draws for all six samplers (Sobol/Lattice/PMJ + blue-noise variants)
across a grid of `(x, y, frame, index, domain)`.

The blue-noise `.bin` blobs under `../src/data/` are likewise a verbatim
little-endian conversion of `openqmc/include/oqmc/data/**/*.txt` (65 536 `u32`
entries each).
