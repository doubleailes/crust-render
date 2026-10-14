## Context

See `proposal.md` for why. What shapes the approach:

- **Where an edit's time goes.** The BVH commit is a small share of an import: 19.1 s
  of Moana's 5:13 parse, 1.4 s of ALab's 2:57 (`docs/moana_profile.md`,
  `docs/alab_profile.md`). Composition and traversal dominate the rest, and on textured
  scenes so does asset decoding (the DPEL teapot's 18 s import is texture decoding,
  `openspec/specs/usd-scene-import/design.md`). So a design that keeps the traversal
  but refits the BVH would save almost nothing. The win is in not traversing at all
  and in not decoding again.
- **The session throws its caches away.** `import_checked` (`crust-render/src/main.rs`)
  builds a new `FileAssets` for each import. The session keeps the old one alive only
  until the new scene swaps in (`mcp/scene.rs`, `Imported._assets`). The tile cache,
  decoded textures, Ptex handles and JIT programs are rebuilt on every edit.
- **The engine's scene is immutable and anonymous.** `World` holds a committed
  `crust_rt::Scene` (no refit, no update API in `crust-rt`), `materials:
  Vec<Arc<dyn Material>>` by `geom_id`, and flags derived from the materials at commit
  (`cutouts`, `light_classes`, `transparent_emitters`, …). Only lights keep their prim
  path (`LightList::names`). The import knows every path → `geom_id` run
  (`LightLinks`), every material path → `Arc` (`MaterialCache`) and every light's shared
  `Emissive`, then drops all three when `load_scene` returns.
- **Edits are already local and typed.** Every edit tool merges USDA into the override
  layer, which is the stage's root layer. Its spec paths are composed prim paths, with
  no path translation. Comparing its `AbstractData` before and after tells exactly
  which specs and fields an edit batch (or an undo) changed. `Snippet::check_against`
  already walks this data.
