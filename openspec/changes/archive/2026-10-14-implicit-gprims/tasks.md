# Tasks

## 1. Baseline and the volume-order fix (D4)

- [x] 1.1 `scripts/check_images.sh record /tmp/implicit-golden` on the pre-change
      binary. This is the baseline every later group checks against. Done when it
      completes for every sample.
- [x] 1.2 In `collect_proto_parts`, move the `crust:volume:type` branch above
      `Mesh`. Add a `usd_inline.rs` test: a `crust:volume:type` `Cube` inside a
      native-instance prototype and inside a `PointInstancer` prototype raises
      `VolumeInPrototype`, and attaches neither a volume nor geometry. Done when the
      test passes, `cargo test -p crust-core` is green, and
      `check_images.sh check /tmp/implicit-golden` reports every sample identical.

## 2. The GeomUtil oracle (D5)

- [x] 2.1 `scripts/geomutil_oracle/probe.cpp`. It reads a case list (family, N, M,
      radii, height, size, width, length, axis) on stdin. For each case it writes
      the topology (face counts, indices, scheme), the points, the normals and
      their interpolation. It calls the generators' full signatures, with the frame
      from `UsdImagingGprimAdapter::GetImplicitBasis(axis)`. Done when it compiles
      against the scratch env from the exploration spike and prints
      `CylinderMeshGenerator`'s 34 points for N = 8.
- [x] 2.2 `scripts/geomutil_oracle.py`, in the shape of `scripts/adobe_oracle.py`
      (docstring with requirements, `--out`, `--keep`):
      - bootstrap `micromamba-2.9.0` from conda.anaconda.org;
      - create an env with `openusd` pinned to version and build string;
      - compile with `$CXX`, rpath into the env;
      - run the fixed case list from D5;
      - write `crates/crust-core/tests/data/geomutil_oracle.txt`.

      Done when a clean run produces the fixture and a second run leaves it
      byte-identical.
- [x] 2.3 Commit the fixture. Add the oracle to the command cookbook in
      `openspec/specs/cli/design.md` and to CLAUDE.md's Commands block, beside
      `adobe_oracle.py`. Done when both mention the script and the test name.

## 3. Generators (D2)

- [x] 3.1 `usd_import/implicit.rs`:
      - `basis(axis)`, transcribing `GetImplicitBasis`;
      - `cuboid`, `plane`, `cylinder(N, rBottom, rTop, height)` and
        `capsule(N, M, rBottom, rTop, height)`, transcribing GeomUtil's points,
        normals and topology, with `f64` maths and `f32` output.

      Each returns points, face counts, indices, and `Option<normals>` (`None` for
      cuboid and plane).
- [x] 3.2 `crates/crust-core/tests/geomutil_oracle.rs` replays every fixture case:
      - points and vertex normals to 1e-6 relative;
      - counts and indices exactly.

      Done when it passes for all cases, including `axis` X / Y / Z, `rTop = 0`,
      unequal capsule radii and N = 3. Expose the generators through a
      `#[doc(hidden)]` path for the test, the way `adobe_oracle.rs` reaches
      `OpenPBR`.
- [x] 3.3 Readers: per-schema functions that read the authored attributes at
      `eval_time()` with the schema fallbacks (`double` or `float` accepted) and
      return a family plus parameters. Add unit tests for the fallbacks of all seven
      schemas, and for `Cone` → `rTop = 0` and `Cylinder` → equal radii. Done when
      the tests pass.

## 4. Into the mesh path (D1, D4)

- [x] 4.1 Split `emit_mesh` into the read and `place_mesh_source`. Done when
      `cargo test -p crust-core` is green and `check_images.sh check` reports every
      sample identical (a pure refactor).
- [x] 4.2 Top-level dispatch: an implicit branch after `BasisCurves`. It builds the
      `MeshSource` (D1 fields) at `N_FIXED` for now and calls `place_mesh_source`.
      If the bound material carries a displacement, raise the new
      `ImplicitDisplacementRefused` warning and place the shape undisplaced. Add
      tests in `usd_scene.rs`:
      - each of the seven schemas attaches geometry with the right bounds;
      - two identical cylinders dedup into one distinct mesh placed twice;
      - a moving cube instances;
      - a scaled cube's bounds follow the scale;
      - a displaced cube warns.
- [x] 4.3 Prototype dispatch: the same branch in `collect_proto_parts`, after
      `BasisCurves`, through `intern` and `ProtoPart::leaf`. Add tests: a `Capsule`
      in a native-instance prototype placed twice, and a `Cube` in a
      `PointInstancer` prototype, both render geometry. Done when the tests pass.
