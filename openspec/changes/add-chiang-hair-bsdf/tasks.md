# Tasks

## 1. Kernel: curve hits report span parameter and tangent (crust-rt)

- [x] 1.1 Record a baseline before any code change, for the comparisons in
  later groups:
  - build `target/release/crust-render` at the base commit and copy it aside
    as binary A;
  - record goldens with `scripts/check_images.sh record <dir>`;
  - count callgrind instructions on `samples/cornellbox.usda` and
    `samples/curves.usda` at `-s 2` with `RAYON_NUM_THREADS=1`.
  Verify: the golden directory and both callgrind totals are saved under the
  scratchpad.
- [x] 1.2 Return the span parameter that `rounded_cone_intersect` and
  `cubic_curve_intersect` already compute:
  - `y/d2` on the cone body, 0 or 1 on the caps, `u0 + s·(u1 − u0)` through the
    subdivision;
  - fill `PrimHit.u` with it, and a new `PrimHit.dpdu` with the object-space
    tangent: `p1 − p0`, or the Bézier derivative at `u`;
  - every other primitive writes a zero `dpdu`;
  - expose `dpdu` on `RayHit`.
  Verify: new tests in `crates/crust-rt/tests/kernel.rs` pass:
  - a linear segment from (0,0,0) to (0,2,0) hit at height 0.5 reports
    `u = 0.25` and a +y tangent;
  - a quarter-circle span's tangent matches the analytic derivative at `u`
    within the flatness tolerance.
  Verify also: the existing kernel, `bvh/tests.rs` size pins and
  `curve.rs` tests pass unchanged.
- [x] 1.3 Map `dpdu` through `InstancePrim::hit` with the linear part of the
  transform. Use the motion-interpolated one from `transforms_at` when the
  instance moves.
  Verify: kernel tests pass for a curve under a rotating instance, an
  `Offset`-labelled group and a motion-blurred instance, comparing against the
  hand-transformed tangent.
- [x] 1.4 Add the curve-exit flag to `crust_rt::Ray`, in its existing padding:
  - curve prims reject hits with `dir · outward > 0`, tested in object space;
  - wire it through `intersect` and `occluded`.
  Verify: kernel tests pass for:
  - a ray from inside a tube ignoring it;
  - a second tube still being entered;
  - a mirrored instance;
  - an unflagged ray reporting exit hits as before.
  Verify also: `std::mem::size_of::<Ray>()` is unchanged.
- [x] 1.5 Prove nothing moved.
  Verify: `scripts/test_simd_matrix.sh -p crust-rt` passes.
  Verify also: callgrind on `cornellbox` against 1.1's baseline is within
  +0.3% instructions. If not, move the instance transform of `dpdu` to the
  final closest hit and re-measure.
- [x] 1.6 Update the records and verify each states the new hit fields, the
  flag and the measured cost:
  - `openspec/specs/intersection-kernel/design.md`;
  - `docs/embree_comparison.md` (curve `u` and tangent, as Embree reports
    them).

## 2. Curve hits carry the strand as their shading tangent (crust-core)

- [x] 2.1 In `World::intersect`, use the normalised `RayHit.dpdu` as
  `HitRecord.tangent` when the geometry has no UV map.
  Verify: a new test in `crates/crust-core/tests/usd_scene.rs` hits the cubic
  "Tuft" strand of `samples/curves.usda`, and the reported tangent is parallel
  to the strand's derivative there.
- [x] 2.2 Add a `PointInstancer` curve fixture to the same test file, with two
  differently rotated placements of a one-strand prototype.
  Verify: each hit's tangent follows its own placement.
- [x] 2.3 Prove that no sample scene's image moves (none puts a MaterialX
  material on a curve, and the other models ignore the tangent).
  Verify: `scripts/check_images.sh check <dir>` against 1.1's goldens reports
  every sample bit-identical, `curves.usda` included.
- [x] 2.4 Update the records:
  - `openspec/specs/usd-scene-import/design.md`'s `UsdGeomBasisCurves` entry
    (tangent source, span parameter);
  - `site/content/docs/usd/geometry.md` (curves shade along the strand; ribbons
    and `wrap` not read).
  Verify: `zola build` in `site/` succeeds.

## 3. MaterialX: the hair leaf and helper nodes (crust-mtlx)

- [x] 3.1 Add `Bsdf::Hair` to `bsdf.rs`:
  - slots for `tint_R`, `tint_TT`, `tint_TRT`, `ior`, the three roughness
    `vector2`s, `cuticle_angle` and `absorption_coefficient`, with the
    nodedef's defaults;
  - `normal` and `curve_direction` become the leaf's `normal` / `tangent` slots;
  - add it to `KNOWN`, `leaf()`, `Bsdf::category` and `Bsdf::for_each_slot`;
  - re-export it from `lib.rs`.
  Verify: a new `crates/crust-mtlx/tests/graph.rs` case compiles a bare
  `chiang_hair_bsdf` with no `unsupported` entry, and its slots hold the
  nodedef defaults.
- [x] 3.2 Vendor `ND_chiang_hair_bsdf` and the three helper nodedefs (MaterialX
  1.39, Apache-2.0) under `crates/crust-mtlx/tests/nodedefs/`.
  Verify: `tests/nodedefs.rs` checks the leaf's default table against them.
- [x] 3.3 Build `chiang_hair_roughness` (all three outputs, through the
  multi-output path), `chiang_hair_absorption_from_color` and
  `deon_hair_absorption_from_melanin` in `Compiler::compile_node`, from
  existing `Op`s only.
  Verify: `cargo test -p crust-mtlx` passes, and no new `Op` variant appears.