- **The session promises bit-identity with the CLI** (mcp-session "Renders are
  reproducible"). Whatever in-place path exists must build exactly the scene a fresh
  import of the same layer would.
- **openusd has no change notification.** The session diffs the layer itself.

## Goals / Non-Goals

**Goals:**

- Changing a light's intensity, a shader input or a render setting in a session costs
  roughly the time to rebuild that object and re-run light selection, not a full
  import.
- Every edit, in place or not, reuses decoded assets that have not changed on disk.
- An in-place edit gives a scene that renders bit-identically to a full re-import of
  the same layer.
- Keep the provenance table and the in-place entry points USD-agnostic at the
  `crust-core` boundary (keyed by path strings, given built values), so a later Hydra
  delegate can drive them.

**Non-Goals:**

- A Hydra render delegate. It adds C++ USD and `unsafe` FFI, which needs a project
  decision.
- Re-traversing a changed subtree (transforms, topology, prims added or removed,
  bindings, variants). These stay `full`.
- BVH refit. It would not match a fresh build bit for bit.
- Keeping the session's composed stage across imports instead of reopening the saved
  layer. That would be a separate change, and it would need its own proof that the
  composition gives the same result.
- Any change to `crust render`.

## Decisions

### D1. Classify by layer diff, conservatively

After an edit batch is merged and saved (or an undo restored), compare the root
layer's data before and after into `(spec path, field)` changes. Classify the batch as
the most expensive class any one change needs:

| change | class |
|---|---|
| attribute `default` / `timeSamples` on the `RenderSettings` prim the render uses, or on the render camera | `settings` |
| `inputs:*` attribute value on an existing prim whose composed type is a UsdLux light, where the input is not a shaping or IES input whose file is new | `lights` |
| `inputs:*` attribute value on an existing `Shader` prim whose ancestor `Material` the import resolved, or an `inputs:*` interface value on that `Material` | `materials` |
| anything else: a spec added or removed, a relationship or connection, metadata, any `xformOp`, `visibility`, `purpose`, a primvar, a composition arc | `full` |

The table is an allow-list. A field not named in it is `full`. Classification reads
composed types from the session's stage, so an opinion on an over (`over "Key"`)
classifies by what the prim is.

*Alternative:* classify from the parsed `Snippet`. Rejected because undo has no
snippet, and because merging can turn an authored spec into no change at all.

### D2. Assets live as long as the session

The session owns one `Arc<FileAssets>` and passes it to every import
(`import_checked(path, assets)`). Before each import, `FileAssets::revalidate()` stats
every file it has loaded and evicts the entries whose length or mtime changed. This
covers the agent re-exporting a texture in the middle of a session. The bit-identity
of streamed and preloaded `u8` textures and of capped Ptex already proves that a
cache's residency state cannot change a pixel. This applies to `full` edits too, and
it is most of the win on textured scenes.

*Trap:* the JIT programs belong to the materials, not to `FileAssets`. A `full`
re-import still recompiles them. An in-place material edit compiles only the changed
material.

### D3. Provenance on `Scene`

`Scene::provenance` keeps three maps, filled from what the import already builds:

- material path → the `World.materials` slot indices whose `Arc` came from that path.
  This is recorded by `Arc` pointer at commit. *Trap:* `MaterialCache` is keyed by
  `(epoch, path)`, and the epoch changes per streamed chunk, so one path can own
  several `Arc`s. Map every one, not the first.
- light path → `LightList` index, plus the `geom_id`s whose `World.materials` slot
  holds that light's `Emissive`;
- geometry run → prim path (the `LightLinks` runs, kept instead of dropped).

It is built in the import's existing traversal order. It adds memory proportional to
the material and light counts and nothing per triangle.

### D4. In-place rebuilds go through the import's own code

The `materials` and `lights` classes rebuild the object by calling the same import
function that built it the first time. That function runs on the session's stage, at
the scene's time code (`EvalTimeScope`), with the session's `FileAssets`. It must not
be a second implementation, or the bit-identity would rest on two copies agreeing.
The import exposes `rebuild_material(stage, path, …)` and
`rebuild_light(stage, path, …)`. Both take the same caches a full import would use,
seeded empty so they resolve as a fresh import does.

Each rebuild then goes through a `crust-core` entry point that takes built values:

- `World::replace_materials(&[(slots, Arc<dyn Material>)]) -> Result<World, Structural>`.
  It recomputes the derived flags and refuses when any of them would differ from the
  committed ones: cutout or opacity class, transparent emitter, light class, or a
  material that starts or stops emitting. A refusal falls back to `full`. The committed
  `crust_rt::Scene` is shared, not rebuilt, so `World` holds it in an `Arc`.
- `LightList::replace(index, LightKind)`, then the same `select_by` a fresh
  `Renderer::new` runs. An area light's new `Emissive` replaces both
  `AreaLight.material` and every `World.materials` slot in its provenance, in one call.
  This keeps the `Emissive::radiance_toward` pair whole.
- `settings`: re-read `RenderSettings` and the camera with the import's own functions.
  `Renderer::retune` already handles a selection that does not depend on them.

*Trap:* `retune` keeps the light selection when the resolution and frame are
unchanged. A `lights` or `materials` edit must rebuild the selection instead (power
CDF and learned cache), because its key does not include scene content.

### D5. In place ↔ full re-import is a bit-identity pair

Each class has a test that renders a sample at 16 spp after an in-place edit and after
a full re-import of the same saved layer, and compares them bitwise, including the
warnings list. `settings` uses an exposure change, `lights` a light's intensity and
colour, and `materials` a base colour, a roughness and a texture-file input.
`CRUST_SCENE_DELTAS=0` runs the same edits through the old path for the A/B. This pair
joins the list in `docs/architecture.md` § Invariants.

## Risks / Trade-offs

- **[A missed field silently renders stale]** → The allow-list in D1 defaults to
  `full`, and the D5 tests run each class against a fresh import. A field that should
  have been `full` and was not shows up as a bitwise difference.
- **[Rebuilding one material resolves differently than in context]** → Material
  resolution reads nothing but the material's own network and the time code. The
  provenance maps record exactly which slots to swap. Bindings are relationships, so a
  binding change is `full`.
- **[A texture file that changes without its mtime changing]** → Accepted. This is the
  same check `make`-style tools rely on, and `CRUST_SCENE_DELTAS=0` gives a fresh
  cache.
- **[Memory: a session keeps textures for prims it no longer renders]** → Bounded by
  the existing tile cache budget. Evicting unused entries is a known gap.

## Migration Plan

Nothing on disk changes. The session's layers, results and renders keep their shape,
plus one new `applied` field. Rollback is `CRUST_SCENE_DELTAS=0` or a revert.

## Open Questions

- Should a light's transform get its own class? A light with no geometry (distant,
  dome) moves without touching the BVH. A `RectLight`'s geometry is in the top-level
  BVH. This is deferred until the D5 tests show the simpler classes are solid.
- Should the D2 revalidation also run for `crust render` in a watch mode? There isn't
  one today.
