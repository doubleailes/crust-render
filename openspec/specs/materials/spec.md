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
scattered ray, BRDF value, and PDF) and an emission query (`emitted`). Materials
that do not implement importance sampling SHALL fall back to the default derived
from `scatter`.

#### Scenario: Integrator queries a material

- **WHEN** the integrator interacts with a hit surface
- **THEN** it obtains a scattered ray, BRDF value, and PDF via
  `scatter_importance`, or `None` when the material absorbs the ray

#### Scenario: Non-emissive material

- **WHEN** `emitted` is queried on a non-light material
- **THEN** it returns zero radiance

### Requirement: Supported shading models

The engine SHALL provide the following material models: Lambertian, Metal,
Dielectric and ComplexDielectric, Blinn-Phong, Cook-Torrance, Disney Principled,
OpenPBR, and Emissive.

#### Scenario: A model is selected for a surface

- **WHEN** a surface is assigned one of the supported material models
- **THEN** rays scatter according to that model's BRDF and parameters

### Requirement: Shared microfacet BRDF helpers

Microfacet materials SHALL share GGX helpers: visible-normal (VNDF) GGX sampling
and its PDF, Schlick Fresnel, and the Schlick-GGX geometry term. Lives in
`material/brdf.rs`.

#### Scenario: Microfacet material samples a direction

- **WHEN** a GGX-based material scatters a ray
- **THEN** the outgoing direction is drawn via VNDF sampling and weighted by
  Fresnel and geometry terms

### Requirement: Emissive surfaces act as light-emitting geometry

An Emissive material SHALL return non-zero radiance from `emitted`, allowing the
same surface to serve as both visible geometry and a light source.

#### Scenario: Emissive sphere is hit directly

- **WHEN** a ray hits an emissive surface
- **THEN** the surface contributes its emission color to the path
