# Spec Delta

## Purpose

Renders USD particle fields — today the OpenUSD 26.03 `ParticleField3DGaussianSplat`
prim, a 3D Gaussian splat capture — as emission-only stochastic presence that camera
and indirect rays see, so a captured set can stand behind, around and in the
reflections of CG.

## ADDED Requirements

### Requirement: Gaussian splat prims are imported

A prim of type `ParticleField3DGaussianSplat` SHALL import as one particle field
placed by the prim's composed transform, honouring `visibility` and `purpose` as
other gprims do. Its particle count SHALL be the length of its positions. Any other
`ParticleField` prim (the base type, or one applying a surflet kernel) SHALL be
skipped with one warning naming the prim and its kernel. No non-USD file (such as
PLY) SHALL be read.

#### Scenario: A splat prim renders

- **WHEN** a `.usda` holds a `ParticleField3DGaussianSplat` with one particle at the
  origin, opacity 1, scale 0.1 and SH degree 0, in front of the camera
- **THEN** the render shows it, and `--stats` reports one particle field of one
  particle

#### Scenario: The prim's transform places and shapes the particles

- **WHEN** the same prim sits under an Xform scaling X by 2 and translating by (1, 0, 0)
- **THEN** the particle is centred at (1, 0, 0) and its footprint is twice as wide
  along X as along Y

#### Scenario: A surflet field is refused, not misread

- **WHEN** a `ParticleField` prim applies `ParticleFieldKernelGaussianSurfletAPI`
- **THEN** it is skipped, one WARN names it, and the rest of the scene renders

#### Scenario: An invisible splat prim

- **WHEN** the splat prim authors `visibility = "invisible"`
- **THEN** it does not render and casts nothing

### Requirement: Particle attributes follow the schema's encoding

Positions, scales, orientations, opacities and SH coefficients SHALL be read from
their `float` attributes, and from the `half` twins (`positionsh`, `scalesh`,
`orientationsh`, `opacitiesh`, `radiance:sphericalHarmonicsCoefficientsh`) only when
the `float` one is unauthored. Scales and opacities SHALL be taken as linear values
(never log-scale or logit), and quaternions with the real part first.

#### Scenario: Float wins over half

- **WHEN** a prim authors both `opacities = [0.25]` and `opacitiesh = [1.0]`
- **THEN** the particle's opacity is 0.25

#### Scenario: Half-only data is read

- **WHEN** a prim authors only the `half` attributes
- **THEN** it renders as the same prim authored in `float` with the same values

#### Scenario: Quaternion order

- **WHEN** a particle of scales (0.3, 0.05, 0.05) authors orientation
  (0.7071068, 0, 0, 0.7071068) as written by USD (real part first)
- **THEN** its long axis lies along Y, a 90° rotation about Z

### Requirement: Missing or mis-sized attributes fall back as the schema says

An orientation, scale, opacity or SH array longer than the particle count (times the
SH element size) SHALL be truncated; a shorter one SHALL be ignored, with one
warning per attribute per prim. An ignored or unauthored attribute SHALL fall back
to no rotation, unit scale, opacity 1, and SH degree 0 with a DC radiance of
(0.5, 0.5, 0.5) respectively. A prim with no positions has no particles.

#### Scenario: Too-short opacities are ignored

- **WHEN** a prim of 4 particles authors 3 opacities
- **THEN** one WARN names the prim and `opacities`, and all 4 particles have opacity 1

#### Scenario: Unauthored SH is mid-grey

- **WHEN** a prim authors positions, scales and opacities but no SH coefficients
- **THEN** every particle met emits (0.5, 0.5, 0.5) before colour conversion, in
  every direction

#### Scenario: No positions

- **WHEN** a splat prim authors no positions
- **THEN** it imports as an empty field: nothing renders and no error is raised

### Requirement: Degenerate particles are dropped

