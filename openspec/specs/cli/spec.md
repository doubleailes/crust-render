# cli Specification

## Purpose

The command-line entry point (the `crust` binary of the `crust-render` crate, `main.rs`). It parses
arguments, builds a `Scene` from USD or a procedural fallback, runs the renderer,
and writes the output image (`crust render`), lists what a stage holds
(`crust ls`), compares two EXRs (`crust diff`, the `image-comparison`
capability), or diagnoses how to render it faster or cleaner (`crust diagnostic`,
the `diagnostics` capability). Each can also write a machine-readable JSON report. This is the only user-facing surface of the tool.
## Requirements
### Requirement: Subcommands

The CLI SHALL require a subcommand:

- `render`, which renders;
- `ls`, which lists;
- `check`, which imports a stage as a render would and reports on it without
  rendering (see the `scene-check` capability);
- `diff`, which compares two EXRs (see the `image-comparison` capability);
- `diagnostic`, which measures and reports how to make the render faster
  or cleaner (see the `diagnostics` capability).

The render flags, `--log-file` among them, SHALL belong to `render`. The
flags that shape the scene and its settings SHALL also be accepted by
`diagnostic` and `check`, with the same names, values and defaults:

- `-i`, `-f`, `--camera`, `--region`;
- `--strategy`, `--light-selection`, `--light-samples`,
  `--light-samples-indirect`;
- `--indirect-clamp`, `--filter`, `--filter-radius`;
- `--subdiv-level`, `--subdiv-edge-length`, `--auto-tx`.

`-l/--level` SHALL be accepted by every subcommand, before or after its
name. `--log-file`'s directory is optional, so it SHALL NOT be accepted
where it could take a subcommand's name or a positional as that directory.

#### Scenario: No subcommand

- **WHEN** the user runs `crust -i scene.usda`
- **THEN** the arguments are refused as a usage error and nothing is loaded

#### Scenario: Render-only flags are refused by diagnostic

- **WHEN** the user runs `crust diagnostic -i scene.usda -s 64`
- **THEN** the arguments are refused as a usage error and nothing is loaded

#### Scenario: A shared flag means the same in both

- **WHEN** the user runs
  `crust diagnostic -i scene.usda --light-selection learned`
- **THEN** the report's effective settings show `light_selection = learned`,
  as a render with the same flag would use

#### Scenario: Comparing two images

- **WHEN** the user runs `crust diff a.exr b.exr`
- **THEN** the two files are compared and nothing is rendered

#### Scenario: Render-only flags are refused by check

- **WHEN** the user runs `crust check -i scene.usda -o out.exr`
- **THEN** the arguments are refused as a usage error and nothing is loaded

### Requirement: Listing what a stage holds

`crust ls <kind> -i <scene>` SHALL print the stage's prims of `kind`, one
absolute path per line on stdout, in namespace order, found by the render's
own walk so that the list is what a render would use. `kind` SHALL be one of
`camera`, `light` or `material`, each also accepted in the plural:

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

Its log SHALL go to stderr, so stdout holds only the list.

#### Scenario: Listing the cameras of a stage

- **WHEN** the user runs `crust ls camera -i samples/cornellbox.usda`
- **THEN** stdout is `/scene/camera1` and the exit status is 0

#### Scenario: An unknown kind

- **WHEN** the user runs `crust ls mesh -i scene.usda`
- **THEN** the arguments are refused as a usage error naming the valid kinds

#### Scenario: A stage that cannot be opened

- **WHEN** the input does not exist
- **THEN** an error is logged and the exit status is non-zero

#### Scenario: The text listing is unchanged by metadata

- **WHEN** the user runs `crust ls light -i scene.usda` without `--json`
- **THEN** stdout holds only the lights' paths, one per line, with no metadata

### Requirement: Command-line argument parsing

`crust render` SHALL accept the following flags:

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

### Requirement: Colour-management flags

The CLI SHALL accept `--ocio-config <CONFIG>` (an OCIO config file, archive or
`ocio://` URI, installed before any colour is converted; an error when it
cannot be loaded or lacks the spaces crust needs; without the flag, the
non-empty `OCIO` environment variable names the config, else the builtin ACES
CG config is used), `--working-space <SPACE>`
(overriding `renderingColorSpace`), and `--display` / `--view` for the PNG
preview.

#### Scenario: An unknown view

- **WHEN** `--view "no such view"` is given
- **THEN** the tool exits with an error before rendering

#### Scenario: The OCIO variable as the fallback

- **WHEN** `OCIO` names a config and `--ocio-config` is not given
- **THEN** that config is installed, and one that cannot be loaded is an error
  naming `$OCIO`
