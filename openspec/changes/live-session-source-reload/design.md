# Design

## Context

See `proposal.md` for the motivation and `specs/mcp-session/spec.md` for the behaviour.
The relevant current state:

- **Two stages per session.**
  - The authoring stage (`mcp/session.rs`, `open_stage`) is opened once per session on
    the override layer, with payloads unloaded. It is reopened only by `restore`
    (an undo, or a failed edit).
  - Every import opens its own stages through `usd_import::open_stage`, with payloads
    loaded: one stage, or one masked stage per streamed chunk, plus a stage masked to
    the camera for adaptive subdivision. Each is dropped when its part of the import
    is done.
- **What is stamped today.** `FileAssets::revalidate()` stamps decoded image files by
  length and modification time (`crust-assets/src/kept.rs`). Nothing stamps a USD
  layer.
- **How an edit is applied.** `Open::apply_change` goes `full` when a kept decode
  changed on disk. Otherwise it classifies the diff of the override layer alone
  (`mcp/delta.rs`) and may apply it in place.
- **Which renders `light_paths` inspects.** A job records a hash of the override
  layer's bytes, and `light_paths` refuses a job whose hash differs from the saved
  layer's.
- **What `openusd` gives.**
  - `Stage::layer_identifiers()` lists every layer the stage has loaded, including
    those reached across reference and payload arcs (the equivalent of C++
    `GetUsedLayers`).
  - `ar::split_package_relative_path_outer` maps a layer inside a package to the
    package file.

## Goals / Non-Goals

**Goals:**

- The set of source layers watched is exactly the set the import composed, payload
  layers included. It is known for every import, streamed or not, without a second
  traversal.
- Checking for a change costs one `stat` per watched layer and adds nothing to
  `crust render`'s render path.
- One external change costs one import, not two: an edit that follows a change
  imports once.

**Non-Goals:**

- Watching the filesystem: no notify thread, no events between calls. A change is
  seen when the next call looks.
- Watching non-USD files the stage names outside `AssetLoader`. This covers `.mtlx`
  documents read by `crust-mtlx`. `reload` is the escape hatch for them.
- Changes made to the override layer by another program. The session owns that
  layer; its own saves move its stamp.
- Re-traversing only the changed layers. A change is always a full import.

## Decisions

### D1. The import records the layers it composed: `Scene::layers`

Before each import stage is dropped (`release_stage` in `traverse_stage`, and the camera
stage), the import adds that stage's `layer_identifiers()` to a set on the import
context. Each identifier is turned into a file:

- a package-relative identifier → its outer package file;
- an anonymous layer → skipped;
- a relative identifier → resolved against the root layer's directory.

The set is sorted and lands on `Scene` as `layers: Vec<PathBuf>`. It is empty for a
hand-assembled scene. An in-place update keeps it: no in-place class can add or remove
an arc, since every composition change is `full` (`delta.rs`).

- *Why the import, not the authoring stage:* the authoring stage has payloads
  unloaded and only composes what `query` has reached, so it does not know the layers
  behind payloads. A BowerBot asset puts its `geo.usda` behind a payload.
- *Why the union over chunks:* a masked stage loads only the layers its subtree
  reaches, but together the chunks reach every layer the traversal read. A layer
  reached only through an inactive prim or an unselected variant is not watched. That
  is fine: making it count needs an edit (`active`, a variant selection), and such an
  edit is already `full`.
- *Alternative rejected:* a recording `ar` resolver wrapped around the default (or
  mapping) resolver. It would see every file opened, `.mtlx` included, but it means
  wrapping the `mmap` resolver whose safety comment rests on one construction site,
  and the listed identifiers are already what composition used.
- *Alternative rejected:* `usd_utils::compute_all_dependencies`. It is a second
  traversal of the whole dependency graph, and it reports asset paths the import did
  not read.

Cost: a few strings per layer. The `crust render` path is unaffected beyond collecting
identifiers once per stage. Callgrind on the Cornell box must show no render function
moving.

