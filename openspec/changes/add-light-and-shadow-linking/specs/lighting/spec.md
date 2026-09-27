## ADDED Requirements

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
transmittance along the ray. For a light whose shadow set is restricted,
bounce-side emission at non-delta vertices SHALL NOT be collected (NEE carries
the light alone there), so the estimator stays unbiased. At delta vertices, the
bounce SHALL keep full weight and see the light through the real occluders.
If a scene needs more distinct shadow classes than the renderer can encode, the
lights beyond the limit SHALL be refused with a warning and shadowed by every
occluder.

#### Scenario: An excluded occluder casts no shadow

- **WHEN** a rect light's `collection:shadowLink` excludes `/World/Hair`
- **THEN** the region `/World/Hair` would shadow from that light is lit as if the
  hair were absent, while `/World/Hair` still shadows every other light and is
  still visible to the camera

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

## MODIFIED Requirements

### Requirement: Known gaps

The following SHALL be documented as unsupported: mesh lights, portal lights,
light filters, `ShadowAPI`, `inputs:diffuse` / `inputs:specular` (warned and
ignored), shaping on distant and dome lights, cone-aware sampling of shaped
lights, and emissive MaterialX surfaces, curves, instances and volumes as
light-list entries (they are found by BSDF sampling only). For light and shadow
linking, the following SHALL be documented: membership inside an instance
prototype is not distinguishable per instance, `membershipExpression` is refused,
and shadow-linked lights are NEE-only at non-delta vertices and physically
shadowed through delta ones.

#### Scenario: An emissive MaterialX surface

- **WHEN** a surface's MaterialX graph emits light
- **THEN** it contributes only through BSDF-sampled bounces, never through NEE

#### Scenario: A pattern-based collection

- **WHEN** a light's `collection:lightLink` authors `membershipExpression`
- **THEN** a warning is logged and the collection is read as the UsdLux default
  (every prim)
