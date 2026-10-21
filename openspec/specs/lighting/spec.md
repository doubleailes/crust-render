# lighting Specification

## Purpose

Define the scene's light sources, how the integrator samples them for next
event estimation, and how UsdLux prims map onto them. Lives in
`crust-core/src/light/`, `lux.rs`, `tracer/light_cache.rs`, `tracer/light_visibility.rs` and `environment.rs`, with
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
density is well conditioned. Disk lights SHALL sample by area by default; with
`CRUST_DISK_SAMPLING = ellipse` they SHALL sample the spherical ellipse they
subtend from a shading point strictly in front of their emitting side, where that
solid angle is well conditioned, and by area otherwise. Cylinder lights with a
one-sided emitter SHALL sample, from a shading point outside the tube, only the
part of their wall that faces it. By default (`CRUST_TUBE_SAMPLING = equiangular`)
they SHALL draw the axial position along the sampled wall line by a density that
follows its inverse-square falloff. They SHALL sample by area from inside the tube
or with a two-sided emitter. `CRUST_DISK_SAMPLING = area` and
`CRUST_TUBE_SAMPLING = area` SHALL restore area sampling for the respective shape. A non-uniform scale SHALL be honoured through an
affine shape, by every strategy. A light's source geometry SHALL be invisible to
camera rays by default. An authored `crust:rayMask` SHALL decide its visibility
outright. Otherwise `crust:light:cameraVisible` SHALL decide it, and otherwise
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

#### Scenario: Disk and cylinder sampling is unbiased

- **WHEN** a diffuse floor lit by a disk light and a cylinder light, each under a
  non-uniform scale, is rendered light-only, BSDF-only and with power MIS, under
  every value of `CRUST_DISK_SAMPLING` and `CRUST_TUBE_SAMPLING`
- **THEN** all the estimates of the floor agree within noise that falls as 1/√N

#### Scenario: A tube's light samples face the shading point

- **WHEN** a one-sided cylinder light is sampled from a point outside the tube
- **THEN** every sample lies on the part of the wall that faces the point, and none
  carries zero radiance for facing away

#### Scenario: A two-sided tube seen through its open end

- **WHEN** a cylinder light with a two-sided emitter is seen from a point on its
  axis beyond one open end, so that its inner wall is visible
- **THEN** it is sampled by area, and its light-only and BSDF-only estimates agree
  within noise

#### Scenario: A disk seen from behind

- **WHEN** a disk light is sampled, under `CRUST_DISK_SAMPLING = ellipse`, from a
  point behind its emitting side
- **THEN** it is sampled by area, as before this requirement

#### Scenario: Area sampling restored

- **WHEN** a stage with disk and cylinder lights is rendered with
  `CRUST_DISK_SAMPLING=area CRUST_TUBE_SAMPLING=area`
- **THEN** the image is bit-identical to the same render before this requirement

#### Scenario: Scenes without round lights are unchanged

- **WHEN** a stage with no disk or cylinder light is rendered at 16 spp
- **THEN** the image is bit-identical to the same render before this requirement

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

`UsdLuxDistantLight` SHALL become a finite-cone distant light, and
`UsdLuxDomeLight` and `UsdLuxDomeLight_1` an environment light. A
`DomeLight_1` SHALL read every input a `DomeLight` reads, with the same
meaning, and SHALL be listed and counted as a light wherever a `DomeLight` is.
A dome's lat-long map SHALL be importance-sampled by luminance × sin θ, and
SHALL be oriented as the UsdLux schema specifies (the OpenEXR lat-long
convention), in the dome's own frame before its prim transform: the top row is
+Y, the image centre (u = ½) faces +Z, u = ¼ faces +X, u = ¾ faces −X and the
left and right edges meet at −Z.

