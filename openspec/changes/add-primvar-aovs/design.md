## Context

See proposal.md for the motivation and specs/ for the behaviour. The current
state that shapes the approach:

- **The request is known before any mesh loads, but does not reach the
  import.**
  - `read_index` resolves the products on the index stage
    (`usd_import/mod.rs`, `import_render_products`) before `traverse_stage`.
  - The request is only attached to the scene at the end (`scene.aovs = …`).
  - `IndexRead.products` never reaches `ImportCaches`.
- **What a mesh reads is decided by its material alone.**
  - `MeshNeeds` (`mesh.rs`) carries the faces, the chart, the chart's
    primvar name and the displacement.
  - It is built in `emit_mesh`, `place_mesh_source` and for prototypes
    (`instancing.rs`).
  - Identical meshes share one `MeshSlot` through `MeshKey`. The key hashes
    the points, the topology, the chart and the material.
- **The chart is the only per-corner table.**
  - `UvMap` (`rt_world.rs`) is 2 lanes: values plus a `[u32; 3]` of
    corner indices per triangle.
  - It is built in `triangulate` and shared by `Arc` with every placement
    and instance.
  - `UvSource` recognises only `faceVarying`. It indexes every other
    interpolation per point, so a `uniform` or `constant` chart reads wrong
    values today. That latent bug is noted under Open Questions; this change
    does not reuse `UvSource` for primvars.
- **Subdivision refines points and the chart only.**
  - Uniform refinement (`subdiv/uniform.rs`) uses `PrimvarRefiner`.
    Points use `interpolate` + `limit` on `[f32; 3]`. A vertex chart gets the
    same calls on `[f32; 2]`. A face-varying chart is FVar channel 0, under
    the mesh's one linear-interpolation rule.
  - Adaptive tessellation (`subdiv/adaptive.rs`) evaluates patches from
    control values, and stitches cage faces with an inline lerp.
  - Every crust-side type there is `[f32; 2]`: `UvChannel`, `RefinedUvs`,
    `TessellatedMesh`, `Emitted`.
- **Displacement moves points only.** The final triangles always come from
  `triangulate` in `MeshArena::intern`, after `displace`. A table built
  there keeps the undisplaced connectivity, so it reads the rest value for
  free.
- **A hit does not keep its barycentrics.**
  - `World::intersect` resolves the chart inside the call. It does so only
    on geometries that are not `plain`, which is the one-byte fast path most
    hits take.
  - `WorldHit` has `prim_id` but not the kernel's `(u, v)`. `HitRecord` has
    neither.
  - `first_wall` stores `(t, rec, geom_id)`, without `prim_id`.
- **The film is ready for a payload.**
  - Light path expressions already give a slot a payload: `SlotKey.lpe: u16`
    indexes a layout list, fed per sample through `SampleExtras.lpe`.
  - Planes and channel output are generic over `components()`.
  - `sample_value` / `UnitAov::add` are 3 lanes at most.
- **Curves are mostly done.** `StrandTable` decodes `constant`, `uniform`,
  `vertex` and `varying` primvars into `crust_mtlx::Val` (4 lanes plus an
  arity), driven by a `StrandRequest` built from the material.

## Goals / Non-Goals

**Goals:**

- A render with no primvar var takes exactly today's code path. The
  zero-AOV instruction count and the `plain` fast path in `intersect` stay
  untouched. The import is the one exception: a subdivided mesh's chart is
  refined at four lanes and narrowed back (`aovs` design record, Known gaps).
- One N-lane primvar representation, shared by meshes (all their paths:
  plain, uniform subdivision, adaptive, displaced, instanced) and curves.

**Non-Goals:**

- Primvars for materials on meshes (`geompropvalue` beyond `st`). The table
  built here could serve that later; this change keeps it AOV-only, so it
  never enters `Signature`.
- Fixing `UvSource`'s handling of `uniform` and `constant` charts.

## Decisions

### D1. A primvar keep set, threaded from the products into the import

`import_render_products` gives back, beside the request, a `PrimvarKeep`.
It is the deduplicated list of `(name, arity)` that primvar vars ask for,
with the `primvars:` prefix stripped. Each entry has a `u16` index, which is
also the var's slot payload (D5).

The keep set is moved into `ImportCaches`, and `MeshNeeds` gains a reference
to it. An empty keep set is the default, and every path then behaves as
today.

*Alternative:* re-reading the RenderVars inside the mesh import. Rejected:
it would resolve products twice, and the index stage is the one that has
them.

### D2. `PrimvarTable`: per geometry, beside `UvMap`, outside `plain`

The new type lives in `crust-core`:

```
PrimvarTable { channels: Vec<Option<PrimvarChannel>> }   // indexed by keep-set index
PrimvarChannel = Constant([f32; 4])
               | PerTriangle { values: Vec<[f32; 4]>, of: Vec<u32> }       // uniform
               | Corners     { values: Vec<[f32; 4]>, corners: Vec<[u32; 3]> } // vertex / varying / faceVarying
```

