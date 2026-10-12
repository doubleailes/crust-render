# Design

## Context

See proposal.md - Why. The current state that shapes the approach:

- **Two dispatch sites decide what a prim becomes.**
  - The top-level traversal is `traverse_into` in `usd_import/mod.rs`. Its order is
    `PointInstancer`, `crust:volume:type`, `Mesh`, `Sphere`, `BasisCurves`,
    camera, then lights.
  - The prototype walk is `collect_proto_parts` in `usd_import/instancing.rs`. Its
    order is `Mesh`, `Sphere`, `BasisCurves`, `crust:volume:type`, then
    `PointInstancer`. **Here the volume check comes after geometry.**
  - Anything that matches no branch in either walk is skipped with no warning.
- **Everything after the read works on a `MeshSource`.** `emit_mesh` takes a
  `&UsdMesh` and reads it with `mesh_source()`. What follows (the non-invertible
  bake, `MeshArena::intern`, `reserve_slot` / `MeshPlacement`, and the prototype
  part) works only on the resulting `MeshSource` plus the prim's own `crust:*`
  attributes.
- **A `MeshSource`'s normals mean "smooth".** `MeshSource::normals` is `Some` only
  for a subdivided mesh, and `MeshKey::smooth` enters the dedup key. Authored USD
  `normals` are not read anywhere. So a polygonal mesh renders faceted, and
  smooth shading needs normals supplied with the source.
- **Screen rates need the camera before the traversal.** `ScreenRate` (eye,
  pixels per unit at unit distance, frustum, target edge, max level) exists only
  when `crust:subdivisionEdgeLength` / `--subdiv-edge-length` is set *and* the
  camera is named before traversal (`subdiv_policy`).
  `ScreenRate::sigma_culled(xf, bounds, cull)` already answers "pixels per local
  unit for this placement": it includes the transform's `stretch`, returns 0
  when the placement is off screen, and returns ∞ when the camera is inside.
- **The schemas are already there.** openusd-schemas generates typed schemas for
  all seven shapes from its `schema.usda`; each has a `get`, an `axis_attr()` and
  so on. Their fallbacks:

  | Schema | Fallbacks |
  |---|---|
  | `Cube` | `size` 2 |
  | `Plane` | `width` 2, `length` 2, `axis` Z (the normal), `doubleSided` true |
  | `Cylinder` / `Cone` | `radius` 1, `height` 2, `axis` Z |
  | `Cylinder_1` | `radiusTop` 1, `radiusBottom` 1, `height` 2, `axis` Z |
  | `Capsule` | `radius` 0.5, `height` 1 (the cylinder part only), `axis` Z |
  | `Capsule_1` | `radiusTop` 0.5, `radiusBottom` 0.5, `height` 1, `axis` Z |

- **What the reference generators produce.** OpenUSD's `pxr/imaging/geomUtil`
  generators were checked by a spike against conda-forge `openusd` 26.08:

  | Generator | Points | Normals | Scheme |
  |---|---|---|---|
  | Cylinder (numRadial 8) | 34 = bottom centre + bottom cap ring + side bottom ring + side top ring + top cap ring + top centre | vertex | catmullClark |
  | Cuboid | 8 shared corners, 6 quads | uniform | bilinear |
  | Plane | 4 points, 1 quad | uniform | bilinear |

  - Storm draws cylinders and capsules with numRadial 10 and capsules with
    numCapAxial 4.
  - The pip `usd-core` wheel has no `GeomUtil`.
  - conda-forge's Python bindings wrap only the original signatures: no
    `radiusBottom` / `radiusTop`, no sweep, no frame.
  - The C++ headers in the same package have all of these, plus
    `UsdImagingGprimAdapter::GetImplicitBasis(axis)`.

## Goals / Non-Goals

**Goals:**
- One tessellation per shape family, matching GeomUtil exactly, so the oracle
  compares point for point and Crust draws what usdview draws.
- No new kernel geometry, no new `crust:*` attribute and no new environment switch.
- The volume-versus-geometry precedence is the same in both walks.

