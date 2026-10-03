## Why

Compositors relight diffuse surfaces by splitting each pixel's light into the
light that arrived and the surface colour that tinted it: V-Ray's
`RawLighting × DiffuseFilter = Lighting` (and `RawGI` for indirect light).
Crust's light path expressions can select the diffuse light (`C<RD>[LO]`), but
an LPE selects paths — it cannot remove the surface colour. Dividing in the
compositor goes wrong at edges and on textures: a pixel averages many samples,
and `mean(colour × light) / mean(colour)` is not the light. The division has
to happen per sample, in the renderer, and the routing added by
`add-usd-render-products-and-aovs` (Phase 2) already isolates each lobe's share
of every sample, so the step is small.

## What Changes

- **Raw light AOVs.** A raw AOV is a light path expression whose every path
  starts with a diffuse reflection (`C<RD>…`), divided per camera sample by the
  diffuse colour of the surface the camera ray hit. New canonical sources, with
  V-Ray's names as aliases:
  - `rawLight` (`RawLighting`): direct diffuse light, `C<RD>[LO]`, raw;
  - `rawGI` (`RawGI`): indirect diffuse light, `C<RD>.+[LO]`, raw;
  - `rawTotalLight` (`RawTotalLighting`): both, `C<RD>.*[LO]`, raw.
- **Raw custom expressions.** `bool crust:aov:raw = true` on an `lpe`
  RenderVar makes any expression raw — for example a light group's diffuse
  light, `C<RD>.*<L.'key'>`. An expression that can start with anything other
  than a diffuse reflection is refused with a warning (there would be no
  diffuse colour to divide by).
- **Diffuse filter AOV.** `diffuse_albedo` (aliases `DiffuseFilter`,
  `diffuseFilter`) becomes its own source: the diffuse lobes' colour at the
  camera ray's hit — the divisor the raw AOVs use. **Changes** the meaning of
  `diffuse_albedo`, which `add-usd-render-products-and-aovs` Phase 2 aliases to
  the all-lobe `albedo` (that change's open question 3).
- **The identity they keep.** Per camera sample, `raw × filter` equals the
  matching non-raw expression (to rounding). Per pixel it holds wherever the
  filter is constant over the pixel; across a texture or an edge the pixel
  average of a product is not the product of averages — the same caveat V-Ray
  documents.
- **No change** to the beauty, to any other AOV, or to a render that asks for
  no raw AOV.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `aovs` (introduced by `add-usd-render-products-and-aovs`, which must be
  archived first): new requirements for raw light AOVs, the `crust:aov:raw`
  modifier and the diffuse filter AOV; `diffuse_albedo` stops being an alias
  of `albedo`.

## Impact

- **Code**
  - `crust-core/src/material/`: a per-lobe diffuse filter beside the lobe
    split — OpenPBR's diffuse colour with its layer weights, a MaterialX
    diffuse leaf's colour times its weight in the tree.
  - `crust-core/src/aov.rs`: the new sources, aliases and the raw flag.
  - `crust-core/src/lpe/`: a check that an expression only accepts paths
    beginning with a diffuse reflection.
  - `crust-core/src/tracer/route.rs`, `path.rs`: record the vertex-0 diffuse
    filter; divide the raw expressions' per-sample results by it.
  - `crust-core/src/scene/usd_import/products.rs`: `crust:aov:raw`, the
    refusal, the new names.
- **Performance**: none without a raw or diffuse-filter AOV. With one, a
  filter evaluation at the first hit and one division per raw expression per
  sample.
- **Docs**: `site/content/docs/usd/aovs.md` (the raw AOVs, the identity and
  its per-pixel caveat), the `aovs` design record, `samples/aovs_lpe.usda`.
- **Dependencies**: none.
