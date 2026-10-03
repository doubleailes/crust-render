## ADDED Requirements

### Requirement: Guiding scales with the material's rough fraction

At each guided vertex the guide-selection probability SHALL be
`crust:guidingProb × f`, where `f` is the share of lobe-selection weight held by
diffuse, sheen and translucent lobes and by specular, coat or transmission lobes at
least `crust:guidingRoughnessThreshold` rough. Delta and subsurface lobes never
count. `f` below 0.01 SHALL count as 0 and above 0.99 as 1. The bounce mixture pdf
and the NEE MIS weight SHALL use the same probability. A threshold of 0 SHALL
disable the scaling.

#### Scenario: Smooth glass is left to the BSDF

- **WHEN** a guided path reaches glass whose only continuous lobe is transmission
  with roughness below the threshold
- **THEN** its bounce is sampled from the BSDF alone, with no guide lookup and no
  training sample, and its NEE weight uses the BSDF pdf alone

#### Scenario: Diffuse surfaces keep full guiding

- **WHEN** a guided path reaches a purely diffuse surface
- **THEN** the guide is selected with probability `crust:guidingProb`, as before
  this requirement, and `cornellbox_guided.usda` renders bit-identically at the
  default threshold

#### Scenario: A rough coat over a diffuse base is partly guided

- **WHEN** a material's lobe selection gives 70% to a diffuse base and 30% to a coat
  smoother than the threshold
- **THEN** the guide is selected with probability `0.7 × crust:guidingProb`

#### Scenario: Threshold zero restores guiding every continuous lobe

- **WHEN** `crust:guidingRoughnessThreshold = 0`
- **THEN** every guided vertex uses `crust:guidingProb` unscaled, delta and
  subsurface lobes included, and the image is bit-identical to a render made before
  this requirement

#### Scenario: Sharp caustics are no noisier than unguided

- **WHEN** `samples/caustic_guided.usda` is rendered with guiding at the default
  threshold and without guiding, at `--indirect-clamp 0` and equal spp
- **THEN** the guided image's relative MSE over the caustic's shadow against a
  converged reference is within seed-to-seed noise of the unguided one, and its mean
  matches within noise
