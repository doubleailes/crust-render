# mcp-session — design record

> Design record for the **mcp-session** capability (`crust mcp`): the reasoning,
> measurements and history behind the behaviour `spec.md` states.
> `openspec/specs/*/design.md` is the whole record; `docs/architecture.md` is the map.
> The session's founding decisions are summarised here and argued in full in the
> archived changes `openspec/changes/archive/2026-10-09-mcp-session/design.md`
> (D1–D10) and `2026-10-09-mcp-first-pixel/design.md` (the first image, `retune`).

## The session is a file

- **The override layer is a real `.usda` on disk, and the import reads that file**
  (mcp-session D1). `open_session` writes `<output>.usda` with `subLayers = [@<input>@]`
  and nothing else; every edit is merged into it and saved. What the session renders,
  and what it leaves behind, is what `crust render <output>` renders: the session
  imports through the CLI's own argument defaults (`Cli::try_parse_from`) and the
  CLI's own `import_checked` / `check_scene`, so a render flag that changes the import
  belongs in those, never in one caller.
- **One thread owns the authoring stage** (D2). openusd's `Stage` is `Rc` / `RefCell`;
  the protocol's async handlers reach it through a channel, one command at a time.
- **Every edit is a USDA snippet merged into the layer** (D3, `mcp/edit.rs`): per
  field, children lists unioned, variant selections and dictionaries key by key,
  `apiSchemas`-style list ops item by item, an `over` never downgrading a `def`.
- **Undo is a stack of the layer's bytes** (D4), for the server process only.
- **Renders are time-bounded and progressive** (D6, first-pixel D1–D3); `probe` and
  `diff` read the retained buffers (D7); `render_final` is the CLI's render body (D8).
- **`probe` names a hit from the provenance**: it casts the pixel-centre camera ray
  (lens centre, shutter open, no opacity) through the render's own camera against the
  session's world, then `Provenance::prim_of_geometry` and a slot → material map turned
  around once per import (`Imported::names`). A job remembers which full import built
  its geometry (`Imported::geometry`, kept by in-place updates, which move none), and
  names nothing once a re-import replaced it. The runs record the prim that emitted the
  geometry, so instances and PointInstancers name the instance or instancer prim.
- **`light_paths` inspects a render on its own layer** (`light-path-inspection` D8): a
  job keeps a hash of the layer bytes it rendered, the call is refused once the saved
  layer's differs (an undo back to it matches again, and an in-place update's scene is
  the full import's), and it re-renders the region with the job's settings through
  `inspect::run` on the session's renderer — refused while another render holds it.
- **`Renderer::retune`** keeps a light selection whose inputs (strategy, resolution,
  frame) did not change, so a session render after a region or samples change pays
  no second `learned` pre-pass (first-pixel D4).

## The authoring stage knows the schemas (`mcp-session-schema-registry`)

- **The session's stage opens with the import's schema registry**
  (`crust_core::usd_schema_registry()`, which `usd_import::stage_builder()` uses too), so
  the families are the import's by construction (crust-core's openusd-schemas
  features), not a second list kept in step. An attribute a prim's schema declares
  has its schema type and fallback before any layer authors it: `query` answers
  `source` `"fallback"`, and `set_attribute` takes the schema's type and refuses a
  different explicit `type`. Only an attribute no schema declares and no layer authors
  is refused, with the error pointing at `set_attribute` and an explicit `type`.
- **`source` is the opinion `get` resolved, not the first layer in the stack.** It
  comes from `resolve_info_at(None)`, the same default time `get` reads: the spec
  site's layer, `"fallback"` when the schema answered, `null` when nothing does.
  *Trap:* the property stack also lists a layer that authors only metadata on the
  property (a `doc`, no `default`), so its first entry can name a layer while the
  value is the schema's fallback; `layers` still lists it as an opinion.
- **Only the registry, not the import's resolver.** The memory-mapped resolver is a
  read-throughput choice; the session's stage is small, payloads unloaded, and the
  layer it writes is replaced by rename (`replace_file`), which a mapping would only
  complicate.
- **A prim `query` lists authored attributes only.** A light's schemas declare some 30
  properties and a material many more; listing them would bury what the scene authors.
  An agent queries a declared attribute by path. Revisit if agents guess names.
- **The schema's declared type wins over an authored one**, as in C++ USD: a source
  that authors `double inputs:exposure` reads as `float`, `set_attribute` authors a
  `float`, and `type="double"` is refused. The tests pin this.

## Edits applied in place (`live-stage-scene-deltas`)

Until this change every edit batch was a full re-import with a fresh `FileAssets`:
dimming one light re-composed the stage, re-traversed every prim, re-decoded every
texture and re-compiled every MaterialX program. The founding design named it as its
open problem (D10, "the seam phase 3 will widen").

### Where an edit's time goes

