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

A surface whose geometry a restricted light's shadow rays ignore SHALL transmit
nothing more from that light to a path that already estimated the light past it:
when a path leaves a vertex that runs NEE and then reaches such surfaces, crossing
each by transmission (refraction, or a delta transmission), no light-sampled or
bounce-sampled estimate of that light SHALL be taken through a further transmission
of those surfaces. The vertex the path left already estimated the light along that
line, with those surfaces absent. Reflection off such a surface SHALL be unchanged,
and so SHALL a path that reaches the surface from the camera, or from a vertex
that ran no NEE, since nothing before the surface estimated the light past it. A transmission through a surface the light's shadow rays do see
ends the rule for that light.

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

#### Scenario: Excluded glass does not count the light twice

- **WHEN** a glossy floor reflects a distant light through a slab of thick glass that
  the light's `collection:shadowLink` excludes and the camera does not see, and the
  floor is rendered with `--strategy light`, `--strategy bsdf` and `--strategy power`
  at a fixed sample count with adaptive sampling off and `--indirect-clamp 0`
- **THEN** each image of the floor agrees within noise with the floor rendered with
  no slab and no link, where the bounce-sampled strategies used to read it about
  twice as bright

#### Scenario: A path from the camera through excluded glass is unchanged

- **WHEN** the camera looks at a light through thick glass that the light's
  `collection:shadowLink` excludes
- **THEN** the image is the one the renderer gave before this rule: no vertex
  before the glass estimated the light past it, so nothing is dropped (a documented
  gap: each glass face's own estimate still sees the light with the glass absent)
