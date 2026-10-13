# Tasks

## 0. Baseline

- [ ] 0.1 From `main`, build the release binary and copy it aside. Record goldens with
      `scripts/check_images.sh record <dir>`, cornellbox's callgrind counts (2 spp, one thread,
      total and `render_pixel`-inclusive), the clean `cargo build --release` time and the
      `crust` binary size. Verify that the four numbers and the golden directory are written
      down in the change's working notes.

## 1. Materials take numbers, Sobol only (design D1)

- [ ] 1.1 Add `BsdfDraw<'a>` (`Lazy(&dyn Fn() -> [f32; 4])`, `Fixed([f32; 4])`, `u()`) to
      `material/material.rs`, re-exported from `lib.rs`. Change
      `Material::scatter_importance` to take it. Update `Emissive` (never calls `u()`),
      `ExitLambertian` (`u()[..2]`), `OpenPBR`, `MtlxMaterial` and the pattern blanket impl.
      Verify that `cargo build -p crust-core` succeeds and a unit test shows
      `Emissive::scatter_importance` never invokes its `Lazy` closure.
- [ ] 1.2 Make `OpenPBR::scatter_with`, `scatter_resolved`, `scatter_split`,
      `ResolvedOpenPBR::{scatter, scatter_split, scatter_without_straight}` and
      `ResolvedClosure::{scatter, scatter_choosing, scatter_split}` take
      `impl FnOnce() -> [f32; 4]`, called where `draw_sample_f32::<4>()` is today (after the
      back-facing early return). Verify that `scatter_split_draws_the_same_direction` passes
      unchanged.
- [ ] 1.3 Make `ResolvedClosure::straight_transmittance` and
      `ShadingPoint::straight_transmittance` take `impl FnOnce() -> [f32; 2]`, still called
      inside `get_or_insert_with`. Verify with a unit test: a closure with no thin-walled
      transmitting leaf never invokes it.
- [ ] 1.4 Make `ShadingPoint::{scatter_importance, scatter_split}` generic over the closure,
      building `BsdfDraw::Lazy` only on the `Resolved::Material` arm. Change the tracer's call
      sites in `tracer/path.rs` to pass `|| bsdf_dom.draw_sample_f32::<4>()` (and `<2>` for
      the pass-through). Verify that `cargo test --workspace` passes.
- [ ] 1.5 Migrate the 24 `PathSampler::new(0, 0, 0, i)` test sites in `material/closure/tests.rs`,
      `material/openpbr/tests.rs` and `subsurface/tests.rs` to `BsdfDraw::Fixed` / literal
      closures. Rewrite any test that searches sample indices for a lobe to hand it exact
      uniforms. Add the `materials` spec's "Exact uniforms select a lobe" test. Verify that
      `cargo test -p crust-core` passes.
- [ ] 1.6 Verify the step is invisible. `scripts/check_images.sh check` against the 0.1 goldens
      shows no change, and cornellbox's callgrind counts are within 0.2% of 0.1's (design D7).
      Fix any inlining drift with an annotation and record its measured cost in a comment, as
      the existing annotations do.
- [ ] 1.7 Update the `materials` design record (the scatter contract: numbers in, one 4D block,
      the lazy pass-through, `BsdfDraw` and its `rng` extension point). Change the "materials draw
      one 4D block from the domain they are handed" paragraph in the `rendering` design record.
      Verify that `zola build` in `site/` still passes if any page links there.

## 2. A generic integrator, Sobol only (design D2)

- [ ] 2.1 Make every `tracer/path.rs` function that takes `PathSampler` generic over
      `S: openqmc::SamplerImpl` (`Sampler<S>`), keeping `nee_sampler` and every `K_*` key as
      they are. Verify that `cargo build -p crust-core` succeeds.
- [ ] 2.2 Make `subsurface::random_walk` and its walk state generic the same way. Verify that
      `cargo test -p crust-core subsurface` passes.
- [ ] 2.3 Split `render_pass` into a sampler-agnostic shell and a generic
      `render_pass_with::<S>()` holding today's body, with `render_pixel` and `advance_pixel`
      generic. The shell still calls only `SobolImpl`. Verify that `cargo test --workspace`
      passes.
- [ ] 2.4 Verify the step is invisible, as in 1.6: goldens identical and callgrind within
      0.2%. Also run `bench_ab.sh` against the 0.1 binary on cornellbox, veach_mis,
      materialx_basic and openpbr_showcase, reporting min and mean.

