## ADDED Requirements

### Requirement: Unauthored UsdLux inputs take the schema's fallback

A UsdLux input that a light does not author, or authors as blocked, SHALL take the
fallback that the UsdLux schema declares for that light's type, including a type's
override of a `LightAPI` fallback. `inputs:intensity` is 50000 on a `DistantLight`
and 1 on every other light. A non-finite authored value SHALL take the same fallback
with a warning. The light listing SHALL report the same values.

#### Scenario: An unauthored sun is the schema's sun

- **WHEN** a `DistantLight` authors no `inputs:intensity`
- **THEN** it imports exactly as one authoring `inputs:intensity = 50000`, and
  `crust ls light --json` reports its intensity as 50000

#### Scenario: A blocked or non-finite sun intensity

- **WHEN** a `DistantLight` authors `inputs:intensity = None` (blocked), or
  `inputs:intensity = inf`
- **THEN** it imports as one authoring `inputs:intensity = 50000`, and only the
  non-finite one raises a `light.non_finite_input` warning

#### Scenario: Every light type's fallbacks are the schema's

- **WHEN** a light of each type (sphere, disk, cylinder, rect, distant, dome) authors
  no inputs, and a twin authors every input crust reads at its schema fallback (for
  a shaped twin, with `ShapingAPI` applied on both)
- **THEN** the two import as the same light: the same radiance, directions and
  densities for every light sample

## MODIFIED Requirements

### Requirement: Known gaps

The following SHALL be documented as unsupported: mesh lights, portal lights,
light filters, `ShadowAPI`, `inputs:diffuse` / `inputs:specular` (warned and
ignored), shaping on distant and dome lights, cone-aware sampling of shaped
lights, `DomeLight_1`'s `poleAxis` (not read: the dome's pole stays on the
light's +Y, where the schema's fallback `"scene"` puts it on the stage's up
axis), and emissive MaterialX surfaces, curves, instances and volumes as
light-list entries (they are found by BSDF sampling only). For light and shadow
linking, the following SHALL be documented: membership inside an instance
prototype is not distinguishable per instance, `membershipExpression` is refused,
shadow-linked dome lights are NEE-only at non-delta vertices (noisier on glossy
receivers), and every shadow-linked light is physically shadowed through delta
vertices. Camera visibility SHALL be documented as the only per-light ray
visibility read: RenderMan's other `visibility:*` primvars (`indirect`,
`transmission`) are not read.

#### Scenario: An emissive MaterialX surface

- **WHEN** a surface's MaterialX graph emits light
- **THEN** it contributes only through BSDF-sampled bounces, never through NEE

#### Scenario: A pattern-based collection

- **WHEN** a light's `collection:lightLink` authors `membershipExpression`
- **THEN** a warning is logged and the collection is read as the UsdLux default
  (every prim)

#### Scenario: A Z-up dome

- **WHEN** a `DomeLight_1` on a stage with `upAxis = "Z"` authors no `poleAxis`
- **THEN** its lat-long texture is oriented with its pole on the light's +Y, as a
  `DomeLight`'s is, not on the stage's +Z
