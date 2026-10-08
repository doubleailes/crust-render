# Spec Delta

## MODIFIED Requirements

### Requirement: Subcommands

The CLI SHALL require a subcommand: `render`, which renders, `ls`, which
lists, or `diff`, which compares two EXRs (see the `image-comparison`
capability). The render flags, `--log-file` among them, SHALL belong to `render`;
`-l/--level` SHALL be accepted by every subcommand, before or after its name.
`--log-file`'s directory is optional, so it SHALL NOT be accepted where it
could take a subcommand's name or a positional as that directory.

#### Scenario: No subcommand

- **WHEN** the user runs `crust -i scene.usda`
- **THEN** the arguments are refused as a usage error and nothing is loaded

#### Scenario: Comparing two images

- **WHEN** the user runs `crust diff a.exr b.exr`
- **THEN** the two files are compared and nothing is rendered

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

## ADDED Requirements

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
