## MODIFIED Requirements

### Requirement: Adaptive subdivision level

When a target edge length is given (`float crust:subdivisionEdgeLength` on the
`RenderSettings` prim, in pixels, or the host override `--subdiv-edge-length`, which
takes precedence), the importer SHALL refine **unshared** subdivision geometry by its
size on screen, and **shared** geometry to one uniform level:

- Unshared geometry is a direct mesh prim, and a prototype placed exactly once — a
  native instance whose prototype has no other placement in the same top-level
  subtree of the stage, or the one placement of a `PointInstancer` prototype. Its
  meshes SHALL be tessellated per face (see "Per-face adaptive tessellation"), or,
  where that does not apply, refined to the smallest level `L` at which the mesh's
  mean cage-edge length, scaled by its world transform and by `2^-L`, projects to no
  more than the target at the nearest point of its world bounds to the render camera
  at shutter open, at the render resolution.
- Shared geometry — a prototype placed more than once — SHALL be refined to the
  resolved `crust:subdivisionLevel` / `--subdiv-level` when either is given, and to
  level 0 otherwise, whatever its distance.

Levels from the screen SHALL be clamped to `0..=max`:
- `max` is the resolved `crust:subdivisionLevel` / `--subdiv-level` when either is
  given, and 3 otherwise;
- a camera inside the bounds SHALL give `max`.

Placements SHALL be counted per top-level subtree of the stage in every import mode,
so whether geometry is shared, and every level, depends only on the stage, the
camera, the resolution and the settings, never on import order or on streaming.

Without a target edge length, the uniform level SHALL apply unchanged. With a target
but no camera resolvable before traversal (from `--camera` or `RenderSettings.camera`),
the importer SHALL warn once and apply the uniform level.

A target edge length that is not a positive finite number SHALL be ignored with a
warning.

#### Scenario: Near and far copies of one cage

- **WHEN** two prims reference the same `catmullClark` cage, one filling the frame and
  one far enough that its edges project below the target at level 0, and
  `crust:subdivisionEdgeLength = 2` is authored
- **THEN** the near one is refined, and the far one renders at its cage resolution

#### Scenario: A shared prototype gets the uniform level

- **WHEN** a `PointInstancer` places one subdivision prototype both near the camera
  and far from it, in adaptive mode, with no level setting
- **THEN** every placement renders the same level-0 version, and one copy of it is
  kept

#### Scenario: A prototype placed once is adaptive

- **WHEN** an `instanceable` prim is the only placement of its prototype in its
  top-level subtree, in adaptive mode
- **THEN** its meshes are refined by their size on screen, as a direct mesh at the same
  transform would be

#### Scenario: The level setting caps adaptive refinement and sets the shared level

- **WHEN** `--subdiv-level 2` is given in adaptive mode
- **THEN** no unshared mesh is refined past level 2 (a rate of 4 per edge), and every
  shared prototype is refined to level 2

#### Scenario: No camera known before traversal

- **WHEN** a target edge length is given but neither `--camera` nor
  `RenderSettings.camera` names a camera
- **THEN** a single warning is logged and every subdivision mesh is refined to the
  uniform level

#### Scenario: Deterministic across import modes

- **WHEN** the same adaptive render is imported streamed and with
  `CRUST_STREAM_IMPORT=0`
- **THEN** every mesh placement is refined the same way in both imports

## ADDED Requirements

### Requirement: Per-face adaptive tessellation

In adaptive subdivision (a target edge length is set and the render camera is known
before traversal), the importer SHALL tessellate each **unshared** subdivision mesh
(see "Adaptive subdivision level") per cage face instead of refining the whole mesh to
one level:

- Each cage edge SHALL be split into a number of segments chosen from that edge alone:
  the smallest count at which each segment, stretched by the placement and seen at the
  edge's nearest point to the render camera, projects to at most the target, between 1
  and `2^max`, where `max` is the adaptive ceiling. A mesh of a prototype placed once
  SHALL be rated through that placement's transform, edge by edge, as a direct mesh.
- Two faces sharing a cage edge SHALL split it at the same points, and the tessellated
  surface SHALL have no crack or T-junction between them, whatever the rates of their
  other edges.
- A face with an edge split more than once SHALL be tessellated on the mesh's limit
  surface, every vertex carrying the limit surface's normal there. A face whose every
  edge is split once SHALL render its cage with smooth normals, as level 0 does, its
  corners shared with a refined neighbour taking that neighbour's limit points, so the
  two meet without a crack.
- A cage edge wholly outside the render camera's view SHALL be split once (unless
  `CRUST_ADAPTIVE_FRUSTUM=0`); a mesh on the per-mesh path wholly outside the view
  SHALL take level 0.
- A `loop` mesh SHALL instead be refined to the per-mesh adaptive level, and SHALL be
  counted in `--stats`. A face-varying texture chart SHALL be evaluated on the
  tessellation, each side of a seam keeping its own values.
- A per-face mesh read through Ptex SHALL address its cage faces, as a uniformly
  refined mesh does.
- The edge rates SHALL depend only on the placement, the camera, the resolution and the
  settings, never on import order.

`CRUST_ADAPTIVE_PER_FACE=0` SHALL restore the per-mesh adaptive level. Without a target
edge length nothing here applies, and the output SHALL be unchanged.

#### Scenario: A large mesh near and far

- **WHEN** a single subdivision mesh spans from next to the camera to far beyond the
  distance where its edges project below the target, in adaptive mode
- **THEN** its faces near the camera are split finely and its far faces are not split
  at all, so it holds fewer triangles than the same mesh refined to the per-mesh level

#### Scenario: Watertight between faces of different rates

- **WHEN** two neighbouring cage faces get different edge rates on their other edges
- **THEN** every edge of the tessellation inside the mesh is shared by exactly two
  triangles, and rays aimed at the shared cage edge never pass between them

#### Scenario: A uniform rate is uniform refinement

- **WHEN** every edge of a mesh gets the rate `2^L`
- **THEN** on faces with no extraordinary vertex the tessellated vertices are the limit
  points of uniform level-`L` refinement to within floating-point rounding, and near an
  extraordinary vertex to within the approximation of the patches that cover it

#### Scenario: A face-varying chart is tessellated, a Loop cage falls back

- **WHEN** a subdivision mesh with a face-varying `primvars:st` and a `loop` mesh are
  read in adaptive mode
- **THEN** the first is tessellated per face, each triangle corner taking the chart's
  value on its own side of any seam, and the second is refined to its per-mesh level
  and counted as one fallback

#### Scenario: Out of view

- **WHEN** the same subdivision cube is placed in front of the camera and behind it
- **THEN** the one in front is split at its rate and the one behind once per edge, and
  both are still hit by rays

#### Scenario: The switch restores per-mesh levels

- **WHEN** an adaptive render is imported with `CRUST_ADAPTIVE_PER_FACE=0`
- **THEN** every mesh is refined to its per-mesh adaptive level, as before this change

#### Scenario: Ptex on a per-face mesh

- **WHEN** a Ptex-textured subdivision mesh is tessellated per face
- **THEN** each hit resolves to the cage face and the position within it that the limit
  surface parameterization gives, so the texture is continuous across the
  tessellation
