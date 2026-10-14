## MODIFIED Requirements

### Requirement: Edits are opinions in the override layer

`set_attribute`, `set_variant`, `set_active` and `bind_material` SHALL author their
opinion in the override layer, as `over` specs where the prim is defined elsewhere.
An attribute value SHALL take the attribute's declared type: the type a schema of
the prim declares, else the type the strongest layer authors. `set_attribute` SHALL
need an explicit `type` only for an attribute neither declares, and SHALL refuse one
that differs from the declared type. A value that does not convert to that type
SHALL be refused without authoring anything.

#### Scenario: Dimming a light

- **WHEN** the agent calls `set_attribute` on `/lights/key` `inputs:exposure` with
  `-0.5`
- **THEN** the override layer holds `over "lights" { over "key" { float
  inputs:exposure = -0.5 } }` (in its own syntax), and the source layers are
  unchanged

#### Scenario: A schema attribute nothing authors yet

- **WHEN** no layer authors `/scene/Sky.inputs:exposure` on a DomeLight and the agent
  calls `set_attribute` on it with `-1` and no `type`
- **THEN** the override layer holds `float inputs:exposure = -1` on `/scene/Sky`

#### Scenario: A value of the wrong type

- **WHEN** the agent sets `inputs:exposure` to `"bright"`
- **THEN** the call fails, naming the declared type, and the layer is unchanged

#### Scenario: A type that contradicts the schema

- **WHEN** the agent sets a DomeLight's `inputs:exposure` with `type` `"double"`
- **THEN** the call fails, naming `float` as the declared type, and the layer is
  unchanged

#### Scenario: Choosing a variant

- **WHEN** the agent calls `set_variant` with `/asset`, `lod` and `high`
- **THEN** the override layer authors that variant selection, and the next render
  composes the `high` variant

## ADDED Requirements

### Requirement: Querying an attribute nothing authors

`query` on an attribute that a schema of its prim declares and that no layer authors
SHALL answer the declared type, the schema's fallback value, and `source`
`"fallback"`. `query` on an attribute that no schema declares and no layer authors
SHALL fail, saying so and naming `set_attribute` with an explicit `type` as the way to
author it.

#### Scenario: A light's unauthored exposure

- **WHEN** no layer authors `/scene/Sky.inputs:exposure` on a DomeLight and the agent
  queries it
- **THEN** the answer has `type` `"float"`, `value` `0` and `source` `"fallback"`

#### Scenario: A name no schema declares

- **WHEN** the agent queries `/scene/Sky.inputs:nonsense`
- **THEN** the call fails, naming `set_attribute` with an explicit `type`
