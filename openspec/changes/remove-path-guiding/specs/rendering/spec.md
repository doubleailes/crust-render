## MODIFIED Requirements

### Requirement: Adaptive sampling stops pixels early

The renderer SHALL keep, for each pixel of an adaptive pass, a convergence
index `e`: the relative standard error of the pixel mean divided by
`crust:varianceThreshold`, so that `e < 1` means the pixel passes its own
test. A pixel that has recorded no sample with non-zero luminance has
`e = +∞`: a zero measured variance from zero observations is not evidence of
convergence.

The renderer SHALL stop sampling a pixel only when all of the following hold:

- it has taken at least the effective minimum, `max(crust:minSamplesPerPixel, ⌈√N⌉)`
  samples, where `N` is the pass's per-pixel sample budget;
- its own index is below 1 (which implies it has recorded some light);
- none of its four cross neighbours (up, down, left, right, inside the
  image) that is still sampling has an index more than
  `crust:adaptiveNeighbourTolerance` above its own: `e_q − e_p ≤ t` for every
  such neighbour `q`.

A neighbour that has stopped, whether converged or out of budget, SHALL NOT
hold a pixel back, and a pixel that has stopped SHALL NOT resume. Diagonal
neighbours are not compared. A negative tolerance disables the neighbour
comparison, and each pixel then stops exactly as it would on its own. A
`crust:varianceThreshold` of 0 disables adaptive sampling. Adaptive sampling
applies to the render pass, and the image SHALL be bit-identical under the tiled and scanline
strategies whatever the tolerance.

#### Scenario: A converged pixel stops early

- **WHEN** a pixel has taken at least the effective minimum, its index is
  below 1, and no still-sampling cross neighbour exceeds its index by more
  than the tolerance
- **THEN** no further samples are traced for that pixel

#### Scenario: A pixel that has seen no light does not stop early

- **WHEN** adaptive sampling is enabled and every sample a pixel has taken
  returned zero radiance
- **THEN** the pixel keeps sampling past the minimum sample count, taking the
  full per-pixel budget if no sample ever returns light

#### Scenario: The minimum grows with the budget

- **WHEN** a render asks for 1024 samples per pixel with
  `crust:minSamplesPerPixel = 8`
- **THEN** no pixel stops before 32 samples

#### Scenario: A less converged cross neighbour holds a pixel

- **WHEN** a pixel's index is 0.5, the tolerance is 1, and its left
  neighbour is still sampling with an index of 2
- **THEN** the pixel keeps sampling until that neighbour's index falls to 1.5
  or below, or that neighbour stops, or the budget is exhausted

#### Scenario: A diagonal neighbour does not hold a pixel

- **WHEN** the only neighbour of a pixel whose index exceeds its own by more
  than the tolerance is diagonal to it
- **THEN** the pixel stops as if that neighbour were converged

#### Scenario: A negative tolerance is the per-pixel stop

- **WHEN** `crust:adaptiveNeighbourTolerance` is negative
- **THEN** every pixel takes exactly the samples it would take if it were
  rendered alone

#### Scenario: Tiles and scanlines agree under the neighbour comparison

- **WHEN** a scene renders with a non-negative tolerance under both the
  tiled and the scanline strategy
- **THEN** the two images are bit-identical

### Requirement: Rendering a region of the frame

When the render has a region smaller than the frame, the renderer SHALL
trace only the pixels inside it. It SHALL keep the full-frame camera,
resolution and per-pixel sampling. Everything derived from the resolution
SHALL be computed for the full frame:

- ray-cone texture filtering;
- adaptive subdivision's screen rate;
- frustum culling.

A pixel's value SHALL be bit-identical to the same pixel in a full-frame
render of the same scene and settings whenever its sample count does not
depend on its neighbours: a fixed sample count, or adaptive sampling with
no neighbour tolerance. Under the adaptive neighbour hold, a neighbour
outside the region SHALL count as absent.

#### Scenario: A crop matches the full render

