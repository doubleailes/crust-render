# Spec Delta

## ADDED Requirements

### Requirement: Every pass described

With `--all-passes`, the report SHALL describe the render with no active pass,
then each pass in `crust ls pass` order. Each pass SHALL give its path, settings
prim, holdout, camera-hidden and render-hidden counts, whether it renders depth
of field and motion blur, and its products with their files. It SHALL also list
each quantity that differs from the no-pass render (camera, resolution, depth of
field, motion blur, each effective setting) with both values.

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

#### Scenario: A stage with no passes

- **WHEN** the user runs `crust check -i samples/cornellbox.usda --all-passes`
- **THEN** the report describes the no-pass render and lists no passes, and the
  exit status is 0

### Requirement: Passes that write the same file are a finding

With `--all-passes`, two passes whose products would write the same file for the
checked frame SHALL be reported as the `passes_share_output` finding. Its
evidence SHALL name the file and both passes. The finding SHALL not change the
exit status.

#### Scenario: Two passes on the default path

- **WHEN** two passes read settings that author no `RenderProduct`
- **THEN** the report has one `passes_share_output` finding naming the default
  beauty file and both passes

## MODIFIED Requirements

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
