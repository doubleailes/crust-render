# image-output Specification

## Purpose

Persist the rendered pixel buffer to disk. The renderer writes a linear
high-dynamic-range EXR, then converts it to a tone-mapped sRGB PNG for viewing.
Both steps live in `crust-render/src/main.rs`; the engine crate only produces
the `Buffer`.

## Requirements

### Requirement: EXR output

The tool SHALL write the rendered buffer as an RGB EXR image to the `-o/--output`
path (default `output.exr`).

#### Scenario: EXR is written

- **WHEN** a render completes
- **THEN** an EXR file is written at the requested output path with the rendered
  resolution

### Requirement: Tone-mapped sRGB PNG conversion next to the EXR

After writing the EXR, the tool SHALL produce a viewable PNG by clamping linear
values to [0,1], applying the sRGB transfer curve, and quantizing to 8-bit,
saving it next to the EXR at the same path with a `.png` extension.

#### Scenario: PNG is produced from the render

- **WHEN** the render's EXR has been written at `-o` path `renders/foo.exr`
- **THEN** a tone-mapped sRGB PNG is saved at `renders/foo.png`

### Requirement: PNG path always tracks the EXR output path

The PNG conversion step SHALL derive its path from the `-o/--output` value
(swapping the extension to `.png`), so a custom output name keeps the EXR and
PNG in sync.

#### Scenario: Custom output name keeps EXR and PNG in sync

- **WHEN** the user renders with `-o some_other_name.exr`
- **THEN** that EXR is written, and the PNG is written to
  `some_other_name.png` alongside it
