## Context

See `proposal.md` § Why for the motivation. The import pipeline this change hooks into
(file:line references as of 2026-10-03):

- **One place turns a cage into refined arrays.** `traverse_into` resolves the prim's
  material first (`usd_import/mod.rs:374`), then calls
  `emit_mesh` → `mesh_source(...) -> MeshSource` (`mesh.rs:922`).
  - `MeshSource` carries `points`, `counts`, `indices`, `normals` (`Some` iff the mesh
    is refined, smooth or limit), and optionally `subdiv_faces` and `uvs`.
  - `MeshArena::intern` (`mesh.rs:308`):
    - hashes `MeshKey` over points, topology, UVs, the smooth flag and the material
      pointer;
    - then triangulates, builds `FaceMap` / `UvMap`, and computes texture densities
      from the local vertices.
  - A placement with a non-invertible transform bypasses `intern` and bakes inline
    (`mesh.rs:492-566`).
- **Where normals come from:**
  - uniform `subdivide`: `smooth_normals`, area-weighted over refined triangles;
  - level 0: `smooth_cage_normals`;
  - per-face `tessellate_adaptive`: limit normals `du × dv` from the patch table;
  - scheme `none`: no normals, so the mesh is faceted.
- **Chart coordinates are known, but only per face.**
  - Uniform refinement keeps Ptex coordinates per refined face (`SubdivFaces`, or a
    dyadic `SubFace` cell). UV coordinates are per vertex or per face-vertex.
  - The per-face tessellator's `emit` closure sees `(key, ptex_face, uv)` for every
    point it creates. It deduplicates shared corner and edge points through
    `VertexKey`, and the first face to emit a point owns it (`subdiv.rs:704`,
    `948-975`).
- **The material and its textures are fully resolved when `mesh_source` runs.** That
  happens per prim, cached per `(epoch, path)`. There is one material per prim
  (GeomSubsets are not read).
- **Texture samplers work at a vertex.**
  - `Texture2D::eval(u, v, width)` and `PtexTexture::eval(face, u, v, width)` are
    `Send + Sync`. A `width` of 0 point-samples.
  - A crust-mtlx `Program` evaluates from a `ShadeCtx { uv, normal, tangent, view,
    position, uv_width }` that needs no `HitRecord`. `Presence` (`materialx.rs:76`)
    is the existing one-root sub-program.
  - `UvInput::sample` takes a `HitRecord` and needs a vertex entry point.
- **Ptex is always decoded by gamma 2.2.** `load_ptex` takes no colour space
  (`crust-assets` `ptex_texture.rs:221`, `ptex_stream.rs:735, 827`).
- **Adaptive dicing tests the undisplaced cage.** The per-mesh path uses
  `Aabb::of_points` (`mesh.rs:234`). The per-edge path uses `segment_at`, which pads
  only by the segment's own diagonal (`adaptive.rs:117`).
- **The import walk is single-threaded** and reads time through the thread-local
  `EvalTimeScope`. Only prototype BVH commits run on rayon.

## Goals / Non-Goals

**Goals:**
- Displaced geometry that is watertight wherever the undisplaced tessellation is, for
  every tessellation path: uniform, per-mesh adaptive, per-face adaptive, level 0, and
  bilinear `none`.
- Zero change, bit for bit, for scenes without displacement, and for any scene under
  `CRUST_DISPLACE=0`.
- Import cost proportional to unique displaced vertices, paid once per distinct mesh.
- No change to `crust-rt`, to the `Material` trait, or to render-time throughput per
  triangle.

**Non-Goals:**
- **Vector displacement.**
- **Dicing driven by displacement.** That covers curvature- or amplitude-aware rates
  and re-dicing after displacement. Rates still come from the undisplaced cage's size
  on screen.
- **Lazy, per-ray or cached-on-first-hit dicing**, or a displacement-aware BVH (no
  displacement bounds inside the kernel).
- **Recovering sub-dicing detail as bump.** Production renderers often add a bump
  from the residual between the map and the diced surface.
- **GeomSubset-level displacement**, since GeomSubsets are not read at all.
- **Displacing analytic spheres.**

