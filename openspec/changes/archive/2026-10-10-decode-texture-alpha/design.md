# Design

## Context

See proposal.md (Why) for the failure. The shape of the code it meets:

- **The seam already carries alpha.** `Texture2D::eval` (`crust_mtlx::Texture`)
  returns `[f32; 4]`, and `UvInput::scalar` reads `s[3]` for `TexOutput::A`.
  Nothing in crust-core changes shape; the import's cutout
  (`preview_surface_material` → `PreviewSurface::with_cutout`) already samples
  and thresholds whatever `a` the host returns.
- **The host drops it in four places.** `decode_tile` narrows through
  `to_rgb8`; `make_tx` too, so `--auto-tx` and `maketx` write RGB; the `.tx`
  readers normalise every tile to three channels (`to_rgb`, `resolve_rgb`); and
  the shared filter writes `a = 1` (`Taps::blend_rgb`, `lerp_rgba`).
- **Two pairs must not move.** Streamed ↔ preloaded `u8` textures are pinned
  bit for bit (`streaming_and_preloading_agree_bit_for_bit`), and
  `reduce_half` ↔ `reduce_half_linear` share `axis_taps`. The RGB path's
  instruction count is guarded too: the textures record measured what a
  per-texel payload decision costs ("the second backing must cost the first
  one nothing", +21% wall clock on an 8-bit render).
- **No checked-in sample texture has alpha** (every PNG is grey or RGB), so a
  change that leaves RGB textures alone moves no golden.

## Goals / Non-Goals

**Goals:**

- `outputs:a` reads the file's alpha, preloaded and streamed, at every level.
- A texture without alpha reads 1.0 and holds and samples exactly what it did:
  same bytes, same bits, no measurable cost.
- Streamed ↔ preloaded bit-identity extends to alpha for 8-bit sources.

**Non-Goals:**

- Premultiplied filtering of the colour. It would move every RGBA texture's
  colour chain; a cutout reads only alpha.
- Un-premultiplying associated alpha (OIIO `maketx`'s TIFF default, EXR's
  convention). Recorded as a gap.
- Ptex alpha. `PtexTexture::eval` returns RGB and nothing asks for more.
- Opacity on `UsdPreviewSurface` translucency (`opacityThreshold = 0`) is
  unchanged: a textured `opacity` there reads `a` the same way, through the
  existing path.

## Decisions

### D1. Alpha is a fourth channel only where it cuts

A tile is RGBA when its file's alpha is authored and **not 1.0 at every
texel** (`drop_opaque_alpha`); otherwise RGB, as before. Most RGBA PNGs never
use their alpha, and a channel that reads 1.0 everywhere would cost a third more
memory to answer what an absent one answers. It also keeps every existing
texture's storage byte for byte, which is what makes "unchanged" provable rather
than measured. The decision is per tile (per UDIM file), because a set's tiles
may differ; `make_tx` applies the same rule, so a converted tile matches its
preloaded twin.

*Alternative:* always decode RGBA. Simpler, but a third more memory and texel
traffic for every RGBA texture, and every RGB texture moves to a four-channel
code path.

### D2. Alpha is coverage

A byte reads `a / 255` through one `const` table (`ALPHA_U8`, equal to `raw`'s
decode table), a float as stored. The colour space's curve and primaries apply to
RGB only (`decode_rgb_of_rgba` wraps the float decodes). Mip levels and the
`CRUST_TEX_MAX` resize average alpha on its own, re-encoding a byte through
`ALPHA_STEPS` (`raw`'s code steps). The colour is not premultiplied, so the
colour channels' arithmetic is exactly the RGB path's and their bits do not
change.

### D3. The alpha decision rides the existing per-lookup dispatch

`UvTexture::eval` already matches its storage once per lookup; it now matches
`(storage, alpha)` into four monomorphisations of `eval_tiles::<T, ANY_ALPHA>`.
`StreamingTexture::eval` keeps its `linear` test for a texture without alpha and
sends one with alpha, through a single test, to an out-of-line `eval_alpha`
(see Measurements for why). With no alpha anywhere, the tile (or chart) is
never asked and the RGB sampler runs as it did.
With some alpha, the tile found says which `TileSource::<T, ALPHA>` /
`ChartSource::<HALF, ALPHA>` reads it. The RGBA fetch is a separate function
(`texel_rgba`, `Tile::rgba_u8` / `rgba_half`) rather than a branch inside the RGB
one, so the RGB closure LLVM inlines into `with_tile` is untouched.

### D4. Where alpha comes from

- `image`: `color().has_alpha()`, except a TIFF whose `ExtraSamples = 0`
  `image_file` rewrote to alpha only to dodge the `tiff` 0.11.3 decode bug: the
  rewrite now reports itself (`decode_with_alpha`), and an unspecified sample is
  not coverage.
- EXR: the channel whose base name is `A`, case-insensitively, like R/G/B —
  `try_read_exr_texels` (preload) and `resolve_alpha` (stream) agree.
- `.tx` TIFF: the first `ExtraSamples` entry is 1 or 2, after one (grey) or
  three (RGB) colour samples (`alpha_sample`). `tiff` reports grey + alpha as
  `Multiband`, so the tags are read directly.

### D5. The writers

`write_tx_rgba` writes four samples with `ExtraSamples = 2` (unassociated: the
colour is stored as given, which is what the sampler reads).
`write_tx_exr_rgba` adds an `A` channel. Both reduce through the same
`reduce_half` / `reduce_half_linear` as the preload, now over an `alpha` flag.
`write_tx` / `write_tx_exr` keep their RGB signatures.

### D6. The warning is retired, and `crust-check` goes to `/2`

`preview.texture_alpha` described an approximation that no longer exists. A
file without alpha reading 1.0 is the node set's specified behaviour, not an
approximation, so the code is removed rather than kept for that case. The
`scene-warnings` rule makes removing a code bump every report that carries
warnings; only `crust-check` does, so it becomes `crust-check/2` with the same
shape.

*Alternative:* keep the code listed but never raised. The `scene-warnings`
spec forbids a reference that lists a code crust cannot raise.

## Risks / Trade-offs

- **A MaterialX `color4` / `vector4` image now reads real alpha.** That is
  MaterialX's definition, and no checked-in document reads an RGBA file's
  fourth channel; a document that relied on it being 1.0 changes.
- **Colour bleed at soft edges** under minification (D2's non-premultiplied
  average), recorded as a gap.
- **Report consumers** matching `format == "crust-check/1"` must accept `/2`.

## Measurements

Callgrind, one thread, `-s 2`, against the base binary. Streamed scenes are a
copy of the sample with `--auto-tx`'s RGB `.tx` beside each texture.

| scene | backing | whole render | `eval` (self) | |
| --- | --- | --- | --- | --- |
| `materialx_basic` | preloaded | +0.005% | 137,515,376, unchanged | `reduce_half` 2.12 M → 2.16 M |
| `materialx_basic` | streamed | +0.047% | +0.82% (2.7 instr a lookup) | `texel::<false>` unchanged |
| `usdpreview_textured` | preloaded | +0.012% | +0.18% (`f32` EXR albedo) | |
| `usdpreview_textured` | streamed | +0.021% | +0.39% | `texel::<false>` unchanged |

Two first versions were worse, and changed to D3's final shape:

- `StreamingTexture::eval` as a four-arm `match (linear, alpha)`, every sampler
  inlined: +5.9 instructions a lookup. The alpha samplers moved behind one
  `alpha` test into an `#[inline(never)]` `eval_alpha`: +2.7.
- `reduce_half` reading its stride at run time: the RGB pyramid build +33% at
  load (2.12 M → 2.83 M). A `const N` per channel count: 2.16 M.

Images: every sample scene bit-identical (`scripts/check_images.sh` recorded with
the base binary, checked with this one; the two Kitchen_set scenes are not in the
checkout). The issue's reproduction (`texture_alpha.rs`) fails on the base commit
with opacity 1.0 on the transparent half, and passes here.
