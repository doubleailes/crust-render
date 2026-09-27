## 1. Reference generator

- [x] 1.1 `scripts/osl_oracle.py`: enumerate the stdlib nodedefs of the
      categories `Compiler::compile_node` implements (minus `image`,
      `tiledimage` and the shading-global nodes), over crust's value types.
- [x] 1.2 Per signature, a defaults case and 16 seeded random cases from
      three-decimal literals; `normalmap` frames fed through connections
      (genosl binds a `defaultgeomprop` in place of an authored literal).
- [x] 1.3 genosl → `oslc` → `testshade -g 1 1`, the result printed with
      `%.9g` by a `printf` appended to the shader body.
- [x] 1.4 Refuse an OSL built with fast math (`acos(0.5)` probe).
- [x] 1.5 Commit `crates/crust-mtlx/tests/data/osl_oracle.txt`.

## 2. Replay test

- [x] 2.1 `crates/crust-mtlx/tests/osl_oracle.rs`: rebuild each case as a
      document, compile, evaluate, compare lanes (1e-5 relative, non-finite
      exactly).
- [x] 2.2 Per-lane `deviation()` rules for the guards in the spec, each keyed
      on the input condition it covers; counts printed.

## 3. Fixes found by the oracle

- [x] 3.1 Unauthored defaults of `multiply`, `divide`, `power`, `modulo`,
      `ln`, `artistic_ior`; `graph.rs`'s defaults test corrected to the
      nodedef.
- [x] 3.2 `sign`, `modulo`, `power`, `clamp`, `smoothstep`, `ln` guard.
- [x] 3.3 `luminance` (`lumacoeffs`, alpha; `Op::Luminance` gains `coeffs`),
      `dotproduct` and `normalize` over the value's lanes.
- [x] 3.4 `convert` widening fill and narrowing to `float`; `combine2`
      concatenation with per-signature operand widths.
- [x] 3.5 `normalmap` `vector2` scale; `artistic_ior` blend as OSL's `mix`.
- [x] 3.6 `crust-jit`: `Convert`, `Combine2`, `Smoothstep` inlining and the
      widths of `Normalize`, `Luminance`, `Combine2`; `jit.rs` extended to
      every `Convert` width and inverted `smoothstep` edges.

## 4. Records

- [x] 4.1 `openspec/specs/materials/design.md`: the oracle, what it found, the
      guards, how to build OSL for it; its coverage limits under Known gaps.
- [x] 4.2 `CLAUDE.md` command list, `docs/architecture.md` § Tests and
      verification, `openspec/specs/cli/design.md` cookbook.
- [x] 4.3 `scripts/check_images.sh` over the sample scenes, old binary against
      new, to see which renders move: `materialx_basic`, `materialx_cutout`
      and `materialx_surfaces`, by ≤ 4.6e-5, all from the `artistic_ior` blend.
