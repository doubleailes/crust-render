# lighting Specification

## Purpose

Define the scene's light sources, how the integrator samples them for next
event estimation, and how UsdLux prims map onto them. Lives in
`crust-core/src/light/`, `lux.rs`, `light_cache.rs` and `environment.rs`, with
import in `scene/usd_import/lights.rs`. The reasoning and measurements behind
these requirements are in `design.md`.

## Requirements

### Requirement: One density for both MIS strategies

For any light and any point on it, NEE and the BSDF bounce side SHALL compute
the same sampling density (the light's own pdf times its selection probability,
times the number of light samples NEE took at that vertex) and the same
radiance. Where a density is not finite (edge-on, degenerate) the point SHALL be
refused on both sides rather than given a finite stand-in.

#### Scenario: A bounce ray hits a light NEE could also have sampled

- **WHEN** a BSDF-sampled ray hits the emitting surface of a light in the list
- **THEN** its emission is MIS-weighted against the density NEE would have used
  for that same point, including the number of light samples taken at the
  vertex the ray left

#### Scenario: Several light samples keep the strategies consistent

- **WHEN** a diffuse floor lit by a sphere light, a rect light and a dome is
  rendered with `--light-samples 4 --light-samples-indirect 4`
- **THEN** the light-only, BSDF-only and power-MIS estimates of the floor agree
  within noise

### Requirement: Area lights

Rect, sphere, disk and cylinder lights SHALL be one-sided `AreaLight`s whose
geometry is also attached to the world. Sphere lights SHALL sample the cone
they subtend and rect lights the spherical rectangle they subtend where that
density is well conditioned; disk and cylinder lights SHALL sample by area. A
non-uniform scale SHALL be honoured through an affine shape. A light's source
geometry SHALL be invisible to camera rays by default. An authored
`crust:rayMask` SHALL decide its visibility outright. Otherwise
`crust:light:cameraVisible` SHALL decide it, and otherwise
`primvars:ri:attributes:visibility:camera` (non-zero means visible).

#### Scenario: A light's source geometry in frame

- **WHEN** a camera ray would hit a light's source geometry
- **THEN** the geometry is invisible to it unless the prim opts in with
  `crust:light:cameraVisible = 1`,
  `primvars:ri:attributes:visibility:camera = 1` or an authored `crust:rayMask`

#### Scenario: The crust attribute wins over the RenderMan primvar

- **WHEN** a rect light authors `crust:light:cameraVisible = 0` and
  `primvars:ri:attributes:visibility:camera = 1`
- **THEN** its source geometry is invisible to camera rays

### Requirement: Hidden light sources do not occlude

The source geometry of a rect, sphere, disk or cylinder light that is invisible
to camera rays (see "Area lights") SHALL NOT occlude a shadow ray toward any
light, at surface or volume vertices, with or without shadow linking. It SHALL
NOT be a caster in any shadow-link class.

#### Scenario: Hidden lights do not shadow one another

- **WHEN** two camera-invisible sphere lights stand side by side over a diffuse
  floor, so that from parts of the floor one sphere covers part of the other
- **THEN** the floor's radiance equals the sum of the two single-light renders,
  everywhere, within noise that falls as 1/√N

#### Scenario: What lies behind a hidden light is lit

- **WHEN** a camera-invisible sphere light hangs just below a diffuse ceiling
  lit by a second light
- **THEN** the ceiling patch above the sphere is lit by the second light as if
  the sphere were absent

### Requirement: Rays cross hidden light sources

A non-camera ray that crosses a hidden light source SHALL collect its emission,
weighted as "One density for both MIS strategies" requires for that light. It
SHALL then continue along the same line as if the source were absent, spending
no path depth, recording no vertex, and leaving the previous vertex's MIS record
to whatever the segment reaches next. When one segment crosses several hidden
sources, each SHALL contribute with its own weight.

#### Scenario: Every strategy agrees across hidden lights

- **WHEN** the two-sphere scene is rendered with power-MIS, light sampling alone
  and BSDF sampling alone
- **THEN** the three estimates of the floor agree within noise

#### Scenario: A bounce collects every hidden light it crosses

- **WHEN** a BSDF-sampled ray from the floor crosses the source of one hidden
  light and then reaches a second one
