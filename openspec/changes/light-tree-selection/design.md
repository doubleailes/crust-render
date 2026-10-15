## Context

See proposal.md (Why). The current state:

- **Selection sits behind four lookups.** `LightList` (`light/list.rs`) serves
  every selection probability through `pick_index_at(p, u)`, `pmf_at(p, k)`,
  `find_index_by_geom_at(geom, p)` and `infinite_at(p)`. NEE calls them at the
  vertex it samples from: surface NEE, volume NEE and the shadow-link twins in
  `tracer/path.rs`. The bounce side calls them at `prev.pos`:
  `bounce_emission_weight` and `escaped_emission`. `power` is a CDF over
  `Light::power` built in `select_by`. `learned` and `neural` override it per
  position through `cache`.
- **The bounce side does not know the normal.** `PrevBounce` carries `pos`,
  `pdf`, `class` and `nee_count`. `PrevVertex::Phase` carries the same minus
  the delta flag. Debug builds check that NEE and the bounce side read the same
  position bits (`nee_picks_from` / `bounce_weighs_from`).
- **The pick dimension is stratified.** `stratified_pick(u, count, i)`
  spreads a vertex's `crust:lightSamples` over the pick dimension. That is
  why `power` inverts a CDF rather than an alias table: the samples that pick
  light *k* stay one contiguous slice.
- **What a light can report.** The only light is `AreaLight`: a `LightShape`
  (`SphereShape`, `RectShape`, `AffineShape` sphere/disk/tube) plus an
  `Emissive` that knows whether it is one-sided and its optional `Shaping`.
  The shaping exposes its world axis and a hard cone angle. `Light::power` is
  already a luminance flux. No light reports bounds.
- **Lights at infinity** (dome, distant) report no power and get a uniform
  share `1/live` under `power`.
- **Scene sizes.** The samples have 2–7 lights and ALab has 47, so no
  checked-in scene has many.

## Goals / Non-Goals

**Goals:**

- A light BVH over the finite lights, with pbrt-v4's `BVHLightSampler` as the
  template, as a new `LightSelection::Tree`.
- Bounds and leaves that can later hold an emissive triangle (mesh lights)
  without changing traversal or the MIS contract.
- One importance function shared by NEE and the bounce side, so that the
  pmf both read is the same `f32` bits.
- A defensive share that bounds the per-light loss against `power`.
- A measured gate that decides the default.

**Non-Goals:**

