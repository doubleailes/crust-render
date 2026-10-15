# Tasks

## 1. Light bounds (`crates/crust-core/src/light/`)

- [x] 1.1 Add `LightBounds` (box, axis `w`, `cos_theta_o`, `cos_theta_e`,
      `phi`, `two_sided`) and `Light::bounds() -> Option<LightBounds>`
      (default `None`) (design D2). Verify: `cargo build -p crust-core`, and
      lights at infinity return `None`.
- [x] 1.2 Implement `bounds()` for `AreaLight` over every `AreaShape`
      (rect, sphere, affine sphere/disk/tube; one- and two-sided; shaped by
      `Shaping::axis()` and its cone angle), with `phi = Light::power`.
      Verify with a unit test per shape: 4 096 `sample_point`s lie in the
      box, and every direction a stratified sweep finds `radiance_toward`
      non-zero for lies within `θo + θe` of `w`.

## 2. The tree (`crates/crust-core/src/light/tree.rs`)

- [x] 2.1 Implement the importance function (design D4): `d²` clamped by
      the box, `θo` / `θb` / `θe` in cosines, the receiver term signed at an
      opaque receiver and `|n·ω|` otherwise, with no normal dropping the
      surface terms, plain `f32`. Verify with unit tests: zero for a node
      wholly below an opaque receiver's horizon and for one whose cone
      misses the point; non-zero below a transmissive receiver and with no
      normal; decreasing with distance.
- [x] 2.2 Implement the binned SAOH build (D3) over lights with
      `bounds().is_some()` and `phi > 0`, with a flat node array, per-light
      bit trails (depth ≤ 64) and deterministic tie-breaks. Verify: building
      twice from the same list gives identical nodes, and a 4 096-light
      random list builds without exceeding depth 64.
- [x] 2.3 Implement the pick by monotone warp and the pmf by trail (D6).
      Verify with unit tests over random `(p, n, u)` on 2, 7, 47 and 1 000
      lights: the pmf of every light sums to 1 within 1e-5; the picked
      light's pmf from the pick equals `pmf` by trail bit for bit; the picked
      leaf index never decreases along a sweep of `u`; and the leaf
      importance matches a flat per-light evaluation (the oracle, D9).

## 3. `tree` as a selection mode, both MIS sides

- [x] 3.1 Add `LightSelection::Tree` (`"tree"` in `CHOICES` with a one-line
      description), parsed from `--light-selection` and
      `crust:lightSelection`. In `select_by(Tree)`, build the tree over the
      power table, or skip it when at most one emitting finite light exists
      (D8). Verify: `names.rs`'s `round_trips` test passes, and
      `crust render --help` lists `tree`.
- [x] 3.2 Make `pick_index_at`, `pmf_at`, `find_index_by_geom_at` and
      `infinite_at` take the receiver's normal, and under `Tree` return the
      D5 mixture `(1 − D) · p_tree + D · p_powerfinite` within the finite
      share, with `D` a named constant (0.25 for now). Verify: a unit test
      that every emitting finite light's pmf is ≥ `D · p_power` at random
      `(p, n)`, including a light wholly behind the receiver.
- [x] 3.2b Add `Material::transmits()` (default `true`) and answer `false`
      for opaque `OpenPBR`, reflection-only MaterialX materials and the
      subsurface exit's Lambertian (design D4). Verify with unit tests: an
      `OpenPBR::default()` is opaque, one with `transmission_weight = 1` or
      thin-walled subsurface is not, and a MaterialX `dielectric_bsdf` in `T`
      mode is not.
- [ ] 3.3 Pass the receiver at every call site: surface NEE (`rec.normal`, opaque by `transmits()`),
      volume NEE (zero), the shadow-link twins, `bounce_emission_weight` and
      `escaped_emission`. Add `normal` to `PrevBounce`, set from the same
      value NEE used, and pass zero for `PrevVertex::Phase`. Extend
      `nee_picks_from` / `bounce_weighs_from` to compare `(p, n)` bits.
      Verify: `cargo test --workspace` passes in debug with those
      assertions on, rendering `usdlux` and `veach_mis` under `tree`.
- [ ] 3.4 Verify the other modes are untouched:
      `scripts/check_images.sh record` on `main` and `check` on this branch,
      each under `--light-selection power`, `uniform`, `learned` and
      `neural`, with 0 differing pixels on every sample. Also check a
      one-rect-light-plus-dome scene under `tree` against `power`, bit for
      bit (spec "matches power").