A `DomeLight_1` SHALL first turn that frame onto its pole axis. When its
`poleAxis` is `Z`, or is `scene` (the schema's fallback) on a stage whose root
layer authors `upAxis = "Z"`, the frame SHALL be rotated +90° about X before
the prim transform: the top row faces +Z, the image centre faces −Y, u = ¼
faces +X and u = ¾ faces −X. When its `poleAxis` is `Y`, or `scene` on a stage
whose `upAxis` is `Y` or unauthored, the frame SHALL be left as it is. The
rotation SHALL orient the dome alone, never its namespace children. A
`DomeLight` SHALL keep its +Y pole whatever the stage's up axis, including when
it authors a `poleAxis` attribute, which its schema does not declare.

The renderer SHALL have no built-in sky: an escaping ray that no infinite light
answers SHALL collect black, including in a stage with no infinite light at
all.

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

#### Scenario: A DomeLight_1 is a dome light

- **WHEN** a stage's only light is a `DomeLight_1` with a uniform colour
- **THEN** escaping rays read that colour, `crust check` counts one light, and
  `crust ls light` lists the prim

#### Scenario: Nothing at infinity

- **WHEN** a stage has no `DomeLight`, `DomeLight_1` or `DistantLight`
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

#### Scenario: A DomeLight_1 on a Z-up stage turns its pole onto +Z

- **WHEN** an untransformed `DomeLight_1` with `poleAxis` unauthored (so
  `scene`) carries the same four-band texture on a stage authoring
  `upAxis = "Z"`, and escaping rays leave horizontally along (+1, −1, 0),
  (−1, −1, 0), (+1, +1, 0) and (−1, +1, 0)
- **THEN** they collect green, blue, red and white respectively, and a ray
  leaving along +Z collects the texture's top row

#### Scenario: poleAxis Z turns the pole on a Y-up stage too

- **WHEN** an untransformed `DomeLight_1` authoring `poleAxis = "Z"` carries the
  four-band texture on a stage authoring `upAxis = "Y"`
- **THEN** rays along (+1, −1, 0), (−1, −1, 0), (+1, +1, 0) and (−1, +1, 0)
  collect green, blue, red and white, as on a Z-up stage

#### Scenario: A DomeLight_1 whose pole is already +Y is unchanged

- **WHEN** an untransformed `DomeLight_1` carries the four-band texture with
  `poleAxis = "Y"` on a Z-up stage, or with `poleAxis` unauthored on a stage
  authoring `upAxis = "Y"` or no `upAxis`
- **THEN** it renders exactly as a `DomeLight` with the same inputs does

#### Scenario: A DomeLight ignores an authored poleAxis

- **WHEN** a `DomeLight` authors `poleAxis = "scene"` on a stage authoring
  `upAxis = "Z"`
- **THEN** its top row still faces +Y and rays along (+1, 0, +1), (−1, 0, +1),
  (+1, 0, −1) and (−1, 0, −1) collect green, blue, red and white

#### Scenario: The pole rotation composes with the prim's transform

- **WHEN** a `DomeLight_1` on a Z-up stage also authors a rotation of 90° about
  Z on its prim
- **THEN** the prim's rotation applies after the pole rotation: the image
  centre faces +X instead of −Y, and the top row still faces +Z

### Requirement: Light selection

NEE SHALL take `crust:lightSamples` / `--light-samples` light samples at the
camera vertex and `crust:lightSamplesIndirect` / `--light-samples-indirect` at
every later surface and volume vertex (each default 1, from 1 to 1024; a stage
value outside that range is clamped with a warning, a command-line one refused).
The light
of each sample SHALL be chosen by `crust:lightSelection` / `--light-selection`:
`power` (default; infinite lights keep a uniform share, the finite lights split
the rest half evenly and half by flux), `tree` (infinite lights keep a uniform
share, and the finite lights are chosen by walking a light BVH by an importance
bound at the shading point's position and normal, mixed with a defensive share
of the `power` probabilities), `uniform` (bit-identical to the
historical renderer), `learned` (a visibility-aware per-cell table trained by
a deterministic pre-pass, over the power table), or `neural` (the power table
weighted by each light's visibility, predicted by a small network that a
deterministic pre-pass trains). The samples at one vertex SHALL
be stratified, so that each light is chosen close to the count times its
selection probability.

#### Scenario: Uniform selection

- **WHEN** a render runs with `--light-selection uniform`
- **THEN** each of N lights is picked with probability 1/N

#### Scenario: One sample per vertex is unchanged

- **WHEN** a render runs with both sample counts at 1, given or by default,
  under `power` or `uniform`
- **THEN** the image is bit-identical to the renderer before sample counts
  existed under the same selection

#### Scenario: Stratified picks

- **WHEN** a vertex takes 4 light samples among three lights whose selection
  probabilities are 0.5, 0.25 and 0.25
- **THEN** the first light is sampled twice and each other light once

#### Scenario: More samples, same image

- **WHEN** a scene is rendered with `--light-samples 4` and with the default,
  at increasing sample counts with the indirect clamp off
- **THEN** the difference between the two falls as 1/√N, and the direct-light
  noise per pixel sample is lower with 4

#### Scenario: Neural selection is accepted

- **WHEN** a render runs with `--light-selection neural`, or its render settings author `crust:lightSelection = "neural"`
- **THEN** the render completes, and the `--light-selection` help lists `neural` with a one-line description

#### Scenario: Tree selection is accepted

- **WHEN** a render runs with `--light-selection tree`, or its render settings
  author `crust:lightSelection = "tree"`
- **THEN** the render completes, and the `--light-selection` help lists `tree`
  with a one-line description, while `power` stays the default

#### Scenario: Existing modes are unchanged

- **WHEN** a checked-in sample renders at 16 spp under `power`, `uniform`,
  `learned` or `neural`, each given explicitly
- **THEN** the image is bit-identical to the one rendered under the same mode
  before `tree` existed

### Requirement: Neural selection falls back to power

Under `neural`, NEE SHALL pick by the power table wherever the network cannot apply: a scene with fewer than two lights, a pre-pass that finds no receivers, a position outside the trained bounds, or more lights than the network supports. Exceeding the light limit SHALL log one `WARN` naming the limit. The other cases SHALL be silent.

#### Scenario: Too many lights

- **WHEN** a scene with more lights than the neural limit renders with `--light-selection neural`
- **THEN** one `WARN` line names the limit, and the image is bit-identical to the same render under `--light-selection power`

#### Scenario: One light

- **WHEN** a scene with a single light renders with `--light-selection neural`
- **THEN** the image is bit-identical to the same render under `--light-selection power`, and nothing is logged at `WARN`

### Requirement: Neural selection is unbiased

Under `neural`, every light that can emit SHALL keep a pick probability of at least its defensive uniform share at every vertex, whatever visibility the network predicts. A render under `neural` SHALL converge to the same image as under `power`.

#### Scenario: A light the network calls hidden stays sampleable

- **WHEN** the network predicts zero visibility for a light at a vertex
- **THEN** that light's pick probability there is still at least the defensive share divided by the number of emitting lights

#### Scenario: Converges to the power reference

- **WHEN** `usdlux` renders under `neural` at increasing spp with `--indirect-clamp 0`, compared against a 1024 spp `power` reference
- **THEN** relMSE falls as roughly 1/N in spp rather than plateauing

### Requirement: Neural selection weights both MIS sides alike

Under `neural`, the probability that NEE picked a light at a vertex SHALL be exactly the probability the bounce side uses when a BSDF-sampled ray from that same vertex hits that light, or escapes to it.

#### Scenario: Emission is not double-counted

- **WHEN** `veach_mis` renders under `neural` with `--indirect-clamp 0` at 1024 spp
- **THEN** its mean pixel value matches the `power` render's within the noise of either

### Requirement: Neural selection is deterministic

The `neural` pre-pass and training SHALL depend only on the scene and the render settings. The same scene, settings and frame SHALL give bit-identical images under tiled and scanline rendering, and at any rayon thread count.

#### Scenario: Thread count does not change the image

- **WHEN** the same scene renders under `neural` with `RAYON_NUM_THREADS=1` and with the default thread count
- **THEN** the two images are bit-identical

#### Scenario: Tiles and scanlines agree

- **WHEN** the same scene renders under `neural` in tiled and in scanline mode
- **THEN** the two images are bit-identical

### Requirement: Tree selection favours the lights that can reach the receiver

Under `tree`, a finite light's selection probability at a vertex SHALL depend
on its flux, its distance from the vertex, whether the vertex lies within the
directions the light emits into (its front side, and its shaping cone when one
is authored), and, at a surface vertex, whether the light lies above or below
the surface. At a volume vertex the surface terms SHALL be ignored.

#### Scenario: A light facing away is picked less

- **WHEN** a receiver is lit by two equal one-sided rect lights at the same
  distance, one facing it and one facing away, under `--light-selection tree`
- **THEN** the light facing it is picked with higher probability than under
  `power`, and the one facing away with lower

#### Scenario: A near light is picked more than an equal far one

- **WHEN** a receiver is lit by two equal sphere lights, one at 1 m and one at
  10 m, both above its surface, under `--light-selection tree`
- **THEN** the near light is picked with higher probability than the far one

#### Scenario: A light below an opaque receiver is picked less

- **WHEN** an opaque diffuse floor is lit by two equal sphere lights at the
  same distance, one above it and one wholly below it, under
  `--light-selection tree`
- **THEN** the light below has only its defensive share there

#### Scenario: A transmissive receiver still sees lights behind it

- **WHEN** a glass or thin-walled transmissive surface is lit by two equal
  sphere lights at the same distance, one on each side
- **THEN** both are picked with more than their defensive share

#### Scenario: A narrow spotlight is not picked outside its cone

- **WHEN** a receiver lies outside the shaping cone of one of two lights under
  `--light-selection tree`
- **THEN** that light's selection probability there is its defensive share
  alone

#### Scenario: Many lights get cleaner direct lighting

- **WHEN** `samples/many_lights.usda` renders at 16 spp with
  `--indirect-clamp 0` under `tree` and under `power`
- **THEN** the `tree` render's relMSE against a 1024 spp reference is lower

### Requirement: Tree selection is unbiased

Under `tree`, every finite light that emits SHALL keep a selection probability
of at least a fixed defensive fraction of its `power` probability at every
vertex, whatever the importance bound gives it. A render under `tree` SHALL
converge to the same image as under `power`.

#### Scenario: A light behind the receiver stays sampleable

- **WHEN** a light lies entirely below a receiver's surface, so that its
  importance bound there is zero
- **THEN** its selection probability there is still the defensive fraction of
  its `power` probability, and greater than zero

#### Scenario: Converges to the power reference

- **WHEN** `usdlux` and `samples/many_lights.usda` render under `tree` at
  increasing spp with `--indirect-clamp 0`, compared against a 1024 spp `power`
  reference
- **THEN** relMSE falls as roughly 1/N in spp rather than plateauing

### Requirement: Tree selection weights both MIS sides alike

Under `tree`, the probability that NEE picked a light at a vertex SHALL be
exactly the probability the bounce side uses when a BSDF-sampled ray from that
same vertex hits that light, or escapes to it: evaluated at the same position
and the same normal.

#### Scenario: Emission is not double-counted

- **WHEN** `veach_mis` renders under `tree` with `--indirect-clamp 0` at 1024 spp
- **THEN** its mean pixel value matches the `power` render's within the noise
  of either

#### Scenario: Strategies agree on a lit floor

- **WHEN** a diffuse floor lit by several sphere and rect lights and a dome is
  rendered under `tree` with the light-only, BSDF-only and power-MIS strategies
- **THEN** the three estimates of the floor agree within noise

### Requirement: Tree selection is deterministic

The light tree SHALL depend only on the scene's lights. The same scene,
settings and frame SHALL give bit-identical images under `tree` with tiled and
scanline rendering, and at any rayon thread count.

#### Scenario: Thread count does not change the image

- **WHEN** the same scene renders under `tree` with `RAYON_NUM_THREADS=1` and
  with the default thread count
- **THEN** the two images are bit-identical

#### Scenario: Tiles and scanlines agree

- **WHEN** the same scene renders under `tree` in tiled and in scanline mode
- **THEN** the two images are bit-identical

### Requirement: Tree selection with nothing to choose matches power

Under `tree`, a scene with at most one finite light that emits SHALL select
exactly as `power` does.

#### Scenario: One rect light and a dome

- **WHEN** a scene with one rect light and one dome renders under
  `--light-selection tree`
- **THEN** the image is bit-identical to the same render under
  `--light-selection power`

### Requirement: UsdLux units and shaping

Light radiance SHALL follow the UsdLux `LightAPI`: `intensity · 2^exposure ·
color` in nits, times the blackbody colour when `enableColorTemperature` is on;
`normalize` divides by the light's world-space area (or the distant light's
angular size factor). `ShapingAPI` focus, cone and IES profiles SHALL scale area
lights' radiance per direction.

