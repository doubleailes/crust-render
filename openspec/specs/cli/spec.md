# cli Specification

## Purpose

The command-line entry point (`crust-render` binary, `main.rs`). It parses
arguments, builds a `Scene` from USD or a procedural fallback, runs the renderer,
and writes the output image. This is the only user-facing surface of the tool.

## Requirements

### Requirement: Command-line argument parsing

The CLI SHALL accept `-i/--input` (USD scene path), `-o/--output` (output path,
default `output.exr`), `-l/--level` (log verbosity, default `info`),
`--scanline` (row-order rendering instead of the default 16×16 tiles),
`-s/--samples` (override the scene's samples-per-pixel), and `--strategy`
(override the scene's MIS sampling strategy: `power` | `balance` | `light` |
`bsdf`). `-b/--bucket` SHALL still be accepted, for compatibility with existing
command lines, and SHALL have no effect.

#### Scenario: Rendering a scene file

- **WHEN** the user runs the binary with `-i <scene.usda>`
- **THEN** the scene is loaded from that USD file and rendered

#### Scenario: Tiled rendering is the default

- **WHEN** neither `--scanline` nor `--bucket` is passed
- **THEN** the renderer uses the tiled (bucket) strategy

#### Scenario: Scanline flag selects row rendering

- **WHEN** `--scanline` is passed
- **THEN** the renderer uses the scanline strategy instead of tiles, and the
  image is bit-identical to the tiled one

#### Scenario: The bucket flag is accepted and ignored

- **WHEN** `--bucket` (or `-b`) is passed
- **THEN** the command line parses and the render is the default tiled one

#### Scenario: Overriding samples and sampling strategy

- **WHEN** `-s <n>` and/or `--strategy <mode>` are passed
- **THEN** they override the scene's `crust:samplesPerPixel` /
  `crust:samplingStrategy` values for this render only

### Requirement: Procedural fallback when no input is given

When no `-i/--input` is provided, the CLI SHALL render a hard-coded procedural
scene (`world::simple_scene` with `get_settings`) instead of failing.

#### Scenario: No input path

- **WHEN** the binary is run without `-i`
- **THEN** it renders the built-in procedural fallback scene

### Requirement: Configurable log verbosity

The CLI SHALL configure `tracing` output at the level chosen by `-l/--level`
(`trace`, `debug`, `info`, `warn`, or `error`), defaulting to `info`.

#### Scenario: Debug logging

- **WHEN** the user passes `-l debug`
- **THEN** debug-level diagnostics (scene path, settings, object counts) are logged
