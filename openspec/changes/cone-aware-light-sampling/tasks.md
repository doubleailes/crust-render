# Tasks

## 1. Measurement scenes

- [ ] 1.1 Author `samples/spotlights.usda` (design D1): six small spots on disjoint pools (four sphere lights, two IES fixtures from `samples/ies/spot30.ies`), a dim fill, and one wide shaped disk near a subject. Verify that `crust check -i samples/spotlights.usda --json -` reports no warnings and `cargo test --workspace` (which loads every sample) passes.
- [ ] 1.2 Render the stage with `--stats` and check in numbers, not by eye: each pool is lit only by its own spot, the gaps are lit only by the fill, and the frame renders in a few seconds at 16 spp. Verify that `crust light-paths` on one pool pixel attributes its direct light to that pool's spot alone.
- [ ] 1.3 Write `scripts/make_spot_sweep.py <out_dir>` (design D1): 96 shaped rect/disk frames plus the 8 unshaped-disk rows, named by their parameters. Verify that it runs, that every generated frame passes `crust check`, and that the frame count matches.
- [ ] 1.4 Add the stage to the golden set (`scripts/check_images.sh record`) and mention the sample and the generator in the `lighting` design record. Verify that `check_images.sh check` passes.

## 2. Baseline

- [ ] 2.1 Render 1024 spp references (`--indirect-clamp 0`, today's defaults) for the stage, `usdlux` and every sweep frame. Verify that the reference commands are recorded in the change's `notes.md` and are reproducible from it.
- [ ] 2.2 Measure gate (A): power vs `learned` relMSE (mean of 4 seeds, 16 spp) and Render time (`bench_ab.sh`, min and mean) on the stage, overall and in the pools. Verify that the table is in `notes.md`, with a one-line conclusion on whether `learned` removes the waste.
- [ ] 2.3 Measure the case (B) baseline: `area` vs `ellipse` on every disk row, and today's rect sampling on the shaped rect rows. Record the fraction of NEE samples that land outside the cutoff circle (from a temporary counter or a probe, not committed). Verify that `notes.md` has the table and the zero-sample fraction for each row.
- [ ] 2.4 Write the gate (A) outcome into the `lighting` design record ("Known gaps: light sampling") and, if it bears on that change, into `learned-light-selection-by-default`'s notes. Verify that the docs build (`zola build` in `site/`), if a user page changed.

## 3. Shaping cone handed to the shape

- [ ] 3.1 Add `CRUST_CONE_CLIP` (`off` | `on`, default `off`) to `crust_core::Config`, and the value `polar` to `CRUST_DISK_SAMPLING`. Add rows to `docs/architecture.md` § Environment switches and sections in `site/content/docs/reference/environment-variables.md`. Verify with the config parsing unit tests and `zola build`.
- [ ] 3.2 Have `AreaLight::new` pass `(axis, tan θc, focus)` from the emitter's shaping to the shape, as it passes one-sidedness (design D3). Pass nothing when `θc ≥ 90°` or the axis is not the normal within 1e-6. Verify with a unit test per case (rect, disk, sheared rect, tilted axis, 90° cone).
- [ ] 3.3 Implement the conservative cutoff radius `ρc = h · tan θc · (1 + 1e-5)` (design D3 trap). Verify with a unit test that, over a dense grid of points just outside `ρc`, `Shaping::factor` is exactly zero, including with softness and focus.

## 4. Cone-clipped rect (gated on 2.3 showing waste on shaped rects)

- [ ] 4.1 Implement the clipped sub-rectangle in `RectShape`'s solid-angle sampler (design D4), with `None` from both `sample` and `pdf` for an empty clip. Verify with unit tests: every sample lies in the clip square, `sample`'s pdf equals `pdf` at the placed point bit for bit, and an empty clip yields no sample.
- [ ] 4.2 Add the spec's unbiasedness test: light-only, BSDF-only and MIS estimates of a floor under a shaped rect agree within 1/√N, for `CRUST_CONE_CLIP` `off` and `on`. Verify that it passes in `cargo test -p crust-core`.
- [ ] 4.3 Verify bit-identity: `check_images.sh check` passes with `CRUST_CONE_CLIP=off`, and with `on` on every sample without a qualifying shaped rect.
- [ ] 4.4 Measure clip vs off on the shaped rect rows, the stage and `usdlux` (design D2 protocol, including callgrind per-sample cost). Record the table in `notes.md` and the gate verdict.

## 5. Polar disk sampler (gated on 2.3)

- [ ] 5.1 Implement the polar strategy in `AffineShape` (design D5): the roundness check, the wedge, the ray–circle span, the closed-form ρ CDF for `k = 0` and `k = focus`, and the cancellation-free slice pdf. Keep the body out of line. Verify that `round_strategies_do_not_grow_the_sampler` still passes, that `sample`'s pdf equals `pdf` bit for bit, and with a brute-force test that the density integrates to 1 over the disk.
- [ ] 5.2 Add the cone cap under `CRUST_CONE_CLIP = on`. Verify with a unit test that every capped sample lies within `ρc`, and that a slice with `ρ1 ≥ ρc` is never drawn.
- [ ] 5.3 Add a precision test in the spirit of `far_end_on_tube_samples_stay_equiangular`: a thin, far slice whose `cos α1 − cos α2` would cancel in f32 keeps distinct strata and a matching pdf. Verify that it passes.
- [ ] 5.4 Extend the "Disk and cylinder sampling is unbiased" test to `polar`, shaped and unshaped, cap on and off, and add the elliptical-disk fallback test from the spec. Verify that both pass.
- [ ] 5.5 Measure polar vs area (and vs ellipse) on every disk row, the stage and `usdlux` (design D2 protocol, callgrind per-sample cost). Record the table in `notes.md` and the gate verdict.

## 6. Defaults and records

- [ ] 6.1 Apply the gate verdicts from 4.4 and 5.5. For each default that flips, change it in `config.rs`, the spec text in this change's delta, `docs/architecture.md` and `reference/environment-variables.md`, and re-record the goldens. Verify that `check_images.sh check` passes against the new goldens and that the old behaviour reproduces bit for bit with the old switch value.
- [ ] 6.2 Write the "Cone-aware sampling" section of the `lighting` design record: the foot-point geometry, the traps (conservative cap, cancellation-free slice pdf), the measurements, and the verdicts. Update its "Known gaps" and the user limitations page to match the spec's narrowed gap. Verify that `zola build` passes and that the warnings/docs consistency tests pass.
- [ ] 6.3 Run the full CI set locally: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`, and `openspec validate cone-aware-light-sampling --strict`. Verify that all pass.

## Workflow follow-up

- If gate (A) shows `learned` removes the stage's waste, cite the stage numbers in `learned-light-selection-by-default`.
- Archive the change once its gates are recorded and the PR is merged.
