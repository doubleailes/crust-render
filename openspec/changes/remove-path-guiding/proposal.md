## Why

Path guiding is too complex for what this project is, and its benefit is too small to
pay for that. It is about 870 lines of SD-tree plus a multi-pass render mode that
reaches into the integrator, the cancellation and snapshot contracts, the statistics,
the diagnostic and the MCP session. It covers surfaces only, trains on luminance only,
and mixes with a fixed probability. Learned light selection, now the default, takes the
direct-light share of what guiding helped with. A guided ALab render still reads 6.9%
darker than an unguided one, undiagnosed ([#244](https://github.com/doubleailes/crust-render/issues/244)).

## What Changes

- **BREAKING**: remove path guiding: the SD-tree (`guiding/`), the guide/BSDF mixture
  at secondary bounces, the training passes, the efficiency gate and the inverse-variance
  blend of passes. A render is always one pass.
- **BREAKING**: the `crust:pathGuiding`, `crust:guidingTrainIterations` and
  `crust:guidingProb` render settings are no longer read. A stage that authors any of
  them imports with one coded warning, `settings.path_guiding_removed`, and renders
  unguided.
- Remove the guiding trial from `crust diagnostic`: the `guiding` factor, the
  `guiding_without_indirect` finding, the training-cost pricing, and the guiding rows
  of the noise breakdown's ordering. A `--baseline` report that names a `guiding`
  trial is read without it.
- Retire the contracts that exist only for multi-pass renders: a guided render's
  published passes, its cancelled-render blend, and its tiles-versus-scanlines and
  crop exceptions.
- Remove `samples/cornellbox_guided.usda`, the guiding tests, the user documentation
  of the three settings, and the guiding sections of the rendering design record,
  replaced by a short "removed" note that keeps what was learned.
- An unguided render is **bit-identical** to today's, for every scene and every
  setting.

Out of scope, and kept: the Dwivedi phase sampling of the subsurface random walk, and
learned light selection.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `rendering`: the opt-in path guiding requirement and the contracts that only a
  guided render needed are removed; the render-region, strategy-equivalence,
  snapshot and cancellation requirements lose their guided clauses.
- `usd-scene-import`: the three guiding settings leave the render settings, with a
  coded warning when a stage authors one.
- `diagnostics`: the tier-1 trials no longer include path guiding.

## Impact

- `crust-core`: `guiding/` deleted; `tracer/path.rs` (mixture, `GuidingContext`,
  training samples), `tracer/mod.rs` (`render_guided`, blending, the pass counters),
  `tracer/settings.rs`, `usd_import/settings.rs`, `warnings.rs` (one new code),
  `aov.rs` (`AovFilm::blend`), `diagnostic/*`, `stats.rs`, `profile.rs`, `lib.rs`.
- `crust-render`: `main.rs` and `mcp/render.rs` lose their guided-render messages.
- Tests: `guiding.rs` and `guiding_field.rs` deleted; guided cases in `lpe.rs`,
  `aovs.rs`, `usd_scene.rs`, `render_smoke.rs`, `diagnostic.rs` and the integrator
  bench removed.
- Docs: the user documentation of the render settings, quick start, limitations,
  design choices and diagnosing-a-render pages; `docs/architecture.md`; `CLAUDE.md`
  (the guide mixture ↔ NEE pair); the warnings reference.
- No dependency changes. Existing stages that author the settings keep rendering, now
  unguided and with a warning.
