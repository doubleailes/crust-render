## 1. Tests first (`crates/crust-core/tests/usd_inline.rs`)

- [x] 1.1 Add a test `AssetLoader` whose `load_environment` returns a generated lat-long
      map: four quarter-width column bands (red, green, blue, white) with a distinct top
      row, and a helper that reads the imported dome's escaping radiance along a world
      direction (the path the four-band orientation scenario already uses). Let the
      inline stage helper take the root layer's `upAxis` (it hard-codes `Y` today).
      Verify: a `DomeLight` on a Y-up stage gives green, blue, red, white along
      (+1, 0, +1), (−1, 0, +1), (+1, 0, −1), (−1, 0, −1).
- [x] 1.2 Write one test per new scenario in the `lighting` delta spec: a `DomeLight_1`
      is a light; a Z-up `DomeLight_1` with `poleAxis` unauthored turns its pole onto
      +Z; `poleAxis = "Z"` turns it on a Y-up stage; `poleAxis = "Y"`, or unauthored on
      a Y-up stage or one with no `upAxis`, matches a `DomeLight` bitwise; a `DomeLight`
      authoring `poleAxis = "scene"` on a Z-up stage keeps +Y; a prim rotation of 90°
      about Z composes after the pole. Verify: they fail on `main` (no light is
      imported) and compile.

## 2. Import (`crates/crust-core/src/scene/usd_import/`)

- [x] 2.1 Change `emit_dome_light` (`lights.rs`) to take the texture-file and
      texture-format attributes and a pole rotation instead of a `&DomeLight`. Fold the
      pole into the stored rotation as `R_world · R_pole` (design: "The pole is a matrix
      folded into the dome's rotation"). Have the warnings name the prim's actual
      schema. Verify: every existing dome test passes unchanged.
- [x] 2.2 Add the `DomeLight_1` arm to the traversal (`mod.rs`), after `DomeLight`.
      Resolve `R_pole` from `pole_axis()` and `Stage::up_axis()` (read per `DomeLight_1`): +90° about X for `Z`, or for `scene` with
      `upAxis = Z`, otherwise the identity. Pass the identity from the `DomeLight` arm.
      Verify: the tests from 1.2 pass.
- [x] 2.3 Recognise `DomeLight_1` in `listing.rs`: the `ls light` record (kind `dome`)
      and the kind test. Add a case to `crates/crust-core/tests/usd_listing.rs`.
      Verify: `crust ls light` lists a `DomeLight_1` and `crust check` counts it, on the
      scratch stage from the exploration (`def DomeLight_1 "Sky"` on a Z-up stage).
- [x] 2.4 Confirm light and shadow linking, camera visibility, backdrops and LPE tags
      reach a `DomeLight_1` (they hang off the prim, not the schema): one inline test
      with a `DomeLight_1` backdrop linked to nothing. Verify:
      `scene.lights.backdrops().len() == 1`.

## 3. No image changes

- [x] 3.1 Record goldens on `main` and check this branch
      (`scripts/check_images.sh record` / `check`, `-s 16`, `--indirect-clamp 0` on
      both sides). Verify: every sample is bit-identical (none authors `DomeLight_1`).
- [x] 3.2 Manual sanity render: the JungleRuins wrapper from the exploration, with its
      `def DomeLight` swapped for `def DomeLight_1` in a scratch overlay. Compare it with
      the hand-rotated overlay (`+90°` about X). Verify: `crust diff` reports the two as
      identical up to sampling noise, and the stock `DomeLight` stage is unchanged.

## 4. Documentation

- [x] 4.1 `site/content/docs/usd/lights.md`: add `DomeLight_1` to the light table, and a
      paragraph on `poleAxis` (the three values, `scene` read from the root layer's
      `upAxis`, a `DomeLight` keeps +Y even when it authors `poleAxis`, author
      `poleAxis = "Y"` to keep a Z-up `DomeLight_1` as a Y-pole sky). Verify:
      `zola build` in `site/` passes.
- [x] 4.2 `openspec/specs/lighting/design.md`, the `UsdLuxDomeLight` bullet: `DomeLight_1`
      import, the pole rotation and its source (`_GetDomeOffset`), the strict
      `DomeLight` rule with JungleRuins as the example, and the Typhoon divergence with
      its evidence (`delegate/light.cpp` reads `SampleTransform` only). Add a Known gaps
      line if anything is left out (for example `poleAxis` on non-lat-long formats,
      which still fall back to the uniform colour).
- [x] 4.3 Run the CI set: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`.
