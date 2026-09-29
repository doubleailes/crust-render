## 1. Collection membership (`usd_import/`)

- [x] 1.1 Check whether openusd 0.7 exposes `UsdCollectionAPI` membership
      publicly. It does: `openusd::usd::Collection::compute_membership_query` →
      `MembershipQuery::is_path_included`. Use it; no `collections.rs`.
- [x] 1.2 Read each light's `lightLink` / `shadowLink` through that query,
      computed when the light is met. Refuse an expression-mode collection
      (`membershipExpression`) with a `WARN`, reading it as the default, and
      build nothing for a default collection.
- [x] 1.3 Tests: nearest-path, `includeRoot` true and false, `explicitOnly`, a
      nested collection, a cycle, and the default collection building nothing.
- [x] 1.4 Record, per prim that emits geometry, its first `geom_id` and its
      interned stage path. After traversal, evaluate every distinct path against
      every linked light, deduplicate into classes, and fill a per-`geom_id`
      light-class and shadow-class table in `rt_world.rs`. Fold
      `light_links.rs`'s "covers no receiver" test into "illuminated-class set is
      empty". Warn once per collection that targets a path inside an instance
      prototype.

## 2. Light linking (`light/`, `tracer/`)

- [x] 2.1 Give each light entry an `illuminates(class)` bitset. The default is all.
- [x] 2.2 Surface NEE and `volume_nee`: skip a picked light that does not
      illuminate the receiver's class, and keep the pick pmf unchanged.
- [x] 2.3 Bounce side: zero the emission of an area light hit from a previous
      vertex it does not illuminate (`bounce_emission_weight` site), and zero
      unlinked infinite lights in `escaped_emission`. Check both pair sides in
      one commit.
- [x] 2.4 `light_cache::train`: apply the same filter to its candidates.
- [x] 2.5 A test that the NEE-only and BSDF-only estimates of a linked scene
      agree, and that an unlinked receiver gets exactly zero from the light.

## 3. Shadow linking (`tracer/`, `usd_import/attrs.rs`)

- [x] 3.1 Only when some light authors a restricted `shadowLink`, encode shadow
      classes in the geometry mask. Class 0 keeps `MASK_SHADOW`; every other
      shadow caster clears it and carries exactly one bit: 3–30 for the 28
      allocated classes, 31 for overflow. Rewrite an authored `crust:rayMask`'s
      bits 3–31 then, with one `WARN` per scene. Leave every mask untouched when no
      shadow link exists. Make sure instance prototypes keep every class bit.
- [x] 3.2 Build each light's shadow-ray mask, and pass it through the surface NEE,
      `volume_nee` and `shadow_transmittance` steps and the `light_cache` rays.
- [x] 3.3 Refuse, with a `WARN`, restricted lights that include some overflow
      classes but not others; they fall back to the unrestricted mask
      (`MASK_SHADOW | bits 3–31`), which every shadow caster matches.
- [x] 3.4 For restricted-shadow lights, make NEE weight 1 and bounce emission 0 at
      non-delta vertices, and keep full bounce weight at delta vertices.
- [x] 3.5 Tests (`crates/crust-core/tests/light_linking.rs`): an excluded
      occluder and an excluded volume cast no NEE shadow; power MIS and NEE-only
      agree for a restricted light (both are NEE-only on it; BSDF-only renders
      the physical shadows, since only NEE rays carry the link); an
      overflow-class geometry blocks both an unrestricted ray and a refused
      light's fallback ray; and authored `crust:rayMask` bits 3–31 without links
      keep their masks.

## 4. Sample, verification and documentation

- [x] 4.1 Add `samples/light_linking.usda` (derived from
      `samples/light_visibility.usda`) exercising `lightLink` include and exclude
      and a `shadowLink` exclude, plus an integration test that loads it.
- [x] 4.2 `scripts/check_images.sh check` shows every existing sample
      bit-identical (25/25), and `scripts/bench_ab.sh` against the pre-linking
      binary shows +1.1% on `usdlux.usda` (+1.3% instructions under callgrind),
      after restoring `trace_path`'s inlining into `render_pixel` (it was +2.9%
      without). Recorded in `openspec/specs/lighting/design.md`.
- [x] 4.3 Measure the NEE-only cost of D3 on the sample (`relmse:` against a
      1024 spp reference, `--indirect-clamp 0`) and record it in
      `openspec/specs/lighting/design.md`.
- [x] 4.4 Retire the linking gap in `openspec/specs/lighting/design.md`,
      `openspec/specs/usd-scene-import/design.md` and `README.md`, recording the
      remaining gaps instead (instance prototypes, `membershipExpression`,
      mirrors).
- [x] 4.5 Follow-ups, scoped separately: per-class pick renormalisation (a
      `density` / `pmf` pair change), a Cycles-style extra ray to restore MIS for
      shadow-linked lights, and a per-ray occluder filter in `crust-rt` if the
      overflow bit refuses lights in a real scene.
