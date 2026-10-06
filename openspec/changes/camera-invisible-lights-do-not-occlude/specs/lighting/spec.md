## ADDED Requirements

### Requirement: Light sources hidden from the camera are transparent emitters

The source geometry of a rect, sphere, disk or cylinder light that is invisible
to camera rays (see "Area lights") SHALL be a transparent emitter. It SHALL NOT
occlude a shadow ray toward any light, at surface or volume vertices, with or
without shadow linking. It SHALL NOT be a caster in any shadow-link class. A
non-camera ray that crosses it SHALL collect its emission, weighted as "One
density for both MIS strategies" requires for that light, and SHALL continue
along the same line as if the source were absent: the crossing spends no path
depth, records no vertex, and leaves the previous vertex's MIS record to every
emitter and surface the segment goes on to reach. When one segment crosses
several such sources, each SHALL contribute with its own weight. A light whose
source is camera-visible SHALL keep a solid source that occludes. An authored
`crust:rayMask` SHALL decide a source's ray visibility outright, as "Area lights"
states, and a source it leaves visible to shadow rays SHALL occlude them.

#### Scenario: Hidden lights do not shadow one another

- **WHEN** two camera-invisible sphere lights stand side by side over a diffuse
  floor, so that from parts of the floor one sphere covers part of the other
- **THEN** the floor's radiance equals the sum of the two single-light renders,
  everywhere, within noise that falls as 1/√N

#### Scenario: Every strategy agrees across hidden lights

- **WHEN** the scene above is rendered with power-MIS, light sampling alone and
  BSDF sampling alone
- **THEN** the three estimates of the floor agree within noise

#### Scenario: A bounce collects every hidden light it crosses

- **WHEN** a BSDF-sampled ray from the floor crosses the source of one hidden
  light and then reaches a second one
- **THEN** the path collects both emissions, each with its own MIS weight, and
  the light path expression `C.*[LO]` still equals the beauty bit for bit

#### Scenario: A visible lamp still casts a shadow

- **WHEN** a sphere light authors `crust:light:cameraVisible = 1` and stands
  between the floor and a second light
- **THEN** it occludes the second light, as before this requirement

#### Scenario: What lies behind a hidden light is reached

- **WHEN** a camera-invisible sphere light hangs just below a diffuse ceiling
  lit by a second light
- **THEN** the ceiling patch above the sphere is lit by the second light as if
  the sphere were absent, and the floor below receives that patch's bounce
  light through the sphere

#### Scenario: Scenes without hidden area lights are unchanged

- **WHEN** a scene's only lights are infinite lights and area lights whose
  sources are camera-visible
- **THEN** the image is bit-identical to the renderer without this requirement
