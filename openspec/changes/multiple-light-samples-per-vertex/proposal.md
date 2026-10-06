## Why

crust takes one light sample per vertex. Its samples are cheap, but on a
production-like interior its direct lighting is noisy for its cost. On the
OpenPBR Shader Playground (432×324, box filter, 16 threads), crust renders a
sample about 11× faster than a comparison renderer that casts about three shadow
rays per shading point, but each crust sample is about 8× noisier (trimmed
relMSE against its own 1024-spp reference). A crust shadow ray costs about
0.27 µs of thread time against about 1.5 µs per shading point, so several light
samples per vertex are affordable. Direct-light variance falls roughly as
1/N in the number of samples.

## What Changes

- **Several light samples per vertex.** `crust:lightSamples` / `--light-samples
  N` sets the number of NEE samples at the camera vertex.
  `crust:lightSamplesIndirect` / `--light-samples-indirect M` sets it at every
  later surface and volume vertex. Both default to 1.
- **Stratified picks.** The N samples at a vertex stratify the light-pick
  dimension and the point-on-light dimensions, so N samples spread over the
  lights in proportion to their selection probabilities instead of repeating
  one choice.
- **Multi-sample MIS.** The bounce side weighs a light hit against N times the
  NEE density used at the vertex the bounce left (Veach's `n_i · p_i`). Every
  NEE ↔ bounce pair moves together: surface NEE ↔ BSDF bounce, volume NEE ↔
  phase bounce, and escaped infinite light ↔ NEE on a dome.
- **Unchanged by default.** With both counts at 1, every image is bit-identical
  and costs the same instruction count.
- A measurement of equal-time relMSE for N, M ∈ {1, 2, 4} on the checked-in
  samples, veach_mis, the OpenPBR Shader Playground and ALab. It recommends
  defaults, and a default change follows as its own change.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `lighting`: "Light selection" takes a per-vertex sample count. "One density for
  both MIS strategies" states the multi-sample density.

`learned-light-selection-by-default` also modifies "Light selection". Whichever
change archives second must restate that requirement with both applied.

## Impact

- `crates/crust-core/src/tracer/path.rs`: the surface NEE block and `volume_nee`
  loop over N stratified samples. `bounce_emission_weight` and
  `escaped_emission` take the count used at the previous vertex, which
  `PrevVertex` carries.
- `crates/crust-core/src/tracer/settings.rs`, `scene/usd_import/settings.rs`,
  `crates/crust-render/src/main.rs`: the two settings and the two flags.
- Guiding's mixture ↔ NEE pair and the learned light cache: the cache keeps its
  own training sample count. Guiding's competing NEE density uses the same N.
- LPE routing: one light event per sample, each with its own weight.
  `C.*[LO]` stays pinned to the beauty.
- `--stats`: shadow rays per shading point rise with N.
- Documentation: `site/` (`reference/command-line.md`, `usd/render-settings.md`)
  and the `lighting` and `rendering` design records.
