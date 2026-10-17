# Spec Delta

## Purpose

Bake opted-in curves into a 3D mass field (coverage density, signed distance
to its surface, and an optional colour) that materials read at any hit, so
strands crossing at a point agree on the mass colour and normal there.

## ADDED Requirements

### Requirement: Opted-in curves bake a mass field

A `BasisCurves` or `NurbsCurves` prim authoring `crust:mass:radius` greater
than 0 SHALL be baked at import into a mass field: the strands' coverage
density, splatted within that radius and weighted by each strand's width; the
signed distance to the surface where coverage equals `crust:mass:threshold`,
negative inside; and, when its material provides one, a mass colour. A curves
prim without a positive radius SHALL bake nothing.

#### Scenario: A clump bakes a field

- **WHEN** a prim of 200 strands authors `crust:mass:radius = 0.05`
- **THEN** the imported scene holds one mass field covering the strands, and
  `--stats` lists it with its size in memory and its bake time

#### Scenario: No radius, no field

- **WHEN** the same prim authors no `crust:mass:radius`, or authors 0
- **THEN** no field is baked, and the render is bit-identical to one of the
  scene before this change

### Requirement: Coverage and threshold are in strand units

Coverage SHALL be normalised so the centre line of a lone straight strand as
wide as the field's radius has coverage 1, and a strand twice as wide
coverage 2. The mass's surface SHALL lie where coverage equals
`crust:mass:threshold` (default 0.5).

#### Scenario: A lone strand's surface

- **WHEN** a lone straight strand as wide as the radius bakes with the default
  threshold
- **THEN** the signed distance is negative on the strand's centre line and
  positive at twice the radius from it, and its zero crossing lies between

### Requirement: The field is a function of position alone

Every value read from a mass field SHALL depend only on the point it is read
at, never on which strand was hit or from where, so two strands crossing at a
point read the same mass colour, normal, depth and density there.

#### Scenario: Crossing sheets agree

- **WHEN** two sheets of strands of different authored colours cross at an
  angle inside one field, and a material reads the mass normal and colour
- **THEN** at the crossing line both sheets return the same normal and the same
  colour to within float rounding of the same lookup

### Requirement: The mass normal points out of the mass

The mass normal at a point SHALL be the normalised gradient of the signed
distance there: outside the mass it points away from the mass's surface, and
inside it points toward the nearest point of that surface. Where that gradient
is zero or not finite (on a symmetry axis, or with no surface within the
field's distance band), the normal lookup SHALL return its default. Depth SHALL
be the negated signed distance where negative, and 0 elsewhere.

#### Scenario: A clump's normal is its surface's

- **WHEN** a round clump of parallel strands bakes a field and the mass normal
  is read at a strand on its silhouette and at one halfway to its axis
- **THEN** the silhouette strand reads a normal pointing radially outward, and
  the inner strand a unit normal pointing away from the axis, with depth
  greater than 0

#### Scenario: The exact axis has no normal

- **WHEN** the mass normal of that clump is read exactly on its centre line,
  where the distance's gradient vanishes by symmetry
- **THEN** the lookup returns its default

#### Scenario: A strand that never reaches the threshold

- **WHEN** a lone opted-in strand narrower than half the field's radius bakes
  with the default threshold, so its coverage stays below it everywhere
- **THEN** the field has no surface: the normal lookup returns its default,
  depth is 0, and density still reads the strand's coverage

### Requirement: The mass colour is a coverage-weighted mean

When the bound material provides a mass colour graph, the field's colour at a
point SHALL be the mean of that graph's value over the strand samples around
it, weighted as coverage is, so sparse coverage never darkens it. Without such
a graph, the field SHALL hold no colour and the colour lookup returns its
default.

#### Scenario: Two colours meet

- **WHEN** a field covers a red sheet of strands and a blue sheet that meet
- **THEN** the mass colour is red well inside the red sheet, blue well inside
  the blue one, and a blend of the two where their coverage overlaps, never
  darker than either

### Requirement: The colour graph is evaluated at strand samples

The mass colour graph SHALL be evaluated at points along each strand with the
point's position, its curve properties (`curve:v`, `curve:index`,
`curve:random`) and the curve primvars it names. A node in it that needs a view
or a surface hit (`viewdirection`, `normal`, `tangent`, a texture footprint)
SHALL be refused with one warning naming it, and read its default.

#### Scenario: A root-to-tip ramp bakes into the mass

