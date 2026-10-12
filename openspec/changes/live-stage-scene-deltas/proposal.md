## Why

Crust reads a USD stage once, converts it into its own `Scene`, and renders that
copy; nothing ties the engine back to the stage. The `crust mcp` session shows the
cost: every edit, however small, saves the override layer and re-imports the whole
stage with a fresh `FileAssets` (`mcp/session.rs` `Open::edit`), so dimming one light
re-composes the stage, re-traverses every prim, re-decodes every texture and
re-compiles every MaterialX program. The mcp-session change already named this as its
open problem ("Incremental sync (phase 3). Every edit batch is a full re-import.";
the latency risk was "tens of seconds").

Following the stage live is also the foundation for a later Hydra render delegate:
Hydra hands a renderer small, keyed updates, and Crust has no way to apply one today.

## What Changes

- **Assets survive edits.** The session keeps one `FileAssets` for its lifetime
  instead of building one per import, so decoded textures, the tile cache and Ptex
  handles are reused. Before each import, any file whose length or modification time
  changed is evicted. Pixels cannot change, because streamed and preloaded textures are
  already pinned bit-identical.
- **The scene remembers where things came from.** The import keeps a provenance table
  on `Scene`: each material's prim path → the `World.materials` slots holding it, and
  each light's prim path → its `LightList` index and the geometry it shares an
  `Emissive` with. Today `LightLinks` builds a path → `geom_id` map during import and
  then drops it.
- **Edits are applied in place when they can be.** The session compares the override
  layer before and after an edit batch (or an undo) and puts the change into one of
  four classes:
  - `settings`: only `RenderSettings` / render-camera attributes changed → the
    settings and camera are re-read, and the world and lights are kept;
  - `lights`: only `inputs:*` values on existing UsdLux light prims changed → those
    lights are rebuilt and the light selection re-run, and the BVH is kept;
  - `materials`: only `inputs:*` values on existing shaders under existing materials
    changed → those materials are rebuilt and swapped into their slots, and the BVH
    and light list are kept;
  - `full`: anything else (prims added or removed, relationships, transforms, topology,
    variants, activation, composition arcs, a material whose change would move a
    structural flag or a light) → re-import as today, with the kept assets.
- **An in-place edit gives exactly what a re-import would.** A render after an
  in-place edit SHALL be bit-identical to a render after a full re-import of the same
  layer. This is a new bit-identity pair (in place ↔ full re-import), pinned by tests
  for each class.
- The edit tools' results report the class used (`applied`) beside the existing
  `import_s`.
- A new `CRUST_SCENE_DELTAS` switch (`1` by default): `0` makes every edit a full
  re-import with a fresh `FileAssets`, the behaviour before this change, so the latency
  gain can be A/B'd honestly.

**Out of scope, recorded as the next step:** a Hydra render delegate (`crust-hydra`: a
C++ `HdRenderDelegate` over a C ABI into the engine). It would reuse the provenance
table and the in-place update entry points, but it adds Pixar's C++ USD as a build
dependency and `unsafe` FFI. `CLAUDE.md` makes both a project decision, not part of
this change. Re-traversing only the changed part of the stage (transform and topology
edits) and BVH refit are also out of scope. A refit tree is not the tree a fresh build
gives, so it would break the session's bit-identity with `crust render`.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `mcp-session`: edits and undo apply in place when the change is confined to settings,
  light inputs or material inputs; results report how each edit was applied; assets
  persist across edits; the session's renders stay bit-identical to the CLI's.

## Impact

- `crust-core`:
  - `scene.rs`: a `Provenance` table on `Scene`.
  - `usd_import/`: provenance is filled from what `LightLinks`, `MaterialCache` and
    `lights.rs` already know. Entry points rebuild one material or one light from a
    stage at the import's evaluation time.
  - `rt_world.rs`: `World` swaps material slots and recomputes its flags. It refuses
    when a structural flag would move.
  - `light/list.rs`: `LightList` replaces entries and re-runs selection.
  - `config.rs`: `CRUST_SCENE_DELTAS`.
- `crust-assets`: `FileAssets::revalidate` (stat the files it has loaded and evict the
  ones that changed).
- `crust-render`: `mcp/session.rs`, `mcp/scene.rs` (layer diff, classification, the
  persistent `FileAssets`); `import_checked` takes an asset loader instead of building
  one.
- Docs: `docs/architecture.md` (switch row, the new invariant pair), the
  environment-variables and Claude Desktop pages in `site/`, and the `mcp-session` and
  `usd-scene-import` design records.
- No new dependency and no `unsafe`. `crust render` is unchanged; every checked-in
  sample renders bit-identical.
