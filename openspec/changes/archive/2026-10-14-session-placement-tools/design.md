## Context

See `proposal.md` for why, and `specs/mcp-session/spec.md` for the behaviour. What the
code has today, and lacks, for placement:

- **Ray queries exist.** `World::intersect` (`rt_world.rs`) is what `probe` casts with.
  The kernel's `crust_rt::Scene::intersect` returns a `RayHit` with the geometric normal
  (turned against the ray), `geom_id` and `prim_id`. Neither can skip given geometry ids
  per ray: the mask bits are fixed at attach, and bits 3–31 already hold light-link
  shadow classes.
- **Geometry can only be read back one triangle at a time.**
  `crust_rt::Scene::triangle_vertices(geom_id, prim_id)` returns a triangle in its own
  scene's space. Recovering world space (`World::world_vertices`, `VertexSource`) is
  private and gives up on PointInstancer slots and motion-blurred instances. No bounds
  exist per geometry, only for the whole scene.
- **Provenance maps geometry to prim, not back.** `Provenance::geometry` is
  `(first geom_id, prim path)` runs sorted by id, and `prim_of_geometry` uses
  `partition_point`. A native instance or a PointInstancer has one run, named by the
  instance or instancer prim. Only prims that emitted geometry have a run.
- **Transforms compose only inside the import.** `compose_with_parent` and
  `local_to_world` (`usd_import/xform.rs`, `camera.rs`) are `pub(super)`, and fold
  openusd's `XformQuery` (suffixed ops, `!invert!`, `!resetXformStack!`).
- **The authoring stage leaves payloads unloaded.** Prims an asset's payload brings, and
  the opinions the payload authors on the asset's root (often its `xformOpOrder`), do not
  compose there. `Provenance::rebuild` already opens a stage masked to one prim with
  payloads loaded (`usd_import/live.rs`, `masked`).
- **There is no area-weighted triangle sampler.** crust has no mesh lights
  (`MeshLightAPI` is unsupported). The one CDF inversion is `Distribution1D`, private to
  `environment.rs`: f64 accumulation, f32 CDF.
- **openqmc can be keyed outside a render.** `light_cache.rs` builds
  `PathSampler::new(i, j, frame, 0).new_domain(K_TRAIN)`.
  `crust_core::PathSampler` is the Sobol sampler, whatever `crust:sampler` says.
- **PointInstancer import** (`instancing.rs`): it reads `prototypes`, `protoIndices`,
  `positions`, `scales`, `orientationsf` before `orientations`, `ids` and
  `invisibleIds`. It skips a prototype root's own transform (`part_local`) and never
  draws a prim under an instancer on its own.
- **Edits are merges.** `edit.rs` merges USDA snippets field by field and cannot remove a
  spec. `Open::edit` (`session.rs`) is the one edit path: refresh, stop renders, author,
  save, `sync`.

## Goals / Non-Goals

**Goals:**
- Place against the exact triangles that render, through instances and PointInstancers,
  with nothing kept per triangle between calls.
- Author placement a USD reader understands without crust: standard xformOps and a
  standard `PointInstancer`.
- Leave the render path untouched. The only shared code that moves is
  `Distribution1D`, and it moves bit for bit.

**Non-Goals:**
- Physical settling: no contact solving, toppling, or collisions between the prims of
  one call.
- Aligning scattered instances to the surface normal. Instances stand upright about the
  stage's up axis. This is a candidate follow-up, not part of the proposal.
- Re-scattering in place. A scatter defines a new prim; to change it, `undo` and scatter
  again. The merge cannot remove the previous prototypes' specs.
- Anything the proposal lists as out of scope.

## Decisions

### D1. Where the code lives

- **`crust-rt`: one world-space geometry visitor.** The walk through instances happens
  once, in the crate that owns the layout. `Scene::visit(ids, mask, f)` hands `f` every
  piece of the top-level geometries attached as `ids` that a ray of `mask` sees:
  - a triangle's vertices in winding order, or a `Piece::Bounds` for any other
    primitive (sphere, disk, cylinder, curve segment or span);
  - in world space at shutter open: an instance's `transform`, not `transform_end`,
    recursing through nested instances;
  - tagged with the attach id of the top-level geometry each piece belongs to.

  Instances are matched on their attach id (`placement - 1`), not on a label. An id that
  is a reserved slot of an `InstanceHitId::Offset` names no geometry, so it gives
  nothing. A prim's bounds are the union of its pieces. crust-rt stays free of crust
  types.
- **`crust-core::place` (new, public):** the computation. `drop_prims` and `scatter`
  take a `&World`, the `&Provenance` and plain inputs, and return numbers: per prim, a
  world-space delta and a status; per instance, a position, angle, scale and prototype
  index. They read no USD and author nothing.