- **WHEN** the colour graph mixes a root and a tip colour by `curve:v`
- **THEN** the mass colour near the roots is the root colour and near the tips
  the tip colour

#### Scenario: A view-dependent node is refused

- **WHEN** the colour graph reads `viewdirection`
- **THEN** the import raises one `mass.color_unsupported` warning naming
  the node and the material, and the bake uses the node's default

### Requirement: Fields are grouped by token under one placement

Curve prims authoring the same `crust:mass:group` token under the same
placement SHALL bake one field together; a prim without a group SHALL bake its
own, and prims under different placements SHALL never share one. A group's
field SHALL take its radius, threshold and cap from the group's first prim in
path order; prims authoring other values SHALL be named by one
`mass.group_conflict` warning.

#### Scenario: A groom split over prims bakes one mass

- **WHEN** a groom's strands are split across three prims sharing
  `crust:mass:group = "hair"`
- **THEN** one field covers all three, and a strand of one prim crossing a
  strand of another reads the same mass normal at the crossing

#### Scenario: Two characters do not merge

- **WHEN** two characters' grooms author the same group token under different
  transforms
- **THEN** each bakes its own field

#### Scenario: Grouped prims disagree

- **WHEN** `/Groom/A` and `/Groom/B` share a group, `A` authoring
  `crust:mass:radius = 0.1` and `B` 0.2
- **THEN** one field bakes with radius 0.1, and one `mass.group_conflict`
  warning names the group and `/Groom/B`

### Requirement: Field resolution is bounded

The field's voxel size SHALL be half its radius, unless that would put more
than `crust:mass:maxResolution` voxels (default 512) along an axis of the
field's bounds, in which case the voxel SHALL grow until it does not. Only the
regions the strands' coverage or the surface's distance band reach SHALL hold
memory.

#### Scenario: The cap binds

- **WHEN** a field's bounds are 1000 radii across with the default cap
- **THEN** it bakes with 512 voxels along that axis, and the import logs the
  enlarged voxel once at DEBUG

### Requirement: Bakes are deterministic

A mass field SHALL be bit-identical across runs and thread counts, and SHALL
add no Monte Carlo noise to the render.

#### Scenario: One thread or many

- **WHEN** the same scene is imported with one worker thread and with eight
- **THEN** every field is bit-identical between the two imports

### Requirement: Lookups resolve their placement or fall back

A field SHALL be read at top-level curves, and at curves directly instanced
with a static, unlabelled transform of rotation and uniform scale, through that
placement, its mass scaling with the instance as its strands do. A hit through
any other placement (a `PointInstancer`'s, a motion-blurred or a non-uniformly
scaled instance) SHALL read every lookup's default, with one
`mass.unplaceable` warning per prim placed so.

#### Scenario: A directly instanced groom reads its field

- **WHEN** a groom prototype with a field is instanced twice with different
  rotations
- **THEN** each instance reads the mass normal rotated with it

#### Scenario: A scaled instance scales its mass

- **WHEN** a groom prototype with a field is instanced at scale 1 and at
  uniform scale 2
- **THEN** at corresponding points the second instance reads the same mass
  normal and twice the depth

#### Scenario: A non-uniformly scaled instance falls back

- **WHEN** the same prototype is instanced with scale (1, 2, 1)
- **THEN** that instance's curves shade with every mass lookup's default, and
  one `mass.unplaceable` warning names the prim

#### Scenario: An instancer-placed groom falls back

- **WHEN** a `PointInstancer` places a groom prototype that authors
  `crust:mass:radius`
- **THEN** the curves shade with every mass lookup's default, and one
  `mass.unplaceable` warning names the prim

### Requirement: The field is baked at the frame's time

A field SHALL be baked from the strands at the frame's time code and SHALL NOT
move over the shutter; under motion blur the strands still blur while their
mass colour and normal stay where the frame put them. This is a known gap.

#### Scenario: A moving groom

- **WHEN** a groom with a field translates during the shutter
- **THEN** its strands blur, and the field is the one baked at the frame time

### Requirement: Edits rebuild the field

An MCP edit changing any `crust:mass:*` attribute, a prim's group membership,
the strands, or the material's mass colour graph SHALL rebuild the affected
fields, and the edited scene SHALL render exactly as a full re-import of the
same layers would.

#### Scenario: A radius edit

- **WHEN** an MCP session edits `crust:mass:radius` on a groom and renders
- **THEN** the image equals a fresh import of the edited layer rendered with
  the same settings
