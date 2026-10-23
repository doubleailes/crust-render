# Tasks

## 1. Refuse a depth below one (usd-scene-import)

- [x] 1.1 In `import_render_settings`, refuse `crust:maxDepth < 1` with a `SettingsInvalidValue` warning naming the attribute, keeping the default. Verify with `a_max_depth_below_one_warns_and_keeps_the_default` in `crust-core/tests/usd_inline.rs` (0 and -2 warn once and give 32; 1 is read without a warning).

## 2. Documentation

- [x] 2.1 `site/content/docs/usd/render-settings.md` § `crust:maxDepth`: vertices, not bounces; what 1 renders; 0 refused.
- [x] 2.2 `RenderSettings::with_max_depth`'s doc comment states the unit.
