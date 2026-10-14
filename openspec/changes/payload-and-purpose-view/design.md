# Design

## Context

See `proposal.md` (Why). The parts of the code this change touches, as they are today:

- **Purpose** is decided in one place, `prune_reason` (`usd_import/mod.rs`), which
  prunes `proxy` and `guide` subtrees where the purpose is authored. Four walks call
  it, and they must agree:
  - the traversal (`traverse_into`);
  - the placement count (`count_placements`, which decides instance or bake);
  - the prototype walk (`instancing.rs`, `prototype_prunes`);
  - the listing (`listing.rs`, `crust ls`).
- **Material binding** is `bound_material` (`materials.rs`). It calls openusd-schemas'
  `compute_bound_material("full")`, which falls back to the all-purpose binding itself.
- **Every composed stage opens through `open_stage`** (`mod.rs`, `InitialLoadSet::LoadAll`,
  optionally masked). That covers:
  - the single-stage import and each streamed chunk;
  - the masked camera stage read for the screen projection;
  - `crust ls`;
  - the session's in-place rebuilds (`usd_import/live.rs`: materials, lights, the camera).

  The index stage (`LoadNone`) is opened separately and read only for settings, links
  and the chunk list.
- **The session imports through the CLI's argument parser.** `check_args`
  (`mcp/scene.rs`) parses `crust check -i <layer>` and the import is the CLI's
  `import_checked`. `render_final` parses `crust render -i <layer>` and runs
  `render_and_write`.
- **openusd's load-rule API.** `pcp::LoadRules` is a value type, a table of path → rule,
  with `load_with_descendants`, `load_without_descendants`, `unload`, `all` and `none`.
  `Stage::set_load_rules` replaces a stage's table. `Prim::is_loaded` is C++ `IsLoaded`.
  The stage builder only takes `LoadAll` or `LoadNone`, and the per-prim `has_payload`
  is not public.

## Goals / Non-Goals

**Goals:**

- One definition of a *view*: a load set, the drawn purposes and the binding purposes.
  It is an import input, expressible as CLI flags, and the session holds it.
- A session image is bit-identical to `crust render <output>` plus the view's flags, by
  construction rather than by a parallel code path.
- The default import (no flags, no authored purposes) is unchanged in output and cost.

**Non-Goals:**

- Applying a view change in place: walking only the resynced subtree, as
  `StageSink::load_rules_changed` reports it. That is phase 2, a separate change (below).
- `model:drawMode` stand-ins (`bounds`, `origin`, `cards`) for unloaded models.
- `crust ls` following a view. It keeps listing what the final render uses.
- Listing a stage's payload-carrying prims for the agent. An agent finds them through
  `probe` and `load` (see D6).

## Decisions

### D1. The view is an import input, not a live stage

A view change re-imports the saved layer with the view's flags. This is the same path
an edit already takes when it cannot be applied in place.

- **Why:** reproducibility needs no second implementation. The session builds its import
  arguments by parsing `crust check -i <layer> <view flags>`, so "the session renders
  what `crust render <output> <view flags>` renders" holds the way it already holds for
  the final view. The cost of a re-import grows with what is loaded. That is small in
  the decision view, which is the point of a decision view.
- **Alternative: a DCC-style live stage** that loads a payload with `Stage::load`, walks
  the resynced subtrees, builds their per-mesh BVHs and rebuilds the top level.
  Rejected for now:
  - **Geometry order.** Appended geometry is numbered after everything already built,
    which a fresh import does not do. Images would differ from the CLI's at noise level,
    and that is the bit-identity the session promises.
  - **Decisions made after the last chunk.** The import makes three of them only once
    every chunk is read: instance or bake (from the final placement counts), light-link
    classes (resolved over every receiver), and the top-level commit. A partial load
    would have to redo all three.
  - **Prototype numbering.** On a live stage, a load can renumber `/__Prototype_N`, which
    every prototype-keyed cache depends on (`ImportCaches::epoch`).

  Phase 2 can do this behind a `CRUST_*` switch whose off side is this change, so the
  A/B is honest.

### D2. The view's shape and where it lives

`UsdImportOptions` gains three fields, so every host and `StageFacts` (which clones the
options) carry them:

- **`purposes: Option<PurposeSet>`**: `render`, `proxy`, `guide` as bits. `None` means
  the stage's `includedPurposes`.
- **`binding_purposes: Option<Vec<BindingPurpose>>`**: ordered `full`, `preview`, `all`.
  `None` means the stage's `materialBindingPurposes`.
- **`payloads: LoadSet`**: a root rule (`all`, the default, or `none`), plus a
  `BTreeMap<path, Load | Unload>`.

  A map, not a sequence: each path has at most one rule, so the flags are
  order-independent, deterministic to print, and round-trip. It matches
  `pcp::LoadRules`, which is also one rule per path.

