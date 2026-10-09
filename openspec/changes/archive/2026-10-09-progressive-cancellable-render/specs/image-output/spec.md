## ADDED Requirements

### Requirement: EXRs record an interrupted render

Every EXR written from a cancelled render (the single beauty EXR and each product)
SHALL carry a string header attribute `crust:renderStatus = "interrupted"`. EXRs from
a completed render SHALL NOT carry it, and SHALL stay byte-identical to those
written before this change.

#### Scenario: An interrupted render's EXR

- **WHEN** Ctrl-C cuts a render short and its EXR is written
- **THEN** the EXR's header has `crust:renderStatus` set to `interrupted`

#### Scenario: A Ctrl-C after the last sample

- **WHEN** Ctrl-C arrives after the render traced its last sample, before its
  outputs are being written
- **THEN** its EXRs are complete and carry no `crust:renderStatus`

#### Scenario: A completed render's EXR

- **WHEN** a render completes
- **THEN** its EXR has no `crust:renderStatus` attribute and is byte-identical to
  the output before this change
