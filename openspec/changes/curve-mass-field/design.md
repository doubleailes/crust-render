## Context

See proposal.md (Why). What this builds on:

- `curve-attributes-to-materials` (prerequisite): a curve hit knows its
  position along the strand `v`, its curve index, a per-curve random number,
  and the primvars its material reads; `ShadeCtx` carries them.
- Hair fibres already let their light pass out of the strand they start in
  (`Ray::ignore_curve_exits`, `ShadingPoint::passes_out_of_curves`), with the
  mixed-vertex rules for a fibre sharing a closure with a transmitting leaf
  (materials design record, `chiang_hair_bsdf`). A ray starting outside a tube
  still meets every other strand as an opaque tube.
- The USD import is single-threaded and reads the time code from a
  thread-local (`EvalTimeScope`); anything parallel must take the time
  explicitly.
- `resolve` must return what per-query shading would (`tests/resolve.rs`); the
  JIT is bit-identical to the interpreter; an MCP edit applied in place must
  equal a full re-import, with import-baked state listed in `Signature`
  (`rt_world.rs`).

## Goals / Non-Goals

**Goals:**

- Two curves crossing at a point read the same mass colour and the same mass
  normal there, exactly (both are functions of position alone).
- The field's look is controlled by its attributes and the material graph, not
  by the simulated geometry: changing the radius, the threshold or the colour
  graph needs no new simulation.
- Deterministic: the same scene bakes the same field, bit for bit, whatever the
  thread count, and adds no Monte Carlo noise.

**Non-Goals:**

- A physically based multiple-scattering model (dual scattering is a separate
  gap of the materials record).
- Shadows read from the field (future work: density transmittance).
- Motion-blurred fields: the field is baked once per frame (see Risks).
- Meshes: the bake reads curves only, though nothing in the field is
  curve-specific once built.

## Decisions

### The field holds density, signed distance and colour

Per voxel: coverage density `ρ`, the signed distance `d` to the iso-surface
`ρ = threshold` (negative inside), and a colour `C` (RGB), when the groom bakes
one.

- **`ρ`** is each strand sample splatted with a smooth compact kernel of radius
  `crust:mass:radius`, weighted by the strand's width so a thick strand counts
  for more. Samples are spaced along each strand at a fraction of the voxel
  size, so the splat does not alias.
- **`C`** is `Σ wᵢ cᵢ / Σ wᵢ` with the same weights: a coverage-weighted mean,
  so a voxel's colour is that of the strands around it, never darker for
  being sparsely covered.
- **`d`** is computed from the thresholded `ρ` by fast sweeping (an Eikonal
  solve over the grid), in a narrow band of a few radii around the
  iso-surface; beyond the band `d` is clamped.

### The mass normal is the gradient of the signed distance

`N_mass(P) = normalize(∇d(P))`, by central differences of trilinear `d`.

- **Alternative: the density gradient, `−∇ρ / |∇ρ|`.** Cheaper, but deep inside
  a clump `∇ρ → 0` and the normal is noise — the opposite of a mass. Rejected.
- **Alternative: the nearest point of an authored proxy mesh.** Total artist
  control, but another asset to model, rig and keep in step with the
  simulation. Kept as a possible later override, not built here.
- `∇d` is defined everywhere the band reaches: inside the mass it points to the
  nearest point of the mass's surface, as a sculpted surface's normal would,
  which is what makes two strands crossing deep in a clump agree.
- **Depth into the mass** is `max(−d, 0)`, exposed as its own lookup: a
  design signal for value variation through the volume, independent of light.