The effective purposes and binding purposes (authored values with the overrides
applied) are resolved once, in `read_index`, and recorded in `StageFacts`. An in-place
settings update (`crust_core::live`) then refuses, as `Structural`, any edit that would
move them. That is how an authored `includedPurposes` edit becomes a full re-import
(spec: "Authored purposes re-import"). Under the decision view the overrides win, the
effective values do not move, and the edit is applied in place with an unchanged image.
The session reports which authored values the view overrides.

### D3. The load set is applied in `open_stage`

`open_stage` takes the load set. With the default (`all`, no per-path rules) it opens
`LoadAll` exactly as today: same code, same cost. Otherwise it opens `LoadNone`, and
calls `set_load_rules` before the first prim query. openusd composes lazily, so nothing
is recomposed; the `--stats` "Open stage" row on the fixture confirms this.

Every composed stage goes through `open_stage`, so the streamed chunks, the single
stage, the camera stage and the session's in-place rebuilds all see the same
composition. That keeps the existing "in place ↔ full re-import" bit-identity pair true
under a view. `crust ls` passes the default set.

**The render camera.** When a camera is named (`--camera` or `RenderSettings.camera`,
both known from the index stage before the traversal),
`load_without_descendants(camera)` is added to the rules: the payloads on its ancestry
load, nothing below them. That rule is internal, never part of the printed flags.
Without a named camera, the first camera of the loaded stage is used, as today.

**Validating `--load` and `--unload`.** Each named path must compose on the index stage
once the payloads on its own ancestry are loaded. Otherwise the import fails with a new
`Error` variant naming the prim, as `CameraNotFound` does. A `--load` whose prim already
resolves loaded under the other rules is a CLI `warn!` (spec), not a coded import
warning: it is about the command line, not the stage.

### D4. Purposes in the one pruning rule

`prune_reason` takes a `PurposeFilter`, built once per import from the effective
purposes. All four walks pass it; the listing passes the final filter. The rule's
order (abstract, inactive, purpose, invisible) and its "decided where authored" logic
are unchanged.

An authored token outside `default` / `render` / `proxy` / `guide` raises a new
`settings.unknown_purpose` warning (`refused`) and is dropped. For
`materialBindingPurposes`, the accepted tokens are `full`, `preview`, `""` and
`allPurpose`.

**Proxy only for an unloaded payload.** When `render` and `proxy` are both drawn, a prim
that authors `purpose = "proxy"` is pruned iff `prim.is_loaded()`. C++ `IsLoaded` is
true exactly when no payload-carrying prim at or above the prim is unloaded (none at
all counts as loaded). So "skip a proxy unless its nearest payload-carrying ancestor is
unloaded" needs no access to openusd's private `has_payload`. The call is made only at
prims that author a proxy purpose, and only in this mix.

- *Alternative: `proxyPrim` pairing.* Rejected. It works only where assets author the
  relationship, which ALab's proxies are not known to do.
- *Alternative: strict USD, which draws both.* Rejected because it makes the session's
  `load` show duplicates. The user chose the crust rule.

**Trap: prototypes.** Load rules are not consulted in `/__Prototype_N` namespace, so
`is_loaded()` on a prototype prim says nothing about the instance being placed. ALab
places 12 605 instances from 380 prototypes, so this is the common case there, not an
edge. The prototype walk therefore takes the decision from the *instance* prim: its
`is_loaded()` is passed into `collect_proto_parts` and `prototype_prunes`.

That is correct only if a loaded and an unloaded instance of one asset never share a
prototype. C++ USD guarantees this: the payload's inclusion is part of the prim index
the instancing key is built from. openusd's behaviour is checked by a fixture test (two
instanceable, payloaded placements of one asset, one loaded). If openusd shares them,
the prototype's parts are cached per (prototype, loaded) instead, under the same epoch.

### D5. Binding purposes are an ordered list

`bound_material` keeps its ancestor walk to the nearest prim carrying
`MaterialBindingAPI` and resolves the list in order: the first purpose that binds the
prim wins, and `""` / `allPurpose` / `all` stands for the all-purpose binding.

openusd-schemas' public `compute_bound_material(p)` is the pair `[p, ""]`: it falls back
to all-purpose itself. That is exact for the default (`full, all`), for the decision
view (`preview, all`) and for `all` alone. A list with two named purposes, or a named
purpose without `all`, needs the strict per-purpose resolution. openusd-schemas has it
as the private `bound_material_for_single_purpose`.

**Decision:** expose it upstream as a public method on `MaterialBindingAPI`. The
workspace already tracks openusd's `main`, and upstreaming is how earlier gaps were
closed (`retire-openusd-workarounds`). Re-implementing binding strength and collection
bindings in crust would make a second binding resolver that can drift from the
reference.

- *Alternative:* accept only `[p]`, `[p, all]` or `[all]` until upstream lands. Kept as
  the fallback (see Risks).