**Non-Goals:**
- Analytic quadrics in `crust-rt`, and changes to `Sphere` (its top-level scale gap
  stays listed in the design record's known gaps).
- Reading authored `normals` on `Mesh` prims.
- Partial sweeps: the schemas have no sweep attribute; GeomUtil's `sweep` parameter
  stays at 360°.
- Texture coordinates and Ptex on implicit shapes.
- Displacement on implicit shapes.
- `doubleSided`. Crust ignores it on every gprim; surfaces are two-sided as they
  are today.
- An instance prim's own schema. A native instance is expanded from its prototype,
  which holds the instance's children, before any dispatch (`traverse_into`), so a
  prim that is itself an instanceable `Cube`, `Mesh` or `crust:volume:*` prim
  imports nothing of its own. That is unchanged here and goes in the known gaps.

## Decisions

### D1. Tessellate into a `MeshSource` and enter the mesh path after the read

Split `emit_mesh` into two functions:
- `emit_mesh` reads `mesh_source()` as today, then calls the second;
- `place_mesh_source(world, prim, src, world_xf, bound, meshes, pending)` holds
  everything that currently follows the read, unchanged.

The prototype walk gains the same split: an implicit shape builds its source and
goes through the existing `intern` → `ProtoPart::leaf` code. The source is built
with:
- `normals: Some(generated)` for curved shapes, and `None` for `Cube` and `Plane`;
- `uvs: None`, `subdiv_faces: None`, `refined: false`;
- `base_face_count` set to the generated face count.

*Alternatives considered:*
- **(a) A subdivision cage.** An unauthored scheme is `catmullClark`, so a cube
  would round into a blob. With sharp creases the cylinder would shrink below its
  radius.
- **(b) Synthesising a USD `Mesh` in an in-memory layer.** This fights openusd
  for nothing.
- **(c) Analytic shapes in `crust-rt`.** Exact silhouettes, but new geometry kinds,
  SIMD packets and bitwise pins. Out of proportion for blockout geometry.

### D2. Four generators for seven schemas, a transcription of GeomUtil

New file `usd_import/implicit.rs`:

```
 Cube                         ──► cuboid(size, size, size)
 Plane                        ──► plane(width, length) · basis(axis)
 Cylinder, Cylinder_1, Cone   ──► cylinder(N, rBottom, rTop, height) · basis(axis)
                                    Cone: rTop = 0. Cylinder: rTop = rBottom = radius.
                                    GeomUtil's cone generator is the rTop = 0 case
                                    of the cylinder; the oracle pins both.
 Capsule, Capsule_1           ──► capsule(N, M, rBottom, rTop, height) · basis(axis)
                                    unequal radii: caps tangent to the cone,
                                    with M doubled, as GeomUtil does
```

- The generators transcribe the C++ `_GeneratePointsImpl` / `_GenerateNormalsImpl`
  and `GenerateTopology`, including vertex order, the split cap rings, the seam at
  +X before the frame, and the `rightHanded` orientation.
- `basis(axis)` transcribes `UsdImagingGprimAdapter::GetImplicitBasis`.
- Points are computed in `f64` and stored as `f32`, as GeomUtil does when it writes
  `GfVec3f`.
- The scheme GeomUtil puts on the topology (`catmullClark` for curved shapes) is
  ignored: the result is a finished surface.
- `Cube` and `Plane` render faceted because their normals are `uniform`, which
  `normals: None` reproduces with no split vertices.

### D3. Screen-adaptive radial count, separate from subdivision's target

Split `ScreenRate` into two parts:
- a `ScreenProjection` (eye, `f_px`, frustum), with `sigma_culled`;
- the subdivision fields (`target`, `max`), which stay on `ScreenRate`.

`subdiv_policy` resolves the projection **whenever a camera is named before
traversal**, even without an edge length. The import keeps it in
`ImportCaches::implicit: ImplicitPolicy { projection: Option<ScreenProjection> }`.
For an unshared placement:

```
 σ   = projection.sigma_culled(world_xf, local_bounds, Cull::Pad(0))   px per local unit
 ρ   = max(rBottom, rTop) · σ                                          radius on screen, px
 N   = ceil(π / acos(1 − ε/ρ))     for ρ > ε     (silhouette sagitta ≤ ε px)
 N   = clamp(N, N_MIN, N_MAX);     ρ ≤ ε or σ = 0 → N_MIN;  σ = ∞ → N_MAX
 M   = max(2, ceil(N / 4))         capsule cap rings (before GeomUtil's doubling)
```

The constants:

| Constant | Value | Meaning |
|---|---|---|
| `ε` | 0.5 px | silhouette error budget |
| `N_MIN` | 16 | Storm uses 10; 16 keeps reflections of off-screen shapes round enough |
| `N_MAX` | 256 | sagitta 7.5·10⁻⁵ r, enough to ρ ≈ 6600 px |
| `N_FIXED` | 48 | shared shapes and loads with no early camera; chord error 0.2 % of r |

The count depends only on the stage, the camera, the resolution and the placement,
so equal shapes at similar screen sizes still dedup.

The same `MeshPlace::World` / `Shared` split as subdivision decides "unshared":
- a direct prim is placed in the world;
- a prototype part is `Shared` unless the walk was given `ProtoPlace::Unshared`;
- a prototype reached through a native instance nested in another prototype is
  always `Shared`, because `collect_proto_parts` splices nested instances from the
  cached shared build (`count_placements` does not descend into instances). This
  matches subdivision exactly, and changing it is out of scope for both.

"Off screen gives `N_MIN`" holds while the frustum test is on (the default).
Under `CRUST_ADAPTIVE_FRUSTUM=0`, `sigma_culled` skips the overlap test by design
(that is the switch's "off" side), so an off-screen shape is sized by distance
alone, like an adaptive mesh under the same switch.

*Alternatives considered:*
- **(A) Driving the count from `crust:subdivisionEdgeLength`.** It is opt-in, so
  shapes would be fixed by default; and the edge-length criterion measures the
  wrong thing for a silhouette. Rejected by the user in exploration.
- **A world-space tolerance.** It is not scale-invariant.

### D4. Dispatch order in both walks

Both walks take the same order: volume, `Mesh`, `Sphere`, `BasisCurves`, implicit
shapes, unsupported gprim. In the prototype walk, the `crust:volume:type` branch
moves above `Mesh`.
- **Top level:** the implicit branch sits after `BasisCurves`. Shapes are not
  cameras or lights, so the order among those does not matter, but keeping the
  geometry branches together keeps the two walks readable side by side.
- **Unsupported gprims** are recognised by `prim.is_a("Gprim")` (openusd's
  `UsdPrim::IsA`, resolved through the schema registry) after every reading branch
  has failed. They are counted per type name in `ImportCaches`: once per prim met
by the top-level walk, and once per prim per prototype *build*. Placements never
add to the count (the walk runs once per build, not per placement), but a streamed
load rebuilds a prototype once per chunk that uses it. Deduplicating across chunks
is not possible by path, since `/__Prototype_N` is renumbered per chunk
(`ImportCaches::epoch`). The count is therefore documented as "prims read", which
is exact for a load that is not streamed and emitted once per
  type at the end of the load as `GeomUnsupportedType` (Skipped, registered `Each`
  because one is raised per type; the message carries the count). Emitting at the end, on the
  importing thread, keeps the count exact across streamed chunks and still lands
  on `Scene::warnings`.

### D5. The oracle: C++ against conda-forge openusd, fixture committed

This follows `adobe_oracle` exactly:

```
 scripts/geomutil_oracle.py
   ├─ downloads micromamba from conda.anaconda.org (conda-forge micromamba-2.9.0)
   │    micro.mamba.pm is not reachable from sandboxed sessions
   ├─ creates a throwaway env: python 3.13, openusd ==26.08 (pinned build string)
   ├─ compiles scripts/geomutil_oracle/probe.cpp with $CXX against the env's
   │    include/ and lib/ (libusd_geomUtil, libusd_usdImaging, libusd_gf, …)
   └─ runs it on a fixed case list → crates/crust-core/tests/data/geomutil_oracle.txt
 crates/crust-core/tests/geomutil_oracle.rs
   └─ replays every case through implicit.rs at the case's (N, M)
```

The probe calls:
- the generators' full signatures (`radiusBottom`, `radiusTop`, a frame matrix);
- `GetImplicitBasis(axis)` for the frame.

The cases cover:
- every family;
- every `axis`;
- schema fallbacks;
- `rTop = 0` (cone) and `rBottom ≠ rTop` (truncated cone, unequal capsule);
- `N ∈ {3, 10, 16, 48}` and `M ∈ {1, 4}`;
- a tiny and a huge radius.

Comparison:
- Points and normals match to 1e-6 relative.
- Counts and indices match exactly.
- Normals are compared only where GeomUtil's interpolation is `vertex`.

## Risks / Trade-offs

- **[Triangle count]** A capsule at `N_MAX` is about 2·256·130 ≈ 66 k triangles.
  → The count is chosen per placement and clamped, and only unshared placements
  can reach the clamp. A capsule filling a 2 k frame takes ρ ≈ 1000 px → N ≈ 100,
  about 10 k triangles. `--stats` gains a DEBUG line with the radial-count
  histogram, so a regression is visible.
- **[Silhouette versus shading]** A smooth normal on a coarse polygon shows the
  shadow-terminator artefact. → `N_MIN = 16` and the ε budget keep facets
  sub-pixel on screen. Off-screen shapes seen only in reflections may show them.
  This is listed as a known gap.
- **[The camera-resolution cost moves into default loads]** Resolving the
  projection can open a stage masked to a camera under a payload. → Only when a
  camera is named. This is the same code path adaptive subdivision already pays,
  and it is skipped when the index stage composes the camera.
- **[Transcription drift]** GeomUtil changes upstream. → The fixture pins the
  version: rerunning the oracle at a new pin shows exactly which points moved.
- **[Hidden scenes change]** Any user scene with these shapes changes image.
  → Intended (alpha, breaking allowed). No sample in the repo changes:
  `check_images.sh check` before and after must report every sample identical.
- **[The unsupported-type check is too broad]** It could flag the shapes themselves
  or `Volume`. → It runs only after every reading branch, and is pinned by the
  "Non-geometry prims are not reported" scenario and a test over the warnings
  sample.

## Migration Plan

Nothing to migrate. Rollback is a revert. There is no environment switch: no prior
behaviour is worth A/B-ing against, because the shapes rendered nothing before.

## Open Questions

- The exact `ε`, `N_MIN` and `N_FIXED` values may be tuned after viewing a test
  scene at 16 spp. That moves the table above, not the approach.
