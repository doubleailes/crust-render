# Spec Delta

## Purpose

Gives every warning raised while a USD stage is imported a stable, documented code
and kind, and collects them into records a host can report, so tools and agents can
act on what crust refused, approximated or skipped without reading log prose.

## ADDED Requirements

### Requirement: Every import warning has a stable code

Every warning raised while a stage is imported SHALL carry a code of the form
`<domain>.<cause>` in lowercase snake_case (e.g. `light.degenerate_shape`). A code
SHALL name a cause, not a call site: two sites reporting the same cause SHALL share
the code. Every code SHALL be listed, with its kind and what to do about it, in the
user documentation's warnings reference.

#### Scenario: Two sites, one cause

- **WHEN** one stage authors a `Material` with no surface shader and another
  `Material` whose shader has an unrecognised `info:id`
- **THEN** both warnings carry the code `material.fallback_default`

#### Scenario: Every code is documented

- **WHEN** the warnings reference in the user documentation is compared with the
  codes crust can raise
- **THEN** every code appears in the reference with its kind, and the reference
  lists no code crust cannot raise

### Requirement: Every code has one fixed kind

Each code SHALL have exactly one kind:

- `refused`: an invalid authored value (non-finite, out of range, unknown token,
  wrong type) was replaced by a fallback;
- `approximated`: a valid authored value is rendered differently from what it asks
  for;
- `skipped`: something authored (a prim, an asset, an output channel) contributes
  nothing; a constant or default may stand in for it.

#### Scenario: A non-finite light input

- **WHEN** a light authors `inputs:intensity = inf`
- **THEN** the warning's kind is `refused`

#### Scenario: A light that cannot be built

- **WHEN** a `RectLight` authors `width = 0`
- **THEN** the warning's kind is `skipped`

### Requirement: Codes are versioned with the reports that carry them

Adding a code SHALL NOT change the version of any report. Renaming or removing a
code, or changing its kind, SHALL bump the version of every report format that
carries warnings.

#### Scenario: A new code

- **WHEN** a release adds a code for a newly detected cause
- **THEN** every report that carries warnings keeps its version, and existing
  consumers still parse it

### Requirement: One record per code, every occurrence counted

An import SHALL produce one warning record per code raised. A record SHALL hold:

- the code and its kind;
- `count`, the number of occurrences;
- `prims`, the distinct prim paths the code fired on, in first-occurrence order and
  capped at 16 entries;
- `message`, the first occurrence's log text without the code prefix.

A warning that names no prim SHALL count without adding a path.

#### Scenario: A warn-once cause on many prims

- **WHEN** 214 meshes are displaced at their cage resolution
- **THEN** the log prints that warning once, and the `mesh.displaced_at_cage`
  record has `count` 214 and the first 16 mesh paths in `prims`

#### Scenario: A stage-level warning

- **WHEN** the requested frame lies outside the stage's time range
- **THEN** the `time.outside_range` record has `count` 1 and an empty `prims`

### Requirement: Records are deterministic

Records SHALL appear in the order their codes first fired. Importing the same stage
with the same flags twice SHALL produce identical records.

#### Scenario: Re-importing

- **WHEN** the same stage is imported twice with the same flags
- **THEN** the two lists of records are equal, element by element

### Requirement: Log lines carry their code

A coded warning SHALL be logged at WARN with its code in brackets before the
message, e.g. `[light.degenerate_shape] RectLight at /lights/key: …`, whether or not
it is raised during an import. Warnings that have no code (environment, diagnostic
analysis) SHALL keep their current text.

#### Scenario: Searching for a code

- **WHEN** a render logs a skipped `PointInstancer`
- **THEN** its WARN line begins `[instancing.no_prototypes]`, and that code is
  listed in the warnings reference

### Requirement: Only import-time warnings are collected

Warnings raised during the stage import SHALL be collected, including those raised
by the asset loaders it calls. Warnings raised while reading the environment, during
the render, or by `crust diagnostic`'s own analysis SHALL only be logged. This is a
known gap, not a guarantee that those phases raise nothing.

#### Scenario: A missing texture referenced by many materials

- **WHEN** three materials reference the same texture file, which does not exist
- **THEN** one record for the unreadable texture has `count` 3 and the three
  material paths in `prims`, and the log names the file once

#### Scenario: A missing UDIM tile

- **WHEN** a UDIM texture set is missing one tile on disk
- **THEN** the import's records include the code for the skipped tile

#### Scenario: A bad environment switch

- **WHEN** `CRUST_TEX_STREAM=maybe` is set
- **THEN** its warning is logged and appears in no record
