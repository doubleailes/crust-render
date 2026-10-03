# Code, technical-debt and documentation audit — 2026-10-03

A read-only audit of the whole workspace at `70a7f38`:
- about 85k lines of Rust across the seven crates;
- the scripts, manifests and CI;
- `docs/`, `openspec/` and the `site/` user documentation.

It covers bugs found along the way, code cleanup, technical debt, and documentation.

Every finding below was checked against the code, not inferred from file names. Those
marked **✔** were also re-read independently while compiling this document. Line
numbers refer to `70a7f38`.

**Effort:** **S** is under an hour, **M** is about half a day, **L** is more.
**⚠** marks a change on a hot path: it needs the per-function callgrind comparison from
`docs/architecture.md` (debt item 3) and a `check_images.sh` run, not just a passing test
suite.

Findings already planned in an OpenSpec change say so in their *Plan* column.
[`harden-usd-import`](../openspec/changes/harden-usd-import/proposal.md) covers §1.1–1.4
and §1.8.

## Contents

0. [What came back clean](#0-what-came-back-clean)
1. [Bugs](#1-bugs)
2. [Duplicated logic](#2-duplicated-logic)
3. [Long functions and oversized files](#3-long-functions-and-oversized-files)
4. [API shape, dead code and rule violations](#4-api-shape-dead-code-and-rule-violations)
5. [Tooling, CI and manifests](#5-tooling-ci-and-manifests)
6. [Tests](#6-tests)
7. [Documentation](#7-documentation)
8. [Suggested order](#8-suggested-order)

---

## 0. What came back clean

These were checked and need no action:

- **No `TODO`/`FIXME`/`HACK`/`XXX`** in any crate or in `scripts/`.
- **The `unsafe` audit matches CLAUDE.md.**
  - crust-jit has exactly four `#[allow(unsafe_code)]` blocks (`lib.rs:82, 213, 263,
    287`), each with a SAFETY comment.
  - crust-core's only `unsafe` is the test-only `GlobalAlloc`
    (`scene/subdiv.rs:1973`, under `#[cfg(test)]`).
  - Every other crate is `forbid(unsafe_code)`.
- **The environment is read only in `crust-core/src/config.rs`.**
- **No asset decoding in crust-core.**
- **Logging levels follow the rules.**
  - The importer's only `info!` (`usd_import/mod.rs:913`) fires once per render.
  - Guided renders add at most two bounded INFO lines.
  - Nothing logs per ray.
- **No `use super::*` between `usd_import/` siblings.** All six uses are in
  `#[cfg(test)]` modules.
- **Env switches agree everywhere.** All 22 `CRUST_*` switches and their defaults agree
  across `Config::default`, `docs/architecture.md` § Environment switches and
  `site/.../reference/environment-variables.md`.
- **CLI flags are documented.** Every clap flag is in
  `site/.../reference/command-line.md`. `-b/--bucket` is the exception, and it is
  hidden on purpose (see §4.10).
- **Every `crust:*` USD attribute read by the code is documented** under
  `site/content/docs/usd/`, with matching defaults.
- **Every `--example` and `scripts/…` path** referenced from markdown exists.
- **No broken relative markdown links.** Both `](…)` and Zola `@/` links were checked.
- **Duplicate dependency versions are all transitive** and can't be fixed here:
  - bitflags 1/2 via cranelift-jit → region;
  - hashbrown 0.16/0.17 via gimli;
  - miniz_oxide 0.8/0.9;
  - syn 2/3.
- **The remaining `#[allow(clippy::…)]`s carry a measured justification:**
  `large_enum_variant`, `vec_box`, `module_inception` and `excessive_precision`.
- **Mip reduction is not duplicated by accident.**
  - `reduce_half` / `reduce_half_linear` and Ptex `reduce_quad` / `reduce_triangle` are
    separate on purpose, documented and pinned by tests.
  - The shared trilinear selection already lives in `mip_filter.rs`.

---

## 1. Bugs

Fix these before any cleanup. Each one either kills the process on bad input or
renders a silently wrong image.

| # | Where | Problem | Effort | Plan |
|---|---|---|---|---|
| 1.1 ✔ | `usd_import/mesh.rs:1346` (`triangulate`) | `let fc = fc as usize`. A negative `faceVertexCounts` entry becomes `usize::MAX`, and `offset + fc` overflows (debug: panic; release: wraps, then an out-of-bounds index). With `panic = "abort"` the process dies. Only the subdivision paths run `validate_cage`, and a subdivision failure falls back to `cage(...)` → `triangulate`, so **every** mesh reaches it. | S | `harden-usd-import` |
| 1.2 ✔ | `usd_import/shapes.rs:243`, `:235`; `usd_import/volume.rs:43` | Three unchecked inputs: <br>• A negative `curveVertexCounts` entry is cast the same way; `&pts[offset..offset+cnt]` then panics. <br>• `widths[0]` panics on an *authored* empty `widths` (the `unwrap_or(vec![1.0])` only covers an unauthored one). <br>• `nx * ny * nz` on `crust:volume:gridDims` can overflow. | S | `harden-usd-import` |
| 1.3 ✔ | `usd_import/settings.rs:139-140, 133-134` | `crust:samplesPerPixel` and `crust:maxDepth` are cast with `as u32`, so -1 becomes about 4 billion. `resolution` is cast with `as usize`, so a negative width asks for an exabyte framebuffer. `crust:minSamplesPerPixel` at `:143` is guarded, with a comment describing exactly this hazard. | S | `harden-usd-import` |
| 1.4 ✔ | `usd_import/xform.rs:217` (`resets_xform_stack_at`) | It probes six schema types (`Xform`, `Mesh`, `Sphere`, `Camera`, `SphereLight`, `RectLight`) and returns `false` for anything else. `!resetXformStack!` is therefore silently ignored on `BasisCurves`, `PointInstancer`, and Disk/Cylinder/Distant/Dome lights, which is a wrong transform with no warning. It also costs up to six schema `get`s per prim at four call sites (`mod.rs:189, 291`, `instancing.rs:390`, `camera.rs:136`). openusd defines the reset as `xformOpOrder[0] == "!resetXformStack!"`, which the local composer already reads. | M | `harden-usd-import` |
| 1.5 ✔ | `tracer/path.rs:135, 171` vs `:1190-1194` | The guide/BSDF mixture pdf is clamped with `.max(1e-4)` on the bounce side and **not** on the NEE side. Where the mixture pdf falls below 1e-4, the two MIS weights use different densities. That bias is small, but it breaks the "guide mixture ↔ NEE" pair in CLAUDE.md. Fix with one `GuidingField::mixture_pdf(pos, dir, bsdf_pdf)`, and decide on purpose whether the clamp belongs on both sides. | S ⚠ | — |
| 1.6 ✔ | `scripts/check_images.sh:33`, `scripts/bench_scenes.sh:67` | Both build only `if [ ! -x "$BIN" ]`. After a code edit, `check_images.sh check` re-renders with the **old** binary and reports "All scenes bit-identical". That defeats the gate CLAUDE.md relies on. Always run `cargo build --release -p crust-render`; it is incremental, and `check_images.sh:36` already does it unconditionally for `exr_diff`. | S | — |
| 1.7 ✔ | `samples/materialx_teapot.usda:23`, `materialx_lion.usda:23`, `materialx_showcase.usda:23, 31` | They reference `@/home/philippe.llerena/Workspace/samples/MaterialXTeapotLion-1.0/…@`, so they fail on every other machine. The site quick-start (`getting-started/quick-start.md:107`) recommends `materialx_showcase.usda` to new users, and `check_images.sh:39` renders every `samples/*.usda`. Use relative paths and document where to unpack the asset (§7.13). | S | — |
| 1.8 | `usd_import/instancing.rs:347-381` (`prototype_prunes`) | Nested native instances are still skipped because of an openusd 0.5 assertion. The design record says 0.6.0 fixed it, and the workspace is on 0.7. Geometry is lost, and the WARN still blames "openusd 0.5". | M | `harden-usd-import` |
| 1.9 | `usd_import/mesh.rs:857`, `shapes.rs:228` | Primvar interpolation is handled inconsistently. UVs read the `interpolation` metadata only to separate `faceVarying` from everything else, so a `uniform`/`constant` `st` is indexed as `vertex`. Curve widths infer interpolation from the array length, so `varying` widths on cubic curves silently fall back to `widths[0]`. See §2.5 for the shared fix. | M | — |
| 1.10 | `usd_import/materials.rs:673` | `srgb_to_linear` is actually gamma 2.2, yet shares its name with the real sRGB transfer function in crust-assets. Rename it `gamma22_to_linear` before someone "fixes" a caller. | S | — |
| 1.11 | `crust-rt/src/scene.rs` (`Expansion::push_geometry`) | *Found during the cleanup, not in the original pass.* A degenerate disk or cylinder, or an instance of an empty scene, returns before `input.geoms.push(table)`. The `GeomTable` list is indexed by `geom_id`, so every later geometry reads its neighbour's entry: a skipped disk followed by a triangle mesh panics at commit (`bvh/mod.rs:345`, index out of bounds), and other orders read the wrong `normal_base` / `tri_base`, which means wrong shading normals. Always push the table (a default entry for a skipped geometry), and add a regression test. | S | — |

---

## 2. Duplicated logic

These are the highest-value cleanups. Several of the splits in §3 get easier once these
helpers exist.

### 2.1 USD attribute reads: about 40 copies of one pattern (M)

Where: `usd_import/{lights, light_links, mesh, shapes, xform, mod, instancing, settings,
materials, preview}.rs`.

`.get_at::<sdf::Value>(eval_time()).ok().flatten()` plus a hand-written `match` is
repeated about 40 times. The type decoders exist in parallel copies that have **drifted
apart**:

- **f32:** `attrs::custom_f32` / `attr_f32`, `materials::shader_input_f32` (`:769`),
  `shapes::sphere_radius` (`:26`) and `xform::value_as_f32`. Only the xform copy accepts
  `Half`.
- **vec3:** `attrs::custom_color3` and `materials::custom_vec3` (`:680`) are an exact
  copy. `shader_input_vec3` (`:802`) and `attr_color3f` (`attrs.rs:257`, used for light
  `color`) reject `Vec3d`.
- **bool:** `attrs::custom_bool` accepts an Int; `shader_input_bool` (`:780`) does not.
- **token-or-string:** decoded three times, in `custom_token`, `shader_info_id`
  (`materials.rs:271`) and `has_shader_id` (`:298`). `sdf::Value::as_str()` already does
  this, and is used in `mod.rs:480` and `preview.rs`.
- **arrays:** `mesh_arrays` (`mesh.rs:753`), the `int_array`/`float_array` closures
  (`mesh.rs:1024`), `instancing::value_vec3f_array` (`:1102`) and the inline reads in
  `shapes.rs:167-207`.

**Fix:**
- Add one `value_at(&Attribute) -> Option<sdf::Value>` plus
  `decode_{f32, bool, vec3, token, f32s, i32s, vec3fs}(&Value)` in `attrs.rs`, and make
  every `custom_*` / `attr_*` / `shader_input_*` a one-liner on top.
- This also puts the per-read `eval_time()` thread-local lookups in one place, which is
  the first step if the import time is ever threaded explicitly (CLAUDE.md).

### 2.2 The prim-pruning rule is written three times (S)

Where: `mod.rs:181` (`count_placements`), `mod.rs:274-299` (`traverse_into`) and
`instancing.rs:312` (`prototype_prunes`). The rule: abstract, inactive, non-render
purpose, invisible.

`count_placements` must make the same decision as the traversal, or a prototype's
shared/unshared verdict (and so its adaptive level) changes.

**Fix:** one `prune_reason(prim) -> Option<Reason>`, with the caller doing the logging.

### 2.3 "reset ? local : parent · local" is written four times (S)

Where: the same four sites as §1.4. Fix with one `child_world(parent, local)` helper.
This is planned in `harden-usd-import`.

### 2.4 Schema dispatch probes up to ~12 schemas per prim (M)

Where: `mod.rs:345-388`, `instancing.rs:181-268` and `xform.rs:183-236`, for 31
`X::get(stage, prim.path().clone())` calls in total.

Each `get` re-reads the prim's type name, and an `Err` from one is silently dropped as
"not this type".

**Fix:** match on `prim.type_name()` once (openusd 0.7 `usd/prim.rs:418`), shared by
the traversal, the prototype walk and the xform fallback.

### 2.5 Primvar reading (M)

Two pieces of code disagree on interpolation (§1.9). `UvSource::index_at`
(`mesh.rs:801`) also duplicates `UvChannel::value_index` (`subdiv.rs:107`).

**Fix:** one `Primvar { values, indices, interpolation }` reader with an
`index_for(face, face_vertex, point, curve)` lookup.

### 2.6 Asset load + cache is hand-written six times (S/M)

Where: `lights.rs:184` (IES), `:477` (RectLight texture), `:708` (dome, which has no
cache), and `materials.rs:497, 551, 592`.

Each copy repeats the same pattern: start a timer, `assets.load_*`, add to
`asset_time`, then a path-keyed cache that also remembers failures.

Two related problems:
- `emit_dome_light` (`lights.rs:678`) takes `stage_path`, `assets` and `asset_time`
  separately. That is the only reason `ImportCtx.stage_path` / `.assets`
  (`mod.rs:251, 257`) exist, and they duplicate `ImportCaches` fields.
- There are two path rules: light textures, IES and the dome go through
  `asset_value_path`, while preview textures go through `attribute_asset_path`, which
  anchors to the authoring layer.

**Fix:**
- one `ImportCaches::load_cached(map, path, loader)`;
- pass `&mut caches` to the dome;
- use one asset-path rule.

### 2.7 The camera frame is computed twice (S)

Where: `camera.rs:20-46` (`build_camera`) and `:84-104` (`screen_projection`) each
derive eye, forward and up.

The two must agree, and only a `debug_assert!` at `mod.rs:787` checks that.

**Fix:** one `camera_frame()` that both use.

### 2.8 UDIM tile sweep: five copies (S/M)

Where: the `for v in 0..10 { for u in 0..10 }` plus `exists()` loop appears at:
- `crust-assets/src/lib.rs:400` (`tile_sources`)
- `uv_texture/mod.rs:205`
- `uv_texture/mod.rs:270`
- `tiled/stream.rs:102`
- `crust-render/examples/maketx.rs:96`

Two more duplications go with it:
- `1001 + u + 10*v` is hard-coded at `tiled/stream.rs:125, 153` and `maketx.rs:100`,
  even though `udim::udim_number` exists (`uv_texture/udim.rs:64`).
- `contains("<UDIM>") || contains("<UVTILE>")` is repeated at `lib.rs:402`,
  `stream.rs:90` and `maketx.rs:96`, even though `TileToken::detect` exists.

`StreamingTexture::open` (`stream.rs:102-160`) also duplicates its whole
open / mip-space-mismatch / intern match between the tiled and single-file arms.

**Fix:** one `pub fn existing_tiles(path) -> Vec<(u32, PathBuf)>` in `udim.rs`, used
everywhere.

### 2.9 Texture-cache machinery is duplicated between `.tx` and Ptex streaming (M ⚠)

- **Microcache:** the thread-local FIFO microcache, with its `Option<FnOnce>` take-dance,
  exists twice: `tiled/cache.rs:775-815` and `ptex_stream.rs:587-635`.
- **MB→bytes budget helpers, with different types:**
  - `TileCache::budget_of → u64` (`cache.rs:443`)
  - `ptex_stream::budget_bytes → usize` (`:58`)
  - `stream_min_bytes_from_env` (`:101`) and `lib.rs:926`, each re-doing
    `* 1024 * 1024`.
- **Default-budget constants:** two of them, `DEFAULT_BUDGET_BYTES` (`cache.rs:126`) and
  `DEFAULT_CACHE_MB` (`ptex_stream.rs:49`).
- **Stale comment:** `ptex_stream.rs:180` says "Four rather than that cache's two", but
  the tiled microcache is now 16 sets × 4 ways (`cache.rs:718-721`). The same claim
  appears at `tests/ptex_stream.rs:225`.

**Fix:**
- `Config::{tex_cache_bytes, ptex_cache_bytes, ptex_stream_min_bytes}` in crust-core;
- optionally, a small generic `micro_lookup` over a `LocalKey`.

### 2.10 Integrator copy-paste (S–M ⚠)

- **Russian roulette** is pasted three times in `trace_path` (`path.rs:922, 1010, 1263`).
  Extract `roulette(&mut beta, v, depth, stats) -> Option<f32>`.
- **Chromatic medium correction** (`Vec3A::new(e.x.exp(), …)`) appears twice
  (`path.rs:993, 1075`).
- **`exp3`** exists privately at `subsurface.rs:231`, inlined at `medium.rs:127`,
  `volume.rs:546` and `path.rs:993, 1075`, and as a closure at `closure/mx.rs:347`.
  Move it to `utils`; the result is bit-identical.
- **Scattered-ray rebuild:** rebuilding the scattered ray with cone, time and mask is
  written twice (`path.rs:953, 1031`).
- **Surface NEE:** about 90 lines inline at `path.rs:1126-1211`, mirroring
  `volume_nee`. Extract `surface_nee(...)`.
- **Mixture pdf:** written three times (§1.5).
- **Light density:** `lights.density(..).max(1e-6)` is repeated at
  `path.rs:363, 440, 721, 1182`, which are `LightList::density`'s only callers. Fold the
  clamp into `density` (`light/list.rs:337`).
- **Ray epsilon:** `0.001` is a magic number at about 14 non-test sites (`path.rs:470,
  586, 661, 803, 835, …`, `light_cache.rs:201, 247`, the volume calls), while
  `subsurface.rs:242` already names it `TRACE_T_MIN`. Make it one shared `const`.

### 2.11 `light_cache.rs` re-implements integrator visibility (M)

- `train` (`:247-262`) re-implements `shadow_transmittance` / `cutout_shadow`
  (`path.rs:470-478`): "occluded, then `cutout_through` if there are cutouts, else 0".
  That is the cutout-visibility invariant pair, so it should be one
  `pub(crate) fn surface_visibility(world, ray, t_max, stats) -> f32`.
- The tile-domain formula `(i>>8) + (j>>8)*4096` is duplicated at `light_cache.rs:186`
  and `tracer/mod.rs:724`. Make it a `pixel_tile(i, j)` helper.

### 2.12 Smaller duplicates (S)

- **`render_pass` dispatch:** `if profiling { advance_pixel::<true> } else { ::<false> }`
  is pasted twice (`tracer/mod.rs:532, 572`).
- **`stats.rs`:**
  - the `count` closure is redefined three times (`:900, 1071, 1088`);
  - the Ptex "mixed report… as this line once did" comment is duplicated verbatim
    (`:1189, 1206`).
- **Selection floor:** `closure/mod.rs` uses `.max(0.02)` seven times; make it a named
  constant.
- **`luminance`:** `fn luminance` at `closure/mod.rs:669` only wraps
  `utils::luminance`; import it instead.
- **Two `Frame` types:** `closure/mod.rs:64` (tangent and rotation) and
  `openpbr/lobes.rs:20` (normal only). The OpenPBR doc ("plus cached view / half / light
  vectors") is stale, since the struct holds only `n, t, b`.
- **Volume interval folds:** the `start`/`end` folds over `active_intervals` are
  duplicated (`volume.rs:459-460, 551-552`).
- **Mesh-source fields:** `RefinedUvs` (`subdiv.rs:116`), `UvSource` and the
  `TessellatedMesh` optional-UV pair all describe the same thing, with three copied
  conversion blocks (`mesh.rs:1128-1143, 1157-1161`). The same function also sets the
  subdivision setup itself up twice; see §3.1.
- **Render-setting tokens:** the parse-a-token-or-WARN-and-use-default block appears
  three times in `settings.rs` (`:157, 166, 178`). Make it one
  `parsed_token::<T>(prim, name, default)`.
- **Colour helpers:**
  - `examples/tex_probe.rs:315` copies `srgb_to_linear` (f64);
  - `examples/exr_diff.rs:17` re-implements EXR loading instead of using
    `crust_assets::read_exr_rgb`.
- **crust-rt fixtures:**
  - `uv_sphere` exists three times (`examples/ray_throughput.rs:34`,
    `benches/traversal.rs:24`, `examples/traversal_probe.rs:19`);
  - `triangle_scene`, `sphere_grid_scene`, `instance_scene` and `ray_batch` are
    near-verbatim copies between `ray_throughput.rs:66-213` and
    `benches/traversal.rs:60-129`.

  Use one shared `#[path]` fixtures module.
- **Benchmarks:** `crust-mtlx/examples/mtlx_bench.rs` and
  `crust-jit/examples/jit_bench.rs` are about 90% identical (procedural texture, shading
  points, `time_ab`). Merge them into one example in crust-jit with reference /
  optimised / JIT columns.
- **Scripts:** `bench_scenes.sh` and `bench_ab.sh` duplicate scene-path resolution
  (`:83-90` / `:85-90`) and the `--stats` parsing.

---

## 3. Long functions and oversized files

Each split goes in its own PR. Prove each one is codegen-neutral with `check_images.sh`
(bit-identical) and, on hot paths, callgrind.

| File (lines) | Problem | Proposed split | Effort |
|---|---|---|---|
| `crust-core/src/scene/subdiv.rs` (2 396) | `tessellate_adaptive` is **588 lines** (`:729-1316`); `subdivide` is 228 (`:170`); about 1 070 lines of inline tests (`:1323-2396`). The two functions share copied setup: `validate_cage`, `expand_crease_runs`, `validate_corners`, and the scheme mapping (`:205-217` vs `~:752-765`). | `scene/subdiv/{mod, topology (shared prepare_cage), uniform, adaptive, normals, tests}.rs`. Break `tessellate_adaptive` into `CageEdges::build`, `rate_edges`, `emit_selected_faces` and `emit_cage_faces`, with one `TessBuilder` holding the ten output vectors. | L |
| `crust-core/src/scene/usd_import/mesh.rs` (1 916) | `mesh_source` is 252 lines (`:922`) with 7 parameters, three of them derived from the material at both call sites (`mesh.rs:469-476`, `instancing.rs:197-202`); pass `&dyn Material` instead. The per-face statistics merge (`:1100-1112`) belongs in a `SubdivPolicy::record` method. | `mesh/{arena, source, faces, bake, tests}.rs` | M |
| `usd_import/mod.rs` | `load_scene` is 324 lines (`:528`); `traverse_into` is 171 (`:263`). The single-stage and streaming branches (`:659-722`) repeat the `count_placements` + `traverse_into` sequence. | Phases: open the index stage, resolve settings, traverse, choose the camera, finish the world, record stats. One loop over `Option<chunk>`. | S/M |
| `crust-core/src/tracer/path.rs` (1 497) | `trace_path` is **642 lines** (`:748-1390`) with 12 parameters. | Extract the helpers in §2.10; see §4.1 for the parameters. | M ⚠ |
| `crust-core/src/tracer/mod.rs` | `render_pass` is 280 lines (`:421`). | First sweep, adaptive rounds, scanline-order gather. | S/M |
| `crust-core/src/stats.rs` (1 652) | `impl Display for RenderStats` (`:704`) is one **687-line** `fmt`, writing `"  {:<28} {}"` / `{:<26}` rows by hand 68 times. | `write_scene / write_rays / write_textures / write_ptex / write_phases`, plus a `row(f, label, value)` helper. | M |
| `crust-core/src/light_cache.rs` | `train` is 253 lines (`:141`). | Trace receivers, per-light estimate, build the grid (after §2.11). | M |
| `crust-core/src/material/closure/mod.rs` (1 224) | `prepare` (`:696`) is a 246-line match. | One function per `Bsdf` arm. | M ⚠ |
| `crust-mtlx/src/eval.rs` (2 155) | `apply` is ~196 lines (`:483`), `compile_node` ~183 (`:1188`), `compile_colorcorrect` ~124 (`:1389`), plus about 520 lines of tests (`:1633-2155`). | `eval/{op, apply, compiler, colorcorrect, tests}.rs` | M |
| `crust-mtlx/src/surface.rs` (1 362) | Three translators in one file: `open_pbr_surface` (~333, `:523`), `standard_surface` (~233, `:856`), `gltf_pbr` (~250, `:1108`). | `surface/{open_pbr, standard_surface, gltf_pbr}.rs` | M |
| `crust-rt/src/scene.rs` (1 804) | `SceneBuilder::commit_with` is ~260 lines (`:399-659`), with about 950 lines of inline tests (`:852-1804`). Stale comment at `:405`: "a primitive node is 128 bytes", but `bvh/tests.rs:428` asserts `size_of::<PrimNode>() == 64`. | Sizing, expansion, then layout and build. Move the tests to `scene/tests.rs`, as was done for `bvh/tests.rs`. | M ⚠ |
| `crust-render/src/main.rs` (1 088) | CLAUDE.md says "`main.rs` only writes images", yet `main()` alone is ~335 lines (`:343-678`). Lines `:571-660` are a ~90-line traversal-stats table formatter that belongs beside `RenderStats`' `Display`. Logging setup, `utc_stamp` and `open_log_file` take `:214-341`, plus about 400 lines of inline tests. Leftover placeholder comments `// World`, `// Camera`, `// Timer` at `:482-487`. | `logging.rs`, `cli.rs`, `output.rs`; move the formatter to `crust-core::stats`. | M |

---

## 4. API shape, dead code and rule violations

### 4.1 Long parameter lists (M ⚠)

These functions carry `#[allow(clippy::too_many_arguments)]`, 15 in the workspace:

| Function | Parameters |
|---|---|
| `trace_path` | 12 |
| `volume_nee` (`path.rs:675`) | 11 |
| `advance_pixel` (`tracer/mod.rs:707`) | 9 |
| `random_walk` (`subsurface.rs:295`) | 9 |
| `walk_subsurface` | 8 |
| `emit_round_light` (`lights.rs:260`) | 9 |
| `nested_instancer_parts` (`instancing.rs:420`) | 8 |
| `encode_shadows` (`light_links.rs:479`) | 8 |
| crust-rt: `curve.rs:175`, `bvh/build.rs:465`, `bvh/mod.rs:957` | — |

Suggested fixes:
- `world`, `lights`, `volumes`, `strategy`, `indirect_clamp` and `guiding` travel
  together through the integrator. One `PathContext<'a>` struct removes most of these
  allows.
- `random_walk`: pass the `&WorldHit` instead of four of its fields.
- `emit_round_light`: pass `ctx` plus a `RoundShape { unit, radius, length }`.
- `encode_shadows`: make it a method on a struct holding `paths`, `weight`, `runs` and
  `vols`.

### 4.2 RNG outside openqmc (M)

CLAUDE.md says "No RNG outside `openqmc`". Two places break that:
- `guiding/dtree.rs:17-27` ✔ hand-rolls PCG32 (`pcg_f32`), seeded at `:168` by hashing
  the QMC seed. `DTree::sample` runs on every guided bounce. Take a `PathSampler` domain
  and use `domain.rng()`, or seed `openqmc::pcg::Rng`. This changes guided output, so
  re-record any guided goldens.
- `crust-rt/examples/ray_throughput.rs:121` hand-rolls an LCG (also inlined in
  `ray_batch`), although openqmc-rs is already a dev-dependency.

### 4.3 Public API wider than its users need (S)

- **crust-core `lib.rs`:**
  - `pub mod subsurface` has no user outside the crate;
  - `pub fn commit_options` (`:65`, placed mid-`pub use` block) is used only
    internally: make it `pub(crate)` or a `Config` method;
  - `distant_size_factor`, `distant_illuminance` and `IesShaping` are used only by the
    importer;
  - `pub use material::*` hides what is exported, and re-exports `materialx` a second
    time beside `:38`.
- **`LightList`** has nine overlapping lookups. `find_by_geom`, `find_by_geom_at`,
  `pick_at`, `iter_at` and `infinite_seen_by` (`light/list.rs:373-470`) are used only by
  tests, and `find_by_geom`'s doc (`:370`) still says "Used by the integrator".
  `#[cfg(test)]` them, or route the tests through the `*_index_*` forms.
- **`Renderer`** has four entry points (`tracer/mod.rs:177-201`): `render`,
  `render_with_tiles`, `render_with_progress(tiled: bool)` and
  `render_with_stats(tiled: bool)`. `render_with_tiles` and `render_with_progress` are
  used only by tests. Replace them with one `render(RenderMode, Option<ProgressCallback>)`.
- **crust-assets:**
  - `lib.rs:40-52` re-exports items with no caller: `ptex_stream_enabled`,
    `ptex_stream_min_bytes_from_env`, `ptex_mip_space_from_env`,
    `ptex_cache_budget_from_env`, `PTEX_DEFAULT_CACHE_MB`, `PtexStreamStats` and
    `max_log2_from_env_opt`;
  - `TileCache::budget_from_env` (`tiled/cache.rs:438`) is unused;
  - `pub mod tiled` re-exports `TileCache`, `StripedCounter`, `Tile`, `TileData`,
    `TileId`, `with_tile` and `write_tx_exr`, while only `TxFormat`, `make_tx` and
    `make_tx_atomic` are used outside the crate;
  - `PtexColor::open`, `UvTexture::open` and `PtexStream::open` read the global
    `crust_core::config()` and are called only from tests. That goes against the
    "build a `Config` and pass it" rule; tests should use `open_with` /
    `FileAssets::with_config`.
- **crust-mtlx:**
  - `lib.rs:30-35` declares `pub mod bsdf/eval/parse/surface/value` *and* re-exports
    their items at the root, so everything has two paths;
  - `Compiler`'s fields are public, and `lib.rs` reaches into `c.program` and
    `c.unsupported`;
  - `Compiled::roots()` clones every closure just to read the slots
    (`self.closures.clone().for_each_slot`).

### 4.4 Easy-to-misuse settings types (S)

- `RenderSettings::new` (`tracer/settings.rs:163`) takes seven positional numbers
  (`u32, u32, usize, usize, u32, f32, isize`), so spp, max_depth and min_spp can be
  swapped silently. Use a builder or named fields.
- `with_guiding(enabled: bool, …)` (`:210`) is a bool parameter, and its defaults
  (4 and 0.5 at `:182-183`) duplicate `GuidingConfig::default()`. Store
  `Option<GuidingConfig>` instead.
- `HitRecord` has `uv` plus a separate `has_uv: bool` (`hittable.rs:33, 48`). That is
  the paired-sentinel pattern its own `face: Option<FaceHit>` doc argues against. Use
  `uv: Option<(f32, f32)>`.
- `RayStats::merge` (`stats.rs:264`) lists 25 fields by hand. It covers them all today,
  but a new counter would silently not merge. Destructure with
  `let RayStats { a, b, … } = o` (no `..`) so the compiler enforces completeness.

### 4.5 Environment switches whose A/B is over (M)

- **`CRUST_BVH_PACKET_SAH` can be retired.**
  `openspec/specs/intersection-kernel/design.md:143-166` records it as measured, proven
  1/√N, neutral to better on timing, with goldens re-recorded with it on. Retiring it
  touches:
  - `Config::bvh_packet_sah`;
  - `lib.rs:65` `commit_options`;
  - `crust_rt::CommitOptions::packet_sah`;
  - the architecture table and the site page.
- **`TriPackets::Auto`** is documented as "= gathered" with no threshold
  (`config.rs:62-68`). It is a third state with no behaviour of its own.
- No other switch is clearly settled:
  - `MTLX_OPT` and `SHADER_JIT` are the reference sides of bit-identity tests;
  - the ADAPTIVE switches are one day old.

### 4.6 Doc comments attached to the wrong item (S)

- `usd_import/mesh.rs:1315-1326`: `triangulate`'s doc sits on `type Triangulated`.
- `tracer/path.rs:87-100`: the doc for `sample_bounce_direction` sits on
  `ray_cones_enabled` (`:103`). That function is also used only at `tracer/mod.rs:742`,
  so it can move there.
- `config.rs:25-54`: the long `PtexMipSpace` doc sits on `TriPackets` (`:62`), and
  `PtexMipSpace` (`:103`) has none. This is the same bug the 2026-09-27 pass fixed once.

### 4.7 Stale workarounds and comments (S)

- **xform fallback warning (`xform.rs:57`).** It says openusd's composition is "known to
  be wrong for multi-op stacks", which is stale on 0.6+. `local_matrix_via_openusd`
  (`:183`) covers the same six types as §1.4 and returns IDENTITY otherwise. Retiring the
  local composer means checking rotations, Euler triples, `orient`, `!invert!` and
  suffixes against openusd; a per-op comparison test would do it.
- **`usd_mat_to_glam` (`xform.rs:20`)** is 16 lines that could be one `.map(|x| x as f32)`.
- **`shader_info_id`'s fallback (`materials.rs:271`).** It says "Fallback for older
  openusd revisions…", but on 0.7 `Shader::id()` reads both Token and String. Confirm,
  then delete it.
- **Silent drop in the prototype walk.** When `mesh_source` returns `None`
  (`instancing.rs:197`), the mesh is dropped with no log, where the top-level path logs
  at DEBUG (`mesh.rs:480`). This is planned in `harden-usd-import`.
- **`prototype_prunes`' doc** says "the placement count's walk passes `false`", but it
  has one caller, which passes `true`.
- **Broken intra-doc links.** These name removed items:
  - `guiding/field.rs:101` → `DTree::sample`
  - `hittable.rs:43` → `UvMap::tangents`
  - `light/shape.rs:372` → `LightShape::sample_solid_angle` (now on
    `SolidAngleSampling`)
  - `material/material.rs:156` → `HitRecord::face_id`

  The `HitRecord` docs (`hittable.rs:27, 59-60`) still name `face_uv` and `face_id`,
  which are now `FaceHit`.
- **Comments that narrate history instead of explaining code.** These belong in the
  design records, per CLAUDE.md:
  - `path.rs:795-798` ("The old recursion still counted…")
  - `path.rs:1203-1207`
  - `path.rs:1250-1256` (the cos² furnace story)
  - `config.rs:5-10` ("They used to be read… six different ways")
  - `stats.rs:1189-1195`

### 4.8 `expect` that a different structure would remove (S)

- `light_links.rs:312` ("dense ids"): keep a `Vec<Path>` beside the id map.
- `light_links.rs:342` ("recorded"): store the path in `nothing`.
- `light_links.rs:369, 494` ("filtered"): use `filter_map`.
- `volume.rs:19` ("checked by dispatch"): pass in the token already read at `mod.rs:345`.
- `mesh.rs:383, 412, 431, 640` are real invariants and can stay.

### 4.9 Hot-loop allocations (M ⚠)

- `volume.rs:419` `active_intervals` allocates a `Vec` per segment and per shadow ray.
- `lobes: Vec` (`:483`) can allocate on every delta-tracking collision step.
- Return a span with start/end from `active_intervals`, and use a fixed-capacity
  inline array.

### 4.10 Small items (S)

- **crust-jit `host_apply` / `host_texture`** (`lib.rs:262, 279`) dereference raw
  pointers but are plain `extern "C" fn`, so any in-crate code could call them safely
  with bad pointers. Declare them `unsafe extern "C" fn`.
- **`tiled/cache.rs:705`** `fn lock() -> Option<MutexGuard>` never returns `None`.
  Return the guard directly.
- **`--bucket`** (`main.rs:64-67`) is a hidden no-op flag. Remove it, or warn when it is
  used.
- **`utils::degrees_to_radians`** has one caller (`camera.rs:36`), against 38 uses of
  `.to_radians()`. They are not bit-identical, so switching it changes camera output
  and needs a goldens re-record.
- **`crates/utils`** has no crate-level `//!` doc. It is the only crate without one.
- **Vestigial modules:** debt register item 1 (`hittable.rs` / `aabb.rs`) is still
  open.

---

## 5. Tooling, CI and manifests

| # | Finding | Fix | Effort |
|---|---|---|---|
| 5.1 ✔ | `check_images.sh` / `bench_scenes.sh` reuse a stale binary. | See §1.6. | S |
| 5.2 | CI never compiles several feature/cfg combinations: <br>• **`traversal-stats`**: the block at `crust-render/src/main.rs:571-660`, the counters in `crust-rt/src/prim.rs` and `bvh/mod.rs`, `rt_world.rs`, and the `traversal_probe` example (`required-features`). <br>• **the non-JIT branches** (`material/materialx.rs:127, 196, 246`): `cargo test --workspace` unifies crust-render's default `jit` onto crust-core, so the documented `--no-default-features` renderer is untested. <br>• **`scripts/test_simd_matrix.sh`** and its IR fused-multiply-add contraction check, which guards a bit-identity pair. | Add these steps to `rust.yml`: <br>• `cargo clippy -p crust-render --all-targets --features traversal-stats` <br>• `cargo check -p crust-render --no-default-features` <br>• the `+avx2,+fma` leg and the IR check (crust-rt only, cheap) | M |
| 5.3 | `cargo doc --no-deps --workspace` gives **38 warnings** (crust-core 21, crust-assets 9, crust-mtlx 4, crust-jit 3, crust-render 1), almost all broken or private intra-doc links. Examples: <br>• `crust-jit/src/lib.rs:20, 36` link the private `host_apply`; <br>• `rt_world.rs:213, 342, 411`; <br>• `crust-mtlx/src/eval.rs:98, 378, 922, 927`; <br>• `crust-assets/src/ptex_stream.rs:194, 268`; <br>• in `profile`, `lib.rs:31` adds a second, outer doc to `pub mod profile`, which breaks its links to `Section`, `Category` and `flush`. <br>Nothing in CI runs rustdoc. | Repair the links, and add `RUSTDOCFLAGS=-D warnings cargo doc --no-deps --workspace` to `rust.yml`. | M (the gate alone is S) |
| 5.4 ✔ | `Cargo.lock` is **gitignored**, although the workspace ships a binary. The comment on the `ptex` git pin in `Cargo.toml` acknowledges this ("`Cargo.lock` is not checked in"), and it is why the pin needs a `rev`. CI and local builds can resolve different versions. | Commit `Cargo.lock` and remove it from `.gitignore`. | S |
| 5.5 ✔ | No `rust-toolchain.toml` and no `rust-version`. CI pins Rust 1.98.1 (`RUST_VERSION` in `rust.yml`) and `nightly-2026-09-26` (`nightly.yml:43`), but local builds are not pinned. CLAUDE.md says "toolchain pinned". | Add `rust-toolchain.toml` (1.98.1, with clippy and rustfmt) and `rust-version` in `[workspace.package]`. | S |
| 5.6 ✔ | Workspace dependencies are not consolidated. These are declared per crate instead of in `[workspace.dependencies]`: <br>• `rayon = "1.10.0"` (crust-core, crust-rt) <br>• `openqmc-rs = "0.2.4"` (three crates) <br>• `criterion = "0.8"` (two crates) <br>• `openusd` / `openusd-schemas = "0.7"` (crust-core, plus crust-render dev-deps). This is the drift hazard the `ptex` comment argues against. <br>• the five `cranelift-* = "0.136"` entries <br>Also, `crates/utils/Cargo.toml` hard-codes `version = "0.1.0"` instead of `version.workspace = true`. | Hoist them all, and fix `utils`. | S |
| 5.7 | No `[workspace.lints]`: `forbid(unsafe_code)` is repeated as an attribute in six crates. | Add `[workspace.lints]` with `unsafe_code = "deny"` and `clippy::undocumented_unsafe_blocks = "deny"`, and give every crate `lints.workspace = true`. The five safe crates keep their `#![forbid(unsafe_code)]` attribute, which tightens the workspace `deny`. (A crate that inherits workspace lints cannot override one of them in its manifest, so the workspace level has to be the loosest one any crate needs.) The "four audited blocks" claim is then enforced rather than conventional. | S |
| 5.8 | `scripts/alab-seq.nu` is an unreferenced one-line Nushell script with hard-coded camera and output paths. | Delete it, or move it into `docs/alab_profile.md`. | S |
| 5.9 | `bench_scenes.sh` does exactly the sequential wall-clock timing that CLAUDE.md § Measuring a change calls misleading. | Label it a smoke benchmark only, or make it a thin wrapper over `bench_ab.sh`. Factor out the shared scene resolution (§2.12). | S |
| 5.10 | The three openusd-bug probes (`crust-render/examples/xform_probe`, `proto_probe`, `rel_probe`) target bugs fixed in 0.6. `usd-scene-import/design.md:666-705` says retiring the local workarounds needs verification. | Turn them into crust-core regression tests on inline stages, so an upstream regression fails CI. At minimum, merge them into one `usd_probe`. | M |

---

## 6. Tests

### 6.1 Stage-writing code is duplicated, and temp paths can collide (M)

- **Four `write_stage` helpers:** `usd_inline.rs:88`, `usd_adaptive.rs:69` and
  `light_linking.rs:12`, plus about 16 inline copies in `usd_scene.rs` (`:170, 782, 976,
  1276, 1367, 1555, 1839, 2245, 2371, 2517, 2623, 2688, 2804, …`).
- **Temp paths collide:** most temp directories have fixed names without the process id,
  so two concurrent `cargo test` runs overwrite each other's files. Only two use
  `std::process::id()`, and files are rarely removed.
- **Repeated headers:** `usd_inline.rs` `load`, `load_with_settings` and `load_at_level`
  (`:13-80`) each re-inline the same `#usda` header.
- **Four `AssetLoader` test doubles:** `usd_scene.rs:1044, 1255` and
  `usd_inline.rs:1400, 2153`.

**Fix:** a `tests/common/mod.rs` with:
- a stage builder (`.body()`, `.settings()`, `.options()`, `.write()`, `.load()`) using
  pid-unique directories;
- a shared `RecordingAssets`;
- `hit_t` / `hits` helpers.

### 6.2 Test files to split by topic (M)

This is debt register item 2, still open.

**`usd_inline.rs`** (2 516 lines, 76 tests) already has section banners. Split along
them:

| Topic | Starts at |
|---|---|
| geometry | `:129` |
| transforms | `:309` |
| settings and camera | `:436` |
| materials | `:608` |
| lights and UsdLux | `:711`, `:913` |
| volumes | `:1439` |
| subdivision | `:1550` |
| stats and errors | `:2056` |
| visibility and links | `:2258` |

**`usd_scene.rs`** (3 125 lines) says it loads the checked-in samples, but about half of
it is inline stages. Move those out:

| New file | Lines |
|---|---|
| `usd_instancing.rs` | `:497-1075` |
| `usd_materialx.rs` | `:1599-1800` |
| `usd_preview.rs` | `:2197-2510` |
| `usd_purpose.rs` | `:2514+` |

### 6.3 Malformed-input coverage (S)

There is no test that a malformed stage loads; every finding in §1.1–1.3 was a crash
reachable from authored data. `cargo test` runs with overflow checks, so one inline
stage per malformed input catches these in the profile where they panic. This is
planned in `harden-usd-import`.

---

## 7. Documentation

### 7.1 Main specs contradict the code: seven finished changes are unarchived (M) ✔

These seven changes have every task checked and their code present, but their delta
requirements never reached `openspec/specs/*`:

| Change | Tasks |
|---|---|
| `add-light-and-shadow-linking` | 19/19 |
| `light-camera-visibility-and-link-exclusion` | 18/18 |
| `add-mtlx-surface-shaders` | 29/29 |
| `add-mtlx-cutout-and-rotation` | 8/8 |
| `add-mtlx-random-walk-subsurface` | 5/5 |
| `check-materialx-nodes-against-osl` | 16/16 |
| `optimize-subsurface-walk` | 12/12 |

As a result:
- `openspec/specs/lighting/spec.md:78-83` still says light and shadow linking SHALL be
  documented as **unsupported**;
- `openspec/specs/materials/spec.md` (81 lines) has no MaterialX closure, cutout,
  random-walk or OSL-oracle requirement;
- the CLI spec lacks "The render profile reports subsurface walks".

**Fix:** run `openspec archive` (which syncs the specs) on all seven.

### 7.2 Two open changes are stale (S/M)

- **`compact-triangle-layout`** (0/29) was superseded by the indexed packets that
  shipped in the archived `compact-triangle-storage` (`CRUST_TRI_PACKETS`,
  `intersection-kernel/spec.md:104-120`). Its `--geometry-layout` /
  `crust:geometryLayout` were never built. Delete it, or archive it as superseded.
- **`add-material-color-management`** (0/20) needs a rewrite before anyone implements
  it:
  - `design.md:14, 16, 93, 209` target `crust-render/src/main.rs::PtexColor::open`,
    which now lives in `crust-assets/src/ptex_*.rs`;
  - `tasks.md:3` proposes a new `ColorSpace{Linear, Srgb, Gamma(f32)}`, but
    `crust-core/src/texture.rs:85` already defines
    `ColorSpace{Srgb, Gamma22, Gamma18, Raw, …}`.
- `stream-ptex-by-default` (0/25, proposed 2026-10-01) is in flight; keep it.

### 7.3 `CRUST_MESH_BAKE` bit-identity contradiction (S)

- These say `CRUST_MESH_BAKE=0` is bit-identical: `config.rs:143`,
  `openspec/specs/cli/design.md:314-316` and
  `openspec/specs/usd-scene-import/design.md:137-138`.
- These say it is not ("~0.2% of cornellbox pixels, relmse 4e-18"):
  `docs/architecture.md:182`, `site/.../environment-variables.md:112` and
  `docs/rust_leverage.md:79`.

Measure which is true, and correct the others.

### 7.4 The env-var list exists in four places, and one copy is incomplete (S)

`cli/design.md:311-334` "§ Environment overrides" covers only about 12 of the 22
switches, as prose. Replace it with a link. The canonical lists are:
- the site page, for users;
- the `architecture.md` table plus the `Config` rustdoc, for contributors.

### 7.5 The technical-debt register is stale (S) ✔

`docs/architecture.md:225-293` lists file sizes that are long out of date:

| File | Register says | Actual |
|---|---|---|
| crust-rt `scene.rs` | 1 350 | 1 804 |
| `stats.rs` | 1 360 | 1 652 |
| `materialx.rs` | 1 430 | 436 (already split) |
| `usd_import/mesh.rs` | 1 410 | 1 916 |

It also omits today's two largest files, `scene/subdiv.rs` (2 396) and
`crust-mtlx/src/eval.rs` (2 155). The "Paid down" paragraphs (`:227-266`) are history;
move them out. This document can seed the refresh.

### 7.6 A known correctness bug is missing from user docs (S)

`UsdPreviewSurface` `diffuseColor` / `emissiveColor` are read without colour decoding
(`usd_import/preview.rs:327, 346`). This is recorded only in
`docs/color_management.md:317` and `textures/design.md:448`. It is absent from:
- `site/.../architecture/limitations.md`;
- the README's limitations;
- the materials spec.

CLAUDE.md requires limitations to be on the site. Add one bullet to each.

### 7.7 The README duplicates the site and has drifted (M)

`README.md` is 516 lines:
- **CLI block** (`:428-445`): omits `--subdiv-level` and `--subdiv-edge-length`.
- **"Known limitations"** (`:455-516`): duplicates `site/.../limitations.md`, but lacks
  its adaptive-subdivision and BVH-build-memory items.
- **Moana section** (`:365-427`): a measurement log whose peak-memory figures disagree
  with each other: 47.6 GiB (`:377`), 43.76 (`:387`) and 47.08 (`:400`). The
  stream-ptex proposal says 43.1.
- **CLI flags** are described in five places: `--help`, the site, the README, the
  `cli/design.md:26-45` cookbook comment, and `cli/spec.md`.

**Fix:** cut the README to a pitch, the build commands and links. The site is canonical
for users; `design.md` / `moana_profile.md` are canonical for measurements.

### 7.8 History written as if current (M)

- **`docs/embree_comparison.md`** (`:12, 106, 120, 176, 284-285`) describes a binary BVH
  in `bvh.rs`, `primitives/` and `tracer.rs:787`. None of these exist: the kernel is now
  crust-rt's SBVH → BVH4. CLAUDE.md still routes readers to this file.
- **`docs/light_sampling.md`** (1 518 lines):
  - `:20` still says "pick one light uniformly (`light.rs`)", then patches itself with
    "since §9.3(j) by power";
  - §3 is 680 lines of dated baselines;
  - `:6, 94, 109, 565, 576` cite `light.rs` / `tracer.rs`, which are now directories.
- **`docs/rust_leverage.md`** is a completed audit (its status line says "implemented
  2026-09-27"), yet §6 (`:572-582`) still describes 19 raw env reads as the current
  state.
- **`docs/shading_performance.md`** is a plan whose steps are partly done.

**Fix:** put a "Historical — as of `<commit>`" banner on each, or move them to
`docs/history/`. Lift whatever is still true into the capability's `design.md`.

### 7.9 `docs/issues/` holds only resolved entries (S)

- Both entries are marked fixed in openusd 0.6.0.
- `docs/issues/README.md:14` says the workspace tracks 0.6.0; the manifests pin 0.7.
- `usd-scene-import/design.md:664-709` "Known gaps: openusd bugs and workarounds" is
  mostly the same resolved history.
- The code warning at `instancing.rs:371` and the comment at
  `samples/nested_instancing.usda:99` still say "openusd 0.5 cannot read".

**Fix:** mark `docs/issues/` as an archive, update the version, and retitle or trim the
design section. The version and the two stale texts are planned in `harden-usd-import`.

### 7.10 The largest design records are hard to navigate (M)

- `materials/design.md` § MaterialX runs **459 lines** (`:183-642`) with no subheadings.
- `usd-scene-import/design.md` § Geometry runs 324 lines with a single h3.

Add h3 subheadings, or a short table of contents at the top of each.

Largest docs by line count (about 16.3k lines in all non-archived docs):

| Doc | Lines |
|---|---|
| `light_sampling` | 1 518 |
| `materials/design` | 781 |
| `usd-scene-import/design` | 773 |
| `textures/design` | 735 |
| `rust_leverage` | 651 |
| `moana_profile` | 542 |
| `shading_performance` | 533 |
| `simd` | 525 |
| `README` | 516 |
| `ptex_streaming` | 512 |

### 7.11 Missing module docs (S)

- `crates/utils/src/lib.rs` has no crate-level `//!` doc.
- These crust-core modules have no module doc:
  - `tracer/mod.rs` (1 101 lines)
  - `material/material.rs` (467; the trait and `ShadingPoint`)
  - `material/brdf.rs` (448)
  - `scene.rs`
  - `light/mod.rs`
  - `world.rs`
  - `camera.rs`

### 7.12 Gaps in the CLAUDE.md doc map (S)

- The capability table omits `image-output`, which has a `spec.md` but no `design.md`.
- `docs/subsurface_walk.md` and `docs/rust_leverage.md` are not routed from it.
- The `traversal_probe` example is missing from the probe list and the cookbook; only
  `docs/simd.md:380` mentions it.
- The `architecture.md:191-201` owner column writes `crust-assets/lib.rs` without the
  `src/` (cosmetic).

### 7.13 No onboarding path for human contributors (S–M)

- There is no `CONTRIBUTING.md`, and the README never links CLAUDE.md, which is the
  actual contributor guide.
- Nothing explains how to obtain the gitignored external scenes (Kitchen_set, ALab,
  Moana, MaterialXTeapotLion) or where to put them. The default scene lists in
  `bench_scenes.sh:57-58` and `check_images.sh:45-46` depend on them, and so do the
  three samples in §1.7.

**Fix:**
- a `CONTRIBUTING.md` that links CLAUDE.md;
- a "test assets" page;
- `rust-toolchain.toml` (§5.5).

### 7.14 Suggested canonical homes

| Content | Canonical home |
|---|---|
| Flags, env vars, `crust:*` attributes and limitations, for users | `site/` |
| Rules, for contributors | `CLAUDE.md` |
| The map, the env table and invariants | `docs/architecture.md` |
| Rationale and measurements | `openspec/specs/*/design.md` |
| Behaviour | `openspec/specs/*/spec.md`, kept in sync by archiving changes |
| Landing page only | `README.md` |
| Current reference | `color_management`, `ptex_streaming`, `simd` in `docs/` |
| Everything else in `docs/` | label as history |

---

## 8. Suggested order

1. **Quick, high-leverage work (about a day):**
   - §1.5 (MIS clamp), §1.6 / 5.1 (stale binary), §1.7 (sample paths) and §1.10;
   - CI gaps §5.2–5.3, lockfile and toolchain §5.4–5.5, manifests §5.6–5.7;
   - archive the finished OpenSpec changes and drop the stale ones (§7.1–7.2);
   - fix the `CRUST_MESH_BAKE` contradiction (§7.3).
2. **`harden-usd-import`:** §1.1–1.4 and §1.8 (already proposed, with tasks).
3. **Shared helpers:** USD attribute decoders and `prune_reason` (§2.1–2.2), UDIM tiles
   (§2.8), the integrator helpers and ray-epsilon constant (§2.10), the test stage
   builder (§6.1). Most of the splits in §3 build on these.
4. **Behaviour fixes:** primvar interpolation (§1.9, §2.5), guiding RNG (§4.2), retiring
   `CRUST_BVH_PACKET_SAH` (§4.5). Each changes images somewhere, so each needs its own
   goldens story.
5. **Structural splits:** §3, one per PR, each gated by `check_images.sh` and, on hot
   paths, callgrind.
6. **Documentation consolidation:** README to a landing page (§7.7), history labelled
   (§7.8–7.9), CONTRIBUTING and a test-assets page (§7.13), the debt register refreshed
   from this document (§7.5).
