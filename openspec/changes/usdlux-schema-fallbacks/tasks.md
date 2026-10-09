## 1. The distant light's intensity fallback

- [x] 1.1 In `crates/crust-core/src/scene/usd_import/lights.rs`, add the `LightSchema`
      trait (design D1): `const INTENSITY: f32 = 1.0`, with `DistantLight` overriding it to
      50000. Implement it for the six imported light types. `light_inputs` and `lux_params`
      take `&impl LightSchema` and fall back to `INTENSITY`. Verify: `cargo build -p
      crust-core`.
- [x] 1.2 Move `listing.rs`'s `read` to the new bound. Verify: `cargo test -p crust-core
      --test usd_listing`, with a `DistantLight` added to
      `light_records_are_the_authored_inputs_at_the_time_code`'s stage, listed with
      intensity 50000 beside the dome's "schema fallbacks".
- [x] 1.3 Add `an_unauthored_distant_light_is_the_schema_sun` to
      `crates/crust-core/tests/usd_inline.rs`. Unauthored, blocked (`= None`) and `inf`
      intensities each import bitwise as `inputs:intensity = 50000`, and that is 50000
      times the light authored at 1. Verify: the test fails on the old fallback and passes
      on the new.

## 2. Every other fallback, pinned

- [x] 2.1 Add `unauthored_light_inputs_are_the_schema_fallbacks` to `usd_inline.rs`
      (design D4). For sphere, disk, cylinder, rect, distant and dome, for a shaped
      sphere with `ShapingAPI` applied, and for a sphere with its colour temperature
      enabled, a prim authoring nothing and one authoring every read input at its schema
      fallback sample bitwise alike. Verify: `cargo test -p crust-core --test usd_inline
      unauthored_light_inputs`. A deliberately wrong fallback (for example, radius 0.4)
      makes it fail.

## 3. Documentation

- [x] 3.1 `openspec/specs/lighting/design.md`: under "UsdLux import", say where fallbacks
      come from, the distant light's 50000 and what an unauthored sun delivers. Under
      "Known gaps: lighting", add `DomeLight_1`'s unread `poleAxis`. Verify: the record
      names every deliberate divergence in the design's audit.
- [x] 3.2 `site/content/docs/usd/lights.md`: each input's fallback (the distant light's
      50000 called out), the cone's 180° without `ShapingAPI`, and `poleAxis` under "Not
      supported". Verify: `zola build` in `site/` (Zola 0.21) passes, or, when Zola is not
      available, the page's links are checked by hand against their targets.

## 4. Integration

- [x] 4.1 Golden images: `scripts/check_images.sh record` with the binary before the
      change, then `check` with the binary after it. Verify: every sample is bit-identical,
      since no sample leaves a distant light's intensity unauthored. *Result:* 36 of the 37
      `samples/*.usda` were identical. The 37th, `cornellbox_guided`, differed by relMSE
      1.7e-3 only because its golden was recorded on a loaded machine: a guided render is
      not bit-identical across schedules (`rendering` design record). Rendered on a quiet
      machine, the binaries before and after the change agree on it at relMSE 0. The
      Kitchen_set pair is gitignored and was not in the checkout.
- [x] 4.2 The CI set: `cargo fmt --all -- --check`, `cargo clippy --workspace
      --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`. Verify: all
      three pass (1801 tests).

## Workflow follow-up

- Sync the delta into `openspec/specs/lighting/spec.md` and archive the change once it is
  reviewed and merged.