#### Scenario: A normalized rect light is scaled up

- **WHEN** a rect light with `inputs:normalize = 1` is scaled to twice its area
- **THEN** its total emitted power is unchanged

### Requirement: Per-lobe light multipliers

A light's `inputs:diffuse` and `inputs:specular` SHALL scale its contribution
to a surface's diffuse and specular response: `inputs:specular` the specular,
coat and transmission lobes and a fibre's primary reflection (R),
`inputs:diffuse` the diffuse, sheen, subsurface and translucent lobes and a
fibre's other terms (TT, TRT and beyond). Both sides of MIS SHALL apply the
same scaling — NEE toward the light, and the light's emission reached by a
BSDF-sampled bounce, an escaping ray or a link twin — as a factor of the
integrand, never of a weight. Scattering in a volume SHALL take the light whole.
A non-finite value SHALL fall back to 1 with a `light.non_finite_input`
warning. The multipliers SHALL steer no sampling decision.

#### Scenario: The image is linear in the multipliers

- **WHEN** a glossy plastic floor is lit by a sphere light and a distant light
  authoring `inputs:diffuse = d` and `inputs:specular = s`, under the `light`,
  `bsdf` and `power` strategies
- **THEN** each pixel equals, to rounding, `d` times the render at `(1, 0)` plus
  `s` times the render at `(0, 1)`, and the strategies agree on its mean

