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

## 5. Docs

- [x] 5.1 `docs/color_management.md`, `docs/architecture.md`, site pages.
