# Tasks

## 1. Baseline

- [x] 1.1 Record the pre-change references: `scripts/check_images.sh record` goldens for `samples/curves.usda`, `samples/hair.usda` and `samples/cornellbox.usda` at 16 spp with `--indirect-clamp 0`, and the callgrind instruction counts of `cornellbox` (`-s 2`, `RAYON_NUM_THREADS=1`) and `curves`; verify the goldens directory and the two counts are saved in the scratchpad for groups 4 and 7

## 2. MaterialX operators (crust-mtlx, crust-jit)

- [x] 2.1 Add `rgbtohsv` and `hsvtorgb` (`color3` and `color4`) as nodes over the existing `rgb_to_hsv` / `hsv_to_rgb` in `eval/apply.rs`, add their signatures to the OSL oracle fixture (`scripts/osl_oracle.py`, `tests/osl_oracle.rs`), and verify `cargo test -p crust-mtlx --test osl_oracle` passes with the new cases and the HSV round-trip scenario holds
- [x] 2.2 Add `ifgreater`, `ifgreatereq` and `ifequal` over every value type MaterialX defines them for (including the `boolean` outputs as float), add them to the oracle fixture, and verify the oracle test and the "conditional picks a branch" scenario as a unit test
- [x] 2.3 Move the integer hash in `crust-core/src/volume.rs` (`hash3`) to `utils`, where `curve:random` (crust-core) can use it too, keeping volume noise bit-identical (verify `cargo test -p crust-core --test medium` and the volume samples identical to their goldens). crust-mtlx keeps its own hash: it has no crust dependencies, and the random nodes must use MaterialX's own lookup3 cellnoise hash to match the reference
- [x] 2.4 Add `randomfloat` and `randomcolor` on a `seed` input, matching MaterialX's stdlib definition and reference hash; check them against the reference when regenerating the oracle, and if the hash cannot be matched bit for bit, record that in `openspec/specs/materials/design.md` § Known gaps and test determinism and uniformity instead; verify with the oracle test or the replacement tests
- [x] 2.5 Compile every new node in `crust-jit` and extend `crates/crust-jit/tests/jit.rs` so each one is pinned bit-identical to the interpreter; verify `cargo test -p crust-jit` and `scripts/test_simd_matrix.sh -p crust-mtlx` pass
- [x] 2.6 Document the new nodes in the materials design record's MaterialX node list and in `site/content/docs/usd/materials.md`; verify `zola build` in `site/` passes

## 3. Shading context and geompropvalue (crust-mtlx, crust-jit)

- [x] 3.1 Add `curve: Option<CurveCtx>` (`v`, `index`, `random`, primvar values in program order) to `ShadeCtx` in `crust-mtlx/src/eval/mod.rs`, and have every existing caller pass `None`; verify `cargo test --workspace` passes unchanged
- [x] 3.2 Compile `geompropvalue` `curve:v` / `curve:index` / `curve:random` to curve-property ops and any other non-`st` name to a named-primvar op with the node's `default` as fallback, replacing today's "unsupported node" path (`compile.rs`, `is_default_chart` keeps `st`); expose on the compiled program the list of primvar names it reads; verify unit tests for a curve context, a `None` context returning `default`, and float → color3 broadcast
- [x] 3.3 Read the curve record in the JIT through the interpreter's accessor layout and add a curve fixture (every curve property, a primvar of each width, every new node) to `crates/crust-jit/tests/jit.rs`; verify it is bit-identical to the interpreter

## 4. Strand table: v and curve index (crust-core)