- **WHEN** both are given
- **THEN** `--ocio-config` is used and `OCIO` is ignored

### Requirement: Render region flag

`crust render` SHALL accept `--region X0,Y0,X1,Y1`: a rectangle in pixels,
with a top-left origin, half-open (`X1` and `Y1` exclusive). Only the
pixels inside it SHALL be rendered and written. It SHALL take precedence
over any `dataWindowNDC` the stage authors.

- The value SHALL be four non-negative integers with `X1 > X0` and
  `Y1 > Y0`; anything else SHALL be a usage error, raised before the stage
  is loaded.
- The rectangle SHALL be clipped to the render's resolution. A rectangle
  that is empty after clipping SHALL be an error naming the resolution, and
  nothing SHALL be rendered.
- With `--stats`, the report SHALL state the region and the share of the
  frame it covers.

#### Scenario: Rendering a crop

- **WHEN** the user runs
  `crust render -i samples/cornellbox.usda --region 100,50,164,114`
- **THEN** a 64×64-pixel region is rendered, and the EXR's data window is
  `(100, 50)–(163, 113)` inside the full-resolution display window

#### Scenario: A malformed region

- **WHEN** the user runs `crust render -i scene.usda --region 10,10,5,20`
- **THEN** the arguments are refused as a usage error and nothing is loaded

#### Scenario: A region outside the image

- **WHEN** the resolution is 640×360 and the user passes
  `--region 700,0,800,100`
- **THEN** an error names the 640×360 resolution, the exit status is
  non-zero, and no image is written

### Requirement: Machine-readable reports share one shape

Every JSON report the CLI writes SHALL be one JSON object, opening with:

- `format`, naming the report and its version (e.g. `crust-stats/1`);
- `crust_version`.

Keys SHALL be snake_case, and a quantity with a unit SHALL name it
(`time_s`, `peak_bytes`). A value that is not finite, or not available on the
platform, SHALL be `null`. A change that removes or renames a key, or changes
its meaning, SHALL bump the version.

#### Scenario: Memory without procfs

- **WHEN** a render runs with `--stats-json` on a platform without
  `/proc/self/status`
- **THEN** each phase's `rss_end_bytes` and `peak_end_bytes` are `null`, and
  the report still parses

#### Scenario: An infinite metric

- **WHEN** a relative metric is infinite
- **THEN** its value in the JSON is `null`

### Requirement: A JSON report on stdout moves the log to stderr

A flag that writes a JSON report SHALL take a path, or `-` for stdout. With
`-`, stdout SHALL hold only that JSON object, and the run's log, progress and
any text report SHALL go to stderr. With a path, every other output SHALL stay
where it is without the flag.

#### Scenario: Stats on stdout

- **WHEN** the user runs `crust render -i scene.usda --stats-json -`
- **THEN** stdout parses as a single JSON object and the INFO log lines
  appear on stderr

#### Scenario: Stats to a file

- **WHEN** the user runs `crust render -i scene.usda --stats-json stats.json`
- **THEN** `stats.json` holds the report and the log stays on stdout as
  without the flag

### Requirement: Render statistics as JSON

`crust render --stats-json PATH|-` SHALL write the `crust-stats/1` report. It SHALL hold the
same information as `--stats`: phases, scene, image, ray, texture, Ptex,
subdivision and displacement counters, material and light kinds, and with
`--profile`, the render profile under its own key. It SHALL NOT print the
text report unless `--stats` or `--profile` is also given. The top-level peak
memory SHALL be one snapshot shared by both forms.

#### Scenario: Phases and rays

- **WHEN** `samples/cornellbox.usda` is rendered with `--stats-json -`
- **THEN** the report has `phases`, each with `name`, `depth` and `time_s`,
  and `rays` with `camera_rays`, `total_rays` and `mean_path_length`

#### Scenario: Peak memory

- **WHEN** a render runs with `--stats --stats-json stats.json` on Linux
- **THEN** `peak_memory_bytes` in the JSON is the same value the text
  report prints as `peak memory (RSS)`

#### Scenario: Both forms

- **WHEN** the user passes `--stats --stats-json stats.json`
- **THEN** the text report is logged as with `--stats`, and `stats.json`
  holds the same figures

#### Scenario: Profile is marked as such

- **WHEN** the user passes `--profile --stats-json stats.json`
- **THEN** the report has a `profile` object, and without `--profile` it has
  none

### Requirement: Listing metadata as JSON

`crust ls <kind> --json PATH|-` SHALL write the `crust-ls/1` report: the
listed prims in the text listing's order, each with its `path` and the values
a render reads for it. Values SHALL be read as the render reads them, not by
reimplementing the import. An unauthored value SHALL be reported as the value
the render falls back to.