#### Scenario: A metal answers to the specular multiplier only

- **WHEN** a metallic floor is lit by lights authoring `inputs:diffuse = 1`,
  `inputs:specular = 0`
- **THEN** it renders black

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
transmittance along the ray.

At a non-delta surface or volume vertex, the BSDF-sampled (bounce) estimate of a
light whose shadow set is restricted SHALL see that light through the same
visibility as its shadow rays: blocked or attenuated only by members of its
`collection:shadowLink`, never by the geometry the link excludes. The light-sampled
(NEE) and bounce-sampled estimates of such a light SHALL be combined by the render's
sampling strategy exactly as for an unlinked light. `--strategy light`, `--strategy
bsdf` and the MIS strategies SHALL therefore estimate the same image.

A dome light whose shadow set is restricted is the exception: it SHALL be sampled by
NEE alone at non-delta vertices, and the bounce SHALL NOT collect it there, so the
estimator stays unbiased.

At delta vertices, the bounce SHALL keep full weight and see the light through the
real occluders. If a light's shadow set cannot be encoded, the light SHALL be refused
with a warning and shadowed by every occluder, including geometry whose class the
renderer could not encode individually.

#### Scenario: An excluded occluder casts no shadow

- **WHEN** a rect light's `collection:shadowLink` excludes `/World/Hair`
- **THEN** the region `/World/Hair` would shadow from that light is lit as if the
  hair were absent, while `/World/Hair` still shadows every other light and is
  still visible to the camera

