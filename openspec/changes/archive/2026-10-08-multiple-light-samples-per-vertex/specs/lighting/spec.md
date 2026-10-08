## MODIFIED Requirements

### Requirement: One density for both MIS strategies

For any light and any point on it, NEE and the BSDF bounce side SHALL compute
the same sampling density (the light's own pdf times its selection probability,
times the number of light samples NEE took at that vertex) and the same
radiance. Where a density is not finite (edge-on, degenerate) the point SHALL be
refused on both sides rather than given a finite stand-in.

#### Scenario: A bounce ray hits a light NEE could also have sampled

- **WHEN** a BSDF-sampled ray hits the emitting surface of a light in the list
- **THEN** its emission is MIS-weighted against the density NEE would have used
  for that same point, including the number of light samples taken at the
  vertex the ray left

#### Scenario: Several light samples keep the strategies consistent

- **WHEN** a diffuse floor lit by a sphere light, a rect light and a dome is
  rendered with `--light-samples 4 --light-samples-indirect 4`
- **THEN** the light-only, BSDF-only and power-MIS estimates of the floor agree
  within noise

### Requirement: Light selection

NEE SHALL take `crust:lightSamples` / `--light-samples` light samples at the
camera vertex and `crust:lightSamplesIndirect` / `--light-samples-indirect` at
every later surface and volume vertex (each default 1, from 1 to 1024; a stage
value outside that range is clamped with a warning, a command-line one refused).
The light
of each sample SHALL be chosen by `crust:lightSelection` / `--light-selection`:
`power` (default; infinite lights keep a uniform share, the finite lights split
the rest half evenly and half by flux), `uniform` (bit-identical to the
historical renderer), or `learned` (a visibility-aware per-cell table trained by
a deterministic pre-pass, over the power table). The samples at one vertex SHALL
be stratified, so that each light is chosen close to the count times its
selection probability.

#### Scenario: Uniform selection

- **WHEN** a render runs with `--light-selection uniform`
- **THEN** each of N lights is picked with probability 1/N

#### Scenario: One sample per vertex is unchanged

- **WHEN** a render runs with both sample counts at 1, given or by default
- **THEN** the image is bit-identical to the renderer before sample counts
  existed

#### Scenario: Stratified picks

- **WHEN** a vertex takes 4 light samples among three lights whose selection
  probabilities are 0.5, 0.25 and 0.25
- **THEN** the first light is sampled twice and each other light once

#### Scenario: More samples, same image

- **WHEN** a scene is rendered with `--light-samples 4` and with the default,
  at increasing sample counts with the indirect clamp off
- **THEN** the difference between the two falls as 1/√N, and the direct-light
  noise per pixel sample is lower with 4
