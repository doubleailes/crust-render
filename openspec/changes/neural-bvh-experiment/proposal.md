## Why

Recent papers replace part of a ray tracer's BVH with a learned model:
- N-BVH (Weier et al., SIGGRAPH 2024);
- AMD's Neural Intersection Function (HPG 2023) and LSNIF (I3D 2025);
- Neural Bounding (Liu et al., SIGGRAPH 2024).

All of them were measured on GPUs, where a small MLP runs at tensor-core rates and BVH
traversal is the irregular part. crust is CPU-only, so it is an open question whether a
neural query pays here at all.

There is one regime where it might. The kernel's design record measures out-of-cache
traversal as latency-bound, at about 140 ns per node. The Moana island's largest
prototypes (isDunesB, the isBayCedar family) are exactly that regime. A hash-grid and
MLP evaluation reads a table of about 1 MiB, which stays in cache, so it might answer a
shadow ray faster than a cold descent through a multi-million-triangle tree.

This change runs the experiment, in the same spirit as the `bvh8` feature. It is opt-in,
measured against the exact kernel, and kept as a record whatever the answer. Its scope
is the one query a learned model can answer without touching shading: **shadow-ray
occlusion inside large instanced prototypes**.

## What Changes

- **A non-default `neural-bvh` cargo feature on `crust-rt`**, forwarded by `crust-core`
  and `crust-render` the way `bvh8` is. With the feature off, nothing is compiled and
  every image and instruction count is unchanged.
- **Learned occlusion proxies, built at commit, in the N-BVH style.**
  - The kernel chooses a *cut* of a prototype's own BVH4.
  - It trains one multi-resolution hash grid and a small MLP. Given a ray segment
    clipped to a cut node's box, they predict whether any surface lies on it.
  - `Scene::occluded` then traverses exactly down to the cut and asks the model there.
  - `Scene::intersect` is untouched and stays exact. Camera rays, bounces, shading
    points and every hit id are exactly what they are today.
  - Training is deterministic: openqmc draws and a fixed-order gradient reduction. It
    is safe Rust with no new external dependency (`openqmc-rs` moves from a dev- to an
    optional dependency of `crust-rt`).
- **Eligibility is narrow on purpose.**
  - A proxy is only built for a motion-free scene above a triangle-count threshold, made
    only of triangles, directly or through static instances (Moana's grouped prototypes
    are instances of parts).
  - The importer only asks for one on instanced prototypes with no pass-through
    material (a cutout, or a thin-walled surface with straight transmission).
  - Queries whose mask differs from the one the proxy was trained for fall back to the
    exact tree.
- **Phase 1, a measurement gate.**
  - `ray_throughput --neural` measures proxy occlusion against `Scene::occluded` on the
    kernel's fixtures, in and out of cache.
  - A new `crust-render` example, `neural_probe`, does the same on real USD prototypes
    with a shadow-ray distribution.
  - Both report throughput, false-positive (spurious shadow) and false-negative (light
    leak) rates, training time and proxy memory.
- **Phase 2, render integration only if Phase 1 passes its gate.**
  - `CRUST_NEURAL_OCCLUSION` (`off` | `on`, default `off`, the exact behaviour) turns
    proxies on for eligible prototypes.
  - `CRUST_NEURAL_MIN_TRIS` sets the size threshold.
  - `--stats` reports the proxies built, their memory and their training time.
- **The render becomes biased when the switch is on.** NEE sees approximate
  visibility while the BSDF bounce side stays exact, so MIS mixes two visibilities.
  This is documented as bias, measured as a relmse plateau against the exact image, and
  never a default.

## Capabilities

### New Capabilities

(none). The proxy is a kernel query mode, and its render switch is an integrator
option. Both belong to existing capabilities.

### Modified Capabilities

- `intersection-kernel`: gains a requirement for opt-in, approximate occlusion proxies.
  These are deterministic, fall back to exact on any query they were not trained for,
  and never change `intersect`.
- `rendering`: gains a requirement for the `CRUST_NEURAL_OCCLUSION` switch (Phase 2):
  - which shadow rays use proxies;
  - that the off side is bit-identical to today;
  - that the on side is a biased approximation whose error is reported.
- `cli`: gains a requirement that `--stats` reports the occlusion proxies when any were
  built (Phase 2).

## Impact

- **`crates/crust-rt`:**
  - a new `neural/` module behind the feature: hash grid, MLP, Adam, the probe-ray
    generator, the cut selection and the trainer;
  - `CommitOptions` gains the proxy request;
  - `Bvh::hit_any` gains a cut-aware variant;
  - `MemoryFootprint` gains a proxy line;
  - `ray_throughput` gains `--neural`.
- **`crates/crust-core`:**
  - `Config` fields for the two switches;
  - `commit_options()` / the instancing import pass the request only for eligible
    prototypes (no pass-through material, which only the importer knows);
  - stats plumbing.
- **`crates/crust-render`:** feature forwarding and the `neural_probe` example.
- **Docs:**
  - `openspec/specs/intersection-kernel/design.md` records the measurements, positive
    or negative, the way the `bvh8` result is recorded;
  - `docs/architecture.md` § Environment switches;
  - `site/` environment-variable and limitation pages (Phase 2 only).
- **Performance:**
  - Feature off: zero cost by construction.
  - Feature on, switch off: at most one predictable branch per `occluded` descent into
    an instance, pinned by callgrind on `cornellbox`.
  - Switch on: import time grows by the training cost (seconds per proxied prototype),
    and kernel memory grows by about 1 MiB per proxy. The exact tree is kept for
    closest-hit, so **this experiment saves no memory**. It is a shadow-ray speed
    question only.
- **Out of scope:**
  - neural closest-hit, which needs hit points, normals, UVs, Ptex face ids and
    material ids;
  - dropping the exact tree below the cut;
  - neural proxies on the top-level scene, curves, spheres or pass-through geometry;
  - caching trained weights on disk;
  - any GPU path.
- **Expected outcome, stated honestly.** Per query, the model costs a few thousand
  instructions, comparable to an in-cache BVH4 any-hit. The win, if any, is confined to
  out-of-cache prototypes. A negative result is an acceptable outcome. In that case
  Phase 2 is dropped, through `/opsx:update`, and only the design record lands.
