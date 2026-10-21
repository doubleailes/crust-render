# Spec Delta

## RENAMED Requirements

- FROM: `### Requirement: geompropvalue reads curve primvars by name`
- TO: `### Requirement: geompropvalue reads primvars by name`

## MODIFIED Requirements

### Requirement: geompropvalue reads primvars by name

A `geompropvalue` whose `geomprop` names a primvar (without its `primvars:`
prefix) SHALL read that primvar's value at the hit, converted to the node's
type as MaterialX converts values, on a curve and on a mesh alike. On a curve
it reads the primvar as the curve interpolates it; on a mesh, a `constant`
value, a `uniform` value per face, and `vertex`, `varying` and `faceVarying`
values interpolated linearly across the hit triangle, `:indices` expanded and
a subdivided mesh's primvars refined with its surface. An implicit shape SHALL
read `constant` primvars. On a hit whose geometry does not author the primvar,
or authors it with a type, interpolation or length that cannot be read, it
SHALL return the node's `default`, with a warning for one that is authored.

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

#### Scenario: A per-point alpha on a mesh

- **WHEN** a mesh authors a `vertex` `float` `primvars:Alpha` and its material
  reads `geompropvalue` `Alpha` into its opacity
- **THEN** each hit's opacity is the alpha interpolated across the triangle it
  lies on

#### Scenario: A face-varying primvar is discontinuous

- **WHEN** a mesh authors a `faceVarying` primvar whose values differ on the
  two sides of an edge
- **THEN** hits on either side of the edge read their own face's values

#### Scenario: An implicit shape's constant primvar

- **WHEN** a `Sphere` or a `Cube` authors a `constant` `color3f`
  `primvars:tint` read by its material
- **THEN** every hit reads the tint; a `uniform` primvar on it reads the
  `default`, with `mesh.unsupported_primvar`
