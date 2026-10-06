## Why

A thin-walled transmissive surface (a window pane, a lampshade sheet, a soap
film, OpenPBR `geometry_thin_walled` with `transmission_weight > 0`) blocks
every shadow ray in crust. `shadow_transmittance` treats any surface other than
a cutout as an opaque occluder (`tracer/path.rs`, `surface_visibility`). Light
behind such a sheet therefore reaches a diffuse receiver only by a BSDF-sampled
bounce that happens to pass the sheet's delta transmission and then hit the
light. The estimate stays unbiased, because the delta continuation is weighted
1 on the bounce side, but its variance is that of BSDF sampling alone: a small
or distant light seen through a window becomes fireflies.

The OpenUSD reference delegate lets shadow rays through thin walls. In NVIDIA's
Typhoon (hdEmbree on OpenUSD `typhoon/main`, 70c45e8), "Thin-walled transmission
always uses straight RGB shadow attenuation" (README, "Caustics and Transparent
Shadows"). `integrator/visibility.cpp` attenuates the shadow ray by transmission
× the interface Fresnel transmission × the transmission tint at each thin-walled
blocker, combined with presence as `(1 − presence) + presence · T`.

A thin wall does not bend the ray, so its straight transmission is not an
approximation in crust's own model. crust's thin-walled transmission is a delta
lobe in both shading paths: the native OpenPBR window model
(`material/openpbr/transmission.rs`) and the MaterialX closure leaf, which
transmits one interface's `(1 − F)`. Handling it as a pass-through, exactly like
a cutout, is therefore exact.

## What Changes

- **Thin-walled straight transmission is a pass-through.** A surface whose
  material reports a straight transmittance `T` (RGB, a function of the
  crossing direction) is crossed like a cutout:
  - A shadow ray is attenuated by `(1 − opacity) + opacity · T`, the same
    combination Typhoon uses, where the cutout alone gives `1 − opacity`.
  - A path meets the surface with probability `1 − q` and otherwise passes it
    with throughput `T / q`, spending no depth and recording no vertex.
  - When the path meets it, it scatters through the material *minus* its
    straight transmission, with throughput divided by `1 − q`.
- **The expected image is unchanged; the variance drops.** Both strategies now
  see the same RGB visibility through the sheet, so NEE and MIS work through
  windows. Light paths keep their events: a pass is still a specular
  transmission (`TS`) for light path expressions.
- **Worlds without thin-walled transmission are bit-identical**, at identical
  cost, behind a `World::has_straight_transmission()` flag like `has_cutouts()`.
- Not in this change: thick dielectrics (bottles, glass jars).
  Their refraction bends the ray, so a straight shadow is biased. Typhoon offers
  it only with caustics off. It is recorded as a known gap and a possible
  opt-in follow-up.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `rendering`: "Cutout surfaces are stochastic presence" is restated so shadow
  rays are attenuated, not blocked, by thin-walled transmission. Four new
  requirements are added: "Thin walls report their straight transmittance",
  "Shadow rays pass thin walls", "Paths pass thin walls stochastically" and
  "Thin-wall passes keep their meaning".

## Impact

- `crates/crust-core/src/material/`: the `Material` trait gains
  `straight_transmittance(ray, rec) -> Vec3A` (default zero) and
  `has_straight_transmission()`. The scatter path gains a way to sample the
  material without its straight transmission. These are implemented for native
  OpenPBR (`openpbr/transmission.rs`, `lobes.rs`) and for the MaterialX closure
  tree (`material/closure/`), where `T` is the summed tree weight of the
  thin-walled delta-transmission leaves. `Material::resolve` must return the
  same, pinned in `tests/resolve.rs`.
- `crates/crust-core/src/tracer/path.rs`: `surface_visibility` and
  `cutout_through` become RGB. `pass_cutouts` handles straight transmission,
  with a new stratified key `K_THIN`.
- `crates/crust-core/src/light/` (learned light cache): it trains on the
  luminance of the RGB visibility (design D4).
- `docs/architecture.md` § Invariants: `pass_cutouts ↔ cutout_through` becomes
  the pass-through pair, covering thin walls.
- Performance: no change without thin-walled transmission. With it, shadow rays
  walk through sheets (bounded by `MAX_CUTOUT_CROSSINGS`) instead of stopping.
