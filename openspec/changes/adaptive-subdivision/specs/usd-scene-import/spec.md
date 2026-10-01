## ADDED Requirements

### Requirement: Adaptive subdivision level

When a target edge length is given (`float crust:subdivisionEdgeLength` on the
`RenderSettings` prim, in pixels, or the host override `--subdiv-edge-length`, which
takes precedence), the importer SHALL choose each subdivision mesh's refinement level
per placement instead of using one level for the load.

The chosen level SHALL be the smallest level `L` at which the mesh's mean cage-edge
length, scaled by the placement's transform and by `2^-L`, projects to no more than
the target. The projection SHALL be done at the nearest point of the placement's
world bounds to the render camera at shutter open, and SHALL use the render
resolution.

The level SHALL be clamped to `0..=max`:
- `max` is the resolved `crust:subdivisionLevel` / `--subdiv-level` when either is
  given, and 3 otherwise;
- a camera inside a placement's bounds SHALL give `max`.

The level SHALL depend only on the placement, the camera, the resolution and the
settings, never on import order. An instanced prototype placed at distances that ask
for different levels SHALL be refined separately for each, and each placement SHALL
render the version its own distance asks for.

Without a target edge length, the uniform level SHALL apply unchanged. With a target
but no camera resolvable before traversal (from `--camera` or `RenderSettings.camera`),
the importer SHALL warn once and apply the uniform level.

A target edge length that is not a positive finite number SHALL be ignored with a
warning.

#### Scenario: Near and far copies of one cage

- **WHEN** two prims reference the same `catmullClark` cage, one filling the frame and
  one far enough that its edges project below the target at level 0, and
  `crust:subdivisionEdgeLength = 2` is authored
- **THEN** the near one is refined to a level above 0, and the far one renders its
  smooth cage

#### Scenario: Instances at different distances

- **WHEN** a `PointInstancer` places one subdivision prototype both near the camera
  and far from it, in adaptive mode
- **THEN** the near placements render a more refined version than the far ones, and
  placements whose distances ask for the same level share one version

#### Scenario: The level setting caps adaptive refinement

- **WHEN** adaptive mode would choose level 4 for a mesh and `--subdiv-level 2` is
  given
- **THEN** the mesh is refined to level 2

#### Scenario: No camera known before traversal

- **WHEN** a target edge length is given but neither `--camera` nor
  `RenderSettings.camera` names a camera
- **THEN** a single warning is logged and every subdivision mesh is refined to the
  uniform level

#### Scenario: Deterministic across import modes

- **WHEN** the same adaptive render is imported streamed and with
  `CRUST_STREAM_IMPORT=0`
- **THEN** every mesh placement is refined to the same level in both imports