#### Scenario: The three strategies agree on a link that matters

- **WHEN** a glossy floor is lit by a sphere light through an occluder that the
  light's `collection:shadowLink` excludes, and the floor is rendered with
  `--strategy light`, `--strategy bsdf` and `--strategy power` at a fixed sample
  count with adaptive sampling off and `--indirect-clamp 0`
- **THEN** the three images of the floor agree within noise that falls as 1/√N, and
  the region behind the occluder is lit in all three

#### Scenario: MIS is whole again for a restricted light

- **WHEN** the same glossy floor is rendered with `--strategy power` and with
  `--strategy light`, from two independent seeds each
- **THEN** the floor's variance under `power` is below its variance under `light`,
  where today they are identical

#### Scenario: A restricted dome stays NEE-only

- **WHEN** a dome light's `collection:shadowLink` excludes a prim and the scene is
  rendered with `--strategy power` and with `--strategy light`
- **THEN** the dome's contribution is the same estimator under both, as before this
  change

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
light filters, `ShadowAPI`, shaping on distant and dome lights, cone-aware sampling of shaped
lights, and emissive MaterialX surfaces, curves, instances and volumes as
light-list entries (they are found by BSDF sampling only). For light and shadow
linking, the following SHALL be documented: membership inside an instance
prototype is not distinguishable per instance, `membershipExpression` is refused,
shadow-linked dome lights are NEE-only at non-delta vertices (noisier on glossy
receivers), and every shadow-linked light is physically shadowed through delta
vertices. For a light's per-lobe multipliers it SHALL be documented that the
emission a bounce reaches is routed to the light path expressions at the
bounce's blended gain on each of that vertex's lobes. Camera visibility SHALL be documented as the only per-light ray
visibility read: RenderMan's other `visibility:*` primvars (`indirect`,
`transmission`) are not read.

#### Scenario: An emissive MaterialX surface

- **WHEN** a surface's MaterialX graph emits light
- **THEN** it contributes only through BSDF-sampled bounces, never through NEE

#### Scenario: A pattern-based collection

- **WHEN** a light's `collection:lightLink` authors `membershipExpression`
- **THEN** a warning is logged and the collection is read as the UsdLux default
  (every prim)