- **Where the gradient vanishes, the lookup gives its default.** Exactly on a
  symmetry axis (a round clump's centre line) the two sides cancel, and a field
  whose coverage never reaches the threshold has no surface, so its distance is
  the band's clamp everywhere and its gradient zero. A normal is never made up
  there; the material's `default` stands in, as it does off a field.

### The colour bake evaluates an explicitly connected graph

The colour is not inferred from the material's BSDF. The bound material
connects a `crust:mass:color` input to a graph; the bake evaluates that graph
at each strand sample, with the sample's position, `v`, curve index, random
number and primvars in `ShadeCtx`.

- A node of that graph that needs a view or a hit (`viewdirection`, `normal`,
  the footprint) is refused with a warning naming it: a bake has neither.
- **Alternative: bake the BSDF leaves' `color` inputs.** Rejected: which input
  is "the colour" is a look decision, and a guess would bake the wrong one.
- **Alternative: no colour bake, density and distance only.** Kept as the
  default when `crust:mass:color` is unconnected: the normal and the depth
  still work, and a material can still blend its own per-strand colour.

### Grouping and resolution

- A field covers one curve prim, or every curve prim sharing a
  `crust:mass:group` token under the same placement, so a groom split over
  several prims bakes one mass and two characters' hair never merge.
- **A group's settings are its first prim's**, in path order: one field has
  one radius, threshold and cap. A prim that authors others is named by one
  `mass.group_conflict` warning rather than silently overruled.
  **Alternative: the largest radius, the smallest threshold.** Rejected: no
  ordering of thresholds is "safer", and a rule per setting is harder to
  predict than one prim deciding.
- **The radius is in the curves' own units**, as `widths` are, so a scaled
  groom scales its mass with its strands. A top-level field is baked in world
  space, its radius scaled by the cube root of the prim's transform's
  determinant (exact for a uniform scale; under a non-uniform one, the
  isotropic stand-in the texture footprint already uses).
- The voxel size is `radius / 2`, its count capped by
  `crust:mass:maxResolution` (per axis); when the cap binds, the voxel grows
  and the import says so once at DEBUG.
- The grid is sparse, in bricks: only bricks the splat or the band reach are
  allocated. `--stats` reports bricks, bytes and bake time per field.
### Instanced grooms: only where the hit can name its placement

A field baked in prototype space needs, at a hit, the placement the ray went
through, to carry `P` into prototype space and the normal back out. A hit
carries `(geom_id, prim_id)` and nothing about the placement
(`rt_world.rs`, `VertexSource`). That decides what this change supports:

- **Top-level curves**, and curves baked into the top-level scene: the field is
  baked in world space and looked up with the hit's `P` as is.
- **A direct, unlabelled, static instance of rotation and uniform scale** —
  the one case where the id *is* the placement, which `VertexSource::Placed`
  already records for tangent frames: one field per prototype, in prototype
  space, scoped by the stage epoch like every prototype-keyed cache. The
  lookup takes `P` through the recorded placement's inverse, returns the
  normal through the inverse transpose of its linear part, renormalised, and
  the depth times its scale. A non-uniform scale would stretch the mass with
  its strands, but depth would then depend on direction, so such an instance
  falls back as below.
- **Every other placement** — a prototype placed through an instancer's group,
  whose forwarded slot id two differently transformed placements share, a
  motion-blurred instance, intersected through a transform interpolated at the
  ray's time, and a non-uniformly scaled instance — cannot be read correctly.
  Its lookups return their `default` inputs, so the material falls back to
  per-strand shading, and the import raises one warning per prim
  (`mass.unplaceable`, listed in `site/content/docs/reference/warnings.md`)
  naming it. No lookup ever guesses a transform.
- **Alternative: bake one world-space field per placement.** Correct for
  every placement, but memory scales with the placement count, and a forwarded
  hit still cannot say which placement's field to read. Rejected.
- **The general fix** is the kernel reporting the hit point in the innermost
  instance's space, beside `dpdu`: an intersection-kernel change with a cost
  on every ray, to be measured in its own change. It is listed as this
  capability's first known gap.

### The lookups are MaterialX nodes in a crust library

`crust_mass_color`, `crust_mass_normal`, `crust_mass_depth`,
`crust_mass_density` read the field of the geometry hit. On geometry without a
field, each returns its `default` input, as MaterialX does for an absent
geometric property, so a material degrades to per-strand shading instead of
failing. The nodedefs ship with the renderer, beside the stdlib it already
reads.

### The mass diffuse is a new leaf, not a reinterpreted one

`crust_mass_diffuse_bsdf(color, normal)`: Lambertian around `normal`, cosine
sampled around it, with the matching pdf in `eval` (the NEE ↔ BSDF pair). Its
hemisphere is `normal`'s, not the tube's, and its light passes out of the
strand it starts in, exactly as a hair fibre's: the leaf is marked like a
fibre leaf (`hair_leaves`).

**Its normal is kept whichever way it faces.** Every other leaf's normal goes
through `prepare` (`closure/mod.rs`), which drops a normal facing away from the
hit's ray-facing normal (`n · rec.normal ≤ 1e-3`) and substitutes the hit's.
On a curve that is the tube's normal, and a mass normal routinely faces away
from it — a strand on the far side of a clump, seen through a gap — so that
rule would put back exactly the discontinuity the field removes. The mass
leaf's frame is built from its own normal unfiltered; only a non-finite or
zero-length one falls back, to the tube's normal. Pinned by a shading test
with the strand's normal and the field's opposed: the leaf's response follows
the field's normal, and differs from what the tube's normal gives.

**Mixed with other leaves.** Its light passing out of the strand is the fibre
rule, so the vertex-wide flag (`ResolvedClosure::hair`) is set for it. A leaf
beside it that is not a fibre must still meet its own tube's far wall wherever
its light enters the tube. Today the mixed-vertex split (`mixes_hair`)
engages only beside a transmitting leaf, on the reasoning that a reflecting
leaf's light never enters the tube. That holds for a reflecting leaf on the
hit's own normal, but not for one with a normal of its own (a normal map, or
another mass leaf's field normal blended into an ordinary diffuse), whose
hemisphere reaches into the tube. This change widens the test: the split
engages beside any non-fibre leaf that is transmitting **or** whose prepared
normal differs from the hit's. The same widening fixes the pre-existing case of
`chiang_hair_bsdf` beside a normal-mapped reflecting leaf. Pinned by a variant
of `only_the_fibres_light_passes_out_of_a_mixed_strand` with a tilted-normal
diffuse in place of the translucent leaf; the common case, a mass leaf mixed
with a fibre or with a diffuse on the hit's normal, keeps the single shadow
ray.

- **Alternative: let `oren_nayar_diffuse_bsdf` on a curve take its `normal`
  input this way.** Rejected: it would change what existing documents render.
- The leaf is not energy-reciprocal against the geometry (its normal is not
  the surface's). That is the point of it; the furnace test checks it returns
  `color` exactly under a uniform environment, and the design record says it
  is a stylisation leaf.

### The bake is a parallel pass after the USD read

The import collects the opted-in curves (spans, widths, the colour graph's
inputs per sample), then bakes every field in parallel, outside the
single-threaded USD read, with the frame's time passed explicitly. The splat
accumulates per brick in a fixed order, so the field is bit-identical across
thread counts — pinned by a test that bakes at 1 and N threads.

### Blending stays in the graph

The engine exposes the lookups and the leaf; how much of the mass a strand
takes is a `mix` in the material, driven by any signal the graph has — a
constant, `v`, depth, density. This keeps every look decision in the asset.
`samples/` gains one documented example graph, built only from the nodes
above.

## Risks / Trade-offs

- **[The iso-surface shimmers as simulated strands jitter frame to frame]** →
  the radius and threshold set how much jitter reaches it; the spike measures
  it over consecutive frames before the bake is tuned; a temporal filter is a
  later option, not a default.
- **[Flyaways far from the mass read a distant surface's normal]** →
  `crust_mass_density` lets the graph fade the mass where coverage is low; the
  example graph does so.
- **[Bake time per frame on dense grooms]** → sparse bricks, splat cost linear
  in samples, measured on the largest available groom and reported by
  `--stats`; samples per voxel is tunable.
- **[Memory]** → bricks only where needed; the cap on resolution bounds the
  worst case.
- **[A field static over the shutter under motion blur]** → documented as a
  known gap; the strands still blur, only their mass colour and normal do not.
- **[Instancer-placed and motion-blurred instanced grooms get no field]** →
  warned per prim, falling back to per-strand shading; the kernel-side local
  hit point is the follow-up (see "Instanced grooms").
- **[MCP edits]** → every `crust:mass:*` attribute, the group membership and
  the colour graph belong in `Signature`, so an edit rebuilds the field
  through the import's own bake.

## First task: a measured spike

Before any of the above is built in full: a synthetic `.usda` with two sheets
of strands crossing at an angle and one clump, rendered with the `normal` AOV
and the beauty, the mass normal and colour blended at 0 and at 1. The measure
is the gradient energy of the normal and of the colour across the crossing
line against the same measure away from it; the interpenetration is gone when
the ratio reaches about 1. Then the same over a few consecutive frames of a
simulated groom, for stability. The result goes into this design record.

### What the spike measured

`samples/mass_spike.usda` (from `scripts/gen_mass_spike.py`): two sheets of
straight strands 0.04 apart and 0.05 wide (so neighbours overlap, as hair
does), in the planes `z = ±0.25 x`, their strands 25° either side of the x
axis, crossing along `x = 0`; and a round clump of 151 parallel strands. The
`mass_spike` example (`crates/crust-render/examples/`) bakes a dense field as
designed (width-weighted compact splat, coverage-weighted colour, fast-swept
signed distance), casts orthographic rays straight down through the
renderer's own world, and compares at each first hit the tube's normal and
the strand's colour with the field's normal and colour. Gradient energy is
the mean squared difference between neighbouring pixels, over hits on the
sheets, in a band of two radii either side of the crossing line and in a band
away from it (three radii clear of the sheets' ends, where the mass curls
round).

**The field removes the seam.** The tube normals show the crossing as a hard
herringbone, the per-strand colour as a hard red/blue line; the mass normal
and colour run through it. At the crossing the mass normal carries 0.04–0.09%
of the tube normal's gradient energy, and the mass colour about 1% of the
per-strand colour's, at every radius tried. The clump's normal is radial at
its sides (mean `|n.x|` 0.85) and up over its axis (mean `n.z` 0.98).

**The crossing/away ratio is not the measure to use.** The tube normal's own
ratio is 0.98: each tube's curvature dominates everywhere, so the ratio cannot
show the seam it should. Each sheet being one colour, the per-strand colour's
ratio is undefined. The mass normal's ratio swings from 0.3 to 4.2 with how
smooth the region away from the crossing is, because at the crossing it keeps
the mass's genuine crease, where two planes meet, smoothed over about a
radius. What shows the seam gone is the mass signal's energy at the crossing
as a fraction of the per-strand signal's: under 0.1% for the normal and about
1% for the colour.

**Central differences of the trilinear distance band at the voxel size.**
With a one-voxel step they equal the voxels' own central differences
interpolated trilinearly. Either way, the normal is only continuous, and with
fast sweeping's first-order error on a tilted plane, it shows faint bands one
voxel apart, which diffuse shading would show as Mach bands. One separable
binomial pass, `(1 2 1)/4` along each axis over the distance before taking
gradients, removes most of it. Mass-normal energy away from the crossing:

| radius (strand spacings) | design's gradient | after one smoothing pass |
|---|---|---|
| 0.08 (2) | 0.000253 | 0.000008 |
| 0.12 (3) | 0.000185 | 0.000030 |
| 0.16 (4) | 0.000655 | 0.000360 |

(the tube normal's is about 0.20). At four spacings the voxel is 0.08 and the
staircase outlasts one pass.

**Stability scales with the radius against the strand spacing.** Five frames,
every strand point displaced independently per frame by up to a fixed
distance in each axis; the field's normal read at frame 0's hit points; the
angle between consecutive frames, with the smoothing pass:

| jitter per frame | radius 0.08: sheets / clump, mean (p95) | 0.12 | 0.16 |
|---|---|---|---|
| 0.008 (a fifth of the spacing) | 4.4° (10.7°) / 1.4° (3.5°) | 2.7° (6.8°) / 0.7° (1.8°) | 2.0° (4.9°) / 0.5° (1.3°) |
| 0.02 (half of it) | — | 7.0° (17.9°) / 1.8° (4.3°) | — |
| 0.04 (all of it) | — | 16.0° (41.2°) / 3.3° (8.0°) | — |

Without the pass each figure is about half as large again (0.12, 0.008:
4.1° / 1.05°). A one-layer sheet is the hard case: every hit on it lies
within half a strand of the mass's medial surface, where the distance
gradient turns over, so under a full spacing of jitter its normal can flip
(maximum 170°) while the clump's stays under 14°. A groom many strands deep
behaves like the clump.

**Voxels finer than half the radius do not help.** At a quarter radius the
seam measure is unchanged, the clump is less stable (the field resolves more
of the jitter: 3.3° against 1.4° at 0.08, smoothed), and the bake is ten
times slower. **A lower threshold does not either**: at 0.08, 0.3 trades a
little of the clump's stability (1.7° against 1.4°) for a little of the
sheets' (3.9° against 4.4°), and leaves the seam measure as it was.

**Settled**: radius about three times the spacing between neighbouring
strands (the documentation's guidance, since the radius is authored per
groom), threshold 0.5, voxel half the radius, and one smoothing pass of the
distance. The whole bake of the sample takes 0.1 s.

### Amendments the spike proposes (pending review)

- **The mass normal's decision** gains the smoothing pass: the normal is the
  normalised gradient of the signed distance after one binomial pass along
  each axis, interpolated from the voxels' central differences.
- **Task 1.2's criterion** becomes the mass signal's gradient energy at the
  crossing as a fraction of the per-strand signal's (under 1% for the normal,
  a few percent for the colour), in place of the crossing/away ratio.
- **A known gap**: a one-layer sheet of strands has every hit near the
  mass's medial surface, so its mass normal is unstable under frame-to-frame
  jitter of the order of the strand spacing; `crust_mass_density` lets a
  material fade the mass there.
