# Proposal

## Why

The importer reads only `Mesh`, `Sphere` and `BasisCurves` as surfaces. Every other
`UsdGeomGprim` falls through both of its dispatch sites (the top-level traversal and
the prototype walk) and disappears with no warning. That includes the implicit
shapes `Cube`, `Plane`, `Cylinder`, `Cylinder_1`, `Cone`, `Capsule` and `Capsule_1`,
which are common in blockouts, layout proxies, props and test scenes. A scene that
renders in usdview, where Storm draws these shapes as meshes from `pxr/imaging/geomUtil`,
renders with holes in Crust and gives no hint why.

## What Changes

- **New: the implicit shapes render.** `Cube`, `Plane`, `Cylinder`, `Cylinder_1`,
  `Cone`, `Capsule` and `Capsule_1` are tessellated at import into the triangles
  USD's own `GeomUtil` generators produce: the same vertex order, split cap rings,
  seam and `axis` frame. They then go through the existing mesh path, so they get
  deduplication, the bake-or-instance decision, prototypes, `crust:rayMask`,
  `crust:motion:translate`, light linking and material binding, with no new kernel
  geometry.
  - Curved shapes shade with GeomUtil's smooth vertex normals.
  - `Cube` and `Plane` render faceted.
  - The tessellation is never subdivided, whatever the topology's scheme.
- **New: screen-adaptive radial count.** Whenever the render camera is resolvable
  before traversal (`--camera` or `RenderSettings.camera`), the radial segment count
  of each **unshared** curved shape is the smallest that keeps its silhouette
  within a fixed screen-space chord error.
  - This is independent of `crust:subdivisionEdgeLength`: shapes are adaptive by
    default.
  - **Shared** shapes (prototypes placed more than once) and every shape in a load
    with no early camera use one fixed count.
  - A capsule's cap rings follow the radial count.
- **New: a `geom.unsupported_type` warning.** Any `Gprim` that neither walk reads
  (`Points`, `NurbsPatch`, `NurbsCurves`, `HermiteCurves`, `TetMesh`, or a future
  type) is reported, once per type with a count, instead of vanishing.
- **Fix: the prototype walk checks for a volume before geometry.** A
  `crust:volume:*` `Cube` inside a prototype keeps its `VolumeInPrototype` warning
  and never becomes a solid box.
- **Refused, with a warning:** displacement bound to an implicit shape (as on
  `Sphere`). Not generated: texture coordinates (GeomUtil produces none).
- **New tooling: a GeomUtil oracle.**
  - `scripts/geomutil_oracle.py` bootstraps a pinned conda-forge `openusd` with
    `micromamba` and compiles a C++ probe against `GeomUtil` and
    `UsdImagingGprimAdapter::GetImplicitBasis`. It writes a committed fixture.
  - A test replays the fixture point for point against Crust's generators.
  - The Python bindings are not enough: they lack the `_1` radii and the axis frame.
- **BREAKING (images):** scenes that author these shapes now render them where they
  rendered nothing before. No sample in the repo changes: every `Cube` there is a
  volume, and volumes are still checked first.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `usd-scene-import`:
  - "Geometry schema mapping" gains the implicit shapes and the
    unsupported-type warning.
  - New requirement for the adaptive radial count of implicit shapes.
  - "Volume region import" states that a volume prim in a prototype is never read
    as geometry.

## Impact

- **Code**:
  - `crust-core/src/scene/usd_import/`:
    - a new `implicit.rs` holding the generators and the radial-count policy;
    - `mesh.rs`: `emit_mesh` split into "read a source" and "place a source";
    - `mod.rs` and `instancing.rs`: dispatch, in both walks;
    - `mod.rs`: `subdiv_policy` also resolves the screen projection when no edge
      length is set.
  - `crust-core/src/warnings.rs`: `GeomUnsupportedType`, `ImplicitDisplacementRefused`.
- **Tests**:
  - `crust-core/tests/geomutil_oracle.rs` and its fixture
    `tests/data/geomutil_oracle.txt`;
  - import tests in `usd_scene.rs` / `usd_inline.rs`;
  - the warnings-page test.
- **Tooling**: `scripts/geomutil_oracle.py`, `scripts/geomutil_oracle/probe.cpp`.
  These need network access to conda.anaconda.org and a C++17 compiler. CI never
  runs them.
- **Docs**:
  - `site/content/docs/usd/geometry.md`, `overview.md`;
  - `site/content/docs/reference/warnings.md`;
  - `openspec/specs/usd-scene-import/design.md`: a new section and known gaps;
  - the importer table in `docs/architecture.md`.
- **Performance**:
  - No change to render throughput for scenes without these shapes.
  - Import cost is proportional to the generated triangles: about 2N² for a
    capsule, 4N for a cylinder.
  - The count is clamped so that a shape filling the frame stays in the low
    thousands of triangles.
- **Dependencies**: none in the workspace. The oracle's dependencies live outside Cargo.
