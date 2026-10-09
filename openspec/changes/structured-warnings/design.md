# Design

## Context

See proposal.md (Why). The current state that constrains the approach:

- **Where warnings come from.** About 140 `warn!` sites. Roughly 85% are in
  `scene/usd_import/` (products 27, lights 13, mesh 12, materials 11, settings 9,
  light links 8, instancing 8, …). The rest are in `scene.rs` (the `NoAssets`
  defaults), `color.rs`, `config.rs` (environment), `diagnostic/`, the tracer (2)
  and crust-assets (about 20).
- **The import is single-threaded.** It reads its time code from the thread-local
  `EvalTimeScope` (`usd_import/time.rs`) for exactly the reason that applies here:
  the value is the same for every read site, and threading it through every helper
  would change every signature in the module. The only rayon work during import,
  `MeshArena::commit_slots` and `scene/displace.rs`, builds from arrays already read
  and raises no warning.
- **Flood control is ad hoc.** `mesh.rs` keeps `cage_warned`, `legacy_warned` and
  `ptex_cage_warned` booleans and says "(and possibly others)".
- **Assets are negative-cached.** `assets::cached_asset` asks the loader once per
  resolved path, so a missing texture is reported once, however many materials
  reference it. The loader knows the file and the cause (not found, corrupt) but
  not the referencing prim. The core knows the prim but not the cause, and for a UV
  texture miss it logs only at DEBUG today.
- **No test captures log text.** No `tracing-test` and no log-capture writer outside
  `logging.rs`, so prefixing lines with their code breaks nothing.

## Goals / Non-Goals

**Goals:**

- A type-checked vocabulary: a code that does not exist does not compile.
- The engine returns warnings as data on `Scene`; the CLI decides what to write.
- No change to what is rendered, and no work added per ray, pixel or sample.
- A test that keeps the code enum and the user-facing reference page in sync.

**Non-Goals:**

