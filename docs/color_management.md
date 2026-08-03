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
(`usd_import.rs`), but decodes no **asset** — every image and Ptex decoder lives
in the host (`crust-render/src/main.rs`), reached through `AssetLoader`. The
`PtexTexture` trait pins the contract at that boundary
(`crust-core/src/texture.rs:32`): values returned from `eval` are linear, not
display-encoded, so the host must have decoded them already.

## Two transfer curves, deliberately

There are two decode curves in the codebase, and they are not
interchangeable:

| Curve | Formula | Where |
| --- | --- | --- |
| **Piecewise sRGB EOTF** | `c ≤ 0.04045 ? c/12.92 : ((c+0.055)/1.055)^2.4` | LDR environment images (`main.rs:121-129`) |
| **Flat gamma 2.2** | `max(c,0)^2.2` | `PxrDisneyBsdf.baseColor` (`usd_import.rs:2862`), Ptex texels (`main.rs:251`) |

The flat curve is *not* a sloppy approximation of the standard one — it is
matched to what the source content actually applies. The Moana island's shading
networks run Ptex colour through a `PxrColorCorrect` gamma-1/2.2 node, and its
GL path declares `sourceColorSpace = "sRGB"`; reproducing the reference render
matters more there than conforming to the sRGB standard. Both decisions carry
that reasoning in a comment at the call site (`usd_import.rs:2856-2861`,
`main.rs:245-250`).

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
genuine bug in the table (see the Verdict column).

### Material shader inputs

| Input | Read at | Curve applied | Verdict |
| --- | --- | --- | --- |
| `UsdPreviewSurface.diffuseColor` | `usd_import.rs:2651` | **none** | ⚠️ **bug** — see [Known gaps](#known-gaps) |
| `UsdPreviewSurface.emissiveColor` | `usd_import.rs:2665` | **none** | ⚠️ **bug** — same |
| `PxrDisneyBsdf.baseColor` | `usd_import.rs:2740` | flat 2.2 | ✅ intentional (island `PxrColorCorrect`) |
| `crust:openpbr` — all 7 colour fields[^1] | `usd_import.rs:2886` | **none** | ✅ intentional — native format is linear-authored |

[^1]: `baseColor`, `specularColor`, `transmissionColor`, `subsurfaceColor`,
`fuzzColor`, `coatColor`, `emissionColor` — all via the `c` closure at
`usd_import.rs:2886`, fields assigned across `2891-2953`.

`crust:openpbr` is crust's own lossless 1:1 mirror of the `OpenPBR` struct, so
values are authored in the renderer's working space by definition. That makes
"no conversion" correct — but note it is achieved by *not calling anything*,
not by a stated decision.

### Lights

| Input | Read at | Curve applied | Verdict |
| --- | --- | --- | --- |
| `inputs:color` (all four lux types[^2]) | `usd_import.rs:2240` via `attr_color3f:3168` | **none** | ✅ correct — see below |

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
**not** double-decoded: `usd_import.rs` only resolves the path and hands it to
`AssetLoader::load_environment`, then multiplies the already-linear map by the
already-linear tint.

### Volume coefficients

| Input | Read at | Curve applied | Verdict |
| --- | --- | --- | --- |
| `crust:volume:sigmaS` | `usd_import.rs:482` via `custom_color3:3130` | **none** | ✅ correct |
| `crust:volume:sigmaA` | `usd_import.rs:483` | **none** | ✅ correct |
| `crust:volume:emission` | `usd_import.rs:484` | **none** | ✅ correct |

These are crust-custom attributes (no upstream schema to defer to) holding
*physical quantities* — scattering and absorption cross-sections, and emitted
radiance. They are authored directly as numbers, never picked from a colour
swatch, so there is no display encoding to undo. Same reasoning as
`crust:openpbr`.

### Textures and environment maps

| Asset | Read at | Curve applied | Verdict |
| --- | --- | --- | --- |
| Ptex `.ptx` colour texels | `main.rs:251` | flat 2.2 | ✅ intentional (island convention) |
| LDR env image (PNG/JPG/…) | `main.rs:121-129` | piecewise sRGB | ✅ correct per format |
| `.hdr` env image | `main.rs:122` | none (pass-through) | ✅ correct — HDR is scene-linear |
| `.exr` env map | `main.rs:78` | none (pass-through) | ✅ correct — EXR is linear |

The `is_hdr` branch at `main.rs:117-120` exists because `image`'s `to_rgb32f`
rescales integer formats into `0..1` *without* removing their transfer curve,
while leaving true HDR values as authored — so the decode must be conditional
on the format, not applied blanket. Pinned by
`ldr_images_are_converted_to_linear` (`main.rs:692`).

Ptex texels are decoded once at load into the preloaded immutable buffer, not
per lookup — which is also why `CRUST_PTEX_MAX_LOG2`'s mip cap and the decode
share the same pass.

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
(`crust-render/src/main.rs`):

- **`.exr`** — the linear values, unmodified. This is the render output.
- **`.png`** — tone-mapped: clamp to `[0,1]`, then encode with the piecewise
  sRGB OETF (`tone_map`, `main.rs:460-468`). A preview, not a deliverable.

`tone_map` is the *inverse* of the LDR-image decode above, using the standard
piecewise curve in the forward direction. Comparing renders numerically should
always use the EXR (`examples/exr_diff`), never the PNG, since the PNG has both
clamped and re-encoded.

## Known gaps

**1. `UsdPreviewSurface` colours are not decoded.** `diffuseColor` and
`emissiveColor` land in `OpenPBR` raw (`usd_import.rs:2651`, `2665`). Values
authored as DCC colour-picker swatches — the normal case for this schema — are
therefore used as if already linear, overshooting albedo substantially. Note
for honesty: the `UsdPreviewSurface` spec itself does **not** mandate a colour
space for these inputs (its only explicit colour-space note concerns normal
maps), so treating them as sRGB is a defensible convention rather than a
standards requirement — but "no conversion at all" is not a defensible reading
of any convention.

**2. No shared abstraction, so nothing is enforced.** There is no `ColorSpace`
type. The two curves exist as independent inline implementations
(`usd_import.rs:2862`, `main.rs:251`) that happen to agree. Nothing forces a
newly added colour input to state its source space; the default behaviour of
adding one is to get gap #1 again, silently.

**3. Per-attribute colour-space authoring is unsupported.** A USD attribute
carrying an explicit `colorSpace` metadatum is ignored; the curve is chosen by
shader family, not by what the asset declares.

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
```

`tex_probe` exists specifically to settle whether a texture is
display-encoded or linear: it prints raw values, so the overshoot signature of
a missing decode is visible as a number rather than guessed from a render.
Rule of thumb — a *linear* albedo should sit well below its authored value
(18% grey encodes to ~0.46), so a linear diffuse colour that still reads
0.5–0.9 across all three channels on a natural material is the usual tell that
a decode was skipped.

See also `docs/openpbr_reference_alignment.md` for how the OpenPBR parameters
these colours feed are defined, and the "Ptex" section of `CLAUDE.md` for the
face-addressing checks that are orthogonal to (and easily confused with)
colour-space correctness.