- [x] 4.4 Warnings: add `GeomUnsupportedType` (`geom.unsupported_type`, Skipped)
      and `ImplicitDisplacementRefused` to `warnings.rs`, with rows in
      `site/content/docs/reference/warnings.md`. Count unread `is_a("Gprim")` prims
      per type in both walks and emit once per type at the end of the load, with the
      count. Add tests:
      - 3 `Points` + 1 `NurbsPatch` give exactly two warnings with counts 3 and 1;
      - one inside a prototype is counted;
      - a prototype holding one `Points`, placed fifty times in a non-streamed
        load, gives count 1;
      - `Xform`, `Scope`, `Camera`, `Material`, lights and the seven shapes raise
        none.

      Done when these tests and the warnings-page test pass.
- [x] 4.5 Docs:
      - `site/content/docs/usd/geometry.md`: a "Supported prims" section listing the
        seven shapes, their attributes, faceted versus smooth, no UVs, no
        displacement, and the unsupported-type warning; the `crust:rayMask` and
        `crust:motion:translate` "read on" lines extended to the shapes;
      - `site/content/docs/usd/overview.md`: the "where each attribute goes" table.

      Done when `zola build` (0.21) in `site/` passes its link and anchor check.

## 5. Adaptive radial count (D3)

- [x] 5.1 Split `ScreenRate` into `ScreenProjection` (eye, `f_px`, frustum,
      `sigma_culled`) and the subdivision target and max. Done when
      `cargo test -p crust-core` is green and the adaptive-subdivision tests are
      unchanged.
- [x] 5.2 `subdiv_policy`: resolve the projection whenever a camera is named
      before traversal, even with no edge length. Keep it as
      `ImplicitPolicy { projection }` in `ImportCaches`. With no named camera, no
      warning. Add tests:
      - a stage with `RenderSettings.camera` and no edge length has a projection;
      - one with no camera has none and raises no warning.
- [x] 5.3 `ImplicitPolicy::radial_count(place, world_xf, local_bounds, r_max)`, by
      D3's formula and constants:
      - capsule `M = max(2, ceil(N/4))`;
      - `MeshPlace::Shared` → `N_FIXED`;
      - no projection → `N_FIXED`.

      Wire it into both dispatch branches, replacing 4.2's fixed count. Add unit
      tests:
      - monotonic in distance;
      - clamped at `N_MIN` off-screen (σ = 0) and at `N_MAX` with the camera inside
        (σ = ∞);
      - a shared prototype takes `N_FIXED`, and so does a cylinder reached only
        through a nested native instance whose outer prototype is placed once;
      - with `CRUST_ADAPTIVE_FRUSTUM=0`, an off-screen cylinder is sized by
        distance, not clamped to `N_MIN`;
      - the sagitta at the chosen N is ≤ ε px, and at N − 1 it is > ε px (unless
        clamped).
- [x] 5.4 A DEBUG line at the end of import with the histogram of radial counts
      and the implicit-shape triangle total. Nothing per prim at INFO.
      Done when `-l debug` on the test scene of 6.1 shows the line and a default
      render still prints four INFO lines.
- [x] 5.5 Docs:
      - `geometry.md`: how the segment count is chosen, the camera requirement, and
        the shared-prototype rule;
      - `openspec/specs/usd-scene-import/design.md`: a new "Implicit shapes"
        section carrying D1–D5 and its traps (normals mean "smooth"; volume order
        in the prototype walk; the Python bindings lack `_1`), and a "Known gaps:
        implicit shapes" section (no UVs or Ptex, no displacement, terminator
        artefacts on off-screen shapes at `N_MIN`, the fixed count for shared
        and nested prototypes, the unsupported-type count per prototype build, an
        instance prim's own schema not read);
      - the importer table in `docs/architecture.md`.

      Done when `zola build` passes.

## 6. Integration checks

- [x] 6.1 Add `samples/implicit_shapes.usda`: all seven schemas, every `axis`,
      one `Capsule_1` and one `Cylinder_1` with unequal radii, a camera named in
      `RenderSettings.camera`, and an OpenPBR material. Render it at `-s 16
      --indirect-clamp 0`. Done when silhouettes show no visible facets at the
      render resolution, and `cargo run --release -- check -i
      samples/implicit_shapes.usda --json -` lists no warnings.
- [x] 6.2 `check_images.sh check /tmp/implicit-golden`. Done when every existing
      sample is identical (only the new sample is new).
- [x] 6.3 Callgrind cornellbox at 2 spp (`RAYON_NUM_THREADS=1`), before and after.
      Done when `render_pixel` and `Bvh::hit` instruction counts are unchanged
      (no shapes there, so the render loop must not move).
- [x] 6.4 CI set: `cargo fmt --all -- --check`, `cargo clippy --workspace
      --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`,
      `cargo deny --locked check`. Done when all four are green.

## Workflow follow-up

- Sync the delta into `openspec/specs/usd-scene-import/spec.md` and archive the
  change (`/opsx:archive`).