- **Lane format.** Values are four lanes, `[f32; 4]` (the arity is the
  keep-set entry's, so `crust_mtlx::Val`'s arity byte would only repeat it);
  curves' `StrandTable` channels are read out as the same lanes. The memory
  cost of four lanes is accepted, because the table exists only when asked
  for.
- **Built in `triangulate`**, from a new N-lane `PrimvarSource`:
  - It reads interpolation, `:indices` and element size properly, for all
    five interpolations.
  - A uniform primvar maps each triangle to its polygon's value.
  - On a subdivided mesh, each triangle maps to its refined face's base
    face, which the refiner's parent-face map provides; the Ptex `FaceMap`
    path already relies on it.
- **Attached** with `World::set_primvars(id, Arc<PrimvarTable>, swapped)`,
  which does **not** clear `plain`. The table is never read by `intersect`;
  only by the AOV first hit (D4). A geometry with a primvar table therefore
  costs the beauty nothing per hit. *As built:* the tables sit in a sparse
  list sorted by `geom_id`, like the motion records, not in `SideTables`: a
  field there would cost every geometry 8 bytes in renders without a primvar
  var.
- **Deduplication.** `MeshKey` hashes the kept channels' values. Otherwise
  two prims that share points but differ in `rest` would share one slot,
  the failure `mesh.rs` documents for charts.
- **Curves.** The curve side builds no `PrimvarTable`. The `StrandRequest`
  gains the keep set, and its `StrandTable` channels serve the AOV through
  the same four lanes.

*Alternative:* resolve primvars inside `intersect`, as the chart is.
Rejected: it would clear `plain`, so every hit on every geometry carrying a
kept primvar would take the slow path, and every primvar would be
interpolated for every bounce, not just the camera's first hit.

### D3. Subdivision and adaptive tessellation carry N extra lanes

- **Uniform refinement.**
  - `SubdivRequest` gains a list of extra channels beside `uvs`.
  - Vertex and varying channels are refined with the points' calls:
    `interpolate` per level, then `limit`, at the channel's width. That
    keeps `rest` exactly on the limit surface `P` is snapped to, which is
    what the spec's "agree with `P`" scenario relies on.
  - Each face-varying channel is one more FVar channel. They all share the
    mesh's `faceVaryingLinearInterpolation`, which is a property of the
    mesh in USD, not of the primvar, so the one-rule-per-refiner limit
    matches the schema.
- **Adaptive tessellation.**
  - *As built:* extra face-varying channels get a refiner and patch table of
    their own. `consider_fvar_channels` isolates a face irregular in *any*
    face-varying channel, so adding one to the surface's refiner would move
    the beauty's geometry. Ptex face ids follow the cage alone, so both
    tables are evaluated at the same `(ptex, u, v)`. The chart stays the
    surface refiner's one channel, as before.
  - Control values are refined per channel.
  - Patches evaluate each channel the way they evaluate `uv_control`, and
    FVar channels by their channel index rather than a hard-coded 0.
  - `Emitted` grows parallel lane arrays, and the cage-face stitch lerps
    them.
- **One code path.** The 2-lane chart becomes one instance of the N-lane
  channel, so there is no second copy to keep in step.
- **Widths.** Values are carried as `[f32; 4]` rather than generic over
  width. The opensubdiv-rs refiner is called with fixed-size arrays; task
  1.1 confirms it accepts `[f32; 4]`, or falls back to width-specialised
  calls.

*Alternative:* refine only `vertex` primvars and refuse face-varying ones on
subdivided meshes. Rejected: extra UV sets are face-varying, and they are
half the issue.

### D4. The first hit resolves primvars from `(geom_id, prim_id, barycentrics)`

The AOV first hit records `geom_id`, `prim_id`, and — only when the layout
has primvar slots — the triangle's barycentrics.

- **`first_wall`.** Its tuple gains the hit's prim (`HitPrim`).
- **Meshes.** `World::primvars_at` looks up the geometry's table and the
  triangle's three corners, undoes the `swapped` corner order as
  `UvMap::resolve` does, and writes each kept channel into the scratch.
- **The barycentrics are the kernel's** (amended while implementing). The
  plan recomputed them from `p` and the triangle's vertices through
  `VertexSource`, but `VertexSource` is `Unresolved` for a part placed
  through a group (a forwarded id names a slot, not a placement) and for a
  moving instance: every multi-part prototype and every
  `crust:motion:translate` placement would have read the clear value.
  Instead `World::hit_barycentrics` traces the camera ray again, restarted
  just short of the hit, and keeps the kernel's `(u, v)` when it lands on
  the same `(geom_id, prim_id)`. The restart, not narrower bounds: every
  kernel query asks for `(TRACE_T_MIN, ∞)`, LLVM propagates those constants
  into the traversal, and the first version's `(t − d, t + d)` cost
  cornellbox's zero-AOV render +0.15% of its instructions.
