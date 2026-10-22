# scene-check Specification

## Purpose

`crust check` is a pre-flight step before a render. It imports a stage as a render
would, renders nothing, and reports what the render would use, what crust refused,
approximated or skipped, and which settings are worth changing, as text or as
versioned JSON.

## Requirements

### Requirement: Check imports as a render would

`crust check` SHALL import the stage given by `-i` as `crust render` with the same
scene-shaping flags would, so that every reported value is the one that render would
use. `-i` SHALL be required.

#### Scenario: Same camera as the render

- **WHEN** a stage's `RenderSettings.camera` names a prim that is not a camera, and
  the user runs `crust check -i scene.usda`
- **THEN** the report's camera is the fallback camera `crust render -i scene.usda`
  would render through

#### Scenario: No input

- **WHEN** the user runs `crust check`
- **THEN** the arguments are refused as a usage error and nothing is loaded

### Requirement: Check changes nothing

`crust check` SHALL render no pixel and write no image, AOV file or file beside the
stage, except the `.tx` files `--auto-tx` creates, as a render would.

#### Scenario: A stage with products

- **WHEN** a stage authors two `RenderProduct`s and the user runs
  `crust check -i scene.usda`
- **THEN** neither product's file exists afterwards, and the report names both files

### Requirement: The render described

The report SHALL give:

- the stage path and the frame;
- the camera path, the resolution and the region (`null` for the whole frame);
- the active pass (`null` with none), the `RenderSettings` prim the render reads
  (`null` when the stage has none and the render uses the defaults), and, with a
  pass, how many geometries are holdouts, how many are hidden from the camera,
  and how many are hidden from every ray;
- every product, with the file a render would write and the channels in it;
  the file is the path with its frame variables expanded for the checked frame.

A stage with no products SHALL report the single beauty image a render would write.

#### Scenario: A stage with AOVs

- **WHEN** a stage's product orders `color`, `albedo` and `normal` RenderVars
- **THEN** the product lists its file and the channels of all three vars

#### Scenario: No products

- **WHEN** the stage authors no `RenderProduct`
- **THEN** one product is reported: the beauty image a render would write by default

#### Scenario: A product with a frame variable

- **WHEN** a stage's product authors `productName = "renders/beauty.$F4.exr"` and the
  user runs `crust check -i scene.usda -f 5`
- **THEN** the product's file is reported as `renders/beauty.0005.exr`

#### Scenario: A pass with its own settings

- **WHEN** the user runs `crust check -i shot.usda --pass /Render/Passes/char`,
  whose `renderSource` targets `/Render/charSettings` and whose `matte` holds out
  three meshes
- **THEN** the report names the pass, `/Render/charSettings` as the settings, and
  three holdouts

#### Scenario: A pass that hides geometry from every ray

- **WHEN** the active pass's `renderVisibility` leaves out two meshes
- **THEN** the report gives two geometries hidden from every ray

#### Scenario: No pass

- **WHEN** the user runs `crust check -i shot.usda` with no `--pass`
- **THEN** the pass is reported as `null` and the settings as the stage's
  `renderSettingsPrimPath` prim

### Requirement: Effective settings with the names that change them

The report SHALL list every setting the render would run with. Each entry SHALL
give its name and value, and the `crust render` flag and `crust:*` attribute that
change it (`null` where there is none), in the same shape and names as
`crust diagnostic`'s effective settings.

#### Scenario: A flag overrides the stage

- **WHEN** the stage authors `crust:lightSelection = "uniform"` and the user runs
  `crust check -i scene.usda --light-selection power`
- **THEN** `light_selection` is reported as `power`, with flag `--light-selection`
  and attribute `crust:lightSelection`

### Requirement: Import costs and scene counts

The report SHALL give the import's phases, each with its wall time and peak memory,
and the scene counts (geometries, primitives, lights, volumes), using the keys
`crust-stats/1` uses for the same quantities. A quantity the platform cannot measure
SHALL be `null`.

#### Scenario: Counting lights

- **WHEN** the Cornell box stage is checked
- **THEN** its light count matches the `scene` section of
  `crust render -i samples/cornellbox.usda --stats-json -`

### Requirement: Findings that need no render

The report SHALL include every static finding of `crust diagnostic` that can be
decided from the import alone. Each SHALL have the same id, kind, evidence and
action that `crust diagnostic` gives for the same stage and flags. Findings that
need a rendered baseline SHALL NOT appear.

#### Scenario: Textures without `.tx`

- **WHEN** a stage's UV textures have no `.tx` beside them and `--auto-tx` is not
  given
- **THEN** the report has the `textures_without_tx` finding with its count and the
  action `--auto-tx`, as `crust diagnostic` reports it

#### Scenario: A baseline-only finding

- **WHEN** any stage is checked
- **THEN** no `firefly_energy`, `clamp_bias` or `light_sampling_misses` finding
  appears

### Requirement: Warnings

The report SHALL include the import's warning records, as the `scene-warnings`
capability defines them, in the order they were raised.

#### Scenario: Skipped lights

- **WHEN** a stage authors two `RectLight`s with zero width
- **THEN** the report has one `light.degenerate_shape` warning of kind `skipped`,
  with `count` 2 and both light paths

### Requirement: Text report

Without `--json -`, `crust check` SHALL print a text report on stdout, in this order:

1. the render described;
2. with `--all-passes`, each pass, with what differs from the render described;
3. effective settings;
4. import costs and counts;
5. findings, one line each with id, kind, summary and action;
6. warnings, one line each with kind, code, count and first prims;
7. a closing line that totals the findings and the warnings by kind.

The log SHALL go to stderr, so stdout holds only the report.

#### Scenario: A clean stage

