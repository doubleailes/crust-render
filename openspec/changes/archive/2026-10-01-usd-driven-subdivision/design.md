## Context

Refinement lives in `mesh_source` (`scene/usd_import/mesh.rs`), before interning. Today
it runs only when `attrs::subdiv_level(prim)` reads a non-zero `crust:subdivisionLevel`.
The scheme then only picks the algorithm, and an unauthored scheme is treated as
`catmullClark`. `scene/subdiv.rs` wraps opensubdiv-rs 0.1:

- uniform refinement, then a limit snap of the positions;
- smooth normals;
- optionally a synthetic Ptex face-varying channel, refined under
  `FVarLinearInterpolation::All`, that maps refined faces back into cage-face unit squares.

The authored UV chart (`UvSource`: values, optional indices, `face_varying` flag) is
discarded whenever a mesh is refined. Render settings are read once, on the index stage,
before the traversal (`load_scene` → `import_render_settings`). CLI overrides such as `-s`
are applied after import, which is too late for geometry.

## Goals / Non-Goals

**Goals:**
- Decide subdivision from USD data alone, with USD's semantics.
- Keep the authored UV chart through refinement, with USD's face-varying semantics.
- Keep every checked-in polygon sample bit-identical (they now author `none`).

**Non-Goals:**
- Adaptive or screen-space tessellation, or a per-mesh level. The level stays global,
  like Hydra's `refineLevel`.
- `holeIndices`, `triangleSubdivisionRule`, and refining primvars other than the UV chart
  (for example `displayColor`).
- Authored `normals` on polygon meshes. They are still ignored, so a `none` cage is
  faceted even when it authors smooth normals.

## Decisions

**D1 — Trigger on the resolved scheme, fallback included.**
Every mesh whose resolved `subdivisionScheme` is not `none` is a subdivision surface; an
unauthored (or blocked) scheme resolves to the schema fallback, `catmullClark`, exactly
as in Hydra, RenderMan and Karma.
Alternative, implemented first and reverted: trigger only on an *authored* value
(`resolve_info().has_authored_value()`), on the belief that exporters meaning "polygons"
leave the scheme unauthored. Both production scenes contradict it. ALab's and
Kitchen_set's render meshes author neither a scheme nor normals — the USD way to say
"subdivision surface" — while ALab's polygonal display proxies author `none` *and*
face-varying normals. Under the authored-only rule ALab rendered its hand and glassware
as faceted cages. The price of following USD is that polygon content must say `none`, so
the checked-in samples now do.

**D2 — One resolved level, carried by the import context.**
The level is resolved once in `load_scene`, in this order:
1. `UsdImportOptions::subdivision_level` (the new CLI `--subdiv-level`);
2. the settings prim's `crust:subdivisionLevel`;
3. the default, `DEFAULT_SUBDIV_LEVEL = 0`: nothing is refined unless a render
   setting or the host asks.

Conservative on purpose (the user's call, after levels 2 and then 1 were tried as the
default). Every mesh not authoring `none` is a subdivision surface: the Moana island
authors `catmullClark` explicitly in 189 of its 213 mesh-bearing files, and ALab and
Kitchen_set take the fallback on their render meshes. Level 1 costs them 4× — ALab goes
from a 32 to a 49 GiB peak — and level 2 does not fit Moana at all. A scene that wants
refinement says so in its RenderSettings, as the DPEL teapot wrappers do.

**Level 0 shades the cage smooth.** A subdivision surface at level 0 keeps its cage's
triangles but gets smooth per-vertex cage normals (`subdiv::smooth_cage_normals`), as
Storm draws one at low complexity. That makes `--subdiv-level 0` the cheap way to render
a large subdivision scene smooth-shaded at its cage's memory. `CRUST_SUBDIV=0` is not
level 0: it is the faceted cage of the behaviour this replaced, so the A/B stays honest
(`SubdivPolicy::enabled`).

