## ADDED Requirements

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
