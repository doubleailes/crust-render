# Spec Delta

## ADDED Requirements

### Requirement: Unlit emitters name their remedy

The `unlit_emitters` finding SHALL carry, as numeric evidence beside its luminance, the
number of emissive prims outside the light list. Its action SHALL be `none`, with a
reason that names `UsdLuxMeshLightAPI` as the authoring change that makes such a mesh a
light and names up to five of those prims, each refused mesh light with the warning
code that refused it. The report format SHALL NOT change.

#### Scenario: An emissive card without the API

- **WHEN** a scene is lit by an emissive mesh `/set/card` that does not apply
  `MeshLightAPI`
- **THEN** the `unlit_emitters` finding's evidence counts one prim, and its reason
  names `/set/card` and `MeshLightAPI`

#### Scenario: The card after the API is applied

- **WHEN** the same scene is diagnosed with `MeshLightAPI` applied to `/set/card`
- **THEN** no `unlit_emitters` finding is reported

#### Scenario: A refused mesh light

- **WHEN** a mesh with `MeshLightAPI` and a textured glow lights the scene
- **THEN** the `unlit_emitters` reason names it with the warning code that refused it