#### Scenario: Camera records

- **WHEN** the user runs `crust ls camera -i samples/cornellbox.usda --json -`
- **THEN** each record has `focal_length_mm`, `aperture_mm` (horizontal,
  vertical), `f_stop`, `focus_distance`, `is_render_camera` and `hidden`

#### Scenario: The camera a render would use

- **WHEN** the first RenderProduct names `/cams/shot`, `RenderSettings.camera`
  names `/cams/layout`, and both are on the stage
- **THEN** only `/cams/shot` has `is_render_camera = true`, as
  `crust render` without `--camera` renders through it

#### Scenario: A settings camera that is missing

- **WHEN** `RenderSettings.camera` names a prim that is not a camera
- **THEN** the camera the render falls back to is the one marked
  `is_render_camera`

#### Scenario: Light records

- **WHEN** the user lists lights as JSON
- **THEN** each record has `type`, `intensity`, `exposure`, `color` and
  `normalize`, as authored on the light (no transform applied)

#### Scenario: Material records

- **WHEN** the user lists materials as JSON
- **THEN** each record has `surface`, the authored surface shader's
  identifier (`null` when the material has none), and `bound`, whether the
  render's binding resolution resolves at least one geometry prim to it

#### Scenario: A binding that never takes effect

- **WHEN** a mesh's own `material:binding` targets `/mtl/a`, and a parent
  binds `/mtl/b` with `bindMaterialAs = strongerThanDescendants`
- **THEN** `/mtl/b` has `bound = true` and `/mtl/a` has `bound = false`,
  unless another prim resolves to it

### Requirement: Listing at a time code

`crust ls` SHALL accept `-f/--frame`, a time code at which every listed value
is evaluated, with the same parsing as `crust render -f`. Without it, values
SHALL be read at their default (non-time-sampled) value, as a render without
`-f` reads them.

#### Scenario: An animated light intensity

- **WHEN** a light's `inputs:intensity` has time samples 1 at frame 1 and 4
  at frame 10, and the user runs `crust ls light -i scene.usda -f 10 --json -`
- **THEN** that light's `intensity` is 4

### Requirement: Interrupting a render keeps its work

Once `crust render` is rendering, the first Ctrl-C (SIGINT) SHALL cancel the render and write
its outputs (EXR or products, and the PNG) from what was traced. It SHALL log a
warning that names the interruption and the samples reached, and exit with
status 130. A Ctrl-C that arrives after the render's last sample but before
`crust render` starts writing its outputs stops nothing: the outputs SHALL be
written complete and unmarked, and the exit status SHALL still be 130. A second
Ctrl-C, one before rendering starts, or a first one once the outputs are being
written, SHALL exit at once with status 130, writing nothing further.

#### Scenario: Ctrl-C during a long render

- **WHEN** the user presses Ctrl-C while `crust render -i scene.usda -o out.exr`
  renders
- **THEN** `out.exr` and `out.png` are written from the partial render, a warning
  says the render was interrupted, and the exit status is 130

#### Scenario: Ctrl-C as the render finishes

- **WHEN** the user presses Ctrl-C after the render traced its last sample but
  before `crust render` starts writing its outputs
- **THEN** the outputs are written complete, without `crust:renderStatus`, and the
  exit status is 130

#### Scenario: A second Ctrl-C

- **WHEN** the user presses Ctrl-C again while the partial outputs are written
- **THEN** the process exits at once with status 130

#### Scenario: Ctrl-C while loading the scene

- **WHEN** the user presses Ctrl-C before rendering has started
- **THEN** the process exits at once with status 130 and writes no output

### Requirement: Checkpoint previews

`crust render` SHALL accept `--checkpoint <SECONDS>`, a positive number. While the
render runs, every interval in which the snapshot changed, the tone-mapped PNG
preview SHALL be rewritten at the path the final PNG will use, from the latest
snapshot. The final outputs SHALL be unchanged by the flag. Without the flag, no
file SHALL be written before the render ends.

#### Scenario: Watching a render

- **WHEN** the user runs `crust render -i scene.usda -o out.exr --checkpoint 10`
- **THEN** `out.png` appears within about ten seconds of the render starting and is
  rewritten every ten seconds, and when the render ends `out.exr` and `out.png`
  are the same files a render without the flag would write

#### Scenario: A non-positive interval

- **WHEN** the user passes `--checkpoint 0`
- **THEN** the arguments are refused as a usage error

#### Scenario: No beauty to preview

- **WHEN** the stage's first product has no beauty var and `--checkpoint` is passed
- **THEN** a warning says no preview will be written, and the render proceeds