- Render-time, environment and diagnostic-analysis warnings (logged only; see the
  spec's known gap).
- Any report or subcommand. `crust check` is the separate change `add-crust-check`;
  `crust-stats/1`, the diagnostic report and a render summary can read
  `Scene::warnings` later without touching the vocabulary.
- Changing the `AssetLoader` seam: `None` still means "fall back".
- Rewording messages beyond the code prefix and dropping "(and possibly others)".

## Decisions

### D1. A scoped thread-local collector, like `EvalTimeScope`

`load_scene` enters a `WarningScope` (a `!Send` guard that restores the previous
collector on drop, including on an early `?`). The warning macro always logs per
its policy, and records into the collector when one is present on the thread. When
the import finishes, the guard is turned into `Vec<Warning>`, which is stored on
`Scene`.

*Alternatives considered:*

- **A `tracing` layer that collects WARN events with a `code` field.** The codes
  would be strings with no compiler check. The global dispatcher would mix the
  warnings of the many imports `crust diagnostic` runs. And every library host
  (Hydra) would have to install the layer itself to get anything.
- **An explicit `&mut Warnings` parameter.** It is the most honest option, but it
  threads a value none of the helpers decide through every signature in
  `usd_import/`, the same cost `time.rs` already rejected. And it cannot reach
  crust-assets without changing the `AssetLoader` trait.

The same soundness rule as `EVAL_TIME` applies: it is sound only while every warning
in the import is raised on the importing thread. The `!Send` guard makes moving the
scope into a rayon task a compile error. A warning raised from a rayon task during
import would be logged but silently not recorded, so this goes in
`docs/architecture.md` § Invariants.

### D2. One table defines the vocabulary

A single declarative macro invocation in `crust-core/src/warnings.rs` lists every
code, one line each: the variant, the code string, its kind, its log policy and a
one-line documentation string. It generates `enum WarningCode`, `as_str()`,
`kind()` and `ALL`. A test reads `site/content/docs/reference/warnings.md` and
checks that the page and `WarningCode::ALL` list the same codes with the same kinds.
That makes it impossible to add a code without documenting it.

### D3. Codes per cause, grouped by domain

The domains follow the import's own files: `settings`, `time`, `camera`, `product`,
`aov`, `lpe`, `xform`, `mesh`, `subdiv`, `displacement`, `curves`, `volume`,
`instancing`, `light`, `light_link`, `material`, `mtlx`, `preview`, `texture`,
`ies`, `color`. Sites that report the same cause share a code. Some examples:

- the `material.fallback_default` sites at `materials.rs:487/559/582/589`;
- the non-finite input sites in `lights.rs:57/69/158/177` as
  `light.non_finite_input`;
- `instancing.rs` "instanceable but has no prototype", which appears twice;
- the products "cannot be accumulated as …" family.

What differs between such sites goes into the message, not into the code. The
expected size is 50–70 codes. Task 2.1 drafts the full table and has it reviewed
before any site moves, because a kind or name fixed now costs a version bump to
change later.

**How kinds are assigned:** a value that fails validation is `refused`. A valid
value that crust reads but renders differently from what it asks for is
`approximated`. Something that contributes nothing is `skipped`, even when a default
stands in for it. For example, "no surface shader → default grey" is `skipped`, and
"vector displacement ignored" is `approximated`.

### D4. Each code has one log policy, which replaces the booleans

- `Each` logs every occurrence. This is the default and matches today.
- `Once` logs the first occurrence, then only records. This replaces `cage_warned`,
  `legacy_warned` and `ptex_cage_warned`. The text says "(further occurrences are
  counted in the import's warnings)" instead of "(and possibly others)".

`Once` is also bounded per thread when there is no scope (outside an import), using
the same per-code flag kept in a thread-local, so the guarantee does not depend on
the caller.

A call site can also record without logging (`record_warning!`). This exists for D6,
where the cause has already been logged once.

### D5. The record and its cost

Each record holds: the code, `count: u64`, `prims: Vec<String>` (distinct, at most
16, in first-occurrence order), and `message: String` (the first occurrence's text).

- Records are kept in a small `Vec` indexed by code and appended in first-fire
  order, so the output is deterministic.
- A site formats its message only when it is logging, or when the record has none
  yet. A `Once` code at its 10,000th occurrence costs one prim-list check (linear
  over at most 16 entries) and one increment.
- 16 is enough to locate a pattern; the count gives the scale.

### D6. Asset failures: the core records with the prim, the loader explains the cause

The loader still decides what "unreadable" means and logs the cause once per file.
Its plain `warn!("{e} — …")` line becomes a `cause_warning!` with the code
(`texture.unreadable`, `ies.unreadable`, `light.map_unreadable`). That logs the
coded line and sets the record's `message` if it has none, but does not count. The
core's memoized helpers (`load_uv_texture`, `load_ptex`, the light and dome maps,
IES) take the referencing prim. Every lookup that returns `None`, on a cache miss or
a cache hit, calls `record_warning!` with the same code and that prim, without
logging. That is the occurrence that counts.

The result is one record per code:

- `message` = the loader's cause line;
- `prims` = the referencing materials and lights;
- `count` = the references.

The log names each file once. The `AssetLoader` seam and its `None`-means-fall-back
rule are unchanged.

The core-side warnings for a loader that does not decode a type at all (the
`NoAssets` defaults in `scene.rs`) become `asset.unsupported_by_host` (`skipped`).

Loader-only causes with no core counterpart are coded in crust-assets with no prim:

- `texture.udim_tile_missing` (`skipped`);
- `texture.tx_stale` (`approximated`);
- `texture.tx_convert_failed` (`approximated`).

`--auto-tx has no effect with CRUST_TEX_STREAM=0` is configuration, so it stays
uncoded.

crust-assets already depends on crust-core, so it uses the exported macros. No
dependency is added in either direction.

### D7. The log prefix is part of the macro, not the subscriber

The macro writes `[code] ` into the formatted message, so every subscriber shows it:
the CLI's, a host's, and `--log-file`. `logging.rs` is unchanged.

## Risks / Trade-offs

- [A future change raises a warning from a rayon task during import, and it is
  silently not recorded] → the `!Send` guard stops the scope from moving. A debug
  assertion in the macro fires when it runs inside a rayon worker while a scope
  exists on another thread (`rayon::current_thread_index().is_some()` during import).
  The invariant is added to `docs/architecture.md`.
- [A badly chosen name or kind becomes a breaking change later] → the full table is
  reviewed as one unit (task 2.1) before migration, and the doc-sync test makes
  every code visible in a single page.
- [Warning text changes break users who grep logs] → only a prefix is added; the
  message bodies stay the same, apart from the "(and possibly others)" wording on
  three `Once` sites.
- [Moana-scale imports raise many warnings] → recording costs O(1) plus a scan of at
  most 16 entries per occurrence, which is negligible beside the attribute read that
  found the problem. Memory is bounded by codes × 16 paths.
- [Asset counts confuse readers: references vs files] → the spec defines `count` as
  references and the message names the first file. Listing every distinct file is a
  possible later `detail` field, not v1.

## Migration Plan

Additive: a new subcommand, a new `Scene` field and a log prefix. No flag or default
changes, and no output image changes (the `check_images.sh` goldens are unaffected,
because warnings never touch pixels). Rollback is a revert.

## Open Questions

- Whether a later `detail` object (structured fields such as `file`, `value`,
  `fallback`) should accompany `message`. It can be added without a version bump, so
  it is deferred until a consumer asks for it.
