# Color management

Every mathematical operation in the renderer — BSDF evaluation, MIS weighting,
volume transport, pixel filtering, sample accumulation — assumes its inputs are
**linear light**. Light adds and scales linearly; a display-encoded value does
not. Mixing the two silently produces plausible-looking images that are wrong:
a display-encoded value used directly as an albedo always *overshoots*
reflectance, by a factor that grows as the colour gets darker — 1.4× at an
authored 0.75, 2.3× at 0.5, 6.6× at 18% grey, 10× at 0.1. Because the factor is
value-dependent, the error is not a brightness offset but a nonlinear
redistribution that no exposure correction can undo.

This document records, per input, what colour space it is assumed to be
authored in and what conversion is actually applied. It exists because the
answer is currently **not uniform** and not enforced anywhere — see
[Known gaps](#known-gaps).

## The invariant

> Every colour-valued input is converted to linear at **import or asset-load
> time**, once, before it reaches any shading, lighting, or transport code.
> Scalar (non-colour) inputs are never transfer-converted. The only
> re-encoding happens on output, when writing the preview PNG.

Import-time conversion is a deliberate choice over converting at lookup: it
costs O(materials + texels loaded) rather than O(samples), so it never appears
in a per-ray profile.

The work is split across the two crates along the same seam as everything else:
`crust-core` converts values it reads from USD *attributes* itself
(`crust-core/src/scene/usd_import/`), but decodes no **asset** — every image and Ptex decoder lives
in the host (`crates/crust-assets`), reached through `AssetLoader`. The
`PtexTexture` trait pins the contract at that boundary
(`crust-core/src/texture.rs`, the `PtexTexture` trait): values returned from
`eval` are linear, not
display-encoded, so the host must have decoded them already.

## OpenColorIO supplies every curve

Every transfer curve and every colour-space name comes from one OpenColorIO
config, through [`ocio`](https://github.com/doubleailes/ocio-rs) — a pure-Rust
port of OpenColorIO, with no `unsafe` — in `crust-core/src/color.rs`:

- **The config** is the builtin ACES CG config, named by its full version
  (`ocio://cg-config-v4.0.0_aces-v2.0_ocio-v2.5`, `color::CONFIG_URI`) so an
  `ocio` bump that ships a newer one cannot move a render on its own.
- **The working space** is `lin_rec709` (`color::WORKING_SPACE`). A decode is
  the OCIO processor from the authored space to it; an encode is the inverse.
- **Names resolve through the config's aliases**, case-insensitively, so a
  MaterialX `colorspace` may name a space by any of them —
  `Utility - sRGB - Texture`, `srgb_rec709_scene`, `g22_rec709_tx` — and still
  get the same curve (`ResolvedColorSpace::from_ocio_name`). `srgb`, which
  older documents use and the config does not list, is kept as a spelling of
  `srgb_texture`.
- **Byte-stored textures never run a processor per texel.** They meet the
  curve through two tables built once per open from the processor: the
  256-entry decode table and the 255 code steps that re-encode a mip level
  (`TransferCurve` in `crust-assets/src/lib.rs`). Everything else converts in
  batches (`decode_slice`), which costs about what the `powf` it replaced did.

Two choices are crust's, not the config's:

- **Only four spaces bind a texture**: `srgb_texture`, `g22_rec709`,
  `g18_rec709` and `raw` (`ResolvedColorSpace`). Each is a per-channel curve
  on Rec.709 primaries, which is what a one-byte-per-channel texture decoded
  through a table can express. A name the config knows on *other* primaries
  (`acescg`, `g22_ap1`, `adobergb`, …) still binds raw, as it did before OCIO
  resolved the names: converting its gamut needs texels stored after the
  conversion, not a per-channel table.
- **Encoded values below zero decode to zero.** The config's power-law spaces
  pass negatives through (`style: pass_thru`); crust has always clamped them,
  since a display-encoded value below black means nothing and an albedo must
  not go negative. Raw data is never clamped — a height may be negative.

## Three transfer curves, deliberately

There are three decode curves in use, and they are not interchangeable:

| Curve | OCIO space | Formula | Where |
| --- | --- | --- | --- |
| **Piecewise sRGB EOTF** | `srgb_texture` | `c ≤ 0.03929 ? c/12.923 : ((c+0.055)/1.055)^2.4` | LDR environment images and `RectLight` textures (`crust-assets/src/environment.rs`, `decode_image_pixels`); UV textures tagged `srgb_texture` |
| **Flat gamma 2.2** | `g22_rec709` | `max(c,0)^2.2` | `PxrDisneyBsdf.baseColor` (`usd_import/materials.rs`, `disney_to_openpbr`), Ptex colour texels (`crust-assets/src/ptex_texture.rs`, `decode_ptex_slice` / `decode_ptex_rgb` under `ColorSpace::Gamma22`, shared by both Ptex backends); UV textures tagged `g22_rec709` |
| **Flat gamma 1.8** | `g18_rec709` | `max(c,0)^1.8` | UV textures tagged `g18_rec709` (`crust-assets/src/lib.rs`, `TransferCurve::to_linear_table`) |

OCIO's sRGB toe differs from IEC 61966-2-1's rounded constants: it derives the
break point and slope from the exponent and offset so the two segments meet
exactly (break 0.03929, slope 1/12.9232, against the standard's 0.04045 and
1/12.92). The largest difference at any 8-bit code is 7.5e-7. The power laws
are bit-identical to `powf`. `crust-core/src/color.rs` pins both, against
formulas written out in the test rather than against OCIO itself.

MaterialX names `srgb_texture`, `g22_rec709` and `g18_rec709` as three
*separate* colour spaces (so does the OCIO config), and
[`ColorSpace::from_mtlx`][cs] maps them onto three separate decodes
accordingly. Folding the two power laws into the sRGB
branch — which this code did until it was caught — is wrong in the shadows for
2.2 (the table below) and wrong across the whole range for 1.8, whose
exponent is not 2.4-ish at all. The primaries in those two names are Rec.709,
which is the space crust already works in, so only the curve differs; a tag
naming *different* primaries (`acescg`, `g22_ap1`) is deliberately left as
`Raw` rather than decoded with the wrong gamut. Pinned by the decode tests in
`crust-assets/src/uv_texture/` and the tag-mapping tests in
`crust-core/src/texture.rs`.

[cs]: ../crates/crust-core/src/texture.rs

The flat 2.2 curve is *not* a sloppy approximation of the standard one — it
is matched to what the source content actually applies. The Moana island's shading
networks run Ptex colour through a `PxrColorCorrect` gamma-1/2.2 node, and its
GL path declares `sourceColorSpace = "sRGB"`; reproducing the reference render
matters more there than conforming to the sRGB standard. Both decisions carry
that reasoning in a comment at the call site (`usd_import/materials.rs`, `disney_to_openpbr`;
`crust-assets/src/ptex_texture.rs`, `decode_ptex_slice`).

How much does the distinction matter? Across most of the range, very little —
maximum absolute difference over `[0,1]` is 0.0085, and at 0.5 the two give
0.2140 vs 0.2176 (1.7% relative). The divergence is concentrated entirely in
the **deep shadows**, where the piecewise curve's linear toe keeps values well
above the pure power law:

| Encoded | sRGB EOTF | Gamma 2.2 | Ratio |
| --- | --- | --- | --- |
| 0.01 | 0.000774 | 0.000040 | 19.4× |
| 0.02 | 0.001548 | 0.000183 | 8.5× |
| 0.05 | 0.003936 | 0.001373 | 2.9× |
| 0.10 | 0.010023 | 0.006310 | 1.6× |
| 0.50 | 0.214041 | 0.217638 | 0.98× |

So: swapping the curves is invisible in midtones and highlights, and up to an
order of magnitude wrong in near-black albedo. Do not "simplify" them into one.

## Input inventory

The authoritative list of every colour-valued input the renderer reads, and its
current treatment. Note that "no curve applied" is the *majority* case and is
correct almost everywhere — the two `UsdPreviewSurface` rows are the only
genuine bug in the table (see the Verdict column). Those rows are the
*constant* inputs; a `UsdPreviewSurface` input driven by a `UsdUVTexture`
takes the texture's `sourceColorSpace` instead (see the textures table).

### Material shader inputs

| Input | Read at | Curve applied | Verdict |
| --- | --- | --- | --- |
| `UsdPreviewSurface.diffuseColor` | `usd_import/preview.rs`, `preview_surface_openpbr` | **none** | ⚠️ **bug** — see [Known gaps](#known-gaps) |
| `UsdPreviewSurface.emissiveColor` | `usd_import/preview.rs`, `preview_surface_openpbr` | **none** | ⚠️ **bug** — same |
| `PxrDisneyBsdf.baseColor` | `usd_import/materials.rs`, `disney_to_openpbr` | flat 2.2 | ✅ intentional (island `PxrColorCorrect`) |
| `crust:openpbr` — all 7 colour fields[^1] | `usd_import/materials.rs`, `decode_crust_openpbr` | **none** | ✅ intentional — native format is linear-authored |
| MaterialX `uniform_edf.color` | `crust-mtlx/src/bsdf.rs`, `edf_walk` | whatever the feeding node declares | ✅ correct per MaterialX |
| MaterialX surface-node colours (`base_color`, `specular_color`, `coat_color`, …) and leaf colours, **literal** | `crust-mtlx/src/surface.rs` / `bsdf.rs`, through the compiler | **none**: the value as authored | ⚠️ correct only for `lin_rec709` documents — see [Known gaps](#known-gaps) #4 |
| same, fed by an `image` | `crust-assets/src/uv_texture/` | the `image`'s own `colorspace`, as for any texture | ✅ correct per MaterialX |

[^1]: `baseColor`, `specularColor`, `transmissionColor`, `subsurfaceColor`,
`fuzzColor`, `coatColor`, `emissionColor` — all via the `c` closure at
the top of `decode_crust_openpbr` (`usd_import/materials.rs`), which assigns every field.

`crust:openpbr` is crust's own lossless 1:1 mirror of the `OpenPBR` struct, so
values are authored in the renderer's working space by definition. That makes
"no conversion" correct — but note it is achieved by *not calling anything*,
not by a stated decision.

**MaterialX emission is a radiance, and the evaluation is colour-space neutral.**
An `edf`'s colour is light leaving the surface, not a reflectance swatch, so the
curve question is answered entirely by whatever feeds it: a literal is authored
in the working space, and an `image` node carries its own `colorspace`
attribute through `ColorSpace::from_mtlx` exactly as `base_color`'s does — with
an absent tag meaning **raw**, which is the right answer for the scene-linear
float file an emission texture usually is. `samples/materialx_emissive.mtlx`
leaves it absent on purpose; tagging a Radiance `.hdr` `srgb_texture` would put
a transfer curve on light. The evaluation itself adds nothing: `MtlxMaterial`
sums the weighted terms (each factor sanitised per channel, a
`generalized_schlick_edf` falloff applied per channel), all in the one working
space, so it cannot introduce a colour error. Note this is also the **first input whose range is
used rather than merely carried** — the `.tx` EXR backing's values above 1.0
reach the film here, where on `base_color` they meet `eon_diffuse`'s ρ ≤ 1
clamp, correctly.

### Lights

| Input | Read at | Curve applied | Verdict |
| --- | --- | --- | --- |
| `inputs:color` (all four lux types[^2]) | `usd_import/lights.rs`, `lux_params`, via `attrs::attr_color3f` | **none** | ✅ correct — see below |

[^2]: `UsdLuxDistantLight`, `UsdLuxDomeLight`, `UsdLuxSphereLight`,
`UsdLuxRectLight` — all four share the one `lux_emission` helper, so there is a
single place where a light's colour is read.

`UsdLuxLightAPI`'s own schema documentation specifies `inputs:color` as being
"**in the rendering color space**." For a linear-light-transport renderer that
*is* linear, so no conversion is the right answer — not an oversight. It is
multiplied by `intensity × 2^exposure` (both pure scalars, no colour-space
implication) into the emission value handed to `DistantLight`/`DomeLight`/
`Emissive`.

Dome-light textures are decoded separately by the host (see below) and are
**not** double-decoded: the importer (`usd_import/lights.rs`) only resolves the path and hands it to
`AssetLoader::load_environment`, then multiplies the already-linear map by the
already-linear tint.

### Volume coefficients

| Input | Read at | Curve applied | Verdict |
| --- | --- | --- | --- |
| `crust:volume:sigmaS` | `usd_import/volume.rs`, `emit_volume`, via `attrs::custom_color3` | **none** | ✅ correct |
| `crust:volume:sigmaA` | `usd_import/volume.rs`, `emit_volume` | **none** | ✅ correct |
| `crust:volume:emission` | `usd_import/volume.rs`, `emit_volume` | **none** | ✅ correct |

These are crust-custom attributes (no upstream schema to defer to) holding
*physical quantities* — scattering and absorption cross-sections, and emitted
radiance. They are authored directly as numbers, never picked from a colour
swatch, so there is no display encoding to undo. Same reasoning as
`crust:openpbr`.

### Textures and environment maps

| Asset | Read at | Curve applied | Verdict |
| --- | --- | --- | --- |
| Ptex `.ptx` colour texels | `crust-assets/src/ptex_texture.rs` (`decode_ptex_slice`), requested `Gamma22` by `usd_import/materials.rs` (`material_ptex`) | flat 2.2 | ✅ intentional (island convention) |
| Ptex `.ptx` displacement texels (`PxrDisplace` → `PxrPtexture`, `PxrBlend` multiply) | `usd_import/materials.rs` (`pxr_displacement`) requests `Raw` → `crust-assets/src/ptex_texture.rs` / `ptex_stream.rs` (`decode_ptex_slice` / `decode_ptex_rgb`) | none (identity; no clamp, so a float height may be negative) | ✅ correct — a height is data, and `PxrPtexture.linearize` defaults to 0 |
| UV texture tagged `srgb_texture` | `crust-assets/src/uv_texture/` (`TransferCurve::to_linear_table`) | piecewise sRGB | ✅ correct per MaterialX |
| UV texture tagged `g22_rec709` | `crust-assets/src/uv_texture/` | flat 2.2 | ✅ correct per MaterialX |
| UV texture tagged `g18_rec709` | `crust-assets/src/uv_texture/` | flat 1.8 | ✅ correct per MaterialX |
| UV texture, any other tag or none | `crust-assets/src/uv_texture/` | none (pass-through) | ✅ correct — normals, roughness and masks are data |
| `UsdUVTexture`, `sourceColorSpace = "sRGB"` | `usd_import/preview.rs` (`preview_uv_input`) → `uv_texture/` | piecewise sRGB | ✅ correct per the node set |
| `UsdUVTexture`, `sourceColorSpace = "raw"` | same | none (pass-through) | ✅ correct per the node set |
| `UsdUVTexture`, `auto` or unauthored | same, resolved by `ColorSpace::resolve_auto` at open | piecewise sRGB for 8-bit RGB/RGBA, none otherwise | ✅ the UsdUVTexture rule (Hydra's) — note the default is `auto`, **not** raw as in MaterialX |
| `UsdUVTexture` feeding `UsdPreviewSurface.inputs:displacement`, `auto` or unauthored | `usd_import/preview.rs` (`preview_displacement` → `preview_uv_input` with `data`) | none (requested `Raw`) | ✅ a height is data, so the 8-bit-RGB-is-sRGB rule does not apply; an explicit `sRGB` still wins |
| Preloaded `.exr` UV texture | `crust-assets/src/uv_texture/` (`decode_exr_tile`) | none under `auto`/`raw`; an explicit curve is applied once, in `f32`, at load | ✅ stored as linear `f32` — no table, no clip |
| MaterialX emission `image`, untagged | `crust-assets/src/uv_texture/` / `tiled/` | none (pass-through) | ✅ correct — an EDF's colour is radiance, and a float file is scene-linear |
| LDR env image (PNG/JPG/…) | `crust-assets/src/environment.rs` (`decode_image_pixels`) | piecewise sRGB | ✅ correct per format |
| `.hdr` env image | `crust-assets/src/environment.rs` (`is_hdr`) | none (pass-through) | ✅ correct — HDR is scene-linear |
| `.exr` env map | `crust-assets/src/environment.rs` | none (pass-through) | ✅ correct — EXR is linear |
| LDR `RectLight` `texture:file` (PNG/JPG/…) | `crust-assets/src/lib.rs` (`load_light_texture` → `read_rgb_image`) | piecewise sRGB | ✅ same decoder as an LDR env image |
| `.hdr` / `.exr` `RectLight` `texture:file` | same | none (pass-through) | ✅ correct — kept as authored, never narrowed to 8 bits |
| Streamed `.tx`, TIFF backing (`u8` tiles) | `crust-assets/src/tiled/cache.rs` (`Tile::rgb`) | the tagged curve, per lookup | ✅ same table as the preload path, by construction |
| Streamed `.tx`, EXR backing (`half` tiles) | — | none (decoded once at conversion) | ✅ correct — the file stores linear samples and records which space they came from |

The `is_hdr` branch in `load_image_environment` exists because `image`'s `to_rgb32f`
rescales integer formats into `0..1` *without* removing their transfer curve,
while leaving true HDR values as authored — so the decode must be conditional
on the format, not applied blanket. Pinned by
`ldr_images_are_converted_to_linear` (`crust-assets/src/environment.rs`).

Ptex texels are decoded once at load into the preloaded immutable buffer, not
per lookup — which is also why `CRUST_PTEX_MAX_LOG2`'s mip cap and the decode
share the same pass.

## Where texels get averaged, and in which space

Two places reduce a texture at load, and they deliberately use **different
spaces**. Both are correct for what they are for, and mixing them up is a
plausible-looking bug rather than an obvious one.

| Reduction | Where | Space | Why |
| --- | --- | --- | --- |
| **`CRUST_TEX_MAX` cap** | `crust-assets/src/uv_texture/`, `decode_tile` | the file's own encoding | It is a *resize*, not a filter: the capped tile should look like the DCC's preview of the same file, which is also computed on encoded bytes. |
| **Mip levels** | `uv_texture/`, `Tile::build_pyramid` | **linear**, re-encoded through the colour space's inverse curve | It *is* a filter — it stands in for integrating light over a pixel's footprint — and summing display-encoded values is not summing light. |
| **Ptex mip levels (preloaded)** | `crust-assets/src/ptex_texture.rs`, in `open_with` | **linear** (already decoded) | Same reason; no round trip needed, since the base is decoded to linear `f32` at load and reduced from there. |
| **Ptex mip levels (`.ptx` on disk)** | the file's writer, read back by `ptex_stream.rs` | the file's own encoding | Not crust's choice — the levels were reduced before crust ever saw the file, and crust decodes Ptex by 2.2 afterwards. So it is the mismatch the row below refuses, and streaming such a texture is declined by default (`CRUST_PTEX_STREAM_MIPSPACE`). |
| **`.tx` mip levels (TIFF backing)** | `crust-assets/src/tiled/write.rs` | **linear**, re-encoded | The same `reduce_half` as the in-memory pyramid — literally the same function, so a streamed render and a preloaded one cannot drift apart. |
| **`.tx` mip levels (EXR backing)** | `crust-assets/src/tiled/exr_write.rs` | **linear**, not re-encoded | The samples are already light: a float file has no transfer curve, so the decode happened once at conversion and the reduction is a plain average (`reduce_half_linear`, written next to `reduce_half` so the two cannot drift on anything but the curve). |

There are therefore **three** decode points, not two, and which one applies is
decided by a tile's payload rather than by its file:

| Payload | Stored as | Decoded | Where |
| --- | --- | --- | --- |
| `TileData::U8` (8-/16-bit TIFF) | the file's own encoding | per lookup, through the 256-entry table | `Tile::rgb` |
| `TileData::Half` (EXR, float TIFF) | **linear** | never — it is already light | — |
| preloaded `UvTexture` | the file's own encoding | per lookup, through the same table | `UvTexture::sample_level` |

Because a TIFF-backed `.tx` stores display-encoded texels but reduces in linear
light, the colour space is **baked into every level above 0** and cannot be
reinterpreted afterwards. Read a chain built for sRGB as raw and level 0 stays
perfectly correct while every coarser level is wrong — an error that appears
only under minification and looks exactly like a filtering bug. An EXR-backed
`.tx` gets there from the other side: its texels were decoded once, at
conversion, so binding it under a different space would apply a curve to data
that has already had one removed.

**Ptex arrives at the same rule from the other end, and gets the same answer.**
A `.ptx` carries no marker and needs none: crust binds Ptex colour as
display-encoded and decodes it by 2.2, while the file's stored levels were
reduced before that decode. The mismatch is therefore unconditional rather than
a property of a particular file, and it is refused the same way — a texture
whose lookups could reach one of those levels is not streamed at all, but
preloaded, where the pyramid is rebuilt in linear light from the decoded base
(`MipSpace::Linear`, the default). `CRUST_PTEX_STREAM_MIPSPACE=file` accepts the
file's chain instead, at a measured 0.147 of darkening on the tiled fixture;
`docs/ptex_streaming.md` prices that trade against the residency it buys.

So the space is written into the file — `crust:mipspace=` in
`ImageDescription` for TIFF (following OIIO's own `oiio:SHA-1=` convention) and
as a header attribute for EXR, where custom attributes are first-class — and it
means "the space this file is to be bound with" for both. A mismatch makes the
streaming path decline, falling back to preloading. A file with no marker —
anything `maketx` wrote — is accepted, since its chain came from a different
filter and there is nothing to match against.

The difference is not academic. A black/white checkerboard averaged in
sRGB-encoded bytes gives `127/255 ≈ 0.5` *encoded*, which decodes to **0.21
linear** — less than half the light actually present. Averaged in linear and
re-encoded it gives 0.5 linear, which stores as `188/255`. Pinned by
`levels_average_in_linear_light_not_in_the_file_encoding` in
`crust-assets/tests/decoders.rs`.

UV mip levels are stored back as `u8` in the file's own encoding rather than
as linear `f32`, so the lookup's `u8`-indexed decode table is unchanged and the
pyramid costs a third of the base rather than four times it. The re-encode is
a table too: [`TransferCurve::code_steps`][enc] holds, for each byte, the
linear value at which the next one begins — OCIO's decode of `(k + 0.5)/255` —
and `quantize` bisects it. Rounding the inverse curve's output to the nearest
byte picks the same code wherever it is not on a half-code boundary, and the
curve never runs per texel (`quantize_is_the_rounded_encode`). The OCIO name of
each space comes from `ResolvedColorSpace::ocio_name`, matched on the variant,
so adding a colour space is a compile error there instead of a silent
fall-through to `Raw`. Every curve takes a `ResolvedColorSpace`, which has no
`Auto`: `ColorSpace::resolve_auto` is the only way from the requested space to
one a decoder can apply.

[enc]: ../crates/crust-assets/src/lib.rs

## Scalar inputs are never converted

Roughness, weights, IOR, anisotropy, metalness, opacity, `intensity`,
`exposure`, volume `anisotropy`, `densityScale` — every scalar parameter is
used at its authored value. A transfer curve encodes *perceptual* response for
colour channels; applying one to a roughness or an IOR is meaningless.

The type system already enforces this direction of the rule by accident: the
decode helpers take `Vec3A`, and scalars are `f32`, so a scalar cannot be fed
to one. The reverse — a colour that never passes *through* a decode — is
unenforced, and is exactly how the `UsdPreviewSurface` gap below arose.

## The output side

The engine produces a linear `Buffer`. The CLI writes it two ways
(`crust-render/src/main.rs`, the one place that still touches pixels):

- **`.exr`** — the linear values, unmodified. This is the render output.
- **`.png`** — tone-mapped: clamp to `[0,1]`, then encode through the OCIO
  config's `sRGB - Display` with the `Un-tone-mapped` view
  (`color::encode_preview`, called from `tone_map` in `main.rs`) — the
  piecewise sRGB curve and nothing else. A preview, not a deliverable.

`tone_map` is the *inverse* of the LDR-image decode above: the same curve in
the forward direction (to within a matrix round trip through the config's
reference spaces; no 8-bit code differs from `lin_rec709 → srgb_texture`). Comparing renders numerically should
always use the EXR (`examples/exr_diff`), never the PNG, since the PNG has both
clamped and re-encoded.

### Scoring images outside the renderer (`scripts/material_fidelity`)

The Material Fidelity harness reads images the engine never sees. It compares them
the way the suite does, as 8-bit **sRGB code values**, and never linearises them:

| Image | Read at | Assumed | Verdict |
| --- | --- | --- | --- |
| `<renderer>.avif` reference or peer render (current suite) | `suite.load_rgb8` (Pillow / libavif) | sRGB primaries, piecewise sRGB curve, 8-bit | ✅ how the suite writes them: its sRGB PNG, re-encoded by `sharp` (AVIF q90, 4:4:4). The files carry no `colr` box, so the YUV→RGB matrix is the decoder default; `sharp` and libavif agree on it |
| `<renderer>.png` reference (older suite layout) | same | sRGB, 8-bit | ✅ the suite's own output encoding |
| crust's EXR | `suite.read_exr_rgb` → `linear_to_srgb8` | scene-linear Rec.709 | ✅ clamp, piecewise sRGB OETF, 8 bits: the suite's "no tone mapping, sRGB" contract, not the CLI's PNG |
| Goldeneye reference PNG | `goldeneye_suite.py` (decoded from the AVIF) | sRGB, 8-bit | ✅ Goldeneye's LDR path applies the same OETF to the EXR before FLIP |

PSNR on code values is the suite's metric (`metrics.ts`), so it is reproduced
as is. It weights errors perceptually, not radiometrically: 1 dB in the shadows
and 1 dB in the highlights are different amounts of light.

## Known gaps

**1. `UsdPreviewSurface` colours are not decoded.** `diffuseColor` and
`emissiveColor` land in `OpenPBR` raw (`usd_import/preview.rs`, `preview_surface_openpbr`). Values
authored as DCC colour-picker swatches — the normal case for this schema — are
therefore used as if already linear, overshooting albedo substantially. Note
for honesty: the `UsdPreviewSurface` spec itself does **not** mandate a colour
space for these inputs (its only explicit colour-space note concerns normal
maps), so treating them as sRGB is a defensible convention rather than a
standards requirement — but "no conversion at all" is not a defensible reading
of any convention.

**2. The abstraction covers assets only, so USD attributes enforce nothing.**
`ColorSpace` (`crust-core/src/texture.rs`) does exist, and every UV texture
crossing the `AssetLoader` seam names its space — that is what makes
`srgb_texture` / `g22_rec709` / `g18_rec709` three distinct decodes. Since the
move to OpenColorIO there is also only one implementation of each curve
(`crust-core/src/color.rs`), which USD attribute reads call too
(`disney_to_openpbr` decodes through `ResolvedColorSpace::Gamma22`). But
nothing forces a newly added colour attribute to state its source space; the
default behaviour of adding one is to get gap #1 again, silently.

**3. Per-attribute colour-space authoring is unsupported.** A USD attribute
carrying an explicit `colorSpace` metadatum is ignored; the curve is chosen by
shader family, not by what the asset declares. The OCIO config already knows
USD's names (`lin_rec709_scene`, `srgb_rec709_scene`, `lin_ap1_scene`, … are
aliases in it), so what is missing is reading the metadatum, and — for a
space on other primaries — a gamut conversion for constant colours, which
unlike a texture's needs no change to storage.

**4. A MaterialX document's `colorspace` is not applied to literal colours.**
The compiler honours `colorspace` only where it crosses into a texture decode
(an `image`'s `file`). A literal `color3` value on a surface node, a BSDF leaf
or a pattern node is used as authored, whatever the document, node-graph or
input `colorspace` says. The surface-shader builders inherit this unchanged
(`openspec/changes/add-mtlx-surface-shaders/`), since they read every input
through the same compiler. It is invisible to the Material Fidelity suite, whose
documents are `lin_rec709` throughout, so no suite result can catch a mistake
here; a document authored `colorspace="srgb_texture"` at the root would shade
too bright.

Gaps #1 and #2 are addressed by the OpenSpec change
`openspec/changes/add-material-color-management/`, which introduces a
`ColorSpace { Linear, Srgb, Gamma(f32) }` enum plus a `RawColor3` newtype at
each of the three attribute-reading primitives (`shader_input_vec3`,
`attr_color3f`, `custom_color3`) so that *every* colour call site must name its
space — including the ones whose answer is `Linear`. **Not yet implemented.**

## Diagnosing a suspected colour-space bug

A wrong transfer curve produces a plausible image, so appearance proves
nothing. Two switches answer in numbers instead:

```bash
# Is the surface's colour coming from the texture or the constant fallback?
# CRUST_PTEX=0 declines every Ptex texture; surfaces fall back to baseColor.
CRUST_PTEX=0 cargo run --release -- -i scene.usda -o out.exr

# What are the actual texel values, before and after decode?
cargo run --release -p crust-render --example tex_probe -- texture.ptx
cargo run --release -p crust-render --example tex_probe -- render.png [x0 y0 x1 y1]

# Is it the decode, or the mip level it is being read at? These turn off the
# filtering without touching the decode, so a difference that survives them is
# a colour-space question and one that does not is a filtering question.
CRUST_TEX_MIP=0 CRUST_PTEX_MIP=0 cargo run --release -- -i scene.usda -o out.exr
CRUST_RAY_CONES=0 cargo run --release -- -i scene.usda -o out.exr
```

The two are worth separating early, because a mip level averaged in the wrong
space has the *same* signature as a missing decode — a minified surface that
drifts darker than it should — and the fix is in a different file. A pyramid
built on encoded bytes loses light at every level, so the error grows with
distance from the camera; a missing decode is wrong at every distance equally.

`tex_probe` exists specifically to settle whether a texture is
display-encoded or linear: it prints raw values, so the overshoot signature of
a missing decode is visible as a number rather than guessed from a render.
Rule of thumb — a *linear* albedo should sit well below its authored value
(18% grey encodes to ~0.46), so a linear diffuse colour that still reads
0.5–0.9 across all three channels on a natural material is the usual tell that
a decode was skipped.

See also `docs/openpbr_reference_alignment.md` for how the OpenPBR parameters
these colours feed are defined, and the "Ptex" section of `openspec/specs/textures/design.md` for the
face-addressing checks that are orthogonal to (and easily confused with)
colour-space correctness.
