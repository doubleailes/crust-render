## ADDED Requirements

### Requirement: The stats report shows BVH node bytes per format

With `--stats`, the report SHALL print the kernel's node bytes for each node format that
any tree in the scene uses, as rows inside the `kernel memory` sum, so that the effect of
`CRUST_BVH_NODES` and of the automatic choice is visible. A format that no tree uses
SHALL print no row.

#### Scenario: A render with both formats

- **WHEN** a scene with one large and one small prototype is rendered with `--stats` and
  the default `CRUST_BVH_NODES`
- **THEN** the report prints a `bvh nodes (quantised)` row and a `bvh nodes (4-wide)`
  row, both inside the `kernel memory` sum

#### Scenario: A render forced to the 4-wide node

- **WHEN** the same scene is rendered with `CRUST_BVH_NODES=wide4 --stats`
- **THEN** the report prints only the 4-wide row, and its other rows are unchanged
  from before this change
