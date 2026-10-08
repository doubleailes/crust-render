## MODIFIED Requirements

### Requirement: Opt-in path guiding

When `crust:pathGuiding` is set on the scene's render settings, the renderer
SHALL run `render_guided()`: a pure-Rust Practical Path Guiding SD-tree
(`GuidingField`) trained over progressive passes at geometrically growing spp
budgets, then a final pass sampling secondary bounces by one-sample MIS
between the trained field and the BSDF. All passes (training and final) SHALL
be blended into the output, each weighted by its share of the total configured
sample budget; no blend weight SHALL depend on a rendered value. Scenes without
`crust:pathGuiding` SHALL render via the ungated path (no SD-tree, no extra
training passes).

#### Scenario: Guiding trains then renders

- **WHEN** `crust:pathGuiding = true` on the render settings
- **THEN** the renderer runs geometrically-growing training passes that splat
  samples into the SD-tree, then a final pass that mixes guided and
  BSDF-sampled directions at secondary bounces

#### Scenario: Delta lobes and untrained regions fall back to the BSDF

- **WHEN** a scattering event has no continuous BSDF component (a delta lobe)
  or lands in an untrained region of the field
- **THEN** the direction is sampled from the BSDF alone, and the estimate
  stays unbiased

#### Scenario: Passes are weighted by their sample budgets

- **WHEN** a guided render blends training passes of 2, 2, 4 and 8 spp with a
  final pass of 256 spp
- **THEN** each pass's weight is its spp divided by 272, whatever the pass's
  pixels hold, and the final pass's weight is the same whether or not adaptive
  sampling stopped some of its pixels early

#### Scenario: Rare bright paths do not darken a guided render

- **WHEN** a scene's light reaches a diffuse surface mostly through rare,
  very bright paths (fireflies, or a caustic from a small light through rough
  glass) and is rendered with and without guiding at `--indirect-clamp 0`
- **THEN** the guided image's mean over that region matches the unguided
  image's within Monte Carlo noise, with no systematic darkening
