## ADDED Requirements

### Requirement: The final pass sweeps in stages

A final pass SHALL take every pixel to its first adaptive check point (or the
whole budget when adaptive sampling is off) in stages of 1, 2, 4, 8, … samples per
pixel, each stage covering the whole region before the next starts. A pixel's
convergence test SHALL run only once the last stage is complete. Training passes
are not staged.

#### Scenario: Stages double up to the first check point

- **WHEN** a final pass renders with a minimum of 32 spp and a 64 spp budget
- **THEN** every pixel reaches 1, 2, 4, 8, 16 and 32 samples in turn, frame-wide,
  before the first adaptive round starts

#### Scenario: A non-adaptive render is staged to its budget

- **WHEN** a final pass renders at 100 spp with a variance threshold of 0
- **THEN** the stages are 1, 2, 4, …, 64 and finally 100 samples per pixel

### Requirement: Staging leaves the image bit-identical

A completed render SHALL produce the same bits in its image, AOVs and counters
as the same render without staging. Each pixel SHALL draw the same sample indices
in the same order, and SHALL be checked at the same sample counts against the same
frozen neighbour state.

#### Scenario: Goldens are unchanged

- **WHEN** `scripts/check_images.sh check` runs against goldens recorded before
  staging existed
- **THEN** every image compares identical

#### Scenario: Staged and unstaged passes agree

- **WHEN** the same scene is rendered once with staging and once in a single sweep,
  under tiles and under scanlines
- **THEN** the four images are bitwise equal

### Requirement: A render publishes snapshots while it runs

When a render is given a control, each work unit SHALL publish its pixels' current
estimates to the control as it finishes a stage or an adaptive round. Any thread
SHALL be able to read the latest region-sized beauty image and a generation count
that increases with every publish. A pixel not yet sampled reads as zero. A render
without a control publishes nothing.

#### Scenario: The image improves while the render runs

- **WHEN** a thread reads the control's snapshot while a 256 spp render runs
- **THEN** it gets a full-region image whose pixels hold the estimates their units
  last published, and a later read with a higher generation reflects more samples

#### Scenario: The last snapshot is the final image

- **WHEN** an unguided render completes and the control's snapshot is read
- **THEN** it equals the returned image bitwise

### Requirement: A guided render publishes its passes

During a guided render, each pass SHALL publish as its units finish, training
passes included, so a snapshot shows the pass in progress. The returned image
remains the inverse-variance blend of the passes.

#### Scenario: Training is visible

- **WHEN** a snapshot is read during a guided render's second training pass
- **THEN** it shows that pass's estimates for the units it has finished, and the
  previous pass's for the rest

### Requirement: A render can be cancelled

A caller SHALL be able to cancel a render through its control from any thread. The
render SHALL stop starting new pixel advances, return within the time of the
pixel advances already in flight, and report that it was cancelled. A render that
is not cancelled SHALL report that it completed.

#### Scenario: Cancelling mid-render

- **WHEN** another thread cancels a 4096 spp render after one second
- **THEN** the render call returns shortly afterwards with a cancelled outcome

#### Scenario: Cancelling before the render starts

- **WHEN** the control is cancelled before the render is called
- **THEN** the render returns a cancelled outcome without tracing any sample

### Requirement: A cancelled render returns what it traced

A cancelled unguided render SHALL return the image, AOVs and ray counters of every
sample it traced, each pixel estimated from its own samples. A pixel that received
no sample SHALL be zero in the image and hold its AOV's clear value, never NaN.

#### Scenario: Partial image

- **WHEN** a render is cancelled after its 4 spp stage completed
- **THEN** every pixel holds an estimate from at least 4 samples, and the ray
  counters cover exactly the traced samples

#### Scenario: No NaN from an unsampled pixel

- **WHEN** a render is cancelled before every pixel received a sample
- **THEN** the unsampled pixels are zero and no pixel of the image or AOVs is NaN

### Requirement: A cancelled guided render blends what it completed

A cancelled guided render SHALL return the inverse-variance blend of the passes it
completed. The interrupted pass SHALL join the blend only when every pixel in it
received at least two samples (so its variance can be estimated), or when no pass
completed, in which case it is returned alone.

#### Scenario: Cancelled during the final pass

- **WHEN** a guided render with three training passes is cancelled during its
  final pass, after its 4 spp stage completed
- **THEN** the returned image blends the three training passes and the partial
  final pass

#### Scenario: Cancelled before the final pass reached two samples

- **WHEN** a guided render with three training passes is cancelled during its
  final pass's 1 spp stage
- **THEN** the returned image blends the three training passes alone

#### Scenario: Cancelled during the first training pass

- **WHEN** a guided render is cancelled before its first training pass completes
- **THEN** the returned image is that partial pass, with unsampled pixels zero
