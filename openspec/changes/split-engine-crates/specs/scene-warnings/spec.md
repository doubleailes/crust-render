# Spec Delta

## MODIFIED Requirements

### Requirement: Records are deterministic

Records SHALL appear in the order their codes first fired, where "first" is defined
by the import's traversal order, not by which thread raised the warning or when.
Importing the same stage with the same flags twice SHALL produce identical records.
A warning raised on any thread the import uses, inside that thread's warning scope,
SHALL be collected.

#### Scenario: Re-importing

- **WHEN** the same stage is imported twice with the same flags
- **THEN** the two lists of records are equal, element by element

#### Scenario: A warning raised off the importing thread

- **WHEN** work done for the import on another thread (inside a warning scope
  opened there) raises a coded warning
- **THEN** the warning appears in the scene's records, in the position its prim's
  traversal order gives it
