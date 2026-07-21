# image-output Specification

## Purpose

Persist the rendered pixel buffer to disk. The renderer writes a linear
high-dynamic-range EXR, then converts it to a tone-mapped sRGB PNG for viewing.
Covers EXR writing (`main.rs`) and the `convert()` step (`convert.rs`).

## Requirements

### Requirement: EXR output

The tool SHALL write the rendered buffer as an RGB EXR image to the `-o/--output`
path (default `output.exr`).

#### Scenario: EXR is written

- **WHEN** a render completes
- **THEN** an EXR file is written at the requested output path with the rendered
  resolution

### Requirement: Tone-mapped sRGB PNG conversion

After writing the EXR, the tool SHALL produce a viewable PNG by clamping linear
values to [0,1], applying the sRGB transfer curve, and quantizing to 8-bit,
saving a timestamped file under `./test_images/`.

#### Scenario: PNG is produced from the render

- **WHEN** the render's EXR has been written
- **THEN** a timestamped tone-mapped sRGB PNG is saved under `./test_images/`

### Requirement: PNG conversion reads the default EXR path

The PNG conversion step SHALL read a fixed `output.exr` input, independent of the
`-o/--output` value. When a different output name is used, the EXR is still
produced but the PNG step reads the fixed `output.exr` instead.

#### Scenario: Default output keeps EXR and PNG consistent

- **WHEN** the output path is left at the default `output.exr`
- **THEN** the PNG is generated from the same EXR that was just written

#### Scenario: Custom output name desynchronizes the PNG step

- **WHEN** the user renders with `-o some_other_name.exr`
- **THEN** that EXR is written, but the PNG conversion still reads the fixed
  `output.exr` (stale or missing) rather than the custom file
