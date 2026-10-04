## Why

crust rendered in linear Rec.709 and nothing else. A texture tagged in a wider
gamut — `acescg`, `g22_ap1`, `srgb_displayp3`, `lin_rec2020` — was read as
stored, with its primaries ignored, and a stage's
`RenderSettings.renderingColorSpace` was refused with a warning. ACES
pipelines author in ACEScg and expect the renderer to do its arithmetic there:
saturated colours that Rec.709 cannot hold survive light transport instead of
being clipped at the input. With every transfer curve already coming from an
OpenColorIO config (`ocio-rs`), the config also knows every gamut conversion;
what was missing was a working space to convert *into*, and a texture path that
can carry a change of primaries without giving up byte storage.

## What Changes

- **A working colour space.** Light transport happens in a scene-linear space
  chosen by `RenderSettings.renderingColorSpace`, overridden by
  `--working-space` / `UsdImportOptions::working_space`; default `lin_rec709`,
  so existing scenes render as before. A space that is not scene-linear is an
  error from the host and a warning from the stage.
- **One conversion rule.** A value that names its colour space is converted
  from it into the working space; a value that names none is taken as already
  in it; data is never converted. Names come from MaterialX `colorspace`
  (input → node → nodegraph → document, colour types only), USD `colorSpace`
  attribute metadata, `UsdUVTexture.sourceColorSpace`, the 8-bit-is-sRGB
  `auto` rule, and crust's own `g22_rec709` convention for Ptex and
  `PxrDisneyBsdf.baseColor`.
- **Any OCIO texture space converts exactly**, split into its per-channel curve
  (in the byte decode table) and its `f64` change of primaries (applied once
  per lookup, after filtering). **BREAKING (fix)**: a texture tagged on other
  primaries than the working space's was read raw; it is now converted.
- **Outputs record the working space**: `colorInteropID` on every product, and
  `chromaticities` off Rec.709 (the single beauty EXR included). A `lin_rec709`
  beauty keeps its header and pixels.
- **CLI**: `--ocio-config` (falling back to `$OCIO`), `--working-space`, `--display`, `--view` (the PNG
  preview through any OCIO display / view, e.g. an ACES output transform).
- **Primaries from the config**: the working space's luminance weights drive
  every heuristic; blackbody is computed straight into it; it is identified
  by its RGB → XYZ matrix when the config gives no interop ID.
- **`UsdColorSpaceAPI`**: a colour's space is inherited from its prim and
  ancestors' `colorSpace:name`.

## Capabilities

### Modified Capabilities

- `textures`: a request names its source space and the working space; any
  curve-and-matrix conversion is applied.
- `usd-scene-import`: the working space is read from `renderingColorSpace`;
  authored colours follow `colorSpace` metadata.
- `image-output`: EXR colour metadata; the preview goes through an OCIO
  display / view.
- `cli`: four colour flags.

## Impact

- `crust-core`: `color.rs` (config, interned `Space`, `Conversion`,
  `ColorSpace` / `ResolvedColorSpace`), `AssetLoader::load_environment` /
  `load_light_texture` take a `ColorSpace`, the importer's colour reads.
- `crust-mtlx`: `Host { load_texture, convert_color }`; colour-space
  inheritance; literal colours converted at compile time.
- `crust-assets`: gamut per lookup (`UvTexture`, `StreamingTexture`,
  `PtexStream`), at load (float textures, preloaded Ptex, environments).
- `crust-render`: flags, EXR attributes, preview.
- No throughput change in `lin_rec709` (no matrix exists); in another space,
  one 3x3 multiply per byte-texture lookup.
