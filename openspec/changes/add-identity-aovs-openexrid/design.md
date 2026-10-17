## Context

Carried over from `add-usd-render-products-and-aovs` (archived), where this was
Phase 3 and its design was D14; the AOV film, the first-hit record, the
importer's product resolution and the light-linking run table it extends are
all in place. The ID-matte half was **blocked**: the `exr` crate (1.74)
writes flat images only — its README lists deep data as not yet supported —
and OpenEXRId files are deep. It was unblocked by a fork of `exr` that reads
and writes deep scanline images in safe Rust (D5).

## Goals / Non-Goals

**Goals:**

- Stable per-prim ids and OpenEXRId mattes a compositor can key across
  frames.
- Primvar AOVs, which need nothing from deep output and can land first.

**Non-Goals:**

- Cryptomatte. The project chose OpenEXRId; the Cryptomatte plan
  (MurmurHash3 names, ranked `<name>NN.rgba` layers, header manifests) is
  dropped, not deferred.
- A C++/FFI EXR writer: `unsafe` on the output path would be a project
  decision of its own.

## Decisions

### D1. A prim-path table, kept only when asked for

Import keeps a compact `geom_id → interned prim path` table for every
geometry prim, extending `LightLinks`' `(first geom_id, path)` runs. It is
kept in `World` only when an identity AOV is requested, and dropped at the end
of import otherwise, as today. An instanced prototype reports the instance
prim's path — light linking's rule — so the stage-epoch rule applies
(`ImportCaches::epoch`). The bound material's path and the nearest `kind =
component | assembly` ancestor are kept beside it, for mattes by material and
by asset.

### D2. `primId` hashes the path

A hash of the prim path (MurmurHash3_x86_32, seed 0, over UTF-8 — any stable
32-bit hash would do; this one is small, safe Rust and well tested), so ids
survive frames and runs, unlike a `geom_id`, which depends on traversal
order. **Rejected:** Hydra's dense index — compositors key mattes across
frames, so stability matters more than density.

### D3. OpenEXRId through a deep writer, layout from its specification

Mattes are coverage-weighted with the beauty's own filter weights, like every
filtered AOV; cutout pass-through contributes to the surfaces behind it. The
channel and metadata layout is OpenEXRId's, taken from its specification when
the work resumes — not designed here. Accumulation keeps a small fixed
per-pixel list of (id, weight) with a spill, never a full-frame hash map.

**Unblocking:** deep scanline writing landing in `exr` (an upstream
contribution), or a deep scanline writer in-tree in safe Rust (the OpenEXR
deep format is documented). Decided in D5.

### D4. Primvars are kept at import because a var asked for them

Primvars named by `sourceType = "primvar"` vars go into a keep set before the
meshes load (`import_render_products` already runs first, on the index
stage), are stored per face-varying or vertex like `st`, and are evaluated at
the first hit. (As built they are kept as authored, per prim, rather than
baked per vertex: D8.)

### D5. As built: the deep writer is a fork of `exr`

`github.com/doubleailes/exrs`, branch `claude/awesome-fermat-41ixgg`, patched
over `exr` 1.74 in the workspace `Cargo.toml` and allowed as a git source in
`deny.toml`. It reads and writes deep scanline images in safe Rust, so the
in-tree writer was not needed. When upstream `exr` reads and writes deep
data, the patch line goes; nothing else names the fork.

### D6. As built: the OpenEXRId layout

OpenEXRId has no written specification; the layout is its `Builder` and its
readers', file version 3: one part, deep scanline, ZIPS; per pixel one
sample per name, front to back; `Id` (UINT, an index into the file's name
table), `Z` (FLOAT, the name's mean camera depth), `A` (HALF, its coverage)
and, when the product also holds the beauty, `R/G/B` (HALF, its share of the
beauty), in the deep "over" form `Builder::finish` writes; header
`EXRIdVersion = 3` and `EXRIdNames`, the names zlib-compressed and
base64-encoded. The table holds only the names some sample uses, sorted.

OpenEXRId disagrees with itself on the separator between names: its Nuke
plug-in splits on `\n`, its library's `Mask::read` (and its OpenFX plug-in)
on `\0`. Crust writes `\n`, as the Nuke plug-in reads it.
`scripts/openexrid_oracle.py` checks crust's mattes with OpenEXR 3 and
OpenEXRId's own reader, which needs a one-point patch to run on OpenEXR 3 at
all.

### D7. As built: the vocabulary

- **A `deepRaster` product is an ID matte**: one var naming what its ids
  stand for, and optionally the beauty, whose share each name then carries.
  Other vars are refused (`aov.not_in_matte`); a product with no naming var
  writes nothing.
- **Mattes by object, material and asset** need a name per hit under each,
  so the flat sources are five: `primId`, `instanceId`, `elementId` as
  planned, plus `materialId` and `assetId` (D1's material and `kind`
  ancestor, hashed like `primId`). The Cryptomatte names stay refused, with
  a message pointing at the mattes.
- **Identity planes hold the id's bits** in the film's `f32` slots, written
  back with `f32::to_bits`: identity sources are closest-only and never
  exposed, so no arithmetic touches them.

### D8. As built: where the data lives

- The names come from light linking's provenance runs, read at the end of
  the import; nothing traverses the stage a second time.
- Primvars are read as authored — values, indices, interpolation — once per
  gprim into a set shared by its instances, and evaluated at the first hit
  through a per-mesh element map (authored face, and where in it), the map
  `elementId` reads. A refined mesh finds its cage face through the Ptex
  face table. Constant primvars are inherited from ancestors.
- A hit's barycentrics are recovered by re-intersecting a sliver of the
  camera ray, only in the AOV instantiation and only where a primvar needs
  them, so the kernel's hit record is unchanged.

### D9. As built: the matte film

A bounded per-pixel list of names (64, the last a spill entry), summing the
beauty's own filter weights, the weighted radiance and the depth; normalised
by the beauty's weight sum, sorted front to back, blended across a guided
render's passes by the beauty's shares, exposed in colour only.

## Risks / Trade-offs

- **[Risk] Deep files are large** (a list per pixel). → The per-pixel list
  is bounded; ids under a coverage floor fold into the spill.
- **[Risk] Upstream `exr` deep support may not come.** → The in-tree writer
  is the fallback; nothing else in the change waits on it (D4 ships alone).
  As built, a fork carries it (D5): a git dependency the workspace patches
  in, which `cargo deny` allows by name.
- **[Trade-off] OpenEXRId's two readers disagree on the name separator.** →
  Crust follows the Nuke plug-in, the one compositors use (D6).
