## MODIFIED Requirements

### Requirement: Light selection

NEE SHALL take `crust:lightSamples` / `--light-samples` light samples at the
camera vertex and `crust:lightSamplesIndirect` / `--light-samples-indirect` at
every later surface and volume vertex (each default 1, from 1 to 1024; a stage
value outside that range is clamped with a warning, a command-line one refused).
The light
of each sample SHALL be chosen by `crust:lightSelection` / `--light-selection`:
`tree` (default; infinite lights keep a uniform share, and the finite lights
are chosen by walking a light BVH by an importance bound at the shading point's
position and normal, mixed with a defensive share of the `power` probabilities),
`power` (infinite lights keep a uniform share, the finite lights split
the rest half evenly and half by flux), `uniform` (bit-identical to the
historical renderer), `learned` (a visibility-aware per-cell table trained by
a deterministic pre-pass, over the power table), or `neural` (the power table
weighted by each light's visibility, predicted by a small network that a
deterministic pre-pass trains). The samples at one vertex SHALL
be stratified, so that each light is chosen close to the count times its
selection probability.

#### Scenario: Uniform selection

- **WHEN** a render runs with `--light-selection uniform`
- **THEN** each of N lights is picked with probability 1/N

#### Scenario: One sample per vertex is unchanged

- **WHEN** a render runs with both sample counts at 1, given or by default,
  under `power` or `uniform`
- **THEN** the image is bit-identical to the renderer before sample counts
  existed under the same selection

#### Scenario: Stratified picks

- **WHEN** a vertex takes 4 light samples among three lights whose selection
  probabilities are 0.5, 0.25 and 0.25
- **THEN** the first light is sampled twice and each other light once

#### Scenario: More samples, same image

- **WHEN** a scene is rendered with `--light-samples 4` and with the default,
  at increasing sample counts with the indirect clamp off
- **THEN** the difference between the two falls as 1/√N, and the direct-light
  noise per pixel sample is lower with 4

#### Scenario: Neural selection is accepted

- **WHEN** a render runs with `--light-selection neural`, or its render settings author `crust:lightSelection = "neural"`
- **THEN** the render completes, and the `--light-selection` help lists `neural` with a one-line description

#### Scenario: Tree selection is the default

- **WHEN** a render runs with no `--light-selection` and no authored
  `crust:lightSelection`
- **THEN** it selects lights by `tree`, and the `--light-selection` help lists
  `tree` as the default with a one-line description

#### Scenario: Existing modes are unchanged

- **WHEN** a checked-in sample renders at 16 spp under `power`, `uniform`,
  `learned` or `neural`, each given explicitly
- **THEN** the image is bit-identical to the one rendered under the same mode
  before `tree` existed

## ADDED Requirements

### Requirement: Tree selection favours the lights that can reach the receiver

Under `tree`, a finite light's selection probability at a vertex SHALL depend
on its flux, its distance from the vertex, whether the vertex lies within the
directions the light emits into (its front side, and its shaping cone when one
is authored), and, at a surface vertex, whether the light lies above or below
the surface. At a volume vertex the surface terms SHALL be ignored.

#### Scenario: A light facing away is picked less

- **WHEN** a receiver is lit by two equal one-sided rect lights at the same
  distance, one facing it and one facing away, under `--light-selection tree`
- **THEN** the light facing it is picked with higher probability than under
  `power`, and the one facing away with lower

#### Scenario: A near light is picked more than an equal far one

- **WHEN** a receiver is lit by two equal sphere lights, one at 1 m and one at
  10 m, both above its surface, under `--light-selection tree`
- **THEN** the near light is picked with higher probability than the far one

#### Scenario: A light below an opaque receiver is picked less

- **WHEN** an opaque diffuse floor is lit by two equal sphere lights at the
  same distance, one above it and one wholly below it, under
  `--light-selection tree`
- **THEN** the light below has only its defensive share there

#### Scenario: A transmissive receiver still sees lights behind it

- **WHEN** a glass or thin-walled transmissive surface is lit by two equal
  sphere lights at the same distance, one on each side
- **THEN** both are picked with more than their defensive share

#### Scenario: A narrow spotlight is not picked outside its cone

- **WHEN** a receiver lies outside the shaping cone of one of two lights under
  `--light-selection tree`
- **THEN** that light's selection probability there is its defensive share
  alone

#### Scenario: Many lights get cleaner direct lighting

- **WHEN** `samples/many_lights.usda` renders at 16 spp with
  `--indirect-clamp 0` under `tree` and under `power`
- **THEN** the `tree` render's relMSE against a 1024 spp reference is lower

### Requirement: Tree selection is unbiased

Under `tree`, every finite light that emits SHALL keep a selection probability
of at least a fixed defensive fraction of its `power` probability at every
vertex, whatever the importance bound gives it. A render under `tree` SHALL
converge to the same image as under `power`.

#### Scenario: A light behind the receiver stays sampleable

- **WHEN** a light lies entirely below a receiver's surface, so that its
  importance bound there is zero
- **THEN** its selection probability there is still the defensive fraction of
  its `power` probability, and greater than zero

#### Scenario: Converges to the power reference

- **WHEN** `usdlux` and `samples/many_lights.usda` render under `tree` at
  increasing spp with `--indirect-clamp 0`, compared against a 1024 spp `power`
  reference
- **THEN** relMSE falls as roughly 1/N in spp rather than plateauing

### Requirement: Tree selection weights both MIS sides alike

Under `tree`, the probability that NEE picked a light at a vertex SHALL be
exactly the probability the bounce side uses when a BSDF-sampled ray from that
same vertex hits that light, or escapes to it: evaluated at the same position
and the same normal.

#### Scenario: Emission is not double-counted

- **WHEN** `veach_mis` renders under `tree` with `--indirect-clamp 0` at 1024 spp
- **THEN** its mean pixel value matches the `power` render's within the noise
  of either

#### Scenario: Strategies agree on a lit floor

- **WHEN** a diffuse floor lit by several sphere and rect lights and a dome is
  rendered under `tree` with the light-only, BSDF-only and power-MIS strategies
- **THEN** the three estimates of the floor agree within noise

### Requirement: Tree selection is deterministic

The light tree SHALL depend only on the scene's lights. The same scene,
settings and frame SHALL give bit-identical images under `tree` with tiled and
scanline rendering, and at any rayon thread count.

#### Scenario: Thread count does not change the image

- **WHEN** the same scene renders under `tree` with `RAYON_NUM_THREADS=1` and
  with the default thread count
- **THEN** the two images are bit-identical

#### Scenario: Tiles and scanlines agree

- **WHEN** the same scene renders under `tree` in tiled and in scanline mode
- **THEN** the two images are bit-identical

### Requirement: Tree selection with nothing to choose matches power

Under `tree`, a scene with at most one finite light that emits SHALL select
exactly as `power` does.

#### Scenario: One rect light and a dome

- **WHEN** a scene with one rect light and one dome renders under
  `--light-selection tree`
- **THEN** the image is bit-identical to the same render under
  `--light-selection power`