- **THEN** the path collects both emissions, each with its own MIS weight, and
  the light path expression `C.*[LO]` still equals the beauty bit for bit

#### Scenario: Bounce light reaches through a hidden light

- **WHEN** a camera-invisible sphere light hangs just below a lit diffuse
  ceiling, above a diffuse floor
- **THEN** the floor below receives the ceiling patch's bounce light through
  the sphere

### Requirement: Visible and masked light sources stay solid

A light whose source is camera-visible SHALL keep a solid source that occludes
shadow rays and ends the rays that reach it. An authored `crust:rayMask` SHALL
decide a source's ray visibility outright, as "Area lights" states. A source
the mask leaves visible to shadow rays SHALL occlude them.

#### Scenario: A visible lamp still casts a shadow

- **WHEN** a sphere light authors `crust:light:cameraVisible = 1` and stands
  between the floor and a second light
- **THEN** it occludes the second light, as before this change

#### Scenario: Scenes without hidden area lights are unchanged

- **WHEN** a scene's only lights are infinite lights and area lights whose
  sources are camera-visible
- **THEN** the image is bit-identical to the renderer without this change

### Requirement: Infinite lights

`UsdLuxDistantLight` SHALL become a finite-cone distant light and
`UsdLuxDomeLight` an environment light. A dome's lat-long map SHALL be
importance-sampled by luminance × sin θ, and SHALL be oriented as the UsdLux
`DomeLight` schema specifies (the OpenEXR lat-long convention), in the light's
own frame before its prim transform: the top row is +Y, the image centre
(u = ½) faces +Z, u = ¼ faces +X, u = ¾ faces −X and the left and right edges
meet at −Z. The renderer SHALL have no built-in
sky: an escaping ray that no infinite light answers SHALL collect black,
including in a stage with no infinite light at all.

Each infinite light SHALL be either visible or invisible to camera rays, and
visible by default. Camera visibility SHALL be read from
`crust:light:cameraVisible`, or, when that is not authored, from
`primvars:ri:attributes:visibility:camera`. The stage's render setting
`domeLightCameraVisibility` (default true), or `crust:domeLightCameraVisibility`
when both are authored, SHALL hide every infinite light and every backdrop from
camera rays when false, whatever the lights author. A camera ray is a ray
generated by the camera. Rays continuing from any scattering, refraction or
reflection event, including delta ones, are not camera rays. An escaping
camera ray SHALL collect radiance only from camera-visible infinite lights. An
escaping non-camera ray SHALL collect radiance from every infinite light that
illuminates, whatever its camera visibility. Camera visibility SHALL NOT change
light selection, NEE or MIS weights.

