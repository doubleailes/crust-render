# Design

## Context

See proposal.md for why. Here is the current state this design replaces, and what was
measured against Arnold 7.5.0.0 (OIIO "2.6.3.0dev", `C:\Program Files\Arnold-7.5.0.0-windows\bin`)
with crust's own OCIO config (`ocio://cg-config-v4.0.0_aces-v2.0_ocio-v2.5`) and
rendering space ACEScg.

**Crust today** (`crates/crust-assets/src/tiled/`, design record "Streaming textures" in
`openspec/specs/textures/design.md`):
- The lookup is `tx_sibling(src) = src.with_extension("tx")`.
- An 8-bit source becomes a u8 TIFF that keeps its encoding. Its levels are reduced in
  linear light and re-encoded (`reduce_half`), and the curve is applied per lookup
  through a 256-entry table.
- A float source becomes a half EXR in the source's primaries (`reduce_half_linear`).
- `crust:mipspace` and `crust:txversion` are written to `ImageDescription` or an EXR
  attribute, and a mismatch is refused.
- The pinned invariant: streamed and preloaded u8 textures are bit-identical.

**Arnold's auto-tx, as measured.** `scripts/arnold_tx_oracle.py` renders each case with
`kick` and `texture_auto_generate_tx on`, one `image` node per `color_space` and one
source per case, and reads every header with `oiiotool --info -v`. Everything below is
in `crates/crust-assets/tests/data/arnold_tx/cases.json`, beside the sources and the
`.tx` files Arnold wrote. The source is 8-bit RGB unless the row says otherwise.