The result is clamped to `MAX_SUBDIV_LEVEL = 6`, and `CRUST_SUBDIV=0` forces it to 0. It
reaches `mesh_source` as a parameter, not a global, so the streamed and single-stage paths
see the same value. It is logged once at `DEBUG`, not stored in `RenderSettings`: that
struct belongs to the tracer, and the level is a geometry setting.
`MeshKey` already hashes the refined arrays, so dedupe is unaffected.
Alternative: re-read the settings prim per mesh. Rejected: it is on a different stage in a
streamed import, and the host override would have no place to live.

**D3 — The UV chart becomes a second face-varying channel.**
`SubdivRequest` gains an optional `uvs: Option<UvChannel>`. For `faceVarying` data it
holds the values plus per-face-vertex value indices. Those are `primvars:st:indices` when
authored, otherwise identity. Negative or out-of-range indices refuse the channel: the mesh
refines without UVs and warns once, the same outcome as today, never a bad chart.

The channel is refined level by level with `interpolate_face_varying`, then snapped with
`limit_face_varying`, so a texel stays pinned to the same limit-surface point the vertex
was snapped to. The result goes back into a `UvSource { face_varying: true, indices:
refined face fvar values }`, so the triangulation, ray-cone density and tangent code
downstream is unchanged.

`vertex` and `varying` UVs are not a face-varying channel. They are interpolated and
limited like the points, and come back as `UvSource { face_varying: false }`.

Channel index: Ptex keeps channel 0 when requested. UV is channel 0 or 1.

**D4 — One refiner, face-varying mode from the mesh, Ptex channel checked.**
In opensubdiv, `Options::fvar_linear_interpolation` applies to the whole refiner, not per
channel. The UV channel needs the authored mode. The Ptex channel needs linear behaviour.
Every Ptex value is private to its face, so every edge of that channel is a face-varying
boundary. Its data (`(0,0) (1,0) (1,1) (0,1)` per face) is affine, and every mode's
boundary rules reproduce affine data. It should therefore refine identically under every
mode.

This is pinned by a test that refines the Ptex face table under all six modes and
compares it bit for bit with the chartless `All` table. Five modes are bit-identical.
`none` is not: it smooths face-varying corners, so the Ptex corners slide (0 → 0.125…).
So when a mesh needs Ptex *and* a `none` chart, `subdivide` refines twice: once
chartless for the face table, once without the table for everything else. The same test
pins that path.
Alternative: always use two refiners. Rejected: it doubles refinement cost for the rare
material that wants both Ptex and UVs.

**D5 — Mapping `faceVaryingLinearInterpolation`.**
The mapping is one to one: `none` → `None`, `cornersOnly` → `CornersOnly`, `cornersPlus1`
→ `CornersPlus1`, `cornersPlus2` → `CornersPlus2`, `boundaries` → `Boundaries`, `all` →
`All`. Unauthored uses USD's fallback, `cornersPlus1`, not opensubdiv's default
`CornersOnly`.

**D6 — Legacy attribute.**
`crust:subdivisionLevel` on a mesh prim is read only to warn. The warning is emitted once
per load through a flag on `SubdivPolicy`, because counting per prim would scale with the
scene (logging rule). The mesh then follows D1 at the load's level, never the prim's.

## Risks / Trade-offs

- [Every mesh not authoring `none` — ALab, Kitchen_set, Moana, and any polygon export
  that leaves the scheme unauthored — gets 4× the triangles once a level of 1 is asked
  for] → the default is 0 (smooth-shaded cages at the cage's memory), and
  `CRUST_SUBDIV=0` restores the faceted cages; the level is logged at `DEBUG`, and each refined mesh
  keeps its existing `DEBUG` line (never `INFO`: it scales with the scene).
- [Import time and memory for the DPEL teapot, about 32k cage quads] → measured with
  `--stats` and `subdivision_memory_probe` (UV channel added), numbers in the design
  record.
- [Seams in the refined chart where `faceVarying` values are discontinuous] → these are
  exactly OpenSubdiv's face-varying boundaries, handled by the refiner. Pinned by a test
  on a two-quad mesh with a UV seam: the refined values on either side of the seam stay on
  their own chart.
- [Golden images] → the polygon samples author `none`, so every golden but
  `subdivision.usda` (and the DPEL teapot scenes) stays bit-identical
  (`check_images.sh check`).
