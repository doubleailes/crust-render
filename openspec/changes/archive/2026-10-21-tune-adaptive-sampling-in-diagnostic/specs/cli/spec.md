# Spec Delta

## MODIFIED Requirements

### Requirement: Subcommands

The CLI SHALL require a subcommand:

- `render`, which renders;
- `ls`, which lists;
- `check`, which imports a stage as a render would and reports on it without
  rendering (see the `scene-check` capability);
- `mcp`, which serves a live editing and rendering session to an MCP client
  over stdio (see the `mcp-session` capability);
- `diff`, which compares two EXRs (see the `image-comparison` capability);
- `diagnostic`, which measures and reports how to make the render faster
  or cleaner (see the `diagnostics` capability);
- `light-paths`, which re-renders a small region and reports the light paths
  its samples took (see the `light-path-inspection` capability).

The render flags, `--log-file` among them, SHALL belong to `render`. The
flags that shape the scene and its settings SHALL also be accepted by
`diagnostic`, `check` and `light-paths`, with the same names, values and
defaults:

- `-i`, `-f`, `--camera`, `--region`;
- `--strategy`, `--light-selection`, `--light-samples`,
  `--light-samples-indirect`;
- `--indirect-clamp`, `--filter`, `--filter-radius`, `--sampler`;
- `--subdiv-level`, `--subdiv-edge-length`, `--auto-tx`.
- `--variance-threshold`, `--min-samples`, `--neighbour-tolerance`.

`light-paths` SHALL also accept `render`'s `-s`, so that it reproduces a render
made at that sample count.

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

#### Scenario: The sampler is a shared flag

- **WHEN** the user runs `crust check -i scene.usda --sampler sobolBn --json -`
- **THEN** the report's effective settings show `sampler` as `sobolBn`, with flag
  `--sampler` and attribute `crust:sampler`

#### Scenario: An unknown sampler on the command line

- **WHEN** the user runs `crust render -i scene.usda --sampler halton`
- **THEN** the arguments are refused as a usage error listing the six accepted values, and
  nothing is loaded

#### Scenario: Comparing two images

- **WHEN** the user runs `crust diff a.exr b.exr`
- **THEN** the two files are compared and nothing is rendered

#### Scenario: Render-only flags are refused by check

- **WHEN** the user runs `crust check -i scene.usda -o out.exr`
- **THEN** the arguments are refused as a usage error and nothing is loaded

#### Scenario: Starting the MCP server

- **WHEN** an MCP client launches `crust mcp`
- **THEN** the server waits for protocol messages on stdin and loads nothing until
  a session is opened

#### Scenario: Inspecting light paths at the render's sample count

- **WHEN** the user runs
  `crust light-paths -i scene.usda -s 64 --region 40,40,50,50`
- **THEN** the region is rendered at 64 samples per pixel, as `crust render -s 64`
  would render it

#### Scenario: Output flags are refused by light-paths

- **WHEN** the user runs `crust light-paths -i scene.usda --region 0,0,5,5 -o out.exr`
- **THEN** the arguments are refused as a usage error and nothing is loaded

#### Scenario: The adaptive flags are shared

- **WHEN** the user runs
  `crust diagnostic -i scene.usda --variance-threshold 0.02 --min-samples 16`
- **THEN** the report's effective settings show `variance_threshold = 0.02` and
  `min_samples_per_pixel = 16`, as a render with the same flags would use

## ADDED Requirements

### Requirement: Adaptive sampling flags

`crust render` SHALL accept `--variance-threshold <t>` (a number ≥ 0; 0 turns
adaptive sampling off), `--min-samples <n>` (an integer ≥ 1) and
`--neighbour-tolerance <t>` (any number; negative turns the neighbour comparison
off). They SHALL override `crust:varianceThreshold`, `crust:minSamplesPerPixel`
and `crust:adaptiveNeighbourTolerance` for this render only.

#### Scenario: Overriding the adaptive settings

- **WHEN** the stage authors `crust:varianceThreshold = 0.05` and the user runs
  `crust render -i scene.usda --variance-threshold 0.02`
- **THEN** the render stops pixels at a relative error of 0.02, and the stage
  is unchanged

#### Scenario: Turning adaptive sampling off

- **WHEN** the user runs `crust render -i scene.usda --variance-threshold 0 -s 64`
- **THEN** every pixel takes exactly 64 samples

#### Scenario: A negative threshold is refused

- **WHEN** the user runs `crust render -i scene.usda --variance-threshold -1`
- **THEN** the arguments are refused as a usage error and nothing is loaded
