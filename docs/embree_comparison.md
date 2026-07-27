# Embree vs. Crust Render — a deep comparison

*Written against Embree 4.4.x ([RenderKit/embree](https://github.com/RenderKit/embree),
latest release 4.4.0, master documents 4.4.1) and the current state of this
repository (2026-07).*

The single most important thing to understand before comparing the two: **they are
different kinds of software**. Embree is a *ray tracing kernel library* — it builds
acceleration structures and answers ray queries, and does nothing else. Crust is a
*complete renderer* — integrator, materials, lights, media, sampling, scene I/O — of
which the part Embree covers (primitives + BVH + traversal) is roughly 600 lines
(`bvh.rs`, `aabb.rs`, `primitives/`, `hittable.rs`). So the comparison splits into
three questions:

1. On the ground Embree actually occupies (intersection kernels), how does crust's
   implementation compare?
2. What does crust do that Embree deliberately does not?
3. What Embree ideas are worth stealing (or binding to) for crust?

---

## 1. What Embree is

Intel's open-source (Apache-2.0) collection of high-performance ray tracing kernels,
targeting professional renderers — it is the intersection backend of Blender Cycles,
Autodesk Arnold, Chaos V-Ray/Corona, DreamWorks MoonRay, and many others. It exposes a
C99 API (`rtcNewDevice` → `rtcNewScene` → `rtcNewGeometry` → `rtcCommitScene` →
`rtcIntersect*`/`rtcOccluded*`) plus a SYCL path for Intel GPUs. It ships **no
shading, no lights, no sampler, no image output** — the application owns the
integrator; Embree owns "given this ray, what does it hit, fast."

### Geometry types

| Type | Details |
|---|---|
| Triangle meshes | Indexed; watertight intersection (Woop et al. 2013) |
| Quad meshes | Stored as one primitive, intersected as two triangles |
| Grid meshes | Regular grids, compressed in-memory representation for heavy displaced geometry |
| Subdivision surfaces | Catmull-Clark, with edge creases, corners, holes, face-varying interpolation |
| Curves | Linear / Bézier / B-spline / Hermite / Catmull-Rom bases; each as *flat* ribbons (distant hair), *round* swept tubes (close-up), or *normal-oriented* ribbons |
| Points | Spheres, ray-oriented discs, normal-oriented discs (particles, point clouds) |
| User geometry | App-provided bounds + intersect/occluded callbacks (analytic prims, out-of-core geometry) |
| Instances | **Multi-level** instancing and instance arrays (low-overhead massive instancing) |

### Motion blur

Multi-segment motion blur with **2–129 time steps** per geometry, on *every* geometry
type: vertex (deformation) blur, affine instance transform blur, and **quaternion
motion blur** for correctly interpolated rotations (propellers, wheels). Rays carry a
`time` in [0,1]; the BVH stores time-bounded bounds (MBlur BVH with 4D
spatial-temporal splits).

### Query API

- `rtcIntersect1/4/8/16` — closest-hit, single rays or SIMD packets of 4/8/16.
- `rtcOccluded1/4/8/16` — dedicated boolean occlusion queries that **terminate on the
  first hit found** — the shadow-ray fast path.
- `rtcForwardIntersect/Occluded` — continuing rays through instance boundaries in SYCL.
- **Filter functions** — per-geometry or per-ray-query callbacks invoked on *every*
  candidate hit, able to accept or reject it: transparency-mapped shadows, alpha
  cutouts, self-intersection avoidance, collecting *all* hits along a ray.
- **Ray masks** — per-geometry/per-ray 32-bit visibility masks (e.g. camera-only or
  shadow-exempt geometry).
- **Point queries** (`rtcPointQuery`) — closest-point-on-surface within a radius
  (photon lookups, signed distance).
- **Collision detection** (`rtcCollide`) — BVH-vs-BVH scene intersection tests (cloth).

### Acceleration structures and builders

- Internally **wide BVHs** — BVH4 / BVH8 chosen per ISA, with SIMD-parallel
  child-box tests during traversal; quantized/compressed node variants
  (`EMBREE_COMPACT_POLYS`, compact scene flag) for memory-constrained scenes.
- Three build qualities per geometry/scene:
  - `RTC_BUILD_QUALITY_LOW` — Morton-code builder, near-linear time, for
    per-frame rebuilds of dynamic geometry;
  - `RTC_BUILD_QUALITY_MEDIUM` — binned SAH (the default);
  - `RTC_BUILD_QUALITY_HIGH` — **spatial-split BVH (SBVH)**, presplitting large
    triangles for markedly better traversal on scenes with non-uniform triangle
    sizes;
  - `RTC_BUILD_QUALITY_REFIT` — keep topology, refit bounds for deformations.
- Builds are **fully parallel** (TBB by default; PPL or an internal task system as
  alternatives) and scale to many-core machines; 4.4 further improved build
  performance when applications oversubscribe threads.
- The builder is also exposed *standalone* (`rtcBuildBVH`) so applications can build
  their own BVHs over custom primitives with Embree's high-quality builders.

### Hardware targets

- **x86**: SSE2 → SSE4.2 → AVX → AVX2 → AVX-512, runtime ISA dispatch in one binary.
- **ARM**: NEON on Apple Silicon and AArch64 Linux (Windows-on-ARM experimental).
- **GPU (SYCL)**: Intel Xe HPG (Arc) and Xe HPC (Data Center Flex/Max), using
  hardware ray tracing units; out of beta as of 4.4. CPU and GPU share the same
  API and feature set (minus a few CPU-only features like collision detection).

---

## 2. Crust's intersection layer, on Embree's terms

Crust's equivalent surface is deliberately small and lives in safe Rust:

- **Primitives**: analytic `Sphere`, `Triangle`/`SmoothTriangle`, and indexed `Mesh`
  (`primitives/`). Triangle intersection is Möller–Trumbore with a
  size-relative epsilon (`triangle.rs:37`) — *not* the watertight scheme; rays can
  slip through shared edges/vertices in pathological cases, which Embree's default
  kernels rule out by construction.
- **BVH** (`bvh.rs`): a single flat-array **binary** BVH used at both levels — a
  top-level tree over scene objects (built in `Renderer::new`) and one nested tree
  per imported mesh (built at USD import). Construction is binned SAH — 12 bins on
  the longest centroid axis, leaf ranges of 2–8 primitives, SAH leaf-termination
  test, median-split fallback — i.e. exactly Embree's `MEDIUM` quality class,
  minus spatial splits. The build is **serial and recursive**; determinism is an
  explicit design goal (same scene → same tree), which Embree's parallel builders
  do not promise by default.
- **Traversal**: iterative with a fixed 64-slot stack, front-to-back child ordering
  by split axis and ray direction sign. One ray at a time; each primitive test is a
  virtual call through `Box<dyn Hittable>`. SIMD exists only *within* vector math
  (`glam::Vec3A` is 4-wide) — there is no packet/stream traversal and no wide-node
  SIMD box test.
- **Occlusion**: there is no dedicated any-hit query. NEE shadow rays reuse the
  closest-hit traversal (`shadow_transmittance`, `tracer.rs:787`), i.e. they keep
  searching for the *nearest* hit when *any* hit would do. Distance-bounded
  (`t_max = d − ε`) but without first-hit early-out.
- **Dynamism**: none. Transforms are baked into world-space triangles at USD import;
  there is no instancing (N copies of a mesh = N copies of its triangles and its
  BVH), no refit, no motion blur (rays carry no time; the camera has no shutter).
- **Parallelism**: rendering is Rayon-parallel over pixels/tiles, which keeps every
  core busy at the *renderer* level even though each individual ray query is scalar.
  This is the same architecture Embree-based renderers use — Embree parallelizes
  *within* a query only insofar as packets do; wavefront parallelism is always the
  application's job.

### Head-to-head on the kernel ground

*(Table updated after the adoption pass — the ✅⁺ rows were ❌ or weaker
when this document was first written; see the addendum in §4.)*

| | Embree 4.4 | Crust |
|---|---|---|
| Language / safety | C++ (+ ISPC, SYCL), unsafe by nature | 100 % safe Rust |
| Triangles | ✅ watertight | ✅⁺ watertight (Woop 2013, f64 tie fallback) |
| Spheres / points | ✅ spheres + 2 disc types | ✅ analytic sphere (also a `LightShape`) |
| Quads / grids / subdivision | ✅ | ❌ (USD import triangulates) |
| Curves / hair | ✅ 5 bases × 3 modes | ✅⁺ round linear segments; cubic bezier/bspline/catmullRom flattened |
| User geometry | ✅ callbacks | ✅-ish (`Hittable` trait — same idea, idiomatic Rust) |
| Instancing | ✅ multi-level + arrays | ✅ multi-level `Instance` (nests to any depth), shared local-space BLAS with content dedup; no instance *arrays* |
| Motion blur | ✅ 2–129 steps, quaternion | ✅⁺ 2-step transform blur (linear matrix lerp) |
| BVH arity | 4–8 wide, SIMD node tests | ✅⁺ 4-wide SoA nodes, `Vec4` slab tests |
| Build quality tiers | low / medium / high(SBVH) / refit | ✅⁺ medium + SBVH spatial splits (α-gated) |
| Parallel build | ✅ TBB, many-core | ✅⁺ rayon subtree tasks, deterministic |
| Two-level structure | ✅ (scene of instances) | ✅⁺ scene of instances sharing BLASes |
| Closest-hit query | ✅ 1/4/8/16 | ✅ single ray |
| Occlusion query | ✅ early-exit `rtcOccluded` | ✅⁺ early-exit `hit_any` on every shadow ray |
| Filter / any-hit callbacks | ✅ | ❌ (no alpha-cutout shadows) |
| Ray masks | ✅ | ✅⁺ camera/shadow/indirect bits via `crust:rayMask` |
| Point queries / collision | ✅ | ❌ |
| ISA dispatch / packets | SSE2→AVX-512, NEON, SYCL GPUs | scalar rays + 4-wide node tests via glam |
| Memory options | compact/quantized nodes | one wide-node layout |

The honest performance summary: for the scenes crust targets (a few meshes, up to a
few hundred thousand triangles), a well-built binary SAH BVH with ordered traversal is
within a small factor of Embree. The gaps that would actually show up in a profile,
in order: shadow rays doing closest-hit work (every NEE vertex), per-primitive
dynamic dispatch in leaves, binary vs. wide nodes, and serial build time on heavy
meshes. The gaps that show up in *capability* rather than speed: instancing, motion
blur, curves, and watertightness.

---

## 3. What crust has that Embree does not (and never will)

Everything above the query line — this is the part of crust that would *survive*
adopting Embree, not be replaced by it:

- The **iterative two-pass path integrator** with MIS (selectable power/balance
  heuristics + diagnostic single-strategy modes), Russian roulette, and
  emission-ownership bookkeeping (`tracer.rs`).
- **OpenPBR übershader** aligned against the MaterialX/Adobe references (aniso GGX
  VNDF, F82/Schlick, EON diffuse, sheen, thin-film, per-channel Cauchy dispersion,
  thin-walled and thick microfacet transmission).
- **Volumetrics**: carried interior media (weighted analog free-flight with chromatic
  correction) and free-standing `VolumeRegion`s (homogeneous / fBm noise / voxel
  grids) with delta tracking, ratio-tracked shadow transmittance, phase MIS. Embree
  has no concept of participating media at all.
- **Path guiding** (Practical Path Guiding SD-tree with variance-weighted pass
  blending and a guiding-efficiency gate) — renderer research territory Embree never
  touches.
- **QMC sampling** via the from-scratch `openqmc-rs` port (bit-for-bit against
  upstream), domain-tree threaded through the integrator by value.
- **Adaptive sampling**, area-light NEE, **USD scene import** (with a local xformOp
  composer working around an upstream `openusd` bug), EXR/PNG output.

Embree's tutorials contain a toy pathtracer, but it is demo code; the library's
contract stops at "what did the ray hit." Everything in this list is crust's actual
value; Embree is (by design) a component such a renderer would sit on top of.

---

## 4. Takeaways — what is worth adopting

Two distinct routes, not mutually exclusive:

> **Addendum 2 (kernel extraction).** The "openqmc move" has since been made:
> the whole intersection layer now lives in a dedicated **`crust-rt`** crate
> behind an Embree-shaped API — `Geometry` objects attached to a
> `SceneBuilder`, `commit()`, `Scene::intersect`/`Scene::occluded`
> (`rtcIntersect1`/`rtcOccluded1`), ID-based `RayHit`s (`geom_id`/`prim_id`),
> per-geometry masks. The renderer maps hits to materials through a
> `geom_id`-indexed table (`crust_core::World`), and light attribution moved
> from `Arc`-address identity to `geom_id`. This is exactly the seam Route A
> below would need: an optional Embree backend is now a matter of
> implementing the same four query/build entry points over `embree4-rs`.

### Route A: bind to Embree

Rust bindings exist (`embree4-sys`, `embree4-rs`). Cycles-class traversal speed,
instancing, curves, and motion blur essentially for free — at the cost of the
project's two founding constraints: **safe Rust** (an FFI kernel is unsafe C++ at the
bottom of every ray) and self-containedness (a large native dependency, ISA/build
matrix, no wasm story). Given that crust is explicitly a toy/learning renderer in
safe Rust, this route is coherent only as an optional, feature-gated backend behind
the existing `Hittable` seam — `trait` object in, `Hit` out — which the codebase's
narrow query interface (`hit(ray, t_min, t_max)`) would make straightforward to slot
in and A/B against the native BVH.