- [x] 4.1 Make `curve_segments` and `nurbs_curve_segments` (`usd_import/shapes.rs`) keep, per span, its curve index and its `[v0, v1]` interval (cubic `k/n`, linear `k/(cnt−1)`, NURBS knot interval over the domain narrowed by `ranges`), only when a bound material needs a curve attribute; verify import unit tests for a 7-point `bspline`, a 5-point `linear` and a trimmed NURBS curve give the interval ends the usd-scene-import scenarios state
- [x] 4.2 Add a per-geometry span → strand table in `rt_world.rs`, built the way `FaceMap` / `SideTables` are and only for geometry carrying it, and fill the hit's curve record (`v = v0 + u·(v1 − v0)`, index, `curve:random` from the shared hash of the prim path hash and index) at every placement, prototype tables scoped by the stage epoch; verify with a test reading `curve:v` and `curve:index` on top-level, natively instanced and `PointInstancer`-placed strands (curves carry no motion blur: `crust:motion:translate` is not read on them)
- [x] 4.3 Add the curve-table size to the `--stats` scene statistics; verify `crates/crust-core/tests/stats.rs` covers it and it is absent for a scene without curve tables
- [x] 4.4 Add `curve:random` tests: stable across two frames of an animated constant-topology groom, different between two identical prims, and within 10% of uniform over ten bins for 10 000 curves; verify they pass
- [x] 4.5 Verify the no-cost requirement: `scripts/check_images.sh check` against the 1.1 goldens is unchanged for `curves`, `hair` and `cornellbox`, and callgrind `cornellbox` is within the tangent's `+0.34%` of the 1.1 count; record the measured delta in the materials design record

## 5. Curve primvars (crust-core)

- [x] 5.1 Collect, per curve prim, the primvars its bound materials read (the programs' name lists from 3.2) plus the chart when `Material::uses_uv()` / `uv_primvar()` asks, as `MeshNeeds::of` does for meshes; verify an import unit test where a prim authoring ten primvars decodes only the one read
- [x] 5.2 Decode `constant` / `uniform` per curve, `varying` per span end, and `vertex` per span as four Bézier control values through the span's own `basis_to_bezier` (two end values for linear), for `float`, `float2`, `float3`, `color3f` (with its colour space, per `docs/color_management.md`) and `int`; evaluate them at the hit into the curve record; verify tests that a `vertex` B-spline colour equals the B-spline of the control colours inside a span, a `uniform` colour per strand, and `varying` linear within a span
- [x] 5.3 Fill `ShadeCtx::uv` from the curve's `st` (or named chart) and read (0, 0) when the prim authors none; verify a test sampling an image along a strand and a no-`st` curve raising no warning
- [x] 5.4 Add `WarningCode`s `curves.unsupported_primvar` (unreadable type, `faceVarying`) and `curves.reserved_primvar` (`primvars:curve:*`), raised with `crust_core::warning!` on the importing thread, documented in `site/content/docs/reference/warnings.md`; verify the warnings-table test and a test for each scenario that raises them
- [x] 5.5 Add the read-primvar set to `Signature` in `rt_world.rs` so an MCP edit that starts reading a primvar or curve property re-imports; verify a `crates/crust-core/tests/live.rs` case that the edited render is bit-identical to a fresh import of the same layer
- [x] 5.6 Extend `crates/crust-core/tests/resolve.rs` with a curve-reading material on a curve hit; verify resolve is bit-identical to per-query shading

## 6. Documentation

- [x] 6.1 Document curve primvars, the chart on curves, and the `curve:v` / `curve:index` / `curve:random` properties (including `curve:random`'s dependence on curve order and the stable-id `seed` recipe) in `site/content/docs/usd/geometry.md` and `site/content/docs/usd/materials.md`; remove "curve primvars don't reach materials" from `site/content/docs/architecture/limitations.md`; verify `zola build` passes
- [x] 6.2 Update the design records: retire the curve-primvar gap in `openspec/specs/materials/design.md` and `openspec/specs/usd-scene-import/design.md`, and describe the strand table, the on-demand decode and the measured cost; verify no "primvars are not read" / "do not reach materials" sentence about curves remains (`grep`)

## 7. Integration

- [x] 7.1 Add a sample groom (`samples/`) with a root-to-tip ramp, per-strand `curve:random` hue variation through `hsvtorgb`, and a `uniform` colour primvar, and an integration test loading it; verify it renders and `crust check` reports no warnings
- [x] 7.2 Run the CI set: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`, `cargo deny --locked check`, and the pinned nightly clippy; verify all pass (one test, `loads_openpbr_showcase_usda`, fails on the base branch as well: commit 643cef8 raised its sample to 1280×720 without updating it)

## Workflow follow-up

- Sync the delta specs into `openspec/specs/materials/spec.md` and `openspec/specs/usd-scene-import/spec.md` and archive the change (`/opsx:archive`) once reviewed.
- `curve-mass-field` depends on this change's `v` and curve index; revisit its proposal after archive.
