## Context

`adaptive-subdivision` (archived 2026-10-02) chooses a level per placement of each
subdivision mesh, from the mesh's nearest point to the camera, and refines the whole
mesh uniformly to it through `subdiv::subdivide()` (`TopologyRefiner::refine_uniform`,
then limit masks). It also put in place what this change reuses unchanged:

- the target, the ceiling and the camera resolved before traversal (`SubdivPolicy`,
  `ScreenRate`, `subdiv_policy`);
- (prototype versions per rate bucket and their survey, which decision 7 replaces);
- the spectral-norm stretch, and the `--stats` block.

What the per-mesh level cannot do is in the proposal (the island). Two facts about the
stack shape this design:

- **What `opensubdiv-rs` ported when this was designed (0.3.0).** Three gaps were filed
  and closed while it was implemented: compact Gregory and patch storage (#21, #22,
  0.4.0), face-varying patches (#23, 0.4.0), and selected-face refinement and patch
  tables (#29, 0.5.0). crust pins tag 0.5.0.
  - `refine_adaptive(AdaptiveOptions { isolation_level, use_single_crease_patch })`.
  - `PatchTable` (`PatchTableFactory::create`), whose `evaluate_basis(patch, u, v)`
    returns the weights for the limit point and its `u` / `v` derivatives.
  - `PatchMap::find_patch(ptex_face, u, v)`, and `PtexIndices` (ptex face ↔ cage face
    and corner).
  - Regular B-spline patches are exact. Irregular regions use Gregory patches, an
    approximation (`far/gregory.rs`).
  - No face-varying patch table, and no `Bfr` (OpenSubdiv 3.5's per-face
    tessellation API).
  - `evaluate_basis` allocates three `Vec`s per call.
- **Ptex numbering.** A Catmull-Clark quad is one Ptex face. An `n`-gon is `n` Ptex
  quads, each spanning a cage-edge half, two spokes to the face centre, and a corner.

## Goals / Non-Goals

**Goals:**
- Tessellate a subdivision mesh at a rate that varies across its faces, watertight,
  every refined vertex on the limit surface.
- Keep the per-mesh path's inputs: target, ceiling, camera, stats.
- Keep uniform mode bit-identical, and the per-mesh adaptive path one switch away.
- Fit the Moana island at the default ceiling (task 7.3).

**Non-Goals:**
- Exact limit near extraordinary vertices: Gregory patches below the isolation depth.
- Loop tessellation: Loop meshes keep the per-mesh level.
- Lazy (first-hit) tessellation, displacement, a dicing reference camera.

## Decisions

### 1. The unit of tessellation is the Ptex face

Each Ptex face is a quad in its own `(u, v) ∈ [0,1]²`, which is exactly the domain that
`PatchMap::find_patch` and `evaluate_basis` address. Tessellating per Ptex face therefore
needs no other parameterization, and the Ptex coordinates of every sample come free.

- **Quad faces:** their four edges are cage edges.
- **`n`-gons:** each of the `n` Ptex quads has two half cage edges and two spokes.
- **Alternative: tessellate per cage face, mapping `n`-gons to a polygonal domain**
  (as `Bfr` does). Rejected: it needs a parameterization the port does not have, and
  Ptex addresses the quads anyway.

### 2. Edge rates are decided per edge, once

An edge's segment count `n` is a pure function of data both of its faces share, which
is what makes the tessellation watertight by construction:

```text
n = clamp(ceil(ℓ · σ / t), 1, 2^max)
ℓ = the edge's cage length (its chord), local units
σ = pixels per local unit, for unshared geometry (decision 7):
      s(world_xf) · f_px / d_edge,
      world_xf = the prim's world transform (for a prototype placed once,
                 placement · local)
      d_edge   = distance from the camera to the box of the edge's two cage
                 vertices (no limit point exists before the patches are built,
                 decision 10)
```

- **Rounding:** `ceil`, and `t` is the per-segment target, so `2^max` segments is the
  density of uniform level `max`.
- **The edge's own distance.** This is the change from the per-mesh level, and what
  lets a terrain mesh be fine near the camera and coarse far away. Shared prototypes
  are not rated at all (decision 7).
- **Edges next to an `n`-gon** are split at their midpoint by the Ptex quads on that
  side. Such an edge's rate is rounded up to an even number (at least 2), and each half
  gets `n / 2`, so the quad on the other side meets the same points.
- **Spokes** are internal to their `n`-gon, so only its own Ptex quads share them.
- **Alternative: rates per face, matched afterwards.** Rejected: whichever face wins a
  shared edge depends on visiting order, and the rate would no longer be a function of
  the edge.
- **Alternative: the limit curve's length instead of the chord.** Rejected: it needs
  the evaluation the rate is meant to size. The chord is usually the longer, which errs
  toward detail.

### 3. Face interiors: a grid, stitched to the edges

As Cycles does:

- **The interior grid:** a Ptex quad with edge rates `(n_bottom, n_right, n_top,
  n_left)` gets an interior grid of `max(n_bottom, n_top)` by `max(n_left, n_right)`
  cells.
- **The stitch:** the ring between the grid's outer row of vertices and each edge's own
  points is triangulated by merging the two ordered point sequences, always taking the
  shorter diagonal. This puts no vertex on an edge that the edge itself did not
  choose, so there are no T-junctions.
- **Degenerate interiors:** a face whose interior grid is one cell wide is triangulated
  directly between its two opposite edges.

The algorithm is pure (rates in, `(u, v)` points and triangles out), lives in a
USD-free `scene/tessellate.rs`, and is unit-tested on its own: watertightness,
manifoldness, triangle count, orientation.

### 4. Shared vertices are evaluated once

- **Who evaluates:** every corner of the cage is evaluated once. Every edge point is
  evaluated once, from the Ptex face with the lowest id among those that share the
  edge, walking the edge from its lower cage vertex.
- **Sharing:** both faces index the same vertex, so their shared edge is bitwise
  identical even where two patches would round apart.
- **Interiors:** points are evaluated by their own face.
- **Normals:** the cross product of the `u` and `v` derivatives, taken from the patch
  that evaluated the vertex. A vertex shared by two faces has one normal.
- **A face whose every edge is split once** is not refined at all: it renders its
  cage, smooth-shaded, as level 0 does (decision 10). A corner it shares with a refined
  face takes that face's limit point, so the two meet without a crack.

### 5. Evaluation through the patch table

- **Build:** `refine_adaptive_selected` at isolation depth **1**
  (`subdiv::ADAPTIVE_ISOLATION`), `use_single_crease_patch` on, and
  `consider_fvar_channels` when the mesh carries a face-varying chart; then
  `create_with_options_selected` for the selected faces (decision 10), with face-varying
  patches when there is a chart; then the refined control values.
- **Why depth 1.** Under Catmull-Clark an all-triangle cage is irregular everywhere
  (every split triangle's centre is a valence-3 vertex), so isolating to depth `d`
  refines every face `d` times. At the design's first `clamp(max, 2, 4)` the Moana
  ocean's 684 416-triangle cage cost a 19.9 GiB transient. Gregory patches cover what
  isolation leaves irregular: on the all-extraordinary cube the samples are exact down
  to the isolation depth and within 0.0185 of the uniform limit below it (0.9% of an
  edge at a rate of 4, next to an extraordinary vertex only); regular faces are exact
  B-spline patches at any depth.
- **Speed:** measured first (task 1.3): `evaluate` with derivatives runs at about
  1.2–1.4 M samples per second per thread, above uniform refinement's ~0.57 M vertices
  per second, so per-sample allocation is not the bottleneck and was not filed.
- **Memory, per patch (0.5.0):** a regular patch 94 B, a Gregory patch 1 038 B (2 721 B
  before #21). With selected faces, only refined faces pay it.

### 6. Texture tables

- **Ptex:** a per-face mesh's `FaceMap` carries, per triangle, its Ptex face and its
  three corners' `(u, v)` in that face, as `f32`s (a new `FaceMap` variant, 24 B per
  triangle). The dyadic `SubFace` (4 B) cannot represent a stitched grid.
- **Vertex-interpolated `st`:** evaluated with the same basis as the positions.
- **Face-varying `st`:** a channel of the refiner, under the mesh's
  `faceVaryingLinearInterpolation`, evaluated with smooth face-varying patches
  (`evaluate_face_varying`, not OpenSubdiv's legacy linear patches). UVs are stored per
  triangle corner, each evaluated in its corner's own Ptex face, so the two sides of a
  seam keep their values (`a_seamed_face_varying_chart_keeps_each_side`). An unselected
  face interpolates its chart linearly from its authored corner values.

### 7. Only unshared geometry is adaptive

The first implementation kept `adaptive-subdivision`'s rate buckets: one prototype
version per bucket, each edge rated at the *placement's* distance. Measured
2026-10-02, that failed the island exactly as the per-mesh level did (killed past the
56 GiB guard at 2 px), because every island element is `instanceable` with its
geometry under a payload, so its kilometre-wide terrain arrived as prototypes, placed
once and rated whole.

Production renderers draw the line elsewhere. MoonRay sets the adaptive error of every
*shared* primitive to 0 (`GeometryManager.cc`: "Set adaptive error to 0 on objects that
are instanced to disable adaptive tessellation"). Cycles bakes the transform into a
mesh only one object uses, which then dices in world space. RenderMan dices a shared
prototype once (`dice:referenceinstance`). So:

- **Unshared geometry** — a direct mesh prim, a native instance whose prototype has no
  other placement in its top-level subtree, and a `PointInstancer` prototype placed
  once — is rated edge by edge through its world transform (`placement · local`), as a
  direct mesh is.
- **Shared geometry** — every other prototype — is refined to the uniform level, the
  resolved `--subdiv-level` / `crust:subdivisionLevel` or 0. A forest costs one cage
  per species, as in uniform mode.
- **Counting placements.** Native placements are counted before a subtree is walked,
  by a walk of its `instanceable` prims (with the traversal's pruning: inactive,
  non-render purpose, invisible), keyed by prototype path. The count is per *top-level
  subtree* in every mode — the partition the streamed import uses, applied even to a
  single-stage import — because prototype paths are renumbered per streamed stage, so
  a whole-stage count would let streaming change the result. A prototype placed once
  in each of two subtrees is therefore unshared in both, and tessellated twice. A
  `PointInstancer` prototype's count is its number of placements in that instancer,
  read before any is built.
- **Nested scatters** inside an unshared prototype compose the world transform down;
  an inner prototype placed more than once by its instancer is shared.
- **What goes away:** the rate buckets, the prototype survey, the `(epoch, path, q)`
  keys (back to `(epoch, path)` for shared versions; an unshared placement is built
  for itself, uncached) and the `prototype versions` stat. This is a behaviour change
  to `adaptive-subdivision`, captured by the MODIFIED requirement.
- **Alternative: per-placement versions for placements that span many distances.**
  Rejected: no renderer surveyed does it, it duplicates shared geometry, and the
  island's spanning placements are all unshared anyway.

### 8. When a mesh takes the per-mesh path

- **Which meshes:** a `loop` mesh (the tessellator cuts quad Ptex faces only), and every
  mesh under `CRUST_ADAPTIVE_PER_FACE=0`. Face-varying charts no longer fall back
  (decision 6).
- **How:** `mesh_source` picks the path and counts it. The per-mesh path is
  `adaptive-subdivision`'s, for unshared geometry (decision 7).
- **The switch's off side** is that behaviour, for an A/B.

### 9. Reporting

`--stats` gains `per-face meshes N (fallback: M)`, `shared meshes N at level L` and an
edge-rate histogram in power-of-two bins, beside the level histogram. A DEBUG line per
tessellated mesh gives its Ptex faces, triangles and rate range, and one at the end of
the import the triangle-shape bins (`4√3·area / Σ edge²`) of interior and stitched
triangles.

### 10. Only faces with a finer edge get patches; the view decides the rest

Measured on the island (2026-10-02), patches for every face were the remaining cost
after decisions 5 and 7: the ocean's `ocean_geo1` has 2 053 248 Ptex faces and
tessellates to 4 107 624 triangles, so at most a few hundred faces are refined at all,
and `ocean_geo` (about 14.9 M triangles) would need about 45 M Gregory patches.

- **Selection.** Every cage edge is rated first (decision 2, from its cage endpoints).
  A face with an edge rated above 1 is *selected*. Only selected faces are refined and
  patched (`refine_adaptive_selected`, `create_with_options_selected`, opensubdiv-rs
  0.5.0, OpenSubdiv's `selectedFaces`), so the cost grows with the refined area.
- **Unselected faces render their smooth cage**, as level 0 does — what MoonRay does
  with a face whose tessellation factor is 0. Their corners take a refined neighbour's
  limit point where one exists. An edge point a refined `n`-gon forces on a shared edge
  (its midpoint) is used by the unselected neighbour, which is then fanned from its
  cage centroid instead of from a corner, so no T-junction opens.
- **The even rule** applies only next to *selected* `n`-gons: applied everywhere, it
  would select every face of an all-triangle cage.
- **The view.** A cage segment whose box, padded by its own diagonal (as MoonRay pads a
  face's), lies wholly outside the render camera's view pyramid is rated 1
  (`adaptive::Frustum`, `CRUST_ADAPTIVE_FRUSTUM`, default on); on the per-mesh path a
  mesh wholly out of view takes level 0. The pyramid is the four sides plus the eye
  plane: the sides alone meet at the eye, so a box behind the camera can pass each
  with a different corner. Out-of-view geometry still exists for reflections and
  shadows, at its cage. This cut `ocean_geo1` from 31.2 M to 4.1 M triangles.
- **Alternative: Cycles' off-screen dicing scale** (a coarser rate rather than none).
  Not taken: on the island the out-of-view ocean is the cost, and a scaled rate still
  refines it; the switch keeps the A/B honest.

## Risks / Trade-offs

- **[Gregory patches near extraordinary vertices are approximate]** → Bounded by
  measurement (decision 5): exact down to the isolation depth, within 0.9% of an edge
  below it on the worst-case cube.
- **[Stitched slivers]** A rate-1 edge beside a rate-8 interior makes long thin
  triangles. → The shorter-diagonal rule; their share is measured on the island
  (task 7.5) and reported at DEBUG.
- **[Unselected faces render their cage]** Far and out-of-view geometry sits on its
  cage, not its limit surface, as at level 0; a selected neighbour's corners are limit
  points, so a face between the two mixes them. → The same trade MoonRay makes, and
  invisible on screen by construction.
- **[Out-of-view geometry is coarse in reflections]** → `CRUST_ADAPTIVE_FRUSTUM=0`
  refines by distance alone.
- **[Explicit Ptex corners are 24 B per triangle]** → Paid only by per-face Ptex meshes.
- **[Shared geometry never gains detail]** A hero asset instanced twice near the camera
  gets the uniform level. → `--subdiv-level` raises every shared prototype; that is
  MoonRay's `mesh_resolution` trade, and the island's instances are vegetation.
- **[A prototype placed once per subtree is tessellated per subtree]** → Only when it
  is placed once in each, which a streamed import already duplicates today.

## Migration Plan

- **Opt-in:** this only acts in adaptive mode. `CRUST_ADAPTIVE_PER_FACE=0` restores
  per-mesh levels.
- **Uniform mode** is not touched, and the goldens gate it.
- **Rollback** is a revert.
