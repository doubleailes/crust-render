# Spec Delta

## ADDED Requirements

### Requirement: Render pass flag

`crust render` and `crust check` SHALL accept `--pass <path>`, the RenderPass prim to render or
check, as the `render-passes` capability describes. With `--pass`, `-o` SHALL replace the pass's
file pattern rather than the first product's path. `--help` SHALL list the flag.

#### Scenario: Rendering a pass

- **WHEN** the user runs `crust render -i shot.usda --pass /Render/Passes/render -f 12`
- **THEN** the pass is rendered at frame 12 and its pattern's files are written

#### Scenario: Checking a pass

- **WHEN** the user runs `crust check -i shot.usda --pass /Render/Passes/render`
- **THEN** the report describes that pass's files, and nothing is rendered

### Requirement: Pass records in the JSON listing

`crust ls pass --json PATH|-` SHALL give each pass's record: `render_source` (the settings path a
render would use), `layers` (the layer names in order, `default` last), `file_pattern` (the
pattern a render would expand, the default when none is authored) and `layer_pattern` (the
channel-name pattern, `$n_$o` when none is authored).

#### Scenario: Pass records

- **WHEN** a pass targets `/Render/hero_settings`, applies `layer:set` then `layer:hero`, and
  authors no `crust:filePattern`
- **THEN** its record has `render_source = "/Render/hero_settings"`,
  `layers = ["set", "hero", "default"]`, `file_pattern = "$l_$n_$o.$04f.$x"` and
  `layer_pattern = "$n_$o"`

## MODIFIED Requirements

### Requirement: Command-line argument parsing

`crust render` SHALL accept the following flags:

- `-i/--input`: the USD scene path.
- `-o/--output`: the output path. When the stage authors RenderProducts, this
  overrides the first product's path. Otherwise it is the single EXR's path,
  default `output.exr`. Its frame variables (`$F`, `$F2`–`$F9`, `$FF`) are
  expanded as `image-output` describes; quote it so the shell leaves them alone.
  With `--pass`, it instead replaces the pass's file pattern, and is expanded
  with the pattern's specifiers as `render-passes` describes.
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

#### Scenario: Output with a pass is a pattern

- **WHEN** the user runs `crust render -i shot.usda --pass /Render/Passes/render -f 3 -o 'out/$n.$04f.$x'`
- **THEN** each layer is written to `out/<layer>.0003.exr`, and no `[product.unknown_variable]`
  warning is logged for `$n` or `$x`

### Requirement: Listing what a stage holds

`crust ls <kind> -i <scene>` SHALL print the stage's prims of `kind`, one
absolute path per line on stdout, in namespace order, found by the render's
own walk so that the list is what a render would use. `kind` SHALL be one of
`camera`, `light`, `material` or `pass`, each also accepted in the plural:

- `camera`: exactly the cameras the render can go through — none under an
  inactive, abstract or proxy- / guide-purpose ancestor, inside an instance's
  prototype or beneath a `PointInstancer`; those under an invisible ancestor
  included.
- `light`: the UsdLux lights the import reads (sphere, rect, disk, cylinder,
  distant, dome), pruned as geometry is, invisibility included. A light the
  import refuses for its evaluated values (a zero radius or size, a transform
  that collapses it) SHALL still be listed: membership does not depend on the
  time code, even when `-f` is given.
- Each path SHALL be listed once, however many streamed chunks contain it.
- `material`: every `UsdShadeMaterial` a binding can reach — all but those
  under an inactive ancestor or inside an instance's prototype.
- `pass`: every `RenderPass` prim `--pass` accepts — all but those under an
  inactive ancestor or inside an instance's prototype.

Its log SHALL go to stderr, so stdout holds only the list.

#### Scenario: Listing the cameras of a stage

- **WHEN** the user runs `crust ls camera -i samples/cornellbox.usda`
- **THEN** stdout is `/scene/camera1` and the exit status is 0

#### Scenario: Listing the render passes of a stage

- **WHEN** a stage defines the RenderPass prims `/Render/Passes/render` and
  `/Render/Passes/fx`, and the user runs `crust ls pass -i shot.usda`
- **THEN** stdout is those two paths, one per line, and the exit status is 0

#### Scenario: An unknown kind

- **WHEN** the user runs `crust ls mesh -i scene.usda`
- **THEN** the arguments are refused as a usage error naming the valid kinds

#### Scenario: A stage that cannot be opened

- **WHEN** the input does not exist
- **THEN** an error is logged and the exit status is non-zero

#### Scenario: The text listing is unchanged by metadata

- **WHEN** the user runs `crust ls light -i scene.usda` without `--json`
- **THEN** stdout holds only the lights' paths, one per line, with no metadata