### Route B: adopt Embree's ideas natively (recommended order)

> **Addendum (implemented).** All seven items below have since been adopted
> natively, in safe Rust:
> 1. `Hittable::hit_any` early-exit occlusion traversal, used by every NEE
>    shadow ray.
> 2. Watertight Woop-2013 triangle intersection shared by
>    `Triangle`/`SmoothTriangle`, pinned by 10k-sample shared-edge tests.
> 3. `Instance` (shared local-space mesh BVHs, content-hash dedup at USD
>    import).
> 4. Parallel deterministic BVH build (`rayon::join` subtrees).
> 5. BVH4: post-build collapse into 4-wide SoA nodes with `Vec4` slab tests.
> 6. SBVH spatial splits with exact triangle clipping (`clipped_aabb`).
> 7. Ray masks (`crust:rayMask`), transform motion blur
>    (`crust:motion:translate` + shutter time on rays), and round curve
>    primitives (`UsdGeomBasisCurves` → sphere-swept cones).
>
> The original recommendation text is kept below as the design rationale.

1. **Dedicated occlusion query** — add `hit_any(ray, t_min, t_max) -> bool` to
   `Hittable` (default-implemented via `hit`) with a real early-exit BVH traversal
   (no front-to-back ordering needed, accept first confirmed hit) and use it in
   `shadow_transmittance`. Cheapest change with the broadest payoff: one shadow ray
   per NEE vertex on every path.
