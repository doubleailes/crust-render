## 1. Settings and flags

- [x] 1.1 Add `light_samples` and `light_samples_indirect` (default 1, at least
      1) to the tracer settings. Read `crust:lightSamples` and
      `crust:lightSamplesIndirect` off `RenderSettings`. Add `--light-samples`
      and `--light-samples-indirect` to the CLI, overriding the scene. Verify
      with an import test and a CLI parse test, including a refused 0.

## 2. Estimator (`crates/crust-core/src/tracer/path.rs`)

- [x] 2.1 Loop the surface NEE block over N (camera vertex) or M (later
      vertices) samples with a stratified pick only (design D1; the
      point-on-light coordinates come unstratified from each sample's own
      sub-domain), averaging the contributions. Verify: at N = 1, a unit test shows the
      draws equal today's.
- [x] 2.2 Carry the count in `PrevVertex`. Use `count · density` in
      `bounce_emission_weight`, `escaped_emission`, the phase arm and guiding's
      competing density (design D2). Verify: a strategy-agreement test at
      N = 4 on a diffuse floor with a sphere light, a rect light and a dome
      shows that light-only, BSDF-only and power-MIS agree within noise.
- [x] 2.3 Do the same in `volume_nee`. Verify: the strategy-agreement test on
      `samples/fog.usda` at M = 4.
- [x] 2.4 Route one light event per sample in the LPE split. Verify: the AOV
      test pinning `C.*[LO]` to the beauty passes at N = 4.
- [x] 2.5 Pick-frequency test: with N = 4 and pmfs 0.5/0.25/0.25, the counts
      per vertex are 2/1/1 exactly (design D1).

## 3. Equivalence

- [x] 3.1 Verify bit-identity at N = M = 1: `scripts/check_images.sh check`
      against a `main` recording reports no differing pixels, and callgrind on
      `samples/cornellbox.usda` (`-s 2`, one thread) is within 0.2% of `main`.
- [x] 3.2 Verify the expectation is unchanged at N = 4: on three samples, the
      difference from N = 1 falls as 1/√N across spp with `--indirect-clamp 0`.

## 4. Measurement and documentation

- [x] 4.1 With `scripts/bench_ab.sh` (interleaved) and relMSE against
      1024-spp references (4 seeds, full and trimmed), measure equal-time
      efficiency for (N, M) ∈ {1, 2, 4} × {1, 2} on the checked-in samples,
      `veach_mis`, the OpenPBR Shader Playground and ALab. Record the table and
      a recommended default in `openspec/specs/lighting/design.md`.
- [x] 4.2 Document both flags in `site/content/docs/reference/command-line.md`
      and both attributes in `site/content/docs/usd/render-settings.md`.
      Verify: `zola build` in `site/`.
- [x] 4.3 Run the CI set: fmt, clippy `-D warnings`, `cargo test --workspace`.

## Workflow follow-up

- If 4.1 recommends defaults other than 1, propose the default change as its
  own OpenSpec change, so images and goldens move in one reviewed step.
  Outcome: 4.1 recommends keeping both defaults at 1 (a 1.46–1.59× gain at equal
  time on ALab and `veach_mis`, a 10–30% loss on dome-lit, glossy and volume
  scenes), so no default change follows.
