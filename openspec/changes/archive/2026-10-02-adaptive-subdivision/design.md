## Context

Subdivision today (`openspec/specs/usd-scene-import/design.md`, "Subdivision surfaces";
`scene/subdiv.rs`):

- **The level.** One level per load is resolved in `load_scene` before traversal:
  `--subdiv-level`, then `crust:subdivisionLevel` on the index stage, then 0. It is
  stored on `MeshArena` as `SubdivPolicy`.
- **Where refinement happens.** In `mesh_source`, *before* interning, so `MeshKey`
  dedupes on the refined arrays. Every path (direct bake, deferred instance-vs-bake,
  prototype parts) sees it exactly once. Level 0 is the smooth cage.
- **Prototype parts** (`instancing.rs`) are built on first use and cached per
  prototype path, scoped by the stage epoch (`ImportCaches::epoch`). They are grouped
  per prototype into one scene, placed by one instance per placement.
  `PointInstancer` placements are read as arrays before their prototypes are built.
  Native instances arrive one by one across streamed chunks.
- **The camera** is chosen before traversal (`--camera`, else
  `RenderSettings.camera`), but only *built* when traversal meets it. Without either,
  the first camera met is used.
- **The index stage** is opened with payloads unloaded. A camera inside a payload
  cannot be read from it.

Constraints:
- **Determinism.** The import is single-threaded and streamed by chunk, and a mesh's
  outcome must not depend on chunk order (the same rule as the global bake decision).
- **Every prototype-path key is epoch-scoped.**
- **Logging.** Anything per mesh is DEBUG.

## Goals / Non-Goals

**Goals:**
- One level per *placement* of a subdivision mesh, chosen from its on-screen size
  against a pixel target, capped by the existing level setting.
- Instancing preserved: a prototype is refined at most once per distinct rate bucket,
  not once per placement.
- No change at all when no target is given.

**Non-Goals:**
- Varying the level across one mesh: per-face refinement, feature-adaptive patches,
  crack-free transitions.
- Coarsening off-screen geometry (frustum or visibility-aware dicing).
- Changing level selection in uniform mode.

## Decisions

### 1. The metric: projected mean cage-edge length at the nearest point

The target `t` is in pixels, from `--subdiv-edge-length`, else
`crust:subdivisionEdgeLength`. For a placement with world transform `M` of a cage whose
mean edge length in local space is `ē`, the projected length is `p = ē · s · ρ` and the
level is:

```text
s = largest column norm of M's linear part       (an upper bound on the stretch)
d = distance from the camera position to the placement's world AABB (0 if inside)
ρ = pixels per world unit at distance d          = f_px / max(d, ε)
    f_px = image_height_px / (2 · tan(vfov / 2))
    (for an orthographic camera: image_height_px / vertical aperture, independent of d)
L = clamp(ceil(log2(p / t)), 0, max)
```

`max` is the resolved uniform level if `--subdiv-level` or `crust:subdivisionLevel` was
given, else 3. In adaptive mode the level setting is a ceiling, and at 3 a cage already
costs 64×.

Each choice is made conservatively, toward more detail:
- the *mean* cage edge, computed once per cage from its authored topology;
- `ceil`;
- the nearest point of the bounds;
- the largest column norm.

- **Alternative: the maximum edge length.** Rejected. A single long seam edge would
  refine the whole mesh, and the mean is what dicing rates are usually tied to.
- **Alternative: frustum-aware coarsening.** Deferred. In a path tracer, off-screen
  geometry still reflects, refracts and shadows. Distance alone is the safe first step,
  and the histogram in `--stats` shows how much a frustum term would save.

### 2. The camera is resolved before traversal

`load_scene` already knows the camera *path* up front. Adaptive mode also needs its
transform, projection and the resolution at shutter open:
- Build it from the index stage when the prim is composed there.
- Otherwise, open a stage population-masked to the camera path with payloads loaded,
  the same mechanism the streamed import uses per chunk, and build it there.
- The traversal still builds the render camera exactly as today. The two must agree,
  and a debug assertion compares them.
