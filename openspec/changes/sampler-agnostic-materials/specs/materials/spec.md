## ADDED Requirements

### Requirement: A BSDF sample is a function of the numbers it is given

A material SHALL sample a scatter from at most four uniform numbers in [0, 1), passed to it by
the integrator, and never from a sampler or other source of randomness. The same material, hit
record, incoming ray and numbers SHALL always give the same `ScatterSample`, whichever QMC
sampler produced the numbers. A material that needs no numbers for a query SHALL NOT cause any
to be drawn.

#### Scenario: Exact uniforms select a lobe

- **WHEN** a test hands an OpenPBR material with a coat four uniforms whose first value falls
  in the coat's share of the lobe pick
- **THEN** the returned sample is the coat lobe's, and the same four numbers return the same
  sample on every call

#### Scenario: The numbers are what the old sampler drew

- **WHEN** the integrator samples a BSDF at a vertex under the `sobol` sampler
- **THEN** the material receives exactly the four values the BSDF domain's 4D block holds,
  so its sample is bit-identical to the one it drew from that domain before this change

#### Scenario: An emitter draws nothing

- **WHEN** a path's scatter is queried on a purely emissive surface
- **THEN** no sample block is drawn for it

#### Scenario: A lazy draw stays lazy

- **WHEN** a MaterialX closure with no thin-walled transmitting leaf is asked for its
  straight transmittance
- **THEN** no sample block is drawn for it
