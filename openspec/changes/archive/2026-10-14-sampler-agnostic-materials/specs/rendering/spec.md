## MODIFIED Requirements

### Requirement: Anti-aliasing via multi-sampling with low-discrepancy sampling

The renderer SHALL average `samples_per_pixel` samples per pixel. It SHALL draw sub-pixel
offsets and every other per-sample number from one OpenQMC sampler, selected per render (see
"The QMC sampler is a render setting"), through its domain tree. Each sample's root is keyed by
pixel, frame seed and sample index. Stratified dimensions are drawn as blocks of at most four
from keyed sub-domains. Unbounded or incidental draws (Russian roulette, delta tracking, guide
descent) come from the domain's pseudo-random stream. No randomness SHALL come from outside the
sampler.

#### Scenario: Samples are averaged per pixel

- **WHEN** `samples_per_pixel` is N
- **THEN** each pixel value is the mean of N traced samples with
  low-discrepancy sub-pixel offsets

#### Scenario: A sample is a function of its keys

- **WHEN** the same pixel, frame seed, sample index and sampler are traced twice, in any
  thread, tile or sweep order
- **THEN** both traces draw the same numbers and return bit-identical radiance

## ADDED Requirements

### Requirement: The QMC sampler is a render setting

The sampler SHALL be chosen by `crust:sampler` on the RenderSettings prim, overridden by
`--sampler`. Its values SHALL be `sobol` (default, Owen-scrambled Sobol), `sobolBn`, `lattice`,
`latticeBn`, `pmj` and `pmjBn`: OpenQMC's three base sequences and their blue-noise variants.
An unrecognised value SHALL raise a `SettingsInvalidValue` warning and render with `sobol`. The
choice SHALL apply to every pass of the render, path-guiding training passes included.

#### Scenario: Default sampler

- **WHEN** a stage authors no `crust:sampler` and the command line gives no `--sampler`
- **THEN** the render uses `sobol`

#### Scenario: The flag overrides the stage

- **WHEN** a stage authors `crust:sampler = "lattice"` and the render runs with
  `--sampler pmjBn`
- **THEN** every pass draws from the blue-noise PMJ sampler

#### Scenario: An unknown value

- **WHEN** a stage authors `crust:sampler = "halton"`
- **THEN** the import's warnings include one `SettingsInvalidValue` naming
  `crust:sampler`, and the render uses `sobol`

#### Scenario: Every sampler converges to the same image

- **WHEN** a scene is rendered unclamped with each of the six samplers over enough
  independent seeds
- **THEN** each sampler's mean image agrees with `sobol`'s mean within the seeds' standard
  error

### Requirement: The default sampler leaves renders unchanged

A render with `sobol` SHALL be bit-identical to the renderer before the sampler became
selectable, at the same settings: its beauty, every AOV and light path expression, its
per-pixel sample counts, and its `--stats` ray counts.

#### Scenario: Goldens hold

- **WHEN** `scripts/check_images.sh check` runs against goldens recorded before this change
- **THEN** every image is identical

#### Scenario: Explicit and implicit default agree

- **WHEN** a scene renders once with no sampler setting and once with `--sampler sobol`
- **THEN** the two EXRs' pixels are bit-identical

### Requirement: Light-selection training is sampler-independent

The pre-pass that trains `learned` light selection SHALL draw from the Sobol sampler whatever
`crust:sampler` says. A scene's learned table SHALL therefore be the same under every sampler.

#### Scenario: Same table under two samplers

- **WHEN** a scene with `crust:lightSelection = "learned"` is prepared with `--sampler sobol`
  and with `--sampler latticeBn`
- **THEN** the two learned light-selection tables are bit-identical