### D2. The session stamps those layers: `Sources`

`Open` gains `sources: Sources`. This holds `(PathBuf, Option<(u64, SystemTime)>)` per
layer in `Imported::scene.layers`, minus the override layer itself. It is the same stamp
as `kept.rs`'s, taken right after each import. `Sources::changed()` re-stats every entry
and returns the paths whose stamp moved.

- *Why length and mtime:* this matches the kept decodes, and the accepted gap is the
  same (a rewrite that keeps both is not seen). `reload` covers it.
- *Why not a hash of the bytes:* hashing would read every layer on every call. On
  ALab that is gigabytes per tool call.

### D3. A changed source reopens the authoring stage at once, and imports lazily

Every tool call that reads the stage or the scene first calls `Open::refresh()`. These
are `query`, `check`, `render`, every edit tool, `undo`, `light_paths` and
`render_final`, which already imports from disk itself. When `Sources::changed()` is
non-empty, `refresh()`:

1. reopens the authoring stage (`open_stage`), so `query` and the next edit's type
   lookups see the new composition;
2. sets `Open::stale = Some(changed paths)`.

The scene is imported only where it is needed:

- **An edit or undo:** `apply_change` skips classification and goes `full` whenever
  `stale` is set. One import serves both the external change and the edit.
- **`render` and `check`:** they import first (`sync` against the current bytes, forced
  `full`) when `stale` is set. The result gains `sources_changed`.
- **`query`:** never imports. It only needs the reopened stage.
- **A failed import:** keeps `stale` set and the old scene, and fails the call with the
  import's error. `render` never renders the old scene while `stale` is set.
- **A successful import:** re-stamps `sources` from the new scene's `layers` and clears
  `stale`.

*Alternative rejected:* importing eagerly in `refresh()`. A `query` after an external
change would then pay a full import it does not need, and an edit would pay two.

### D4. `reload` is a forced full import

`reload` stops the running render, reopens the authoring stage, and imports the saved
layer through the same `sync` path. It reports `sources_changed` (from `Sources` before
the import), `applied` `full`, and the warning codes gained and lost.

- It imports even when nothing changed. That makes it the escape hatch for what D2
  cannot see (a same-length same-mtime rewrite, a `.mtlx`).
- It does not touch the undo stack: undo holds override-layer bytes, which compose
  over whatever the sources are now.
- It still revalidates the kept decodes, as every import does. A reload with
  `CRUST_SCENE_DELTAS=0` gets a fresh loader.

### D5. A render records the sources it rendered

`Job::layer` (the override layer's hash) is joined by the sources' stamps at render
start, so `light_paths` refuses a job rendered before an external change, as it already
refuses one rendered before an edit. `probe` already names nothing once a re-import has
replaced the geometry (`Imported::geometry`), so it needs no change.

## Risks / Trade-offs

- **A layer rewritten in place during an import can fault the process under `mmap`.**
  This is the existing `mapping_resolver` caveat. Writers that replace by rename
  (openusd's `Layer::save`, USD's `SdfLayer::Save`) are safe. → Document it in the user
  page. The session's own authoring stage never maps.
- **An external writer caught mid-write.** The session may stat a half-written file.
  Its import fails, `stale` stays set, and the next call retries. Or the import
  succeeds on a partial file and the next stat (the writer's final mtime) makes it
  stale again. Either way the scene converges once the writer is done.
- **The stat cost on very large stages.** ALab composes on the order of thousands of
  layers. A few thousand `stat`s per tool call is milliseconds, against seconds for an
  edit. → Measure on the largest checked-in stage and record the number in the design
  record.
- **Unwatched files** (`.mtlx`, files read outside `AssetLoader`). → `reload`, and a
  Known-gaps entry.

## Migration Plan

Additive. No new environment switch is needed: the change only adds work when a source
moved, and the old behaviour (stale) is not one worth A/B-ing. `CRUST_SCENE_DELTAS=0`
still forces a full import per edit, and source stamping applies there too, so the
authoring stage is refreshed either way.