## Decisions

### 1. Displace between tessellation and kernel hand-off, once per distinct mesh

The pass runs inside `MeshArena::intern`, after the `MeshKey` lookup misses and before
`triangulate`. The key stays the **undisplaced** source plus the material pointer. The
displaced result is a pure function of exactly those inputs, so:
- prims sharing a mesh pay once;
- the instancing decision, which counts placements per key, is untouched.

Densities and `FaceMap` are then built from displaced points without further change. The
non-invertible bake path calls the same function before baking.

- **Alternative: at the end of `mesh_source`.** Rejected. It runs per prim before
  deduplication, so a cage placed directly 1 000 times would be displaced 1 000 times.
- **Alternative: in the kernel, at hit time.** That means displaced micro-triangles
  generated per ray, as in lazy tessellation. Rejected: it changes `crust-rt`, needs
  displacement bounds in the BVH, and goes against the pre-tessellating architecture
  the codebase chose (`compact-triangle-storage` design § Deferred).

### 2. `Displacement` lives beside the material, not inside it

Material resolution returns an optional `Displacement` with the `Arc<dyn Material>`, in
the same `(epoch, path)` cache. It is a closed crust-core enum:

```text
Displacement {
  value: Constant(f32)
       | Uv  { tex: Arc<dyn Texture2D>, channel, scale, bias }   // UsdUVTexture
       | Ptex{ tex: Arc<dyn PtexTexture>, scale }                 // PxrDisplace via Ptex
       | Mtlx{ program: one-root Program (+ JIT), scale }         // displacementshader
  bound: Option<f32>      // |constant|, else crust:displacementBound
}
fn eval(&self, VertexCtx { uv, ptex: Option<(u32, [f32;2])>, position, normal, width }) -> f32
```

- **Alternative: a `Material::displacement()` method.** Rejected. `Material` is the
  shading contract: every implementation, `resolve`, and the
  `crust-core/tests/resolve.rs` pins are about per-hit shading. Displacement is consumed
  once at import, and a `dyn` hop per vertex buys nothing.
- **Why `Uv` is its own variant** rather than reusing `UvInput`: `UvInput` carries
  fallback and wrap state for shading. The vertex sampler shares its scale/bias/channel
  code through a `HitRecord`-free entry point, so the two cannot drift.

### 3. One sample per unique vertex, taken from its owner corner

Every unique vertex gets its chart coordinates from one owner, the first face corner
that references it in face order.
- **Per-face tessellator:** the owner is the face that first emits the point, the rule
  `VertexKey` deduplication already applies. When the mesh is displaced, the `emit`
  closure records `(ptex_face, uv)` and the chart UV per new vertex in a side table.
  Nothing is recorded otherwise, so undisplaced memory is unchanged.
- **Uniform and level-0 meshes:** one pass over the face-vertex lists fills the same
  table from each vertex's first corner. The source is `UvSource.indices` for
  face-varying UVs, and `SubdivFaces` / `SubFace` corners for Ptex.

Since positions are shared and each is displaced exactly once, the displaced mesh has
the undisplaced mesh's connectivity. It is therefore watertight exactly where the
undisplaced one is, across UV seams and Ptex face boundaries alike.

- **Alternative: average the samples of all incident corners.** Rejected. It needs
  incidence lists, costs one sample per corner instead of one per vertex, and at a seam
  where the map is discontinuous it produces a value neither side authored. The owner
  rule's artefact is limited to one ring of triangles at a seam whose map is itself
  discontinuous, which is an authoring error the map's own filtering also shows.

### 4. Direction: the pre-displacement smooth normal; units: local

The offset is `d · n̂`, where `n̂` is the unit normal the tessellation path already
produces: limit normal, refined smooth normal, or smooth cage normal. It is applied to
local-space points before the placement transform. That matches UsdPreviewSurface,
MaterialX (object space) and RenderMan's default (`displacementbound` coordinate system
`object`).

- **Faceted sources have no normal**, so the pass computes area-weighted smooth normals
  over the undisplaced tessellation just for the direction. Faceted sources are a
  bilinear-diced `none` mesh and any mesh under `CRUST_SUBDIV=0`.