A particle with a non-finite position, scale, orientation or opacity, a scale
component not above zero, or a zero-length quaternion SHALL be dropped, and each
prim that dropped any SHALL log one warning with the count. Opacities outside
[0, 1] SHALL be clamped into it, counted in the same warning. A particle of
opacity 0 SHALL be dropped silently: it contributes nothing.

#### Scenario: Mixed valid and broken particles

- **WHEN** a prim of 1000 particles has 3 with a zero scale component and 2 with NaN
  positions
- **THEN** 995 particles render and exactly one WARN for the prim reports 5 dropped

### Requirement: Spherical harmonics degree and layout

The SH degree SHALL be read from `radiance:sphericalHarmonicsDegree` (schema
fallback 3) and each particle SHALL own `(degree + 1)²` contiguous RGB coefficients,
in the order the reference 3D Gaussian Splatting implementation and OpenUSD's own
PLY conversion write them. Degrees 0 to 3 SHALL be supported; a higher degree SHALL
be striped by its authored element size, rendered with bands 0 to 3, and warned once.

#### Scenario: Each band is evaluated

- **WHEN** a fixture of degree 3 has one non-zero coefficient per band, each
  generated by the reference conversion toolchain
- **THEN** the radiance toward each of a fixed set of directions matches the
  reference SH evaluation to within 1e-6 relative

#### Scenario: Degree 4 data

- **WHEN** a prim authors degree 4 with 25 coefficients per particle
- **THEN** it renders using the first 16 of each particle's 25 coefficients, and one
  WARN says bands above 3 were ignored

### Requirement: A ray meets a splat at its peak response

