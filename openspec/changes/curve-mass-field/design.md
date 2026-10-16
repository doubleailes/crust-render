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
- The voxel size is `radius / 2`, its count capped by
  `crust:mass:maxResolution` (per axis); when the cap binds, the voxel grows
  and the import says so once at DEBUG.
- The grid is sparse, in bricks: only bricks the splat or the band reach are
  allocated. `--stats` reports bricks, bytes and bake time per field.
- Instanced grooms bake one field per prototype, in prototype space, scoped by
  the stage epoch like every prototype-keyed cache; a placement's lookup
  transforms `P` into it.

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
fibre leaf, so a closure mixing it with `chiang_hair_bsdf` has nothing to split
and the mixed-vertex path (`mixes_hair`) only engages beside a transmitting
leaf, as it does today.

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
