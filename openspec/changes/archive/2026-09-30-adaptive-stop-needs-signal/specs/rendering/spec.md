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
applies to the main/final render pass, never to path-guiding training
passes, and the image SHALL be bit-identical under the tiled and scanline
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

## ADDED Requirements

### Requirement: Adaptive neighbour tolerance setting

The importer SHALL read `crust:adaptiveNeighbourTolerance` (float, in units
of the convergence index, default 1) from the render settings prim. A
non-finite value SHALL fall back to the default, with a warning naming the
authored value.

#### Scenario: Unauthored tolerance

- **WHEN** `crust:adaptiveNeighbourTolerance` is not authored
- **THEN** the render compares cross neighbours with tolerance 1

#### Scenario: Non-finite tolerance

- **WHEN** `crust:adaptiveNeighbourTolerance = nan` is authored
- **THEN** the render uses tolerance 1 and logs a warning
