# Tasks

## 1. Baseline

- [x] 1.1 Build the parent commit's release binary (`bin_before`). Record
      `scripts/check_images.sh record <dir>` with it, using `--indirect-clamp 0`,
      and verify the record exits 0 with one EXR per scene.
- [x] 1.2 Record the zero-fuzz instruction count: `RAYON_NUM_THREADS=1` callgrind
      on `samples/cornellbox.usda -s 2` with `bin_before`. Keep the total and the
      `openpbr::lobes` inclusive counts for 6.2.
- [x] 1.3 Render `samples/openpbr_showcase.usda` and `samples/materialx_lion.usda`
      at `-s 64` with `bin_before`, as the "before" images for the change record.

## 2. Adobe oracle (no image change)

- [x] 2.1 Write `scripts/adobe_oracle/probe.cpp`. It reads cases (parameters,
      ω_o, ω_i) on stdin and prints Adobe's eval (diffuse and specular parts), pdf,
      directional albedo (sampler mean over a fixed quasi-random set) and emission.
      Verify it builds against `adobe/openpbr-bsdf@c91aad1` plus GLM with
      `g++ -std=c++17`, and reproduces the README example's output.
- [x] 2.2 Write `scripts/adobe_oracle.py`. It fetches the pinned Adobe commit and GLM
      into a temporary directory, compiles the probe, draws cases from a fixed seed
      (random parameters and directions, plus hand-picked corners: each layer alone,
      grazing views, roughness 0 and 1), and writes
      `crates/crust-core/tests/data/adobe_oracle.txt` with a header naming the
      commit. Verify that two runs produce byte-identical fixtures.
- [x] 2.3 Write `crates/crust-core/tests/adobe_oracle.rs`. It replays each case
      through native `OpenPBR` (the mapped parameter set, eval, pdf, emission, and
      directional albedo by crust's own sampler over the same point set), with a
      completeness test like `osl_oracle.rs::the_fixture_is_complete`. Verify that
      `cargo test -p crust-core --test adobe_oracle` runs offline.
- [x] 2.4 Implement deviation rules, each a named predicate on the case's inputs
      with a measured bound, composed by summing bounds. Write one per gap in
      `docs/openpbr_reference_alignment.md`, the current fuzz included, plus the
      interactions the fixture exposes, and make a stale rule (every case passes
      without it) fail the test. Verify that the suite passes on the current shader and that deleting
      any single rule makes it fail.
- [x] 2.5 Document it:
      - the generator's requirements and usage in its docstring;
      - a command-cookbook entry in `openspec/specs/cli/design.md`;
      - the two command lines beside the OSL oracle's in `CLAUDE.md`;
      - in `docs/openpbr_reference_alignment.md`, each gap's rule name and measured
        bound.

      Verify that every rule name in the doc exists in `adobe_oracle.rs`.

## 3. The Zeltner lobe

- [ ] 3.1 Add `scripts/tables/ltc_sheen_to_rust.py`. It converts Disney's
      `ltc-sheen` "Volume" table to a `[[f32; 3]; 1024]` constant in
      `material/brdf.rs` (or a sibling table module, as `bsdl_tables.rs` is). Verify
      its output equals Adobe's `openpbr_ltc_data.h`, value for value.
- [ ] 3.2 Implement `ZeltnerSheen` as in design D3: bilinear fetch, `eval → (R·D_ltc,
      D_ltc)`, `sample`, `albedo = R`, and the azimuth rotation. Verify with unit tests:
      - the pdf integrates to 1 over the hemisphere;
      - the sample histogram matches the pdf;
      - every sample's weight is R;
      - R at the spec's points: (0.3, 1.0) → 0.0008, (0.3, 0.25) → 0.166,
        (1.0, 1.0) → 0.342.
- [ ] 3.3 Run a white-furnace sweep of the lobe alone, α ∈ {0, 0.005, 0.01, 0.02,
      0.05, …, 1}, all cos θ_o. If it gains energy at small α, add a floor and record
      it as an oracle deviation rule (D3). Verify that the test pins whichever outcome
      holds.
- [ ] 3.4 Correct `brdf.rs`'s `sheen_charlie_v` doc (Neubelt's smoother denominator,
      not Imageworks). Add the `THIRD-PARTY.md` entry for Disney `ltc-sheen` and Adobe
      `openpbr-bsdf` (Apache-2.0), and verify that `cargo deny --locked check` still
      passes.

## 4. Native OpenPBR fuzz

- [ ] 4.1 Make `base_atten = 1 − w·R(ω_o)` in `eval_all` and `eval_split`, keeping
      the `w == 0` skip as the literal 1.0. Verify with the spec's furnace scenario
      (white diffuse, `specular_weight = 0`, any fuzz) and with the `C.*[LO]` LPE
      bitwise pin.
