## ADDED Requirements

### Requirement: A path-scheduling switch

`CRUST_WAVEFRONT` (`0` | `1`, default `0`) SHALL select the batched sweep (`1`) or the
one-path-at-a-time sweep (`0`). It SHALL be parsed once into the configuration, warn
once on a bad value, and be listed in `docs/architecture.md` and the user
environment-variable reference with the one-path-at-a-time sweep as its off side.

#### Scenario: Unset

- **WHEN** `CRUST_WAVEFRONT` is not set
- **THEN** the render traces one path at a time, as before the switch existed

#### Scenario: A bad value

- **WHEN** `CRUST_WAVEFRONT=fast` is set
- **THEN** one warning names the variable and the render proceeds one path at a time

#### Scenario: Either value, same image

- **WHEN** a scene is rendered with `CRUST_WAVEFRONT=0` and with `CRUST_WAVEFRONT=1`
- **THEN** `crust diff` reports the two EXRs identical (exit 0)
