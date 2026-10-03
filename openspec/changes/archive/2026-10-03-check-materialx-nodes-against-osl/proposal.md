## Why

`crust-mtlx` reimplements the MaterialX standard-library pattern nodes from
the specification, and nothing checked that reimplementation against
MaterialX's own. Every error in it renders as something plausible: a wrong
default, a truncating `modulo` or a `luminance` that ignores its coefficients
produces a colour, not a crash, and "verified in numbers, not by eye" had no
reference number to compare against. MaterialX ships a reference
implementation of every stdlib node as OSL source, run through its `genosl`
code generator, and OSL's `testshade` runs such a shader once and prints the
result. That is an oracle crust can be held to without depending on either
tool at build time.

## What Changes

- Add `scripts/osl_oracle.py`, which generates one-node MaterialX documents
  for every stdlib signature of the pattern nodes `crust-mtlx` compiles (259
  signatures over `float`, `vector2/3/4`, `color3/4`), turns each into OSL with
  MaterialX's `genosl`, compiles it with `oslc`, runs it with `testshade` and
  writes the expected lanes to `crates/crust-mtlx/tests/data/osl_oracle.txt`
  (4 420 cases, committed).
- Add `crates/crust-mtlx/tests/osl_oracle.rs`, which replays every case through
  `Compiler` + `Program::eval` and fails on any lane that differs, except where
  a named deliberate-deviation rule applies.
- **Fix** the node semantics the oracle found wrong, to match the reference:
  unauthored defaults (`multiply` / `divide` / `power` / `modulo` `in1` = 0,
  `ln` `in` = 1, `artistic_ior` reflectivity), `sign(0)`, floored `modulo`,
  OSL's `pow` for negative bases and `0^y`, `smoothstep` / `clamp` with
  inverted edges, `luminance`'s `lumacoeffs` input and `color4` alpha,
  four-lane `dotproduct` / `normalize`, the wide `combine2` signatures,
  `convert`'s alpha-1 widening, `normalmap`'s `vector2` scale, `ln`'s guard
  constant and `artistic_ior`'s IOR blend. The JIT (`crust-jit`) changes with
  the interpreter wherever it inlines an affected op, so the two stay bit
  identical.
- **BREAKING (output)**: a MaterialX graph using any affected node, with an
  affected input, renders differently — as the reference renders it. `Op::Luminance`
  gains a `coeffs` operand (a `crust-mtlx` API change).

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `materials`: MaterialX pattern nodes are required to evaluate as the
  MaterialX reference implementation does, up to a listed set of deliberate
  guards.

## Impact

- `crates/crust-mtlx/src/eval.rs` (operators and defaults),
  `crates/crust-mtlx/src/value.rs` (`broadcast_to` is `pub(crate)`),
  `crates/crust-jit/src/lib.rs` (`Convert`, `Combine2`, `Smoothstep` inlining;
  widths of `Normalize`, `Luminance`, `Combine2`).
- New test data (~360 KiB) and a Python script that needs MaterialX (`pip`)
  and a source-built OSL; neither is needed by `cargo test` or CI.
- No change to render throughput: the changed ops keep their shapes, and the
  new `Convert` ops `combine2` emits are inlined by the JIT.