### D6. The session's view

- **State.** `Open` gains `view: View`: the preset name when the view is one, the
  `LoadSet`, and the two optional lists. `View::flags()` prints the `crust render`
  arguments, empty for `final`. `check_args(layer, &view)` parses
  `["crust", "check", "-i", layer, flags...]`, so the session imports the flags it
  reports, by construction.
- **`load` and `unload`** apply USD semantics to a `pcp::LoadRules` value the view owns,
  then convert it to the `LoadSet` map. A round-trip test checks rules → flags → rules.
  The session's authoring stage is not used for this: it loads payloads wherever a
  `query` reaches (its own design), so its table is not the view.
  - Validation loads the path's ancestry on the authoring stage (as a query would) and
    checks that the prim exists.
  - A failed validation or a failed import leaves `view` and `scene` as they were, as a
    failed `undo` does.
- **Finding what to load.** An agent finds what to load by probing. `probe` names the
  proxy prim it hit, and `load` of that prim loads the payloads on its ancestry, which
  includes the asset's (USD `Load` semantics). No payload listing is added.
- **`set_view(preset | purposes | binding_purposes)`.** A preset replaces the whole view.
  The two lists replace their parts, and `null` means the stage's own value.
- **What stays as it is:**
  - `render_final` already parses `crust render -i <layer>` without view flags, so it
    is the final render by construction.
  - `undo` restores layer bytes only.
  - Resuming starts from `open_session`'s `view` argument.
  - The layers the session watches (`live-session-source-reload`) are the layers the
    last import composed. An unloaded payload's layers are not watched until it is
    loaded, which is correct: a change to them cannot affect the image.
- **Results.** Each result carries
  `view: { preset, payloads: { root, load: [...], unload: [...] }, purposes, binding_purposes, flags }`.
  The JSON key order is fixed.
- **Server instructions.** The MCP server's instructions text gains a short paragraph:
  - a session opens in the decision view;
  - `probe` then `load` judges an asset, and `set_view(binding_purposes=["full","all"])`
    judges its look;
  - `render_final` is always the final render.

## Risks / Trade-offs

- **[Agents judge looks on preview materials or proxies]** → every scene result carries
  the view and its flags, and the server instructions say what the decision view hides.
  An edit result also says when the view overrides an authored purpose (D2).
- **[The decision view is still slow when heavy geometry lies outside payloads]** (ALab's
  sublayered fur, 9.4 M cubic spans, if its purpose is `default` or `render`) → this is
  measured, not designed around. The design record gets the decision-view `--stats` on
  ALab frame 1004, and `--purposes proxy` without `render` is the lever if the fur is
  render purpose.
- **[Proxies inherit a `full` binding from the asset root]** and so decode UDIMs in the
  decision view → `preview` is tried first, so this happens only when a proxy has no
  preview binding. The same ALab measurement says whether it does.
- **[openusd shares a prototype between loaded and unloaded instances]** → the fixture
  test of D4 catches it, and the per-(prototype, loaded) cache key is the fix.
- **[Upstream does not expose strict binding resolution in time]** → binding-purpose
  lists are limited to `[p]`, `[p, all]` and `[all]`. A longer authored
  `materialBindingPurposes` then uses its first named purpose plus `all`, with a
  `settings.unknown_purpose`-style `approximated` warning. That is recorded as a known
  gap instead of a silent approximation.
- **[Every view change is a full re-import]** → acceptable while little is loaded, and
  exactly what phase 2 addresses. `import_s` in every result makes the cost visible.
- **[Spec merge order]** → three of the `mcp-session` requirements this delta modifies
  are also modified by `live-stage-scene-deltas`, `live-session-source-reload` and
  `add-light-path-inspector`. This delta is written over their text, so it archives
  after them.

## Migration Plan

- **CLI:** nothing changes without the new flags.
- **Session clients:** a session now opens in the decision view. `open_session(view =
  "final")` restores the old behaviour. The only clients are this repository's tests,
  which pass `final` where they compare with the CLI, and Claude Desktop, which reads
  the schema and instructions each session.
- **Rollback:** revert. No file format, layer or report version changes. `crust-check/2`
  gains effective-setting entries, which adding settings does not version.

## Phase 2 (follow-up change, not this one)

Apply `load` / `unload` / purpose changes in place, DCC-style:

1. Keep the composed stage in the session.
2. On `StageSink::load_rules_changed`, walk only the resynced subtrees into the existing
   world, reusing each per-mesh BVH (`Arc`).
3. Redo the post-last-chunk decisions for what changed: placement counts and instance or
   bake, light-link classes, the top-level commit.
4. Accept noise-level differences from a fresh import, proven 1/√N.
5. A/B it against this change with a `CRUST_*` switch on ALab.

This change's stateless path stays the reference.