A camera-visible infinite light that illuminates nothing (see "Lights linked to
nothing") is a **backdrop**. When a stage has at least one backdrop, an escaping
camera ray SHALL collect the radiance of the backdrops alone, as if they were a
surface at infinity in front of every other infinite light. No ray other than a
camera ray SHALL see a backdrop, and a backdrop SHALL NOT occlude any light for
any other ray.

#### Scenario: A dome light is present

- **WHEN** a stage contains a `DomeLight`
- **THEN** escaping rays read the dome's radiance

#### Scenario: Nothing at infinity

- **WHEN** a stage has no `DomeLight` or `DistantLight`
- **THEN** every escaping ray collects black

#### Scenario: A camera-invisible HDRI

- **WHEN** a `DomeLight` authors `primvars:ri:attributes:visibility:camera = 0`
  and is the only infinite light
- **THEN** camera rays that escape collect black, while surfaces are lit and
  shadowed by the dome and mirror-like surfaces reflect it

#### Scenario: A backdrop in front of the HDRI

- **WHEN** a stage holds an HDRI `DomeLight` (camera-visible or not) and a
  camera-visible backdrop `DomeLight`
- **THEN** the camera sees the backdrop's texture wherever a camera ray escapes,
  never the HDRI or the sum of the two, and every surface's lighting, shadows,
  reflections and refractions come from the HDRI alone

#### Scenario: Sky seen through water

- **WHEN** a camera ray refracts through a transmissive surface and then escapes
  in a stage holding an HDRI dome and a backdrop dome
- **THEN** the escaping ray collects the HDRI, not the backdrop

#### Scenario: Dome camera visibility switched off

- **WHEN** the render settings author `domeLightCameraVisibility = false` over
  a stage with an HDRI dome and a backdrop dome
- **THEN** camera rays that escape collect black, and surfaces are still lit by
  the HDRI

#### Scenario: A lat-long dome is oriented as UsdLux specifies

- **WHEN** an untransformed `DomeLight` carries a lat-long texture of four
  quarter-width column bands, red, green, blue and white from left to right,
  and escaping rays leave horizontally along (+1, 0, +1), (−1, 0, +1),
  (+1, 0, −1) and (−1, 0, −1)
- **THEN** they collect green, blue, red and white respectively (u = ⅜, ⅝, ⅛
  and ⅞)

### Requirement: Light selection

NEE SHALL take `crust:lightSamples` / `--light-samples` light samples at the
camera vertex and `crust:lightSamplesIndirect` / `--light-samples-indirect` at
every later surface and volume vertex (each default 1, from 1 to 1024; a stage
value outside that range is clamped with a warning, a command-line one refused).
The light
of each sample SHALL be chosen by `crust:lightSelection` / `--light-selection`:
`power` (default; infinite lights keep a uniform share, the finite lights split
the rest half evenly and half by flux), `uniform` (bit-identical to the
historical renderer), or `learned` (a visibility-aware per-cell table trained by
a deterministic pre-pass, over the power table). The samples at one vertex SHALL
be stratified, so that each light is chosen close to the count times its
selection probability.

#### Scenario: Uniform selection

- **WHEN** a render runs with `--light-selection uniform`
- **THEN** each of N lights is picked with probability 1/N

#### Scenario: One sample per vertex is unchanged

- **WHEN** a render runs with both sample counts at 1, given or by default
- **THEN** the image is bit-identical to the renderer before sample counts
  existed

#### Scenario: Stratified picks

- **WHEN** a vertex takes 4 light samples among three lights whose selection
  probabilities are 0.5, 0.25 and 0.25
- **THEN** the first light is sampled twice and each other light once

#### Scenario: More samples, same image

- **WHEN** a scene is rendered with `--light-samples 4` and with the default,
  at increasing sample counts with the indirect clamp off
- **THEN** the difference between the two falls as 1/√N, and the direct-light
  noise per pixel sample is lower with 4

### Requirement: UsdLux units and shaping

Light radiance SHALL follow the UsdLux `LightAPI`: `intensity · 2^exposure ·
color` in nits, times the blackbody colour when `enableColorTemperature` is on;
`normalize` divides by the light's world-space area (or the distant light's
angular size factor). `ShapingAPI` focus, cone and IES profiles SHALL scale area
lights' radiance per direction.

#### Scenario: A normalized rect light is scaled up

- **WHEN** a rect light with `inputs:normalize = 1` is scaled to twice its area
- **THEN** its total emitted power is unchanged

### Requirement: Light linking

A light SHALL illuminate only the geometry that is a member of its
`collection:lightLink`, resolved as the `usd-scene-import` capability's "Light
collection membership" requirement states. For a non-member receiver, the light's
contribution SHALL be zero on every strategy that could deliver it: NEE at surface
and volume vertices, emission reached by a BSDF- or phase-sampled bounce (judged
against the vertex the bounce left), and an escaped ray's infinite-light radiance.
For member receivers, the light's contribution and MIS weights SHALL be unchanged.

#### Scenario: An unlinked receiver is dark

- **WHEN** a sphere light's `collection:lightLink` sets `includeRoot = 0` and
  includes only `/World/Hero`
- **THEN** `/World/Hero` is lit by it, and a floor lit by no other light renders
  black, both directly and in its BSDF-sampled bounces toward the light

#### Scenario: An excluded prim ignores a dome

- **WHEN** a `DomeLight`'s `collection:lightLink` excludes `/World/Set`
- **THEN** rays escaping from `/World/Set` collect no radiance from that dome,
  while every other prim still sees it

### Requirement: Shadow linking

A shadow ray toward a light SHALL be blocked or attenuated only by geometry that
is a member of that light's `collection:shadowLink`, including volume
transmittance along the ray. For a light whose shadow set is restricted,
bounce-side emission at non-delta vertices SHALL NOT be collected (NEE carries
the light alone there), so the estimator stays unbiased. At delta vertices, the
bounce SHALL keep full weight and see the light through the real occluders.
If a light's shadow set cannot be encoded, the light SHALL be refused with a
warning and shadowed by every occluder, including geometry whose class the
renderer could not encode individually.

