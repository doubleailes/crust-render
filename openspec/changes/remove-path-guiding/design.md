## Context

See `proposal.md` for why. What shapes the approach:

- **Guiding is two layers.** `guiding/` is self-contained (SD-tree, quadtree, field).
  The integrator side is not: `trace_path` threads a `GuidingContext`, the scatter step
  mixes `α·p_guide + (1-α)·p_bsdf`, `bounce_emission_weight` carries the twin of that
  mixture for NEE, and path records carry training samples. Deleting the first layer
  without the second does not compile, and deleting the second wrongly changes
  unguided renders.
- **`render_guided` is the only multi-pass render.** `render_unguided` is one
  `render_pass`. `blend_passes`, `blend_weights`, `blend_luminance`, `AovFilm::blend`
  and the efficiency estimate are called from `render_guided` and nowhere else.
- **Several contracts were written for passes.** `samples_reached` versus
  `max_samples_reached`, the interrupted-pass rule, a guided render's published
  snapshots, and the cancellation blend exist because a render could have more than one
  pass.
- **Unguided renders must not move.** `K_GUIDE` is a keyed QMC sub-domain, so removing
  its draws cannot shift another stream, and the guide branch is not taken when
  guiding is off. The golden EXRs at 16 spp pin this.
- **`crust diagnostic` reads old reports.** `--baseline` compares a new run with a
  `crust-diagnostic/1` file that may have run the `guiding` trial.

## Goals / Non-Goals

**Goals:**

- One render is one pass, with one code path through `Renderer::render`.
- Every unguided render stays bit-identical, so a diff against goldens proves the
  removal changed nothing it should not.
- A stage that authored guiding is told once that it is no longer honoured.

**Non-Goals:**

- Replacing guiding with another sampler, or leaving a seam for one. A future guide
  is a new design; keeping a `GuidingContext`-shaped hole for it would keep the cost.
- Touching the subsurface Dwivedi sampler or learned light selection.
- Renaming or restructuring what stays (the tile/scanline strategies, adaptive
  sampling).

## Decisions

**1. Delete, don't gate.** The cost is the integrator hooks, which a cargo feature
would not remove. Git history keeps the implementation, and the rendering design
record keeps what was learned (the efficiency-gate idea, the #244 finding).
*Alternative: keep `guiding/` behind a feature.* Rejected: the hooks stay compiled in
and tested, which is the complexity being removed.

**2. A coded warning for the three settings, then forget them.** The importer checks
whether any of `crust:pathGuiding`, `crust:guidingTrainIterations` and
`crust:guidingProb` is authored and raises `settings.path_guiding_removed` once
(kind `Refused`, policy `Once`), whatever their values, including `false`. It stores
nothing. Silent ignoring would change renders without telling anyone; a hard error
would break stages that never needed guiding.
*Alternative: keep parsing for a release.* Rejected: the fields would stay in
`RenderSettings` with no behaviour.

**3. Collapse the pass counters only where guiding was the reason.**
`max_samples_reached` and the interrupted-pass logic go when nothing needs them. Each
removal is its own task with its own test, because a progressive render's counters
are read by the CLI and the MCP session. If a counter turns out to serve adaptive
rounds as well, it stays and only its guiding comment goes.

**4. `--baseline` tolerates a `guiding` trial.** An unknown factor name in a baseline
report is skipped with the trial it belongs to, not an error. No schema bump:
removing a possible value narrows the report, it does not change its fields.

**5. Prove it with goldens.** Record `scripts/check_images.sh record` goldens at 16 spp
and `--indirect-clamp 0` on the unguided samples before touching code, `check` after
each group. The one scene that disappears, `cornellbox_guided`, has no golden to
check.

## Risks / Trade-offs

- **The mixture sits inside `scatter`.** Removing it could change the unguided path
  through an extra `max(1e-4)` or a reordered draw. → The `check_images.sh` goldens
  and the existing bitwise pins (JIT, `Tri4`, tiles versus scanlines) catch any
  difference; a failure means the removal was wrong, not that the golden is stale.
- **A counter shared with adaptive sampling is deleted by mistake.** → Decision 3:
  the adaptive and cancellation tests must pass unchanged after that group.
- **Users with guided stages get slower or noisier renders.** → Accepted in the
  proposal; the warning names the cause.
- **A baseline report in the wild names `guiding`.** → Decision 4, with a test on a
  report that has one.

## Migration Plan

1. Record goldens, then remove from the outside in: the settings and diagnostic
   first (so nothing selects guiding), then `render_guided` and the blend, then the
   integrator hooks, then `guiding/`.
2. Each group lands with the goldens green and its tests and docs updated.
3. Rollback is `git revert`: nothing is migrated on disk.