- Mesh lights, emissive triangles as leaves, `MeshLightAPI`.
- Adaptive tree splitting (Conty & Kulla's several lights per cut).
- BSDF-aware importance for glossy lobes (Liu et al. 2019, Tokuyoshi et al.
  2024).
- Visibility: that is `learned`'s job, and the layering of `learned` over the
  tree is the follow-up (`learned-light-selection-by-default`, on hold).
- Folding lights at infinity into the importance.
- Light-link-aware traversal. A picked light that does not illuminate the
  receiver is still discarded after the pick, as under every other mode.

## Decisions

### D1. A binary BVH over finite lights, one light per leaf

`light/tree.rs` builds a binary tree over the lights with finite power above
zero. Nodes go in a flat `Vec` in depth-first order, with one child implicit.
Each light stores a **bit trail**, its root-to-leaf path as a `u64` with one
bit per level, so the depth is at most 64. The map `light index → (leaf,
trail)` is filled at build.

Alternatives:
- A flat O(N) scan of per-light importance. Simpler, and as good below a few
  dozen lights, but it would have to be rewritten for mesh lights. The flat
  scan is kept as a test oracle (D9).
- A BVH4/wide tree. Fewer levels, but 4-way importance ratios complicate the
  monotone warp and the trail. pbrt-v4 and Cycles use binary trees.

### D2. Conservative bounds per light: `Light::bounds()`

A new trait method returns `Option<LightBounds>`. `None` at infinity and for a
light that emits nothing. The fields:
- `bounds`: a world box.
- `w`: an axis.
- `cos_theta_o`: the spread of the emitting normals about `w`.
- `cos_theta_e`: the emission spread beyond them.
- `phi`: the flux of an unshaped emitter as bright as the light is in its
  brightest direction (`Emissive::peak_flux`). It is not `Light::power`. The
  cone is already in `θe`, so a cone-integrated flux would penalise a narrow
  spot twice. Inside its pool it would be ranked like a dim light and then
  weighted hugely; the first `many_lights` renders showed exactly those
  fireflies. pbrt-v4 gives a spotlight the flux of an isotropic light of the
  same peak intensity for the same reason. The defensive power arm keeps
  `Light::power`.
- `two_sided`.

For `AreaLight`:

| shape / emissive | `w` | `θo` | `θe` |
|---|---|---|---|
| rect, disk; one-sided | world normal | 0 | π/2 |
| rect, disk; two-sided | world normal | π | π/2 |
| sphere, tube | any | π | π/2 |
| shaped (any shape) | `Shaping::axis()` | 0 | min(cone angle, π/2) |

The shaped row is valid because `Shaping` zeroes every direction farther than
its cone angle from its axis, whatever the shape. Focus and IES never zero a
direction inside the cone, so they leave `θe` alone. The world box comes from
the shape's placement: the eight transformed corners of the local box.

The bounds must contain every point the light can emit from and every
direction it emits into. A bound too tight is the one way this change can
introduce bias, and only when the defensive share is zero (D5).

### D3. Build: binned SAOH, deterministic

The build is a binned surface-area-orientation heuristic (Conty & Kulla
2018, as pbrt-v4): 12 buckets on each axis of the centroid box, with a cost
of `phi · M_Ω · surface area`, regularised by the box's aspect ratio. Ties go
to the lower axis, then the lower bucket. Lights enter in list order. The tree
is therefore a pure function of the light list, which the spec's determinism
requirement needs. The build runs in `LightList::select_by(Tree)`, single-
threaded. With N ≤ a few thousand it is far below a millisecond.

### D4. Importance: Conty & Kulla, pbrt-v4's form, with `|n·ω|`

`importance(p, n, node)` follows pbrt-v4's `LightBounds::Importance`:
- `d²` is clamped to at least half the box's diagonal length, so a receiver
  inside the box does not divide by a near-zero distance.
- The angle between the box's direction and `w` is reduced by `θo` and the
  box's subtended angle `θb`. The result is 0 beyond `θe`.
- `cos θ'_i` is reduced by `θb`, and how it is taken depends on the
  receiver. At an **opaque** receiver it is the signed `n·ω`, with `n` facing
  the side the ray arrived from, so a node wholly below the horizon gets zero.
  At a receiver whose material may transmit it is `|n·ω|`, pbrt-v4's form, so
  a transmissive receiver still sees lights behind it. When `n` is zero the
  term is 1.
- Which receivers are opaque comes from a new `Material::transmits()`. It is
  known when the material is built, never per hit, and conservative: the
  default is `true`. It answers `false` for:
  - an `OpenPBR` with no transmission weight and no thin-walled subsurface;
  - a MaterialX material whose every leaf is reflection-only (diffuse,
    conductor, sheen, or a dielectric/Schlick in `R` mode);
  - the subsurface exit's Lambertian.

  This was decided during apply. pbrt-v4's `|n·ω|` alone cannot cut the
  15.1% of ALab's picks that land below the horizon, and a signed form
  everywhere would starve glass of light from behind.
- All the trigonometry is in cosines and sines, with no `acos`, as pbrt does.
- Plain `f32`, no `mul_add`, one `#[inline]` function. Every caller then
  computes the same bits for the same `(p, n, node)`.

### D5. The pick: `(1 − D) · p_tree + D · p_power` within the finite share

Infinite lights keep `1/live` each, as under `power`. The finite share
`finite/live` is split as `(1 − D) · p_tree(k | p, n) + D · p_powerfinite(k)`.
`p_powerfinite` is the existing `power` table's finite part, renormalised, so
it already contains the even half.

- **NEE.** The first `D` of the finite slice of `u` inverts the power CDF.
  The rest walks the tree.
- **The pmf.** On both sides it is the mixture of the two probabilities.

Under the mixture, no emitting finite light falls below `D · p_power`. So its
NEE weight is at most `1/D` times what `power` gives it, the same way the even
half of `power` bounds it against `uniform`.

`D` is chosen from {0.1, 0.25, 0.5} by measurement (task 4.2), starting at
0.25. pbrt-v4 has no floor. Every pure form crust has measured, `power` and
`learned` alike, fireflied without one.

Alternative: no floor. Then a tight but valid bound gives a zero pmf only where
the light truly cannot contribute, and the bounce side keeps such paths at full
weight, so it is unbiased too. But bump and normal-mapped shading make "cannot
contribute" approximate, and a light the bound wrongly rates low is a firefly.

### D6. Traversal preserves stratification

At a node, `pL = I_L / (I_L + I_R)`. Then `u < pL` goes left with
`u' = u / pL`, and otherwise right with `u' = (u − pL)/(1 − pL)`. `u'` is
clamped to `ONE_MINUS_EPSILON`, and a node where both importances are zero
returns no pick. Leaves are in a fixed order and each step is monotone in `u`,
so the slice of the pick dimension that picks light *k* stays contiguous at any
`(p, n)`. The "Stratified picks" scenario then holds under `tree`.

The pmf is the product of the per-level ratios, accumulated root to leaf.
`pmf_at(p, n, k)` walks the same nodes by `k`'s trail and multiplies the same
ratios in the same order, so it reproduces the bits NEE got.

### D7. The normal travels with the vertex

- Every `*_at` lookup takes a `Receiver { p, n, opaque }` instead of `p`.
  `From<Vec3A>` gives a receiver with no normal, so the probes and tests that
  pass a position keep compiling, and that is the volume form.
- Surface NEE passes `rec.normal`, the interpolated normal from before normal
  mapping, which every NEE site has. It faces the side the ray arrived from
  (`set_face_normal`). `opaque` is `!mat.transmits()`.
- Volume NEE passes no normal.
- `PrevBounce` gains the same `Receiver` in place of `pos`.
  `PrevVertex::Phase` uses a receiver with no normal.
- `nee_picks_from` / `bounce_weighs_from` compare the receiver's bits.
- `learned`, `neural`, `power` and `uniform` ignore `n`, which is how they
  stay bit-identical.

Alternative: the shading normal after normal mapping. It is closer to what the
BSDF sees, but it lives in the vertex's `ShadingPoint`, and the bounce side
would need it stored too. The geometric normal (`front_face` side) loses
smooth-shading information. `rec.normal` is the one value both sides have
without new plumbing.

### D8. Degenerate lists skip the tree

With at most one emitting finite light, `select_by(Tree)` builds no tree and
leaves the `power` table in place. The spec's "matches power" requirement then
holds by construction.

### D9. Tests that pin the contract

- **A flat oracle.** For random `(p, n)`, the tree's pmf over every light
  sums to 1 within 1e-5. The per-light importance at the leaves matches a
  flat evaluation of the same function.
- **Bits.** `pick_index_at` and `pmf_at` return the same `f32` bits for the
  picked light, over random `(p, n, u)`.
- **Monotone picks.** Over a sweep of `u`, the picked leaf index never
  decreases.
- **Bounds contain the light.** For each shape, its sampled points lie in the
  box. Every direction `radiance_toward` lights lies within `θo + θe` of `w`.
- **The strategy-agreement scenario.** Light-only, BSDF-only and MIS agree on
  a lit floor under `tree`, as for `power`.

### D10. The gate decides the default

The gate is `learned-light-selection-by-default`'s D3, so the numbers compare.
`tree` becomes the default only if, on every checked-in sample, `veach_mis`,
the OpenPBR Shader Playground, ALab and `samples/many_lights.usda`, its
equal-time relMSE is no more than 5% worse than `power`'s, on both the full
and the trimmed measure. Each scene uses 4 seeds and `--indirect-clamp 0`.

`scripts/bench_ab.sh` compares two binaries, so the two sides are one binary
behind two wrapper scripts, each adding its `--light-selection`.

If a scene fails:
- `tree` ships opt-in;
- the design record names the failing scenes.

**Amended during apply: this change does not flip the default.** ALab and the
OpenPBR Shader Playground were not available where it was built. ALab is
where the tree's case is weakest: occlusion is 52% of its NEE waste, which no
bound sees. So the gate runs on every checked-in sample, its table is
recorded, `tree` ships opt-in whatever it says, and the flip is a follow-up
made once those two scenes are measured. Of the 39 samples, 28 have at most
one finite light (one, `materialx_emissive`, has none) and render under `tree`
bit for bit as under `power` (D8), so they pass by construction; the other 11
are measured.

### D11. A many-lights sample

`samples/many_lights.usda` is a floor and back wall with low occluders, lit by
256 small lights:
- a grid of sphere lights at mixed heights;
- a row of one-sided rect panels, half facing the floor and half facing the
  ceiling;
- a few shaped spots with narrow cones;
- flux spread over two orders of magnitude.

It renders in seconds at 16 spp, so it can join `bench_scenes.sh` and the
integration tests. It exercises every column of D2.

## Risks / Trade-offs

- [Each pick and each bounce-side weight costs about 2·depth importance
  evaluations, roughly 6 at 7 lights and 12 at ALab's 47] → The gate is
  measured at equal time. Callgrind counts on `cornellbox` and `usdlux` at 2 spp
  go in the design record. D8 skips the walk where it cannot help.
- [Small light counts gain little] → Expected. The 2–3-light samples can only
  tie, and the gate allows ±5%. The tree's case rests on `usdlux`, ALab and
  `many_lights`.
- [A normal on `PrevBounce` grows the path state] → 16 bytes (`Vec3A`) on a
  struct that already carries a `(Ray, HitRecord, …)` check tuple. Measured by
  callgrind with the other costs. `wavefront-integrator`
  carries `PrevVertex` between iterations and will inherit the field. Its
  design needs a note, not a change.
- [A bound that is wrong (too tight) under-samples a light] → The defensive
  share D keeps it unbiased, and the "bounds contain the light" tests (D9)
  catch it per shape.
- [A shading normal far from `rec.normal` makes the horizon term wrong] →
  The `|n·ω|` form and the defensive share bound the cost to noise.
- [Default flip changes every multi-light render's noise] → Accepted.
  `--light-selection power` stays bit-identical, and the release notes and the
  site say so.
- [ALab's dominant loss is occlusion, which the tree cannot see] → Known. The
  tree's case there is only the 29.5% backfacing and 15.1% below-horizon
  picks. `learned` over the tree is the follow-up.

## Migration Plan

Users who need yesterday's images pass `--light-selection power` or author
`crust:lightSelection = "power"`. The site's command-line and render-settings
pages and the release notes say so. Rollback is a one-line default change in
`tracer/settings.rs` and `scene/usd_import/settings.rs`.

After this change archives, `learned-light-selection-by-default` resumes
through `/opsx:update` (its proposal's on-hold note lists what changes).

## Open Questions

- Whether `many_lights.usda` should also author a dome, to exercise the
  infinite share next to the tree. Deciding it while writing the sample changes
  nothing above.