## 3. The setting (design D3, D5, D6)

- [ ] 3.1 Add `SamplerKind` (`CHOICES`, `FromStr`, `name()`, `Default = Sobol`) and
      `RenderSettings::{with_sampler, sampler}`. Verify with unit tests: every token
      round-trips through `FromStr` / `name()`, and `"halton"` is an error.
- [ ] 3.2 In the shell from 2.3, match `SamplerKind` once per pass, calling
      `Sampler::<S>::warm_cache()` before the pass timer starts, then `render_pass_with::<S>()`.
      Verify with a test: a pass with `pmj` reports a `Render` phase that excludes the table
      build (the build is logged at DEBUG, outside the phase).
- [ ] 3.3 Measure the build cost against 0.1's numbers: clean release build time and binary
      size. If over +40% or +25% (design D4), stop and cut the set to `sobol`, `sobolBn`,
      `lattice`, `pmj`, amending the `rendering` delta's value list before going on. Verify
      that the numbers are recorded in the `rendering` design record.
- [ ] 3.4 Keep `light_cache.rs` on the concrete `PathSampler` (design D5). Verify the
      `rendering` spec's "Same table under two samplers" scenario with a test comparing the
      learned tables built under `sobol` and `latticeBn`.
- [ ] 3.5 Read `crust:sampler` in `scene/usd_import/settings.rs`, raising `SettingsInvalidValue`
      and falling back to `sobol` on an unknown token. Verify with tests in
      `tests/usd_inline.rs`: `"lattice"` selects Lattice and `"halton"` yields one warning
      naming `crust:sampler`.
- [ ] 3.6 Add `--sampler` with `choices(SamplerKind::CHOICES)` to the shared flags of `render`,
      `check` and `diagnostic`, overriding the stage. Add the effective-settings row
      (`sampler`, `--sampler`, `crust:sampler`). Verify with CLI parse tests in `main.rs` (an
      unknown value is a usage error listing the six) and a `crust check --sampler sobolBn
      --json -` test.
- [ ] 3.7 Stamp `crust:sampler` into every EXR (`stamp.rs`). Add it to `compare.rs`'s
      comparability keys, reading an absent key as `sobol`. Verify with
      `crust-render/tests/stamp.rs` and the `image-comparison` spec's three scenarios as tests
      in `compare.rs`.
- [ ] 3.8 Check how an MCP session picks up a `crust:sampler` edit. It should behave like a
      `crust:pixelFilter` edit; if `live::Signature` or the session's settings refresh needs the
      field, add it there. Verify with a test: an MCP edit setting `crust:sampler` renders the
      same pixels as a fresh import of the edited layer.
- [ ] 3.9 Add a convergence test: on a small, cheap scene (cornellbox at low resolution,
      `--indirect-clamp 0`), each of the six samplers' mean over N seeds agrees with `sobol`'s
      within 3 standard errors. Verify that it runs in under a minute in `cargo test --release`.
      If not, mark it `#[ignore]` with the command to run it in the `rendering` design record.
- [ ] 3.10 Document. Add `crust:sampler` to `site/content/docs/usd/render-settings.md` and
      `--sampler` to `site/content/docs/reference/command-line.md`, each with the spike's
      guidance: `sobol` by default; Lattice depends on the scene; `*Bn` is for low-spp previews
      and denoised output, at 3–10% time. Update the `rendering` design record (the sampler
      setting, D3–D5, the spike's tables, and the `*Bn` 256-pixel tile-seam known gap) and
      `docs/architecture.md` (the integrator's sampler parameter). Verify that `zola build` in
      `site/` passes (Zola 0.21).

## 4. Integration

- [ ] 4.1 Run the CI set locally: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`, `cargo deny --locked check`, and the pinned
      nightly clippy and `bvh8` legs from CLAUDE.md. Verify that all pass.
- [ ] 4.2 Run the final default-render check: goldens identical, `--sampler sobol` bit-identical
      to no flag, callgrind within 0.2% and `bench_ab.sh` against 0.1's binary (design D7).
      Verify that the numbers are recorded in the PR description.
- [ ] 4.3 Run `openspec validate sampler-agnostic-materials --strict` and verify that it passes.

## Workflow follow-up

- Archive the change once merged, syncing the deltas into `rendering`, `materials`, `cli`,
  `image-output` and `image-comparison`.
- Follow-up change (non-goal here): a `crust diagnostic` trial over samplers.
