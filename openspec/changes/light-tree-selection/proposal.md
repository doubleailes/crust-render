## Why

Light sampling is the main source of crust's noise at 16 spp, and the default
`power` selection picks lights by flux alone. It cannot tell a near light from a
far one, a panel facing the receiver from one facing away, or a light below the
receiver's horizon from one above it. On ALab, 29.5% of NEE picks went to a light
facing away and 15.1% to one below the receiver's horizon
(`docs/light_sampling.md` §3.12). A light BVH ranks lights by an importance
bound built from flux, distance, the light's emission cone and the receiver's
normal. It is the default selection in pbrt-v4 and Cycles. Crust has none
(`docs/light_sampling.md` §6.3 and §9.3 m).

`learned-light-selection-by-default` is on hold behind this change. The tree
goes first, so that the learned table can later be built over it rather than
over `power`.

## What Changes

- **A new selection mode, `tree`.** `--light-selection tree` and
  `crust:lightSelection = "tree"`. It picks among the finite lights by
  walking a BVH over them. At each node the walk chooses a child in proportion
  to a conservative importance bound at the shading point. The bound uses the
  shading point's position and normal, and zero importance is given only where
  no light in the node can deliver light.
- **A defensive share.** Every finite light that emits SHALL keep at least a
  fixed fraction of its `power` probability at every vertex, whatever the tree
  says. The fraction is set by measurement.
- **Lights at infinity** (dome, distant) stay outside the tree with the same
  share `power` gives them.
- **The light-selection probability may depend on the receiver's normal.**
  Today it depends on position at most. The bounce side of MIS SHALL evaluate
  it at the same position and normal NEE used. Volume vertices have no
  normal, and there the bound drops its normal terms.
- **A many-lights sample.** `samples/many_lights.usda` has hundreds of small
  lights, varied in distance, orientation and power. It is the scene where a
  light tree is expected to matter, and no checked-in sample is one today.
- **The default becomes `tree`, if it passes a gate.** The gate is
  equal-time relMSE against `power`, the same gate as
  `learned-light-selection-by-default`'s D3 plus the new sample. If it
  passes, a render with no `crust:lightSelection` uses `tree`. **BREAKING**
  for noise patterns: every render with two or more finite lights gets a
  different noise pattern, though the same expectation.
  `--light-selection power` keeps today's images bit for bit. If any scene
  fails the gate, `tree` ships opt-in and the design record names the
  failing scenes.
- `crust diagnostic` tries `tree` as one more light-selection trial. The
  `many_lights_uniform` finding recommends the default rather than `power`.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `lighting`: "Light selection" gains `tree` and, if the gate passes, its new
  default. New requirements state that tree selection is unbiased, that it
  weights both MIS sides alike (at position *and* normal), and that it is
  deterministic.

`learned-light-selection-by-default` also modifies "Light selection". It is on
hold, and will be restated over this change's version when it resumes.

## Impact

- `crates/crust-core/src/light/`: a new `tree.rs` (build, traversal,
  importance, bit trails), a conservative per-light bounds method on the
  `Light` trait with an `AreaLight` implementation, `LightSelection::Tree`,
  and the `*_at` lookups in `list.rs` taking the receiver's normal.
- `crates/crust-core/src/tracer/path.rs`: `PrevBounce` carries the normal NEE
  used. Every NEE pick site and the bounce-side weights (`bounce_emission_weight`,
  `escaped_emission`, link twins) pass the normal. Debug assertions check that
  both sides read the same position *and* normal bits. The
  `wavefront-integrator` change also carries `PrevVertex` between iterations,
  and sees the new field.
- `light_cache.rs` and `light_visibility.rs` are unchanged: under `learned`
  and `neural` the normal is ignored, and their fallback stays the power table.
- Settings, CLI and docs: `tracer/settings.rs`, `scene/usd_import/settings.rs`,
  `LightSelection::CHOICES`, the `--light-selection` help, the site's
  `reference/command-line.md` and `usd/render-settings.md`, the `lighting`
  design record, and `docs/light_sampling.md` (§3, §9.3 m).
- `crust-core/src/diagnostic/checks.rs`: the trial list and
  `many_lights_uniform`.
- Performance: every NEE pick and every bounce-side weight walks the tree,
  about two importance evaluations per level. With at most one finite light
  there is nothing to choose and the walk is skipped. The gate is measured at
  equal time, so the cost is part of the verdict.