- **`crust-core::live`:** `Provenance::geometry_of(path)` returns the id ranges of every
  run whose prim is `path` or a descendant of it. It scans the runs (O(runs) per call,
  no new map) and records each run's end beside its start, since today a run
  implicitly ends where the next one begins and an unattributed gap would be absorbed.
  `prim_frames(layer, paths)` opens the stage masked to the paths with payloads loaded,
  at the scene's time code, and returns per prim:
  - its parent-to-world matrix in f64;
  - its composed `xformOpOrder` and whether it resets the stack;
  - whether it is `Xformable`.

  It folds the import's own `compose_with_parent` (the "rebuilds go through the
  import's own code" rule), never a copy.
- **`crust-render::mcp`:** the tools, their argument validation, and the snippets
  (`edit.rs`: `placement_snippet`, `instancer_snippet`). They go through `Open::edit`,
  so saving, the undo step, `sync` and the result shape are an edit's.

*Alternative rejected:* computing bounds and transforms on the authoring stage from USD
`extent` / `points`. It misses everything behind a payload, and it would not be the
subdivided, displaced geometry that renders.

### D2. Dropping: rays from the prim's own vertices

- **The prim's geometry.** The geometry of `path` is `geometry_of(path)` over the
  current import. A prim with no range is reported `no geometry`.
- **The rays.** One ray per vertex of the prim's triangles goes down from the height of
  the prim's top (the highest of its vertices along up), straight below the vertex. A
  non-mesh geometry (curves, implicit shapes) contributes a 9 × 9 grid over the bottom
  face of its bounds instead.
  - Each ray takes the first hit whose `geom_id` is not the prim's own and, with
    `target`, is one of the target's. Other hits are skipped by restarting the ray at
    them, never by raising `t_min`: every intersection asks for `(TRACE_T_MIN, ∞)`, and
    LLVM folds those constants into the kernel. The kernel has no per-ray id filter, and
    adding one would touch the traversal's hot loop.
  - Hits count only on camera-visible geometry (`RayMask::CAMERA`).
- **The drop.** Each column gives `d_i`, the vertex's height above its hit; the move is
  `min_i d_i` along down. A negative minimum raises the prim. The vertex that sets it
  lands exactly on its surface, and no other vertex can end up below its own surface.
  This is the spec's "rests on" for vertex-to-face contact.
  - *Gap:* a sharp peak of the surface between two of the prim's vertices can poke
    through a face. Denser geometry narrows it.
  - Columns that hit nothing are ignored. All of them missing is `nothing below`.
- **Why vertices, not a footprint grid:** a grid over the bounding box rests the box's
  bottom on the highest hit under the box. On uneven ground that floats a rock whose
  lowest point is not above the highest ground. Rays from the vertices touch.
- **Cost:** rays scale with the prim's vertex count, cast with rayon. The reduction is a
  `min`, so the result does not depend on thread order. Nothing is kept after the call.

### D3. Aligning

1. The pivot is the centre of the bottom face of the prim's bounds.
2. One ray goes down from the top of the bounds above that centre. Its hit's geometric
   normal `n` faces up, since the kernel turns it against the ray.
3. The rotation is the shortest arc from up to `n` about the pivot.
4. D2 then runs on the rotated vertices along the stage's down axis, not along `-n`, so
   the prim still rests.

A miss under the centre is reported `nothing below`.

### D4. Authoring the move: one matrix op, folded

- **The math.** The world-space move Δ (a translation, or a rotation about the pivot
  then a translation) becomes one op on the prim. With `P` the parent-to-world matrix
  (identity after `!resetXformStack!`) and `C` that op's previous value (identity when
  there is none):

  `C' = P⁻¹ · Δ · P · C`

  This makes `World' = Δ · World` exactly, whatever shear or scale `P` carries. The
  arithmetic is f64 throughout, since `matrix4d` is what USD stores.
- **The op.** `xformOp:transform:crustPlace` is authored first in `xformOpOrder`, after
  a leading `!resetXformStack!`, followed by the stage's composed ops in order. The
  composed order is read on the payload-loaded masked stage (D1), so ops a payload
  authors are kept.
  - A prim already led by the op folds the move into it, so repeats leave one op.
  - The drop is pure translation when there is no alignment.
- **Why one matrix:** a `translate` op conjugated by a parent with rotation and scale is
  still a translation, but an aligned move is not a rotation in parent space when `P`
  has non-uniform scale. One `matrix4d` covers both, and folding a repeat is a
  multiplication.
  - *Rejected:* `translate` + `orient` + pivot ops, which give three specs to keep in
    step and cannot express the sheared case.
- **Validation.** A prim that is not `Xformable` (including a typeless prim, whose ops
  the import ignores), or that is an instance proxy (`editable_prim`), refuses the whole
  call before anything is authored.
