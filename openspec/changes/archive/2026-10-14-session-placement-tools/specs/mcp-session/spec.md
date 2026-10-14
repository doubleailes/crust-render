## MODIFIED Requirements

### Requirement: An MCP server over stdio

`crust mcp` SHALL run a Model Context Protocol server on stdin/stdout until stdin
closes. stdout SHALL carry only protocol messages. The log SHALL go to stderr, and no
progress bar SHALL be drawn.

#### Scenario: Listing tools

- **WHEN** an MCP client initialises the server and asks for its tools
- **THEN** it receives `open_session`, `query`, `check`, `set_attribute`,
  `set_variant`, `set_active`, `bind_material`, `author_usda`, `drop_to_surface`,
  `scatter_on_surface`, `undo`, `reload`, `render`, `snapshot`, `cancel`, `probe`,
  `light_paths`, `diff` and `render_final`, each with a JSON input schema

#### Scenario: Nothing but protocol on stdout

- **WHEN** a session imports a stage that raises warnings
- **THEN** every stdout line parses as a protocol message, and the warnings appear
  on stderr

## ADDED Requirements

### Requirement: Placement is computed against the scene the session renders

`drop_to_surface` and `scatter_on_surface` SHALL compute placement against the
geometry the session's current import renders: subdivided, displaced and instanced as
it renders, at the session's frame and at shutter open, counting only geometry visible
to the camera. When a source layer changed on disk, the session SHALL import it before
computing. "Up" SHALL be the stage's `upAxis` (`Y` when none is authored), and "down"
its opposite. Each call that places anything SHALL be one edit batch: saved, brought up
to date as every edit is, one undo step, its result reporting `applied`, `import_s`
and the warning codes gained or lost. A call that places nothing SHALL author nothing
and add no undo step.

#### Scenario: Placing after a source changed

- **WHEN** another program moves a table in a layer the input composes, and the agent
  drops a vase onto it
- **THEN** the vase rests on the table where the rewritten layer puts it

#### Scenario: A Z-up stage

- **WHEN** the stage authors `upAxis = "Z"` and the agent drops a prim
- **THEN** the prim moves along Z only

#### Scenario: Undoing a placement

- **WHEN** the agent drops a prim, then calls `undo`
- **THEN** the override layer's content is equal to its content before the drop, and
  the next render equals the render before the drop, bit for bit

### Requirement: Dropping prims onto the surface below

`drop_to_surface` SHALL take one or more prim paths and, for each, move the prim along
the down axis until its geometry rests on the highest surface beneath it: after the
move no point of the prim's geometry lies below that surface, and at least one rests on
it. The surface SHALL be every other geometry of the scene, or only the geometry of the
`target` prims (and their descendants) when `target` is given. A prim's own geometry
SHALL never count as a surface. A prim already sunk into the surface SHALL be raised.
Each prim SHALL be placed against the scene as it was before the call, so prims in one
call do not land on each other's new positions.

The move SHALL be authored in the override layer as one transform op on the prim,
first in its `xformOpOrder` (after a `!resetXformStack!`), keeping every op the stage
already composes in their order. Dropping a prim again SHALL fold the new move into
that same op. The prim's other opinions and its descendants SHALL be unchanged.

A prim that has no geometry in the scene, or that nothing lies beneath, SHALL be
reported with the reason and left unchanged. A path that is not a prim of the stage,
lies inside an instance, or names a prim that cannot hold a transform SHALL be refused
with the reason, and nothing SHALL be authored for any prim of the call.

#### Scenario: A vase onto a table

- **WHEN** the agent calls `drop_to_surface` with a vase prim hanging above a table
- **THEN** the vase's lowest vertex lies on the table top within 1e-4 stage units, no
  vertex lies below it, and its position along the other two axes is unchanged

#### Scenario: A rock on uneven ground

- **WHEN** the agent drops a rock over a terrain mesh that is not flat beneath it
- **THEN** no vertex of the rock lies below the terrain, and at least one lies on it
  within 1e-4 stage units

#### Scenario: Raising a sunken prim

- **WHEN** a box's lower half is below a floor and the agent drops it
- **THEN** the box moves up until its lowest vertex lies on the floor

#### Scenario: Only onto the target

- **WHEN** a cup hangs above a shelf that is above a table, and the agent drops it with
  `target` the table
- **THEN** the cup rests on the table, below the shelf

#### Scenario: The authored transform keeps the prim's own

- **WHEN** a prim composes `xformOpOrder = ["xformOp:translate", "xformOp:rotateY"]`
  from a payload, and the agent drops it twice
