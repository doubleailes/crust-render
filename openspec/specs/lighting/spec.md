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
the same sampling density (`LightList::density`: the light's own pdf times its
selection probability) and the same radiance (`Emissive::radiance_toward`).
Where a density is not finite (edge-on, degenerate) the point SHALL be refused
on both sides rather than given a finite stand-in.

#### Scenario: A bounce ray hits a light NEE could also have sampled

- **WHEN** a BSDF-sampled ray hits the emitting surface of a light in the list
- **THEN** its emission is MIS-weighted against the density NEE would have used
  for that same point

### Requirement: Area lights

Rect, sphere, disk and cylinder lights SHALL be one-sided `AreaLight`s whose
geometry is also attached to the world. Sphere lights SHALL sample the cone
they subtend and rect lights the spherical rectangle they subtend where that
density is well conditioned; disk and cylinder lights SHALL sample by area. A
non-uniform scale SHALL be honoured through an affine shape.

#### Scenario: A light's source geometry in frame

- **WHEN** a camera ray would hit a light's source geometry
- **THEN** the geometry is invisible to it unless the prim opts in with
  `crust:light:cameraVisible = 1` or an authored `crust:rayMask`

### Requirement: Infinite lights

`UsdLuxDistantLight` SHALL become a finite-cone distant light and
`UsdLuxDomeLight` an environment light that replaces the built-in sky gradient.
A dome's lat-long map SHALL be importance-sampled by luminance × sin θ.

#### Scenario: A dome light is present

- **WHEN** a stage contains a `DomeLight`
- **THEN** escaping rays read the dome's radiance instead of the sky gradient

### Requirement: Light selection

NEE SHALL sample one light per vertex, chosen by `crust:lightSelection` /
`--light-selection`: `power` (default; infinite lights keep a uniform share, the
finite lights split the rest half evenly and half by flux), `uniform`
(bit-identical to the historical renderer), or `learned` (a visibility-aware
per-cell table trained by a deterministic pre-pass, over the power table).

#### Scenario: Uniform selection

- **WHEN** a render runs with `--light-selection uniform`
- **THEN** each of N lights is picked with probability 1/N

### Requirement: UsdLux units and shaping

Light radiance SHALL follow the UsdLux `LightAPI`: `intensity · 2^exposure ·
color` in nits, times the blackbody colour when `enableColorTemperature` is on;
`normalize` divides by the light's world-space area (or the distant light's
angular size factor). `ShapingAPI` focus, cone and IES profiles SHALL scale area
lights' radiance per direction.

#### Scenario: A normalized rect light is scaled up

- **WHEN** a rect light with `inputs:normalize = 1` is scaled to twice its area
- **THEN** its total emitted power is unchanged

### Requirement: Known gaps

The following SHALL be documented as unsupported: mesh lights, portal lights,
light filters, light and shadow linking, `inputs:diffuse` / `inputs:specular`
(warned and ignored), shaping on distant and dome lights, cone-aware sampling
of shaped lights, and emissive MaterialX surfaces, curves, instances and volumes
as light-list entries (they are found by BSDF sampling only).

#### Scenario: An emissive MaterialX surface

- **WHEN** a surface's MaterialX graph emits light
- **THEN** it contributes only through BSDF-sampled bounces, never through NEE
