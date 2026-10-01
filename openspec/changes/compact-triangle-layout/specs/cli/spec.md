## MODIFIED Requirements

### Requirement: Command-line argument parsing

The CLI SHALL accept `-i/--input` (USD scene path), `-o/--output` (output path,
default `output.exr`), `-l/--level` (log verbosity, default `info`),
`--scanline` (row-order rendering instead of the default 16×16 tiles),
`-s/--samples` (override the scene's samples-per-pixel), `--strategy`
(override the scene's MIS sampling strategy: `power` | `balance` | `light` |
`bsdf`), `--subdiv-level <n>` (override the scene's subdivision refinement
level, 0–6; clamped to 6 with a warning), and `--geometry-layout packed|compact`
(override the scene's triangle storage layout). `-b/--bucket` SHALL still be accepted,
for compatibility with existing command lines, and SHALL have no effect.

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

#### Scenario: Overriding the geometry layout

- **WHEN** `--geometry-layout compact` is passed
- **THEN** every committed triangle scene uses the compact layout, whatever the
  scene's `crust:geometryLayout`, and the output image is bit-identical to the
  packed render
