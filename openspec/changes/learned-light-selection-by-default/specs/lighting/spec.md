## MODIFIED Requirements

### Requirement: Light selection

NEE SHALL sample one light per vertex, chosen by `crust:lightSelection` /
`--light-selection`: `learned` (default; a visibility-aware table trained by a
deterministic pre-pass and blended between neighbouring cells, over the power
table), `power` (infinite lights keep a uniform share, the finite lights split
the rest half evenly and half by flux), or `uniform` (bit-identical to the
historical renderer).

#### Scenario: Uniform selection

- **WHEN** a render runs with `--light-selection uniform`
- **THEN** each of N lights is picked with probability 1/N

#### Scenario: Learned is the default

- **WHEN** a stage authors no `crust:lightSelection` and the command line gives
  no `--light-selection`
- **THEN** lights are selected by the learned table

#### Scenario: Power reproduces the previous default

- **WHEN** a render runs with `--light-selection power`
- **THEN** the image is bit-identical to the renderer before this change at the
  same settings

## ADDED Requirements

### Requirement: Learned selection bounds its firefly risk

A light's learned selection probability SHALL vary continuously with the
receiver's position, with no jump at cell boundaries. A light that delivered
light to any training receiver near a point SHALL keep a selection probability
there that does not shrink with how rarely it was seen. A light seen by no nearby
receiver SHALL keep the uniform defensive share. Both MIS strategies SHALL read
the same probability at the vertex NEE sampled from.

#### Scenario: No jump at a cell edge

- **WHEN** a receiver point moves across the boundary between two learned cells
- **THEN** every light's selection probability changes continuously

#### Scenario: A rarely seen light is not starved

- **WHEN** a light was seen by a single training receiver in a cell's
  neighbourhood
- **THEN** its selection probability in that cell is at least the
  visibility floor, not the uniform defensive share

#### Scenario: Strategies agree under learned selection

- **WHEN** `samples/usdlux.usda` is rendered with learned selection by
  power-MIS, light sampling alone and BSDF sampling alone
- **THEN** the three estimates agree within noise
