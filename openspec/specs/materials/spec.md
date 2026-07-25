# materials Specification

## Purpose

Define how surfaces scatter and emit light. Materials are the integrator's
extension point: each implements a common trait exposing importance-sampled
scattering and emission. This capability covers the material trait contract, the
supported shading models, the shared microfacet helpers, and emissive surfaces.
Lives in `crust-core/src/material/`.

## Requirements

### Requirement: Material trait contract

Every material SHALL implement a common `Material` trait providing
importance-sampled scattering (`scatter_importance` returning an optional
`ScatterSample` — scattered ray, BRDF value, PDF, and a `delta` flag marking
singular lobes such as transmission), a continuous-component evaluator
(`eval(r_in, rec, wi) -> Option<(value, pdf)>`, used by NEE and guided MIS;
`None` means no continuous component and must not depend on `wi`), and an
emission query (`emitted`).

#### Scenario: Integrator queries a material

- **WHEN** the integrator interacts with a hit surface
- **THEN** it obtains a `ScatterSample` (scattered ray, BRDF value, PDF, delta
  flag) via `scatter_importance`, or `None` when the material absorbs the ray

#### Scenario: Non-emissive material

- **WHEN** `emitted` is queried on a non-light material
- **THEN** it returns zero radiance

### Requirement: Supported shading models

The engine SHALL provide exactly two material implementations: **`OpenPBR`**,
a single übershader covering diffuse, metal, glass/transmission, coat, fuzz,
thin-film, and subsurface (with `diffuse`/`metal`/`glass`/`glossy` Rust-side
preset constructors), and **`Emissive`**, a pure emitter with no geometry
knowledge.

#### Scenario: A model is selected for a surface

- **WHEN** a surface is assigned a material
- **THEN** rays scatter according to `OpenPBR`'s layered BSDF and parameters,
  or the surface is a pure `Emissive` light

### Requirement: Shared microfacet BRDF helpers

`OpenPBR` lobes SHALL share GGX helpers from `material/brdf.rs`: anisotropic
visible-normal (VNDF) GGX sampling and its PDF, Schlick/F82 Fresnel, EON
(energy-preserving Oren-Nayar) diffuse, Charlie sheen, thin-film, and Cauchy
dispersion.

#### Scenario: Microfacet lobe samples a direction

- **WHEN** a GGX-based lobe of `OpenPBR` scatters a ray
- **THEN** the outgoing direction is drawn via VNDF sampling and weighted by
  Fresnel and geometry terms

### Requirement: Emissive surfaces act as light-emitting geometry

An Emissive material SHALL return non-zero radiance from `emitted`, allowing the
same surface to serve as both visible geometry and a light source.

#### Scenario: Emissive sphere is hit directly

- **WHEN** a ray hits an emissive surface
- **THEN** the surface contributes its emission color to the path

### Requirement: Unbound geometry falls back to grey OpenPBR

Geometry with no resolvable bound material SHALL be assigned a default grey
`OpenPBR` material, not a separate shading model.

#### Scenario: Unbound geometry

- **WHEN** a mesh or sphere has no resolvable bound material
- **THEN** it renders with a default grey diffuse `OpenPBR` material