Each particle SHALL be a Gaussian of unit standard deviation in its local frame,
mapped by its position, orientation and scale (and the prim's transform), with
support truncated at 3σ. A ray crossing that support SHALL hit the particle at the
distance `t*` where the Gaussian's response along the ray peaks, with presence
`α = opacity · exp(−½ d²)`, `d` the Mahalanobis distance at `t*`.

#### Scenario: Head-on through the centre

- **WHEN** a ray passes through the centre of a particle of opacity 0.8
- **THEN** it hits at the centre's distance with presence 0.8

#### Scenario: One standard deviation off-centre

- **WHEN** a ray passes one standard deviation from the centre along the particle's
  short axis
- **THEN** its presence is `0.8 · e^(−½)` and its `t*` is the ray's closest approach
  in the particle's normalised frame

#### Scenario: Outside the support

- **WHEN** a ray passes 3.01 standard deviations from the centre
- **THEN** it does not hit the particle

### Requirement: A met splat emits its spherical-harmonic radiance

A splat that is met SHALL end the path, adding the path's throughput times the
radiance `max(0, 0.5 + Σ cₖ Yₖ(ω))`, per channel, with `ω` the ray's direction
(from the viewer toward the splat) in the particle field's local frame, then
converted as "Splat radiance colour space" requires. Meeting a splat SHALL be an
emission event of type `O` for light path expressions.

#### Scenario: View-dependent colour

- **WHEN** a degree-1 particle carries a coefficient that makes it red seen along +Z
  and green seen along −Z, and two cameras look at it from either side
- **THEN** one render shows it red and the other green

#### Scenario: Negative radiance is clamped

- **WHEN** the SH sum toward the camera is below −0.5 in a channel
- **THEN** that channel contributes 0, never a negative value

#### Scenario: An LPE routes splats as objects

- **WHEN** a render product requests `C<L.>O` and `C<L.>L`
- **THEN** the splats appear in the `O` AOV and not in the `L` AOV, and the beauty is
  their sum plus every other contribution

### Requirement: Splat radiance colour space

The evaluated radiance SHALL be converted into the working space from the colour
space resolved for `radiance:sphericalHarmonicsCoefficients` (its `colorSpace`
metadatum, else the nearest `colorSpace:name`), and from `srgb_texture` when none
is named, because splats are trained against display-encoded photographs. The
transfer curve SHALL apply to the evaluated radiance, never to coefficients.

#### Scenario: Unnamed radiance is display-encoded

- **WHEN** a particle of SH degree 0 evaluates to 0.5 in every channel and names no
  colour space, in a `lin_rec709` render
- **THEN** it emits the sRGB decode of 0.5, about 0.214, per channel

#### Scenario: A named linear space is honoured

- **WHEN** the coefficients attribute authors `colorSpace = "lin_rec709"`
- **THEN** the same particle emits 0.5 per channel

### Requirement: Which rays see splats

Camera rays and indirect rays (BSDF, phase and pass-through continuations) SHALL
see splats. Shadow rays SHALL NOT: a splat never blocks or attenuates light to
anything, because its own shadows are baked into its radiance. Splats SHALL NOT be
light-list entries, so next-event estimation never samples them.

#### Scenario: A chrome sphere reflects the capture

- **WHEN** a perfect mirror sphere sits inside a splat capture
- **THEN** the sphere shows the capture's radiance in its reflection

#### Scenario: Splats cast no shadow

- **WHEN** a dense opaque splat field stands between a diffuse floor and a sphere
  light
- **THEN** the light-sampled estimate of the floor equals the same scene's estimate
  with the splats removed

#### Scenario: A capture lights CG by bounces only

- **WHEN** a diffuse sphere sits inside an emitting splat capture with no lights
- **THEN** the sphere is lit, the light list is empty, and the estimate is unbiased
  (it converges to the reference as 1/√N)

### Requirement: Passing a splat is a cutout pass

A splat SHALL be met with probability equal to its presence and otherwise passed,
exactly as the rendering capability's "Cutout surfaces are stochastic presence"
requires: no depth spent, no emission added, no vertex recorded. A ray SHALL cross
each particle at most once, as "A pass-through crosses each surface once" requires.

#### Scenario: Two half-present splats in a row

- **WHEN** a camera ray passes through the centres of two particles of opacity 0.5,
  the near one emitting 1 and the far one emitting 0, over a black background,
  with linear radiance
- **THEN** the pixel converges to 0.5

#### Scenario: A splat is not met twice

- **WHEN** a camera ray passes one particle of opacity 0.5 emitting 1 over a black
  background, with linear radiance
- **THEN** the pixel converges to 0.5, not to 0.75

#### Scenario: Crossing bound

- **WHEN** a ray passes more pass-throughs than the cutout crossing bound
- **THEN** the next splat or cutout it hits is met, and `--stats` counts the ray as
  having reached the bound

### Requirement: Scenes without particle fields are unchanged

A scene with no particle field SHALL render bit-identically to the renderer before
this capability existed, with the same instruction count on the single-threaded
Cornell box reference render.

#### Scenario: Cornell box

- **WHEN** `samples/cornellbox.usda` is rendered at `-s 16`
- **THEN** the EXR is bit-identical to the previous release's and callgrind counts
  the same instructions within 0.1%

### Requirement: Particle statistics

`--stats` SHALL report the number of particle fields, particles and dropped
particles, the memory the particle data and its acceleration structure hold, and,
for rays that met or passed splats, the mean and maximum number of splats crossed
per ray and how many rays reached the crossing bound.

#### Scenario: Stats on a capture

- **WHEN** a scene with one splat prim of 1,256,332 particles is rendered with
  `--stats`
- **THEN** the report shows 1 field, 1,256,332 particles, their memory in MB, and
  the mean and maximum crossings per ray

### Requirement: Known gaps

The capability SHALL be documented as lacking: relighting (splats receive no light),
shadow casting, surflet and other non-ellipsoid kernels, particle motion blur,
instanced particle fields, SIMD packets for particles, a k-nearest traversal, and
the hints `projectionModeHint` and `sortingModeHint`, which are ignored.

#### Scenario: A light in a capture

- **WHEN** a sphere light is placed among splats
- **THEN** the splats render exactly as without it, and the user documentation lists
  relighting as unsupported
