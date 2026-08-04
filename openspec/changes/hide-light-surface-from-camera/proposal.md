## Why

A `UsdLuxSphereLight` or `UsdLuxRectLight` currently attaches its emissive
surface to the world with `MASK_ALL`, so primary rays photograph the light
shape. Production renderers do not behave that way: an artist places lights
inside the camera frustum and builds physically impossible rigs precisely
because the renderer is not expected to show the light geometry — that
freedom is one of the reasons CG lighting is not bound by what a real
photographic set could hold. Arnold, RenderMan and Karma all default a
light's camera visibility off and expose re-enabling it as an opt-in.

The opt-out mechanism already exists and silently does nothing on lights.
`crust:rayMask` (bit 0 camera, bit 1 shadow, bit 2 indirect) is read by
`prim_ray_mask` and honored for meshes, spheres, curves and prototype
parts — but `emit_sphere_light` and `emit_rect_light` never call it. They
go through `WorldBuilder::attach`, which hard-codes `MASK_ALL`. So there is
currently **no way at all** to hide a light's surface from any ray
category, and a scene authoring `crust:rayMask` on a light prim is silently
ignored rather than refused.

## What Changes

- A `UsdLuxSphereLight`/`UsdLuxRectLight` emissive surface is attached
  visible to every ray category **except the camera**, instead of to all of
  them. The light still bounces, still occludes, and still carries the
  bounce half of MIS.
- `crust:rayMask` authored on a light prim is now honored verbatim, exactly
  as it already is for geometry prims — so `int crust:rayMask = 7` restores
  the previous behavior and puts the light back in the picture. This closes
  the silent-ignore gap as well as providing the opt-in.
- **BREAKING**: any scene relying on a light's shape being directly
  photographed renders differently — the surface no longer appears to
  primary rays — until it authors `crust:rayMask`. Indirect appearance
  (reflections in a mirror, light bouncing off walls) and shadowing are
  unaffected.
- The default is expressed as "all categories except camera" rather than as
  the literal pair `shadow | indirect`, so a ray category added later
  defaults to visible, matching the "everything unless stated" meaning
  `MASK_ALL` already carries.
- Sample scenes whose framing includes the light author the opt-in, so their
  rendered output is unchanged.

Explicitly **not** changed:

- `UsdLuxDistantLight` and `UsdLuxDomeLight` attach no geometry at all, so
  they have nothing to hide and are untouched. In particular this change
  does **not** address a stage authoring two dome lights both contributing
  to the sky — that is a per-light enable question, not a surface-visibility
  one, and remains open.
- The procedural non-USD fallback scene's lights (`world::simple_scene`)
  stay camera-visible: there is no prim there, hence no attribute to author,
  so hiding them would leave no way to get them back.

## Capabilities

### Modified Capabilities

- `usd-scene-import`: the "Light schema mapping" requirement changes — a
  mapped light's surface is no longer unconditionally visible geometry, and
  the "Ray visibility mask" behavior now extends to light prims with a
  different default than geometry prims.

## Impact

- `crates/crust-core/src/scene/usd_import.rs`: new `light_ray_mask` helper;
  `emit_sphere_light` and `emit_rect_light` take a mask and use
  `WorldBuilder::attach_masked` instead of `attach`.
- `samples/`: new `light_visibility.usda` demonstrating both behaviors;
  `crust:rayMask = 7` authored on the lights of the scenes that frame them.
- `crates/crust-core/tests/usd_scene.rs`: new test covering the default, the
  opt-in, and — closing a workspace-wide coverage gap — `MASK_INDIRECT`.
- Docs asserting "a light is both light and visible object" (`CLAUDE.md`,
  `README.md`, rustdoc on `AreaLight` and `Scene`).

## Risk

The one real hazard is masking off more than the camera bit. `AreaLight`
records the `geom_id` of its geometry, and `LightList::find_by_geom` feeds
`bounce_emission_weight` — the bounce half of MIS. Hiding light geometry
from `MASK_INDIRECT` would delete that half while NEE keeps its
down-weighted `light_weight` factor, losing energy in exactly the regime
`samples/veach_mis.usda` exists to expose, and punching a sky-gradient hole
where the light was (`AreaLight` implements no `Light::escaped`). The
camera bit alone is safe: the direct-view term is the "no previous vertex"
branch, which no MIS weight depends on.
