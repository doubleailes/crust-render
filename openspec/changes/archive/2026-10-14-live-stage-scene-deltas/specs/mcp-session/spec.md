# Spec Delta

## MODIFIED Requirements

### Requirement: Each edit batch is saved and imported

After each edit tool call, the session SHALL save the override layer, then bring the
scene up to date with the saved layer, either in place or by importing it as
`crust render <output>` would. A call's result SHALL report the warning codes the
scene gained or lost, the time taken (`import_s`), and how the edit was applied
(`applied`: `settings`, `lights`, `materials` or `full`).

#### Scenario: An edit that introduces a warning

- **WHEN** the agent authors a `RectLight` with `width = 0`
- **THEN** the result lists `light.degenerate_shape` as gained, the result reports
  `applied = full`, and the file on disk already holds the light

#### Scenario: Dimming a light is applied in place

- **WHEN** the agent sets `inputs:intensity` on an existing `RectLight`
- **THEN** the result reports `applied = lights`

### Requirement: Undo

`undo` SHALL restore the override layer to its content before the most recent edit
batch, save it, and bring the scene up to date with it as an edit would, in place
when the restored change allows it. Undo history SHALL last for the session. After a
resume, undo SHALL NOT reach edits made in earlier sessions. An `undo` whose import
fails SHALL NOT be applied: the layer and the scene stay as they were, and the step
stays in the history.

#### Scenario: Undoing an exposure change

- **WHEN** the agent sets an exposure, then calls `undo`
- **THEN** the override layer's content is equal to its content before the set, the
  undo reports `applied = settings`, and the next render equals the render before the
  set, bit for bit

## ADDED Requirements

### Requirement: Edits apply in place when they are confined

The session SHALL compare the override layer before and after each edit batch or undo,
and apply the change without a re-import when every changed field belongs to one of
these classes:

- `settings`: attribute values on the `RenderSettings` prim the render uses, or on
  the render camera;
- `lights`: `inputs:*` values on existing UsdLux light prims;
- `materials`: `inputs:*` values on existing shaders, or interface inputs, of
  materials the scene already uses, when the change does not move the material's
  opacity, transmission or emission class.

Any other change SHALL be applied as `full`, a re-import of the saved layer. This
includes a spec added or removed, a relationship, connection, transform, visibility,
primvar, variant selection, activation or composition arc. A batch with changes in
several classes SHALL be applied as the most expensive of them, in the order
`settings` < `materials` < `lights` < `full`. A render after an edit applied in place
SHALL be bit-identical to a render after a `full` application of the same layer
content. With `CRUST_SCENE_DELTAS=0`, every edit SHALL be applied as `full`, with no
assets kept from earlier imports.

#### Scenario: A shader input

- **WHEN** the agent sets `inputs:base_color` on an existing `standard_surface` shader
- **THEN** the result reports `applied = materials`, and a 16 spp render is
  bit-identical to the same render after `crust render <output>` imports the layer

#### Scenario: A transform is not confined

- **WHEN** the agent sets `xformOp:translate` on a mesh
- **THEN** the result reports `applied = full`

#### Scenario: Making a material emissive

- **WHEN** the agent sets `inputs:emission` above 0 on a shader whose material
  emitted nothing
- **THEN** the result reports `applied = full`

#### Scenario: The switch turns it off

- **WHEN** the session runs with `CRUST_SCENE_DELTAS=0` and the agent sets
  `inputs:intensity` on a light
- **THEN** the result reports `applied = full`

### Requirement: Assets persist across edits

The session SHALL keep the decoded assets (images, texture tiles, Ptex) of earlier
imports for its lifetime. Before each import, it SHALL discard any kept asset whose
file's length or modification time changed on disk.

#### Scenario: A re-exported texture

- **WHEN** a texture the stage uses is overwritten on disk with different content
  during a session, and the agent then makes any edit
- **THEN** the next render uses the new texture

#### Scenario: Unchanged textures are not decoded again

- **WHEN** the agent makes a `full` edit on a textured stage whose texture files did
  not change
- **THEN** the import decodes no texture file a second time
