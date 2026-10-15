## ADDED Requirements

### Requirement: A batched sweep leaves the render bit-identical

A render with the batched sweep (`CRUST_WAVEFRONT=1`) SHALL produce the same bits as the
same render without it: every image, AOV plane, light-path inspection record, render
counter and guiding field. This SHALL hold under tiles and scanlines, staged and unstaged,
adaptive or not, and for guided renders. The batched sweep changes the order in which
paths advance, never what any path draws or computes.

#### Scenario: Goldens are unchanged

- **WHEN** `scripts/check_images.sh check` runs with `CRUST_WAVEFRONT=1` against goldens
  recorded without it
- **THEN** every image compares identical

#### Scenario: Both strategies, staged and unstaged

- **WHEN** a sample scene is rendered batched and one path at a time, each under tiles
  and scanlines, staged and in a single sweep
- **THEN** all eight images are bitwise equal

#### Scenario: A guided render

- **WHEN** `cornellbox_guided.usda` is rendered batched and one path at a time
- **THEN** the guiding training samples of every pass, the pass blend and the image are
  bitwise equal

#### Scenario: AOVs and inspection

- **WHEN** `aovs_lpe.usda` is rendered batched and one path at a time, and
  `crust light-paths` inspects the same pixel under both
- **THEN** every AOV plane is bitwise equal and the inspection reports list the same
  records

### Requirement: A pixel accumulates its samples in sample order

Whatever order its paths finish in, a pixel SHALL add each sample's radiance, filter
weight, luminance moments, AOV contributions and measured clamp to its sums in
increasing sample index, as the one-path-at-a-time sweep does. The adaptive stop SHALL
therefore see the same sums at the same sample counts.

#### Scenario: Paths of one pixel finish out of order

- **WHEN** a pixel's sample 3 terminates at its first bounce and sample 2 runs to the
  maximum depth in the same batch
- **THEN** sample 2's contribution is added before sample 3's

#### Scenario: Adaptive stopping

- **WHEN** an adaptive render (`-s 64`, a non-zero variance threshold) runs batched and
  one path at a time
- **THEN** every pixel takes the same number of samples under both

### Requirement: A batch stays within one work unit's stage

A batch SHALL hold paths of a single work unit (tile or row) and a single stage of the
first sweep or adaptive round, made of whole pixel advances. A cancelled batched render
SHALL stop starting new batches and return within the time of the batches in flight,
keeping every pixel advance a finished batch completed.

#### Scenario: Cancellation mid-render

- **WHEN** a batched render is cancelled during its first sweep
- **THEN** it returns the image, AOVs and counters of every pixel advance its finished
  batches completed, and every unsampled pixel is zero, never NaN

#### Scenario: Batch cap

- **WHEN** a 16x16 tile's stage needs 64 samples per pixel (16,384 paths) and the batch
  cap is smaller
- **THEN** the stage runs as consecutive batches of whole pixels, in pixel order, and
  the result is unchanged