- **Alternative: the face normal per face.** Rejected. Vertices on an edge between two
  faces would move in two directions and crack.

### 5. Footprint: the vertex's chart spacing

`width` is the longer of the two chart-space edges meeting at the owner corner:
- UV units for `Uv` and `Mtlx` (passed as `uv_width`);
- face-local units for `Ptex`.

A cage-resolution mesh therefore reads a coarse mip level, a low-pass of the map at the
dicing rate, rather than aliasing its finest texels. At high rates `width` falls below a
texel and the lookup reaches the finest level.

- **Alternative: point sampling (`width = 0`).** Rejected. Displacement at a coarse rate
  would then be a random subsample of the map, which flickers between dicing rates and
  frames.

### 6. Recompute normals from the displaced triangles

After displacement, `normals` is replaced by area-weighted `smooth_normals` over the
displaced triangles, using the existing function. The exception is `CRUST_SUBDIV=0`,
where the source stays faceted and only its positions move.

- **Alternative: analytic normals** from `∂P/∂u + ∂d/∂u · n + d · ∂n/∂u`. Rejected. It
  needs map derivatives at the dicing scale, which aliasing makes unreliable, and patch
  normal derivatives the per-face path does not evaluate.
- **Alternative: keeping limit normals.** Rejected, because they shade the undisplaced
  surface.

Recomputing loses the limit-normal precision that undisplaced per-face meshes have. Only
displaced meshes take this path.

### 7. Displaced `none` meshes dice bilinearly

A `none` mesh with a `Displacement` enters `mesh_source` as if its scheme were
`bilinear`. The uniform level, the per-mesh adaptive level and per-face tessellation all
already accept bilinear meshes. Its faces stay flat, so the shape before displacement is
the cage.

Hard edges soften: smooth normals across a cage edge that was faceted. This is recorded
as a known gap rather than solved here; preserving sharpness would need creases
synthesised on every edge.

- **Alternative: displace only the cage vertices.** Rejected. A plane authored as one
  quad, which is the common case for a displaced ground, would never show its map.

### 8. Adaptive dicing pads by the displacement bound

`ScreenRate` gains a per-mesh pad: `bound · max_axis_scale(placement)`.
- The per-mesh path grows the world `Aabb` by the pad before computing the
  nearest-point distance and the frustum test.
- `segment_at` grows each segment's box by it before the frustum test.
- A displaced mesh with no bound sets the frustum term off for that mesh only, and is
  counted.

After displacement, the largest sampled `|d|` is compared with the bound, and one
warning per mesh fires if it is exceeded. The offset is never clamped: geometry is
pre-tessellated, so the BVH bounds are exact either way. An undersized bound can only
make out-of-view edges coarser.

- **Alternative: derive the bound from the map's value range.** Rejected for now. The
  bound would come from a scale and bias over `[0, 1]` for integer formats, or from a
  scan of float maps. That needs a new texture-trait method, `crust-mtlx` included, and
  cannot bound a MaterialX graph anyway. An authored bound is the production convention
  (RenderMan `displacementbound`, Arnold `disp_padding`).
- **Alternative: skip the frustum term for every displaced mesh.** Rejected. It costs
  the island's out-of-view memory saving wholesale.

### 9. Ptex requests carry a `ColorSpace`

`AssetLoader::load_ptex(path, space)`:
- `material_ptex` passes `ColorSpace::Gamma22`, today's decode stated explicitly, so
  colour renders stay bit-identical.
- Displacement passes `Raw`.
- `crust-assets` applies the curve per request on both the preloaded and the streamed
  path.
- The import cache key becomes `(resolved path, space)`, so a file read both ways is
  opened twice, which is correct and rare.

- **Alternative: a separate `load_ptex_raw`.** Rejected. It splits a pair (preloaded ↔
  streamed) into four paths, and the `ColorSpace` vocabulary already exists for UV
  textures.

### 10. MaterialX: a second root, compiled like `Presence`

`crust-mtlx` follows `surfacematerial.displacementshader` to a `displacement` node:
- a `float` `displacement` input compiles to an extra root, with `scale` folded as a
  constant when it is one, or a second root otherwise;
