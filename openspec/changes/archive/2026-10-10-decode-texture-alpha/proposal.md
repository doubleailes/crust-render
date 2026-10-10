## Why

A `UsdPreviewSurface` alpha cutout renders fully opaque (issue #267). When
`opacity` connects to a `UsdUVTexture`'s `a` output under `opacityThreshold > 0`,
the import already builds a per-hit cutout from the texture
(`PreviewSurface::with_cutout`). But both UV texture samplers return an opaque
alpha: the preload decodes through `image`'s `to_rgb8`, the `.tx` readers keep
three channels, and the shared bilinear and level blends (`Taps::blend_rgb`,
`lerp_rgba`) write `a = 1`. So `a` reads 1.0 at every texel, every texel passes
the threshold, and leaves, grass and other card foliage render as solid
rectangles. The import says so (`preview.texture_alpha`, "reads 1.0"), and the
warnings reference lists it as an approximation.

Foliage is one of the commonest uses of `UsdPreviewSurface`. On JungleRuins
(Houdini/Karma export) every foliage material reads `opacity` from its base
colour TIFF's alpha under `opacityThreshold = 0.5`; the import raises 298
`preview.texture_alpha` warnings and the whole canopy renders as opaque cards.
C++ USD 26.08 cuts the issue's two-colour test card at its alpha; crust renders
both halves the same red.

## What Changes

- **UV textures decode alpha.** A tile whose file carries an authored alpha
  that is not opaque everywhere is held RGBA; a lookup returns that alpha as
  its fourth component, at every mip level. Sources: `image`'s alpha (PNG,
  TIFF, TGA, …), an EXR's `A` channel (matched by base name, as R/G/B are), a
  `.tx` TIFF's first extra sample when `ExtraSamples` declares it alpha, a
  `.tx` EXR's `A`.
- **Every other texture is unchanged.** No alpha, or an alpha opaque
  everywhere, is stored RGB exactly as before and reads 1.0, as `UsdUVTexture`
  specifies. A TIFF extra sample left unspecified (`ExtraSamples = 0`, which
  `image_file` declares alpha only to decode it) is not alpha.
- **Alpha is coverage, never colour-managed.** `a / 255` from a byte, the value
  from a float; the colour space's curve and primaries apply to RGB alone. Mip
  levels average it as coverage, independently of the colour (which stays
  unpremultiplied, so its bits are unchanged).
- **Both backings, one answer.** `make_tx` — what `--auto-tx` and the `maketx`
  example run — keeps the alpha: a fourth TIFF sample declared unassociated
  alpha, or an EXR `A` channel. A streamed `.tx` converted from an RGBA source
  is bit-identical to the preloaded source, alpha included, extending the
  streamed ↔ preloaded `u8` bit-identity pair.
- **`UsdUVTexture.outputs:a` reads it**, so the existing cutout path gets a
  real mask. A MaterialX `image` of type `color4` / `vector4` reads it as its
  fourth channel, which MaterialX specifies and which was 1.0.
- **The `preview.texture_alpha` warning is retired**, since nothing is
  approximated any more. **BREAKING (report format):** removing a code bumps
  every report that carries warnings (the `scene-warnings` versioning rule), so
  `crust check`'s JSON becomes `crust-check/2`. Its shape does not change.
- The warning's message also had a run of spaces where a string continuation
  missed its `\` (the issue's aside); it goes with the warning.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `textures`: a new requirement, "Texture alpha"; "Streaming is an A/B of
  preloading" extends its bit-identity to alpha; "Known gaps" drops texture
  alpha.
- `usd-scene-import`: "Imported opacity inputs are cutouts" gains a scenario
  for a cutout read from a texture's alpha.
- `scene-check`: "JSON report" writes `crust-check/2`.

## Impact

- `crates/crust-assets/src/`: `image_file.rs` (`decode_with_alpha`),
  `lib.rs` (`ALPHA_U8`, `ALPHA_STEPS`, `drop_opaque_alpha`,
  `decode_rgb_of_rgba`), `environment.rs` (`try_read_exr_texels`),
  `mip_filter.rs` (`blend_rgba`, `lerp_rgba_alpha`), `uv_texture/` (tile alpha,
  `reduce_half` / `reduce_half_linear` over RGBA, the sampler monomorphised over
  alpha), `tiled/` (`TileData::alpha`, `Tile::rgba_u8` / `rgba_half`,
  `write_tx_rgba`, `write_tx_exr_rgba`, the readers' alpha, `make_tx`,
  `MadeTx::alpha`).
- `crates/crust-core/`: `preview.rs` and `warnings.rs` lose
  `PreviewTextureAlpha`; `check.rs`'s `FORMAT` is `crust-check/2`.
- `crates/crust-render/`: the `maketx` example reports a kept alpha; the
  `crust check` and MCP docs and tests name `crust-check/2`.
- Images: none of the checked-in samples binds a texture with alpha, so no
  golden changes. Performance: the RGB sampler is unchanged; whether a lookup
  reads alpha is decided in the per-lookup storage match that already exists
  (measured in `design.md`).