- *Risk (accepted, documented):* authoring `xformOpOrder` in the override layer shadows
  a later change to that list in a source. See Risks.

### D5. Scattering: a CDF over triangle areas, keyed by the seed

- **Collecting triangles.** The target's eligible triangles are gathered in world space
  through `visit` over `geometry_of(target)`, in id order and stored order, so
  the list is deterministic.
  - `max_slope` filters on the triangle's geometric normal, from its winding as it
    renders, against up.
  - Geometry with no triangles is skipped and named in the result.
- **`Distribution1D`** moves from `environment.rs` to a shared `crust-core` sampling
  module unchanged, and the environment map uses it from there.
  - Its f64 accumulation, f32 CDF is as fine as the f32 numbers the sampler draws. A
    triangle below ~1e-7 of the total area can be over- or under-drawn. That is
    accepted for placement, and noted as a gap.
  - *Rejected:* a second CDF type, which the proposal's "shared rather than copied"
    rules out.
- **Instance `i`** draws from `PathSampler::new(0, 0, seed, i)`:
  - domain `K_SCATTER`, 4D: the triangle (CDF inversion), two barycentric numbers
    (uniform triangle warp, added to `utils` beside the other warps), and the prototype;
  - domain `K_SCATTER_SHAPE`, 2D: the angle about up and the scale in `[min, max]`.

  Each instance is a function of `(seed, i)` and the triangle list alone, so the result
  does not depend on threads or on the render's `crust:sampler`. Sobol across `i`
  stratifies the area draw. The count is `count`, or `round(density × eligible area)`.
- **Local values.** Instance `i`'s world matrix is `T(p)·R_up(θ)·S(s)`. Its local value
  is `P⁻¹` times that matrix, decomposed into the position, the orientation (`quath`,
  the attribute every USD reader knows) and the scale (`float3`). A parent whose shear
  leaves a residual above 1e-5 after decomposition refuses the call, since the arrays
  cannot express it. Instances stand upright in the world whatever the parent's
  rotation.
- **The instancer snippet.**
  - It is a `def PointInstancer`, with children
    `Prototypes/<name>` (from the source prim's name or the file's stem, deduplicated
    with a suffix).
  - Each child is a **typeless** `def` with `references` to the prim (an internal
    reference) or the asset (path re-anchored to the override layer), and
    `uniform token[] xformOpOrder = []`. *Trap:* a typed `def Xform` would win over the
    referenced `Mesh`'s type and turn the prototype into an empty Xform.
  - The empty op order drops the source's own placement for any reader. crust skips the
    prototype root's transform already, and a reader that applies it gets identity.
  - Prototypes under the instancer are drawn only through it. The prim a stage prototype
    references still renders where it is.
- **Size.** At most 100 000 instances: about 8 MB of USDA through one parse and merge,
  which keeps a `full` re-import's parse in proportion. A scatter defines a new prim, so
  it is always `full`.

### D6. Freshness and the edit batch

Both tools run, on the session thread, as:

1. `refresh()`, then `bring_current()`, so a changed source is imported before
   placement reads the world.
2. Compute.
3. `edit()` with the snippet. That re-runs `refresh` (a no-op now), stops renders, saves,
   `sync`s (classified `full`), and pushes the undo step.

When a source changed, this costs one import before computing and one after the edit.
That is the price of never placing against a stale scene. A call that places nothing
returns its report without calling `edit`.

## Risks / Trade-offs

- **The override's `xformOpOrder` shadows later source changes to it.** A placed prim
  whose asset later adds an op keeps the list from placement time. → Documented in the
  design record and the user page. `query` shows the authored list, and `undo` or
  re-dropping after a `reload` re-reads it.
- **Placement is valid at one frame.** An animated parent makes `P⁻¹ΔP` differ per frame
  for an aligned move. Motion-blurred geometry is placed at shutter open. → Documented
  gap. The op is authored as a default, not a time sample.
- **Geometry follows the import's settings.** A later subdivision-level or displacement
  change moves surfaces, and the placed prims do not follow. → Documented.
- **Opacity cutouts are ignored.** Rays hit the geometry, as `probe`'s do, so a
  cut-out leaf counts as a surface. → Documented gap.
- **Two imports when a source changed** (D6), about 112 s each on ALab. → Accepted:
  placing against a stale scene is wrong, and an unchanged source costs only the stats.
- **Ray count scales with the prim's vertices.** A ten-million-vertex prim casts ten
  million rays, a few seconds across the pool. → Reported in `import_s`'s sibling
  `place_s`, and measured on the largest checked-in mesh.
- **Render path.** Only `Distribution1D`'s module changes. → `scripts/check_images.sh`
  goldens recorded before the change must match bitwise, and callgrind on the Cornell
  box must show no render function's count moving.