- **WHEN** `samples/cornellbox.usda` is rendered at `-s 16` once full-frame
  and once with `--region 37,21,101,77`
- **THEN** every pixel, every AOV channel and `sampleCount` of the crop are
  bitwise equal to the full render's at the same coordinates

#### Scenario: Tiles and scanlines agree on a region

- **WHEN** the same region is rendered once with tiles and once with
  `--scanline`
- **THEN** the two images are bitwise equal

#### Scenario: The full frame is unchanged

- **WHEN** no region is authored and `--region` is not given
- **THEN** the image is bitwise equal to the image rendered before this
  change

### Requirement: A render publishes snapshots while it runs

When a render is given a control that takes snapshots, each work unit SHALL
publish its pixels' current estimates to the control as it finishes a stage or an
adaptive round. Any thread SHALL be able to read the latest region-sized beauty
image and a generation count that increases with every publish. A pixel not yet
sampled reads as zero. A render without a control, or with a control that only
cancels, publishes nothing.

#### Scenario: The image improves while the render runs

- **WHEN** a thread reads the control's snapshot while a 256 spp render runs
- **THEN** it gets a full-region image whose pixels hold the estimates their units
  last published, and a later read with a higher generation reflects more samples

#### Scenario: The last snapshot is the final image

- **WHEN** a render completes and the control's snapshot is read
- **THEN** it equals the returned image bitwise

#### Scenario: A control that only cancels

- **WHEN** a render runs with a control created without snapshots
- **THEN** no snapshot is ever available from it, and cancelling it still stops the
  render

### Requirement: A cancelled render returns what it traced

A cancelled render SHALL return the image, AOVs and ray counters of every
sample it traced, each pixel estimated from its own samples. A pixel that received
no sample SHALL be zero in the image and hold its AOV's clear value, never NaN.

#### Scenario: Partial image

- **WHEN** a render is cancelled after its 4 spp stage completed
- **THEN** every pixel holds an estimate from at least 4 samples, and the ray
  counters cover exactly the traced samples

#### Scenario: No NaN from an unsampled pixel

- **WHEN** a render is cancelled before every pixel received a sample
- **THEN** the unsampled pixels are zero and no pixel of the image or AOVs is NaN

## ADDED Requirements

### Requirement: Scanline and bucket rendering agree

The renderer SHALL offer two Rayon-parallel execution strategies that produce a
**bit-identical** image buffer: a scanline strategy
(`render`) parallel over pixels within each row, and a tiled strategy
(`render_with_tiles`) parallel over 16×16 buckets. The choice is the caller's —
the `Renderer` API favours neither — and the CLI defaults to tiles (see the cli
spec). Progress callbacks SHALL be delivered one at a time, with the completed
count increasing by one per report, under either strategy.

#### Scenario: Scanline rendering

- **WHEN** `render()` is called (CLI `--scanline`)
- **THEN** it fills the buffer, parallelising pixels within each scanline

#### Scenario: Bucket rendering

- **WHEN** `render_with_tiles()` is called (the CLI default)
- **THEN** it divides the image into 16×16 tiles rendered in parallel and
  reassembles them into the same buffer

## REMOVED Requirements

### Requirement: Parallel scanline and bucket rendering

**Reason**: Replaced by "Scanline and bucket rendering agree", which states the same contract without the guided-render clause and scenario; a delta cannot drop a scenario from a modified requirement.

**Migration**: None; the scanline and bucket strategies stay bit-identical.

### Requirement: Opt-in path guiding

**Reason**: Path guiding is removed: too complex for this project, for a benefit too small to pay for it.

**Migration**: Delete `crust:pathGuiding`, `crust:guidingTrainIterations` and `crust:guidingProb` from stages; the import warns about them (usd-scene-import) and renders unguided.

### Requirement: A guided render publishes its passes

**Reason**: Only a guided render had more than one pass.

**Migration**: None; a render is one pass.

### Requirement: A cancelled guided render blends what it completed

**Reason**: Only a guided render had passes to blend.

**Migration**: None; a render is one pass.