- [ ] 3.5 Add `crates/crust-core/tests/tree_selection.rs`: tiled ↔ scanline
      and `RAYON_NUM_THREADS=1` ↔ default bit-identity under `tree`, and
      the light-only / BSDF-only / MIS agreement on a floor lit by sphere
      lights, rect lights and a dome. Verify: the tests pass.
- [x] 3.6 Document `tree` as an opt-in mode:
      `site/content/docs/reference/command-line.md` (`--light-selection`),
      `site/content/docs/usd/render-settings.md` (`crust:lightSelection`) and
      a "Light tree" section in `openspec/specs/lighting/design.md`
      (bounds, importance, D, the normal on `PrevBounce`, the pair it adds to
      "Pairs that must change together"). Verify: `zola build` in `site/`
      (Zola 0.21).

## 4. The many-lights sample and tuning

- [ ] 4.1 Author `samples/many_lights.usda` (design D11): 256 lights
      (sphere grid, rect panels facing both ways, narrow spots, flux over two
      decades) over a floor and wall with occluders. Verify: it renders in
      under 10 s at 16 spp, `crust check -i samples/many_lights.usda` reports
      no warnings, and the integration tests that load `samples/*.usda` pass.
- [ ] 4.2 Choose `D` from {0.1, 0.25, 0.5}: full and trimmed relMSE at
      16 spp, 4 seeds, `--indirect-clamp 0`, against a 1024 spp `power`
      reference, on `usdlux`, `veach_mis`, `domelight` and `many_lights`.
      Record the table in `docs/light_sampling.md` (a new §3.15). Verify:
      the constant matches the recorded choice.
- [ ] 4.3 Measure cost: callgrind instruction counts at 2 spp on
      `cornellbox`, `usdlux` and `many_lights`, `tree` against `power`.
      Record them in §3.15. Verify: the numbers are in the document.
- [ ] 4.4 Verify unbiasedness: relMSE × spp stays flat from 16 to 256 spp
      under `tree` on `usdlux` and `many_lights`; `veach_mis`'s mean at
      1024 spp is within noise of `power`'s. Record in §3.15.

## 5. The gate and the default (design D10)

- [ ] 5.1 Measure equal-time relMSE, `tree` against `power`, full and
      trimmed, 4 seeds, `--indirect-clamp 0`, with `scripts/bench_ab.sh`
      timing (one binary behind two wrapper scripts), on every checked-in
      sample, `veach_mis`, the OpenPBR Shader Playground, ALab and
      `many_lights`. Record the table in §3.15.
- [ ] 5.2 If every scene passes, make `tree` the default in
      `tracer/settings.rs`, the USD import fallback
      (`scene/usd_import/settings.rs`) and the CLI help, and change
      `diagnostic/checks.rs`'s `many_lights_uniform` to recommend the
      default. Verify: a render with no `crust:lightSelection` uses `tree`;
      `--light-selection power` is bit-identical to the `main` recording
      from 3.4; the diagnostic tests pass.
- [ ] 5.3 If any scene fails, keep `power` as the default. Revert the delta
      spec's "Tree selection is the default" scenario and the default in its
      `Light selection` text to `power`. List the failing scenes and their
      numbers in the lighting design record. Verify: `openspec validate
      light-tree-selection` passes.
- [ ] 5.4 Update for the outcome: the lighting design record's "Known gaps:
      light sampling" (selection by importance; what the tree still cannot
      see: visibility, glossy lobes, infinite lights' share),
      `docs/light_sampling.md` §9.3 m, the site pages from 3.6 (the default
      and the `power` migration note) and `docs/architecture.md` if the
      invariants list changes. Verify: `zola build` in `site/`.
- [ ] 5.5 Run the CI set: `cargo fmt --all -- --check`, `cargo clippy
      --workspace --all-targets -- -D warnings`, `cargo test --workspace
      --no-fail-fast`, `scripts/test_simd_matrix.sh -p crust-core`. Verify:
      all pass.

## Workflow follow-up

- Archive the change once reviewed; the `lighting` spec's "Light selection"
  then carries `tree`.
- Resume `learned-light-selection-by-default` through `/opsx:update`, per the
  on-hold note in its proposal.
- Add a note to `wavefront-integrator`'s design: `PrevBounce` now carries the
  normal.
