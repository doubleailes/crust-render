## 1. Shadow side (`usd_import/attrs.rs`, `light_links.rs`)

- [ ] 1.1 In `light_ray_mask`, return `MASK_INDIRECT` alone for a source that is
      not camera-visible, and `MASK_SHADOW | MASK_INDIRECT | MASK_CAMERA` for one
      that is. An authored `crust:rayMask` still wins. Correct the doc comment:
      Typhoon does not let hidden lights occlude. Verify with an import
      test that a hidden source lacks `MASK_SHADOW` and a camera-visible one
      keeps it.
- [ ] 1.2 Make the caster-class assignment in `light_links.rs` skip light-source
      geometry (design D5). Verify: the existing shadow-linking tests pass, and a
      new test shows a restricted light's shadow rays pass a hidden light source.

## 2. Bounce side (`tracer/path.rs`)

- [ ] 2.1 Add `World::has_transparent_emitters()`, set when any light source was
      attached without `MASK_SHADOW` and without `MASK_CAMERA` (design D3).
      Verify: false on a world with only camera-visible lights.
- [ ] 2.2 After `pass_cutouts`, when the hit is a transparent emitter, add its
      emission with `bounce_emission_weight` to the previous record and restart
      the segment past it, sharing the cutout crossing budget (design D4). Spend
      no depth and record no vertex. Verify with a unit test: a bounce ray
      through a hidden sphere light toward a second one collects both
      emissions.
- [ ] 2.3 Extend `VertexRec` to hold one emission slot plus an inline overflow
      (design D2). Make `eval_all` / `eval_split` and the LPE routing emit one `L`
      event per crossed emitter. Verify: the AOV test pinning `C.*[LO]` to the
      beauty passes on a new scene where a bounce crosses two hidden lights.

## 3. Equivalence and cost

- [ ] 3.1 Count instructions with callgrind (`RAYON_NUM_THREADS=1`, `-s 2`) on
      `samples/cornellbox.usda` and on the zero-AOV render. Verify:
      cornellbox's count moves by less than 0.5%, and the zero-AOV render's
      count pin in `docs/architecture.md` holds or is updated with the measured
      reason.
- [ ] 3.2 Add the scenario tests from the spec: on two overlapping hidden sphere
      lights over a diffuse floor, the light-only, BSDF-only and power-MIS
      estimates agree, and the floor equals the sum of the two single-light
      renders, within noise shown to fall as 1/√N.
- [ ] 3.3 Re-render `samples/veach_mis.usda` and the portable copy. Verify in
      numbers: the sum of the four single-light renders equals the four-light
      render, ratio 1.000 ± noise on the wall's edge columns (it was up to 1.31).
- [ ] 3.4 Run `scripts/check_images.sh check` against a `main` recording.
      Confirm every sample without a camera-invisible area light is
      bit-identical. For each sample that moves, record its relMSE against the
      old golden at 1024 spp in the design record, and re-record it.

## 4. Documentation

- [ ] 4.1 Update `openspec/specs/lighting/design.md` (area lights, camera
      visibility) with the rule, the Typhoon reference and the
      veach_mis measurement. Add the NEE ↔ bounce rule to
      `docs/architecture.md` § Invariants.
- [ ] 4.2 Update `site/content/docs/usd/lights.md` (what a hidden light is, and
      `crust:rayMask` to keep a solid one). Verify: `zola build` in `site/`.
- [ ] 4.3 Run the CI set: fmt, clippy `-D warnings`, `cargo test --workspace`.
