# Spec Delta

## ADDED Requirements

### Requirement: geompropvalue reads a curve hit's properties

A MaterialX `geompropvalue` whose `geomprop` is `curve:v`, `curve:index` or
`curve:random` SHALL read, on a curve hit, the position along the strand, the
curve's index within its prim, and a per-curve value in [0, 1). On any other
hit it SHALL return the node's `default`, as MaterialX specifies for an absent
property. `geompropvalue` `st` keeps reading the default chart.

#### Scenario: A root-to-tip ramp

- **WHEN** a hair material's `base_color` is a `mix` of a root and a tip
  colour driven by `geompropvalue` `curve:v`
- **THEN** each strand is the root colour at its first point and the tip
  colour at its last

#### Scenario: Curve properties on a mesh read the default

- **WHEN** the same material is bound to a mesh, with the `geompropvalue`'s
  `default` 0.5
- **THEN** the mesh renders the even mix of root and tip, and no warning is
  raised

### Requirement: curve:random is a stable per-curve hash

`curve:random` SHALL be a deterministic hash of the prim's path and the curve
index, uniform over [0, 1): the same strand reads the same value on every
frame, sample and render while its prim keeps the same curve order, and
neighbouring curves and different prims read uncorrelated values. Reordering a
prim's curves SHALL reassign the values.

#### Scenario: Stable across frames

- **WHEN** an animated groom with constant topology is rendered at frames 1
  and 2 with a material driven by `curve:random`
- **THEN** each strand reads the same value on both frames

#### Scenario: Two prims do not repeat each other

- **WHEN** two `BasisCurves` prims with identical curves and the same material
  are rendered side by side
- **THEN** corresponding strands of the two prims read different values

#### Scenario: Values cover the unit interval

- **WHEN** `curve:random` is read for 10 000 curves of one prim
- **THEN** every value is in [0, 1), and their histogram over ten equal bins
  is within 5% of uniform in each bin

### Requirement: geompropvalue reads curve primvars by name

A `geompropvalue` whose `geomprop` names a curve primvar (without its
`primvars:` prefix) SHALL read that primvar's value at the hit, converted to
the node's type as MaterialX converts values. On a hit whose geometry does not
author the primvar, or authors it with a type that cannot be read, it SHALL
return the node's `default`.

#### Scenario: A per-strand colour

- **WHEN** a groom authors a `uniform` `color3f` `primvars:hairColor` and its
  material reads `geompropvalue` `hairColor` into `base_color`
- **THEN** every strand renders with its own authored colour

#### Scenario: A float primvar into a colour input

- **WHEN** a `geompropvalue` of type `color3` reads a `float` primvar
- **THEN** it reads the float in all three lanes

#### Scenario: A stable id seeds the variation

- **WHEN** a groom authors a `uniform` `int` `primvars:strandId`, and a
  `randomfloat`'s `seed` is a `geompropvalue` `strandId`
- **THEN** each strand's random value is unchanged when the prim's curves are
  reordered

### Requirement: MaterialX HSV, conditional and random nodes

`rgbtohsv`, `hsvtorgb`, `ifgreater`, `ifgreatereq`, `ifequal`, `randomfloat`
and `randomcolor` SHALL compile and evaluate as MaterialX's standard library
defines them, under the pattern-node reference requirement. Where the reference
hash of the random nodes cannot be matched bit for bit, they SHALL still be
deterministic, uniform over their output range, and the difference SHALL be
recorded as a known gap.

#### Scenario: The HSV round trip

- **WHEN** `hsvtorgb` is applied to the output of `rgbtohsv` on
  (0.2, 0.5, 0.8)
- **THEN** it evaluates to (0.2, 0.5, 0.8) to within 1e-5 relative

#### Scenario: A conditional picks a branch

- **WHEN** an `ifgreater` of type `color3` has `value1` = 0.7,
  `value2` = 0.5, `in1` red and `in2` blue
- **THEN** it evaluates to red, and to blue when `value1` = 0.3

#### Scenario: A random node depends only on its seed

- **WHEN** two `randomfloat` nodes with `min` = 0, `max` = 1 read the same
  `seed`
- **THEN** they evaluate to the same value, in [0, 1), at every hit

#### Scenario: Interpreter and JIT agree

- **WHEN** a program using every new node and every curve property is
  evaluated on a curve fixture by the interpreter and by the JIT
- **THEN** the two results are bit-identical

### Requirement: Curve attributes resolve as per-query shading does

On a curve hit, a material reading curve properties or curve primvars SHALL
resolve to exactly what per-query shading of the same hit gives. An MCP edit
that makes a curve material read a property or primvar it did not read before
SHALL render as a full re-import of the edited layer does.

#### Scenario: Resolve matches per-query shading on a curve

- **WHEN** the resolve pin runs a curve-reading material on a curve hit
- **THEN** the resolved shading point is bit-identical to per-query shading

#### Scenario: A live edit starts reading a primvar

- **WHEN** an MCP session edits a curve material to read
  `geompropvalue` `hairColor` it did not read before
- **THEN** the next render is bit-identical to rendering the edited layer from
  a fresh import
