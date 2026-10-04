## 1. Colour core

- [x] 1.1 Interned `Space`, configurable OCIO config (`use_config`), the five
      well-known spaces checked at load.
- [x] 1.2 `Conversion`: curve processor + exact `f64` matrix from the optimised
      processor; refuse other shapes once, with a warning.
- [x] 1.3 `ColorSpace` / `ResolvedColorSpace` carry the working space;
      `from_mtlx` / `from_usd` take it.
- [x] 1.4 Tests: the split equals the whole processor for every ACES texture
      space, both directions; aliases intern once; working spaces must be
      scene-linear.

## 2. Textures

- [x] 2.1 Byte UV textures (preloaded, streamed): curve table + matrix per
      lookup; float textures converted whole at load.
- [x] 2.2 Ptex: per texel at load (preloaded) or per decode (streamed).
- [x] 2.3 Environment and `RectLight` images take a `ColorSpace`.
- [x] 2.4 Tests: sRGB into ACEScg, `auto`, float, out-of-gamut clamp, streamed ==
      preloaded `.tx` and Ptex in ACEScg.

## 3. Import

- [x] 3.1 Working space from `renderingColorSpace` / `UsdImportOptions`.
- [x] 3.2 `colorSpace` metadata on colour attributes and texture files.
- [x] 3.3 MaterialX: inheritance and literal conversion through `Host`.
- [x] 3.4 Tests: selection, metadata, blackbody, preview surface.

## 4. Output and CLI

- [x] 4.1 `--ocio-config`, `--working-space`, `--display`, `--view`.
- [x] 4.2 EXR `colorInteropID` / `chromaticities`; preview through display/view.
- [x] 4.3 Default renders unchanged: EXR header and pixels, PNG bytes, on all
      33 samples at 16 spp against the previous commit.

## 5. Primaries (after the Cycles / Typhoon review)

- [x] 5.1 `color::to_xyz` from the config's scene-referred XYZ space, with the
      `aces_interchange` + AP0 fallback.
- [x] 5.2 Working-space luminance weights (`utils::Luma`) through light power,
      the light cache, guiding, environment importance, OpenPBR and MaterialX
      lobe selection, adaptive sampling and the `variance` AOV.
- [x] 5.3 Blackbody from XYZ into the working space (`lux::blackbody_in`).
- [x] 5.4 Interop ID by matrix fingerprint; chromaticities from it.
- [x] 5.5 `UsdColorSpaceAPI` inheritance for colour attributes and texture
      files.
- [x] 5.6 Default renders still bit-identical.

## 6. Docs

- [x] 5.1 `docs/color_management.md`, `docs/architecture.md`, site pages.
