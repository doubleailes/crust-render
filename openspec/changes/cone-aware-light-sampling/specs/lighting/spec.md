## ADDED Requirements

### Requirement: Rect lights can be sampled within their cone

With `CRUST_CONE_CLIP = on`, a rect light SHALL be sampled only within the bounding
square of its cutoff circle when all of these hold: its shaping cone angle is below
90°, its shaping axis is its normal, and the shading point is in front of it. The
cutoff circle is the region of the light's plane that emits toward the point. A
point that sees none of the light SHALL draw no light sample. `off`, the default,
SHALL leave sampling unchanged.

#### Scenario: Cone-clipped rect sampling is unbiased

- **WHEN** a floor under a rect light with a 20° cone, softness 0.4 and focus 2 is
  rendered light-only, BSDF-only and with MIS, with `CRUST_CONE_CLIP` `off` and `on`
- **THEN** all the estimates of the floor agree within noise that falls as 1/√N

#### Scenario: Samples land where the light emits

- **WHEN** a shaped rect light much wider than its cutoff circle is sampled with
  `CRUST_CONE_CLIP = on` from a point inside its beam
- **THEN** every sample lies within the bounding square of the cutoff circle

#### Scenario: A point outside every part of the beam

- **WHEN** a shaped rect light is sampled with `CRUST_CONE_CLIP = on` from a point
  whose cutoff square misses the rect
- **THEN** no light sample and no shadow ray is drawn for that light

#### Scenario: Unclipped lights are unchanged

- **WHEN** a stage is rendered at 16 spp with `CRUST_CONE_CLIP = on` and its lights
  are unshaped, have cones of 90° or wider, or have a shaping axis that is not their
  normal
- **THEN** the image is bit-identical to the same render with `CRUST_CONE_CLIP = off`

### Requirement: Polar disk sampling

With `CRUST_DISK_SAMPLING = polar`, a disk light that is round in world space and
whose shaping axis (if any) is its normal SHALL be sampled from a point in front of
it in polar coordinates about the point's foot on the disk's plane. Along each
direction the density SHALL follow `cos θl / r²`, and focus as well when focus is
authored. With `CRUST_CONE_CLIP = on`, samples SHALL stop at the cutoff circle.
Other disks and points SHALL be sampled by area.

#### Scenario: Polar disk sampling is unbiased

- **WHEN** a diffuse floor under a disk light, unshaped and then shaped with a 30°
  cone, softness 0.4 and focus 2, is rendered light-only, BSDF-only and with MIS
  under `CRUST_DISK_SAMPLING = polar`, with `CRUST_CONE_CLIP` `off` and `on`
- **THEN** all the estimates of the floor agree within noise that falls as 1/√N

#### Scenario: An elliptical disk falls back

- **WHEN** a disk light under a non-uniform scale in its plane is sampled under
  `CRUST_DISK_SAMPLING = polar`
- **THEN** it is sampled by area, as under `CRUST_DISK_SAMPLING = area`

#### Scenario: The capped disk samples stay in the beam

- **WHEN** a shaped disk light is sampled under `CRUST_DISK_SAMPLING = polar` and
  `CRUST_CONE_CLIP = on` from a point inside its beam
- **THEN** every sample lies within the cutoff circle about the point's foot

## MODIFIED Requirements

### Requirement: Known gaps

The following SHALL be documented as unsupported: mesh lights, portal lights,
light filters, `ShadowAPI`, `inputs:diffuse` / `inputs:specular` (warned and
ignored), shaping on distant and dome lights, and emissive MaterialX surfaces,
curves, instances and volumes as light-list entries (they are found by BSDF
sampling only). Cone-aware sampling of shaped lights SHALL be documented as limited
to rect lights and round disk lights whose shaping axis is their normal: light
selection by power ignores the cone, sphere and cylinder lights are sampled without
regard to it, and an IES profile shapes no light's samples beyond its cone cutoff.
For light and shadow
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

#### Scenario: A small spotlight among others

- **WHEN** a stage of small shaped sphere lights lighting disjoint pools is rendered
  with power light selection
- **THEN** a point in one pool still draws NEE samples from the other lights, whose
  cones exclude it
