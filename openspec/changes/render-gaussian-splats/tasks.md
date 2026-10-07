# Tasks

## 1. Probe: can openusd 0.7 read the 26.03 prim?

- [ ] 1.1 With `usd-core` ≥ 26.03 in a scratch venv, author a 3-particle `ParticleField3DGaussianSplat` twice, once in `float` and once with only the `half` twins, as `.usda` and `.usdc`; verify `usdcat` prints all six attributes for each file
- [ ] 1.2 Read those four files through openusd 0.7 in a throwaway `*_probe` example: the prim's type name, `point3f[]`, `float3[]`, `quatf[]`, `float[]`, `half*[]`, `quath[]`, the `elementSize` metadata and `uniform int`. Verify by printing the decoded values against the authored ones, and record which types decode in the design's Risks section (a missing `half` decode means `half`-only prims are skipped with a WARN and listed as a gap)

## 2. Reference fixtures

- [ ] 2.1 Add `scripts/splat_fixtures.py` (usd-core ≥ 26.03, run in a venv as `scripts/osl_oracle.py` documents). It writes the `.usda` fixtures of design Decision 8 into `crates/crust-core/tests/fixtures/splats/`: single, anisotropic+rotated, 1e-4-thin, degrees 0–3 one band each, degree 4, `half`-only, float+half conflict, mis-sized arrays, degenerate particles, unauthored SH, an invisible prim, and a surflet `ParticleField`. Verify that re-running it reproduces the committed files byte for byte
- [ ] 2.2 Have the script also write `expected.json`: per probe ray, `t*`, `d²` and `α` computed in f64, and radiance from a NumPy port of 3DGS `eval_sh` before and after the sRGB decode. Verify the port against a hand-computed degree-1 value in the script's self-check
- [ ] 2.3 Add the fixture recipe to the command cookbook in `openspec/specs/cli/design.md`, and verify the documented command runs as written

## 3. crust-rt: Gaussian-ellipsoid particle geometry

- [ ] 3.1 Add a particle geometry kind (SoA centre plus world→normalised 3×3, stored inline), its exact 3σ AABB, and attachment with a mask. Unit tests check the bound against a dense sampling of the ellipsoid surface
- [ ] 3.2 Implement the hit at `t*` with `u = d²` and `prim_id = k` for `intersect` and `occluded`. Verify with the `intersection-kernel` scenarios (closest approach, rotated anisotropic, outside support) and the 1e-4-thin fixture values against f64
- [ ] 3.3 Build particle ranges with object splits only. Verify the "ordering by peak, not by entry" scenario and the 1-vs-16-thread determinism test on 100,000 random particles
- [ ] 3.4 Verify that scenes without particles are untouched: `scripts/test_simd_matrix.sh -p crust-rt` passes and `ray_throughput` on the existing fixtures is bit-identical
- [ ] 3.5 Document particles in `openspec/specs/intersection-kernel/design.md` (the hit formula, why the `t*` order is safe under nearest-hit pruning, the object-split choice) and add particles and the k-nearest query to its known gaps

## 4. USD import of `ParticleField3DGaussianSplat`

- [ ] 4.1 Add `usd_import/particle_field.rs` (a `pub(super)` sibling with explicit imports), dispatched on the prim type name before the mesh/sphere dispatch, honouring `visibility` / `purpose` and baking the composed transform into each particle. Verify the "A splat prim renders", "The prim's transform…" and "An invisible splat prim" scenarios on fixtures
- [ ] 4.2 Read attributes float-first with the `half` fallback, quaternions real-first, linear scale and opacity, and the schema's truncate / ignore / fallback rules with one WARN per attribute. Verify with the conflict, `half`-only, mis-sized, no-positions and unauthored-SH fixtures
- [ ] 4.3 Drop degenerate particles, clamp opacity, drop opacity-0 particles silently, emit one WARN per prim with counts, and log the per-prim summary at DEBUG. Verify the "Mixed valid and broken particles" scenario, with a captured log showing exactly one WARN
- [ ] 4.4 Read the SH degree (fallback 3), stripe by the authored element size, and render bands 0–3 with a WARN above degree 3. Skip non-ellipsoid `ParticleField` prims with a WARN naming the kernel. Verify the degree-4 and surflet fixtures
- [ ] 4.5 Write `site/content/docs/usd/particle-fields.md`: the supported prim, attribute rules, PLY → USD conversion happening outside Crust, and the colour space. Link it from `usd/_index.md` and verify `zola build` (0.21) passes its link check