#### Scenario: An excluded occluder casts no shadow

- **WHEN** a rect light's `collection:shadowLink` excludes `/World/Hair`
- **THEN** the region `/World/Hair` would shadow from that light is lit as if the
  hair were absent, while `/World/Hair` still shadows every other light and is
  still visible to the camera

#### Scenario: A mirror still shows the physical shadow

- **WHEN** the same light and excluded occluder are seen through a perfect mirror
- **THEN** the mirrored view shows the occluder's shadow (a documented gap)

### Requirement: Unlinked scenes are unchanged

When no light authors a non-default `lightLink` or `shadowLink` (UsdLux's fallback
`includeRoot = 1` with no `includes` / `excludes`), the rendered image SHALL be
bit-identical to the renderer without linking support.

#### Scenario: Golden images hold

- **WHEN** `scripts/check_images.sh check` runs over the checked-in samples, none
  of which author links
- **THEN** it reports no difference

#### Scenario: Authored high ray-mask bits without links

- **WHEN** a prim authors a `crust:rayMask` with bits 3–31 set and no light
  authors a restricted `lightLink` or `shadowLink`
- **THEN** its geometry mask is used exactly as authored, and the image is
  bit-identical to the renderer without linking support

### Requirement: Lights linked to nothing

A light whose `collection:lightLink` covers no geometry SHALL illuminate nothing,
as the `usd-scene-import` capability's "Light schema mapping" requirement
defines. It SHALL NOT be chosen by light selection. It SHALL contribute nothing
to NEE at surface or volume vertices, and nothing to emission reached by a BSDF-
or phase-sampled bounce or by an escaped non-camera ray. Only camera visibility
remains, as "Infinite lights" states. Removing such a light SHALL leave the
selection probabilities and MIS weights of every other light as if it had never
been authored.

#### Scenario: A backdrop dome casts no light

- **WHEN** a stage holds an HDRI `DomeLight` and a second `DomeLight` whose
  `collection:lightLink` excludes the root of every geometry prim
- **THEN** every surface is lit exactly as if the second dome were deactivated

#### Scenario: A rect light linked to nothing

- **WHEN** a `RectLight` sets `collection:lightLink:includeRoot = 0` and
  includes nothing
- **THEN** it lights no surface, and a surface lit by no other light renders
  black both directly and through its BSDF-sampled bounces

### Requirement: Unauthored visibility is unchanged

When no light authors `primvars:ri:attributes:visibility:camera` or a
`collection:lightLink` that covers no geometry, the render settings do not
author `domeLightCameraVisibility = false`, and the stage has at least one
infinite light, the rendered image SHALL be bit-identical to the renderer
without this capability.

#### Scenario: Golden images hold

- **WHEN** `scripts/check_images.sh check` runs over the checked-in samples
  that have a `DomeLight` or `DistantLight`, none of which author these
  attributes
- **THEN** it reports no difference, and only the samples with no infinite
  light (which lost the built-in sky) change

### Requirement: Known gaps

The following SHALL be documented as unsupported: mesh lights, portal lights,
light filters, `ShadowAPI`, `inputs:diffuse` / `inputs:specular` (warned and
ignored), shaping on distant and dome lights, cone-aware sampling of shaped
lights, and emissive MaterialX surfaces, curves, instances and volumes as
light-list entries (they are found by BSDF sampling only). For light and shadow
linking, the following SHALL be documented: membership inside an instance
prototype is not distinguishable per instance, `membershipExpression` is refused,
and shadow-linked lights are NEE-only at non-delta vertices and physically
shadowed through delta ones. Camera visibility SHALL be documented as the only
per-light ray visibility read: RenderMan's other `visibility:*` primvars
(`indirect`, `transmission`) are not read.

#### Scenario: An emissive MaterialX surface

- **WHEN** a surface's MaterialX graph emits light
- **THEN** it contributes only through BSDF-sampled bounces, never through NEE

#### Scenario: A pattern-based collection

- **WHEN** a light's `collection:lightLink` authors `membershipExpression`
- **THEN** a warning is logged and the collection is read as the UsdLux default
  (every prim)
