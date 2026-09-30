## MODIFIED Requirements

### Requirement: Adaptive sampling stops pixels early

The renderer SHALL stop sampling a pixel once it holds at least
`crust:minSamplesPerPixel` samples, has recorded at least one sample with
non-zero luminance, and the relative standard error of the pixel mean drops
below `crust:varianceThreshold` (0 disables adaptive sampling). A pixel whose
samples have all been exactly zero SHALL keep sampling until it records a
non-zero sample or reaches the full per-pixel budget: a zero measured
variance from zero observations is not evidence of convergence. This applies
to the main/final render pass, never to path-guiding training passes.

#### Scenario: A converged pixel stops early

- **WHEN** a pixel has accumulated at least the minimum sample count, at least
  one of its samples is non-zero, and its relative standard error is below
  the variance threshold
- **THEN** no further samples are traced for that pixel

#### Scenario: A pixel that has seen no light does not stop early

- **WHEN** adaptive sampling is enabled and every sample a pixel has taken
  returned zero radiance
- **THEN** the pixel keeps sampling past the minimum sample count, taking the
  full per-pixel budget if no sample ever returns light
