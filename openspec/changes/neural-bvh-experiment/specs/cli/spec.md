## ADDED Requirements

### Requirement: The stats report shows occlusion proxies

*Phase 2: implemented only if the Phase 1 measurement gate passes.* With `--stats`,
when at least one occlusion proxy was built, the report SHALL print the number of
proxies, their bytes as a kernel-memory row, and the total time spent training them.
With no proxy built, the report SHALL print no proxy rows.

#### Scenario: A proxied render's report

- **WHEN** a scene with an eligible prototype is rendered with
  `CRUST_NEURAL_OCCLUSION=on --stats`
- **THEN** the report prints `occlusion proxies` with their count, a kernel-memory row
  for their bytes that is part of the `kernel memory` sum, and the training seconds

#### Scenario: An exact render's report

- **WHEN** the same scene is rendered with `--stats` and the switch off
- **THEN** the report has no proxy rows and is otherwise unchanged