| `color_space` on the node | file name | stored | conversion |
|---|---|---|---|
| `sRGB Encoded Rec.709 (sRGB)`, `srgb_tx`, `srgb_texture`, `auto` | `x_sRGB Encoded Rec.709 (sRGB)_ACEScg.png.tx` | half EXR, DWAA (level 45), `roundingmode 0` | sRGB → ACEScg |
| `sRGB` (a name this config does not define) | `x_sRGB_ACEScg.png.tx` | half EXR, DWAA | **none**: the convert is an identity, the stored values are the source's bytes / 255 |
| `Linear Rec.709 (sRGB)` | `x_Linear Rec.709 (sRGB)_ACEScg.png.tx` | u8 TIFF, zip | Rec.709 → ACEScg, requantised to 8 bits |
| `Raw` (OCIO space) | `x_Raw_ACEScg.png.tx` | u8 TIFF, zip | identity |
| `raw` (Arnold's keyword) | `x_raw.png.tx` | **half EXR, DWAA** | none |
| `ACEScg` (= target), `""` | `x_raw.png.tx` | u8 TIFF, zip | none |
| `auto`, 8-bit **single-channel**, or 16-bit | the sRGB name | half EXR, zip, one `Y` channel (8-bit); uint16 TIFF (16-bit) | sRGB → ACEScg (crust's previous `auto` read both raw) |
| `auto`, half or float EXR | `x_raw.exr.tx` | the source's type, zip | none |
| any, 16-bit source (grey or RGB) | as above | **uint16 TIFF**, zip, even under sRGB | as above |
| `""` / `raw` / `auto`, float EXR | `x_raw.exr.tx` | **float** EXR, zip | none |
| `Linear Rec.709 (sRGB)` / sRGB, float EXR | `x_<C>_ACEScg.exr.tx` | float EXR, zip | converted (sRGB's curve runs on values above 1 too) |
| any, half EXR | as above | half EXR, zip | as above |

So the sample type is the source's, except that an **8-bit** source bound in a space
Arnold does not treat as linear (a curve, a name the config does not define, or the
`raw` keyword) becomes half. Every other file property in the table, measured on the
same fixtures:
- A half EXR with one channel is zip, not DWAA, though the command line still asks for
  DWAA.
- An image whose three channels are equal everywhere is stored as one `Y` channel.
- An alpha that is 1 everywhere is dropped.
- A constant image larger than a tile is stored as one 64x64 tile and carries
  `oiio:ConstantColor`.
- Levels are rounded down: 37x23 → 18x11 → 9x5 → 4x2 → 2x1 → 1x1.
- Alpha is stored **premultiplied** (associated), in both TIFF and EXR.
- `fovcot` is width / height.
- `oiio:AverageColor` is the **source's** mean (premultiplied, before any resize) put
  through the conversion as one colour, not the mean of the stored texels. Rec.709 →
  ACEScg of `eotf(0.496)` is the 0.2097 Arnold records for a texture whose stored
  level 0 averages 0.307.
- The modification time is the source's **truncated to whole seconds**, not equal to
  it.
- Header keys:
  - EXR: `autotx`, `compression`, `fovcot`, `oiio:AverageColor`, `oiio:ColorSpace
    "Linear"`, `oiio:SHA-1`, `openexr:dwaCompressionLevel`, `openexr:roundingmode`,
    `textureformat "Plain Texture"`, `wrapmodes "black,black"`, `Software`, and
    `Exif:ImageHistory` equal to `Software`.
  - TIFF: `Exif:ColorSpace 1`, `oiio:ColorSpace "sRGB"` (OIIO's TIFF reader reports
    that for every u8 and u16 file, converted or not) and no `autotx`.

**How Arnold's USD procedural names a binding** (the `usd` section of `cases.json`):

| binding | Arnold's `.tx` |
|---|---|
| UsdUVTexture `sourceColorSpace = "sRGB"` | `x_sRGB_ACEScg.png.tx`, **unconverted**: Arnold passes the token to `image.color_space` as is, and renders the texture undecoded, the same as `raw` |
| UsdUVTexture `"raw"` | `x_raw.png.tx`, half DWAA |
| UsdUVTexture `"auto"` or unset | the canonical sRGB name |
| `colorSpace` metadatum `srgb_texture` or the canonical name, UsdUVTexture or MaterialX | the canonical sRGB name |
| MaterialX `colorspace = "lin_rec709"` | `x_Linear Rec.709 (sRGB)_ACEScg.png.tx`, u8 TIFF |
| MaterialX `colorspace` unset | the canonical sRGB name: Arnold reads unset as `auto`, crust as the working space |

The command line is `maketx <src> --opaque-detect --constant-color-detect
--monochrome-detect --fixnan box3 --oiio --attrib tiff:half 1 --unpremult -u
[--format exr -d half --compression dwaa] [--colorconvert <C> <T>] --attrib autotx 1 -o <dst>`,
stored in `Software` as `OpenImageIO-Arnold 2.6.3.0dev : maketx …`. The paths are the
ones Arnold resolved, with forward slashes, and quoted when they contain a space. They
are not canonicalised: a scene path given in 8.3 short form stays short. `<C>` is
quoted when it contains a space and is spelled as the name says (`sRGB` for the USD
token). The float rows carry no `--format exr -d half --compression dwaa`.

**How Arnold reads a `.tx` bound directly.** A constant u8 sRGB source `(0.8, 0.4, 0.1)`
is rendered through `flat` into an ACEScg EXR (`direct_binding` in `cases.json`).

| binding | result |
|---|---|
| source, sRGB | 0.416 0.164 0.036 (reference) |
| Arnold's converted `.tx`, bound sRGB | 0.096 0.031 0.008: converted again |
| the same file renamed `renamed.tx`, bound sRGB | 0.096 …: the name is ignored |
| converted `.tx`, `""`, `auto` or `ACEScg` | 0.416 … |
| plain u8 `.tx` (no conversion), bound sRGB or `auto` | 0.416 …: converted per lookup |
| plain u8 `.tx`, bound `""` | 0.8 0.4 0.102 |

**When Arnold rebuilds a `.tx`** (`rebuild` in `cases.json`; each probe edits Arnold's
own file, restores its time and renders again). Kept: unchanged. Rebuilt:
- `Software`'s prefix changed to `crust-render : maketx …`;
- one flag changed;
- `Software` empty;
- the time one second newer **or** older than the source's.

So `-u` compares the whole `Software` string, prefix included. A rebuild over an
existing `.tx` fails on Windows ("Could not rename file: Access denied": Arnold holds
the old file open). Arnold then falls back to the source and, with `abort_on_error`,
aborts the render. A crust file Arnold judges stale therefore breaks an Arnold render
on Windows; it is not merely rewritten.

## Goals / Non-Goals

**Goals:**
- A file crust writes is one Arnold 7.5 accepts as its own: it is not rebuilt, and it
  renders the same within tolerance. Arnold's files are read by crust under the same
  rules Arnold applies.
- One conversion in the workspace, shared by `--auto-tx` and `examples/maketx`, as
  today.

**Non-Goals:**
- Byte-identical files. DWAA output, SHA-1 inputs and float rounding may differ from
  OIIO's.
- Matching Arnold versions other than 7.5 / OIIO 2.6.3. The flag string is pinned to
  what 7.5 emits.
- Keeping crust's previous `.tx` convention, its markers, or an environment switch back
  to it. Such a switch would A/B a convention, not an optimisation, which is not what
  `CRUST_*` switches are for.
- Ptex. It is untouched and still never converted.

## Decisions

### 1. The name is computed, never parsed

Stems and colour-space names both contain `_` and spaces, so splitting a file name is
ambiguous. The lookup builds the single candidate from the source path, the binding
and the target. The source path keeps any UDIM token, since the stem keeps it.

- **The `_raw` case.** It applies when the authored string is unset or empty, is the
  literal keyword `raw` (before alias resolution, because Arnold's keyword `raw` and
  the OCIO space `Raw` name different files), or when the resolved space equals the
  target. `auto` is resolved first, by Arnold's rule (decision 8): an integer source
  becomes the sRGB space and a float one `_raw`.
- **Otherwise** the name is `_<C>_<T>`, where C is the OCIO config's canonical name for
  the resolved space. The colour module gains an accessor for the config's own name,
  next to `Space::label`, which returns crust's short label.
- **UsdUVTexture's `sRGB` token** resolves to the sRGB space, as the schema says, so its
  name is the canonical `_sRGB Encoded Rec.709 (sRGB)_<T>`. Arnold names the same
  binding `_sRGB_<T>` and stores it unconverted (Context), because this config defines
  no `sRGB`. Crust never reads such a file: the two renderers do not share `.tx` files
  for this token. That is recorded as a known gap, and the user documentation tells
  Arnold users to author `srgb_texture` instead, which both renderers name the same.
  Decided by the user over two alternatives:
  - Arnold's name with converted data would make Arnold's render of crust's file differ
    from its render of its own.
  - Arnold's reading would render every sRGB UsdUVTexture undecoded.

The space alone does not determine the name or the file type: `""`, the keyword `raw`
and the OCIO `Raw` all resolve to crust's raw space, but give three different files
(Context). So a texture request carries the binding as authored next to its resolved
space.

Considered instead: parsing names, or globbing `<stem>_*_<T>.<ext>.tx`. Either would
pick up stale files made under another config, such as the playground's
`_srgb_tx_ACEScg` files, which hold **unconverted** data written by Arnold's built-in
config.

### 2. One rule for the space a `.tx` is read in

The effective space E is:
- the binding's space for a directly bound `.tx` and for a `_raw` file;
- T for a `_<C>_<T>` file.

The tile payload no longer implies "linear":
- A `u8` tile keeps the 256-entry table, now built for E.
- A `half` tile whose E has a curve is decoded **at page-in**: the tile is converted
  once, when it enters the cache.
- A `half` tile whose E is linear is used as stored.
- The change of primaries E → working stays per lookup (`StreamingTexture::gamut`), as
  today.

The common case, a converted half EXR read in ACEScg, then costs one matrix per lookup
and nothing else. The case that is slow by nature (a curved space over half data) only
arises from a direct binding, and it pays once per tile, not per texel.

Considered instead: a per-lookup curve on half values. That puts a `powf` or OCIO call
in the hottest loop, for a case Arnold users avoid.

### 3. The writer reproduces OIIO's pipeline, in OIIO's order

The steps, in order:
1. Decode.
2. `--fixnan box3`: replace a NaN with the 3x3 box average of its finite neighbours.
3. Detect: opaque (drop an alpha that is 1 everywhere), constant-colour (shrink to
   one tile of that colour, as OIIO does), monochrome (R = G = B everywhere gives one
   `Y` channel).
4. Unpremultiply.
5. Convert C → T with crust's OCIO processor in `f32`.
6. Re-premultiply.
7. Quantise to the output sample type, rounding as OIIO does.
8. Build each level from the previous one with OIIO's box filter at round-down sizes.
   For even sizes that is a 2x2 mean; for odd sizes it uses fractional weights.
9. Write with the metadata from the Context table, and set the modification time to
   the source's.

The file type follows the table in Context:
- the source's sample type, in a zip TIFF for 8- and 16-bit and a zip EXR for half
  and float;
- except that an 8-bit source in a space Arnold does not treat as linear becomes a
  half EXR, DWAA for three or four channels and zip for one.

The 8-bit-to-8-bit conversion in step 7 is deliberately lossy, because Arnold's is.

`reduce_half` and `reduce_half_linear` remain for the preloaded pyramid. `.tx` levels
use the new box reduction. Each step is checked against Arnold's `maketx.exe` output on
the same source (task 1 fixtures).

### 4. `Software` holds Arnold's command line verbatim

`-u` skips a rebuild only on an equal time and an equal command line, so crust writes
exactly the string Arnold 7.5 would generate for that binding. That string is pinned
in one function with the flag table above.

The prefix is Arnold's `OpenImageIO-Arnold 2.6.3.0dev : maketx …`. Task 1.3 measured
that `-u` compares the whole string: a file whose prefix is `crust-render` is rebuilt,
and on Windows that rebuild aborts the render (Context). The prefix is interop
metadata, not a claim of authorship; the `.tx` is told apart from Arnold's by nothing,
which is the point.

The path in the string has to be the one Arnold resolves, byte for byte, or Arnold
rebuilds. Crust writes the absolute path it resolved, with forward slashes. That
matches Arnold whenever both open the scene by the same path; a scene opened through
two spellings of one directory (a mapped drive and a UNC path, an 8.3 short name) does
not share files, in Arnold alone as much as between the two.

Considered instead: letting Arnold rebuild once. The user rejected this: crust's file
has to be the one Arnold uses.

### 5. `oiio:SHA-1` and `oiio:AverageColor`

`AverageColor` is what task 1 measured (Context): the mean of the source, premultiplied,
put through the C → T conversion as one colour (unpremultiplied around it when there is
alpha). It is not the mean of the stored level 0. `SHA-1` is a hash over the pixels,
used by OIIO only to share identical textures in its cache.
It is implemented in crate as about 80 lines of safe code checked against the FIPS
180-4 test vectors. No dependency is added; this path is cold. If it fails to equal
OIIO's hash for the same pixels, OIIO only loses that sharing, so that is a recorded
gap, not a failure.

### 6. `--tx-color-space`, default `ACEScg`

It is a scene-shaping flag, shared by `render`, `diagnostic`, `check` and
`light-paths` like `--auto-tx`. It is validated at parse time against the OCIO config.
It is independent of the working space (`RenderSettings.renderingColorSpace`), so a
default `lin_rec709` render still writes and finds the files Arnold uses. It is not a
`CRUST_*` switch, because it is a user setting, not an A/B.

### 7. Tolerance replaces bit identity

The contract the specs reference. These are starting values, which task 1 measures and
records in the textures design record:
- **Streamed vs preloaded lookups, level 0:** mean absolute difference ≤ 0.005 and 99th
  percentile ≤ 0.02 per channel, for values in [0, 1]. For values above 1 the same
  bounds apply relative to the value.
- **Rendered images:** `crust diff --json` at `-s 16`, `--indirect-clamp 0`, relMSE
  ≤ 1e-3 between `CRUST_TEX_STREAM=0` and streamed, on `samples/materialx_basic` and
  one textured UDIM fixture.
- **Crust's `.tx` vs Arnold's `.tx` of the same source, every level:** mean ≤ 0.002 and
  maximum ≤ 0.02. The maximum is loose because two DWAA encoders may quantise
  differently.
- **Arnold renders crust's file vs its own:** a `kick` render of each, same tolerance as
  the rendered-image check, compared with `crust diff`.

Arnold-backed tests run where `ARNOLD_BIN` (or the default install path) holds
`maketx.exe`, `oiiotool.exe` and `kick.exe`, and are skipped elsewhere, as the
existing reference scripts are.

### 8. `auto` and an unset MaterialX `colorspace` resolve as Arnold resolves them

Measured in task 1 (`grey_auto`, `deep_auto`, `deep_rgb_auto`, `half_auto`, and the
`usd_mx_*_unset` cases):
- **`auto`**: an image with integer samples (8- or 16-bit, any channel count) is
  sRGB, and a half or float image is used as stored.
- **A MaterialX image with no `colorspace`** on the input, its node, its nodegraph or
  the document is `auto` when it is a colour image (`color3`, `color4`). A non-colour
  image (`float`, `vector2/3/4`) is never converted.

Crust adopts both. That changes how existing scenes render:
- A greyscale 8-bit roughness PNG under UsdUVTexture `auto` was raw and is now
  sRGB-decoded, as in Arnold. The same map through a MaterialX `ND_image_float` is
  still raw.
- An albedo PNG through MaterialX with no `colorspace` was taken as already in the
  working space and is now sRGB-decoded.

Decided by the user, over keeping crust's previous readings (Hydra's `auto`,
MaterialX's "absent means the working space"). With the previous readings, the two
renderers would neither share these files nor render these bindings alike.

### 9. Textures carry premultiplied colour

Arnold's files store colour premultiplied by alpha (associated), as OIIO does
everywhere. Crust adopts that as the convention for every UV texture lookup, preloaded
and streamed: the colour a lookup returns is premultiplied, in linear light.
- **A source with straight alpha** (PNG, and a TIFF whose `ExtraSamples` is 2) is
  premultiplied once at load, in linear light. That means decode, multiply by alpha,
  re-encode for a u8 store, which is what `reduce_half` then averages.
- **A `.tx` or EXR** is associated already and is read as stored. A `.tx` that declares
  straight alpha is premultiplied when a tile pages in.
- No lookup divides alpha back out, and none multiplies per texel.

The convention changes the colour of every partly transparent texel: a leaf card's
fringe now reads darker, as in Arnold. A texture without alpha, or with an alpha that
is 0 or 1 everywhere it is opaque, reads the same as before wherever alpha is 1. Ptex
is out of scope, as it is for the rest of this change. Decided by the user, over two
alternatives:
- dividing the alpha out of a streamed lookup;
- excluding partly transparent texels from the streamed-vs-preloaded tolerance.

## Risks / Trade-offs

- [Streamed colour textures are cached as half, twice the bytes of u8] → Measure on a
  textured scene with `bench_ab.sh` and `--stats` peak cache bytes, and record the
  result in the textures design record. Don't change `CRUST_TEX_CACHE_MB`; document the
  change in effective capacity.
- [DWAA decode is slower than Deflate, and `exr`'s DWA path has never been exercised
  here] → A tile round-trip test, plus decoding Arnold's own DWAA files in task 1. Time
  the page-in with `--profile`.
- [`exr`'s DWAA output may not be readable by OpenEXR/OIIO, or may differ visibly] →
  Task 1 has `oiiotool` read crust's DWAA file and compares it with Arnold's. If OIIO
  can't read it, stop and decide (zip half EXR, accepting an Arnold rebuild); don't ship
  something unreadable.
- [The flag string is tied to Arnold 7.5, and another version changes it, so Arnold
  rebuilds crust's files] → It lives in one function, with the version in a comment and
  in the design record. A rebuilt file is still correct, only redundant.
- [The UsdUVTexture or MaterialX colour space that Arnold's USD procedural passes to
  `image.color_space` may not be the string crust resolves, so the names would differ
  for the same asset] → Task 1 renders a USD fixture through `kick` and compares names
  for `sourceColorSpace` sRGB / raw / auto / unset and MaterialX `colorspace` set /
  unset. Any difference is fixed in the naming function before anything else is built
  on it.
- [Breaking: existing crust `.tx` files go unused, and `--auto-tx` reconverts every
  texture once] → Stated in the proposal and in the site's documentation. The old
  files are left in place, not deleted.
- [The 8-bit linear → ACEScg requantisation bands, as Arnold's does] → Accepted, since
  that is what Arnold does. Recorded as a known gap.

## Migration Plan

There is no flag-day mechanism: the next `--auto-tx` render converts to the new names,
and a render without it preloads anything without an Arnold-named `.tx` (with the
existing `textures_without_tx` finding from `check` and `diagnostic`). To roll back,
revert the change. Old `<stem>.tx` files are never deleted, so they are still there
for the previous binary.

## Open Questions

- Settled by task 1.1: the literal `raw` keyword gives a half DWAA EXR named
  `x_raw.png.tx`, while `""` gives a u8 TIFF under the same name. Both share one name
  but not one file type; whichever renderer converts first wins, and the other reads it
  correctly either way, since neither is converted.
- Whether OIIO's `oiio:SHA-1` can be reproduced exactly (decision 5). Deferrable: only
  OIIO's cache sharing depends on it.
- Raised by task 1.2 and decided before group 2:
  - UsdUVTexture `sRGB`: canonical name, not shared with Arnold (decision 1).
  - `auto` and an unset MaterialX `colorspace`: follow Arnold (decision 8).
  - Alpha: premultiplied everywhere (decision 9).