- **Curves.** A curve hit on a geometry with a `StrandTable` already
  carries a `CurveHit` with its `(prim_id, u)`. The AOV path reads the kept
  channel from it. A curves prim with a table built only for the AOV loses
  `plain` (see Risks).
- **Volumes, escapes and non-table geometry** leave the scratch at the
  clear value.
- **Cost.** One extra kernel traversal per camera sample, from just short
  of the hit, and only in the AOV instantiation of a render with a primvar
  var. Adding `(u, v)` to `WorldHit` would widen a struct every hit copies,
  in the beauty's path too.

*Precision:* the restarted ray's barycentrics differ from the original's by
float rounding. That is invisible in a closest-mode `rest`. The spec's
"agree with `P`" scenario allows float rounding.

### D5. A primvar slot carries a keep-set index and a role

- `AovSource::Primvar` joins the vocabulary, and `SlotKey` gains
  `primvar: u16`, as it has `lpe`.
- The var carries the role its `dataType` implies: colour, vector-like
  (point, vector, normal), texture coordinate, scalar, or integer. The role
  picks two things:
  - **The default accumulation**, set by the importer, since
    `AovSource::default_accumulation` has no `dataType`: closest for
    vector-like and integer, filtered otherwise.
  - **The `ChannelKind`**, from the existing kinds, so a `point3f` var is
    named like `P` and a `color3f` like a colour.
- **Width.** `type_fits` takes the arity from the `dataType` for a primvar
  source. A four-component `dataType` is refused (the spec), because
  `sample_value` is 3 lanes. Widening it is a separate step.
- **Exposure.** `apply_exposure` leaves primvar slots alone: they are data,
  not radiance.
- **Integers** go through the film as `f32`, and are written as `UINT` like
  `sampleCount`. Closest accumulation keeps them exact below 2²⁴.

### D6. Inheritance is detected, not honoured

When a kept primvar is missing on a mesh or curves prim, the import walks
its ancestors once per `(name, ancestor)`, memoised. When an ancestor
authors it as `constant`, it raises one `PrimvarNotInherited` warning per
var. The same warning covers `primvars:<name>` on a `PointInstancer` and on
an instance prim, where the instancing import meets them. The walk only
runs for kept names, so a render without primvar vars never walks.

*Alternative:* honour constant inheritance now. Rejected for this change by
scope (slice B), but the memoised walk is exactly where it would go.

### D7. Live edits refuse a changed keep set

- **Settings refresh.** `StageFacts` records the keep set the geometry was
  imported with. The settings refresh in `usd_import/live.rs` compares it
  with the new request's keep set, and refuses on a difference, beside the
  subdivision, working-space and camera checks. A full re-import then
  rebuilds the tables, which keeps the "edit in place ↔ re-import" pair
  honest.
- **RenderVar and product edits** already fall through to a re-import.
- **Material swaps.** Since the tables are driven only by the request, they
  stay out of `Signature`.

### D8. Stats

`SceneCounters` gains `primvar_tables_bytes`, deduplicated by `Arc` like
`strand_tables_bytes` and printed beside it. That is what the spec's
zero-cost scenario reads.

## Risks / Trade-offs

- **[Risk] Memory on dense scenes:** a value is 16 bytes, and a `rest`
  corner table costs 12 B per triangle plus 16 B per value. → It is only paid when
  asked for, and it is reported by `--stats`. Packing values at their arity
  is a later optimisation, behind the same type.
- **[Risk] Generalising the adaptive tessellator can shift the chart's bits**
  if lane order or arithmetic changes. → The 2-lane chart through the
  N-lane path must stay bit-identical. `check_images.sh` on the subdivided
  samples, plus the adaptive tests, pin this before the primvar work lands.
- **[Risk] opensubdiv-rs may not accept `[f32; 4]`.** → Task 1.1 checks this
  first. The fallback is width-specialised calls for 1, 2 and 3 lanes.
- **[Trade-off] Re-tracing for barycentrics instead of carrying them** costs
  one traversal per camera sample in primvar renders, for zero cost to the
  beauty. This is acceptable for data AOVs.
- **[Trade-off] Curves with an AOV-only `StrandTable` take the non-`plain`
  path in `intersect`.** That is the path every curve whose material reads
  a curve property already takes. → Accepted: it applies only to renders
  with a primvar var, and `bench_ab.sh` on a groom measures it (task 5.4).
- **[Risk] The `MeshKey` gets more expensive to hash** when primvars are
  kept. → Only kept channels are hashed, and only then.

## Migration Plan

None: `primvar` vars are refused today, so nothing that renders now changes.
Archiving this change also removes task group 1, D4 and the "Primvar sources"
requirement from `add-identity-aovs-openexrid` (task 6.3).

## Open Questions

- **The latent `UvSource` bug.** A `uniform` or `constant` `primvars:st` is
  indexed per point today. It should be reported as its own issue; the
  `PrimvarSource` reader here could later replace `UvSource` for the chart
  too.
