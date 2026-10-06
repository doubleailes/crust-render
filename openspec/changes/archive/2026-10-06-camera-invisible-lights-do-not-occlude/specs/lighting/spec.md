## ADDED Requirements

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
