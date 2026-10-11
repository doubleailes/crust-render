## ADDED Requirements

### Requirement: A MaterialX document composes into the stage

A `.mtlx` file reached by any composition arc (a reference, a payload, a
sublayer) or opened as the root layer SHALL compose into the stage as
`UsdShade` prims, laid out as C++ `usdMtlx` lays them out: each
`surfacematerial` a `Material` at `/MaterialX/Materials/<name>`, each node a
`Shader` whose `info:id` names its MaterialX nodedef, and each input's
effective colour space authored as `colorSpace` metadata. A material composed
this way SHALL render through the inline-network requirement.

#### Scenario: A referenced material is not empty

- **WHEN** a `Material` prim's only opinion is
  `references = @materialx_basic.mtlx@</MaterialX/Materials/name>`
- **THEN** the composed prim has an `outputs:mtlx:surface` connected to a
  `Shader` whose `info:id` starts with `ND_`, and it renders as a MaterialX
  material, not the default grey

#### Scenario: A payload works like a reference

- **WHEN** the same material is brought in by a payload instead of a reference,
  and the payload is loaded
- **THEN** it renders the same as through the reference

### Requirement: A composed MaterialX document renders as the document

A material composed from a `.mtlx` SHALL render the same as the document read
directly did, bit-identical at 16 spp. Asset paths inside the document SHALL
resolve relative to the document, not to the layer that referenced it.

#### Scenario: Textures resolve beside the document

- **WHEN** a shot layer in another directory references an asset layer that
  references a `.mtlx` whose `image` node names `textures/albedo.png`
- **THEN** the texture is loaded from the `textures/` directory next to the
  `.mtlx`

#### Scenario: Unchanged images

- **WHEN** `scripts/check_images.sh check` runs on goldens recorded before
  this change
- **THEN** every `.mtlx`-referencing sample reports `identical`
