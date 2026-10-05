# cli Specification

## Purpose

The command-line entry point (the `crust` binary of the `crust-render` crate, `main.rs`). It parses
arguments, builds a `Scene` from USD or a procedural fallback, runs the renderer,
and writes the output image. This is the only user-facing surface of the tool.
## Requirements
### Requirement: Command-line argument parsing

The CLI SHALL accept the following flags:

- `-i/--input`: the USD scene path.
- `-o/--output`: the output path. When the stage authors RenderProducts, this
  overrides the first product's path. Otherwise it is the single EXR's path,
  default `output.exr`.
- `-l/--level`: log verbosity, default `info`.
- `--scanline`: row-order rendering instead of the default 16×16 tiles.
- `-s/--samples`: override the scene's samples-per-pixel.
- `--strategy`: override the scene's MIS sampling strategy: `power` |
  `balance` | `light` | `bsdf`.
- `--subdiv-level <n>`: override the scene's subdivision refinement level,
  0–6; clamped to 6 with a warning.

`-b/--bucket` SHALL still be accepted, for compatibility with existing command
lines, and SHALL have no effect.

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

#### Scenario: Overriding the subdivision level

- **WHEN** `--subdiv-level 0` is passed on a scene whose meshes author
  `subdivisionScheme = "catmullClark"`
- **THEN** those meshes render as their cages, whatever the scene's
  `crust:subdivisionLevel`

#### Scenario: Output overrides the first product

- **WHEN** the stage authors products `a.exr` and `b.exr`, and the user passes
  `-o out/x.exr`
- **THEN** the first product is written to `out/x.exr` and the second to
  `b.exr`

### Requirement: Adaptive subdivision flag

The CLI SHALL accept `--subdiv-edge-length <px>`: a positive target cage-edge length
in pixels that turns on adaptive subdivision for this render. It overrides the
scene's `crust:subdivisionEdgeLength`. With it, `--subdiv-level` SHALL act as the
maximum level. A value that is not a positive finite number SHALL be rejected at
argument parsing.

#### Scenario: Turning on adaptive subdivision from the command line

- **WHEN** `--subdiv-edge-length 2 --camera /cam` is passed on a scene with
  subdivision meshes and no authored edge length
- **THEN** each subdivision mesh is refined to the level its on-screen size asks for,
  at most 3

#### Scenario: Rejecting a bad edge length

- **WHEN** `--subdiv-edge-length 0` or a negative value is passed
- **THEN** argument parsing fails with a message naming the flag

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

### Requirement: The stats report shows the geometry layout

With `--stats`, the kernel-memory block SHALL list, besides its existing rows, the
bytes held by vertices, per-vertex normals and triangle records, the bytes of gathered
and of indexed triangle packets separately, the share of packet lanes holding a
triangle, and the kernel bytes per resident triangle.

#### Scenario: A subdivided scene's report

- **WHEN** `samples/subdivision.usda` is rendered with `--stats` at `--subdiv-level 3`
- **THEN** the report prints `vertices`, `vertex normals`, `triangle records`,
  `triangle packets (gathered)` or `(indexed)`, `lanes filled` as a percentage and
  `bytes per triangle`, and the rows sum to `kernel memory`

### Requirement: A geometry-layout switch

`CRUST_TRI_PACKETS` (`gathered` | `indexed` | `auto`, default `auto`) SHALL force the
packet layout on every tree; it SHALL be parsed once into `Config`, warn once on a bad
value, and be listed in `docs/architecture.md` with the behaviour before indexed
packets as its off side (`gathered`).

#### Scenario: A bad value

- **WHEN** `CRUST_TRI_PACKETS=fast` is set
- **THEN** one warning names the variable and the render proceeds under `auto`

### Requirement: The render profile reports subsurface walks

With `--profile`, the report SHALL time every subsurface random walk, entry to exit
record, as a section named `Subsurface` nested under `MainLoop`, counted once per
walk. A scene without subsurface walks SHALL show no `Subsurface` row and SHALL
render with the same per-vertex work as before the section existed.

#### Scenario: Walks are their own row

- **WHEN** `samples/materialx_subsurface.usda` is rendered with `--profile`
- **THEN** the render profile lists `Subsurface` under `MainLoop` with a call count
  equal to the `subsurface walks` count of the same report's ray statistics

#### Scenario: A scene without walks pays nothing

- **WHEN** `samples/cornellbox.usda` is rendered with `--profile`
- **THEN** no `Subsurface` row appears, and without `--profile` the render
  executes the same instructions as before the section within 0.01%
