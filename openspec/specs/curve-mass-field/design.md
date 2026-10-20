# Curve mass fields: design record

The behavioural spec is `spec.md` beside this file; the user documentation is
`site/content/docs/usd/mass-fields.md`. This is the record of why the field
is built the way it is, what the spike measured before it was built, the
traps met on the way, and the gaps that remain.

## What it is for

Stylised hair wants its colour designed into the *mass* of the hair — hue and
value varying across clumps and through the volume as the art direction
paints them, independently of the lighting — with the specular kept as a
separate layer. Rendering each strand with its own colour and its own shading
normal defeats that: wherever two strands cross, they disagree on both, and
the crossing reads as a hard line, the normal most of all. A field built from
the strands answers both, since everything read off it is a function of
position alone, and the look is then controlled by the field's parameters
rather than by the simulated geometry.

## The field

Per voxel: coverage density `ρ`, the signed distance `d` to the iso-surface
`ρ = threshold` (negative inside), the same distance after one smoothing pass
(the normal's), and, when the groom bakes a colour, `Σ w·c`.

- **`ρ`** is each strand sample splatted with a smooth compact kernel
  `(1 − r²)³` of radius `crust:mass:radius`, weighted by the strand's width.
  Samples lie a quarter voxel apart along each piece (`SAMPLES_PER_VOXEL`),
  each standing for its length of strand, and the weight is normalised by the
  kernel's line integral (`32/35`) so a lone straight strand as wide as the
  radius sums to coverage 1 on its axis, and one twice as wide to 2. That is
  what makes `crust:mass:threshold` a quantity in strand units.
- **The colour**'s weight is `ρ` itself: `Σ w·c` and `ρ` are accumulated by
  the same increments, so the lookup `Σ w·c / ρ` is a mean, interpolated
  between voxels as a mean (both channels interpolated, then divided), and
  never darker where coverage is sparse. The colour is not inferred from the
  material's BSDF: the bound material connects `outputs:crust:mass:color` to a
  graph and the bake evaluates that graph at each sample with the sample's
  position and its curve properties and primvars. Which input of a BSDF is
  "the colour" is a look decision, and a guess would bake the wrong one.
- **`d`** comes from the thresholded `ρ`: interface voxels seeded by linear
  interpolation of `ρ − threshold` along each axis, then fast sweeping
  (Zhao's eight diagonal orders, Godunov upwind update, twice over) in a band
  of three radii, clamped beyond it. The clamp also keeps a field whose
  coverage never reaches the threshold finite, its gradient zero rather than
  not a number.

### The mass normal

`N_mass(P) = normalize(∇d(P))`, by central differences of the trilinear
distance one voxel apart, which with that step are the voxels' own central
differences interpolated trilinearly.

- **The alternative, the density gradient `−∇ρ / |∇ρ|`**, is cheaper but
  deep inside a clump `∇ρ → 0` and the normal is noise, the opposite of a
  mass. Rejected.
- **An authored proxy mesh's nearest point** would give total artist control
  at the cost of another asset to model, rig and keep in step with the
  simulation. Kept as a possible later override.
- `∇d` is defined everywhere the band reaches: inside the mass it points to
  the nearest point of the mass's surface, as a sculpted surface's normal
  would, which is what makes two strands crossing deep in a clump agree.
- **Smoothing.** Fast sweeping is first-order, and on a tilted plane its
  staircase shows in the normal as faint bands a voxel apart, which diffuse
  shading turns into Mach bands. One separable binomial pass `(1 2 1)/4`
  along each axis over the distance removes most of it (the spike's
  measurements are below). The pass is applied to a copy: the normal is the
  gradient of the smoothed distance, while depth and the inside test read the
  distance as swept. A lone strand exactly as wide as the radius has an
  inside half a voxel thick, which the smoothing erases; the spec wants its
  centre line negative, and it is.
- **The grid is aligned to multiples of the voxel.** A symmetric field read
  at a symmetric point gives a symmetric answer only when the trilinear
  corners are symmetric about it: a round clump's axis lies on voxels, so
  the two sides cancel there and the lookup gives its default, as the spec
  has it; with an unaligned origin it would give a unit vector of rounding.
- **A vanishing gradient is relative to the voxel.** The gradient of a
  uniform band is rounding (`Σ band·wᵢ` differs from `band` in the last
  bit), which normalised is a plausible unit vector. A difference below a
  thousandth of a voxel is no surface: the lookup returns its default.
- **Depth** is `max(−d, 0)`, a design signal for value variation through the
  volume, independent of light.

### Sparse bricks, and the bake's order

The grid is held in bricks of 8³ voxels, with a dense brick index over the
grid: a splat reaches two voxels (the radius is two voxels) and a brick holds
eight, so the index is at most `(512/8)³` entries, a megabyte at the cap.
Bricks are held where a piece's splat box reaches, and the band's bricks are
the 26 neighbours of any brick holding an interface voxel (a band of six
voxels fits in a neighbouring brick). A brick holds every channel, so a
band-only brick costs its coverage too; the channels are `f32`s, 12 bytes
per voxel, 24 with a colour.

The splat is deterministic by construction: pieces are binned into the bricks
their splat box reaches, and every brick sums its pieces' samples in piece
order, independently — the bricks are shared among rayon's threads, but no
brick's sum depends on which thread summed it. A piece overlapping several
bricks is sampled once per brick, and its colour graph evaluated once per
sample per brick, rather than every sample being kept in memory: the sample
count of a production groom is tens of millions. The sweep and the smoothing
run once over the whole field, single-threaded, in grid order; fields bake in
parallel with one another. Pinned by the unit test that bakes a clump on one
thread and on eight and compares every channel bit for bit, and by the import
test that does the same through `Scene::from_usd`.

### The voxel and the cap

The voxel is half the radius (the spike: a quarter helps nothing and costs
ten times the bake). `crust:mass:maxResolution` caps the voxels along any
axis of the field's bounds (the strands' box padded by four radii: a radius
of splat and three of band); when it binds the voxel grows, once at DEBUG.

### Pieces

The bake splats straight pieces. A linear segment is one piece; a cubic span
is cut into pieces about half a radius long (at most 32), each knowing its
span and its part of the span's parameter, so the colour graph reads the
strand's properties and primvars at the sample through the bake's own strand
table — one decoded beside the material's, for the graph's primvars, since
the table a hit reads is in the material's program's order and the graph has
its own.

### Where the lookup lives

A hit carries `(geom_id, prim_id)` and nothing about its placement; the
field has to be found from the geometry. It is carried on the geometry's
strand table (`StrandTable::mass`), which a curve hit already borrows
(`CurveHit`): the hit record stays its 96 bytes, which the integrator moves
at every vertex and which grew by a `memcpy` call per move the last time a
field was added to it. A table is built for a curve whose material reads any
`crust_mass_*` node, whether or not it reads anything else of the strand; a
material reading none carries no lookup, and the field is baked and reported
either way. The MaterialX program says which of the four properties it reads
(`Program::mass_reads`), decided once per material, and the hit looks up those
alone: a normal is six trilinear reads of the smoothed distance, which a
material reading only the density should not pay.

### Instanced grooms: only where the hit can name its placement

A field baked in prototype space needs, at a hit, the placement the ray went
through, to carry `P` into prototype space and the normal back out. What a
hit knows decides what is supported:

- **Top-level curves** bake in world space, their pieces carried through the
  prim's transform and the radius scaled by the cube root of its determinant
  (exact for a uniform scale; under a non-uniform one, the isotropic stand-in
  the texture footprint already uses), and are read with the hit's `P` as is.
- **A direct, unlabelled, static `instanceable` instance of rotation and
  uniform scale** is the one case where the id *is* the placement, which the
  world already records for tangent frames (`VertexSource::Placed`): the
  field is baked once per prototype in the curves' own space, and each such
  placement gets its own copy of the strand table carrying the field through
  its own transform (`Arc::make_mut` on attach), the normal through the
  linear part renormalised (the inverse transpose's direction, for a rotation
  and uniform scale) and the depth times the scale. A non-uniform scale would
  make depth depend on direction, so it falls back.
- **Every other placement** — through a prototype's group, whose forwarded
  slot id two differently transformed placements share; by a `PointInstancer`;
  with a non-uniform scale — gets every lookup's default, and the import
  raises one `mass.unplaceable` per prim, on the importing thread when the
  baked fields are attached. The prim named is the prototype's own, as
  composition numbers it (`/__Prototype_N/...`), the only prim the slot knows.
  No lookup ever guesses a transform.
- The streamed import composes a prototype met in two chunks twice (the
  stage epoch), so such a prototype bakes one field per chunk. Memory, not
  correctness; the same holds of its kernel scenes.
- **The alternative, one world-space field per placement**, is correct for
  every placement but scales memory with the placement count, and a forwarded
  hit still cannot say which placement's field to read. Rejected.
- **The general fix** is the kernel reporting the hit point in the innermost
  instance's space, beside `dpdu`: an intersection-kernel change with a cost
  on every ray, to be measured in its own change. It is the first known gap.

### Grouping and the colour terminal

A field covers one curve prim, or every curve prim sharing a
`crust:mass:group` token under the same placement — at the top level, the
same world transform (its matrix's bits); in a prototype, the prototype and
the part's frame in it — so a groom split over sibling prims bakes one mass
and two characters' grooms never merge. A group's settings are its first
prim's in path order, decided when the field bakes (the traversal visits
siblings in reverse order, so arrival order is not path order), and a prim
authoring others is named by `mass.group_conflict`. The alternative — the
largest radius, the smallest threshold — was rejected: no ordering of
thresholds is "safer", and a rule per setting is harder to predict than one
prim deciding.

The colour terminal is compiled once per material, beside it in the material
cache, through the inline-network translator and the same host as a surface
(textures resolve, colours convert), optimised to its root, and kept as a
`MassColor` with a fingerprint of its program. A node needing a view or a
hit (`viewdirection`, `normal`, `normalmap`, `heighttonormal`, a shifted
chart) is named by `mass.color_unsupported` and reads its default: the bake
evaluates the graph with no view and no surface. A terminal connected to
anything but a MaterialX shader's output is ignored with the same code.

### Edits in a session

Every `crust:mass:*` attribute and a prim's group are geometry attributes,
never `inputs:`, so the session's classifier already makes their edit a full
import, which bakes the fields again. The colour graph is a material's
`inputs:` edit, which the session applies in place when the world lets it:
every curves slot records the fingerprint of the graph its material had, and
`World::check_mass_colors` refuses a rebuilt material whose fingerprint
differs — the field holds that graph's colour — so the session falls back to
the full import. A material edit that leaves the graph alone stays in place.
Pinned by the live test (refused, kept, kept) and the session test that
renders each edit against a fresh import of the same layer.

### The mass diffuse leaf, and what it changed in the closure

`crust_mass_diffuse_bsdf(color, normal)` is the Oren–Nayar lobe at zero
roughness, exactly `color / π`, cosine sampled with the matching pdf, around
a frame built from the leaf's own normal **unfiltered**. Every other leaf's
normal goes through the rule that drops one facing away from the hit's
(`n · rec.normal ≤ 1e-3`): on a curve that is the tube's normal, and a mass
normal routinely faces away from it, so the rule would put back exactly the
discontinuity the field removes. Only a non-finite or zero normal falls back
to the hit's. Its light passes out of the strand it starts in, as a fibre's
does: the leaf sets the vertex-wide flag and its bit in `hair_leaves`.

The mixed-vertex split (`mixes_hair`) was widened. It engaged only beside a
transmitting leaf, on the reasoning that a reflecting leaf's light never
enters the tube — true of one on the hit's own normal, not of one with a
normal of its own (a normal map, the field's normal blended into an ordinary
diffuse), whose hemisphere reaches into the tube. It now engages beside any
non-fibre leaf that transmits *or* whose prepared normal differs from the
hit's; the common mixes, a mass leaf or a fibre beside a diffuse on the hit's
normal, keep the single shadow ray. The same widening fixes the pre-existing
case of `chiang_hair_bsdf` beside a normal-mapped reflecting leaf. The leaf is
a stylisation leaf: not reciprocal against the geometry, since its normal is
not the surface's; the furnace test checks it returns `color` exactly.

The lookups and the leaf are defined in a crust node library
(`crates/crust-mtlx/libraries/crust/crust_defs.mtlx`) shipped with the reader
beside MaterialX's own definitions, with tables in `crust_mtlx::mass` that a
test keeps in step with the document; the JIT hands the lookup op to the
interpreter, as it does every op that reads the shading context, and is
pinned bit-identical over all four.

## What the spike measured

`samples/mass_spike.usda` (from `scripts/gen_mass_spike.py`): two sheets of
straight strands 0.04 apart and 0.05 wide, in the planes `z = ±0.25 x`, their
strands 25° either side of the x axis, crossing along `x = 0`; and a round
clump of 151 parallel strands. The `mass_spike` example
(`crates/crust-render/examples/`) bakes a dense field as designed, casts
orthographic rays straight down through the renderer's own world, and
compares at each first hit the tube's normal and the strand's colour with the
field's. Gradient energy is the mean squared difference between neighbouring
pixels, over hits on the sheets, in a band of two radii either side of the
crossing line and in a band away from it.

**The field removes the seam.** The tube normals show the crossing as a hard
herringbone, the per-strand colour as a hard red/blue line; the mass normal
and colour run through it. At the crossing the mass normal carries 0.04–0.09%
of the tube normal's gradient energy, and the mass colour about 1% of the
per-strand colour's, at every radius tried. The clump's normal is radial at
its sides (mean `|n.x|` 0.85) and up over its axis (mean `n.z` 0.98).

**The crossing/away ratio is not the measure.** The tube normal's own ratio
is 0.98 (each tube's curvature dominates everywhere); each sheet being one
colour, the per-strand colour's is undefined; the mass normal's swings from
0.3 to 4.2 with how smooth the region away from the crossing is, because at
the crossing it keeps the mass's genuine crease, where two planes meet,
smoothed over about a radius. The measure is the mass signal's energy at the
crossing as a fraction of the per-strand signal's.

**Smoothing.** Mass-normal energy away from the crossing, by radius (in
strand spacings), before and after one binomial pass:

| radius (spacings) | design's gradient | after one pass |
|---|---|---|
| 0.08 (2) | 0.000253 | 0.000008 |
| 0.12 (3) | 0.000185 | 0.000030 |
| 0.16 (4) | 0.000655 | 0.000360 |

(the tube normal's is about 0.20). At four spacings the voxel is 0.08 and
the staircase outlasts one pass.

**Stability scales with the radius against the strand spacing.** Five
frames, every strand point displaced independently per frame by up to a fixed
distance in each axis; the field's normal read at frame 0's hit points; the
angle between consecutive frames, with the smoothing pass:

| jitter per frame | radius 0.08: sheets / clump, mean (p95) | 0.12 | 0.16 |
|---|---|---|---|
| 0.008 (a fifth of the spacing) | 4.4° (10.7°) / 1.4° (3.5°) | 2.7° (6.8°) / 0.7° (1.8°) | 2.0° (4.9°) / 0.5° (1.3°) |
| 0.02 (half of it) | — | 7.0° (17.9°) / 1.8° (4.3°) | — |
| 0.04 (all of it) | — | 16.0° (41.2°) / 3.3° (8.0°) | — |

Without the pass each figure is about half as large again. A one-layer sheet
is the hard case: every hit on it lies within half a strand of the mass's
medial surface, where the distance gradient turns over, so under a full
spacing of jitter its normal can flip while the clump's stays under 14°. A
groom many strands deep behaves like the clump.

**Voxels finer than half the radius do not help** (the seam measure is
unchanged, the clump less stable, the bake ten times slower), **nor does a
lower threshold** (0.3 trades a little of the clump's stability for a little
of the sheets', and leaves the seam measure as it was).

**Settled**: radius about three times the spacing between neighbouring
strands (the documentation's guidance, since the radius is authored per
groom), threshold 0.5, voxel half the radius, one smoothing pass.

## Cost

Measured on this machine (4 cores, `cargo build --release`), `--stats`:

| groom | strands | cubic spans | bricks | field memory | bake |
|---|---|---|---|---|---|
| `samples/hair_mass.usda`, the mass clump (radius 0.45 cm, three spacings) | 421 | 2 526 | 195 | 2.29 MiB (with a colour) | 0.15 s |

The bake is 2.3% of that render's import and render time at 16 spp; the
`Bake mass fields` phase and the `curve mass fields` row report it.

**Rendering costs nothing where no field is read.** Instruction counts
(callgrind, one thread) against the branch point, images compared bit for
bit:

| scene | before | after | change | image |
|---|---|---|---|---|
| cornellbox, `-s 2` | 4 264 068 279 | 4 265 430 204 | +1.36 M (+0.03%) | identical |
| `samples/hair.usda`, `-s 2` | 693 213 085 | 694 867 041 | +1.65 M (+0.24%) | identical |
| `samples/hair.usda`, `-s 8` | 2 451 634 027 | 2 453 162 260 | +1.53 M (+0.06%) | identical |

The difference does not grow with the samples — four times the shading at
8 spp, the same 1.5 M — so it is the import's: reading the `crust:mass:*`
attributes and the colour terminal of every curve prim and material. Per
hit, the lookup is not read and the widened mixed-vertex split does not
engage (no fibre beside a tilted or transmitting leaf), and the shading
context carries the field by reference, a pointer, so a material that reads
none copies nothing more. Every other checked-in sample renders
bit-identical at 16 spp (`scripts/check_images.sh` against goldens from the
branch point); `cornellbox_guided` matches when both are rendered on an
idle machine — its guided render differs between two runs of the same
binary under load, which predates this change.

## Known gaps

- **Instanced placements.** A groom placed by a `PointInstancer`, through a
  prototype's group or with a non-uniform scale reads no field (above). The
  fix is the kernel reporting the hit point in the innermost instance's
  space, an intersection-kernel change to be measured on its own.
- **Motion blur.** The field is baked once per frame from the strands at the
  frame's time code and does not move over the shutter: the strands blur,
  their mass colour and normal do not.
- **A one-layer sheet of strands** has every hit near the mass's medial
  surface, so its mass normal is unstable under frame-to-frame jitter of the
  order of the strand spacing (the spike's table); `crust_mass_density` lets
  a material fade the mass there. A temporal filter is a later option, not a
  default.
- **Memory.** A brick holds every channel, so the band's bricks pay for
  coverage and colour they never hold; channels are `f32`. A head of hair at
  a production radius is a few tens of megabytes; the cap bounds the worst
  case.
- **The colour bake re-evaluates a piece's graph once per brick it
  overlaps**, and a sample's primvars are looked up per sample. A graph with
  textures on a dense groom is the bake's cost; the splat itself is linear in
  the samples.
- **Shadows** are still per strand: the field's density is not read as a
  transmittance. Future work, with an artist-friendly Marschner-family lobe
  and ribbons.
