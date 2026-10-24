# Spec Delta

## ADDED Requirements

### Requirement: Mesh lights

A `UsdGeomMesh` with `UsdLuxMeshLightAPI` applied SHALL be a light-list entry whose
emitting surface is the triangles the mesh renders with, after subdivision and
displacement. NEE SHALL sample it, and a BSDF-sampled ray that hits it SHALL be weighted
as "One density for both MIS strategies" requires. It SHALL emit from both sides of its
surface. Emissive geometry without the API SHALL remain outside the light list.

#### Scenario: A mesh light is sampled by NEE

- **WHEN** a diffuse floor lit only by a quad mesh with `MeshLightAPI` is rendered
  light-only, BSDF-only and with power MIS
- **THEN** all three estimates of the floor agree within noise that falls as 1/√N

#### Scenario: Applying the API reduces noise without changing the picture

- **WHEN** a scene lit by a flat-coloured emissive mesh is rendered at 16 spp with
  `--indirect-clamp 0`, once as authored and once with `MeshLightAPI` applied to the
  mesh and its light inputs unauthored
- **THEN** the two images agree within noise that falls as 1/√N, and the second has a
  lower MRSE

#### Scenario: A subdivided mesh light

- **WHEN** a mesh with `subdivisionScheme = catmullClark` and `MeshLightAPI` lights a
  floor
- **THEN** NEE samples the refined surface the rays hit, and the light-only and
  BSDF-only estimates agree within noise

#### Scenario: Emissive geometry without the API

- **WHEN** a mesh whose material emits carries no `MeshLightAPI`
- **THEN** it contributes only through BSDF-sampled bounces, as before this change

#### Scenario: Scenes without mesh lights are unchanged

- **WHEN** a stage with no `MeshLightAPI` prim is rendered at 16 spp
- **THEN** the image is bit-identical to the same render before this requirement

### Requirement: Mesh light radiance

A mesh light SHALL have one radiance, seen alike by camera rays, bounce rays and NEE.
With L = `color` · 2^`exposure` · `intensity` (times the blackbody under
`enableColorTemperature`, divided by the world area under `normalize`), it SHALL be
glow · L under `light:materialSyncMode = materialGlowTintsLight` (the `MeshLightAPI`
fallback), and L under `independent` and `noMaterialResponse`. Glow is the bound
material's emission.

#### Scenario: Unauthored light inputs keep the material's brightness

- **WHEN** an emissive mesh gets `MeshLightAPI` with no light input authored
- **THEN** its radiance, seen by a camera ray, equals the material's emission

#### Scenario: The light tints the glow

- **WHEN** a mesh whose material emits (2, 2, 2) authors `MeshLightAPI` with
  `inputs:color = (1, 0.5, 0.25)` and `inputs:intensity = 3`
- **THEN** camera rays, bounce rays and NEE all see radiance (6, 3, 1.5)

#### Scenario: Independent ignores the glow

- **WHEN** a mesh light authors `light:materialSyncMode = "independent"` and
  `inputs:color = (0, 0, 1)` over a material emitting (1, 0, 0)
- **THEN** every ray sees it emit (0, 0, 1), and its material still reflects light

#### Scenario: No material response

- **WHEN** a mesh light authors `light:materialSyncMode = "noMaterialResponse"`
- **THEN** it emits L and reflects, transmits and scatters nothing

#### Scenario: Normalized mesh light

- **WHEN** a mesh light authors `inputs:normalize = 1`, and its mesh is scaled to twice
  the world area
- **THEN** its radiance halves, and the power it emits is unchanged

### Requirement: Mesh light visibility

A mesh light's surface SHALL be visible to camera rays and solid (it occludes shadow
rays and ends the rays that reach it) by default. An authored `crust:rayMask` SHALL
decide its visibility outright; otherwise `crust:light:cameraVisible`, and otherwise
`primvars:ri:attributes:visibility:camera`, SHALL decide its camera visibility. Hidden
from the camera, it SHALL stay solid to every other ray: it is not a transparent emitter.

#### Scenario: A mesh light in frame

- **WHEN** the camera sees a mesh light that authors no visibility
- **THEN** the camera sees its surface at its radiance

#### Scenario: A hidden mesh light

- **WHEN** a mesh light authors `crust:light:cameraVisible = 0`
- **THEN** camera rays do not see it, it still lights the scene, and it still casts
  shadows from other lights

### Requirement: Mesh lights honour the light controls

Light linking, shadow linking and the per-lobe multipliers `inputs:diffuse` /
`inputs:specular` SHALL apply to a mesh light as they do to every light, and it SHALL
be selected under every light-selection mode.

#### Scenario: A linked mesh light

- **WHEN** a mesh light's `collection:lightLink` excludes a sphere
- **THEN** the sphere receives no light from it, through NEE or through bounces

#### Scenario: Specular off

- **WHEN** a mesh light authors `inputs:specular = 0` over a glossy floor
- **THEN** the floor shows no specular reflection of it, and its diffuse lighting is
  unchanged

### Requirement: Mesh lights that cannot be sampled

A `MeshLightAPI` prim SHALL stay outside the light list, keeping its BSDF-only
emission, with one `skipped` warning per cause, when: its radiance would vary over its
surface or with direction (a textured, primvar-driven, coated or fuzzed glow under
`materialGlowTintsLight`); its material is a cutout; it is placed through an instance
prototype; it moves over the shutter; or it is not a `Mesh`.

#### Scenario: A textured glow

- **WHEN** a mesh whose material's emission reads a texture applies `MeshLightAPI`
  under the fallback `materialSyncMode`
- **THEN** a warning names the prim and the cause, and the mesh renders as without the
  API

#### Scenario: A textured glow under independent

- **WHEN** the same mesh authors `light:materialSyncMode = "independent"`
- **THEN** it becomes a mesh light emitting its light inputs, with no warning

#### Scenario: An instanced mesh light

- **WHEN** a mesh with `MeshLightAPI` lies inside a native instance's prototype
- **THEN** a warning names the prim, and every placement renders as without the API

#### Scenario: A mesh light that emits nothing

- **WHEN** a mesh with `MeshLightAPI` under `materialGlowTintsLight` has a material that
  does not emit
- **THEN** a warning names the prim, and it is not a light-list entry

## MODIFIED Requirements

### Requirement: Known gaps

The following SHALL be documented as unsupported: portal lights,
light filters, `ShadowAPI`, shaping on distant and dome lights, cone-aware sampling of shaped
lights, and emissive surfaces, curves, instances and volumes as
light-list entries unless a mesh applies `MeshLightAPI` (they are found by BSDF sampling
only). For mesh lights the following SHALL be documented: triangles are sampled by area,
not by the solid angle they subtend; a mesh is one light-list entry, not one per
triangle, under every light selection; a glow that varies over the surface or with
direction, a cutout, an instanced or a moving mesh cannot be a mesh light; and
`light:materialSyncMode` is read as one radiance for every ray rather than the schema's
split between light rays and other rays. For light and shadow
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

- **WHEN** a surface's MaterialX graph emits light and its mesh does not apply
  `MeshLightAPI`
- **THEN** it contributes only through BSDF-sampled bounces, never through NEE

#### Scenario: A pattern-based collection

- **WHEN** a light's `collection:lightLink` authors `membershipExpression`
- **THEN** a warning is logged and the collection is read as the UsdLux default
  (every prim)
