## ADDED Requirements

### Requirement: A streamed chunk resolves materials bound from outside it

A material bound to geometry in one streamed chunk SHALL resolve exactly as it would in a single-stage import, wherever on the stage the material prim lives. A binding whose target exists nowhere SHALL still fall back to the default material, with a `material.fallback_default` warning naming the target.

#### Scenario: Materials in a sibling top-level scope

- **WHEN** a stage with four or more top-level subtrees keeps its materials in
  one of them, `/Looks`, and binds geometry in the others to `/Looks/Red`
- **THEN** that geometry renders with `/Looks/Red`, and no warning is raised for it

#### Scenario: A dangling binding

- **WHEN** a streamed chunk binds `/Looks/Missing`, which no layer defines
- **THEN** that geometry takes the default material, with one
  `material.fallback_default` warning naming `/Looks/Missing`

#### Scenario: Nothing bound from outside costs nothing

- **WHEN** every material a chunk binds lies inside that chunk
- **THEN** the import opens no stage for its materials beyond the chunk's own
