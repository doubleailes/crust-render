## ADDED Requirements

### Requirement: A sampler mismatch is a comparability warning

When both files carry a sampling stamp and their samplers differ, `crust diff` SHALL report
comparability `warn` with a note naming `sampler`. Different samplers draw different numbers,
so their pixels differ by noise even when both images converge to the same result. A stamp
without `crust:sampler` was written before the sampler was selectable and SHALL be read as
`sobol`.

#### Scenario: Two samplers

- **WHEN** `a` has `crust:sampler = sobol` and `b` has `crust:sampler = pmjBn`
- **THEN** the status is `warn`, with a note naming `sampler`

#### Scenario: An EXR from before the setting

- **WHEN** `a` has a `crust:*` stamp but no `crust:sampler`, and `b` has
  `crust:sampler = sobol` with every other stamp equal
- **THEN** the status is `ok`

#### Scenario: The exit status is unaffected

- **WHEN** the samplers differ and the pixels are identical
- **THEN** the exit status is still 0