The BVH commit is a small share of an import — 19.1 s of Moana's 5:13 parse, 1.4 s of
ALab's 2:57 (`docs/moana_profile.md`, `docs/alab_profile.md`). Composition, traversal
and, on textured stages, asset decoding dominate. So keeping the traversal and
refitting the BVH would save almost nothing, and a refit tree is not the tree a fresh
build gives, which would break the session's bit-identity with `crust render`. The win
is in not traversing at all, and in not decoding again.

### Assets live as long as the session

The session owns one `Arc<FileAssets>` built with `keep_decoded()` and passes it to
every import (`import_checked(args, keep_stage, assets)`). The loader keeps every
texture, Ptex, light texture and IES profile it decoded (`crust-assets/src/kept.rs`),
stamped with the length and modification time of each file the decode read — for a UV
texture, each UDIM tile and the `.tx` sibling the streaming path looks for, so a `.tx`
appearing later counts as a change, and for a `<UDIM>` / `<UVTILE>` set its directory,
whose modification time moves when a tile is added or removed. Before every edit,
in place or not, `FileAssets::revalidate()` drops the entries whose stamps moved (and
the Ptex handles opened from them, re-budgeting the streams left), starts a new
generation, and restarts the preload counters, so `crust-check/2`'s facts count an
import as a fresh loader's would. After each full import `FileAssets::sweep()` lets go
of every entry that import did not ask for: what the scene no longer uses, and the Ptex
streams opened from it.

- *Trap:* when anything changed on disk the edit is `full`, even one confined to a
  light. The scene's *other* materials and lights still hold the old decode, and only a
  re-import replaces them (`a_file_changed_on_disk_makes_the_next_edit_full`). The first
  version revalidated only on the full path, so an in-place edit after a re-export
  rendered stale texels.
- *Trap:* an in-place update loads no texture, so it re-checks the scene with the
  preload count its import recorded; the report's phases are the import's followed by
  `Update in place (<class>)`, the update's own time.

- Only successes are kept: a file that failed is tried again by the next import, which
  raises the warnings a fresh loader would.
- Environment maps are not kept: they cross `AssetLoader` by value, so a kept one would
  be copied per hit. A dome's map is decoded again by each import and each dome rebuild.
- *Trap:* JIT programs belong to the materials, not to `FileAssets`. A `full` edit
  recompiles every MaterialX program; an in-place material edit compiles only the
  changed material.
- *Accepted:* a file rewritten without its length or mtime changing is not seen.
  `CRUST_SCENE_DELTAS=0` gives a fresh loader per import.

### Classification: an allow-list over a layer diff

After an edit batch is merged and saved (or an undo restored), `mcp/delta.rs` compares
the layer's bytes before and after — both parsed with `openusd::usda::parse` — spec by
spec and field by field (`layer_diff`), and `classify` puts the changes into the
cheapest class that can follow all of them:

| change | class |
|---|---|
| attribute `default` / `timeSamples` on the `RenderSettings` prim the import read, or on the render camera (not `xformOp:*` / `xformOpOrder`) | `settings` |
| `inputs:*` value on a light the scene has, other than `width`, `height`, `radius`, `length`, `treatAsPoint`, `treatAsLine` (the BVH holds its shape) | `lights` |
| `inputs:*` value on a `Shader` under a material the scene uses, or on that `Material`, when no other material reads it | `materials` |
| anything else: a spec defined or removed, a relationship, a connection, metadata, a transform, `visibility`, `purpose`, a primvar, a variant, a composition arc, layer metadata | `full` |

A batch is applied as its most expensive class, `settings` < `materials` < `lights` <
`full`, and rebuilds every part it names. Namespace bookkeeping is not a change: an
`over` prim spec that only names children (what an edit authors to reach an attribute,
and what an undo removes) and the `primChildren` / `propertyChildren` lists. An
attribute spec that only declares and sets a value (`typeName`, `variability`,
`custom`, `default`, `timeSamples`) is a value change, both when it appears and when an
undo removes it. A diff with no change at all is applied in place with nothing to
rebuild and reports `settings`.

Composed prim types come from the session's authoring stage, so an opinion on an
`over` classifies by what the prim is. Which lights and materials "the scene has" is the
import's provenance (below), not the stage: a light the import skipped, or a material
only a prototype uses, is `full`.