- [ ] 4.2 Replace `eval_fuzz` with `ZeltnerSheen`. Give the fuzz its own pdf in
      `pdf_all` and its own sampler in `mod.rs`. Make `LobePmf::selecting` take the
      view cosine, with fuzz weight `w·R·max(fuzz_color)` and base weights scaled by
      `base_atten`. Verify with new pdf-integration and sample-histogram tests for
      fuzz-only and fuzz-over-base materials at several ω_o, and with
      `the_reduced_lobe_set_samples_what_eval_reports`.
- [ ] 4.3 Give `diffuse_filter` and `albedo` the view cosine and use the directional
      `base_atten` in them. Verify that the diffuse-filter AOV identity
      (`raw × filter` = `eval_split`'s diffuse) still holds.
- [ ] 4.4 Attenuate emission in `emitted_directional` by `base_atten(cos_θo)` after
      the coat passage. Verify with the spec's "Emission dims behind a fuzz"
      scenario and an NEE-vs-bounce agreement test on an emissive fuzz surface.
- [ ] 4.5 Add Adobe's fuzz-to-coat roughness coupling where the coat α is derived,
      and zero the fuzz for `entering == false`. Verify that oracle cases with
      coat plus fuzz, and back-facing cases, match.
- [ ] 4.6 Keep `crates/crust-core/tests/resolve.rs` bit-exact for fuzz materials
      (D5), and verify that it passes.
- [ ] 4.7 Delete the fuzz deviation rule. Verify that `adobe_oracle` passes with
      every fuzz case matching within the global tolerance.
- [ ] 4.8 Document it:
      - retire "Fuzz" from `docs/openpbr_reference_alignment.md` and update its
        filter table (`base_atten` is now directional, view-side);
      - update `openspec/specs/materials/design.md`'s OpenPBR section, with the R
        table excerpt and the before/after numbers;
      - update `site/content/docs/usd/materials.md` § Fuzz (meaning of
        `fuzzRoughness`, the rim-only look at low roughness).

      Verify with `zola build` in `site/`.

## 5. MaterialX `zeltner` sheen

- [ ] 5.1 Carry `SheenMode` into the `Sheen` leaf in `crust-mtlx` and remove the
      "evaluated as conty_kulla" report. Invert the `a_zeltner_sheen_is_reported`
      test into "is silent", and verify `cargo test -p crust-mtlx`.
- [ ] 5.2 Branch `prepare_sheen`, `eval_lobe` and `sample_lobe` on the mode (D6):
      `zeltner` uses `ZeltnerSheen` with throughput `1 − weight·R`, and `conty_kulla`
      stays bit-identical. Verify with the spec's "Both Zeltner sheens agree"
      scenario and with closure pdf/sample consistency tests for a `zeltner` leaf.
- [ ] 5.3 Update `crates/crust-core/tests/mtlx_surfaces.rs`: the zeltner-reported
      assertion becomes silent, and the white-furnace bound and the Teapot
      four-layer stack are re-pinned. Verify that `cargo test -p crust-core --test
      mtlx_surfaces` passes.
- [ ] 5.4 Update `openspec/specs/materials/design.md`:
      - remove "Approximated leaves" (Zeltner) from Known gaps;
      - add the departure from MaterialX's analytic Zeltner fits (up to ≈0.01 in R,
        ≈0.1 in a⁻¹), with the measured table.

      Verify that the record's "sheen fit overestimates by up to 0.035" paragraph
      now names `conty_kulla` only.

## 6. Integration

- [ ] 6.1 Run `scripts/check_images.sh check <dir>` against 1.1's goldens. Verify
      that every scene without fuzz or a `zeltner` sheen is bit-identical, and that
      only `openpbr_showcase`, `materialx_lion` and the `materialx_surfaces` scenes
      differ.
- [ ] 6.2 Re-run 1.2's callgrind on the new binary. Verify that the `cornellbox`
      total is within noise of the baseline (the zero-fuzz path is unchanged).
- [ ] 6.3 Render 1.3's scenes with the new binary and put the before/after pair in
      the change's PR description.
- [ ] 6.4 Run the CI set: `cargo fmt --all -- --check`, `cargo clippy --workspace
      --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast` and
      `cargo deny --locked check`. Run the pinned nightly leg as well. Verify that
      all exit 0.

## Workflow follow-up

- Archive the change after review, then sync the `materials` delta into
  `openspec/specs/materials/spec.md`.
- Follow-up changes close the next oracle deviations, one rule each: MMS
  compensation, the F0→IOR `specular_weight` remap, and interior emission.