- **WHEN** a stage raises no warning and no finding
- **THEN** the closing line says there are no findings and no warnings

#### Scenario: Passes in the text report

- **WHEN** the user runs `crust check -i samples/render_passes.usda --all-passes`
- **THEN** stdout names `/Render/Passes/char` and `/Render/Passes/props`, each
  with its settings prim and products, after the render described

### Requirement: JSON report

`--json PATH|-` SHALL write one JSON object of format `crust-check/2`, following the
shared report shape, with keys in this fixed order:

- `format`, `crust_version`;
- `scene`, `pass`, `settings`, `passes`, `products`, `effective_settings`;
- `import`, `counts`;
- `findings`, `warnings`;
- `denied`: the codes of the warnings that a `--deny` matched, or an empty array.

`passes` SHALL be `null` without `--all-passes`, and otherwise an array with one
object per pass, in the order the text report gives them.

With `-`, the JSON SHALL replace the text report on stdout. With a path, the text
report SHALL still go to stdout.

#### Scenario: JSON on stdout

- **WHEN** the user runs `crust check -i scene.usda --json -`
- **THEN** stdout parses as one JSON object with `format` `crust-check/2`, and the
  log appears on stderr

#### Scenario: JSON to a file

- **WHEN** the user runs `crust check -i scene.usda --json check.json`
- **THEN** `check.json` holds the report and the text report is on stdout

#### Scenario: Passes in the JSON

- **WHEN** the user runs `crust check -i samples/render_passes.usda --all-passes --json -`
- **THEN** `passes` holds two objects, the first with `path`
  `/Render/Passes/char` and `settings` `/Render/charSettings`

#### Scenario: No all-passes

- **WHEN** the user runs `crust check -i samples/render_passes.usda --json -`
- **THEN** `passes` is `null`

### Requirement: Denying warning kinds

`--deny` SHALL take a comma-separated list of the kinds `refused`, `approximated`
and `skipped`, or `all`. An unknown kind SHALL be a usage error. When the import
raises a warning of a denied kind, `crust check` SHALL write its reports, with those
codes in `denied`, and exit 3.

#### Scenario: Denying skipped warnings

- **WHEN** a stage skips a light and the user runs
  `crust check -i scene.usda --deny skipped --json -`
- **THEN** the JSON's `denied` holds `light.degenerate_shape` and the process exits 3

#### Scenario: A kind not raised

- **WHEN** a stage raises only `approximated` warnings and the user passes
  `--deny refused,skipped`
- **THEN** `denied` is empty and the process exits 0

#### Scenario: An unknown kind

- **WHEN** the user passes `--deny fatal`
- **THEN** the arguments are refused as a usage error

### Requirement: Exit status

`crust check` SHALL exit:

- 0 when the import succeeds and no denied warning was raised;
- 3 when a denied warning was raised;
- 1 when the stage cannot be opened or imported, or a report cannot be written;
- 2 on a usage error.

Findings SHALL NOT affect the exit status.

#### Scenario: A stage that cannot be opened

- **WHEN** the user runs `crust check -i missing.usda`
- **THEN** the error is logged on stderr, no report is written, and the process
  exits 1

#### Scenario: Findings alone

- **WHEN** a stage has a `textures_without_tx` finding, no warnings, and `--deny all`
  is given
- **THEN** the process exits 0

### Requirement: Every pass described

With `--all-passes`, the report SHALL describe the render with no active pass,
then each pass in `crust ls pass` order. Each pass SHALL give its path, settings
prim, holdout, camera-hidden and render-hidden counts, whether it renders depth
of field and motion blur, and its products with their files. It SHALL also list
each quantity that differs from the no-pass render (camera, resolution, depth of
field, motion blur, each effective setting) with both values. The warnings SHALL
add what rendering each pass would raise that the no-pass render does not:
reading the pass and choosing its products, each code's occurrences on the prims
the no-pass record does not name, and denied as its own.

#### Scenario: A depth of field that leaks into a pass

- **WHEN** `/Render/settings` renders a camera with depth of field, and the stage's
  `/Render/Passes/util` reads `/Render/utilSettings`, which inherits
  `/Render/settings` and changes only the product
- **THEN** the util pass reports depth of field on, and lists only its product as
  different from the no-pass render

#### Scenario: An override that applies

- **WHEN** `/Render/utilSettings` also authors `disableDepthOfField = 1`
- **THEN** the util pass lists depth of field as on in the no-pass render and off
  in the pass

#### Scenario: A product only a pass refuses

- **WHEN** the no-pass render refuses its product `/Render/P/unnamed_base` (no
  `productName`), and a pass's settings name another such product,
  `/Render/P/unnamed_a`
- **THEN** the `product.no_name` record names both prims with a count of 2, and
  `--deny skipped` denies it

#### Scenario: A stage with no passes

- **WHEN** the user runs `crust check -i samples/cornellbox.usda --all-passes`
- **THEN** the report describes the no-pass render and lists no passes, and the
  exit status is 0

### Requirement: Passes that write the same file are a finding

With `--all-passes`, two passes that would write the same file for the checked
frame, as `crust render --all-passes` compares them (products and PNG previews,
paths resolved against the working directory), SHALL be reported as the
`passes_share_output` finding. Its evidence SHALL name the file and both passes.
The finding SHALL not change the exit status.

#### Scenario: Two passes on the default path

- **WHEN** two passes read settings that author no `RenderProduct`
- **THEN** the report has one `passes_share_output` finding naming the default
  beauty file and both passes

#### Scenario: Two spellings of one file

- **WHEN** one pass writes `./x.exr` and another `x.exr`
- **THEN** the report has a `passes_share_output` finding naming `./x.exr` and
  both passes