- With no camera path known up front, warn once and use the uniform level. Scanning for
  the first camera would mean a second traversal of the stage.

### 3. Per-placement levels for directly placed meshes

`mesh_source` receives the prim's world transform, which it already has for baking, and
computes `L` before refinement. A cage referenced by several prims is refined once per
distinct `L`. `MeshKey` already hashes the *refined* arrays, so equal levels still
dedupe and unequal levels cannot collide. The cage's `ē` is memoised per cage content
hash for the load, so a cage met many times measures once.

### 4. Prototypes are cached per rate bucket

A prototype's meshes all see the same placement, so what varies between placements is
`σ = s · ρ`, the world-to-pixel scale. Its bucket is `q = ceil(log2(σ))`, rounded up,
so a placement never gets less detail than its exact rate asks for. Inside the
prototype, each mesh's level is computed from `ē_mesh` with `σ = 2^q`, its own local
transform included.

- **The key.** The prototype-parts cache becomes `(prototype path, epoch, q)`. In
  uniform mode `q` is a constant, which is exactly today's behaviour.
- **Prototypes with no subdivision mesh.** A prototype whose parts contain no
  subdivision mesh is marked *rate-independent* the first time it is built, and every
  later `q` aliases that build. So only prototypes that actually refine can multiply.
- **PointInstancer.** Placements are grouped by `q` before parts are built. Each group
  gets the version for its `q`, and its own instances keep their ids.
- **Native instances.** Each instance computes its own `q` and asks the cache, whatever
  the chunk order.
- **Nested instancers inside a prototype.** They inherit the *outer* placement's `d`,
  as the nearest point of the outer bounds is no farther than any inner part, and
  compose the scales. This is conservative, and needs no information the prototype
  build does not already have.
- **Labels and ids.** Each `q` version is an independent group scene, so ids and
  `InstanceHitId` labels work unchanged. Materials, Ptex face tables and UV tables
  attach per part, as today.
- **Bounding the copies.**
  - `q` versions collapse whenever their per-mesh levels coincide. The per-mesh `L` is
    clamped to `0..=max`, so at most `max + 1` distinct level sets exist per mesh.
  - The cache canonicalises `q` to the per-prototype range where any level changes
    (`q_lo..q_hi`, computed from the prototype's `ē` extremes at first build), so
    out-of-range buckets alias.

### 5. Reporting

- `--stats` gains `subdivision levels  L0 n · L1 n · …`, counted per placement, and
  `prototype versions  N (rate-dependent: M)`.
- A DEBUG line per refined mesh prints its chosen level, `ē`, `d` and `p`, so a
  surprising level can be traced.
- INFO stays bounded: one line saying adaptive mode is on, with its target and maximum.

## Risks / Trade-offs

- **Memory can go up**: a widely scattered prototype might now exist at four levels.
  → Bucket aliasing (decision 4) and `MeshKey` sharing bound it. `--stats` reports the
  versions, and the island measurement in the tasks decides whether a per-prototype cap
  is needed.
- **Silhouette pops between neighbouring placements at different levels.** A single
  image does not animate, but a sequence can show an instance changing level as the
  camera moves.
  → Documented as a gap. Adaptive mode is opt-in, and per-frame imports are the
  existing model.
- **Cracks between meshes at different levels.** Two separate meshes meeting at a
  shared boundary can refine differently.
  → Identical to authoring two separately refined meshes today. Within one mesh the
  level is uniform, so no cracks.
- **Camera under a payload** costs one extra masked stage open.
  → It is a single prim's population, and is measured on the island, where `shotCam`
  is the case to check.
- **Mean edge length on a degenerate cage** (zero-length edges, a single face).
  → `ē` floors at ε, and a malformed cage already degrades to the cage with a warning.

## Migration Plan

Opt-in: no target edge length means no behaviour change. Rollback is a revert.

## Open Questions

- **The default for `max` in adaptive mode (3).** It is a round number. The island
  histogram from the tasks may argue for 2, or for 4 on hero assets. Changing it later
  does not change the design.