2. **Watertight triangle test** — adopt Woop-style watertight intersection (or at
   minimum a shared-edge-consistent formulation) in `triangle_hit`; crust's
   epsilon-based Möller–Trumbore is the classic source of pinhole leaks on shared
   edges of thin geometry.
3. **Instancing** — an `Instance` `Hittable` that stores an `Arc<Mesh>` (sharing the
   nested BVH) plus a world↔local transform, intersecting in local space. Removes the
   N× memory cost of repeated USD prims and is the prerequisite for `UsdGeomPointInstancer`.
4. **Parallel BVH build** — the per-mesh builds are embarrassingly parallel across
   meshes today (a Rayon `par_iter` at import time); a parallel top-down build of a
   single large tree (subtree tasks below a size threshold) is the second step.
   Determinism can be preserved: parallelism changes *when* subtrees are built, not
   *what* is built, if splits stay deterministic.
5. **Wide BVH (BVH4)** — collapse the binary tree post-build and test 4 child boxes
   with `glam`-friendly SoA layout (4×6 f32 slabs). This is the standard CPU answer
   to per-node cost and works in safe Rust; measure against the added leaf/dispatch
   cost before committing.
6. **Spatial splits (SBVH)** at import for meshes with high triangle-size variance —
   Embree's `HIGH` quality tier; helps architectural scenes far more than the
   showcase scenes currently in `samples/`.
7. Longer-term, only with a driving use case: motion blur (needs shutter-time in
   `Ray` and time-bounded AABBs), curve primitives for hair, and ray masks
   (trivially expressible as a bitset on `Hit`/geometry once needed).

Packet/stream traversal is deliberately *not* on the list: crust's Rayon-over-pixels
parallelism already saturates cores, packets pay off mainly for coherent primary
rays, and incoherent path-traced bounces defeat them — Embree itself steers users
toward single-ray queries for incoherent workloads.

---

## Sources

- [RenderKit/embree](https://github.com/RenderKit/embree) — README (master, v4.4.1)
- [Embree releases](https://github.com/RenderKit/embree/releases/) — 4.4.0 latest release
- [Embree CHANGELOG](https://github.com/RenderKit/embree/blob/master/CHANGELOG.md)
- This repository: `crates/crust-core/src/{bvh,hittable,tracer,volume}.rs`,
  `crates/crust-core/src/primitives/`, `CLAUDE.md`,
  `docs/openpbr_reference_alignment.md`