*Trap:* the authoring stage leaves payloads unloaded, so a shader a payload brings in
(ALab's looks) composes there only as the edit's own typeless `over`. A prim with *no*
type under a material the scene uses still classifies `materials`, and is listed in
`Change::shaders`. The rebuild then requires it to be a `Shader` on the material's own,
payload-loaded stage, and anything else (a `NodeGraph`, a typo) falls back to `full`
(`a_shader_behind_a_payload_edits_in_place`). A prim whose type the authoring stage
does read and is not `Shader` is `full` at once, as before.

*Alternative rejected:* classifying the parsed `Snippet`. An undo has no snippet, and
merging can turn an authored spec into no change at all.

*Trap:* a shader can belong to more than one material. Another material can compose
it through an `inherits`, `specializes` or `references` arc, or connect into it, and an
edit there changes that material too. So the import records, per material it resolved
(prototype-scoped ones included), the prims outside it that its network reads: the
paths of its prims' `prim_stack` sites and its connection targets
(`material_reads`). An edit at or above or below a prim another material reads is
`full` (`Provenance::other_readers`, `an_edit_another_material_reads_is_full`). A
referenced layer's paths are in that layer's namespace and may coincide with an edited
one; that costs a full import, never a stale material.

### Provenance

`Scene::provenance` (`crust_core::live::Provenance`), filled from what the import
already builds:

- material prim path → the `World` slots holding its `Arc` (`MaterialCache::origins`,
  by pointer, after commit), and whether a mesh was displaced by it;
- light prim path → its index in the `LightList` *after* light-link resolution, and the
  `geom_id`s it emitted (an area light's emitter). *Trap:* resolution takes a light whose
  `lightLink` includes no receiver out of the list and shifts the later ones down, so the
  index recorded at emission can name another light. `LightLinks::resolve` returns its
  remap (`index_remap`), the import applies it, and a light taken out gets no entry, so
  an edit to it is `full` (`light_indices_are_the_list_as_the_links_left_it`);
- the `LightLinks` geometry runs (first `geom_id`, prim path), kept instead of dropped
  — `Provenance::prim_of_geometry`;
- the stage-wide facts a settings change must not move: the host's import options, the
  `RenderSettings` path, the subdivision level, what tessellation read from the camera
  (below), the dome's camera visibility, the working space, the render camera.

Memory is in proportion to the material, light and emitting-prim counts, nothing per
triangle. *Trap:* `MaterialCache` keys a prototype-scoped path (`/__Prototype_N`) by
epoch, so one such path names a different material in each streamed chunk. Those paths
are left out: no edit names one (an edit inside an instance is refused), and an
authored path keys on the path alone, so it has exactly one `Arc` however many chunks
bind it (pinned by `a_streamed_import_maps_a_shared_material_from_every_chunk`).

### Rebuilds go through the import's own code

`Provenance::rebuild` (`scene/usd_import/live.rs`) runs at the scene's time code
(`EvalTimeScope`), with the session's `FileAssets` and import caches seeded empty, so
everything resolves as a fresh import does. It never copies an import function — the
bit-identity would then rest on two copies agreeing — so the import was factored to
share them:

- **materials:** a stage population-masked to the material's prim with payloads loaded
  (one streamed chunk's kind of stage), then `resolve_fresh` — what a `MaterialCache`
  miss runs — and `resolve_displacement`. Refused when the network connects to a prim
  outside the material (the masked stage would not compose it), or when it defines a
  displacement (its inputs shape geometry).
- **lights:** a stage masked to the light, the world transform by `local_to_world`
  (the traversal's own fold of `compose_with_parent`), and `emit_light`, the dispatch
  the traversal itself calls, into a scratch context whose `WorldBuilder` has the
  light's first `geom_id`s claimed first, so its emitter gets the id it had. Refused
  when it would not emit exactly one light over the same geometry ids. Light links do
  not refuse it: resolution reads only the `collection:*` relationships, which an
  in-place edit never changes, and everything it produced (per-light illuminated
  classes, shadow masks, per-geometry light classes, shadow bits in the geometry masks,
  backdrops) is keyed by index or `geom_id` and kept as resolution left it.
  `emit_light` reads no link state, only the prim's own visibility
  (`linked_light_edits_stay_in_place`).
- **settings:** the index stage re-read by `read_index` — the function `load_scene`
  reads it with — then the camera rebuilt by `build_camera` on a stage masked to it and
  its exposure applied by `apply_exposure`. Refused when the working space, the
  subdivision level, the dome's camera visibility or the render camera would change, or
  when tessellation that reads the camera would read something else. Adaptive
  subdivision and implicit shapes sized on screen are handed one `ScreenProjection`
  (eye, pixel rate, and the frustum when `CRUST_ADAPTIVE_FRUSTUM` builds one). The
  import records it, and adaptive subdivision's target and ceiling, as a `ScreenKey`
  (every number's `to_bits()`), and the refresh compares that key with the one
  `read_index` gives now. So an exposure, an f-stop or a sampling setting stays in
  place, while a move, a lens or a resolution re-imports
  (`camera_tessellated_settings_edits_stay_in_place`). The key is what the tessellation
  is handed, not the camera attributes, so it cannot go stale when `CameraFrame::read`
  reads one more.

**Warnings.** The update keeps the scene's warnings list, so it is refused whenever
that list might not be what a fresh import reports: a material or light rebuild when
the scene's warnings name the prim (or have kept their sixteen prims and may name it
beyond), or when the rebuild raises one that the scene does not already carry for the
same prim (an ancestor's, raised again, is fine); a settings refresh when the scene
carries a warning of a family a setting can move (`settings.`, `product.`, `aov.`,
`lpe.`, `camera.`, `time.`, `color.`, `subdiv.`) or the refresh raises any.

### Applying the rebuilt parts

`Update::check` then `Update::apply`, both in `crust-core`, keyed by slot and index:

- `World::check_materials` compares each rebuilt material's `live::Signature` with the
  one in every slot it goes into — `kind`, cutout, straight transmission, medium
  boundary, `may_emit`, `uses_uv`, `uv_primvar`, the Ptex face count. These are what
  `WorldBuilder::commit` derives flags from and what the mesh import built the UV and
  per-face side tables and the face-count warning on; everything else about a material
  is read per hit. `World::replace_materials` then swaps the `Arc`s; the committed
  kernel scene, side tables and flags stay. `Material::may_emit` is new: its default is
  a non-zero `emitted()`, and the MaterialX and pattern materials, which can emit
  through `emitted_at` alone, answer from their compiled emission.
- `LightList::check_replace` refuses another light kind, a move to or from infinity,
  or another `geom_id`; `LightList::replace` keeps the light's name, LPE
  tag, escape mask and geometry index and drops the selection back to uniform. An area
  light's new `Emissive` goes into its emitter's slot in the same update, so the light
  and its geometry keep one `Emissive` and `radiance_toward` stays one answer
  (`a_replaced_light_and_its_geometry_share_one_emissive`).
- The session (`mcp/scene.rs`, `Imported::update`) checks first, then takes its
  renderer apart into a `Scene`, applies, re-runs `check_scene` (the report, the product
  selection and its warnings) and builds a new renderer with `Renderer::new` around the
  same committed world. *Trap:* `retune` keeps the light selection when the strategy,
  resolution and frame are unchanged — its key holds no scene content — so a lights or
  materials update must not be retuned into an old renderer; `Renderer::new` rebuilds
  the power table and the `learned` pre-pass.

Any refusal, at classification, rebuild or check, falls back to `full` with the scene
untouched, and the result reports `applied = full`.

### In place ↔ full re-import is a bit-identity pair

`edits_in_place_render_as_a_full_import` (`crust-render/src/mcp/tests.rs`) authors one
edit of each kind on one stage — a camera exposure and a `crust:maxDepth`
(`settings`), a `RectLight`'s intensity and colour and a dome's texture file (`lights`),
a Standard Surface base colour, a preview surface roughness and a texture file
(`materials`) — and after each, and after undoing each in reverse, renders at 16 spp
and compares every plane bitwise, and the warnings list, with a second session that
imports the same saved layer whole. Disabling the material swap fails it on the first
light (24% of pixels), and skipping revalidation fails
`a_re_exported_texture_is_read_again` (51%). `unconfined_edits_are_full` pins the
`full` rows (a transform, an emission from 0, a light's width, a new prim, a binding)
and a mixed batch's class; `a_full_edit_decodes_no_texture_again` the kept decodes.

### Measured

Edit latency (`import_s`), min / mean of 10 edits of each kind alternating two values,
release build, one machine (4 cores), `CRUST_SCENE_DELTAS` on and off:

| stage | open | `settings` | `lights` | `materials` | `full` |
|---|---|---|---|---|---|
| synthetic: 16 quads, each its own 2048² PNG, on | 0.81 s | 1.7 / 1.9 ms | 0.9 / 1.0 ms | 1.2 / 1.3 ms | 8.9 / 9.1 ms |
| the same, off | 0.76 s | 608 / 632 ms | 633 / 662 ms | 634 / 659 ms | 624 / 644 ms |
| `usdpreview_textured`, on | 18 ms | 1.0 / 1.2 ms | 0.6 / 0.7 ms | 0.9 / 1.3 ms | 4.3 / 5.1 ms |
| the same, off | 15 ms | 5.7 / 6.5 ms | 5.8 / 6.3 ms | 5.5 / 6.5 ms | 5.6 / 6.1 ms |

On the textured stage an in-place edit is ~400× faster, and a `full` edit ~70× faster,
because the kept decodes leave nothing to decode. The DPEL teapot and ALab, the scenes
the change was proposed against, are not in the repository's checkout and were not
measured; neither were their `--stats` peak memories.

The provenance's own cost on a render: callgrind on `crust render` of the Cornell box at
`-s 2`, one thread, 4 222 741 990 instructions before and 4 222 911 859 after (+169 869,
0.004%), all of it in the import (`sdf` path comparisons for the extra
`RenderSettings` lookup, and the maps' allocations); no render function's count moved.
Every checked-in sample renders bit-identical to goldens recorded before the change
(`scripts/check_images.sh`).

### Known gaps

- **`query` shows a schema fallback the render may not use.** The import reads every
  attribute through `value_at`, which takes a value equal to the schema fallback as
  unauthored and substitutes crust's own default. Where those differ, the session's
  answer is not what renders:
  - a DistantLight with no `inputs:intensity` queries as `50000` (UsdLux's fallback)
    and imports as `1.0` (`light_inputs`; measured with `crust ls light --json`
    beside a session `query`). Lights' `exposure` (0) agree.
  - a camera with no `verticalAperture` queries as the schema's `15.2908` and imports
    as `horizontalAperture × height / width`, which follows the image's aspect
    (`CameraFrame::read`); one with no `focusDistance` queries as `0` and imports as
    `10`.
  - a render product with no `resolution` queries as the schema's and inherits the
    settings' (`value_at`'s own list of these readers).
  Closing it means either the import honouring UsdLux's per-type fallbacks (a render
  change) or `query` reporting crust's default where the import substitutes one.
- **A Hydra render delegate** (`crust-hydra`: a C++ `HdRenderDelegate` over a C ABI
  into the engine) would reuse the provenance and the `live` entry points, which take
  paths and built values for that reason. It adds Pixar's C++ USD as a build dependency
  and `unsafe` FFI, which `CLAUDE.md` makes a project decision.
- **Transforms, topology and namespace edits re-import.** Re-traversing only the
  changed subtree, and BVH refit, are not done: a refit tree is not the tree a fresh
  build gives. A light with no geometry (distant, dome) could move without touching the
  BVH; it is `full` until the simpler classes have proved themselves.
- **On a production shot, only confined edits are cheap.** ALab frame 1004
  (`in-place-edits-on-production-shots` `notes.md`): an exposure, a light's intensity and
  a payload-delivered shader's roughness apply in place in 18–60 ms. Any transform,
  binding or prim edit is a ~112 s re-import, since composition and traversal, not
  decoding, are that stage's import (the kept assets save it 2–5 s). Where the in-place
  milliseconds go on a large stage, against ~1 ms on the teapot, was not profiled.
- **Kept decodes are let go only by a full import.** A run of in-place edits that stops
  using a texture keeps it until the next `full` edit sweeps.
- **Environment maps are decoded again** by every import and dome rebuild.
- **The session re-composes the stage per rebuild** (a masked stage per material or
  light, the index stage for settings) rather than keeping one composed stage across
  imports, which would need its own proof that composition gives the same result.

## Sources on disk (`live-session-source-reload`)

A session can sit beside another program that rewrites the stage it is an override of,
for example a scene-assembly agent running as a second MCP server in the same client.
Before this change the session saw none of it:
- the kept decodes were stamped, but no USD layer was;
- the edit classifier diffed the override layer alone, so an edit after a rewrite could
  apply in place over a stale scene;
- the authoring stage kept the composition it opened with.

### What is watched: the layers the import composed

- **The import records them** (`Scene::layers`). Before each of its stages is released
  (the single stage, every streamed chunk, the index stage), it adds the stage's
  `layer_identifiers()` to a set (openusd's `GetUsedLayers`). Identifiers become files:
  - a layer inside a package is the package;
  - a relative identifier is anchored to the root layer's directory;
  - file format arguments are dropped;
  - anonymous layers are left out.

  An in-place update keeps the list, since no in-place class adds or removes an arc. The
  camera stage masked for adaptive subdivision is not collected: it composes one prim the
  traversal composes too.
- *Why the import, not the authoring stage:* the authoring stage has payloads unloaded and
  composes only what `query` reaches, so it never knows the layers behind payloads. That
  is where an assembled asset puts its geometry.
- *Accepted:* a layer reached only through an inactive prim or an unselected variant is
  not watched. Making it count takes an edit (`active`, a variant selection), which is
  `full` already.
- *Alternatives rejected:*
  - a recording `ar` resolver, which would have to wrap the mapping resolver whose
    safety argument rests on its one construction site;
  - `usd_utils::compute_all_dependencies`, a second traversal reporting asset paths the
    import did not read.

### How a change is seen and applied

- **Stamps.** `mcp/sources.rs` stamps each watched layer, other than the override layer,
  with its length and modification time, as `kept.rs` stamps decodes.
  `Sources::changed()` re-stats every layer and re-stamps the ones that moved, so each
  change is reported once.
  - The stamps are taken *before* the import (`Sources::now`), so a layer written while
    the import read it still differs from its stamp, and the next call imports again
    (`a_write_during_an_import_is_seen_afterwards`).
  - *Gap:* a layer the previous import did not compose is stamped after the import.
- **`Open::refresh()`.** Every tool that reads the stage or the scene calls it first:
  `query`, `check`, `render`, the edit tools, `undo`, `light_paths`, `render_final`. On a
  change it records the changed layers in `stale`, then reopens the authoring stage. It
  imports nothing.
  - The change is recorded before the reopen, and the reopen is retried (`restage`)
    until it succeeds. *Trap:* the first version re-stamped and then failed to reopen,
    so the change was forgotten and the old scene rendered.
  - A change that brings a composition error the stage did not have is refused, naming
    the changed layers. openusd composes a layer that no longer parses as if it were
    absent (`MalformedSublayer`), and so does `crust render`. A writer caught mid-write
    looks the same, so the session waits for the layer to compose instead of rendering
    the scene without it. Errors the stage already had are not refused.
- **The import is lazy.**
  - An edit or `undo` while `stale` is set goes `full` without classifying, so one import
    serves both the external change and the edit.
  - `render` and `check` import first (`bring_current`).
  - `query` only needs the reopened stage.
  - A failed import keeps `stale` set, so the next call tries again, and `render` never
    renders the old scene while it is set.
  - A successful full import re-stamps the sources and clears `stale`.
  - *Trap:* importing eagerly in `refresh` would make a `query` after a rewrite pay a full
    import, and an edit pay two.
- **`reload`** forces the same full import whether or not a stamp moved. It is the escape
  hatch for what stamps cannot see. It leaves the undo stack alone: the stack holds
  override-layer bytes, which compose over the sources as they are now.
- **A render's identity is its layer and its sources.** The job's hash covers the override
  layer's bytes and the sources' stamps as `bring_current` last checked them, not a
  fresh read. `light_paths` refreshes first, which moves the stamps to what is on disk,
  so it refuses a render made before a rewrite, as it refuses one made before an edit.

**Measured.** The check costs one `stat` per watched layer on every tool call that reads
the stage or the scene:
- the checked-in samples compose at most three layers;
- 5 000 stats of local files take 11.7 ms (best of 10, from Python's `os.stat`, an upper
  bound on Rust's), against seconds for an import of a stage that size.
- Not measured: ALab, which composes thousands of layers and is not in the checkout.

The render path does not move. Callgrind on `crust render` of the Cornell box at `-s 2`,
one thread, before and after the change:
- every render function's count is identical, and the images are bit-identical;
- the total moved by 118 380 instructions (−0.003%), all of it in libc `memcpy` and
  `malloc`, consistent with the two binaries' output paths differing in length.

The tests that pin these behaviours are in `crust-render/src/mcp/tests.rs`, section
"Sources on disk":
- a rewritten sublayer then an in-place-class edit is `full` and renders bitwise as a
  fresh session;
- a query after a referenced layer's rewrite answers the new value;
- a render after a payload layer's rewrite shows it;
- a source that no longer composes fails `render` until it is fixed;
- `light_paths` refuses a render from before a rewrite;
- the `reload` cases.

The listing is pinned in `crust-core/tests/usd_layers.rs`: single-stage, streamed (the
reference and the payload reached from different chunks), and a `.usdz`.

### Known gaps

- **Files outside the USD layer graph are not watched.** This covers a MaterialX document
  `crust-mtlx` reads, and environment maps, which are never kept. A kept decode is
  revalidated by its own stamps. `reload` covers the rest.
- **A rewrite that keeps both the length and the modification time is not seen**, as for
  kept decodes. `reload` covers it.
- **A layer rewritten in place during an import can fault the process** under the `mmap`
  feature (the `mapping_resolver` safety note). Writers that replace the file by rename,
  such as openusd's `Layer::save` and USD's `SdfLayer::Save`, are safe.
- **No filesystem notification.** A change is seen when the next tool call looks; the
  agent is not told between calls.

## Placement (`session-placement-tools`)

`drop_to_surface` and `scatter_on_surface` compute placement against the scene the session
renders and author it as one edit (`mcp/place.rs`, through `Open::edit`). They exist
because placement through `author_usda` means guessing heights. A scene-assembly agent
would otherwise run its own surface queries against geometry that is not what renders.

### Where it is computed

- **`crust-rt`: `Scene::visit(ids, mask, f)`** hands out the pieces of the top-level
  geometries `ids` in world space, at shutter open:
  - a triangle's vertices in winding order, or the bounds of any other primitive;
  - recursing through instances (each at its `transform`, not `transform_end`);
  - each piece tagged with the top-level attach id.

  Instances match on their attach id (`placement - 1`), never on a label, so the reserved
  slots of an `InstanceHitId::Offset` give nothing. It scans the tables and keeps nothing.
  *Alternative rejected:* reading geometry back through `World::world_vertices`, which is
  private and gives up on PointInstancer slots and moving instances.
- **`crust-core::place`** turns the world and the provenance into numbers (moves,
  instances). It reads no USD and authors nothing. `World::intersect_geometry` is the
  kernel's own hit (geometric normal, no material, no opacity) over
  `(TRACE_T_MIN, ∞)` only.
- **`Provenance::geometry_of(path)`** is the reverse of `prim_of_geometry`. It scans the
  runs (one per emitting prim) by whole path components. The import now records each
  run's end beside its start. `prim_of_geometry` and light linking still read a run as
  ending where the next begins, so neither moves.
- **`Provenance::prim_frames(layer, paths, op)`** opens a stage masked to the paths with
  payloads loaded, at the scene's time code, and folds the traversal's own
  `compose_with_parent`. It returns the parent's and the prim's local-to-world (the
  import's `f32` matrices, promoted), the composed `xformOpOrder`, and whether the prim is
  `Xformable`.
  - *Why not the authoring stage:* it leaves payloads unloaded, and the opinions an
    asset's payload authors on its root, often its whole op stack, are not there.
- **`Distribution1D`** moved from `environment.rs` to `distribution.rs` unchanged, shared
  by the environment map and the scatter. Every golden, domes included, is bit-identical.

### Dropping

1. **Columns.** One ray per distinct vertex of the prim's triangles (deduplicated by bit
   pattern: a mesh vertex is a corner of about six triangles), plus a 9 × 9 grid on the
   bottom face of any other primitive's bounds.
2. **Rays.** Each goes down from the prim's top, straight above its vertex.
3. **The surface.** A ray takes the first hit that is not the prim's own and, with
   `target`, is the target's. The kernel has no per-ray id filter (mask bits 3–31 are
   light-link classes), so a ray skips a hit by re-intersecting past it.
4. **The drop.** The move is the least height of a vertex above its hit, so the vertex
   that sets it lands exactly on the surface. A negative least height raises the prim.
   The reduction is a `min` with ties broken by column, so the result is the same on any
   number of threads.

- *Why vertices, not a footprint grid:* a grid over the bounding box rests the box on the
  highest hit under it, which floats a rock on uneven ground.
- *Trap (twice):* stepping past a hit by raising `t_min`.
  - First it hung. The kernel range-tests a hit before dividing by the determinant, so a
    hit can come back at or a hair below the `t_min` it was given, and resuming at
    `t.next_up()` found the same hit forever.
  - Then it cost every render. Every intersection in the program asks for
    `(TRACE_T_MIN, ∞)`, and fat LTO propagates those constants into `Bvh::hit`. A second
    caller passing a variable `t_min` cost the Cornell box 5.1 M more instructions in
    `Bvh::hit` (+0.6%, an extra spill in its prologue) with identical images, which is
    the `TRACE_T_MIN` comment's own story.
  - `place::first_hit` now restarts the ray at the skipped hit (`World::intersect_geometry`
    takes no bounds), as the tracer's `restarted` does. A surface within `TRACE_T_MIN`
    behind a skipped one is skipped with it.
- **Alignment** turns the prim about the bottom centre of its footprint, by the shortest
  arc from up to the geometric normal under the footprint's centre, then drops the turned
  vertices along the stage's down axis.
  - *Trap:* the kernel's hit normal is the interpolated *vertex* normal on a mesh that
    authors normals, a shading normal.
  - The face normal is found by visiting the hit geometry's provenance run for the
    triangle the hit point lies on (`face_normal_at`). This is one scan per aligned prim,
    off the kernel's hot path. Pinned by `an_aligned_drop_follows_the_face_not_authored_normals`.
- **A prim and its ancestor in one call are refused.** Each move is computed against the
  scene before the call, so the descendant would take the ancestor's move on top of its
  own.

**The authored op.** The world-space move Δ becomes one `matrix4d`,
`xformOp:transform:crustPlace = P⁻¹ · Δ · P · C`. Here `P` is the parent's local-to-world
(the world after a `!resetXformStack!`) and `C` is the op's composed value, or identity.
- It is listed after the last reset, else first, and the composed ops follow in order.
  Again: a reset in the middle of the stack would otherwise discard the op.
- A second drop folds into the op, so a prim keeps one.
- One matrix covers a turn under any parent, sheared or not. A `translate` + `orient`
  pair cannot.
- `xformOpOrder` is authored whole, which shadows later source changes to it (Known gaps).

**Up axis.** Stage metadata is the root layer's, and the override layer authors no
`upAxis`. So placement reads the override layer's `upAxis`, then the input's root layer's,
alone with `sdf::Layer::open`, then falls back to Y.

### Scattering

1. **Triangles.** The target's visible triangles, in `visit`'s fixed order, filtered by
   `max_slope` on their geometric normal (from the winding as it renders), become an area
   CDF.
2. **Draws.** Instance `i` draws from `PathSampler::new(0, 0, 0, i).new_domain(seed)`:
   - domain 0 (4D): the triangle, two numbers for the square-root triangle warp
     (`utils::uniform_triangle`), and the prototype;
   - domain 1 (2D): the turn about up and the scale.

   *Trap:* the seed keys a domain, not openqmc's `frame`. `State64Bit::new` adds the
   index's high bits to the frame unchecked, so seed `i32::MAX` past 65 536 instances
   panicked in debug builds. `new_domain` wraps.

   The sampler is always the Sobol one, whatever `crust:sampler` says. So an instance
   depends on the seed, `i` and the triangle list alone, and Sobol over `i` stratifies
   the area draw.
3. **The instancer.** Each instance's world matrix `T(p)·R_up(θ)·S(s)` is taken into the
   parent's space and decomposed into `positions`, `orientations` (`quath`, which every
   USD reader knows) and `scales`. A residual above 1e-5 refuses the call: a parent that
   rotates with non-uniform scale shears the instance, which the arrays cannot express.
4. **Prototypes.** Each is a **typeless** `def` under `<instancer>/Prototypes` that
   references the prim or the asset, with an empty `xformOpOrder`.
   - An asset must be a USD file (`.usd`, `.usda`, `.usdc`, `.usdz`), since scenes come
     from USD alone. An absolute path that names a file is an asset, not a prim path.
   - The instancer's parent may not be an instance or inside one: USD reads nothing
     authored there, and the edit would save, report and render nothing.
   - *Trap:* a `def Xform` would win over the referenced `Mesh`'s type and author an empty
     Xform.
   - The empty op order drops the source's own placement for any reader. crust skips a
     prototype root's transform already.

A scatter defines a new prim, so it is `full`. A path the stage already composes is
refused: the merge cannot remove the previous prototypes' specs, so a re-scatter is
`undo` then scatter.

### Freshness

Both tools run `refresh`, then `bring_current` (which imports when a source changed), then
compute, then `edit`. So a rewrite costs one import before the placement and one after it.
A call that places nothing authors nothing and adds no undo step. The result carries the
edit's fields, `place_s`, and the `sources_changed` that `bring_current` imported.

### Measured

Release build, one machine, the largest prim of each sample:

| prim | triangles | drop, 1 thread | drop, 8 threads | scatter of 100 000, 8 threads |
|---|---|---|---|---|
| `materialx_lion` statue | 980 266 | 1.62 s | 0.41 s | 0.044 s |
| `materialx_teapot` body | 176 148 | 0.38 s | 0.08 s | 0.012 s |

Deduplicating the columns halved the lion's drop (0.92 s before, at 8 threads).

The render path does not move. Callgrind on `crust render` of the Cornell box at `-s 2`,
one thread, before and after the change:
- 4 250 609 068 instructions before and 4 250 526 128 after (−82 940), all of it in the
  import (`sdf` path ordering, `memcpy`, `malloc`);
- every render function's count is identical but `render_pass`'s, by +15 of 27 million;
- every checked-in sample renders bit-identical to goldens recorded before the change
  (`scripts/check_images.sh`).

The first version, which raised `t_min` (above), cost +5.1 M in `Bvh::hit`.

The tests live in two places:
- `crust-core/tests/place.rs`: the visitor against the shading hit, `geometry_of`,
  `prim_frames` against the rendered geometry, every drop case, and the scatter's area
  split, seed and slope;
- `crust-render/src/mcp/tests.rs`, section "Placement": every scenario of the spec, the
  scatter rendering bitwise as a fresh session of its saved layer included.

### Known gaps

- **Contact is vertex-to-face.** A sharp feature of the surface between two of the prim's
  vertices can poke through a face. A curve or implicit shape rests on its bounds.
- **Prims in one call are placed against the scene before it.** They do not land on each
  other. Call twice to stack.
- **One frame, shutter open.** An animated parent makes an aligned move differ at other
  frames. The op is a default value, not a time sample.
- **Placement follows the import's settings.** A later subdivision-level or displacement
  change moves the surface away from what was placed on it.
- **Opacity cutouts are solid** to the rays, as to `probe`'s.
- **`xformOpOrder` shadowing.** The override authors the whole list, so a source that
  later adds an op is hidden until the prim is dropped again after an `undo`.
- **CDF resolution.** The CDF is stored in f32, as fine as the sampler's numbers: a
  triangle under ~1e-7 of the target's area can be over- or under-drawn.
- **Instances stand upright.** They are not aligned to the surface normal.
- **The override layer carries no `upAxis`.** Found while writing the up-axis read: a
  session's composed stage is Y-up whatever the input says. Placement reads the input's
  root layer to compensate. Where the render reads the stage's up axis (a `DomeLight_1`
  with `poleAxis = "scene"`), a session over a Z-up input renders the dome as Y-up, unlike
  `crust render` of the input. `open_session` should copy the input's `upAxis` (and
  `metersPerUnit`) into the layer it creates.
