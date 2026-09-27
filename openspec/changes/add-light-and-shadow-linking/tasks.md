## 1. Collection membership (`usd_import/`)

- [ ] 1.1 Check whether `openusd-schemas` 0.7 exposes `UsdCollectionAPI`
      membership publicly (it resolves collection bindings for
      `compute_bound_material`). Use it if it does; otherwise add
      `usd_import/collections.rs`.
- [ ] 1.2 Resolve `includes` / `excludes` (nearest path wins), `includeRoot`,
      `expansionRule` and included collections with cycle refusal. Refuse
      `membershipExpression` with a `WARN`, reading it as the default.
- [ ] 1.3 Unit tests: nearest-path, `includeRoot` true and false, `explicitOnly`,
      a nested collection, a cycle, and the default collection building nothing.
- [ ] 1.4 Resolve light collections before geometry is emitted (a light-first
      pass), and assign each emitted `geom_id` a light-link class and a
      shadow-link class through a per-`geom_id` table in `rt_world.rs`.
      Deduplicate classes. Warn once per collection that targets a path inside an
      instance prototype.

## 2. Light linking (`light/`, `tracer/`)

- [ ] 2.1 Give each light entry an `illuminates(class)` bitset. The default is all.
- [ ] 2.2 Surface NEE and `volume_nee`: skip a picked light that does not
      illuminate the receiver's class, and keep the pick pmf unchanged.
- [ ] 2.3 Bounce side: zero the emission of an area light hit from a previous
      vertex it does not illuminate (`bounce_emission_weight` site), and zero
      unlinked infinite lights in `escaped_emission`. Check both pair sides in
      one commit.
- [ ] 2.4 `light_cache::train`: apply the same filter to its candidates.
- [ ] 2.5 A test that the NEE-only and BSDF-only estimates of a linked scene
      agree, and that an unlinked receiver gets exactly zero from the light.

## 3. Shadow linking (`tracer/`, `usd_import/attrs.rs`)

- [ ] 3.1 Encode shadow classes in ray-mask bits 3–31 (class 0 = `MASK_SHADOW`).
      Reserve those bits in `crust:rayMask` (mask them off and `WARN`), and make
      sure instance prototypes keep every class bit.
- [ ] 3.2 Build each light's shadow-ray mask, and pass it through the surface NEE,
      `volume_nee` and `shadow_transmittance` steps and the `light_cache` rays.
- [ ] 3.3 Refuse, with a `WARN`, lights whose sets need more than 29 classes;
      their shadows fall back to every occluder.
- [ ] 3.4 For restricted-shadow lights, make NEE weight 1 and bounce emission 0 at
      non-delta vertices, and keep full bounce weight at delta vertices.
- [ ] 3.5 Tests: an excluded occluder casts no NEE shadow, an excluded volume does
      not attenuate, and NEE and BSDF estimates agree for a restricted light.

## 4. Sample, verification and documentation

- [ ] 4.1 Add `samples/light_linking.usda` (derived from
      `samples/light_visibility.usda`) exercising `lightLink` include and exclude
      and a `shadowLink` exclude, plus an integration test that loads it.
- [ ] 4.2 `scripts/check_images.sh check` shows every existing sample
      bit-identical, and `scripts/bench_ab.sh` shows no throughput change on
      `usdlux.usda`.
- [ ] 4.3 Measure the NEE-only cost of D3 on the sample (`relmse:` against a
      1024 spp reference, `--indirect-clamp 0`) and record it in
      `openspec/specs/lighting/design.md`.
- [ ] 4.4 Retire the linking gap in `openspec/specs/lighting/design.md`,
      `openspec/specs/usd-scene-import/design.md` and `README.md`, recording the
      remaining gaps instead (instance prototypes, `membershipExpression`,
      mirrors).
- [ ] 4.5 Follow-ups, scoped separately: per-class pick renormalisation (a
      `density` / `pmf` pair change), a Cycles-style extra ray to restore MIS for
      shadow-linked lights, and a per-ray occluder filter in `crust-rt` if 29
      classes proves too few.
