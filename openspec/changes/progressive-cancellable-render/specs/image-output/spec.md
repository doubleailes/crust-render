## ADDED Requirements

### Requirement: EXRs record an interrupted render

Every EXR written from a cancelled render (the single beauty EXR and each product)
SHALL carry a string header attribute `crust:renderStatus = "interrupted"`. EXRs from
a completed render SHALL NOT carry it, and SHALL stay byte-identical to those
written before this change.

#### Scenario: An interrupted render's EXR

- **WHEN** a render is interrupted with Ctrl-C and its EXR is written
- **THEN** the EXR's header has `crust:renderStatus` set to `interrupted`

#### Scenario: A completed render's EXR

- **WHEN** a render completes
- **THEN** its EXR has no `crust:renderStatus` attribute and is byte-identical to
  the output before this change
