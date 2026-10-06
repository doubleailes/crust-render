# Spec Delta

## MODIFIED Requirements

### Requirement: Light selection

NEE SHALL sample one light per vertex, chosen by `crust:lightSelection` /
`--light-selection`: `power` (default; infinite lights keep a uniform share, the
finite lights split the rest half evenly and half by flux), `uniform`
(bit-identical to the historical renderer), `learned` (a visibility-aware
per-cell table trained by a deterministic pre-pass, over the power table), or
`neural` (the power table weighted by each light's visibility, predicted by a
small network that a deterministic pre-pass trains).

#### Scenario: Uniform selection

- **WHEN** a render runs with `--light-selection uniform`
- **THEN** each of N lights is picked with probability 1/N

#### Scenario: Neural selection is accepted

- **WHEN** a render runs with `--light-selection neural`, or its render settings author `crust:lightSelection = "neural"`
- **THEN** the render completes, and the `--light-selection` help lists `neural` with a one-line description

#### Scenario: Existing modes are unchanged

- **WHEN** a checked-in sample renders at 16 spp under `power`, `uniform` or `learned`
- **THEN** the image is bit-identical to the one rendered before `neural` existed

## ADDED Requirements

### Requirement: Neural selection falls back to power

Under `neural`, NEE SHALL pick by the power table wherever the network cannot apply: a scene with fewer than two lights, a pre-pass that finds no receivers, a position outside the trained bounds, or more lights than the network supports. Exceeding the light limit SHALL log one `WARN` naming the limit. The other cases SHALL be silent.

#### Scenario: Too many lights

- **WHEN** a scene with more lights than the neural limit renders with `--light-selection neural`
- **THEN** one `WARN` line names the limit, and the image is bit-identical to the same render under `--light-selection power`

#### Scenario: One light

- **WHEN** a scene with a single light renders with `--light-selection neural`
- **THEN** the image is bit-identical to the same render under `--light-selection power`, and nothing is logged at `WARN`

### Requirement: Neural selection is unbiased

Under `neural`, every light that can emit SHALL keep a pick probability of at least its defensive uniform share at every vertex, whatever visibility the network predicts. A render under `neural` SHALL converge to the same image as under `power`.

#### Scenario: A light the network calls hidden stays sampleable

- **WHEN** the network predicts zero visibility for a light at a vertex
- **THEN** that light's pick probability there is still at least the defensive share divided by the number of emitting lights

#### Scenario: Converges to the power reference

- **WHEN** `usdlux` renders under `neural` at increasing spp with `--indirect-clamp 0`, compared against a 1024 spp `power` reference
- **THEN** relMSE falls as roughly 1/N in spp rather than plateauing

### Requirement: Neural selection weights both MIS sides alike

Under `neural`, the probability that NEE picked a light at a vertex SHALL be exactly the probability the bounce side uses when a BSDF-sampled ray from that same vertex hits that light, or escapes to it.

#### Scenario: Emission is not double-counted

- **WHEN** `veach_mis` renders under `neural` with `--indirect-clamp 0` at 1024 spp
- **THEN** its mean pixel value matches the `power` render's within the noise of either

### Requirement: Neural selection is deterministic

The `neural` pre-pass and training SHALL depend only on the scene and the render settings. The same scene, settings and frame SHALL give bit-identical images under tiled and scanline rendering, and at any rayon thread count.

#### Scenario: Thread count does not change the image

- **WHEN** the same scene renders under `neural` with `RAYON_NUM_THREADS=1` and with the default thread count
- **THEN** the two images are bit-identical

#### Scenario: Tiles and scanlines agree

- **WHEN** the same scene renders under `neural` in tiled and in scanline mode
- **THEN** the two images are bit-identical
