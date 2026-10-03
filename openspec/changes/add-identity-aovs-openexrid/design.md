## Context

Carried over from `add-usd-render-products-and-aovs` (archived), where this was
Phase 3 and its design was D14; the AOV film, the first-hit record, the
importer's product resolution and the light-linking run table it extends are
all in place. The ID-matte half is **blocked**: the `exr` crate (1.74) writes
flat images only — its README lists deep data as not yet supported — and
OpenEXRId files are deep.

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
deep format is documented). Decide between them when the work resumes.

### D4. Primvars are kept at import because a var asked for them

Primvars named by `sourceType = "primvar"` vars go into a keep set before the
meshes load (`import_render_products` already runs first, on the index
stage), are stored per face-varying or vertex like `st`, and are evaluated at
the first hit.

## Risks / Trade-offs

- **[Risk] Deep files are large** (a list per pixel). → The per-pixel list
  is bounded; ids under a coverage floor fold into the spill.
- **[Risk] Upstream `exr` deep support may not come.** → The in-tree writer
  is the fallback; nothing else in the change waits on it (D4 ships alone).
