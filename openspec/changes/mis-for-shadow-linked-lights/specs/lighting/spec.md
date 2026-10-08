## MODIFIED Requirements

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

### Requirement: Known gaps

The following SHALL be documented as unsupported: mesh lights, portal lights,
light filters, `ShadowAPI`, `inputs:diffuse` / `inputs:specular` (warned and
ignored), shaping on distant and dome lights, cone-aware sampling of shaped
lights, and emissive MaterialX surfaces, curves, instances and volumes as
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
