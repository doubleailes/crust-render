## Why

`learned` light selection (`crust:lightSelection = "learned"`) is visibility-aware.
A light tree is not: it ranks lights by power, distance and orientation and
cannot see a wall. `learned` already removes most of crust's direct-light noise
where lights are occluded. On ALab it gives 4.07× lower relMSE on direct lighting
for +14–23% render time. On veach_mis it gives 2.0× (trimmed) and 1.7× (full)
lower relMSE for +4%. It stays opt-in for one reason: its fireflies. On the
OpenPBR Shader Playground at 64 spp it lowers the trimmed relMSE 1.5× but
raises the full relMSE 1.8×. On ALab the full image gains only 1.01×. Fix the
fireflies and `learned` can become the default selection.

## What Changes

- **Blended tables.** The selection probability at a point SHALL vary
  continuously across cell boundaries: the per-cell tables are blended over the
  neighbouring cells instead of switching at each cell edge. A cell edge that
  straddles a shadow boundary is the suspected main firefly source: a light
  that is nearly never picked on one side but visible there is picked rarely
  and weighted hugely.
- **A visibility-aware floor.** A light that delivered light to any training
  receiver in a cell's neighbourhood SHALL keep a selection probability that
  does not shrink with how rarely it was seen. Lights never seen nearby keep the
  uniform defensive share, so ALab's hidden exterior lights stay out of the
  picks.
- **`learned` becomes the default**, gated on the acceptance criteria in
  design.md (equal-time relMSE, 4 seeds, full and trimmed, no scene worse than
  `power`). **BREAKING** for noise patterns: every render's noise changes,
  though its expectation does not. `--light-selection power` and
  `crust:lightSelection = "power"` keep today's images bit for bit.
- The `lighting` design record, the CLI help and the user documentation state
  the new default and the gate it passed.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `lighting`: "Light selection" changes its default to `learned`, and a new
  requirement bounds the learned table's firefly risk.

`multiple-light-samples-per-vertex` also modifies "Light selection". Whichever
change archives second must restate that requirement with both applied.

## Impact

- `crates/crust-core/src/light_cache.rs`: blended lookup and the visibility
  floor. The pre-pass is unchanged.
- `crates/crust-core/src/light/list.rs` (`pick_index_at`, `pmf_at`,
  `find_index_by_geom_at`, `infinite_at`): both MIS sides read the blended
  probability at the vertex NEE sampled from, as today.
- The defaults in `tracer/settings.rs`, `scene/usd_import/settings.rs` and
  the CLI's `--light-selection` help.
- Goldens: every checked-in sample is re-recorded. Each moves by noise alone,
  which the tasks verify.
- Performance: a blended lookup reads 8 tables instead of 1 per NEE pick and
  per bounce-side weight. The pre-pass is unchanged. The equal-time gate
  includes this cost.