- **THEN** the override layer authors one placement op, first in an `xformOpOrder`
  that then lists `xformOp:translate` and `xformOp:rotateY`, and the prim's rotation
  is unchanged

#### Scenario: Nothing below

- **WHEN** the agent drops two prims and nothing lies beneath the second
- **THEN** the first is placed, the result reports the second with `nothing below`,
  and the second's opinions are unchanged

#### Scenario: A prim inside an instance

- **WHEN** one of the paths lies inside an instance
- **THEN** the call fails naming that path, and the layer is unchanged

### Requirement: Aligning a dropped prim to the surface

With `align_to_normal`, `drop_to_surface` SHALL also turn each prim so that its up axis
follows the geometric normal of the surface beneath the centre of the prim's footprint,
pivoting about the bottom of that footprint, and then drop it as without alignment. A
prim with nothing beneath the centre of its footprint SHALL be reported and left
unchanged.

#### Scenario: A box on a slope

- **WHEN** the agent drops a box with `align_to_normal` onto a plane tilted 30° about
  the X axis
- **THEN** the box's up axis is the plane's normal within 1e-4, and its lowest vertex
  lies on the plane within 1e-4 stage units

### Requirement: Scattering instances over a surface

`scatter_on_surface` SHALL take the `target` prims, the `prototypes` (prim paths of the
stage or asset paths), a new prim path for the instancer, a `seed`, and either a
`count` or a `density` (instances per square stage unit of eligible surface). It SHALL
define one `PointInstancer` at that path in the override layer whose instances lie on
the target's triangles, distributed in proportion to area, each with its prototype's
origin on the surface. Each prototype SHALL be a child of the instancer referencing the
given prim or asset, placed at its own origin: its own transform in the stage is not
carried. The instancer SHALL author `positions`, `orientations`, `scales` and
`protoIndices` (prototypes chosen with equal probability) in its parent's space, and
render as `crust render` reads any `PointInstancer`.

Optional inputs SHALL be:
- `max_slope`: only triangles whose geometric normal is within this many degrees of up
  are eligible;
- `random_rotation`: each instance turned about the up axis by an angle drawn from the
  seed (otherwise none);
- `scale`: a `[min, max]` range each instance's uniform scale is drawn from (otherwise
  1).

The call SHALL be refused, authoring nothing, when:
- the instancer path names a prim the stage already composes, or its parent does not
  exist;
- the target has no eligible triangle;
- both or neither of `count` and `density` are given, or more than 100 000 instances
  would be authored;
- a prototype is neither a prim of the stage nor a file that exists;
- `scale` is not `0 < min <= max`, or `max_slope` is not in `(0, 180]`.

Geometry of the target that is not a triangle mesh (curves, implicit shapes) SHALL be
skipped and named in the result.

#### Scenario: Stones over terrain

- **WHEN** the agent scatters 500 instances of two stone assets over a terrain mesh
- **THEN** the override layer defines one `PointInstancer` with 500 positions, each on
  a terrain triangle within 1e-4 stage units, and `protoIndices` naming both stones

#### Scenario: A density

- **WHEN** the agent scatters with `density` 2 over a target whose eligible area is 50
  square units
- **THEN** the instancer holds 100 instances

#### Scenario: Steep faces left bare

- **WHEN** the agent scatters over a mesh with flat and vertical faces, with
  `max_slope` 30
- **THEN** no instance lies on a vertical face

#### Scenario: A prototype from the stage

- **WHEN** a prototype is `/World/rock`, defined at `(10, 0, 0)` in the stage
- **THEN** each instance places the rock's geometry at the instance's position, not 10
  units away, and `/World/rock` itself still renders where it was

#### Scenario: Rendering the scatter

- **WHEN** the agent scatters, then renders at 16 spp to completion
- **THEN** the render is bit-identical to a fresh session's 16 spp render of the same
  saved override layer

#### Scenario: An existing path

- **WHEN** the instancer path names a prim the stage already composes
- **THEN** the call fails naming it, and the layer is unchanged

### Requirement: A scatter is reproducible

Two `scatter_on_surface` calls with the same inputs over the same scene SHALL author
byte-identical instancers, whatever the render's sampler setting and the number of
threads. A different `seed` SHALL give a different distribution.

#### Scenario: Scattering again with the same seed

- **WHEN** the agent scatters with seed 7, calls `undo`, and scatters again with the
  same inputs
- **THEN** the override layer's bytes are identical after the two scatters

#### Scenario: Another seed

- **WHEN** the agent scatters with seed 7, undoes it, and scatters with seed 8
- **THEN** the two instancers' `positions` differ
