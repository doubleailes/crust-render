## ADDED Requirements

### Requirement: Different exposure scales are not comparable as is

When both files' stamps record a `crust:exposureScale` and the two differ, `crust diff`
SHALL report the comparability status `warn`, with a note naming `exposureScale` and
saying that every radiance channel differs by the ratio of the two scales. A stamp
without `crust:exposureScale` (a file written before it was recorded) SHALL be read as
a scale of 1.

#### Scenario: One stop apart

- **WHEN** `a` records `crust:exposureScale = 1` and `b` records `2`
- **THEN** the status is `warn`, with a note naming `exposureScale`

#### Scenario: An older file

- **WHEN** `a` has a stamp without `crust:exposureScale` and `b` records `1`
- **THEN** no note names `exposureScale`
