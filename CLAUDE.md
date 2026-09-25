# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Crust Render is a toy, physically-based path tracer written in safe Rust (edition 2024),
inspired by PBRT, *Ray Tracing in One Weekend*, and Autodesk Standard Surface / OpenPBR.
Scenes are loaded exclusively from **USD** (`.usda` / `.usdc` / `.usdz`) via the pure-Rust
[`openusd`](https://github.com/mxpv/openusd) crate — RON support was removed.

## Commands

```bash
# Build / render (single binary in the workspace, so bare cargo run works)
cargo run --release -- -i samples/openpbr_showcase.usda -o out.exr
cargo run --release -- -i samples/cornellbox.usda
cargo run --release -- -i samples/usdlux.usda    # every UsdLux light: normalize, colour temperature, shaping, IES
cargo run --release -- -i samples/materialx_teapot.usda    # MaterialX + UDIM (needs the DPEL download)
cargo run --release -- -i samples/materialx_lion.usda      # the other DPEL asset: 140-op graph, sheen, 1.06 M tris
cargo run --release -- -i samples/materialx_showcase.usda  # both, framed after the assets' overview.png (1080p)
cargo run --release -- -i samples/materialx_basic.usda     # MaterialX fixture, self-contained
cargo run --release -- -i samples/usdpreview_textured.usda # UsdPreviewSurface + UsdUVTexture (UDIM, EXR, auto)
cargo run --release                 # no -i → hard-coded procedural fallback (world::simple_scene)
cargo run --release -- --bucket -i samples/cornellbox.usda   # tiled/bucket rendering

# CLI flags: -i/--input, -o/--output (default output.exr), -l/--level (log level),
# --log-file [DIR] (tee the log to crust-render-<UTC stamp>.log), -b/--bucket,
# -s/--samples (override spp), -f/--frame (USD time code to evaluate the stage at),
# --strategy (power|balance|light|bsdf), --light-selection (uniform|power),
# --filter (box|triangle|gaussian|blackman|mitchell) + --filter-radius (pixels),
# --camera PRIM_PATH (render through that camera; else RenderSettings.camera,
#   else the first camera found -- a wrong path errors and lists the stage's cameras),
# --stats (per-phase profile + scene statistics),
# --auto-tx (convert UV textures to a .tx beside the original on first use)

# Keep a full record of a render. The file gets the same events as the terminal
# at the same -l level, so DEBUG has to be asked for; bare --log-file writes
# into the working directory, and a directory argument is created if missing.
cargo run --release -- -i samples/cornellbox.usda -l debug --log-file renders/logs

# Render one frame of an animated stage (time samples resolve at that code,
# interpolated; unanimated attributes read their default). Without -f every
# attribute reads its *default* value -- not frame 0.
cargo run --release -- -i samples/animation.usda -f 5 -o frame.0005.exr

# Where did the time and memory actually go? (parse vs build vs render vs output)
cargo run --release -- -i samples/curves.usda --stats

# Tests (integration tests live in crust-core/tests/usd_scene.rs, load sample USD files)
cargo test
cargo test -p crust-core loads_cornellbox_usda     # run a single test by name

# Benchmarks (criterion)
cargo bench -p crust-core            # bench targets: "vec3 dot", "simple world", "simple world guided"
cargo bench -p crust-rt              # kernel traversal: intersect/occluded over 3 scene kinds + build

# Perf probes (better than criterion for kernel A/B: min-of-N, not a drifting mean)
cargo run --release -p crust-rt --example ray_throughput          # Mray/s per scene & query
cargo run --release -p crust-render --example exr_diff -- a.exr b.exr   # did the image change?

# Placing a camera in a downloaded production asset, and settling whether a
# texture is display-encoded or linear (see "Ptex" under USD import).
cargo run --release -p crust-render --example scene_bounds -- scene.usda
cargo run --release -p crust-render --example tex_probe -- texture.ptx
cargo run --release -p crust-render --example tex_probe -- render.png [x0 y0 x1 y1]

# What OpenPBR parameters does a MaterialX graph actually reduce to? A wrong
# albedo decode is a plausible pastel and a wrong mask is a plausible blend, so
# a MaterialX surface cannot be checked by eye -- this prints the numbers at a
# named point on the chart. See "MaterialX" under USD import.
cargo run --release -p crust-render --example mtlx_shade -- \
    samples/materialx_basic.mtlx mtlx_ceramic 0.25 0.5

# It goes through `FileAssets`, not just `UvTexture`, so `CRUST_TEX_STREAM` is
# honoured and the `emission` column shows the range an HDR texture actually
# carries -- a preloaded texel clips to 1.0 and a streamed one does not. This
# is the A/B for MaterialX emission (see "Known incomplete work"):
cargo run --release -p crust-render --example maketx -- \
    samples/textures/mtlx_emission.hdr raw
# Once the .tx exists it is streamed by default, so the preloaded side of the
# A/B has to ask for preloading with CRUST_TEX_STREAM=0.
CRUST_TEX_STREAM=0 cargo run --release -p crust-render --example mtlx_shade -- \
    samples/materialx_emissive.mtlx mtlx_emitter_textured 0.37 0.12   # (1 1 1)
cargo run --release -p crust-render --example mtlx_shade -- \
    samples/materialx_emissive.mtlx mtlx_emitter_textured 0.37 0.12   # (16 9 3)

# ...and the same thing end to end, where the difference is 15.0 exactly.
CRUST_TEX_STREAM=0 cargo run --release -- -i samples/materialx_emissive.usda -o ldr.exr -s 32
cargo run --release -- -i samples/materialx_emissive.usda -o hdr.exr -s 32
cargo run --release -p crust-render --example exr_diff -- ldr.exr hdr.exr

# Is a Ptex file actually being addressed correctly? Neither check renders
# anything -- a wrong Ptex lookup still produces a plausible-looking surface,
# so both answer in numbers instead. See "Ptex" under USD import.
#   1. face ids: does the .ptx's embedded base cage match the mesh it is bound
#      to, face for face and vertex order for vertex order?
cargo run --release -p crust-render --example ptex_verify -- model.usd /mesh/prim color.ptx
#   2. (u,v) orientation: are texels continuous across the seams the file's own
#      adjacency data declares, and more so than transposed or than chance?
cargo run --release -p crust-render --example ptex_seams -- color.ptx

# openusd composition probes. Each was written to pin down a bug that is now
# fixed upstream (see docs/issues/ and "Known incomplete work"); keep them as the
# regression check to run when bumping openusd.
cargo run --release -p crust-render --example proto_probe -- stage.usda [/prim]   # prototypes
cargo run --release -p crust-render --example rel_probe   -- stage.usda [relName] # rel targets
cargo run --release -p crust-render --example xform_probe -- stage.usda /prim     # xformOpOrder

# Kernel correctness is stated in exact float bits, so it must hold under every
# codegen: runs the suite with AVX/AVX2/AVX-512 off, with AVX2+FMA, and native.
scripts/test_simd_matrix.sh -p crust-rt

# Does texture filtering actually remove the aliasing? No checked-in sample
# minifies a texture, so this generates the case that does -- a checkerboard
# plane receding to the horizon -- and measures each configuration against a
# high-spp reference OF ITSELF (aliasing does not converge, so comparing
# filtered against unfiltered would measure bias, not error).
python3 scripts/gen_texture_alias_scene.py /tmp/alias --measure

# Streaming textures. A `.tx` beside a texture (same path, `.tx` extension) is
# always looked for and streamed when present -- `CRUST_TEX_STREAM=0` preloads
# everything instead. Convert by hand (the mip chain is reduced in linear light,
# the same `reduce_half` the in-memory pyramid uses, so a streamed render and a
# preloaded one agree texel for texel; a float source whose values exceed 1.0
# takes the EXR backing, `--format` overrides)...
cargo run --release -p crust-render --example maketx -- 'albedo.<UDIM>.png' srgb_texture
cargo run --release -p crust-render --example maketx -- sky.exr raw            # half tiles
cargo run --release -p crust-render --example maketx -- albedo.png srgb_texture --format=exr
# ...or let the renderer convert on first use (missing or stale .tx only; any
# float source keeps half tiles). The next render converts nothing.
cargo run --release -- -i scene.usda --auto-tx
CRUST_TEX_CACHE_MB=256 cargo run --release -- -i scene.usda --stats

# Streaming Ptex. No conversion step -- a .ptx is already a tiled per-face mip
# pyramid, so this just turns the reader's cache on. Capping both backends
# alike is what makes the A/B an equality rather than a comparison of two
# different resolutions; uncapped is what streaming is *for*.
# Two variables are needed beyond the switch, and leaving either out makes the
# A/B compare a preloaded render against itself and agree for the wrong reason:
#   CRUST_PTEX_STREAM_MIN_MB=0  -- these fixtures are kilobytes, and a texture
#       smaller than its own cache slot is preloaded by default.
#   CRUST_PTEX_STREAM_MIPSPACE=file -- a mipmapped .ptx is preloaded by
#       default, because its stored levels were reduced in the file's own
#       encoding while the preloaded pyramid is reduced in linear light. This
#       takes the file's chain (darker under minification, up to 0.147 on the
#       tiled fixture) and the residency that comes with it.
# `--stats` says `backend` either way, and names the variable that declined.
CRUST_PTEX_MAX_LOG2=5 cargo run --release -- -i samples/ptex_quads.usda -o a.exr
CRUST_PTEX_MAX_LOG2=5 CRUST_PTEX_STREAM=1 CRUST_PTEX_STREAM_MIN_MB=0 \
    CRUST_PTEX_STREAM_MIPSPACE=file \
    cargo run --release -- -i samples/ptex_quads.usda -o b.exr
cargo run --release -p crust-render --example exr_diff -- a.exr b.exr   # 0 pixels
# The configuration that streams under the *default* policy: no pyramid, so no
# chain to reduce in the wrong space. Exact and uncapped, at the cost of the
# pyramid's anti-aliasing.
CRUST_PTEX_STREAM=1 CRUST_PTEX_MIP=0 cargo run --release -- -i scene.usda --stats
CRUST_PTEX_STREAM=1 CRUST_PTEX_CACHE_MB=64 CRUST_PTEX_STREAM_MIPSPACE=file \
    cargo run --release -- -i scene.usda --stats

# --- The optimization loop (see "Measuring a change" below) --------------
scripts/bench_scenes.sh                        # min-of-N Render seconds + Mray/s per scene
scripts/check_images.sh record <dir>           # golden EXRs at 16 spp
scripts/check_images.sh check  <dir>           # re-render and diff; exits non-zero on any change
scripts/bench_ab.sh -a <binA> -b <binB> [scenes...]   # interleaved A/B of two binaries

# CI runs (toolchain pinned, RUSTFLAGS=-D warnings), as three parallel jobs:
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
```

Logging uses `tracing`; set verbosity with `-l debug|info|warn|error|trace` (default `info`).

**The level is decided by whether the line scales with the scene, not by how interesting
it is.** A default render prints four `INFO` lines — what is being rendered, how long it
took, and the two images written — and that count does not change between a cornell box
and the Moana island. So:

- **`INFO`** is for facts about *this render*, emitted a bounded number of times whatever
  the stage holds: the resolution/spp/depth banner, the elapsed time, each output path,
  the once-per-process residency policy (`FileAssets::new`'s streaming notices, including
  the one explaining why `CRUST_PTEX_STREAM=1` alone streams nothing, and `--auto-tx`'s
  one notice and one conversion summary — the per-file lines are `DEBUG`), and the path
  guiding ΔEff verdict, which announces that the final pass will render unguided.
- **`DEBUG`** is for anything whose line count grows with the input — per prim, per
  material, per texture, per prototype, per chunk, per pass. The island binds 3 618 Ptex
  textures and composes 20 subtrees; one line each is a debugging tool, not a progress
  report. `--stats` is where the *totals* belong.
- **`WARN`** keeps its own meaning and was not touched: something authored was refused,
  approximated or skipped, and the image differs from what the stage asked for.

The practical consequence when adding a log: if you can write a stage that makes your new
line print a thousand times, it is `DEBUG`. Nothing is logged per ray, per pixel or per
sample — the finest granularity in the engine is per *pass* (`render_pass`), which is one
line on an ordinary render and a handful on a guided one.

`--log-file` tees the same stream to `crust-render-<YYYYMMDDTHHMMSSZ>.log`. Four
details are load-bearing. It is **two `fmt` layers over a registry**, not one writer
teed into both sinks, because ANSI is a per-layer setting — a single writer would
either fill the file with escape codes or strip the colour from the terminal; the
registry costs no new dependency, since `fmt` already pulls `sharded-slab` and
`thread_local`. The file is **unbuffered**, because several error paths end in
`std::process::exit`, which runs no destructors — a `BufWriter` would drop exactly the
lines explaining why the run stopped. And `utc_stamp` **hand-rolls** the civil-from-days
conversion (Hinnant's, era-shifted to March so leap days land at the end of a 400-year
cycle) rather than taking `chrono` or `time`, neither of which is in the graph and
either of which would be the largest dependency in this binary for the sake of naming a
file. The format is fixed-width and zero-padded so lexical order is chronological;
`utc_stamp_*` in `main.rs` pins that, plus the 2000-vs-2100 leap rule.

Fourth, **the `--stats` report is a log event too, on a target `-l` cannot silence.**
It used to go straight to stdout on the grounds that it is a report to read rather
than a log line — which was right about the formatting and wrong about the
destination, since it left the profile out of exactly the file kept to record a
render. It now emits under `STATS_TARGET` (`crust_render::stats`), and the subscriber's
filter admits that target at any level while applying `-l` to everything else: `--stats`
is an explicit request, so `--stats -l warn` must not silently produce nothing.
`event_enabled` is a named function rather than an inline closure so that case is
unit-tested. Two formatting consequences follow from it being one event: the report is
accumulated and emitted whole (a per-row emit would stamp all forty rows), and it opens
with a newline, or the event prefix would indent the first rule and only that one. It
still goes to **stdout**, so `--stats > report.txt` is unaffected.

Environment overrides, all of which exist to A/B an optimization against the behaviour it
replaced: `CRUST_STREAM_IMPORT=0` forces the single-stage USD import; `CRUST_MESH_BAKE=0`
forces every mesh to be instanced instead of baking single-placement geometry flat (output
is bit-identical with it set, which is what separates a deferral bug from a baking
difference); `CRUST_PTEX=0` declines every Ptex texture so surfaces fall back to their
constant `baseColor`; `CRUST_PTEX_MAX_LOG2` caps the per-face texture resolution loaded
(log2 edge length, default 5 = 32x32); `CRUST_SUBDIV=0` forces every
`crust:subdivisionLevel` to 0 so subdivision-surface meshes render their base cage — the
A/B that separates a subdivision artifact from a material or lighting one; `CRUST_TEX=0`
declines every UV texture so a MaterialX surface renders on its constant inputs (the
`CRUST_PTEX=0` of the UV path), and `CRUST_TEX_MAX` caps each decoded texture tile's edge
length in pixels (default 1024). Texture *filtering* has three more, which pair up:
`CRUST_TEX_MIP=0` and `CRUST_PTEX_MIP=0` build no mip pyramid (one level per tile / per
face, a third less memory, and `eval`'s width ignored structurally rather than by a
branch), while `CRUST_RAY_CONES=0` zeroes every footprint with the pyramids still
resident. Either side alone is bit-identical to the pre-filtering renderer, and the two
produce the same image as each other — which is what makes them an honest A/B of the two
halves: the pyramid, and the footprint that selects from it.

Texture *residency* has two more. A texture with a `.tx` beside it (same path, extension
swapped: `foo.1001.exr` → `foo.1001.tx`) is streamed instead of preloaded, paging 64x64
tiles under a byte budget set by `CRUST_TEX_CACHE_MB` (default 1024, matching OIIO's own).
That lookup is **always on**: a `.tx` exists only because someone converted the texture
for streaming. `CRUST_TEX_STREAM=0` turns it off and preloads everything, which is the A/B
(`=1` is accepted and changes nothing). It falls back to preloading for any texture it
declines — no `.tx`, a UDIM set only partly converted (it would stream with black holes),
a mip chain reduced in a different colour space, a file it cannot read — so a stray `.tx`
can make a render slower but never break it. **`--auto-tx`** creates the missing ones:
before a texture opens, every tile whose `.tx` is missing or older than its source is
converted beside it (`tiled::make_tx_atomic`, in parallel, written to a temporary and
renamed so an interrupted run leaves no truncated `.tx` to trust). A float source keeps
`half` tiles even when its values fit in `[0, 1]` (`TxFormat::FromSampleType` —
`maketx`'s by-range default would band linear data), and the colour space recorded is
the one the material binds with, `auto` resolved. A tile that fails to convert preloads
its texture. Without the flag a stale `.tx` is still used, with a warning. **Ptex is
excluded from all of it** (`tiled::is_ptex`): a `.ptx` is already a tiled per-face mip
pyramid that streams as it is (`CRUST_PTEX_STREAM`), so it is never converted — `make_tx`
refuses one outright — and a `foo.tx` beside a `foo.ptx` is never read in its place, even
when the `.ptx` reaches `load_texture` as a UV texture. A `.tx` is backed by either a tiled TIFF (`u8`
tiles) or a tiled mip EXR (`half` tiles), picked by magic number rather than extension.

Ptex has the same pair. `CRUST_PTEX_STREAM=1` swaps `PtexColor` for a `PtexStream` that
pages one tile of one level of one face out of the `.ptx` under `CRUST_PTEX_CACHE_MB`
(default 1024, matching the UV budget), and falls back to preloading for a file it cannot
open — same policy, same reason. It needs **no conversion step**, which is the whole
difference between the two: a `.ptx` is already a tiled per-face mip pyramid, so the
missing piece was never a format but a cache, and that cache now lives in `ptex-rs`
(`SharedReader`) rather than here. With it on, `CRUST_PTEX_MAX_LOG2` stops being load-bearing:
an unset cap means *uncapped*, and setting one is how the two backends are compared at a
resolution both hold.
**`CRUST_PTEX_STREAM_MIPSPACE` is the second gate, and it is on the correctness side of
the trade rather than the tuning side.** A `.ptx`'s stored mip levels were reduced in the
file's own encoding while `PtexColor` reduces in linear light from the decoded base, which
is the same mismatch `crust:mipspace` **refuses** for `.tx` — and refuses for the reason
it is dangerous, not for tidiness: level 0 stays perfectly correct and only coarser levels
are wrong, so it reads as a filtering bug rather than a colour one. So it is refused here
too. The default (`linear`) declines to stream a texture whose lookups could reach such a
level and preloads it instead, which under a mip pyramid means *every* mipmapped `.ptx`:
`CRUST_PTEX_STREAM=1` alone therefore streams nothing on a normal render, and `--stats`
says so on the `backend` line. `=file` takes the file's chain and the residency with it
(the island figures below are all `=file` figures), and `CRUST_PTEX_MIP=0` is the third
way out — no pyramid, so nothing to get wrong, exact and uncapped. See
`docs/ptex_streaming.md`.

## Measuring a change

Three things about this codebase make naive measurement actively misleading, so the
tooling above exists to work around each.

1. **Sequential wall-clock comparisons lie.** On a busy machine, measuring before and
   after minutes apart reported a **12% regression** for a change `bench_ab.sh` then showed
   to be a 4-5% *improvement* — the difference was background load. Always compare two
   binaries with `bench_ab.sh`, which alternates A and B within the same seconds so load
   lands on both. Report min *and* mean.
2. **Small steps are below the noise floor.** The run-to-run spread reaches 15%, so a
   1-5% change cannot be resolved by timing at all. Count instructions instead — they are
   deterministic:

   ```bash
   RAYON_NUM_THREADS=1 valgrind --tool=callgrind --cache-sim=no --branch-sim=no \
       target/release/crust-render -i samples/cornellbox.usda -o /tmp/x.exr -s 2
   callgrind_annotate --inclusive=no callgrind.out.<pid>
   ```

   Function names resolve from the symbol table with no debug info; add
   `RUSTFLAGS='-C debuginfo=line-tables-only'` only when you want line-level detail.
   Note callgrind counts instructions, not cycles, so it under-reports anything whose gain
   is cache behaviour — and it cannot see `panic = "abort"` at all.
3. **Compare images at `-s 16`, never higher.** `min_samples_per_pixel` defaults to 32 and
   the adaptive early-stop needs `taken >= min_spp`, so at 16 spp every pixel takes exactly
   16 samples. Above that a single-ulp difference changes a pixel's sample budget and
   cascades, making a bit-identical change look structural. `check_images.sh` pins this.

For a change that *does* legitimately alter output (a different BVH reorders exact ties),
prove it is noise rather than bias by checking the difference falls as 1/√N across spp
rather than plateauing.

## Workspace layout

Six crates under `crates/`:

- **`crust-rt`** (lib name `crust_rt`) — the intersection kernel, factored out the way
  `openqmc-rs` was, behind a deliberately **Embree-shaped API**: `Geometry` values
  (triangle meshes with optional per-vertex shading normals, analytic spheres, disks
  and open cylinders — the last two exist for UsdLux's `DiskLight` / `CylinderLight` —
  round curve segments, `Instance`s — which nest — with transform motion blur) attach to a
  `SceneBuilder` with per-geometry visibility masks; `commit()` builds the acceleration
  structure; `Scene::intersect`/`Scene::occluded` mirror `rtcIntersect1`/`rtcOccluded1`.
  Hits are plain `Copy` `RayHit`s carrying `geom_id`/`prim_id` — the kernel never sees
  materials. Instanced hits report the *instance's* top-level `geom_id` with the inner
  `prim_id`. Internals: watertight Woop-2013 triangles, rounded-cone curves, and the
  parallel deterministic SBVH build collapsed to BVH4 (details below). Depends only on
  glam + rayon; deliberately swappable for Embree bindings behind the same seam.

- **`crust-mtlx`** (lib name `crust_mtlx`) — the MaterialX `.mtlx` reader,
  factored out the same way as `crust-rt`: a standalone library with **no crust
  dependency** (roxmltree + glam only) behind a seam crust-core consumes.
  `parse` (XML → name-addressed graph), `value` (the one runtime value), `eval`
  (the graph compiled to a slot-indexed `Program`), `bsdf` (the closure tree —
  `layer`/`mix` flattened to weighted `Lobe`s, and `edf` to `Emission` terms), and `compile()` running all three for one
  material node. It names the one thing it asks of its host — `Texture`, a
  `(u, v) → RGBA` sampler — and crust-core re-exports that trait as its own
  `Texture2D`, exactly as it adopts `crust_rt::Geometry`. What a renderer does
  with the lobes is not decided here; crust's OpenPBR pooling is in crust-core.
- **`crust-core`** — the engine as a library (`crust_core`): renderer, integrator,
  materials, lights, volumes, path guiding, USD import (MaterialX through
  `crust-mtlx`, with `material/materialx.rs` as the adapter — `MtlxMaterial` and
  the lobe pooling onto `OpenPBR`) — everything above the intersection layer, which it consumes from `crust-rt` through `rt_world.rs`
  (`WorldBuilder`/`World`: kernel geometries paired with a `geom_id`-indexed
  material table).
  UI-free by design: no progress-bar or image-encoding dependencies; progress is
  reported through a `ProgressCallback`, and fallible entry points return
  `crust_core::Error` instead of exiting.
  **`stats.rs`** collects per-phase timings and scene counts (`RenderStats`,
  inspired by Guerilla Render's "Profiling And Statistics"): the importer fills
  in its own phases and the counters onto `Scene::stats`, the host appends
  render/output phases, and the report prints three blocks — statistics, profile
  by execution tree, profile by time. Collection is one `Instant` per *phase*,
  never per ray, so it costs nothing in the integrator and is always on; only
  printing is gated (`--stats`). Two primitive views are reported because for an
  instanced scene they answer different questions: `top_level` is what the root
  BVH traverses, `unique` descends into instances counting each distinct
  prototype **once** and is therefore what occupies memory.
- **`crust-assets`** (lib name `crust_assets`) — the host side of
  `crust_core::AssetLoader` for a program that reads files: `FileAssets`
  implements the trait over `exr`, `image` and `ptex-rs`, and the decoders
  behind it are public — `load_exr_environment` / `load_image_environment`,
  `parse_ies` / `load_ies` (IES LM-63 → `crust_core::IesProfile`),
  `PtexColor` (+ `read_channel`, `max_log2_from_env`), `PtexStream` (the
  tile-paging backend behind `CRUST_PTEX_STREAM`), `UvTexture` (UDIM sets,
  the `CRUST_TEX_MAX` cap) and one `srgb_to_linear`. Everything that knows a
  file format lives here, so the probe examples decode a texture *exactly* the
  way the renderer does instead of carrying copies (`read_channel` used to
  exist three times). Owns the `CRUST_PTEX`, `CRUST_TEX`, `CRUST_PTEX_MAX_LOG2`,
  `CRUST_TEX_MAX`, `CRUST_PTEX_STREAM` and `CRUST_PTEX_CACHE_MB` switches.
- **`crust-render`** — the thin CLI binary. Parses args, builds a `Scene` (with
  `crust_assets::FileAssets`), calls the `Renderer` (wiring an `indicatif` bar to the
  progress callback), writes the EXR and the tone-mapped PNG. `main.rs` is the only
  source file, and it only *writes* images — decoding is `crust-assets`.
- **`utils`** — math/RNG helpers (`random*`, `random_cosine_direction`, `align_to_normal`,
  `balance_heuristic`, `power_heuristic`, `clamp`, `Lerp`). Depended on by `crust-core`.
Two libraries were extracted out of this tree into repositories of their own on the
same principle (zero crust types in the API, generally useful, published) and are
consumed as ordinary dependencies:

- **`openqmc-rs`** (crates.io; crate `openqmc-rs`, lib name `openqmc`) — a self-contained, from-scratch Rust port
  of [AcademySoftwareFoundation/openqmc](https://github.com/AcademySoftwareFoundation/openqmc)
  (Apache-2.0), the quasi-Monte Carlo sampling library. Modules map one-to-one to the
  upstream `oqmc/*.h` headers (`pcg`, `reverse`, `rotate`, `permute`, `encode`, `float`,
  `range`, `owen`, `rank1`, `lookup`, `stochastic`, `state`, `sampler`, plus the samplers)
  and reproduce the upstream sample values **bit-for-bit** (pinned by
  `tests/golden_upstream.rs`, generated from the real C++ headers). It exposes the native
  **domain-tree** API: build a root `Sampler<T>` per `(x, y, frame, index)`, derive
  independent 4D sub-patterns with `new_domain(key)` (padding), draw ≤4 dims per domain
  with `draw_sample*` (high-quality stratified) or `draw_rnd*` / `rng()` (a `pcg::Rng`
  stream for incidental/unbounded draws). Six samplers — Owen-scrambled `SobolSampler`
  (the renderer's, aliased `crust_core::PathSampler`), `LatticeSampler`, `PmjSampler`, and
  their blue-noise variants `SobolBnSampler`/`LatticeBnSampler`/`PmjBnSampler` (optimised
  tables bundled as LE binary blobs in `src/data/`). Depended on by `crust-core`
  (materials, `guiding/`, `volume.rs`, `tracer.rs`) for every stochastic draw. Idiomatic
  divergences from the C++: the caller-allocated `void*` cache (a GPU concern) becomes a
  lazy process-global, keeping every `Sampler<T>` a small `Copy + Send` value.

`crust-core/src/lib.rs` re-exports the public surface (`Renderer`, `Scene`, `Camera`, the
material types, `simple_scene`, `get_settings`). Prefer importing from `crust_core::` roots.

## Render pipeline (the big picture)

1. **`main.rs`** builds a `Scene { camera, world, lights, settings, volumes }` — either from
   USD (`Scene::from_usd`) or the procedural fallback (`world::simple_scene` + `get_settings`).
2. **`Renderer`** (`tracer.rs`) drives sampling. Two entry points, both Rayon-parallel:
   - `render()` — parallel over pixels within each scanline row.
   - `render_with_tiles()` — parallel over 16×16 tiles (the `--bucket` path).
   Pixel reconstruction (`filter.rs`, `crust:pixelFilter` / `--filter`) is **filter
   importance sampling**, not splatting: each pixel warps its jitter through the
   filter's distribution and weights radiance by `f/p`, keeping every per-pixel
   mechanism (adaptive early-stop, QMC domains, pass blending) intact. The default
   is triangle at radius 1.0; box at radius 0.5 reproduces the historical in-pixel
   jitter bit-identically (`--filter box` when comparing against pre-filter
   renders). Mitchell is the only kind with negative weights.
3. **`trace_path()`** (`tracer.rs`, public wrapper `ray_color()`) is the integrator — an
   **iterative** path tracer in two passes: a forward walk that traces one segment per
   bounce and records a `VertexRec` per vertex, then a backward gather that folds the
   records into the radiance estimate and emits guiding training samples (which need
   the radiance from the rest of the path — the reason for the backward pass). Features:
   - **MIS** combining direct light sampling and BRDF sampling. The heuristic is
     selectable via `SamplingStrategy` (`crust:samplingStrategy` attr / `--strategy`
     flag): `power` (β=2 power heuristic — the default and historical behavior),
     `balance`, plus diagnostic single-strategy modes `light` (NEE at full weight,
     bounce-hit emission on light-list lights dropped) and `bsdf` (no shadow rays,
     bounce emission at full weight). All four are unbiased — the strategy's
     `light_weight`/`bounce_weight` pair is a partition of unity (pinned by a unit
     test); route BOTH sides of any new weight through the strategy or emission gets
     double-counted. `utils` now has both `balance_heuristic` (true `a/(a+b)` —
     historically this name computed the power formula) and `power_heuristic`
     (`a²/(a²+b²)`). `samples/veach_mis.usda` is the classic Veach comparison scene.
     Emission at a bounce-arrival vertex is owned by the *previous* vertex's record
     (`next_emit` + MIS weight); counting it at the vertex itself too would double it.
   - **Russian roulette** from the 4th vertex on (`RR_START_BOUNCE`): survival tracks
     path throughput with a probability floor (`RR_MIN_PROB`), factor divided out on
     survival.
   - Carried-medium transport (subsurface / glass interiors) when a ray holds
     `Some(Medium)` (set by transmissive OpenPBR refraction — see `ray.rs` /
     `medium.rs`): weighted analog free-flight sampling at the extinction majorant with
     the chromatic correction `e^{(σ̄−σₜ)t}` (gray media reduce to the classic
     albedo/Beer-Lambert forms). Carried-medium scatter vertices run **no NEE** and keep
     `prev = None` (full-weight next emission) — that pairing is what avoids double
     counting there. Free-space rays are unaffected.
   - **Volume regions** (`volume.rs`): free-standing smoke/fog/absorption/fire volumes
     held on `Renderer.volumes`, *outside* the surface BVH so their bounds never occlude
     shadow rays. Each `VolumeRegion` is an oriented box (composed prim xform) with a
     `DensityField` (`Homogeneous` | `Noise` fBm | `Grid` trilinear voxels) and
     σₛ/σₐ/g/emission (densityScale pre-folded into the coefficients). Per segment the
     integrator clips `Volumes::sample_interaction` to `min(t_surface, t_medium)` — the
     nearest-event competition between surface, carried medium and regions is then exact.
     Distance sampling is weighted delta tracking against the summed per-region majorant
     (chromatic-safe null collisions; absorption decays the weight instead of
     analog-terminating). Volume scatter vertices run **NEE with MIS** (`volume_nee`):
     the phase mixture's value == pdf (`PhaseMix`), and the bounce-side
     `bounce_emission_weight` has a matching `PrevVertex::Phase` arm — the two are a
     coupled pair exactly like the surface NEE pair; change one and you double-count
     emission. Shadow rays use `shadow_transmittance` (boolean surface occlusion ×
     `Volumes::transmittance` — ratio tracking, exact Beer-Lambert fast path when all
     crossed regions are homogeneous). Volume emission accumulates `σₐ·Lₑ` along the walk
     into `VertexRec.segment_emit`, which the gather adds **outside** `atten` (it is
     already weighted; folding it in would double-attenuate). Emission reached by a
     bounce (`next_emit`) is stored pre-multiplied by the arriving segment's attenuation.
     `volumes.is_empty()` short-circuits everything — volume-free scenes render as before.
   - A sky-gradient background when nothing is hit (attenuated by, and adding the
     emission of, any volumes the escaping segment crossed).
4. **Path guiding** (opt-in via `crust:pathGuiding`, `guiding/` module): a pure-Rust
   Practical Path Guiding SD-tree (`GuidingField`). `render_guided()` runs training
   passes at 2, 2, 4, 8, … spp (geometric, floored at 2 so every pass can estimate
   its own variance), splats `(position, direction, luminance·cos²)` samples
   into the field between passes, then renders the final pass with one-sample MIS
   between the frozen field and the BSDF (mixture pdf; secondary bounces only —
   primary vertices sit far below the field's spatial resolution). All passes
   (training + final) are blended into the output weighted by inverse variance, so
   the training budget is not discarded. Delta/transmissive
   materials (`Material::eval` → `None`) and untrained regions fall back to pure BSDF
   sampling. The NEE weight competes against the same mixture pdf — keep the two sides
   consistent or emission gets double-counted.
   The training passes double as a **guiding efficiency estimate** (Li et al. 2026,
   "Path Guiding in Disney's Zootopia 2"): efficiency `E = 1/(wall-clock cost × MRSE)`,
   comparing the first pass (field untrained → effectively unguided) against the last
   training pass (field most trained). MRSE normalizes each pass's per-pixel variance
   by one *shared* reference image (the blend of all training passes) — never by the
   pass's own noisy mean, which would correlate numerator and denominator and break
   the 1/spp scaling the comparison relies on. If `ΔEff < 1`, the final pass renders
   unguided (training passes still blend in; every pass is unbiased either way).
5. **Adaptive sampling**: pixels stop early once they hold `crust:minSamplesPerPixel`
   samples and the relative standard error of the pixel mean drops below
   `crust:varianceThreshold` (0 disables). Applies to main/final passes, never to
   guiding training passes.
6. The CLI writes the linear EXR to the `-o` path and a tone-mapped sRGB PNG next to it
   (same path, `.png` extension) — e.g. `-o renders/foo.exr` produces `renders/foo.exr`
   and `renders/foo.png`. Tone mapping and PNG encoding live in `main.rs`; the engine
   crate only produces the `Buffer`.

## Core traits (extension points)

- **Geometry & intersection** — there is no `Hittable` trait anymore: all intersection
  lives in the **`crust-rt`** kernel crate (see the workspace layout), and crust-core
  talks to it through `rt_world.rs`: a `WorldBuilder` pairs every attached
  `rt::Geometry` with its `Arc<dyn Material>` (`attach(...) -> geom_id`), and the
  committed `World` resolves kernel hits back to materials **by `geom_id`** —
  `World::intersect` returns a `WorldHit { rec: HitRecord, mat, geom_id, prim_id }`,
  `World::occluded` is the shadow-ray early-exit query (NEE never uses closest-hit).
  `HitRecord` (`hittable.rs`) remains the material-facing `Copy` hit geometry (point,
  ray-facing normal, `t`, `front_face`). Inside the kernel: `Sphere`,
  triangles with one shared **watertight** intersector (Woop et al. 2013 — dominant-axis
  shear, 2D edge functions with f64 fallback on exact-zero ties; no pinholes along
  shared edges), `RoundCurves` (sphere-swept cones for hair), and `Instance` (a
  committed inner `rt::Scene` placed by a transform — rays transform into local space
  with unnormalized direction so `t` carries over, normals map back by
  inverse-transpose; an optional end-of-shutter transform lerps per-ray for motion
  blur). Per-geometry masks gate intersection on `ray.mask` (`MASK_*` consts,
  re-exported from the kernel). The build is reference-based **SBVH** (binned object
  SAH + spatial splits gated by the α-overlap test, references clipped via exact
  Sutherland-Hodgman for triangles and duplicated across children), runs subtrees in
  parallel via `rayon::join` above 4096 refs, is **deterministic** (input-only
  decisions, pinned by a build-twice test), and is then **collapsed to BVH4**: 128-byte
  4-wide SoA nodes whose slab tests run on `Vec4` lanes (`safe_inv3` keeps
  zero-direction components NaN-free; closest-hit traversal orders lanes near-to-far,
  occlusion traversal early-exits). Traversal is mask-driven: `RaySlab` pre-splats the
  ray once per query, `cmple(..).bitmask()` yields all four lane verdicts at once, and
  a validity nibble in `WideNode::flags` masks unused lanes (they cannot just be given
  empty bounds — the slab test's per-axis min/max un-inverts an inverted box).
  Leaf payloads live in a side `Leaf` table (keeping the node at two cache lines), and
  each leaf's triangles are packed into **4-wide `Tri4` SIMD packets**: the Woop shear
  is per-*ray* (`RayShear`, derived once per traversal), so four triangles are
  intersected per vector round, with lanes whose edge functions come out exactly `0.0`
  handed back to the scalar path for its f64 tie-break — watertightness intact. The
  packet and scalar intersectors are **bit-identical** (pinned by
  `simd_matches_scalar_bitwise`); change one and you must change the other.
  `MIN_LEAF_PACKED` (4) is the leaf floor for all-triangle ranges so packets fill,
  while non-packable prims keep `MIN_LEAF` (2) — see `docs/simd.md` for the audit,
  the measurements, and why `std::simd` is not used (nightly-only on stable).
- **`Material`** (`material/material.rs`) —
  `scatter_importance(r_in, rec) -> Option<ScatterSample>` used by the integrator
  (`ScatterSample.delta` marks singular lobes like transmission: never mixed with a
  continuous density, no tracer cosine, emission carried at full weight),
  `eval(r_in, rec, wi) -> Option<(value, pdf)>` (evaluate the *continuous* component
  toward a given direction — what NEE and guided MIS need; `None` = no continuous
  component at all, and per its contract that decision must never depend on `wi`),
  `emitted()`, and `emitted_at(r_in, rec, cos_theta_o)` — the hit-aware emission the
  integrator calls at a surface hit, defaulting to `emitted_directional` so only a
  material whose emission is textured need override it. Keeping `emitted()` and
  `emitted_directional()` hit-free is deliberate: the **light list** reads the first,
  and the coat's angular emission factor stays unit-testable against a bare cosine.
  A material that emits only through `emitted_at` must therefore never become a
  light-list entry, or NEE would sample it at zero radiance while the bounce side saw
  the real value. Four implementations: **`OpenPBR`**,
  the single übershader for all surfaces (with `diffuse`/`metal`/`glass`/`glossy` preset
  constructors used by `world.rs` and the USD fallback), **`Emissive`**, a pure
  emitter with no geometry knowledge, and **`MtlxMaterial`**
  (`material/materialx.rs`), which evaluates a MaterialX graph per shading point and
  *delegates* the BSDF to the `OpenPBR` it reduces to — so sampling, MIS
  densities and energy compensation stay in one place — and **`PreviewSurface`**
  (`material/preview_surface.rs`), the same delegation for a `UsdPreviewSurface`
  whose inputs are driven by `UsdUVTexture`s. Two hooks gate the
  per-triangle side tables the importer would otherwise build for every mesh:
  `face_texture()` for Ptex and `uses_uv()` for the `primvars:st` chart. Shared shading helpers (aniso GGX VNDF sampling,
  Schlick/F82 Fresnel, EON diffuse, Charlie sheen, thin-film, Cauchy dispersion) live in
  `material/brdf.rs`. The OpenPBR formulas are aligned against the MaterialX nodegraph
  and Adobe's `openpbr-bsdf` reference — the item-by-item alignment record (with the
  remaining gaps, e.g. no LUT-based multiple-scattering compensation and no random-walk
  SSS entry) is `docs/openpbr_reference_alignment.md`.
- **`Light`** (`light.rs`) — `sample_point`/`pdf`/`emission`/`material`. The one
  implementation is **`AreaLight`**: a `LightShape` (pure emitting geometry —
  `SphereShape`, `RectShape`, and `AffineShape`, a unit sphere / disk / tube under any
  invertible affine) paired with the `Arc<Emissive>` its scene geometry carries.
  A light's `Emissive` is built by `Emissive::light`: **one-sided** (emission only
  toward the geometry's front, which is why the rect light's triangles are wound with
  their normal along local −Z) and optionally **shaped** (`lux::Shaping`). Both MIS
  halves ask it the same question — `Emissive::radiance_toward(dir, front)` — NEE from
  `AreaLight::sample_li`, the bounce side through `Material::emitted_at`; route any new
  directional emission through that one function or the two strategies see different
  lights. `LightShape::inv_pdf_area` is the reciprocal of the shape's *own* sampling
  density (default: its area, i.e. uniform); `AffineShape` samples uniformly in local
  area, so its world density varies under a non-uniform scale and it reports
  `local_area · |det M| · |M⁻ᵀ n|` — both MIS halves read it, so it need only be the
  density actually sampled. Where the area density `d²/(cos θ_l · A)` is not finite
  (back-facing, edge-on) `AreaLight` **refuses** the point on both sides, pbrt-v4's
  way: `sample_li` returns `None` and `pdf_at_point` returns **0**, which
  `bounce_emission_weight` reads as "NEE never delivers this" and gives full weight.
  It used to add `1e-4` to that denominator instead, an NEE bias of
  `1 + 1e-4/(cos θ_l · A)` (+12.7% measured on a 1.3e-3 m² disk,
  `docs/light_sampling.md` §3.10); do not reintroduce a finite stand-in.
  A shape with a better strategy than area sampling implements
  **`LightShape::sample_solid_angle` / `solid_angle_pdf`** (default `None`, meaning
  "sample me by area"): a point plus its *solid-angle* pdf as seen from the shading
  point, which `AreaLight::sample_li` and `pdf_at_point` both prefer when present. The
  contract is what keeps the two MIS sides one strategy — whether the shape answers
  must depend on `from` alone, never on `u`/`v`, and the two methods must answer for
  exactly the same `from`s with the same density. **`SphereShape` samples the cone it
  subtends** (Shirley et al. 1996, pbrt-v4's `Sphere::Sample`): uniform over the
  visible cap, pdf `1/(2π(1 − cos θ_max))`, with pbrt's small-angle threshold
  `sin² θ_max < sin² 1.5°` (`SMALL_CONE_SIN2`, where `1 − cos θ_max` cancels in f32)
  but **not its approximation below it**: pbrt draws `sin² θ = u·sin² θ_max` there,
  whose density goes as `cos θ` against a constant pdf, so crust takes
  `1 − cos θ_max = sin² θ_max/(1 + cos θ_max)` and draws `t = u(1 − cos θ_max)`,
  `sin² θ = t(2 − t)` — exact and cancellation-free (`SubtendedCone::sample`). A cone
  whose pdf would overflow f32 is refused on both hooks, and area sampling only from
  inside the sphere. Area sampling spent at least half its
  shadow rays on the hemisphere facing away. An **`AffineShape` sphere** (non-uniform
  scale) samples the *unit* sphere's cone in local space and maps the point through the
  placement — exact, because an affine map preserves which points of a convex surface
  face a given point — with the direction map's solid-angle Jacobian
  `|Mω|³ / |det M|` on the pdf (`world_solid_angle_pdf`), so its density varies over
  the cap and both MIS sides evaluate it at the point. `AffineShape` disks and tubes are
  still area-sampled.
  **`RectShape` samples the spherical rectangle it subtends** (Ureña, Fajardo & King
  2013, as pbrt-v4's `SampleSphericalRectangle`): uniform in solid angle, pdf `1/Ω`,
  and the point is returned through the rectangle's own `(s, t)` so it lies on the
  light exactly as an area sample does (the triangles a bounce hits, the texel a card
  reads). It runs in **f64**, because `Σg − 2π` cancels in f32 at the solid angles it
  hands back to area sampling at, and its setup is not the paper's: in the rectangle's
  frame the four edge-plane normals are axis-aligned in closed form, so each corner
  angle is `atan2(h·|v|, ±x·y)`, and the sums the map needs are arguments of complex
  products — **one** `atan2` in all, `g2 + g3` kept as its normalised `(cos, sin)`.
  Area sampling stays, on both hooks alike (`RectShape::spherical_rect`), for a sheared
  parallelogram, from behind the one-sided light or on its plane, and outside
  `[1e-4, 6.22]` sr (pbrt-v4's bounds). Textured cards take it too, since crust does not
  sample the image (pbrt gives the map up only because it does). It is not a free win:
  ~110 ns more per NEE sample than area sampling, and on a glossy receiver it can lose,
  because area sampling's `r²/cos θ_l` density happens to follow some highlights —
  measured in `docs/light_sampling.md` §3.9. The bilinear cosine warp (Hart et al.
  2020) is not done: it needs the receiver's normal, which `Light::sample_li` is not
  given.
  Lights are stored in a `LightList` and their surfaces are also attached to `world` as
  emissive geometry — masked out of **camera** rays by default (the industry convention:
  a light in frame does not show its source; `crust:light:cameraVisible` opts back in,
  an authored `crust:rayMask` wins outright, and shadow/indirect rays always see it) —
  the `AreaLight` records the geometry's `geom_id`, which is how the integrator
  attributes a bounce-hit emissive surface to its light (`LightList::find_by_geom`).
  **NEE samples one light per vertex**, picked by the `LightList`'s selection,
  `crust:lightSelection` / `--light-selection`, built in `Renderer::new` from the
  settings.
  - **`power` (the default)** is defensive:
    - lights at infinity keep their uniform share;
    - the finite lights split the rest `DEFENSIVE_SHARE` (½) evenly and ½ in
      proportion to `Light::power`;
    - a black light gets zero.
  - **`uniform`** is one in N, the renderer before this was a choice, reproduced
    **bit for bit**: 0 differing pixels on all 22 checked-in samples at 16 spp.

  **Both halves of the design were forced by measurement** (`docs/light_sampling.md`
  §3.8):
  - **The fixed infinite share.** Pure power selection, pbrt-v4's `PowerLightSampler`
    with a dome's power taken against the scene radius, made `domelight` 1.42× and
    `usdlux` 1.47× noisier at 16 spp. The sun took 88% of the rays, yet in its own
    shadows the dome is the only light.
  - **The even half.** Power is blind to distance and visibility, and the even half
    bounds what that blindness costs.
  - **Result, over four seeds:**
    - a key among seven dim fills: 4.6× lower relMSE;
    - `usdlux`: 2.6% lower;
    - `veach_mis`: 6% higher. Its equal-radiometric-power lights are tinted, so their
      *luminance* powers differ by ±10%, and each lights its own band of the plates,
      so the rays moved away from the red and blue lights cost exactly the pixels
      those lights own;
    - everything else within 1%;
    - time within noise.

  The light strategy's MIS density is `light.pdf · pmf`, computed by
  `LightList::density` on **both** sides. `pick`, `find_by_geom` and `iter` all hand
  back the same `pmf` for the same light. Under uniform, `density` is the historical
  division `pdf / n`, not `pdf · (1/n)`, which rounds differently when n is not a power
  of two; that is what keeps the A/B exact. A light with `pmf = 0` keeps its bounce
  emission at full weight, since NEE never samples it.

  `Light::power` is a *flux* — a sampling weight, never shading — and `None` at
  infinity:
  - `AreaLight` uses `Emissive::flux`: `π A L` unshaped whatever the shape, times the
    texture's mean texel for a textured card.
  - A shaped light uses `Shaping::integrate`, taken in rings about the axis *out to
    the cone angle only*, so a 2° spot is resolved as well as a hemisphere.
  - The table is inverted by **CDF, not an alias table**. The map from the pick
    dimension to a light stays monotone, so the samples that pick light *k* remain one
    contiguous slice, as under uniform.
  - A `LightList` that never passes through `select_by` (a bare `ray_color`, or a list
    with a light `add`ed since) picks uniformly, consistently on both sides.
  - A per-light `DEBUG` line gives each light's power and pick probability.

  Emissive geometry with no light-list entry is handled: the bounce keeps its emission
  at full weight.

Sampling goes through the **`openqmc`** crate's native domain-tree API (see the workspace
layout above). The integrator (`tracer.rs`) threads the sampler *by value* — no stateful
`&mut dyn Sampler`: `render_pixel` builds a root `PathSampler::new(x, y, frame, index)` per
sample (with an extra `new_domain(tile)` so images wider/taller than 256 stay decorrelated,
since OpenQMC's pixel decorrelation tiles at 256), draws the camera dims from a `K_CAMERA`
domain, and hands the root to `trace_path`. Each path vertex derives `path.new_domain(depth)`
and each sampling event a further keyed sub-domain (`K_NEE`, `K_BSDF`, `K_GUIDE`, `K_PHASE`,
…, keys defined atop `tracer.rs`); materials draw one 4D block from the `SobolSampler` domain
they are handed. Unbounded/incidental draws — Russian roulette, volume delta-tracking,
carried-medium free flight — use `draw_rnd` or a `pcg::Rng` seeded from a domain
(`domain.rng()`), matching OpenQMC's `drawSample` vs `drawRnd` split. Tests that just need
randomness use `openqmc::pcg::Rng`.

## USD import (`scene/usd_import.rs`)

The only scene format. `load_scene` opens the stage, imports `RenderSettings` first (the
camera needs the aspect ratio), then traverses prims with an explicit stack that bakes the
Xform hierarchy into world matrices.

**Streaming import.** A production stage is dominated by USD itself, not by the
renderer's own structures: composing all of Moana costs openusd 75.74 GiB and 6m19s,
against 1.10 GiB and 2.6s for a stage masked to one element. So `load_scene` opens a
cheap index stage (`InitialLoadSet::LoadNone`) for the settings and the list of
top-level subtrees, then composes, traverses and **drops one masked stage per subtree**
(`stream_roots` + `traverse_into`), bounding the composed set at about one element.
On the island: 117.10 GiB / 13:20 → 43.76 GiB / 09:19, output pixel-identical.
`MIN_STREAM_CHUNKS` keeps the single-stage path for scenes too small to repay the
re-opens; `CRUST_STREAM_IMPORT=0` forces it.
The subtlety is cache keying: prototype paths (`/__Prototype_N`) are **numbered per
composition**, so every masked stage has its own `/__Prototype_0`. Anything keyed on
such a path must be scoped per stage, or one chunk's data is silently handed to the
next — see `MaterialCache::key` and `ImportCaches::epoch`. Keying materials on the bare
path cost 5 835 258 triangles before it was caught, and *no single element reproduces
it*: it needs two chunks that both carry prototype-internal materials.

Schema mapping:

- `UsdGeomMesh` → **either** world-space triangles in the top-level BVH **or** a
  local-space committed `rt::Scene` placed by an `rt::Geometry::Instance`, decided by how
  many times the geometry is placed. Prims with identical points/topology/material are one
  *distinct mesh* (content hash + memoized material Arcs, so binding paths compare by
  pointer); a distinct mesh placed **exactly once** is baked flat, anything placed more
  than once keeps one shared kernel scene and an instance per placement. Instancing what
  is placed once buys no sharing and costs every entering ray a transform, a slab setup and
  a cold descent into a second tree — and presents the parent BVH a box-of-a-box that
  spatial splits cannot tighten. On cornellbox this took instance descents from 3.85 to
  0.13 per camera ray.
  The decision is deferred: `emit_mesh` interns the triangles and reserves a `geom_id`
  (`WorldBuilder::reserve_slot`), and `flush_meshes` fills the slots in after the last
  streamed chunk — placement counts are only final then, and deciding **per chunk** would
  both make results depend on stage layout and give geometry shared between two elements
  one resident copy each. `CRUST_MESH_BAKE=0` forces the all-instanced behaviour; with it
  set the output is bit-identical, which is what separates a deferral bug from a baking
  difference. Baking a mirrored (`det < 0`) placement **swaps two indices**: world-space
  vertices wind the opposite way, so without it `front_face` inverts.
  Non-invertible transforms still bake immediately, and a mesh authoring
  `crust:motion:translate` always instances (a baked mesh has no transform left to lerp).
  `UsdGeomSphere` → analytic `Sphere` geometry.
- **Subdivision surfaces** (`scene/subdiv.rs`, via the pure-Rust
  [`opensubdiv-rs`](https://github.com/doubleailes/OpenSubdiv-rs) port of OpenSubdiv's
  Far/Sdc layers — zero dependencies, `forbid(unsafe_code)`, pinned by git tag). A mesh
  prim authoring `crust:subdivisionLevel` (int, default 0, clamped to 6) is uniformly
  refined that many times and **snapped to the limit surface**, with smooth per-vertex
  shading normals (the kernel's `TriangleMesh.normals`, interpolated by barycentrics).
  Deliberately **opt-in per prim** rather than triggered by `subdivisionScheme`: USD's
  fallback scheme is `catmullClark`, so honouring the scheme alone would subdivide
  virtually every mesh ever authored (all of the Moana island included), and USD has no
  standard per-prim refinement level — Hydra treats refinement as a render setting. The
  scheme still picks the algorithm once a level asks: unauthored/`catmullClark` →
  Catmark, `bilinear` → Bilinear, `loop` → Loop (all-triangle cages only), `none` →
  warn and render the cage. `creaseIndices`/`creaseLengths`/`creaseSharpnesses`
  (per-run or per-edge sharpness, 10 = infinite), `cornerIndices`/`cornerSharpnesses`
  and `interpolateBoundary` are honoured; `holeIndices` and
  `faceVaryingLinearInterpolation` are not. Refinement happens in `mesh_source`,
  *before* interning, so every path (direct bake, deferred instance-vs-bake,
  prototypes) sees it exactly once and `MeshKey` dedupes on the refined arrays. A
  malformed cage or refiner error warns and degrades to the cage. **Ptex keeps
  indexing the base cage**: refined triangles carry explicit corner UVs
  (`FaceMap.uvs`) mapping them back into their cage face's unit square (a synthetic
  face-varying channel refined with linear-everywhere interpolation), and
  `check_face_count` compares the texture against the *authored* face count. Baked
  placements push normals through the inverse transpose (`bake_normals`), matching the
  kernel's instance path exactly — mirrors included. Sample scene:
  `samples/subdivision.usda` (levels 0–3 plus a fully edge-creased cube that stays a
  cube); `CRUST_SUBDIV=0` is the kill switch.
  **Memory, measured** (the refiner retains every level 0..L, a ×4/3 geometric series
  over the last level): `subdivide()` transiently allocates **~313 B per refined face**,
  ~556–592 B when the material needs Ptex sub-face UVs (the synthetic fvar channel is a
  full parallel hierarchy); the returned mesh holds 48 B/face (84 with UVs). Pinned by
  the allocation-counting probe `cargo test -p crust-core --lib
  subdivision_memory_probe -- --ignored --nocapture --test-threads=1` (deterministic
  requested-byte ceilings ~25% above those numbers). End to end
  (`scripts/gen_subdiv_stress.py`, 1.18 M refined quads at level 3, A/B'd with
  `CRUST_SUBDIV=0`): traverse-phase peak +310 MiB — within 6% of the model — and
  kernel-resident memory scaling exactly ×4 per level (34.67 MiB at level 1 → 2.17 GiB
  at level 4). The whole-process peak is **not** opensubdiv: at level 4 traversal peaks
  at 1.16 GiB while the SBVH build over the baked result peaks at 2.19 GiB — the
  pre-existing build transient (see "Known incomplete work"), which subdivision merely
  feeds 4^L× more triangles.
- `UsdGeomBasisCurves` → an instanced `rt::Geometry::RoundCurves` batch: `linear` curves
  directly, `cubic` (bezier | bspline | catmullRom) flattened at 8 samples per span; widths
  (USD diameters) resolve per-vertex / per-curve / constant by array length.
- **Instancing** — both USD mechanisms reduce to the same thing, and share one code path
  (`collect_proto_parts` → `attach_proto_parts`): build a prototype's geometry *once*, then
  place it by transform. A prototype becomes a `Vec<ProtoPart>` — one part per bound leaf
  geometry, each a committed local-space `rt::Scene` plus its prototype-relative transform,
  material and ray mask. It is split per *part* rather than per prototype because `World`
  maps materials by top-level `geom_id`: a prototype binding two materials must become two
  instances or one material is lost. Prototypes are memoized by path in `ImportCaches`.
  - `UsdGeomPointInstancer` → one instance per entry of the per-instance arrays. The
    transform is USD's `translate ∘ orient ∘ scale`, under the instancer's own world
    matrix; `orientationsf` (quatf) wins over `orientations` (quath); `invisibleIds`
    prunes by `ids` (array index where `ids` is absent). The instancer's children are
    **not** traversed — prototypes are conventionally authored beneath it and are drawn
    only through it. Nested instancers and volumes inside prototypes warn and are skipped.
  - Native instancing (`instanceable = true` + a composition arc) → the prim's prototype
    (`/__Prototype_N`) is built once and shared by every instance; the instance's own proxy
    subtree is never descended into. Without this the importer re-read and re-hashed each
    instance's geometry (~30% of load time on a 2000-instance scene).
  - **Nesting.** A `PointInstancer` inside a prototype expands into real nested sub-scenes
    (`nested_instancer_parts`), one part per (prototype, part) so the grouping stays
    per-material: M nested placements of a K-part prototype become K parts, each a
    committed scene holding M instances. Flattening instead would multiply the outer
    instance count by the inner one — the blow-up instancing exists to prevent. The kernel
    nests to arbitrary depth. A nested *native* instance is skipped (upstream bug, below).
    Sample scene: `samples/nested_instancing.usda`. `MAX_INSTANCE_NESTING` (8) is a
    backstop against a malformed stage describing an instancing cycle.
  - `class` prims are abstract and never drawn on their own — only reached through the
    prototypes that reference them. `collect_proto_parts` deliberately ignores that rule,
    since naming a class as a prototype is how "geometry that exists only to be instanced"
    is authored. Sample scene: `samples/instancing.usda`.
  - Non-invertible instance placements (a zero scale — a common "hide this" idiom) are
    skipped: `rt::Geometry::Instance` requires an invertible transform.
- Any geometry prim may author `crust:rayMask` (int; bit 0 camera, bit 1 shadow, bit 2
  indirect — default all, except **light** source geometry which defaults to shadow|indirect;
  `crust:light:cameraVisible = 1` on a light prim re-adds the camera bit, an authored
  `crust:rayMask` wins outright — sample: `samples/light_visibility.usda`) to hide from
  ray categories, and `crust:motion:translate`
  (float3, world-space) to streak through that translation over the shutter (transform
  motion blur; primary rays draw a `K_TIME` shutter sample and every secondary/shadow ray
  inherits the path's time). Sample scenes: `samples/motionblur.usda`, `samples/curves.usda`.
- Materials resolve via `MaterialBindingAPI::compute_bound_material("full")`
  (`bound_material`), which uses the `full` purpose falling back to all-purpose, with
  bindings inherited from ancestors, binding strength honoured, and collection bindings
  read. The walk starts at the nearest ancestor that *carries* the API, because
  `MaterialBindingAPI::get` refuses any other prim — which is every mesh under a bound
  group. `preview` bindings are never used. Prims under a `proxy` or `guide` purpose are
  pruned with their subtree, like `active = false`. Then dispatch is on the bound
  shader's `info:id`:
  - `UsdPreviewSurface` → mapped into `OpenPBR` (portable; `diffuseColor→baseColor`,
    `metallic→baseMetalness`, `roughness→specularRoughness`, etc.). Constant inputs
    are still read **undecoded** (the openspec `add-material-color-management` change
    owns that). An input connected to a **`UsdUVTexture`** makes the material a
    `PreviewSurface` (`material/preview_surface.rs`), which samples the network per
    hit and writes each result over its `OpenPBR` field before delegating the BSDF, as
    `MtlxMaterial` does. With no texture connection the surface stays the plain
    `OpenPBR` it always was, with no chart and no per-hit cost, and every sample golden
    is unchanged. `preview_uv_input` resolves the whole node through
    `value_producing_attributes`, so interface-connected inputs work: `file` (layer-anchored,
    `<UDIM>` intact), `sourceColorSpace`, `wrapS`/`wrapT`, `scale`/`bias`, `fallback`,
    and which output (`r`/`g`/`b`/`a`/`rgb`) is connected. Four details:
    - **`sourceColorSpace` defaults to `auto`, not raw**, which is the opposite of
      MaterialX's default and why `ColorSpace::from_usd` is separate from
      `from_mtlx`. `ColorSpace::Auto` crosses the seam unresolved, and the host settles it
      against the file (`resolve_auto`). By the UsdUVTexture rule, 8-bit RGB/RGBA is sRGB
      and anything else is raw. So a greyscale roughness PNG stays raw and an EXR is
      linear.
    - **A texture that does not load reads the node's `fallback`, unscaled** (the spec),
      and with none authored the *surface input's* constant, and with none of that the
      input's **`UsdPreviewSurface` schema default** (`preview_surface_default`:
      roughness 0.5, diffuse 0.18, ior 1.5, …) — not the node set's opaque black, which
      is not neutral. So `CRUST_TEX=0` still means "render on constants". ALab authors a deliberate green `fallback`, which
      therefore shows up only when a file genuinely fails.
    - **Wrap modes apply only to a single image.** A `<UDIM>` set's addressing is the
      host's, and wrapping would fold every tile onto the first. `repeat` and
      `useMetadata` are the identity (the host already wraps periodically).
    - **`normal` is decoded by the texture's own `scale = 2, bias = -1`**, which is the node
      set's convention and not a hard-coded `*2-1`, then rotated by
      `crust_mtlx::perturb_normal`, the half of MaterialX's `normalmap` split out for
      this. It is under the same no-flip guard.
    `UsdPrimvarReader_float2` naming a primvar other than `st` (or the `mesh_uvs`
    fallbacks) warns and shades from the one chart crust reads. `UsdTransform2d` warns,
    and the identity chart is used. Textured emission answers through `emitted_at` only,
    like MaterialX's.
  - `crust:openpbr` → decoded 1:1 into `OpenPBR`; every input is the camelCase mirror of the
    Rust field name (lossless but non-portable). Reference scene: `samples/openpbr_showcase.usda`.
  - `PxrDisneyBsdf` → mapped into `OpenPBR` (both descend from Burley's model). Checked
    **before** `compute_surface_source()`, by looking for a child shader with that
    `info:id` — a material with several render-context outputs resolves through that call
    to whichever one USD prefers, and on the Moana island (which authors
    `outputs:ri:surface`, `outputs:glslfx:surface` and `outputs:ri:displacement` on every
    material) that is the *preview* shader, whose inputs are all `.connect`ed to the
    material interface rather than authored as values. Decoding it yields every parameter
    at its default. Parameters are therefore read off the **Material** prim, where the
    island authors them. `sheen` is deliberately **not** mapped to `fuzz_weight`: Disney
    adds sheen at grazing angles, OpenPBR mixes fuzz *over* the layers beneath, so the
    island's `sheen = 1` erased all base colour (Ptex included) and rendered smooth
    plastic. `subsurface*`, `diffuseTransmission` and `specularTint` have no equivalent
    lobe and are dropped.
  - **`.mtlx` reference** → the MaterialX graph, read by crust itself
    (`crust-mtlx` + `material/materialx.rs`, below). Checked at each point the USD path gives up,
    not first: finding the reference means walking the prim's composition
    graph, which a stage of ordinary USD materials should not pay for.
  - Unbound geometry → grey diffuse `OpenPBR`.
- **MaterialX** (`crates/crust-mtlx`, adapter in `material/materialx.rs`) — `.mtlx` look-dev graphs, read directly.
  USD's own answer is a file-format plugin that composes a `.mtlx` into the
  stage as `UsdShade` prims; **openusd ships none**, so a `Material` prim whose
  only opinion is `references = @foo.mtlx@</MaterialX/Materials/name>` composes
  to a prim with a `Material` type name and *nothing inside it*. Every schema
  query then fails and the surface falls back to grey — which is what the two
  DPEL assets (MaterialX Teapot, MaterialX Lion) did on import. The reader is
  the standalone `crust-mtlx` crate — `parse.rs` (XML → a flat,
  name-addressable graph), `value.rs` (the one runtime value), `eval.rs` (the
  graph compiled to a slot-indexed program), `bsdf.rs` (the BSDF tree
  flattened to weighted lobes) — and crust-core's `material/materialx.rs` is
  the adapter: `MtlxMaterial` (impl `Material`), `reduce()` pooling the lobes
  onto OpenPBR, and the importer-facing `load()`.
  - **Compiled once, not walked per hit.** A look-dev graph must be evaluated
    per shading point — its textures and masks are the point — but the teapot's
    ceramic graph is ~50 nodes consulted several times per path vertex (sample,
    then once per NEE and guide evaluation). So the graph is compiled into a
    `Program`: a topologically ordered `Vec<Op>` whose operands are slot
    *indices*. Evaluation is a linear scan with no name hashing and no
    allocation (the value stack is a thread-local scratch buffer). ~30 node
    types are implemented; an unknown one degrades that one input to a constant
    and is reported once per material, never fails the material.
  - **The BSDF reduction is the lossy part, and deliberately so.** MaterialX
    assembles a look from *standalone* BSDF nodes (`oren_nayar_diffuse_bsdf`,
    `dielectric_bsdf`, `conductor_bsdf`, `sheen_bsdf`) glued with `layer` and
    `mix`; crust has one übershader with a fixed lobe stack. At **compile
    time** the tree is flattened — a `mix(fg, bg, m)` sends `m` down one branch
    and `1 − m` down the other, a `layer` sends full weight down both — so each
    leaf arrives with a weight that is a product of mask expressions, compiled
    into the same program. At **shading time** the leaves pool by kind and the
    pools normalise into OpenPBR parameters (diffuse+metal → `base_color` and
    `base_metalness` as their ratio; dielectric+conductor → one joint
    `specular_roughness`; sheen → fuzz). A leaf's own `weight` input multiplies
    its path weight; a branch that is a *literal* zero — either a leaf whose
    `weight` is 0, the "transmission dummy" both assets use as a mix's null
    branch, or a `multiply(BSDF, 0)` — is pruned at flatten time rather than
    carried at weight 0. Both forms have to be pruned, and for the structural
    reason rather than the numeric one: `reduce` drops a zero-weight lobe
    anyway, but the `layer` promotion below counts it as a specular interface
    first. **Two specular lobes.** The flattening
    keeps one structural fact: a dielectric that is the `top` of a `layer`
    whose base already carries a specular (another dielectric, a conductor)
    arrives as `LobeKind::Coat` and pools onto OpenPBR's coat lobe with its own
    roughness and IOR, while a dielectric directly over a diffuse stays the
    base specular (that is how OpenPBR's own dielectric base is built). So a
    varnish over a conductor keeps its varnish — the single pool used to lose
    it, since a metal base zeroes the dielectric Fresnel term — and the DPEL
    **lion**, whose glaze sits over a `mix(conductor, diffuse)`, reduces to
    `coat_weight = 1`. The **teapot** does not: its glaze sits over a plain
    `oren_nayar_diffuse_bsdf`, so both its glazes stay the base specular and it
    reduces to `coat_weight = 0` at every point — correctly, and worth knowing
    before blaming the coat for anything the teapot does. The decision is
    by tree shape, never by evaluated weight (a per-point flip would draw a
    seam along a mask's zero contour), which is why a literal-zero branch has
    to be pruned before the layer looks at its base. What this still cannot
    represent: three or more stacked dielectrics pool their upper ones into one
    coat roughness, and a coat dielectric's `tint` is ignored (MaterialX tints
    the coat's reflection; OpenPBR's `coat_color` is substrate absorption). A
    `conductor_bsdf` fed by `artistic_ior` is reduced back to a reflectivity
    colour through the exact inverse of Gulbrandsen's formula, so a metal
    authored either way lands on the same OpenPBR metal lobe.
  - **Two unit conversions the reduction owes MaterialX**, both easy to
    re-break. First, a MaterialX specular BSDF's `roughness` input is the GGX
    **alpha**, not a perceptual roughness — that is what `roughness_anisotropy`
    exists to produce, and the DPEL teapot's `.mtlx` makes it explicit with
    `power` nodes named `desquare_roughness_*`. crust's OpenPBR roughness is
    perceptual and gets squared again, so `reduce()` takes the square root
    once, *after* pooling (`√` is concave, so pooling in alpha and converting
    at the end is both cheaper and the better stand-in for a GGX mixture). Only
    the GGX lobes convert: `oren_nayar_diffuse_bsdf`'s roughness is an
    Oren-Nayar sigma and `sheen_bsdf`'s drives the Charlie NDF directly.
    Second, a MaterialX coat arrives with `coat_darkening = 0`, set on the base
    in `load()`: MaterialX's `layer` is single-scattering, so imposing
    OpenPBR's coat-underside TIR bounce series would darken a substrate the
    source material never darkened (on the lion, by a factor of 0.52).
    Relatedly, the dielectric and conductor pools carry **independent
    coverage**, and keeping them independent takes both halves of the seam.
    `base_metalness` is the conductor's share of the substrate and
    `specular_weight` is the dielectric interface's share of the *dielectric
    base* (not of the whole surface), so `eval_specular` reconstructs the two as
    `base_metalness` and `(1 − base_metalness)·specular_weight` and each comes
    back as authored. The metal lobe therefore does **not** read
    `specular_weight` — that parameter belongs to the dielectric base, and a
    metal has no dielectric interface to weigh. While it scaled both halves, one
    pool's coverage multiplied the other: a conductor at 0.25 under a glaze at
    0.75 rendered its metal at 0.25 × 0.75. Two consequences worth knowing: the
    dielectric base's coverage is `max(diffuse + sss, dielectric)` rather than a
    sum, because MaterialX's `layer` puts an interface *on top of* a substrate
    rather than beside it — and a conductor mixed with a *bare* dielectric used
    to pin `base_metalness` to 1 and lose the dielectric outright. A graph with
    no base `dielectric_bsdf` at all — the lion, whose glazes are both coats, and
    the teapot's metal — now reduces to `specular_weight = 0`, which is correct:
    it has no base specular, and it used to be given one.
  - **The EDF half is a second list, not a seventh lobe kind.** MaterialX's
    `<surface>` has an `edf` input beside its `bsdf`, and the flatten now walks
    both — the same `layer`/`mix`/`multiply`/`add` algebra, since a mix
    partitions radiance exactly as it partitions reflectance. Three things are
    load-bearing. The walk carries a **closure domain**, because
    `closure_input` gates a branch on its declared type and an EDF-typed `mix`
    declares `type="EDF"`: asking "is this a BSDF?" in the emission tree
    resolves both branches to `None` and the emission vanishes silently. The
    terms land in `Flattened::emission` rather than as a `LobeKind`, which
    makes the `layer` arm's `base_has_specular` scan *structurally* unable to
    see an emitter — the strongest form of "emission does not disturb the coat
    promotion" — and keeps the two algebras apart: BSDF pools take a weighted
    **mean** (two diffuse leaves are one surface shared between them) while
    emission **sums** (two emitters are twice the light). And neither the
    weight nor the colour is clamped above: `multiply(uniform_edf, 100)` is how
    a document authors a bright emitter, and this is the one shading input for
    which a value above 1.0 is meaningful rather than an authoring error.
    **The weight is not a scalar**, and reading it as one is a quiet, severe
    bug: MaterialX declares `ND_multiply_edfC`, a `multiply` on an EDF by a
    `color3`, so a tinted emitter is ordinary authoring and `Mul` promotes arity
    into the weight slot per channel. Taking lane 0 alone turned a weight of
    `(0, 0.6, 0.9)` into a **black** emitter and `(1, 0.5, 0.2)` into a neutral
    one at full strength. `reduce()` reads both factors with `Val::rgb`, which
    broadcasts an arity-1 value so the `float` case is unchanged, and sanitises
    each factor *before* the product — clamping the product instead would let
    two negative channels multiply into positive light. Non-finite is refused
    per channel here where the lobe loop drops the whole lobe, and that
    asymmetry is structural: emission sums, so a zeroed channel contaminates
    nothing, while a lobe's weight is a *divisor* (`Pool::w` normalises every
    colour in its pool). The **BSDF** lobe weight still reads lane 0, and
    deliberately: `ND_multiply_bsdfC` exists too, but every OpenPBR coverage
    field it feeds (`base_weight`, `specular_weight`, `coat_weight`,
    `fuzz_weight`, `subsurface_weight`) is an `f32`, so a colour there has
    nowhere to go short of folding the tint into the lobe's own colour.
    `reduce()` factors the summed radiance by its **peak channel**, so
    `emission_color` is always a chromaticity in `[0,1]³` and the range lives
    in `emission_luminance`; factoring by Rec.709 luminance instead — what
    OpenPBR's spec means by nits — would send a saturated emitter's *colour*
    above one, since (0, 0, 8) has luminance 0.43 and would store (0, 0, 18.6).
    A `surface` with an `edf` and no `bsdf` also has its `base_weight` zeroed,
    or a pure emitter would keep OpenPBR's default grey diffuse underneath it.
  - **The shipped `.mtlx` files are not well-formed XML.** They address a UDIM
    set as `value="Albedo.<UDIM>.png"` — a bare `<` inside an attribute value,
    which XML forbids. MaterialX's own reader is PugiXML, which accepts it;
    `roxmltree` rejects the whole document with an `InvalidChar`. `parse.rs`
    escapes the two specified tokens first, so this is not "the UDIM path is
    wrong" but "no material at all" if it is ever removed.
  - **Verified in numbers, not by eye** — `examples/mtlx_shade` prints the
    OpenPBR parameters a graph reduces to at a named `(u, v)`. This is what
    settled the teapot: the render looked washed out against the reference, and
    the probe showed the graph producing exactly the right deep blue
    (0.005, 0.024, 0.074) on the body against light ribs — so the fault was the
    sample scene's exposure, not the material. A mis-decoded albedo is a
    plausible pastel and a mask read at the wrong colour space is a plausible
    blend; comparing renders settles nothing. It prints an `emission` column
    too — the *product* `emission_color * emission_luminance`, since OpenPBR
    only ever multiplies the two back together and either field alone would
    mislead — and it reaches its textures through `FileAssets` rather than
    `UvTexture` directly, so `CRUST_TEX_STREAM` is honoured. That last part is
    not tidying: the preloaded decoder narrows to 8 bits, so an HDR emission
    texture probed through it reads 1.0 whatever the file holds, and a probe
    that cannot see the range is worse than no probe because it answers
    confidently.
  - Sample scenes: `samples/materialx_basic.usda` + `.mtlx` (self-contained, 20
    KiB of textures, what `tests/usd_scene.rs` runs against; its `mtlx_lacquer`
    is the two-dielectric stack that must reduce to base specular + coat),
    `samples/materialx_emissive.usda` + `.mtlx` (the EDF fixture: a constant
    `multiply(uniform_edf, 12)` and a pure emitter driven by
    `textures/mtlx_emission.hdr`, whose bright cells sit at (16, 9, 3) —
    deliberately a **separate** document, because three assertions pin
    `materialx_basic` at exactly three materials and three textures), and the two shot
    layers for the DPEL assets, which are gitignored and must be downloaded:
    `samples/materialx_teapot.usda` and `samples/materialx_lion.usda`, plus
    `samples/materialx_showcase.usda` composing both after the `overview.png`
    the assets ship with (the lion is scaled to 0.52 there: both are ~0.26 m
    tall as authored, yet the overview shows the lion at ~60% of the teapot's
    height while standing nearer the camera, so it is scaled, not pushed back;
    the seamless sweep is a near-white floor under a uniform dome, the two
    meeting at the horizon because a Lambertian floor of albedo a under
    radiance L reflects a·L). The lion
    is the larger graph (140 ops, 8 lobes, 7 textures over 6 UDIM tiles, 1.06 M
    baked triangles) and the one that layers a `sheen_bsdf`, so it is what
    exercises the fuzz pool; both import with no unsupported nodes.
- **UV textures** (`texture.rs`, host decoder in `crust-assets/src/uv_texture.rs`) —
  the chart a `primvars:st` primvar carries, as opposed to Ptex's per-face
  parameterisation. `Texture2D` is `crust_mtlx::Texture` re-exported — the
  standalone reader has to name the sampler it consumes and crust-core adopts
  that name, the same way it adopts `crust_rt::Geometry`; a separate crust-core
  trait plus adapter would put a second vtable hop on every texel fetch that fat
  LTO cannot remove. It is shaped like `PtexTexture` and for the same
  reason: a production UDIM set is fourteen 4K images per map, so the host
  decodes and hands back a **sampler**, and `Texture2D::eval` takes
  **unwrapped** coordinates — `u = 3.4` is the fourth UDIM tile, not `0.4` of
  the first, and tile selection is the host's addressing job. Both of
  MaterialX's tile tokens are expanded (`TileToken`): `<UDIM>` → `1001 + u +
  10·v`, `<UVTILE>` → `u<u+1>_v<v+1>`. They name the same 10x10 grid, so tiles
  are keyed by UDIM number whichever token named the file. `AssetLoader::load_texture`
  carries the colour space with the path, because the file does not say: an
  8-bit PNG holding albedo is display-encoded while the same encoding holding a
  normal, a roughness or a mask is raw data. MaterialX states it per input
  (`colorspace="srgb_texture"`), and **anything else, including an absent
  attribute, means raw**.
  - `UvMap` (`rt_world.rs`) carries per-triangle **corner** UVs, not per-vertex:
    USD's `st` is usually `faceVarying`, and a vertex on a UV seam has one
    position but two texture coordinates. Built only when the bound material
    reports `uses_uv()` — 36 bytes a triangle that an untextured production
    stage should not pay.
  - **Tangents exist only on baked geometry.** A tangent frame is world-space,
    so it can only be built once a placement is known — and a prototype shared
    by N instances has N transforms against one table. Instanced meshes
    therefore carry UVs (which no transform touches) and no tangent, and normal
    maps on them fall back to the geometric normal.
  - **The resolution cap is not an optimisation.** Fourteen 4096² tiles is 674
    MiB for one map, and the teapot's ceramic binds four across two materials.
    `CRUST_TEX_MAX` (default 1024) box-filters each tile down at load; tiles are
    kept as `u8` and converted through a 256-entry table on lookup, since the
    files are 8-bit PNGs and nothing recovers precision that was never there.
    **An `.exr` is the exception: it preloads as linear `f32`** (12 B/texel,
    through `read_exr_rgb`, the workspace's one EXR reader), because a linear
    albedo quantised to 8 bits bands in the shadows and anything above 1.0
    would clip — and ALab's thousands of UDIM EXRs otherwise loaded as nothing
    (`image` has no EXR decoder). The storage is chosen once per `eval` by a
    `match` on the texture, then `sample_tile::<T: Texel>` is monomorphised, so
    the `u8` path is unchanged to the instruction (`sample_level::<u8>`
    279,025,964 before and after on `materialx_basic -s 2`; the match costs
    `eval` +4.7%, whole-render +0.15%). An explicit curve on an EXR is applied
    once, at load. `.hdr` still takes the `u8` path, which keeps the
    streamed-versus-preloaded emission A/B below as documented.
  - **Each tile carries a mip pyramid** below that cap, read trilinearly at the
    hit's footprint (see "Texture filtering" below). Two details that are easy
    to re-break. Levels average in **linear light** and re-encode through the
    colour space's inverse curve, because averaging display-encoded bytes is
    not averaging light: a black/white checker comes out at 0.21 linear instead
    of 0.5, and the chain drifts darker at every level. The `CRUST_TEX_MAX`
    reduction deliberately does *not* — it averages in the file's own encoding,
    to keep a capped tile matching a DCC's preview of the same file — so the
    two conventions differ on purpose; `docs/color_management.md` records both.
    And axes halve by **`div_ceil`**, not `>> 1`: `decode_tile` reduces by an
    arbitrary integer factor, so an odd level 0 is routine (3000×2000 under a
    1024 cap is 1000×666), and the lookup maps `x = u·width − 0.5`, so flooring
    an odd axis drops its last half-texel and that level's domain slips against
    level 0's — visible as a crawl across mip transitions on a slow camera move.
    **An odd axis is then a resample, not a 2×2 box**, and `axis_taps` weights
    it by area: the lookup reads each texel as an equal-width slice of the
    whole tile, so a destination texel is the average of the source over
    exactly its own `src/dst ≤ 2` texels (at most three of them). Clamping the
    source index instead — reading the trailing texel twice and averaging it
    as though two were there — hands that column a third of the level's weight
    where it is owed a fifth, at *every* level: a 5-wide row of
    `[250, 200, 150, 100, 50]` came out `[225, 125, 50]`, mean 133 against the
    source's 150, and a 25-wide tile lit only at its right edge bottomed out
    **6× too bright** while the same tile lit at its *left* edge came out too
    dark — an 8× disagreement decided by nothing but which end the clamp was
    at. On an **even** axis every overlap is exactly 1.0 and the divisor
    exactly 4.0, so the reduction is bit-identical to what it was; every
    checked-in texture is 64×64, so no sample golden moves and the
    streamed-versus-preloaded invariant is untouched. Both halves of that hold
    only because `reduce_half` and `reduce_half_linear` share `axis_taps` —
    they back the TIFF and EXR `.tx` writers, and one fixed without the other
    would leave two internally consistent chains that disagree. A `.tx`
    written by an older build still carries old-filter odd levels; they are
    gitignored artefacts `maketx` regenerates, so this is a note rather than a
    migration.
- **Streaming textures** (`crust-assets/src/tiled/`) — the residency half of the
  texture problem, as opposed to the filtering half above. Used whenever a `.tx`
  stands beside the texture (see the environment overrides above, and
  `--auto-tx`); the preloaded `UvTexture` remains the correctness oracle and the
  path for any texture without one.
  - **Why.** Preloading makes memory scale with the scene's total texture
    footprint, which is the only reason `CRUST_TEX_MAX` exists — and a cap is a
    poor residency policy, because it discards authored detail permanently and
    still cannot help a scene that binds more than fits. Production renderers
    convert once, offline, to a tiled mip-mapped file and stream tiles behind a
    bounded cache, so memory scales with the *cache* instead.
  - **Two backings, one seam.** A `.tx` is either a tiled mip **TIFF** with
    `u8` tiles or a tiled mip **EXR** with `half` ones, and `TiledFile` picks
    between them by **magic number** — which is what makes `maketx --format exr
    -o foo.tx`, an EXR inside a file named `.tx`, simply work. The split is by
    *sample type*, not container: unsigned integer samples (8- and 16-bit TIFF)
    page in as `TileKind::U8` exactly as before, float samples as
    `TileKind::Half`. An 8-bit texture pays nothing for HDR existing — its
    tiles, its bytes and its bit-identical agreement with the preload path are
    untouched — and an HDR one is not silently clipped to fit an 8-bit cache.
    A tile's payload is bytes plus that kind rather than an enum over two
    buffers, and the sampler reads it through `Tile::rgb_u8` or `Tile::rgb_half`
    chosen by a const generic — for the measured reason two bullets down.
  - **Why EXR and not float TIFF.** TIFF can hold `f32`, but the format is the
    smaller half of the question and the industry already answered it: OIIO's
    `maketx --format exr` writes 64x64-tiled, full-MIPMAP, zipped `half` with
    `textureformat`/`wrapmodes` as first-class header attributes (better
    provenance than TIFF, which has to smuggle them through `ImageDescription`),
    V-Ray's native streaming texture format *is* tiled mip EXR, and Karma
    recommends `.exr` or `.rat`. Adding EXR is the opposite of diverging from
    OIIO; a TIFF-only path was the narrower one. `half` rather than `f32`
    because it is what every streaming texture format stores and what keeps an
    HDR tile at twice a `u8` tile rather than four times — a texture is shading
    input, not a render target.
  - **The EXR reader is hand-rolled around the block API; the writer is not.**
    `exr` writes tiles and mip levels through its ordinary public API
    (`Blocks::Tiles` + `Levels::Mip`), so unlike TIFF there is no container
    code at all — `exr_write.rs` is the pyramid, the de-interleave to planar,
    and the attributes. Reading is the sharp part: `filter_chunks`, the one
    entry point that looks like random access, **consumes** the reader and
    sorts the offsets, so `exr_read.rs` composes the layer beneath it —
    `MetaData::read_from_buffered` → offset table → seek → `Chunk::read` →
    `UncompressedBlock::decompress_chunk` → `lines()`. Three details worth
    keeping: `enumerate_ordered_header_block_indices()` supplies the
    `(level, tile) → chunk index` map (EXR mandates no chunk order, so it must
    not be assumed row-major); `decompress_chunk` returns **native**-endian
    samples, so reading them is a reinterpret; and the level sizes come from
    the header's own `RoundingMode` rather than from `div_ceil`, because
    `maketx` writes `ROUND_DOWN` while crust writes `ROUND_UP` to match
    `reduce_half`.
  - **The offset table is probed, not trusted.** `MetaData` is read through a
    `PeekRead` that may hold one byte it has consumed and not handed back, so
    the reader's position afterwards is either the table's start or one past
    it. The table is self-describing — chunks begin immediately after it, so
    its smallest entry equals its own end — and that identity picks between the
    two candidates. The current `exr` happens to land exactly right, so the
    fallback is forced by a test (`the_offset_table_is_found_even_when_the_
    reader_is_a_byte_late`) rather than left to rot.
  - **`.tx` is a plain TIFF.** Tiled 64x64, mip levels as chained IFDs,
    Deflate. `tiff` 0.11.3 reads one tile at one level with a real seek
    (`seek_to_image` + `read_chunk`, which walks the cached `TileOffsets`
    table); it **cannot write** tiled, so `write.rs` supplies the tile grid, the
    per-tile zlib stream and the tile tags while borrowing `DirectoryEncoder`
    for the header, IFD chaining and entry serialisation. Three upstream
    hazards are designed around: tiled **LZW** fails to decode (#395) so only
    Deflate is ever written; `PlanarConfiguration = 2` **panics** inside
    `expand_chunk` (#403) so planar files are refused at open rather than
    allowed to abort a worker; and the right-edge fix (#400) is in the
    `read_image` assembly path, which is why only `read_chunk` is used.
  - **A tile is written padded and read back clipped.** TIFF6 says a tile is
    always `TileWidth x TileLength`, but `tiff` returns `chunk_data_dimensions`,
    so an edge tile is narrower. Indexing it by the nominal edge reads 64 texels
    of stride into a 22-texel row and shears the right-hand column of every
    texture whose size is not a multiple of 64.
  - **The cache is OIIO's algorithm, not an LRU.** `check_max_mem` there is a
    clock hand giving each entry one second chance, `try_lock`ed so a thread
    that finds a sweep in progress carries on rather than queueing behind it —
    the budget is a target, not an invariant, and is briefly exceeded by
    whatever other threads insert while one sweeps. That is a few hundred lines
    of `std::sync`, which is why there is no `moka`/`quick_cache` dependency:
    both carry internal `unsafe`, and the whole workspace is now
    `forbid(unsafe_code)` (`crust-core` is `deny`, for one test-only
    `GlobalAlloc`).
  - **Three tiers, and the top one does the work.** A per-thread two-entry
    microcache, then 64 sharded maps, then a decode. Measured on the alias
    scene: **98.6% of 8.7 M lookups never reach a lock**, because a bilinear tap
    reads one tile four times and trilinear alternates between two levels —
    which is why there are two slots and not one. It also means the lookup path
    itself, not contention, is what to optimise: `with_tile` hands the tile to a
    closure rather than returning an `Arc`, because one refcount pair per texel
    was the difference between streaming costing 4x a preloaded render and
    costing 2x.
  - **The second backing must cost the first one nothing, and twice it did
    not.** `bench_ab` against the pre-EXR binary on the 8-UDIM alias scene said
    **+21%** on an 8-bit streamed render — a path that gains nothing from HDR
    existing. Callgrind found both causes and the fixes are load-bearing, not
    tidying. First, a `TileData` **enum** read per texel: matching it inside the
    `with_tile` closure grew that closure past what LLVM would inline, so
    `texel` went 414.9 M → 471.5 M instructions *and* grew a 216.7 M
    out-of-line `texel::{closure#0}` that had not existed. The payload is
    therefore bytes plus a `TileKind`, and the sampler is monomorphised over a
    `const HALF: bool` decided once per `eval` from the file's own kind — the
    information is per *texture*, so it does not belong in a per-texel branch.
    Second, and larger, the `dyn Backend` facade itself: `texel` asks for
    `tile_edge()` and `level()`, and routing those through a vtable is an
    indirect call in the hottest loop in a textured render. `TiledFile` now
    **copies** the geometry out of the backing at open and touches `inner` only
    to read a tile. Together: `texel::<false>` is 414,851,273 instructions,
    equal to the pre-EXR binary's to the instruction, whole-render instructions
    are +0.017%, and wall clock lands at −5.4% min / −6.1% mean (i.e. noise).
  - **The colour space is recorded in the file** (`crust:mipspace=`, in
    ImageDescription for TIFF and as a header attribute for EXR) and a mismatch
    is refused. It means "the space this file is to be bound with", and the two
    backings reach that from opposite directions. A TIFF `.tx` stores
    display-encoded texels and reduces its levels in *linear light*, so the
    space is baked into every level above 0: read an sRGB chain as raw and
    level 0 is perfectly correct while every coarser level is wrong — visible
    only under minification and, by eye, indistinguishable from a filtering
    bug. An EXR `.tx` stores **linear** texels, decoded once at conversion
    because EXR has no transfer curve of its own, so binding it under another
    space would apply a curve to data that has already had one removed. A file
    with no marker (anything `maketx` wrote) is accepted, since its chain came
    from OIIO's filter and there is nothing to match against.
  - **Which backing a conversion produces is decided by the source's range**,
    not its extension: `maketx` writes EXR for a source that actually carries
    values above 1.0 and TIFF otherwise, so a `.hdr` of an overcast sky does
    not pay double for a range it never uses. `--format tiff|exr` overrides
    either way, and a TIFF conversion that clips is warned about rather than
    done quietly.
  - **The invariant.** For any texture at or below the preload cap, streamed
    and preloaded renders must be **bit-identical** — same level 0, same
    `reduce_half`, same level selection. `samples/materialx_basic` at 16 spp:
    0 of 230 400 pixels differ. Measured on 8 UDIM tiles of 2048² (96 MiB
    authored), 640x360 at 4 spp: preload capped 54.16 MiB / 0.223s with detail
    discarded, preload uncapped 142.46 MiB / 0.231s, **streamed at a 16 MiB
    budget 15.94 MiB / 0.449s and bit-identical to the uncapped preload**. The
    ~2x render cost is the honest worst case — one textured plane at depth 2,
    so nearly every shading call is a fetch. That invariant is about the `u8`
    path and stays exactly as it was; the `half` path is where the two paths
    are *supposed* to disagree. Measured at the seam rather than in a render: a
    Radiance source whose white checks are at 8.0 comes back at 8.0 streamed
    and at exactly 1.0 preloaded (`to_rgb8()` clips it), while below 1.0 the
    two agree to within the 8 bits the preload path keeps — so the divergence
    is the range and not a different lookup.
  - **Conversion is explicit, or opt-in automatic.** `examples/maketx` converts by
    hand, and `--auto-tx` converts on first use (Arnold's `autotx`). Both run one
    conversion, `crust_assets::tiled::make_tx`. Automatic conversion stays behind a flag
    because a renderer that silently writes multi-gigabyte files next to a read-only
    asset library is a surprise nobody asked for.
  - **The microcache is keyed by cache as well as tile.** A `TileId`'s file index is
    unique only within one `TileCache`, while the per-thread microcache outlives any one
    cache. So a second `FileAssets` in one process (tests, a host rendering twice) was
    handed the first one's tiles on the same thread; the `clear_microcache()` calls in
    the cache tests were working around exactly that. Each cache now carries a
    process-unique `id` in the key. Measured free: `texel::<false>` is 446,589,173
    instructions before and after.
- **Ptex** (`texture.rs`, plus the decoder in `crust-assets/src/ptex_texture.rs`) — per-face colour textures via
  the pure-Rust [`ptex-rs`](https://github.com/doubleailes/ptex-rs) reader, driving
  `OpenPBR::base_color`. A material's `inputs:surfaceMap` asset is the hook (both of the
  island's Ptex shader paths — `PxrPtexture.filename` and `HwPtexTexture_1.file` —
  `.connect` to it, so no network walk is needed). Asset paths come from openusd's
  `resolved_path()`, which anchors against the *authoring* layer — essential here, since a
  production stage's `../../../textures/foo.ptx` is authored several directories below the
  root layer.
  - **crust-core still decodes nothing**, but the `AssetLoader` seam *inverts* for Ptex:
    an environment map crosses it as a decoded pixel buffer, whereas a `.ptx` — a per-face
    mip pyramid that can reach gigabytes — crosses it as a **sampler**
    (`load_ptex → Arc<dyn PtexTexture>`) the host owns. Defaulted to `None`, so existing
    hosts are unaffected.
  - **Face ids are mesh face indices**, so `triangulate` records per emitted triangle its
    source face plus which slice of that face's fan it is (`FanSlice`), and `World`
    resolves a hit's barycentrics into `(face_id, u, v)` — Ptex parameterises a quad
    `v0=(0,0) v1=(1,0) v2=(1,1) v3=(0,1)`, so the lower fan half gives `(u+v, v)` and the
    upper `(u, u+v)`. The table lives on `World` keyed by `geom_id`, **not** on `MeshGeom`,
    which is dropped the moment a mesh is baked or committed. A skipped face must still
    consume its face id or everything after it shades from the wrong texel
    (`face_table_tests`). `bake_indices`' mirror swap exchanges `u` and `v`, so the table
    carries that flag per placement. Built only when the material reports a
    `face_texture()`, so an untextured stage allocates nothing.
  - The table is carried on **both** geometry paths. A direct mesh gets it in
    `flush_meshes`; a prototype carries it on its `ProtoPart` and
    `attach_proto_parts` records it against the instance's `geom_id`. Wiring only
    the direct path is not enough and is not obviously broken either: the island's
    geometry is almost entirely prototype-based, so Ptex silently applied to none
    of it and every textured surface fell back to its constant `baseColor` — which
    for a `PtexBaseMaterial` is an unused placeholder, so the gardenias rendered
    flat red. A part is always exactly one leaf geometry (the walk splits per bound
    mesh; a nested instancer groups per (prototype, part)), so one table serves
    every placement and the `prim_id` a hit reports indexes it unambiguously
    however many instance levels it passed through.
  - The host **preloads every face** into one immutable buffer by default (the streaming
    alternative is the next bullet): `PtexReader` reads from
    disk on each call (`&mut self`, pixel data uncached), and a path tracer asks from every
    thread in an unpredictable order. Faces load **mip-reduced**, capped at 32×32 by
    default (`CRUST_PTEX_MAX_LOG2` overrides as a log2 edge length) — full resolution is
    authored for close-ups, so `isLavaRocks`' 631 MB / 11 384-face colour file costs
    130 MiB instead of several GB, at a resolution far past what a 595×520 framing
    resolves. **Beneath that cap each face carries a full mip pyramid** down to 1×1,
    selected per hit by the ray cone's footprint (see "Texture filtering" below). The two
    answer different questions and it is worth keeping them apart: the cap is the
    *ceiling* on detail, the pyramid is what makes minification below it correct. The cap
    used to double as an accidental anti-aliaser, and now that it does not have to, it can
    come **down**: the island is recorded below at 1.84 GiB with a 16×16 base against 4.58
    GiB flat at 32×32, so 16×16 plus a pyramid is around 2.45 GiB — derived from that
    figure rather than re-measured — for under half the memory and better filtering at
    distance. `examples/tex_probe`'s budget table counts the pyramid, so that comparison
    can be made against a real asset directly. Levels are reduced **in memory from the
    decoded linear base**, not by asking the reader for each resolution: every extra read
    takes `&mut self` through the serial load loop (another seek and inflate) and comes
    back display-encoded, needing the `powf(2.2)` again — and averaging in that encoding
    is not averaging light, which is the whole reason the pyramid is built here.
    **Which texels get averaged is the file's business, though, not ours**: a
    `meshtype = triangle` Ptex packs *two* triangles into each square of texels, the
    upright one and its mirror across the anti-diagonal, so Ptex reduces three texels of
    the upright 2x2 with the one mirrored texel that completes it
    (`w-1-2u`, `w-1-2v` — note the index swap) rather than with the neighbour
    below-right. `PtexColor` reads `mesh_type()` once at open and picks `reduce_triangle`
    or `reduce_quad` accordingly; a 2x2 box over a triangle face mixes texels from both
    triangles and is wrong at every level above 0 by up to ~65% while still looking like
    plausible texture, which is why `triangle_levels_match_ptex_rs_reduction` compares
    against `ptex::utils::reduce_tri` rather than against an expectation written by
    hand, and why a second test pins that the two reductions really do disagree.
    Triangle faces also clamp both axes together, since the format defines only
    symmetric reductions for them. A level's offset is walked rather
    than stored — `Face` gains one `u8` in its existing padding, which over 2.5 M faces is
    the difference between free and a per-face offset array. The `+1/3` figure holds for
    square faces only: once a non-square face's short axis pins at one texel the chain
    halves rather than quarters, so 64×16 lands at 1.335×. Texels are decoded to linear once at load (the island's graph gammas raw
    Ptex, and `HwPtexTexture_1` declares `sourceColorSpace = "sRGB"`; treating the data as
    already linear overshoots albedo ~4×, which `examples/tex_probe` exists to settle).
    `docs/color_management.md` is the per-input inventory of which colour space every
    input is assumed to be in and what curve is applied — including the two gaps that
    are still open (`UsdPreviewSurface` colours are read undecoded, and nothing enforces
    that a new colour input states its space at all).
  - **Streaming** (`texture.rs`'s seam again, host side in `crust-assets/src/ptex_stream.rs`)
    — the residency alternative to that preload, opt-in via `CRUST_PTEX_STREAM=1` under
    a `CRUST_PTEX_CACHE_MB` byte budget (default 1024). `PtexStream` pages one tile of one
    level of one face through `ptex::SharedReader`, so memory scales with the cache
    instead of with the asset and **the resolution cap stops being load-bearing**: an
    unset `CRUST_PTEX_MAX_LOG2` means uncapped. Unlike the UV path there is no conversion
    step, because a `.ptx` is *already* a tiled per-face mip pyramid — the missing piece
    was a cache, and it is the reader's, not crust's. Preloading remains the default, the
    oracle, and the fallback for a file that will not open. Four things worth keeping:
    the base level is **bit-identical** to the preloaded one (0 of 57 600 pixels differ on
    `samples/ptex_quads.usda` at 16 spp with both capped alike, and texel-for-texel across
    four fixtures and every cap in `tests/ptex_stream.rs`); the **coarser levels are not**,
    since a streamed level is reduced on disk in the file's encoding while a preloaded one
    is reduced in linear light, which convexity makes the streamed chain the darker of by
    up to 0.147 — **so a texture that could read one is declined by default and preloaded**
    (`MipSpace`, `CRUST_PTEX_STREAM_MIPSPACE`, below); the microcache keeps **four** slots
    rather than the `.tx` cache's two,
    because a `.ptx` grids per *face* so a four-tile-corner tap is routine and two slots
    measured 0.000 hit rate there against 0.998 with four; and the budget moves residency
    only — a 4 MiB render is bit-identical to a 1 GiB one. `docs/ptex_streaming.md` has
    the measurements and the reasoning.
    **The mip chain is refused, not documented, and that is the project's own standard
    rather than a new rule.** `crust:mipspace` refuses a `.tx` whose levels were reduced
    in the wrong colour space, because the failure is invisible — level 0 is perfectly
    correct and only minification is wrong, which by eye is a filtering bug and nothing
    else. A `.ptx` has no marker and needs none: crust decodes Ptex by 2.2 and the file
    reduced before that, so the mismatch is unconditional. `MipSpace::Linear` (the
    default) therefore declines such a texture at admission and preloads it, reported as
    its own `backend` reason. The gate asks about the *texture*
    (`PtexStream::chain_is_exact`), not the switch, so the two configurations with no
    chain to get wrong still stream: `CRUST_PTEX_MIP=0` (exact *and* uncapped — the base
    level is the bit-identical one — at the cost of anti-aliasing), and a texture whose
    every face is one texel under the cap. `CRUST_PTEX_STREAM_MIPSPACE=file` is the
    opt-in that takes the file's chain instead; it is what the C++ `PtexCache` does and
    what **every measurement in `docs/ptex_streaming.md` was taken with**, the island's
    included — so with a pyramid on, `CRUST_PTEX_STREAM=1` by itself now streams nothing.
    That is deliberate, and the lever is one variable. The fix that would retire both
    gates is a reader that reduces in a declared working space: building the linear chain
    here would need a second pyramid cache (the design "Known incomplete work" ruled out)
    *and* a level-0 read to answer a coarse lookup, which defeats streaming exactly where
    the island uses it.
    **`CRUST_PTEX_CACHE_MB` is the render's budget, not a file's**, and that takes work
    here: `ptex::SharedReader` owns its cache (right for a library, wrong for a scene),
    so N textures opened at the full budget would hold N times it — on a stage binding
    Ptex per element, like the island, the default 1 GiB would become tens of GiB and
    the feature would be unbounded in the texture count. `FileAssets::rebudget_ptex`
    divides one budget over the streamed textures as they arrive, **exactly** — with no
    floor under the share, because a floor is what breaks the bound: `max(budget / n,
    1 MiB)` hands out 39 MiB against a budget of 8, and overshoots *more* the smaller
    the budget gets. `MIN_PTEX_SHARE` (1 MiB) is read as a capacity instead — at most
    `budget / MIN_PTEX_SHARE` readers may stream and anything past that preloads
    (`budget_full`, which `--stats` names with a "raise CRUST_PTEX_CACHE_MB" hint) — so
    every admitted reader gets a usable share *and* `n * (budget / n) <= budget` holds
    by construction. It costs the default path nothing (1 GiB seats 1 024 readers, the
    island wants 39) and engages only when the budget is genuinely small, which is when
    honouring it matters. The `.tx` path gets all of this for free — every streaming
    texture there shares one `TileCache`.
    **The microcache is inside that budget too.** Its slots hold decoded `PixelData` the
    reader cannot count, so a slot has a 256 KiB ceiling (`MICRO_SLOT_MAX`, above any
    real tile and below the whole-face reads upstream refuses as `oversized`), the
    allowance `threads * MICRO_SLOTS * MICRO_SLOT_MAX` — capped at half the budget — is
    subtracted before the readers divide the rest, and `--stats` prints
    `thread tiles / reserve`. Without the ceiling a refused tile was retained anyway,
    four slots deep on every worker thread and invisible to both the budget and the
    report. At a 1 MiB budget a slot is 32 KiB against a 512 KiB face read, so retention
    is zero and every tap goes to the reader — slower, still correct.
  - **`--stats` prints a `Ptex` block**, and unlike the `Texture Cache` one it reports
    for *both* backends, because the first question it has to answer is which ran:
    `backend`, textures and faces, preloaded resident bytes, and for a streamed run the
    live resident/budget total, the three fetch tiers and evictions. Without it an
    island run's peak RSS is uninterpretable — the figure is dominated by geometry and
    the SBVH build transient, with Ptex residency buried inside it.
  - `CRUST_PTEX=0` declines every texture so the same scene renders on its constant
    `baseColor` — the A/B switch that separates a wrong Ptex lookup from a wrong material
    or wrong lighting. It applies to both backends.
  - **A `crust:openpbr` material cannot bind Ptex**, and nothing warns. `inputs:surfaceMap`
    is consulted only for `UsdPreviewSurface` and `PxrDisneyBsdf` — the two the island
    authors — because `decode_crust_openpbr` reads its shader's inputs 1:1 and never looks
    at the material's interface. A Ptex material authored the native way therefore renders
    on its constant `baseColor`, which is indistinguishable from `CRUST_PTEX=0`.
    `samples/ptex_quads.usda` uses `UsdPreviewSurface` for that reason.
  - **Verified numerically, not by eye** — a wrong face id or a transposed `(u,v)` still
    renders as plausible rock, so appearance proves nothing and the reference image
    (different camera, lighting, displacement and subdivision) proves less. Two
    render-free checks, both passing on the island:
    `ptex_verify` exploits the fact that each island `.ptx` embeds the base cage it was
    baked against (`PtexFaceVertCounts` / `PtexFaceVertIndices` / `PtexVertPositions`), so
    the texture can be asked which vertices *its* face N has and the answer compared with
    the mesh's face N. All 10 isLavaRocks meshes and isMountainA pass with face-vertex
    index sequences equal **in order** (45 536 and 134 012 indices respectively) — which
    pins the face correspondence *and* the corner ordering that fixes the UV orientation.
    `ptex_seams` then tests the quad convention itself against the file's `adjface` /
    `adjedge` data: mean texel difference across shared edges is 1.5–16x lower under
    `v0=(0,0)` than transposed, and 1.9–97x lower than between unrelated faces.
    Alongside those, 10 textures / 28 816 faces against 57 632 triangles (exactly 2 per
    quad). The importer warns when a texture's `numFaces` disagrees with its mesh. Not reproduced: no
    displacement (`inputs:displacementMap` is unread), no subdivision *on the island*
    (subdivision is opt-in via `crust:subdivisionLevel`, which the island does not
    author, so its `catmullClark` base cages render as before — Ptex is indifferent
    either way, since face ids index cage faces and subdivided face tables map back to
    them), and the reference's `islandsunEnv.tex` environment is a
    RenderMan-only format.
- **Texture filtering** (`ray.rs`'s `RayCone`, `camera.rs`, `rt_world.rs`, the two
  decoders) — how a texture lookup learns how much texture a pixel covers, which is
  what a mip level is chosen from. Both texture paths sampled a single resolution
  before this; minification was suppressed only by accident, because the memory caps
  threw away the high frequencies first.
  - **The footprint is a ray cone**, not ray differentials: a path tracer spawns one
    ray at a time, so the four extra rays differentials want have nowhere to come
    from, while two floats ride along free. `RayCone { width, spread }` is the
    footprint's **diameter perpendicular to the ray** and its growth per world unit.
  - **Primary rays** get `spread = Camera::pixel_span / |direction()|`. `get_ray`'s
    direction lands on the focus plane at ray parameter 1, so one pixel of `s` moves
    that point by `horizontal / res_w` — and dividing by the direction's length makes
    `focus_dist` cancel, leaving `2·tan(vfov/2)/res_h` down the frame's axis. Pinned
    by a test, because a wrong derivation here still looks plausible. The per-pixel
    `1/|direction()|` is kept rather than simplified away: at the frame edge the
    direction is longer and that pixel really does subtend less.
  - **Two invariants that are easy to break.** The grazing `1/|cos θ|` stretch is
    applied on the way *out* to a texture width and discarded — folded back into the
    cone it compounds at every bounce (five grazing hits is 3125×) and every deep
    texture reads its 1×1 level. And a bounce's lobe width comes from
    `ScatterSample::spread`, **not** from `pdf`: by the time the tracer sees a sample
    the pdf may have been replaced by the guide/BSDF mixture, so a near-mirror under a
    trained guiding field would report a broad density and blur its own reflection.
    A cosine lobe's pdf also goes to zero at grazing, which says nothing about how
    wide the lobe is.
  - **Cone → texture space** goes through a per-triangle **density**,
    `sqrt(parametric_area / world_area)`, on `UvMap` (chart units) and `FaceMap`
    (face units). Always built in the mesh's **local** frame with the placement's
    `cbrt(|det|)` recorded per `geom_id`: at `flush_meshes` a baked placement shares
    a local-space `FaceMap` while cloning its `UvMap`, and one scale cannot serve one
    table in world space and the other in local. `MeshArena::intern` is the single
    funnel every shared mesh passes through, so there are only two build sites (the
    other is the non-invertible bake, whose vertices are already world-space and
    which therefore takes scale 1.0). Being a *ratio of areas* a density needs no
    mirror-swap correction, unlike every other lookup in `rt_world.rs` — do not add
    a `swapped` arm. `FaceMap`'s parametric area is the constant 0.5 for every mapped
    fan slice (both quad arms are unit-determinant shears, `Triangle` is the
    identity), **except** on a subdivided mesh, whose triangles carry explicit
    sub-face UVs: the constant would over-estimate by 4^L, 64× at level 3, and send
    every Ptex lookup on the mesh to its coarsest level.
  - **`SideTables::Default` is hand-written for one field**: `scale` must be 1.0, not
    0.0. `attach_masked` pushes one per geometry, so a derived default divides every
    footprint by zero and hands each texture an infinite width — which renders as a
    perfectly plausible coarse mip.
  - **`0.0` means point-sample** throughout: both `eval` methods take a width and
    both read zero as "finest level", so a host that tracks no footprint gets the
    historical behaviour rather than a wrong one. `Op::Texture` scales the width by
    `uvtiling` alongside the coordinates — a texture tiled 10× is minified 10×.
  - **Magnification short-circuits before the `log2`.** It is the common case, its
    answer is level 0 regardless, and taking it through the clamp instead measured
    ~9% of render on a scene whose output does not change at all.
  - **Verified numerically** (`scripts/gen_texture_alias_scene.py --measure`), because
    a wrong mip level is a plausible blur. Aliasing does not converge, so each
    configuration is compared against a high-spp reference *of itself* — comparing
    filtered against unfiltered would measure the bias between two different correct
    answers. On the generated checkerboard plane, 16 spp against 1024 spp: RMSE
    0.01024 filtered against 0.04211 point-sampled, a **4.11× reduction**. Cost is
    ~1.7% of render on a magnified textured sample and ~19% on that plane, which is
    the honest worst case (one textured plane, depth 2, so nearly every shading call
    is a trilinear fetch).
- **UsdLux units follow the spec** (`LightAPI` in OpenUSD's `usdLux/schema.usda`), and
  where the spec's prose and OpenUSD's reference implementation — hdEmbree's
  `lightSamplers.cpp`, the delegate that grew the UsdLux reference — disagree, crust
  follows the implementation and says so in `lux.rs`. Every light shares
  `lux_params`: `intensity · 2^exposure · color` is the emitted **luminance in nits**,
  times `blackbody_rgb(colorTemperature)` when `enableColorTemperature` is on
  (hdEmbree's Krystek-locus → Rec.709 conversion, luminance-normalised, so 6500 K is
  (1.044, 0.983, 1.036) and not quite white — the spec text claims white, the older
  table-driven `blackbody.cpp` admits it is not). **`inputs:normalize`** divides by the
  light's `sizeFactor`: its **world-space surface area** for rect / disk / sphere /
  cylinder (transform scale included — `AffineShape::area` integrates it, exact for a
  disk and any similarity), `π·sin²θmax` for a distant light (`distant_size_factor`),
  and nothing for a dome. `inputs:diffuse` / `inputs:specular` ≠ 1 warn and are ignored
  (per-lobe multipliers crust's transport does not split). **`ShapingAPI`**
  (`lux::Shaping`) — focus, focus tint, cone angle + softness, IES — is a per-direction
  factor on area lights' radiance, measured off the light's −Z. It is read off the prim
  whether or not the API is applied, but an unauthored `cone:angle` falls back to the
  schema's **90° only when `ShapingAPI` is applied** and to 180° otherwise: that is what
  Hydra hands hdEmbree (the attribute does not exist without the API), and applying 90°
  unconditionally would cut the back off every sphere light ever authored. IES profiles
  cross the `AssetLoader` seam (`load_ies` → `Arc<IesProfile>`); `crust-assets::ies` is
  a port of the Cycles LM-63 reader hdEmbree vendors, minus its type-A infinite loop.
  The profile's 0° is the light's −Z, `ies:normalize` *divides* by the profile's mean
  intensity (the spec's formula says multiply; every implementation divides), and
  `ies:angleScale` is the spec's bimodal remap. Sample: `samples/usdlux.usda` (every
  light type, `normalize`, colour temperature, a shaped spot, an IES fixture from
  `samples/ies/spot30.ies`, a squashed sphere light, and a textured window card from
  `samples/textures/window_card.exr`).
- `UsdLuxDistantLight` → a `DistantLight` in the light list only (no scene geometry). It
  points down its local -Z; `inputs:angle` is the source's angular *diameter* (default
  0.53°, the sun's) and a zero angle is widened to `MIN_DISTANT_ANGLE_DEG` rather than
  made singular, so the integrator keeps one MIS path instead of a delta special case.
  **`intensity` is the sun disk's luminance in nits** — so an un-normalised 0.53° sun
  needs an intensity in the tens of thousands to light anything, exactly as in Hydra.
  With **`inputs:normalize`** it is the **illuminance in lux** on a surface facing the
  light, and widening the angle softens shadows without changing exposure. A zero angle
  is a delta light whose intensity the spec and hdEmbree both deliver as illuminance.
  All four cases reduce in `emit_distant_light` to one number — the illuminance the
  *authored* cone delivers (`distant_illuminance`, f64) — handed to
  `DistantLight::new`, which spreads it as radiance over the cone crust actually
  samples, dividing by `projected_cone_solid_angle` evaluated from the **same f32
  cosine** that bounds the cone. That is what makes the delivered lux exact: at the
  sun's size the f32 cosine is a few ulps from 1, so the cone crust samples differs
  from the authored one by up to 0.5% of its solid angle, and at 0.01° its cosine is
  exactly one. Widening to `MIN_DISTANT_ANGLE_DEG` therefore preserves the illuminance
  and moves only the penumbra. *History:* crust used to treat `intensity` as irradiance
  unconditionally and spread it as `E / Ω` with `Ω = 2π(1 − cos θ)` computed in f32 —
  whose cancellation made a 1.5° sun 2e-4 too bright — so `samples/domelight.usda` now
  authors `normalize = 1` to keep the look it was lit with. Bounce rays find it by *escaping*
  along a direction inside its cone, which is the `Light::escaped` half of MIS.
- `UsdLuxDomeLight` → a `DomeLight`: an infinite environment covering every direction, so
  once one exists it **replaces the built-in sky gradient** (`Light::escaped` answers for
  every escaping ray). Radiance is `intensity × color × 2^exposure` (× the colour
  temperature's blackbody; `normalize` does not apply to a dome) times an optional
  lat-long `EnvironmentMap`; only `latlong`/`automatic` `texture:format` is supported and
  anything else warns and falls back to the uniform colour. The prim's *rotation* orients
  the sky (a dome is at infinity, so its translation and scale are meaningless). The map
  is importance-sampled by luminance × sinθ — the Jacobian matters, without it polar
  texels are over-sampled — which is what keeps a small bright sun in an HDRI from
  becoming a firefly farm.
  - **crust-core decodes nothing.** `inputs:texture:file` is resolved against the USD
    layer's directory and handed to the host through the `AssetLoader` trait
    (`Scene::from_usd_with_assets`); `Scene::from_usd` passes `NoAssets`, which warns and
    falls back to the uniform colour. `crust-assets` implements it with `exr` (OpenEXR)
    and `image` (`.hdr` and LDR, the latter un-gamma'd to linear). This is the seam
    general texture support should grow through.
- The four **area lights** are one-sided `Emissive::light` surfaces paired with an
  `AreaLight`:
  - `UsdLuxRectLight` → two emissive `Triangle`s + `AreaLight(RectShape)` (local XY plane,
    emitting along −Z). The emitting normal is −Z under the *normal* transform
    (`±edge_u × edge_v`), not the transformed −Z, which stops being perpendicular to the
    rectangle under a shear; the transformed axis is kept whenever it is perpendicular,
    so ordinary lights render as before. **`inputs:texture:file`** multiplies the
    emission per point (`lux::RectTexture` on the light's `Emissive`, read by both MIS
    halves through `radiance_toward(p, …)`): image top row at the light's +Y edge,
    left column at −X, **nearest-texel** — all three hdEmbree's `_SampleLightTexture`
    conventions — and `normalize` still divides by the area. The map crosses the seam
    as `AssetLoader::load_light_texture` → `LightTexture`, linear **float** RGB decoded
    by `crust_assets::read_rgb_image` (the dome's decoder, EXR / `.hdr` kept as
    authored, LDR un-gamma'd), *not* the UV-texture path, which narrows to 8 bits when
    preloading. The light is sampled by the solid angle it subtends (see `RectShape`
    under "Core traits"), not by the map.
    Sample: `samples/rectlight.usda`.
  - `UsdLuxSphereLight`, `UsdLuxDiskLight` (local XY plane, emitting along −Z) and
    `UsdLuxCylinderLight` (along local X, emitting from its side and **not** its end caps)
    share `emit_round_light`: the light's radius / length are folded into the transform
    of a `UnitShape`, and where that is a similarity over the axes the shape is round in
    the geometry is the kernel's world-space analytic `Sphere` / `Disk` / `Cylinder`;
    under a non-uniform scale (an ellipsoid, an ellipse, an elliptical tube) it is the
    unit primitive placed by an `Instance`, which the kernel intersects exactly under
    any affine map, sampled by an `AffineShape`. The uniformly scaled sphere keeps the
    historical `SphereShape` so existing scenes stay bit-identical — but its radius
    **now includes the transform's scale**, which it used to ignore. `treatAsPoint` /
    `treatAsLine` are ignored (hints for renderers without area lights, which the schema
    lets an area-light renderer ignore).
  The source geometry is camera-invisible by default (`light_ray_mask`) — see the
  `crust:rayMask` bullet above for the opt-ins. Sample: `samples/light_visibility.usda`.
  `PortalLight`, `GeometryLight` / `MeshLightAPI`, `VolumeLightAPI`, light filters and
  light linking are not read.
- **Volumes**: any prim carrying `crust:volume:type` imports as a `VolumeRegion` (checked
  *first* in the dispatch, so it never becomes geometry — its bounds must not occlude
  shadow rays). The local box is `[-size/2, size/2]³` when the prim authors `size` (a
  `Cube`; USD's default size is 2), else the unit cube; placement/orientation/scale come
  from the composed prim transform. Attributes (all in `crust:volume:`, defaults in
  parentheses): `type` = `homogeneous` | `smoke` | `grid` (required); `densityScale` (1);
  `sigmaS` color3f (0.5 grey); `sigmaA` color3f (0); `emission` color3f (0);
  `anisotropy` (0, clamped ±0.99). Smoke adds `noiseScale`/`noiseOctaves`/`noiseGain`/
  `noiseLacunarity`/`noiseThreshold`/`noiseSeed` (4 / 4 / 0.5 / 2 / 0.3 / 0); grid needs
  `gridDims` int[3] + `gridData` float[] (x-fastest, length must equal nx·ny·nz — warns
  and skips otherwise). Sample scenes: `samples/fog.usda` (homogeneous god rays),
  `samples/smoke.usda` (noise plume + emissive ember + tiny explicit grid).
- **Frame / time code** (`-f/--frame`, `Scene::from_usd_at_frame`). Every attribute read in
  the importer goes through `eval_time()` — `Attribute::get_at` at the requested code —
  so transforms, points, camera, lights, instancer arrays and render settings all move
  together; an attribute with no time samples reads its default either way, and only
  animated ones change. The time lives in a scoped **thread-local** (`EvalTimeScope`)
  rather than a parameter, because ~40 read sites in helpers handed only a `Prim` would
  otherwise all carry a value none of them decide; that is sound only because the import
  is single-threaded — parallelising it means threading the time explicitly. **No
  frame means the attribute *default*, not frame 0**: a stage that authors only
  `timeSamples` reads its schema fallback, which is exactly the pre-`--frame` behaviour
  (pinned pixel-identical on the samples). openusd's own xformable composition (the
  `compose_xform_ops` fallback) has no default arm and keeps its historical 0.0. A frame
  also sets the sampler's frame seed (its integer part, over `crust:frame`), so a
  sequence gets independent noise per frame, and a frame outside an authored
  `startTimeCode..endTimeCode` warns (USD holds the end samples; it is usually a typo).
  A non-finite frame is refused in `load_scene` with `Error::InvalidFrame` (and earlier
  by the CLI's `parse_frame`): `NaN` compares false against the range, so it would
  otherwise pass the check silently.
  Not time-aware: `UsdPreviewSurface` inputs (read by openusd-schemas'
  `read_preview_surface`, default only), and `crust:motion:translate` motion blur, which
  is still an authored offset rather than derived from the samples across the shutter.
  Sample: `samples/animation.usda`.
- **Camera choice** (`UsdImportOptions::camera`, the CLI's `--camera`): the named prim,
  else the stage's **`RenderSettings.camera`** relationship (read off the index stage, so
  the traversal knows it before meeting any camera), else the first `UsdGeomCamera` the
  traversal meets. A requested path that is malformed is refused before the stage opens
  (`Error::InvalidCameraPath`), and one that names no camera fails after traversal with
  `Error::CameraNotFound`, which **lists every camera the stage has** — a production
  camera's path is usually buried in a referenced binary cache, and passing a bogus path
  is the quickest way to discover the real one. A dangling `RenderSettings.camera`
  target warns and falls back to the first camera, since the stage, not the operator,
  made that mistake.
- `UsdRenderSettings` gives `resolution`; per-render params live as custom attrs in the
  `crust:` namespace (`crust:samplesPerPixel`, `crust:maxDepth`, `crust:minSamplesPerPixel`,
  `crust:varianceThreshold`, `crust:frame`, `crust:samplingStrategy` token = `power` |
  `balance` | `light` | `bsdf`, `crust:lightSelection` token = `uniform` | `power`,
  `crust:pixelFilter` token = `box` | `triangle` |
  `gaussian` | `blackman` | `mitchell` + `crust:pixelFilterRadius` float). Missing attrs
  fall back to defaults (128 spp, depth 32, 640×360, power MIS, power light selection,
  triangle filter at radius 1.0) defined as consts at the top of the file.

Note: `openusd` is a hard dependency and USD is always compiled in — there is no `usd`
feature flag.

**`openusd` is tracked at `0.7`**, and **the typed schemas are a second crate**:
0.7 moved `UsdGeom` / `UsdLux` / `UsdShade` / `UsdRender` out of the core crate into
[`openusd-schemas`](https://docs.rs/openusd-schemas), versioned in lockstep and carrying
the `geom` / `lux` / `shade` / `render` feature flags the core crate used to. So
`openusd::schemas::geom` is now `openusd_schemas::geom`, and core `openusd` has no
features left but `serde`.

Three API changes came with it, all in `scene/usd_import.rs`:

- `Stage::prim` / `Stage::attribute` / `sdf::Layer::prim` take any path-like argument and
  therefore return a `Result` whose error is a *parse* failure. Every call here passes an
  already-parsed `sdf::Path`, so `prim_at()` wraps the unreachable arm once rather than
  scattering the same `expect` over a dozen sites.
- `StagePopulationMask::new` is fallible (a mask path must be an absolute prim path).
  `open_stage` reports it as an ordinary `Error::UsdOpen` — `stream_roots` only ever
  yields composed top-level prim paths, so a failure would be a bug, not bad input.
- `Material::compute_surface_source` takes an **ordered render-context list** and returns
  the whole resolved terminal (every source driving it) instead of one shader. 0.6 took no
  argument: universal terminal first, then every authored context alphabetically. The
  `SURFACE_RENDER_CONTEXTS` const restores that preference — `""` (the universal context)
  leads, `glslfx` is the only namespaced one crust decodes, and an `ri` surface is a
  PxrDisneyBsdf that `has_shader_id` already caught upstream of the call. Verified
  output-preserving: all 15 checked-in sample scenes render **pixel-identical** to the 0.6
  build at 16 spp (`exr_diff`, 0 differing pixels).

Earlier history worth knowing when reading old branches: 0.6.0 fixed two composition bugs
that made the Moana island unreadable (written up under `docs/issues/`), and renamed 0.5's
`prim_at` to `Stage::prim` and made `sdf::Value::Token` carry an interned `tf::Token`
rather than a `String`.

## Rendering the Moana island

**`usd/island.usda` imports directly**: 3 151 850 geometries, 21 904 388 top-level BVH
primitives, ~6:18 to parse (~4:45 of it traversal), ~47.6 GiB peak. It reads from its own
root layer with no preparation — the openusd bugs that used to prevent that are fixed in
0.6.0. The only geometry still lost is 35 empty xgen prototypes (beach shells, fibers,
seaweed, palm debris); the six `PointInstancer`s that used to vanish with them now import,
which is worth ~74 500 resident instances of bay cedar understory.

`renders/moana_island/island_root.usda` (gitignored) is a hand-assembled root layer that
references each element at **stage root** instead of under `/island`. It is no longer
needed, and is kept only as the workaround for openusd 0.5, where the nested-reference
bug otherwise yields almost no geometry. `/island` carries no transform, so the two are
geometrically equivalent; they differ ~0.06% in the *unique* (resident) triangle count,
which is prototype-sharing accounting rather than rendered geometry — the likely cause
being that `/island` is a single top-level subtree, so `MIN_STREAM_CHUNKS` keeps the
direct read single-stage while the 22-prim layer streams, giving the two different
`MaterialCache` epochs. That layer also authors a camera inline as a copy of `shotCam`,
from before `--camera` existed: without a named camera the importer takes the *first*
`UsdGeomCamera` it meets, and traversal order is unspecified, so reading `island.usda`
directly gets whichever of its seven cameras comes first. `--camera` names one instead.

Measured per element (Ptex declined, so geometry only): **~57.7 M top-level triangles**
across the 20 elements, the largest being `osOcean` (15.6 M), `isCoral` (14.5 M),
`isMountainA` (6.7 M) and `isMountainB` (6.4 M). Worst single-element openusd composition
peak is ~12.9 GiB (`isCoral`), which streaming keeps as a transient rather than a sum.
Ptex over the whole island is **3 618 textures / 2 564 203 faces**, and preloading them
costs **5.98 GiB** — the often-quoted 4.58 GiB is the 32x32 base *without* the mip
pyramid, which adds the expected 4/3. (1.84 GiB at a 16x16 base, 736 MiB at 8x8, and
**494 GiB** at full resolution, which is why the cap was not an optimisation but the
thing that made the island possible.)

**Streaming replaces that cap** (`CRUST_PTEX_STREAM=1` **plus
`CRUST_PTEX_STREAM_MIPSPACE=file`** — every island `.ptx` is mipmapped, so the default
policy declines them all and the switch alone reproduces the preloaded column exactly;
see `docs/ptex_streaming.md`).
Measured at 640x360 / 8 spp against the same build preloading: Ptex residency **5.98 ->
0.61 GiB** (39 textures streamed, 3 579 preloaded under the size threshold), Ptex decode
**84.8 s -> 14.0 s** so `Load assets` falls 01:40.7 -> 27.3 s, `Traverse prims` RSS
**47.34 -> 41.48 GiB**, peak RSS **51.28 -> 47.08 GiB**, and `Render` costs **+1.2%** —
nothing like the 2.8x the deliberately texture-bound sample scene shows, because the
island is traversal-bound. The whole run finished 69 s sooner. Peak falls by less than
residency does because peak lands at `Commit acceleration structure`, the SBVH build
transient; the figure this feature moves is the traverse RSS.
The cache held **3.28 MiB of a 2 GiB budget with zero evictions**: at this framing the
ray cone asks for coarse levels, and a coarse level of a face is a few texels — reading
only the resolution the frame resolves is exactly what preloading cannot do. The budget
is a ceiling, not an allocation, so do not lower it on that number; a closer camera or a
4K frame walks the same faces at finer levels.

Two costs specific to the full rig: `island.usda` authors *two* `DomeLight`s, and crust
has no per-light camera-visibility, so both light the scene (the sky is doubled) and both
textures decode — `islandsunVIS.png` is 16384x8192 and the pair peaks at ~11 GiB. Dropping
`sky_dome_cam_llc` (`active = false`) is the first lever if memory or exposure matters.

## Known incomplete work

- **Geometry/acceleration caveats.** Motion blur is transform-only and lerps the *matrix*
  linearly (no deformation blur, no quaternion motion — a large shutter rotation bows
  slightly, but the union-of-endpoints bbox stays conservative). Curve import flattens
  cubic spans to polylines (no exact cubic intersector) and lerps widths across a span in
  parameter; the rounded-cone can report an interior sphere surface for rays *starting
  inside* the hull (irrelevant for opaque hair). Mesh-BVH sharing needs identical
  points/topology *and* material binding. Emissive curves/instances are not light-list
  entries (BSDF-sampled only, like emissive volumes).
  Baking single-placement meshes (above) leaves *resident* memory unchanged — the same
  triangles, one fewer BVH — but it moves work into a single large top-level SBVH build,
  and that build's **transient** peak is higher: on `Kitchen_set` (1 394 meshes baked,
  414 599 top-level triangles) kernel memory went 134.59 → 134.44 MiB while peak RSS went
  374 → 579 MiB. The cause is pre-existing and not specific to baking: `PrimRef` and the
  binary `Node` are 48 bytes each, and `merge` concatenates child node arrays while both
  children are still alive, so one build over ~600 K references (with SBVH duplication)
  transiently holds a few hundred MiB where 1 788 small builds held almost nothing. If that
  peak ever matters more than the ~20% render win, the lever is a triangle-count cap on
  baking; the real fix is a builder that does not materialise the whole binary tree.
  Subdivision surfaces sharpen this: a level-L cage feeds 4^L× more triangles into the
  same build, and on the `gen_subdiv_stress.py` scene at level 4 the SBVH commit peak
  (2.19 GiB) is nearly twice the refiner's own traverse-phase peak (1.16 GiB) — so the
  build transient, not opensubdiv, is the first lever if a subdivided scene runs out of
  memory. Within `subdivide()` itself the tail holds ~5 copies of the last level's
  positions (`verts`, `limit`, `points`, `verts_a`, `normals`) plus the whole retained
  refiner; restructuring to drop the refiner before the copies would shave roughly a
  third off its ~313 B/face transient if that ever matters.

- **Instancing caveats.** The kernel nests instances to arbitrary depth (transforms
  compose, normals map back through every level, masks gate per level — pinned by
  `instances_nest`, `nested_instances_compose_transforms_and_normals` and
  `nested_instances_respect_masks_at_each_level` in `crust-rt`), and the importer expands
  a `PointInstancer` inside a prototype into real nested sub-scenes. What is *not*
  supported is a natively-instanced (`instanceable`) prim inside another instance's
  prototype: `openusd` 0.5 cannot read its contents at all (see the upstream bug below),
  so the importer skips it with a warning. Volumes inside prototypes are skipped — they
  live outside the surface BVH by design and cannot ride an instance transform.
  `PointInstancer`
  `velocities` / `accelerations` / `angularVelocities` are ignored, so vectorized instances
  do not motion-blur (`crust:motion:translate` still works on ordinary prims), and all
  per-instance arrays are read at the default time sample. Top-level `UsdGeomSphere` prims
  still bake their centre into world space and so ignore scale; spheres *inside* a
  prototype go through the instanced path and scale correctly.

- **SIMD stops at 128 bits** (`docs/simd.md` has the audit and the numbers). Everything
  vectorized — `glam`'s `Vec3A`/`Vec4`, the BVH4 slab test, `Tri4` leaf packets — is
  SSE2/NEON-width, because `std::simd` is still nightly-only and crust builds on stable.
  Current practice says target AVX2 instead, so the reasons not to are recorded: merely
  *enabling* AVX2 codegen is worth 2–4% (LLVM cannot widen a 4-lane algorithm), and
  8-wide leaf packets would buy exactly nothing because no leaf holds more than 4
  triangles — pinned by `eight_wide_packets_would_not_reduce_vector_rounds`. **BVH8
  nodes** are the one place 256-bit vectors would still pay, and that is a project
  decision, not just an optimization: reaching 256 bits at *runtime* needs `unsafe`
  `core::arch` intrinsics (against this crate's "100% safe Rust" claim), a new
  dependency (`wide`/`multiversion`), or a non-distributable `-C target-cpu`.
  Also: only *triangles* pack — sphere, curve and instance leaves still run scalar (no
  `Sphere4`); rays are traced one at a time, so there is no coherent ray-packet tracing
  (that needs the integrator restructured, not just the kernel); and the checked-in
  sample scenes are shading-bound, so kernel speedups barely show up there — use
  `scripts/gen_stress_scene.py` to benchmark traversal changes end to end.

- **`openusd` xformOp bug, worked around locally; fixed upstream in 0.6.0.** `openusd`
  0.5.0 composed multi-op `xformOpOrder` stacks in the wrong order (the authored
  translate came back multiplied by the scale), which used to make
  `samples/cornellbox.usda` render as floating objects against sky. `usd_import.rs`
  therefore composes the individual `xformOp:*` attributes itself
  (`compose_xform_ops`: translate/scale/rotateX·Y·Z/rotate-Euler-triples/orient/
  transform, `!invert!` prefixes, namespaced suffixes), falling back to openusd's
  composition — with a warning — only for op kinds it cannot decode. Regression test:
  `cornellbox_transforms_compose_correctly`.
  **On 0.6.0 the case that motivated it is fixed**: a translate+scale stack composes to the
  authored translation with the scale on the diagonal (`examples/xform_probe`). That is one
  case, not the whole surface — rotations, Euler triples, `orient`, `!invert!` prefixes and
  namespaced suffixes are unverified — so the local composer stays authoritative and the
  fallback stays in place. Retiring either means checking those kinds first.

- **Fixed in openusd 0.6.0, keep in mind when reading old branches.** Two composition
  bugs used to make the Moana island import as almost nothing, and both failed silently —
  the data was readable, the API reported success, and the importer just saw less than was
  there. A prototype did not materialize when the `instanceable` prim arrived through a
  reference on a non-root prim (which is exactly `island.usda`'s shape), and a
  relationship's targets resolved to zero when its prim reached the prototype through a
  variant selection (worth six `PointInstancer`s, all isBayCedarA1's variant geometry).
  Written up with minimal reproductions under `docs/issues/`, kept because the symptoms
  are worth recognising, and because pinning to 0.5 brings both back. `examples/proto_probe`
  and `examples/rel_probe` are the diagnostics.

- **Nested native instances are still skipped, but no longer have to be.** An
  `instanceable` prim *inside another instance's prototype* could not be read on
  `openusd` 0.5.0: resolving its prototype, or reading the type name of anything beneath
  it, tripped a `debug_assert!` in `pcp/instancing.rs::materialize_prototype` (debug builds
  aborted, release had it compiled out). So `collect_proto_parts` tests `is_instance()`
  **before** any schema lookup — a schema `get()` reads the type name, which is what
  aborted — and skips such prims with a warning. Regression test:
  `nested_native_instance_degrades_gracefully`.
  **On 0.6.0 that abort is gone**: a debug build resolves the nested prototype to a valid
  prim with its geometry (checked with `examples/proto_probe` on a four-line stage). The
  skip arm is therefore now conservative rather than necessary, and deleting it would
  recover this geometry — splice the inner prototype's parts in with composed transforms,
  since a native instance is a single placement and needs no extra level of kernel
  indirection. Not done yet, and it costs the Moana island nothing (that arm never fires
  there), so it is a correctness improvement for other stages rather than a fix for this
  one. The regression test would need rewriting to assert the geometry arrives instead of
  that it is skipped.
- **MaterialX caveats.** The BSDF reduction projects a layered MaterialX stack
  onto one OpenPBR lobe set. Two stacked dielectrics survive (the upper one is
  the coat), but a *third* is averaged into the coat's roughness, a coat's
  `tint` is dropped, and a glaze over a base specular whose mask is zero at
  some point still shades there as coat-over-diffuse (the promotion is
  structural, by design). Anything past two specular interfaces needs a
  layered BSDF material, not a different reduction. `subsurface_bsdf` maps to OpenPBR's
  subsurface weight but not its radius; `thin_film_bsdf` is pooled as an
  ordinary dielectric; MaterialX transmission maps to no lobe, so a
  MaterialX-authored glass renders opaque. The graph is re-evaluated at every
  `scatter`/`eval` call on a vertex rather than memoised per hit, which is the
  obvious optimisation if MaterialX surfaces ever dominate a render. Only
  document-scope and `<nodegraph>` nodes are read — `<nodedef>` custom node
  *implementations* are not, so a graph instantiating one gets that input at a
  constant (reported, not silent). No `<look>` / `<materialassign>`: bindings
  come from USD.
  On the **emission** side: `uniform_edf` is read exactly, and it is the only EDF
  that is. `conical_edf`, `measured_edf` and `generalized_schlick_edf` are all
  *directional* distributions, and crust's OpenPBR emitter is uniform —
  `emitted_directional` varies with angle only through the coat, which is a slab
  above the emitter and not the emitter's own lobe shape — so a cone, an IES profile
  or a Schlick falloff has nowhere to go. Pooling one onto a uniform emitter would be
  a plausible glow at the wrong intensity, so they are refused and reported rather
  than approximated, the same standard `crust:mipspace` applies. A `surface`'s
  `opacity` input is still dropped. And an **emissive MaterialX surface is not a
  light-list entry**: `AreaLight` pairs a `LightShape` with an `Arc<Emissive>`, whose
  radiance is a constant, and a graph's emission is a function of the shading point.
  So such a surface is found by BSDF/bounce sampling only, at full weight — exactly
  how emissive curves, instances and volumes already behave — which means no NEE and
  a firefly risk near a small bright emitter. That is also why `MtlxMaterial` leaves
  `emitted()` at zero and answers through `emitted_at` instead: the light list reads
  the hit-free one, and the two must agree for anything it samples.
- **Texture residency caveats.** Ptex streams now (`CRUST_PTEX_STREAM=1`, above and in
  `docs/ptex_streaming.md`), so what is left is the shape of it rather than its absence.
  The cache is the reader's, which is what this section used to ask for and is still the
  right place for it — do not grow a second one here, and do not bolt Ptex onto the `.tx`
  tile cache. What remains: the **mip chain cannot be built in linear light from streamed
  tiles**, because a streamed level comes off disk reduced in the file's own encoding
  while a preloaded pyramid is reduced in linear light — convexity makes the streamed
  chain the darker, measured at up to 0.147 on the tiled fixture, and the base level is
  bit-identical. That is now *refused* rather than merely recorded
  (`CRUST_PTEX_STREAM_MIPSPACE`, above), which makes the gap a live restriction rather
  than a wrong render: with a pyramid on, streaming is off unless the operator opts into
  the file's chain. Retiring both gates needs the reduction to happen in the reader,
  against a declared working space — not a second pyramid cache here, which would also
  have to read level 0 to answer a coarse lookup. `PtexColor` remains the default and
  the oracle. Ptex is still 8-bit
  through both backends, so an HDR `.ptx` gains nothing from either. Filtering across
  face boundaries is still not attempted (see the filtering caveats), and the streaming
  path does not change that. Cost on the worst case (`samples/ptex_quads.usda`, two
  textured planes filling frame): ~2.8x the preloaded render, against ~2x for the UV
  path's.
  On the UV side: automatic conversion needs `--auto-tx` and writes beside the asset
  (a read-only library preloads, with a warning per failed tile), 16-bit integer sources are still narrowed to 8 on page-in (deliberately —
  the renderer decodes `u8` through a 256-entry table, and two more bits of an LDR
  texture is not worth halving what the byte budget holds), there is no
  single-flight on a miss so two workers can decode the same tile at once (counted as
  "concurrent double fills", bounded by the thread count), and the cache is per-process
  rather than shared between renders. The EXR backing reads RGB (or a replicated single
  channel) and ignores alpha, refuses ripmaps, multi-layer and deep files, and requires
  square tiles; the TIFF writer still emits 8-bit RGB only, so an HDR conversion is an
  EXR conversion.
- **An HDR texture's range now has a consumer, and the remaining limits are
  elsewhere.** The input that uses the range is **emission**, and MaterialX's `edf`
  is read: `uniform_edf` flattens to an `Emission` term beside the BSDF lobes, and
  `reduce()` pools those onto `emission_color`/`emission_luminance` with nothing
  clamping either. The path is unbroken — file → `.tx` → `Texture2D::eval` →
  `Op::Texture` → `Emission.color` → `emitted_at` → `next_emit` → film — so the claim
  is a sample scene now (`samples/materialx_emissive.usda`) rather than only a seam
  test. Measured on it at 32 spp: the same frame preloaded and streamed differs on
  72.6% of pixels with a **max absolute difference of exactly 15.0**, which is the
  bright cell's authored 16.0 against the 8-bit path's clamp at 1.0; `mtlx_shade`
  prints the two side by side as `(1.000 1.000 1.000)` and `(16.000 9.000 3.000)`.
  What is still true: `base_color` above 1 is **still** clamped by `eon_diffuse`, and
  correctly — an albedo above 1 creates energy where a radiance above 1 is just a
  bright light. The preloaded `UvTexture` still narrows a non-EXR source to 8 bits at
  `to_rgb8()`, so an HDR `.hdr` emission texture needs `CRUST_TEX_STREAM=1` and a
  converted `.tx`; without them it renders, clipped, rather than failing. An `.exr`
  preloads at `f32` and keeps its range. And an **emissive MaterialX surface
  is not a light-list entry** — see the MaterialX caveats below.
  The **dome-light / HDRI path was never affected by any of this**, which is worth
  stating so the next reader does not go looking for a clamp that is not there:
  `load_exr_environment` and `load_image_environment` decode to `Vec<Vec3A>` through
  `to_rgb32f` (never `to_rgb8`), `EnvironmentMap` stores `f32`, and
  `exr_environment_round_trips` pins that a value above 20 survives. The workspace's
  one `to_rgb8()` is on the preloaded UV-texture path, which an environment map
  never takes. So the Moana island's domes carry whatever range their files hold;
  that `islandsunVIS.png` is an 8-bit PNG is a property of the asset, not of crust.
- **Texture filtering caveats.** Minification is filtered now (ray cones plus
  trilinear mip pyramids, above), so what remains is the shape of that filter
  rather than its absence. It is **isotropic**: a chart stretched in one axis is
  filtered by the geometric mean of the two, so grazing minification over-blurs
  where an EWA or ripmap filter would not — the `1/|cos θ|` stretch widens the
  footprint without giving it a direction. Cone spread ignores **surface
  curvature**, so a reflection in a curved mirror filters as though the mirror
  were flat, and ignores the **lens aperture** (a non-negative cone cannot
  express a footprint converging to the focus plane — harmless, since defocus is
  resolved by sampling) and the **IOR change across a refraction**. The
  **base-resolution cap remains**: the pyramid retires aliasing, not the ceiling,
  so a close-up still cannot resolve past 32×32 (Ptex) or 1024 (UV). **Nested
  instances** filter against the outer placement's scale only — the inner
  placements live inside a committed kernel scene and the kernel does not surface
  the instance chain — and a **non-uniform** placement collapses to `cbrt(|det|)`,
  so a `(1, 1, 10)` scale is off by up to ~4.6× on the stretched axis. Both
  degrade to a slightly wrong level, never to a wrong lookup. Ptex still does not
  filter across **face boundaries**, and on a **triangle** Ptex it does not filter
  across the packed anti-diagonal either: the mip chain is Ptex's own triangular
  reduction now, but `sample_level` is still a plain bilinear tap on the square, so a
  tap within half a texel of the diagonal picks up the mirrored triangle where
  `PtexTriangleFilter` would not. The face mapping is right either way — a
  three-vertex face resolves through `FanSlice::Triangle`, whose barycentrics *are*
  Ptex's parametric coordinates. The guide branch of `sample_bounce_direction`
  reports the widest possible lobe spread rather than the material's own, since it
  never picked a lobe; that costs sharpness only on guided secondary bounces,
  where the cone is near-saturated anyway.
- **UV texture caveats.** Normal maps need a tangent,
  which only baked single-placement geometry has (above). On the
  `UsdPreviewSurface` side: texture **alpha** is not carried (both samplers return
  opaque RGB, so `outputs:a` reads 1.0 before `scale`/`bias`), `UsdTransform2d` is
  not evaluated, `occlusion`/`displacement`/`specularColor` are not read, and a
  texture's `fallback` default when unauthored is the surface input's constant, then
  its schema default, rather than the spec's opaque black (deliberately — see above). A **subdivided** mesh
  carries no chart at all: refining a face-varying UV channel is a second
  synthetic hierarchy through the refiner, and carrying the cage's UVs onto
  refined triangles would stretch every texture across the patch it came from —
  so such a mesh warns and renders on its material's constant inputs. Only
  `primvars:st` (and `uv`/`st0`/`UVMap` as fallbacks) is read; there is no
  general primvar plumbing and no second UV set. `texcoord`'s `index` input is
  ignored for the same reason. And `decode_tile`'s `CRUST_TEX_MAX` resize has a
  cousin of the odd-level defect the mip chain was just fixed for: it takes
  `floor(sw / factor)` destination texels and **drops the remainder columns**
  rather than covering them, so a 2050-wide source under a 1024 cap loses one
  column of 2050 and the tile's domain slips by that much. Under a destination
  texel, against the full mis-weighted one the mip chain had — and unlike that
  one it is a *resize* averaged in the file's own encoding, so fixing it would
  move every render of a texture above the cap. Worth doing, not urgent.
- **Light sampling is the main source of 16 spp noise**, and `docs/light_sampling.md`
  is the survey of the state of the art (SIGGRAPH/EGSR/HPG, pbrt-v4, Cycles,
  RenderMan, Arnold, Hyperion) with a ranked roadmap and a measured baseline. In
  short: the light pick is by power, defensively (4.6× lower relMSE on a key among dim
  fills, 6% higher on `veach_mis`, §3.8 there; `--light-selection uniform` is the
  bit-identical A/B); sphere lights now sample their visible cone
  (1.3–12.9× lower relMSE at 16 spp on the five sphere-lit samples for ~8% more time
  per sample, §3.7 there),
  and rect lights their spherical rectangle (1.4–1.5× lower relMSE on near panels and
  fog, but 4–9% *higher* on the glossy `materialx_basic`/`usdpreview_textured` tiles, at
  ~110 ns more per NEE sample, §3.9 there), but disk/tube lights still sample by area
  rather than solid angle; the built-in sky
  gradient is not a light, so NEE never samples it. (The shadow ray is traced *last*,
  after the light's radiance and `mat.eval`, and only for a non-zero product —
  bit-identical, since it draws from its own `K_NEE_SHADOW` domain, §3.11 there.)
  Measure changes with
  `exr_diff ref.exr test.exr`'s `relmse:` against a 1024 spp reference (§8 there).
- **Lighting caveats.** Mesh lights (`MeshLightAPI` / `GeometryLight`), `PortalLight`,
  light filters, light/shadow linking and `ShadowAPI` are not read. A textured
  `RectLight` is sampled by solid angle rather than by its map's luminance (a card
  with a small bright region is noisier than it need be), its lookup is nearest-texel
  as the reference's is, and a `.tex` (RenderMan) map is not decoded.
  `inputs:diffuse` / `inputs:specular` warn and are ignored rather than split per lobe. Shaping is per *direction* only, so a shaped light
  is still sampled without regard to its cone (by solid angle for a rect, by area otherwise): a narrow spotlight wastes the NEE samples its cone
  cuts off (unbiased, but noisier than cone-aware sampling), and shaping is not applied
  to distant or dome lights (as in hdEmbree). IES evaluation is bilinear, as the
  reference's is. A tube light samples non-uniformly in world area (correct, not
  optimal); a squashed sphere samples its visible cone. `DomeLight` sampling is nearest-texel with no bilinear filtering, so a
  low-resolution HDRI shows texel edges in a mirror; `inputs:texture:format` values other
  than `latlong` are refused rather than mapped wrongly; and light-list picking by
  power is blind to position, orientation and visibility, so a far light gets as many
  shadow rays as an equally powerful near one, and a dome or sun only its uniform share
  however much it lights (a light BVH, `docs/light_sampling.md` §6.3, is the fix). Neither infinite light
  is visible to the guiding field's spatial structure (they have no position).
- **ALab gaps** (Netflix Animation Studios' ALab 2.2, `samples/ALab/`, gitignored).
  Shot mk020_0281, frames 1004–1057. `entry.usda` sublayers the baked procedurals
  (fur and cloth as value-clipped `BasisCurves`), the trailer cameras and the shot. It
  imports and animates under `-f`: 14 893 geometries, 12.9 M triangles plus 9.4 M cubic
  fur spans, 37 lights, ~3:07 to parse, ~30.5 GiB peak, ~38 s to render 1280x720 at
  64 spp. Two download facts come first. The **Asset Structure** package ships every
  geometry, camera, layout and light-rig `.usd` under `fragment/` as a 213-byte
  placeholder layer. Without **techvar assets** merged *over* that tree, the stage
  composes to just the fur, at its rig origin. Nothing fails; every placeholder
  prototype just logs "contributed no geometry" (~1 250 of them), and there is no set,
  body, light or shot camera. The techvar zip extracts to `techvar_assets/fragment/…`,
  which nothing references. It has to be merged into `fragment/` (`cp -rlf
  techvar_assets/fragment/. fragment/` hard-links it at no disk cost; the 2 111
  replaced placeholders are in `placeholders_backup.tgz`). What crust still lacks,
  most visible first:
  - **Materials now render, and texture memory is the limit.** Four fixes made
    frame 1004 shade (1 752 `usd_full` preview surfaces, 1 738 of 1 739 UDIM
    sets loaded). With all four in place, only 29 prims are still unbound, all
    camera projection planes and a few set pieces:
    - bindings resolve for the `full` purpose, inherited from ancestors (`bound_material`);
    - `proxy`/`guide` subtrees are pruned (`non_render_purpose`, 1 459 `GEO_PROXY`
      duplicates);
    - unresolvable `<UDIM>` paths anchor on their authoring layer
      (`attribute_asset_path`);
    - single-channel `rgb.R` EXRs decode (`read_exr_rgb`).
    **Every ALab texture is already a `.tx` in all but name**: all 6 832 EXRs are
    64x64-tiled and mip-mapped (OIIO `maketx` output), so they stream straight from
    the source with no conversion — `--auto-tx` tries the source first and leaves such
    a file alone. What stopped that was the streaming reader's channel check
    (`resolve_rgb`), which matched `R`/`G`/`B` by full name and refused the 661
    three-channel sets written as `rgb.R`/`rgb.G`/`rgb.B`; they fell back to
    preloading at `f32`, 34 GiB on one frame. It matches by base name now, as the
    preload reader does. (Preloading everything at the default cap would be ~92 GiB
    against a ~35 GiB import on a 61 GiB machine, which is why this matters.) `occlusion` is
    not read, and the `usd_preview` materials are flat proxies and not worth decoding.
    **One map is missing from the dataset itself**, not mis-resolved:
    `tool_wrench_boxend03`'s `usd_full` connects `roughness` to
    `tool_wrench_boxend03_roughness.<UDIM>.exr`, which ALab does not ship (its folder
    has `ao`, `ior`, `metallic`, `ntu`, `surfaceColor`). The host logs one
    `No tiles found` ERROR for it every render, and the wrench shades at the schema's
    roughness 0.5.
  - **The shot camera has to be named.** `entry.usda` carries 29 camera prims — 28
    under `/root/cameras` for the 27 trailer shots (`mk020_0110` has two) plus the
    shot's — and no
    `RenderSettings.camera`, and the first one traversed is trailer camera
    `mk020_0280`. Render the shot with
    `--camera /root/camera01/GEO/renderCam_hrc/renderCam_buffer/renderCam_srt/renderCam`
    (found by passing a bogus `--camera`, whose error lists every camera). Each trailer
    camera also carries a `projectionPlane_M_geo` mesh, which still imports as grey
    geometry; deactivating `/root/cameras` in a wrapper layer (`over "cameras" ( active
    = false )`) removes both.
  - The rig's **13 `CylinderLight`s** (oscilloscope and ham-radio button lights) now
    import as analytic cylinder lights; this used to be a gap.
- **Path guiding** covers surfaces only (no volume/phase guiding) and trains on luminance
  (no chromatic distributions). Thick transmission — dispersive or not — is a
  continuous Walter et al. 2007 microfacet BTDF — sampled via VNDF + Snell, evaluable
  over the full sphere, and part of the NEE/guide mixtures (guide-chosen directions
  cross the interface via `Material::make_ray`, which tags the interior medium).
  Dispersion is continuous per-channel: each RGB channel refracts with its own
  Cauchy/Abbe-derived IOR (`cauchy_ior`, anchored at the Fraunhofer d line), sampling
  picks one channel's IOR uniformly, and evaluation runs three per-channel
  BTDF evaluations whose sampling pdfs average into the channel-mixture density. Only
  thin-walled transmission remains a per-sample delta lobe (`ScatterSample::delta`),
  excluded from continuous mixtures — carrying window-model energy
  (`(1−R)/(1+R)` transmittance, boosted `2R/(1+R)` reflection, view-dependent tint). The guide-vs-BSDF selection probability is fixed (no learned α), and
  spatial lookups are not parallax-compensated.
- **Volume regions** (`volume.rs`) have no OpenVDB / `UsdVolVolume` import — density is
  homogeneous, procedural fBm noise, or an inline voxel grid authored in the USDA.
  (`openusd-schemas` 0.7 does ship a `vol` feature — `Volume` plus `OpenVDBAsset` /
  `Field3DAsset` views — so this is now an unwritten importer rather than a missing
  dependency; it was the latter through openusd 0.6.) No volume path guiding (volume vertices push
  `train: None`; volume-heavy scenes train the surface field on noisier estimates —
  slower convergence, not bias). One global majorant per region — no coarse max-grid, so
  a high `densityScale` over a large box tracks slowly. Emissive volumes are not
  light-list entries: fire is found only by phase/BSDF-sampled paths (firefly risk near
  bright emission), never by NEE. Carried-medium (subsurface) scatter vertices run no
  NEE. Region overlap uses summed extinction (exact) with a σₛ-weighted phase mixture.
- **HG convention fix**: `sample_henyey_greenstein` used to apply PBRT's inversion to the
  propagation direction (PBRT's frame is around the *reversed* `wo`), so `g > 0`
  scattered backward. It now scatters forward, matching `hg_phase` (value == pdf, cosθ
  against the propagation direction); the histogram test in `medium.rs` pins the pair.
  The carried-medium estimator was also fixed: it double-counted extinction for
  scattering media (analog free-flight at σ̄ *plus* full Beer-Lambert) — scattering
  interiors (subsurface) render brighter than before, correctly. And bounce-hit emission
  (`next_emit`) is now attenuated by the arriving segment (tinted glass / smoke in front
  of an emitter used to pass emission through undimmed).
