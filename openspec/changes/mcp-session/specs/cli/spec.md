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

#### Scenario: Starting the MCP server

- **WHEN** an MCP client launches `crust mcp`
- **THEN** the server waits for protocol messages on stdin and loads nothing until
  a session is opened
