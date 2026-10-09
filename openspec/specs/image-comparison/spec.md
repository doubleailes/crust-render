# image-comparison Specification

## Purpose

Answers "did this image change, and by how much?" for two EXRs: `crust diff`.
It defines bitwise identity, the beauty's error metrics, an exit status that
scripts and agents branch on, a versioned JSON report, and a comparability
verdict read from the sampling stamps crust writes into its EXRs.

## Requirements

### Requirement: Comparing two EXRs

`crust diff <a> <b>` SHALL compare two EXR files. Every named channel of every
layer SHALL be compared, under the name `layer.channel`, or the bare channel name
in an unnamed layer. Two channels SHALL be equal only when every sample is
bitwise equal. A channel present in only one file SHALL count as a difference
in every pixel.

#### Scenario: A NaN that moved

- **WHEN** a pixel's `R` is NaN in `a` and NaN in `b` at a different pixel
- **THEN** both pixels count as differing

#### Scenario: Equal infinities

- **WHEN** a depth channel holds `+inf` at the same pixels in both files
- **THEN** those pixels do not differ

#### Scenario: An AOV missing from one file

- **WHEN** `a` has `albedo.R`, `albedo.G`, `albedo.B` and `b` does not
- **THEN** the report lists those channels as only in `a`, and every pixel
  differs

### Requirement: Exit status answers whether the images are identical

`crust diff` SHALL exit with:

- `0` when the files have the same resolution and every channel is identical;
- `1` when they differ, a resolution mismatch included;
- `2` when either file cannot be read, or the arguments are invalid.

A read failure SHALL be reported as an error message, never a panic.

#### Scenario: Identical renders

- **WHEN** the same scene is rendered twice at `-s 16` and the two EXRs are
  compared
- **THEN** the exit status is 0

#### Scenario: Different resolutions

- **WHEN** `a` is 640×360 and `b` is 320×180
- **THEN** the exit status is 1 and the report states both resolutions

#### Scenario: A missing file

- **WHEN** `b` does not exist
- **THEN** an error naming `b` is printed to stderr and the exit status is 2

### Requirement: Beauty error metrics

When both files have `R`, `G` and `B` at the image's size, the report SHALL
give, against `a` as the reference:

- the largest absolute and relative channel difference;
- the mean absolute difference and the RMSE;
- the relative MSE `(a − b)² / (a² + 0.01)`;
- the same relative MSE with the worst 0.1% of pixels discarded.

Without a beauty in both files, the report SHALL say so and give no metrics.

#### Scenario: A noise comparison against a reference

- **WHEN** `a` is a 1024 spp render and `b` the same scene at 16 spp
- **THEN** the report gives a non-zero `relmse` and a trimmed `relmse` no
  larger than it

#### Scenario: AOV-only products

- **WHEN** neither file has `R`, `G`, `B`
- **THEN** the identity check and per-channel lines are reported, and the
  metrics are absent

### Requirement: Text and JSON reports

By default `crust diff` SHALL print a human-readable report on stdout. Its
first line SHALL give the resolution and the count of differing pixels. With
`--json PATH`, it SHALL also write the `crust-diff/1` report to `PATH`. With
`--json -`, the JSON SHALL go to stdout instead of the text report.

#### Scenario: JSON on stdout

- **WHEN** the user runs `crust diff a.exr b.exr --json -`
- **THEN** stdout parses as one JSON object with `format = "crust-diff/1"`,
  `identical`, `differing_pixels`, `total_pixels`, `channels`, and `beauty`
  when both files have one

#### Scenario: A bounded list of differing pixels

- **WHEN** 10,000 pixels differ
- **THEN** the report lists the values of at most the first 8, in row order

### Requirement: Comparability from sampling stamps

`crust diff` SHALL read both files' `crust:*` sampling stamps and report a
comparability status:

- `unknown` when either file has no stamp;
- `warn` when the stamps show a condition that makes pixel differences
  unreliable;
- `ok` otherwise.

Each warning SHALL be a note naming the condition. Comparability SHALL NOT
change the exit status.

#### Scenario: Adaptive sampling on one side

- **WHEN** `b` was rendered with `crust:spp = 64` and `crust:minSpp = 32`
- **THEN** the status is `warn`, with a note that adaptive sampling was
  active and one-ulp differences can cascade into a pixel's sample budget

#### Scenario: Different firefly clamps

- **WHEN** `a` has `crust:indirectClamp = 10` and `b` has `0`
- **THEN** the status is `warn`, with a note that the clamp differs and the
  metrics include its bias

#### Scenario: Different frames, cameras or sample counts

- **WHEN** the stamps' `crust:frame`, `crust:camera` or `crust:spp` differ
- **THEN** the status is `warn`, with a note naming each field that differs

#### Scenario: Different pixel filters

- **WHEN** `a` was rendered with `--filter gaussian` and `b` with
  `--filter box`, or the same filter at a different radius
- **THEN** the status is `warn`, with a note naming `pixelFilter` or
  `pixelFilterRadius`

#### Scenario: Two builds, same settings

- **WHEN** the stamps differ only in `crust:version`
- **THEN** the status is `ok`

#### Scenario: An EXR from another renderer

- **WHEN** `a` has no `crust:` attributes
- **THEN** the status is `unknown`, the comparison is still made, and the
  exit status depends only on the pixels
