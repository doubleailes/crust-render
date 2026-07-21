# cli Specification

## Purpose

The command-line entry point (`crust-render` binary, `main.rs`). It parses
arguments, builds a `Scene` from USD or a procedural fallback, runs the renderer,
and writes the output image. This is the only user-facing surface of the tool.

## Requirements

### Requirement: Command-line argument parsing

The CLI SHALL accept `-i/--input` (USD scene path), `-o/--output` (output path,
default `output.exr`), `-l/--level` (log verbosity, default `info`), and
`-b/--bucket` (tiled rendering, default off).

#### Scenario: Rendering a scene file

- **WHEN** the user runs the binary with `-i <scene.usda>`
- **THEN** the scene is loaded from that USD file and rendered

#### Scenario: Bucket flag selects tiled rendering

- **WHEN** `--bucket` is passed
- **THEN** the renderer uses the tiled (bucket) strategy instead of scanline

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