- [x] 3.4 Pin the helpers against genglsl:
  - write `scripts/hair_reference.py`, a float64 transcription of the three
    genglsl functions;
  - it writes `crates/crust-mtlx/tests/data/hair_helpers.txt` in the
    `osl_oracle.txt` case format: default inputs, the edge clamps, and 16
    cases per signature;
  - add `crates/crust-mtlx/tests/hair_helpers.rs`.
  Verify: `cargo test -p crust-mtlx --test hair_helpers` passes at 1e-5
  relative, and the script's output is byte-identical when re-run.
- [x] 3.5 Prove the JIT agrees with the interpreter on the helpers.
  Verify: a fixture graph using all three helpers is added to
  `crates/crust-jit/tests/jit.rs`, and it matches the interpreter bitwise.

## 4. The hair lobe (crust-core)

- [x] 4.1 Add `material/closure/hair.rs` with the D1 and D2 maths:
  - `Mp` (with the low-variance form), `Ap` (with closed-form TRRT+), and `Np`
    (trimmed logistic, uniform for p = 3);
  - the cuticle shifts, with genglsl's sign;
  - `eval`, `pdf`, and `sample` with the `DemuxFloat` split;
  - the grazing guard.
  Verify: unit tests ported from pbrt-v3's hair tests pass:
  - the white furnace, uniform and sampled, in [0.95, 1.05] over a grid of
    roughness and ωo;
  - sampling weights equal to tint × `Ap`;
  - sampling consistency between uniform and importance estimates;
  - plus a test pinning the R and TRT peak shift directions at
    `cuticle_angle = 0.6`.
- [x] 4.2 Add `Lobe::Hair` and hook it into `prepare()`, `eval_lobe`,
  `sample_lobe`, `albedo` (`Σ tint_p·Ap`), the layer throughput
  (`1 − albedo`), `diffuse_filter`, and `Prepared::event` (by hemisphere, D3).
  Verify: `material/closure/tests.rs` passes with a hair leaf added to its
  fixtures:
  - `no_leaf_reflects_more_than_it_receives`;
  - `every_sample_agrees_with_eval`;
  - `the_mixture_pdf_integrates_to_one`;
  - `the_lobe_split_sums_to_eval_bitwise`.
- [x] 4.3 Add `PooledClosure::hair`, set in `walk()`, and use it to set the
  curve-exit flag:
  - in `ResolvedClosure::ray`;
  - in the NEE shadow ray in `tracer/path.rs`, through a `ShadingPoint` query.
  Verify:
  - a tracer test rendering one strand in front of a small light directly
    behind it shows nonzero radiance at the strand;
  - the same strand with an `oren_nayar_diffuse_bsdf` shows the strand's
    shadow side;
  - `scripts/check_images.sh check` still reports every existing sample
    bit-identical.
- [x] 4.4 Pin resolution.
  Verify: the hair fixture materials are added to
  `crates/crust-core/tests/resolve.rs`
  (`resolve_matches_per_query_shading_for_every_material`), and the test passes
  bitwise.
- [x] 4.5 Update `openspec/specs/materials/design.md`:
  - the hair lobe;
  - each deliberate deviation from genglsl: no `1/π`, TRRT+ `Np = 1/(2π)`, and
    importance sampling;
  - the cuticle-sign reading and the roughness clamp;
  - the self-hit rule, and the joint re-entry and metre-scale known gaps.
  Verify: the record's "Known gaps" section lists both gaps.

## 5. Fixture, user documentation

- [x] 5.1 Add `samples/hair.usda` and `samples/hair.mtlx`:
  - backlit and front-lit tufts of cubic strands;
  - one material per way of reaching the leaf: a bare leaf, via
    `chiang_hair_roughness`, absorption from a colour, absorption from melanin,
    and a `mix` with a diffuse;
  - a clear fibre and its diffuse twin.
  Verify: new tests in `crates/crust-core/tests/hair.rs` pass (beside the
  strand scenes of 4.3, where they share helpers):
  - every hair material is bounded on a strand in a white furnace, the clear
    fibre at 1;
  - absorbing fibres come out warm;
  - the mix lists two leaves with weights 0.25 and 0.75.
  Verify also: `cargo run --release -- -i samples/hair.usda -o /tmp/hair.exr`
  renders with no `WARN` line.
- [x] 5.2 Document `chiang_hair_bsdf` and the three helpers in
  `site/content/docs/usd/materials.md`:
  - the inputs, and how `curve_direction` defaults to the strand;
  - an example `.mtlx`.
  Add the metre-scale limitation and the ribbon / `wrap` gaps to
  `site/content/docs/architecture/limitations.md`, and hair to `README.md`'s
  materials and geometry sections.
  Verify: `zola build` in `site/` succeeds.

## 6. Integration

- [ ] 6.1 Run CI locally:
  - `cargo fmt --all -- --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test --workspace --no-fail-fast`;
  - `cargo deny --locked check`;
  - the pinned-nightly clippy and `bvh8` legs from `CLAUDE.md`.
  Verify: all pass.
- [ ] 6.2 Run `scripts/bench_ab.sh -a <binary A from 1.1> -b target/release/crust-render`
  on `samples/curves.usda` and `samples/cornellbox.usda`, and redo 1.5's
  callgrind count.
  Verify: both are recorded, min and mean, in the materials design record.
  `cornellbox` is within the noise floor, and its callgrind count is within the
  +0.3% budget.