## 5. Splat radiance

- [ ] 5.1 Implement the 3DGS SH evaluation (degrees 0–3, the reference constants and order) toward the ray direction rotated into the field's local frame, with `max(0, 0.5 + Σ)`. Verify every degree fixture against `expected.json` to 1e-6 relative
- [ ] 5.2 Resolve the coefficients attribute's colour space (metadatum, else `colorSpace:name`, else `srgb_texture`) and apply it to the evaluated radiance. Verify the "Unnamed radiance is display-encoded" (≈0.214) and "A named linear space is honoured" (0.5) scenarios
- [ ] 5.3 Add splats to the input inventory and the one-rule exceptions in `docs/color_management.md`, and verify the table renders in the site build

## 6. Transport through the integrator

- [ ] 6.1 Add the splat-field material: presence `opacity[k]·exp(−½u)`, `emitted_at` = converted radiance, no BSDF. Keep the light-list builder blind to it. Verify with a test that a splat-only scene has an empty light list
- [ ] 6.2 Attach particle geometry without the shadow-ray mask bit. Verify "Splats cast no shadow" (the light-sampled floor estimate equals the estimate without splats) and that `surface_visibility` never visits a particle
- [ ] 6.3 Route met and passed splats through `pass_cutouts`, treating a particle as one-sided under the re-hit rule. Verify "Two half-present splats in a row" → 0.5 and "A splat is not met twice" → 0.5 within noise falling as 1/√N, plus the "Crossing bound" scenario
- [ ] 6.4 Verify that indirect rays see splats: the "A chrome sphere reflects the capture" scenario, and "A capture lights CG by bounces only" converging as 1/√N at `--indirect-clamp 0`
- [ ] 6.5 Make a met splat an `O` event with first-hit AOVs (depth `t*`, normal `−ω̂`, albedo as for an emissive material outside the light list). Verify "An LPE routes splats as objects" and that `C.*[LO]` stays bitwise equal to the beauty
- [ ] 6.6 Verify zero cost: the Cornell box at `-s 16` is bit-identical (`scripts/check_images.sh check`), and its `RAYON_NUM_THREADS=1` callgrind count is within 0.1 % of the parent commit's
- [ ] 6.7 Record the transport in the `rendering` and `lighting` design records: splats as emitting cutouts, the shadow mask, the reason they are not light-list entries, and the re-hit trap at `t*`

## 7. Statistics and measurement on a real capture

- [ ] 7.1 Add `--stats` counters: fields, particles, dropped, particle and BVH memory, and mean / max splat crossings per ray, plus rays that reached the crossing bound, emitted on `STATS_TARGET`. Verify the "Stats on a capture" scenario on a fixture and document the lines in `site/content/docs/reference/command-line.md`
- [ ] 7.2 Render the reference capture (1,256,332 particles, from a production-renderer tutorial; not committed) through a local wrapper `.usda` with the 180° flip and a camera. Record import time, memory, Mray/s and crossings per ray (`--stats`) in the new design record. Verify the numbers are reproducible across two runs within `bench_scenes.sh` noise
- [ ] 7.3 If a production-renderer or `hdParticleField` reference EXR of the same view can be obtained, report relmse with `exr_diff` for both colour-space defaults and confirm or flip Decision 7. Otherwise record in the design record that the default is unconfirmed
- [ ] 7.4 Write `openspec/specs/particle-fields/design.md`: the representation choice and rejected alternatives, the reference-capture measurements, the known differences from rasterised 3DGS, and the known gaps. List the gaps in the site's architecture/limitations page and verify `zola build` passes

## 8. Integration checks

- [ ] 8.1 Verify that `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast` and `cargo deny --locked check` all pass
- [ ] 8.2 Verify the pinned nightly leg: `cargo +nightly-2026-09-26 clippy --workspace --all-targets -- -D warnings` and `cargo +nightly-2026-09-26 test -p crust-rt --features bvh8` pass
- [ ] 8.3 Verify that `openspec validate render-gaussian-splats --strict` passes

## Workflow follow-up

- Archive the change once reviewed. That syncs `particle-fields` and the `intersection-kernel` delta into `openspec/specs/`.
- After archive, check that `openspec/specs/particle-fields/spec.md` has its Purpose and that `docs/architecture.md` lists the new capability in its map.
