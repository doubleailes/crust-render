## Why

A light's source geometry in crust is a solid emitter even when the camera cannot
see it. `light_ray_mask` (`usd_import/attrs.rs`) gives every area light's
geometry `MASK_SHADOW | MASK_INDIRECT`, and adds `MASK_CAMERA` only when the
light opts into camera visibility. Its doc comment justifies this as the
industry default (other renderers' "shadow and indirect rays still see it").
The OpenUSD reference delegate does the opposite (below).

The consequence was measured on `samples/veach_mis_portable.usda` (branch
`claude/veach-mis-portable`), whose four
sphere lights sit in a row and are not camera-visible. Without lights occluding
one another, the four single-light renders would sum to the four-light render.
In crust they do not. Its four-light render is darker than the sum by up to 31% (G) at
the wall's right edge and 3% (B) at its left. That is the geometric signature of
the big sphere shadowing its neighbour, and the reverse. The gap is 1.0000 in the
middle of the wall, and it does not depend on light selection or MIS strategy.

NVIDIA's Typhoon (hdEmbree on OpenUSD `typhoon/main`, 70c45e8) builds geometry for a light only when the light is
`visibleInPrimaryRay`, which defaults to false (`delegate/light.cpp`). Only then
does a shadow ray that hits the light's geometry count as occluded
(`integrator/visibility.cpp`). A light the camera cannot see is there an emitter
and nothing else: it casts no shadow on other lights' receivers.

## What Changes

- **A camera-invisible light's source is a transparent emitter.** For a rect,
  sphere, disk or cylinder light whose geometry is invisible to camera rays (the
  default), that geometry SHALL NOT occlude shadow rays toward any light. A
  bounce ray that crosses it SHALL collect its emission, MIS-weighted exactly as
  today, and continue along the same line as if the source were absent. It
  spends no depth and records no vertex. NEE and bounce therefore agree that
  lights do not occlude one another.
- **Camera-visible light sources are unchanged.** A source that opts into camera
  visibility (`crust:light:cameraVisible = 1`,
  `primvars:ri:attributes:visibility:camera = 1`) stays a solid emitter that
  occludes, as Typhoon's visible light geometry does.
- **`crust:rayMask` still wins outright.** An authored mask keeps exactly
  today's behaviour, which is the escape hatch for a scene that wants solid
  hidden lights.
- **BREAKING for every scene with camera-invisible area lights.** The largest
  change is where hidden lights see one another (veach_mis: up to 31%). Even a
  scene with a single hidden light changes, usually slightly: a bounce ray that
  reaches the light now also reaches whatever is behind it, and surfaces the
  light used to shadow from other lights are lit. Scenes whose only lights are
  infinite lights or camera-visible sources render bit-identically.
  `scripts/check_images.sh` lists the samples that move.
- The misleading doc comment on `light_ray_mask` is corrected, and the
  `lighting` design record and the user documentation say what a hidden light
  is.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `lighting`: three new requirements, "Hidden light sources do not occlude",
  "Rays cross hidden light sources" and "Visible and masked light sources stay
  solid".

## Impact

- `crates/crust-core/src/scene/usd_import/attrs.rs`: `light_ray_mask` drops
  `MASK_SHADOW` from a camera-invisible source.
- `crates/crust-core/src/scene/usd_import/light_links.rs`: light sources are
  not shadow casters, so they are never given a caster class.
- `crates/crust-core/src/tracer/path.rs`: after the closest hit (and after
  `pass_cutouts`), a hit on a transparent emitter adds its weighted emission
  and restarts the segment past it, bounded like the cutout walk. The vertex
  record must hold the emission of every emitter crossed on one segment (design
  D2), and the LPE routing must emit one `L` event per crossed emitter.
- `crates/crust-core/src/light/` / `material/emissive.rs`: a way to tell a
  transparent emitter from a solid one at a hit (design D3).
- `docs/architecture.md` § Invariants: the NEE ↔ bounce pair gains the rule.
- Performance: shadow rays no longer test hidden light geometry, which is
  slightly faster. A bounce ray that hits a hidden light now traces one more
  segment past it, which is rare and bounded.
