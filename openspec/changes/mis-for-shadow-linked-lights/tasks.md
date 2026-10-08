## 1. Baseline

- [ ] 1.1 Record the "before" in `docs/alab_profile.md` (dated section), following the
      design's Context. Include:
      - the one-frame layer-offset seed trick;
      - the light-group overlay (`crust:light:lpeTag` per light, `C<RG><L.'g'>` /
        `C<RD><L.'g'>` vars);
      - the `(a − b)²/4` variance estimate;
      - the power / learned / light-samples / strategy / blockers-off tables, with the
        exact commands.
- [ ] 1.2 Add a linked test scene: a glossy floor (roughness ~0.15) lit by a sphere light
      through an occluder the light's `collection:shadowLink` excludes, so the link
      changes the image. Render it under `light`, `bsdf` and `power`, 4 seeds, fixed
      spp, adaptive off, `--indirect-clamp 0`. Record today's means (bsdf disagrees)
      and variances (power = light).

## 2. Light-side support test (D2)

- [ ] 2.1 Add a ray–shape intersection to the area-light shapes (sphere, rect, affine
      unit disk and cylinder wall). It returns the first point and distance along a ray
      from `from`, on exactly the domain the shape samples (one-sidedness included).
- [ ] 2.2 Pair test: for each shape, fire rays from many origins and check the analytic
      hit against `World::intersect` on the light's own geometry. The hit or miss must
      agree, and the distance must agree within the kernel's tolerance.
- [ ] 2.3 Expose a "point along ω" query on the light (area: 2.1; distant: `escaped`;
      dome: none), with the emission answered by `Emissive::radiance_toward` /
      `escaped`, the single emission answer NEE uses.

## 3. Link flags (D5)

- [ ] 3.1 Split `LightLinks::nee_only` into:
      - `restricted`: the shadow mask is set;
      - `nee_only`: a restricted dome only.

      Add a precomputed list of restricted non-dome light indices on `LightList`. Keep
      the importer's debug line, saying which of the two applies.
- [ ] 3.2 Add the `CRUST_LINK_TWIN` switch: a `Config` field, a row in
      `docs/architecture.md` § Environment switches, and its section in
      `site/content/docs/reference/environment-variables.md`. When it is off, every
      restricted light is `nee_only` again, which is today's behaviour exactly.

## 4. The twin and its pair (D1, D3, D4, D6, D8)

- [ ] 4.1 Add `K_LINK_TWIN` to the keyed sub-domains in `tracer/path.rs`, one domain per
      restricted light index.
- [ ] 4.2 At a continuous surface vertex, after the bounce direction and its pdf
      (guide mixture included) are drawn, for each restricted non-dome light:
      1. run the support test;
      2. on a hit, cast `shadow_transmittance` along ω with the light's shadow mask up
         to the light point;
      3. add the contribution `throughput × emission × tr × weight`, with the weight per
         D3: `bounce_weight` under MIS, unopposed under `bsdf`, not run under `light`.
- [ ] 4.3 The same at a phase (volume) vertex, with the phase pdf.
- [ ] 4.4 Make the ordinary bounce collect nothing from a restricted non-dome light at a
      continuous vertex, under every strategy. That covers
      `bounce_emission_weight`, the hidden-emitter crossings, and `escaped_emission`
      with `escaped_split` (a pair: change both).
- [ ] 4.5 In the surface and phase NEE, replace the `nee_only → 1.0` weight with
      `strategy.light_weight(light_pdf, bounce_pdf)` for restricted non-dome lights.
      This lands in the same commit as 4.2–4.4.

## 5. Light path expressions (D7)

- [ ] 5.1 Route the twin's contribution as the sampled lobe's event followed by `L` with
      the light's tag, through the lobe split the bounce-hit emission uses. Compute the
      value once and route what was added.
- [ ] 5.2 Extend the LPE tests with the linked scene from 1.2:
      - `C.*[LO]` stays bit-identical to the beauty;
      - a lobe split (`C<RD>…`, `C<RG>…`, …) adds up to it to rounding;
      - `nee_only_and_bsdf_only_agree_per_expression` holds on a link that matters.

## 6. Tests

- [ ] 6.1 Replace `power_mis_matches_nee_only_for_a_shadow_linked_light` with the spec's
      "three strategies agree" test on the 1.2 scene (means within 1/√N noise).
- [ ] 6.2 Add the spec's "MIS is whole again" test: two seeds each, and the floor's
      variance under `power` is below that under `light`.
- [ ] 6.3 Add the spec's "a restricted dome stays NEE-only" test.
- [ ] 6.4 Run `scripts/check_images.sh check` against goldens recorded before the
      change. Every unlinked sample must report 0 differing pixels. `light_linking.usda`
      may differ only where its link matters.
- [ ] 6.5 Run the instruction-count gate: an unlinked sample's callgrind count is
      unchanged (the `links().is_none()` early-out).
- [ ] 6.6 Run `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings` and
      `cargo test --workspace`, plus the pinned-nightly clippy leg.

## 7. Measure on ALab

- [ ] 7.1 Repeat 1.1's 256 spp, two-seed, light-group measurement with the twin, at
      `--light-samples 4 --light-selection learned`. Expect `lgt_sun_distant`'s
      direct-glossy variance to fall by ≥ 90% (the blockers-off prediction is −97%), and
      the means to stay unchanged within noise.
- [ ] 7.2 Time it with `scripts/bench_ab.sh` against the pre-change binary (`-p Render`,
      n ≥ 3, ALab frame 1004). The budget is +5% Render.
- [ ] 7.3 If 7.2 exceeds the budget, profile it (`--profile`; callgrind on the 1.2
      scene) before changing anything.

## 8. Documentation

- [ ] 8.1 Lighting design record (`openspec/specs/lighting/design.md`): rewrite
      § Shadow linking's "NEE-only at continuous vertices" paragraph into the twin
      (D1–D8, with the 7.x measurement). Narrow § Known gaps to restricted domes and to
      delta vertices.
- [ ] 8.2 `site/content/docs/architecture/limitations.md`: narrow "A shadow-linked
      light is sampled only by light sampling" to dome lights. Update
      `site/content/docs/usd/lights.md` if it states the old behaviour, and build the
      site with Zola 0.21.
- [ ] 8.3 `docs/alab_profile.md`: add the "after" beside 1.1's "before".
- [ ] 8.4 Decide the open question (keep `CRUST_LINK_TWIN` or remove it). Record the
      decision in the design record.
