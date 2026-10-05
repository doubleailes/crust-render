# Image output — design record

What `crust-render` writes and why it is written that way. The behaviour is in
`spec.md` beside this file; the AOV vocabulary (what each channel *means*) is
the `aovs` capability's.

## Two writers, chosen by the stage

- **No `RenderProduct`.** `write_beauty` in `crust-render/src/main.rs`:
  `exr::prelude::write_rgb_file` at `-o` (default `output.exr`), then the PNG
  beside it. This is the path every sample, golden and test without products
  takes, and it is kept exactly as it was before products existed, so their
  output did not move.
- **Products authored.** `crust-render/src/products.rs`: one EXR per accepted
  product, at its `productName` (or `-o` for the first), then the PNG from the
  first product's beauty, beside that product.

Moving the no-products case to the product writer (scanline, tagged with
`colorInteropID`) is a deliberate follow-up, to land with re-recorded goldens:
it changes every file's encoding without changing a pixel.

### The trap: "byte-identical" EXRs are not

Two runs of the *same* binary on `samples/cornellbox.usda` write EXRs that
differ from byte 391 on, with every pixel identical and the same file size:
the `exr` crate compresses blocks in parallel and writes them in completion
order (the offset table records where each landed). So the no-products guarantee
is "same writer, same encoding, bit-identical pixels", checked with
`examples/exr_diff`, and the PNG — encoded serially — *is* byte-identical. A
`cmp` of two EXRs proves nothing either way.

## The product writer

- **One part, scanline, ZIP16.** Multi-part has weaker reader support and
  nothing here needs per-part compression. Tiled — the `exr` crate's default —
  crashes tinyexr (`docs/material_fidelity.md`), which is why the product
  writer sets `Blocks::ScanLines` explicitly.
- **Channels through `AnyChannels::sort`.** EXR stores channels alphabetically
  and readers find them by name; an unsorted list is a malformed file.
- **Names** follow `<layer>.<component>` and the ASWF Color Interop rule that
  only colour gets `R/G/B`: colour `R/G/B[/A]`, vectors `X/Y/Z`, UVs `U/V`, a
  scalar one channel named after its layer. The first beauty var of a product
  is bare, so viewers show it as the image. Two vars that would write the same
  name are not both written (`product_channels` refuses the second with a
  `WARN`) — `exr` would otherwise reject the whole file.
- **Precision** per var: HALF, FLOAT, or UINT with `-1` as `0xFFFFFFFF`.
  Accumulation is always f32; the conversion happens at write.
- **Header.** `software = crust-render <version>`; `colorInteropID =
  lin_rec709_scene`, crust's one rendering space; the product's
  `driver:parameters:*` text values. `exr` refuses a standard attribute name
  as a custom one, so `comments`/`owner` go to their typed fields and any
  other standard name is refused with a `WARN` rather than failing the write.
  A forwarded `colorInteropID` is refused too: it describes the pixels, and
  only crust knows what space it wrote them in.
- **Rows** are top-down in the file, the film's rows bottom-up — the same flip
  `Buffer::get_rgb` does, applied once in `AovFilm::var_channels`.
- **Paths.** `productName` as authored, relative to the working directory as
  husk and usdrecord resolve it; parent directories are created.

## `-o` with products

`-o` replaces the first product's `productName` (husk's rule) and leaves the
others alone. `scripts/material_fidelity/run.py` passes `-o` to fixtures that
author one product, so their output path did not change when products started
being honoured. `scripts/check_images.sh` passes `-o` too: on a stage with
several products (`samples/aovs.usda`) the later ones are written to their own
`productName`s under the working directory — `/renders` is ignored by git for
that reason.

A product with no `productName` and no `-o`, or with no var crust can write,
is skipped with a `WARN`, as is a product whose path an earlier product
already writes (compared lexically, after `-o` is applied — the second write
would replace the first file and lose its channels); if every product is skipped, the beauty goes to `-o`
as if none were authored, so a render never silently writes nothing.

## Logging

One `INFO` line lists the products written (bounded by the stage's product
count, which is per render, not per prim of the scene); the channel list of
each file is `DEBUG`.

## Known gaps

- The no-products EXR is still the `exr` crate's default encoding (tiled,
  untagged); see above.
- No deep output, no display drivers, no progressive writes.
- The PNG is always the first product's beauty, clamped and sRGB-encoded; no
  view transform is applied.
