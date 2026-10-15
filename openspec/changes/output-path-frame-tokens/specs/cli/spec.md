# Spec Delta

## MODIFIED Requirements

### Requirement: Command-line argument parsing

`crust render` SHALL accept the following flags:

- `-i/--input`: the USD scene path.
- `-o/--output`: the output path. When the stage authors RenderProducts, this
  overrides the first product's path. Otherwise it is the single EXR's path,
  default `output.exr`. Its frame variables (`$F`, `$F2`–`$F9`, `$FF`, or
  `<F>`, `<F2>`–`<F9>`, `<FF>`) are expanded as `image-output` describes; quote
  it so the shell leaves them alone.
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

- **WHEN** the user runs `crust render -i <scene.usda>`
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

#### Scenario: Output with a frame variable

- **WHEN** the user runs `crust render -i scene.usda -f 40 -o 'out/x.$F4.exr'`
- **THEN** the EXR is written to `out/x.0040.exr`
