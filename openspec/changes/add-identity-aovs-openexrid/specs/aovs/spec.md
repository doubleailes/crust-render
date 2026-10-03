## MODIFIED Requirements

### Requirement: Identity and ID mattes

`primId` SHALL be a stable integer per prim path: the same prim gets the same
id in every frame and every run. `instanceId` SHALL be the instance's index
within its instancer, and `elementId` the authored (pre-triangulation) face
index; both SHALL be `-1` where there is none. Identity sources SHALL be
closest-sample AOVs, written as UINT.

ID mattes SHALL be written as
[OpenEXRId](https://github.com/MercenariesEngineering/openexrid) deep EXRs,
in the layout its specification defines: per pixel, each object's id and its
coverage, weighted by the beauty's pixel filter, with the names the ids
stand for in the file. Cryptomatte layers SHALL NOT be written; their names
(`crypto_object` …) SHALL be refused with a warning.

#### Scenario: Stable IDs across frames

- **WHEN** two frames of an animated scene are rendered with `primId`
- **THEN** a given prim has the same ID in both frames

#### Scenario: Coverage sums to one

- **WHEN** a product writes an OpenEXRId matte for a scene with no background
  visible
- **THEN** in every pixel the coverages of its samples sum to 1

## ADDED Requirements

### Requirement: Primvar sources

A RenderVar with `sourceType = "primvar"` SHALL hold the named primvar's value
at the camera ray's first hit, interpolated as the primvar is authored
(constant, uniform, vertex, varying or face-varying), filtered with the
beauty's pixel-filter weights. Its type SHALL come from `dataType`. Where the
hit surface does not author the primvar, the channel SHALL hold the var's
clear value. A primvar whose type cannot be written as the var's `dataType`
SHALL be refused with a warning.

#### Scenario: Display colour

- **WHEN** a RenderVar authors `sourceName = "displayColor"`,
  `sourceType = "primvar"` and `dataType = "color3f"` on a scene whose meshes
  author `primvars:displayColor`
- **THEN** the channel shows each mesh's display colour, and the clear value
  where the camera sees a surface without it
