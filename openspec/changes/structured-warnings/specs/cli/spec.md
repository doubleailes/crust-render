# Spec Delta

## MODIFIED Requirements

### Requirement: Subcommands

The CLI SHALL require a subcommand:

- `render`, which renders;
- `ls`, which lists;
- `check`, which imports a stage as a render would and reports its warnings
  (see the `scene-warnings` capability), rendering nothing;
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

## ADDED Requirements

### Requirement: Checking a stage

`crust check` SHALL import the stage given by `-i` exactly as `crust render` with
the same scene flags would. It SHALL render nothing and write no image, but for the
`.tx` files `--auto-tx` creates, as a render would. `-i` SHALL be required: there is
no procedural fallback to check.

#### Scenario: Checking a stage

- **WHEN** the user runs `crust check -i samples/cornellbox.usda`
- **THEN** the stage is imported, no EXR or PNG is written, and the process exits 0

#### Scenario: No input

- **WHEN** the user runs `crust check`
- **THEN** the arguments are refused as a usage error

### Requirement: The check's text report

Without `--json -`, `crust check` SHALL print to stdout:

- the camera path, resolution and products a render would use;
- one line per warning record (its kind, code, count and first prims);
- the total number of warnings.

The log SHALL go to stderr, so stdout holds only the report.

#### Scenario: A stage with skipped lights

- **WHEN** a stage authors two `RectLight`s with zero width, and the user runs
  `crust check -i scene.usda`
- **THEN** stdout has a `skipped` line for `light.degenerate_shape` with count 2
  and both light paths

#### Scenario: A clean stage

- **WHEN** the stage raises no warning
- **THEN** stdout says there are no warnings, after the camera, resolution and
  products

### Requirement: The check as JSON

`--json PATH|-` SHALL write the check as one JSON object of format `crust-check/1`,
following the shared report shape. It SHALL hold the input path, the frame, the
render's camera, resolution and products, the scene counts, and `warnings`: the
records in the shape the `scene-warnings` capability defines. With `-`, the JSON
replaces the text report on stdout.

#### Scenario: JSON on stdout

- **WHEN** the user runs `crust check -i scene.usda --json -`
- **THEN** stdout parses as one JSON object whose `format` is `crust-check/1` and
  whose `warnings` is an array, and the log appears on stderr

#### Scenario: JSON to a file

- **WHEN** the user runs `crust check -i scene.usda --json check.json`
- **THEN** `check.json` holds the report, and the text report still goes to stdout

### Requirement: Denying warning kinds

`--deny` SHALL take a comma-separated list of kinds (`refused`, `approximated`,
`skipped`) or `all`. When the import raises any warning of a denied kind,
`crust check` SHALL still write its reports and SHALL exit 3. An unknown kind SHALL
be a usage error.

#### Scenario: Denying skipped warnings

- **WHEN** a stage skips a light, and the user runs
  `crust check -i scene.usda --deny skipped`
- **THEN** the report is written and the process exits 3

#### Scenario: A kind not raised

- **WHEN** the stage raises only `approximated` warnings and the user passes
  `--deny refused,skipped`
- **THEN** the process exits 0

#### Scenario: An unknown kind

- **WHEN** the user passes `--deny fatal`
- **THEN** the arguments are refused as a usage error

### Requirement: Check exit codes

`crust check` SHALL exit 0 when the import succeeds and no denied warning was
raised, 3 when a denied warning was raised, 1 when the stage cannot be opened or
imported or a report cannot be written, and 2 on a usage error.

#### Scenario: A stage that cannot be opened

- **WHEN** the user runs `crust check -i missing.usda`
- **THEN** the error is logged on stderr, no report is written, and the process
  exits 1