- a `vector3` input is refused and reported.

crust-core builds the one-root program with `Program::optimize`, and JIT-compiles it
behind the existing `jit` feature. The interpreter ↔ JIT bit-identity pin extends to it.

The program runs with these `ShadeCtx` fields:
- `uv` and `uv_width` from the owner corner;
- `position` and `normal` in local space;
- `tangent` zero and `view = normal`.

So `position`- and `normal`-driven graphs work, and view-dependent nodes have no
meaningful value. That is documented.

### 11. RenderMan / Moana through the Material interface

The island authors its Ptex paths as Material-prim interface inputs (`inputs:surfaceMap`
today, `inputs:displacementMap` for displacement), and its shader inputs are
`.connect`ed to them. Displacement is therefore read the way `PxrDisneyBsdf` is:
- detect a `PxrDisplace` child shader;
- read `dispAmount` from it if authored as a value, else follow its connection to the
  interface input;
- read the Ptex file from `inputs:displacementMap`, or from the file of the
  `PxrPtexture` / `PxrTexture` that `dispScalar` connects to.

The exact island attribute names are confirmed with an openusd probe as task 5.1.
Spellings may vary; the approach does not.

### 12. Deterministic, parallel within a mesh

The pass reads no USD: samplers and programs are already resolved. So it may run over
vertices with rayon in fixed chunks without touching the thread-local time. The output
is per vertex and independent, so it is bit-identical for any thread count. Owner
assignment is sequential and deterministic.

### 13. `CRUST_DISPLACE` and stats

`Config::displace` (default `true`). When off, material resolution returns no
`Displacement`. Every other step keys off its presence, so the off side is the old code
path, not an approximation of it.

`--stats` gains a `DisplacementCounters` block: meshes, vertices, time, max `|d|`, at
cage resolution, and frustum skipped. Time is measured within "Traverse prims", and
the block is printed only when the mesh count is nonzero.

## Risks / Trade-offs

- **[Memory on the island]** Displacement only shows detail with refinement, and users
  will raise rates. → Rates stay governed by the existing ceiling and edge-length
  target. Measure the island at its documented 2 px / ceiling-3 setting with
  displacement on, against the external 56 GiB guard, before archiving.
- **[Streaming Ptex at import]** Displacement touches every face of a displacement Ptex
  during import, which churns a streamed cache. → Displacement maps go through whatever
  residency `CRUST_PTEX_STREAM` selects. Preload remains the default, and the stats
  report already names cache behaviour.
- **[Rates ignore displacement]** Strongly displaced regions are diced at the cage's
  rate. → This is a stated non-goal. `--subdiv-edge-length` can be lowered, and
  `--stats` reports max `|d|` so the need is visible.
- **[Seam step]** A map that is discontinuous across a UV seam shows a one-ring step at
  the seam (decision 3). → This is inherent to the authoring. It is documented.
- **[Hard edges on displaced `none` meshes soften]** → Recorded as a known gap in the
  `usd-scene-import` design record and the user-docs limitations.
- **[Unbounded meshes lose frustum culling]** → Counted in `--stats`, and the remedy,
  `crust:displacementBound`, is documented beside the attribute.
- **[Owner order coupling]** The displaced result depends on face order, as
  deduplication already does. → Face order is the authored order, so this is
  deterministic, and the streamed ↔ unstreamed test pins it.
- **[Island names unverified]** → Task 5.1 confirms them on the island before the
  RenderMan path is written. Absent names leave the island undisplaced, as today.

## Migration Plan

- **Default on.** A scene without displacement inputs takes no new code path and stays
  bit-identical (`check_images.sh check` over every sample).
- Scenes that author displacement change shape. That is the point of the change, and
  it is called out in the user docs.
- **Rollback:** `CRUST_DISPLACE=0`.

## Open Questions

- **Should `crust:displacementBound` also be read from RenderMan's
  `primvars:ri:attributes:displacementbound:sphere`** where the island authors it? It
  is an additive attribute read and can follow once the probe in task 5.1 shows whether
  the island authors it.
