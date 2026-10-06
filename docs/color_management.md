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
authored in, what conversion is actually applied, and how the result reaches
the working space the renderer does its arithmetic in. The rule is uniform
(see [What is converted](#what-is-converted-one-rule)); what it does not yet
enforce is listed under [Known gaps](#known-gaps).

## The invariant

> Every colour-valued input is converted to linear light in the **working
> space** at **import or asset-load time**, once — or, for a byte texture, its
> curve at load and its change of primaries once per lookup — before it
> reaches any shading, lighting, or transport code.
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

## OpenColorIO and the working space

Every transfer curve, every gamut conversion and every colour-space name comes
from one OpenColorIO config, through [`ocio`](https://crates.io/crates/ocio)
— a pure-Rust port of OpenColorIO, with no `unsafe` — in `crust-core/src/color.rs`:

- **The config** is the builtin ACES CG config, named by its full version
  (`ocio://cg-config-v4.0.0_aces-v2.0_ocio-v2.5`, `color::DEFAULT_CONFIG`) so an
  `ocio` bump that ships a newer one cannot move a render on its own. The host
  may install another before the first colour is converted (`--ocio-config`,
  else `$OCIO` — read into `Config::ocio` and obeyed by the CLI only, so a
  library test never depends on the shell — through `color::use_config`); it must define `raw`, `lin_rec709`, `srgb_texture`,
  `g22_rec709` and `g18_rec709` (the five well-known `Space`s), which every
  ACES config does as aliases.
- **The working space** is the scene-linear space light transport happens in:
  `lin_rec709` unless `RenderSettings.renderingColorSpace` or the host
  (`--working-space`, `UsdImportOptions::working_space`) names another —
  ACEScg, linear Rec.2020, … The default is `lin_rec709` whatever the config:
  the config's `scene_linear` role (ACEScg in the builtin config and the ACES
  studio configs) is never read, so swapping configs (`$OCIO`) cannot change
  a render's working space, and a default render stays bit-identical to the
  pre-OCIO one. It is not a global: every texture request
  (`ColorSpace`) carries the working space it converts *to*, the importer
  holds it in `ImportCaches::working`, and the `Scene` reports it
  (`Scene::working_space`) for the outputs.
- **Spaces are interned** (`color::Space`, `Copy` and hashable): any name or
  alias, in any case, of one space is one id. `srgb`, which older MaterialX
  documents use and the config does not list, is kept as `srgb_texture`.

### What is converted: one rule

> A value that **names** its colour space is converted from it into the
> working space. A value that names **none** is taken as already in the
> working space. **Data** (the config's `Raw`) is never converted.

That is what MaterialX specifies for a document with no `colorspace`, what
UsdLux says of `inputs:color` ("in the rendering color space"), and what Karma
and other OCIO renderers do. The places a value names its space:

| Source | Named by |
| --- | --- |
| MaterialX `color3` / `color4` literal, and an `image` with a colour output | the effective `colorspace`: input, else node, else node graph, else document (crust-mtlx, `Doc::colorspace_of`); a data image (`float`, `vector*`) is never converted |
| USD colour attribute (light `inputs:color`, `focusTint`, preview `diffuseColor` / `emissiveColor`, `crust:openpbr` colours, `crust:volume:*`) | as `UsdColorSpaceAPI::ComputeColorSpaceName` resolves it: the attribute's `colorSpace` metadatum, else `colorSpace:name` on its prim, else on the nearest ancestor that authors one (`usd_import/attrs.rs`, `attr_color_space` / `in_working`) |
| `UsdUVTexture` | `inputs:file`'s colour space resolved as above, else `sourceColorSpace` (`sRGB` → `srgb_texture`, `auto` → the format rule) |
| Dome / `RectLight` image | `texture:file`'s colour space resolved as above, else `auto`: an 8-bit image is `srgb_texture`, a float one already in the working space |
| `PxrDisneyBsdf.baseColor`, colour Ptex | `g22_rec709`, the island convention (below), unless metadata says otherwise |
| Colour temperature | the Planckian locus's XYZ, straight into the working space (`lux::blackbody_in`) |

A `lin_rec709` render of a scene that names nothing but sRGB textures is
therefore exactly what it was before the working space existed: every
conversion it meets is a curve on its own primaries.

### A conversion is a curve and a matrix

Every texture space an OCIO config defines is a per-channel transfer curve
followed by a 3x3 change of primaries, and the *optimised* OCIO processor says
so: a run of per-channel ops (`Exponent`, `ExponentWithLinear`, `LogCamera`,
…) and at most one trailing `Matrix`. `color::Conversion` keeps the two apart
(`color::build`): the matrix is taken exactly, in `f64`, from the processor's
group transform, and the curve is an OCIO processor of the remaining ops,
checked to have no channel crosstalk. A conversion of any other shape — a 3D
LUT, a matrix *before* the curve — is refused once, with a warning, and the
values are used as stored. `curve_then_matrix_is_the_ocio_processor` pins the
split against the whole processor for every texture space of the ACES config,
into both `lin_rec709` and ACEScg.

The split is what lets a **byte texture keep its byte storage**. The 256-entry
decode table is the curve alone, in the file's own primaries; the matrix is
applied once per lookup, *after* filtering — a linear map commutes with the
area-weighted average, so a mip level reduced in the source's linear light is
the same light in any working space. So:

| Payload | Curve | Matrix |
| --- | --- | --- |
| preloaded `u8` UV texture | decode table | per lookup (`UvTexture::gamut`) |
| streamed `.tx`, TIFF (`u8`) or EXR (`half`) backing | table / at conversion | per lookup (`StreamingTexture::gamut`) |
| preloaded float UV texture (EXR) | at load | at load — stored in the working space |
| Ptex, preloaded | at load | at load, per texel |
| Ptex, streamed | table (`u8`) / per texel | per texel |
| environment map, `RectLight` image | at load | at load |
| constant colour | at import | at import |

Every path applies the matrix through one function,
`color::apply_gamut` (`crust_assets::to_working` for an RGBA lookup), so the
pinned equalities hold off the working primaries too: streamed and preloaded
`.tx` agree bit for bit in ACEScg (`a_streamed_tx_matches_the_preloaded_texture_in_acescg`),
and so do streamed and preloaded Ptex (`streamed_and_preloaded_agree_in_acescg`).
A `.tx` records only its *source* space (`crust:mipspace=`), since its levels
depend on the curve and not on the working space it is later bound into.

**Byte textures never run a processor per texel.** Besides the decode table,
the mip re-encode is a table too: the 255 code steps (`TransferCurve`, in
`crust-assets/src/lib.rs`). Everything else converts in batches
(`decode_rgb_slice`), which costs about what the `powf` it replaced did.

### Primaries: XYZ and luminance

`color::to_xyz` is a scene-linear space's RGB → CIE XYZ matrix, adapted to
D65, read from the config: the space's conversion into its scene-referred XYZ
space (`cie_xyz_d65_scene`), or, without one, into `aces_interchange` and the
standard AP0 → XYZ-D65 (Cycles' fallback). The config's
`cie_xyz_d65_interchange` role is not used: in the ACES configs it names the
*display*-referred XYZ space. Three things come from it:

- **Luminance weights** (`color::luma`, a `utils::Luma`): its `Y` row. Every
  heuristic that weighs a colour by one number uses the working space's —
  light power (`LightList::luma`, `Light::power`), the learned light cache,
  guiding's training signal, environment importance
  (`EnvironmentMap::new_in`), lobe selection (`OpenPBR::luma`, MaterialX
  `load_in`), and the renderer's adaptive-sampling, guiding-blend and
  `variance` statistics. They travel by value with the scene, not in a
  global. `lin_rec709` keeps `Luma::REC709`, the config's own luma
  coefficients (0.2126, 0.7152, 0.0722; pinned by
  `utils_luminance_uses_the_config_luma_coefficients`), which `Luma::of`
  multiplies in the order the old constant expression did — so a default
  render is bit-identical. In ACEScg the weights are (0.2722, 0.6741, 0.0537)
  to four digits, and a colour's luminance is the same whichever space holds
  it (`luminance_weights_are_the_working_spaces_y_row`). The grey fallback
  material keeps Rec.709's, which weigh a grey the same.
- **Blackbody** (`lux::blackbody_in`): the Planckian locus's XYZ through the
  inverse matrix, normalised to unit luminance in the working space — as
  Typhoon does it, so a low temperature is not clipped at Rec.709's gamut on
  its way to ACEScg. `lin_rec709` is `blackbody_rgb`, bit for bit.
- **Identification** (`color::interop_id`): the fingerprint above.

### crust's two clamps

- **Encoded values below zero decode to zero** before a curve. The config's
  power-law spaces pass negatives through (`style: pass_thru`); crust has
  always clamped them, since a display-encoded value below black means
  nothing. Raw data is never clamped — a height may be negative — and a curve
  is free to produce a negative (ACEScct's lowest codes do).
- **A change of primaries clamps its result at zero.** A colour outside the
  working gamut — saturated AP1 red rendered in Rec.709 — comes out with a
  negative component, which as a reflectance or an emission is not a colour.
  A conversion with no matrix is untouched by this clamp.

### The outputs

The `Buffer` is in the working space, and the outputs say so:

- **EXR** — every RenderProduct carries `colorInteropID`, the working space's
  ASWF Color Interop ID (`lin_rec709_scene`, `lin_ap1_scene`, …;
  `color::interop_id`): the config's when it gives one, else the standard
  whose primaries the space's RGB → XYZ matrix matches to 1e-4 (the
  fingerprint Cycles takes), so a studio config without interop IDs still
  says `lin_ap1_scene` for ACEScg. Off Rec.709 the file also carries
  `chromaticities`, the standard's own (ACES's white, not the adapted D65;
  `color::chromaticities`), and so does the single beauty EXR written without
  products. An EXR without `chromaticities` *is*
  Rec.709 by the format's definition, which is why a `lin_rec709` beauty is
  still the file `write_rgb_file` wrote: the same header and pixels. (Not the
  same bytes run to run: `exr` compresses blocks in parallel and writes them in
  completion order, before and after this change alike.)
- **PNG** — encoded through the config's display / view
  (`color::encode_preview`): `sRGB - Display` / `Un-tone-mapped` by default,
  a clamp to `[0, 1]` and the display's curve, so the preview is the EXR
  clipped. `--display` / `--view` pick another; an ACES output transform
  (`ACES 2.0 - SDR 100 nits (Rec.709)`) maps the whole scene-linear range.

## Three transfer curves, deliberately

There are three decode curves in use, and they are not interchangeable:

| Curve | OCIO space | Formula | Where |
| --- | --- | --- | --- |
| **Piecewise sRGB EOTF** | `srgb_texture` | `c ≤ 0.03929 ? c/12.923 : ((c+0.055)/1.055)^2.4` | LDR environment images and `RectLight` textures (`crust-assets/src/environment.rs`, `decode_image_pixels`); UV textures tagged `srgb_texture` |
| **Flat gamma 2.2** | `g22_rec709` | `max(c,0)^2.2` | `PxrDisneyBsdf.baseColor` (`usd_import/materials.rs`, `disney_to_openpbr`), Ptex colour texels (`crust-assets/src/ptex_texture.rs`, `decode_ptex_slice`, and in `ptex_stream.rs` the conversion resolved once per file, under `Space::G22_REC709`); UV textures tagged `g22_rec709` |
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
| `UsdPreviewSurface.diffuseColor` | `usd_import/preview.rs`, `preview_surface_openpbr` | **none** unless `colorSpace` metadata names a space | ⚠️ see [Known gaps](#known-gaps) #1 |
| `UsdPreviewSurface.emissiveColor` | `usd_import/preview.rs`, `preview_surface_openpbr` | same | ⚠️ same |
| `PxrDisneyBsdf.baseColor` | `usd_import/materials.rs`, `disney_to_openpbr` | `g22_rec709` → working (or its `colorSpace`) | ✅ intentional (island `PxrColorCorrect`) |
| `crust:openpbr` — all 8 colour fields[^1] | `usd_import/materials.rs`, `decode_crust_openpbr` | **none** unless `colorSpace` metadata names a space | ✅ intentional — native format is authored in the working space |
| `crust:openpbr` `subsurfaceRadiusScale` | same | never — a per-channel radius multiplier, not a colour | ✅ |
| MaterialX `uniform_edf.color` | `crust-mtlx/src/bsdf.rs`, `edf_walk` | whatever the feeding node declares | ✅ correct per MaterialX |
| MaterialX surface-node colours (`base_color`, `specular_color`, `coat_color`, …) and leaf colours, **literal** | `crust-mtlx/src/eval/compile.rs`, at compile time through `Host::convert_color` | the effective `colorspace` → working; none when no scope declares one | ✅ correct per MaterialX |
| same, fed by an `image` | `crust-assets/src/uv_texture/` | the `file`'s effective `colorspace` → working, as for any texture | ✅ correct per MaterialX |

[^1]: `baseColor`, `specularColor`, `transmissionColor`, `transmissionScatter`,
`subsurfaceColor`, `fuzzColor`, `coatColor`, `emissionColor` — all via the `c`
closure at the top of `decode_crust_openpbr` (`usd_import/materials.rs`), which
converts through `in_working`; the one non-colour vector goes through `v`.

`crust:openpbr` is crust's own lossless 1:1 mirror of the `OpenPBR` struct, so
values are authored in the renderer's working space by definition, and "no
conversion" is the rule's answer for an attribute that names no space.

**MaterialX emission is a radiance, and the evaluation is colour-space neutral.**
An `edf`'s colour is light leaving the surface, not a reflectance swatch, so the
curve question is answered entirely by whatever feeds it: a literal is authored
in the working space, and an `image` node carries its own `colorspace`
attribute through `ColorSpace::from_mtlx` exactly as `base_color`'s does — with
an absent tag meaning **already in the working space**, which is the right
answer for the scene-linear float file an emission texture usually is. `samples/materialx_emissive.mtlx`
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
| `inputs:color` (all lux types[^2]) | `usd_import/lights.rs`, `lux_params`, via `attrs::in_working` | **none** unless `colorSpace` metadata names a space | ✅ correct — see below |
| colour temperature | `usd_import/lights.rs`, `lux_params` → `lux::blackbody_in` | XYZ → working through the config's XYZ matrix, unit luminance there (`lin_rec709`: `blackbody_rgb`, bit for bit) | ✅ |
| `inputs:shaping:focusTint` | `usd_import/lights.rs`, `lux_shaping`, via `attrs::custom_color` | as `inputs:color` | ✅ |

[^2]: `UsdLuxDistantLight`, `UsdLuxDomeLight`, `UsdLuxSphereLight`,
`UsdLuxRectLight` — all four share the one `lux_emission` helper, so there is a
single place where a light's colour is read.

`UsdLuxLightAPI`'s own schema documentation specifies `inputs:color` as being
"**in the rendering color space**" — crust's working space — so no conversion
is the right answer, unless the attribute's `colorSpace` metadatum says
otherwise. It is
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
| `crust:volume:sigmaS` | `usd_import/volume.rs`, `emit_volume`, via `attrs::custom_color` | **none** unless `colorSpace` metadata names a space | ✅ correct |
| `crust:volume:sigmaA` | `usd_import/volume.rs`, `emit_volume` | same | ✅ correct |
| `crust:volume:emission` | `usd_import/volume.rs`, `emit_volume` | same | ✅ correct |

These are crust-custom attributes (no upstream schema to defer to) holding
*physical quantities* — scattering and absorption cross-sections, and emitted
radiance. They are authored directly as numbers, never picked from a colour
swatch, so there is no display encoding to undo. Same reasoning as
`crust:openpbr`. They are per-channel quantities all the same, so a
`colorSpace` metadatum converts them like a colour (and the gamut clamp keeps
a converted cross-section non-negative).

### Textures and environment maps

Every row converts into the working space after its curve (the table in
[A conversion is a curve and a matrix](#a-conversion-is-a-curve-and-a-matrix)
says where the matrix is applied); in `lin_rec709` every curve below is on the
working primaries, so there is no matrix.

| Asset | Read at | Curve applied | Verdict |
| --- | --- | --- | --- |
| Ptex `.ptx` colour texels | `crust-assets/src/ptex_texture.rs` (`decode_ptex_slice`), requested `g22_rec709` by `usd_import/materials.rs` (`material_ptex`) | flat 2.2 | ✅ intentional (island convention) |
| Ptex `.ptx` displacement texels (`PxrDisplace` → `PxrPtexture`, `PxrBlend` multiply) | `usd_import/materials.rs` (`pxr_displacement`) requests `Raw` → `crust-assets/src/ptex_texture.rs` / `ptex_stream.rs` (`decode_ptex_slice`, and the streamed conversion resolved per file) | none (identity; no clamp, so a float height may be negative) | ✅ correct — a height is data, and `PxrPtexture.linearize` defaults to 0 |
| UV texture tagged `srgb_texture` | `crust-assets/src/uv_texture/` (`TransferCurve::to_linear_table`) | piecewise sRGB | ✅ correct per MaterialX |
| UV texture tagged `g22_rec709` | `crust-assets/src/uv_texture/` | flat 2.2 | ✅ correct per MaterialX |
| UV texture tagged `g18_rec709` | `crust-assets/src/uv_texture/` | flat 1.8 | ✅ correct per MaterialX |
| UV texture tagged any other OCIO space (`acescg`, `g22_ap1`, `lin_rec2020`, `srgb_displayp3`, …) | `crust-assets/src/uv_texture/` | that space's curve, then its primaries → working | ✅ correct per MaterialX |
| UV texture, untagged, or a MaterialX data image (`float`, `vector*`) | `crust-assets/src/uv_texture/` | none (pass-through) | ✅ correct — already in the working space, or data (normals, roughness, masks) |
| `UsdUVTexture` whose `inputs:file` carries `colorSpace` metadata | `usd_import/preview.rs` (`preview_uv_input`) | that space, over `sourceColorSpace` | ✅ the file names its space outright |
| `UsdUVTexture`, `sourceColorSpace = "sRGB"` | `usd_import/preview.rs` (`preview_uv_input`) → `uv_texture/` | piecewise sRGB | ✅ correct per the node set |
| `UsdUVTexture`, `sourceColorSpace = "raw"` | same | none (pass-through) | ✅ correct per the node set |
| `UsdUVTexture`, `auto` or unauthored | same, resolved by `ColorSpace::resolve_auto` at open | piecewise sRGB for 8-bit RGB/RGBA, none otherwise | ✅ the UsdUVTexture rule (Hydra's) — note the default is `auto`, **not** raw as in MaterialX |
| `UsdUVTexture` feeding `UsdPreviewSurface.inputs:displacement`, `auto` or unauthored | `usd_import/preview.rs` (`preview_displacement` → `preview_uv_input` with `data`) | none (requested `Raw`) | ✅ a height is data, so the 8-bit-RGB-is-sRGB rule does not apply; an explicit `sRGB` still wins |
| Preloaded `.exr` UV texture | `crust-assets/src/uv_texture/` (`decode_exr_tile`) | none under `auto`/`raw`; an explicit space is converted once, curve and primaries, in `f32`, at load | ✅ stored as working-space `f32` — no table, no clip |
| MaterialX emission `image`, untagged | `crust-assets/src/uv_texture/` / `tiled/` | none (pass-through) | ✅ correct — an EDF's colour is radiance, and a float file is scene-linear |
| LDR env image (PNG/JPG/…) | `crust-assets/src/environment.rs` (`decode_pixels`) | piecewise sRGB (`auto`) | ✅ correct per format |
| `.hdr` env image | `crust-assets/src/environment.rs` (`is_hdr`) | none (pass-through) | ✅ correct — HDR is scene-linear |
| `.exr` env map | `crust-assets/src/environment.rs` | none (pass-through) | ✅ correct — EXR is linear |
| any env / `RectLight` image whose `texture:file` carries `colorSpace` metadata | `usd_import/lights.rs` (`texture_color_space`) → `decode_pixels` | that space | ✅ |
| LDR `RectLight` `texture:file` (PNG/JPG/…) | `crust-assets/src/lib.rs` (`load_light_texture` → `read_rgb_image`) | piecewise sRGB | ✅ same decoder as an LDR env image |
| `.hdr` / `.exr` `RectLight` `texture:file` | same | none (pass-through) | ✅ correct — kept as authored, never narrowed to 8 bits |
| Streamed `.tx`, TIFF backing (`u8` tiles) | `crust-assets/src/tiled/cache.rs` (`Tile::rgb`) | the tagged curve, per lookup | ✅ same table as the preload path, by construction |
| Streamed `.tx`, EXR backing (`half` tiles) | — | none (the curve was applied once at conversion; the primaries are the source's, converted per lookup) | ✅ correct — the file stores linear samples and records which space they came from |

The `is_hdr` flag in `decode_image_pixels` exists because `image`'s `to_rgb32f`
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
curve never runs per texel (`quantize_is_the_rounded_encode`). Both tables are
the curve alone, so the stored levels are in the file's own primaries whatever
the working space. Every curve takes a `ResolvedColorSpace`, which has no
`auto`: `ColorSpace::resolve_auto` is the only way from the requested space to
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

The engine produces a linear `Buffer` in the working space. The CLI writes it
two ways (`crust-render/src/main.rs`, the one place that still touches pixels;
[The outputs](#the-outputs) above says how each records its space):

- **`.exr`** — the linear values, unmodified. This is the render output.
- **`.png`** — through the OCIO config's display / view
  (`color::encode_preview`, called from `tone_map` in `main.rs`): by default
  `sRGB - Display` with `Un-tone-mapped`, a clamp to `[0,1]` and the piecewise
  sRGB curve and nothing else. A preview, not a deliverable.

With the default view, `tone_map` is the *inverse* of the LDR-image decode
above: the same curve in the forward direction (to within a matrix round trip
through the config's reference spaces; no 8-bit code differs from
`lin_rec709 → srgb_texture`). Comparing renders numerically should
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

**1. `UsdPreviewSurface` colours are taken as authored in the working space.**
`diffuseColor` and `emissiveColor` land in `OpenPBR` unconverted unless their
attribute carries `colorSpace` metadata (`usd_import/preview.rs`,
`preview_surface_openpbr`). That is the working-space rule, and USD's own colour
management agrees with it; but values picked from a DCC swatch without
metadata — the common case for this schema — are display-encoded, and render
too bright. The `UsdPreviewSurface` spec does **not** mandate a colour space for
these inputs, so decoding them as sRGB by default would be a convention, not a
standards requirement. The OpenSpec change
`openspec/changes/add-material-color-management/` proposes it.

**2. Nothing forces a new colour attribute to name its space.** There is one
implementation of every conversion (`crust-core/src/color.rs`), and the
importer's colour reads go through `attrs::in_working` / `custom_color`, but a
newly added colour attribute read through `custom_color3` gets no conversion
and no compile error. The `RawColor3` newtype of
`openspec/changes/add-material-color-management/` would make it one.

**3. USD's own fallback colour space is not applied.** With nothing authored
on the attribute, its prim or any ancestor, `ComputeColorSpaceName` answers
`lin_rec709_scene`; crust takes such a value as already in the working space
instead (the rule above, and Typhoon's and Karma's reading). The two agree in
the default `lin_rec709`, and differ only for unauthored colours rendered in
another space.

**4. A conversion that is not a curve and a matrix is refused.** A space whose
optimised OCIO processor contains a 3D LUT, or a matrix before its curve, is
used as stored, with one warning per pair. Every texture space of the ACES
configs has the supported shape; a custom config with LUT-based input
transforms does not.

**5. A `.tx` is matched to its binding by source space name.** A `.tx`
converted under `raw` and one converted under `lin_rec709` hold identical
levels — neither has a curve — but their `crust:mipspace` markers differ, so
binding one under the other's space declines streaming and preloads instead.
Safe, not optimal.

## Diagnosing a suspected colour-space bug

A wrong transfer curve produces a plausible image, so appearance proves
nothing. Two switches answer in numbers instead:

```bash
# Is the surface's colour coming from the texture or the constant fallback?
# CRUST_PTEX=0 declines every Ptex texture; surfaces fall back to baseColor.
CRUST_PTEX=0 cargo run --release -- render -i scene.usda -o out.exr

# What are the actual texel values, before and after decode?
cargo run --release -p crust-render --example tex_probe -- texture.ptx
cargo run --release -p crust-render --example tex_probe -- render.png [x0 y0 x1 y1]

# Is it the decode, or the mip level it is being read at? These turn off the
# filtering without touching the decode, so a difference that survives them is
# a colour-space question and one that does not is a filtering question.
CRUST_TEX_MIP=0 CRUST_PTEX_MIP=0 cargo run --release -- render -i scene.usda -o out.exr
CRUST_RAY_CONES=0 cargo run --release -- render -i scene.usda -o out.exr
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
