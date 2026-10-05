# cli — design record

> Design record for the **cli** capability: the reasoning, measurements and
> history behind the behaviour `spec.md` states. Moved out of `CLAUDE.md`, which
> now keeps only the rules and pointers. Section and path references such as
> "above" or "see X" may point to another capability's `design.md` —
> `openspec/specs/*/design.md` is the whole record; `docs/architecture.md` is the map.

## Command cookbook

Every probe and A/B recipe, with the context each needs. `CLAUDE.md` keeps the everyday subset.

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
cargo run --release -- --scanline -i samples/cornellbox.usda # row order (tiles are the default)

# CLI flags: -i/--input, -o/--output (default output.exr), -l/--level (log level),
# --log-file [DIR] (tee the log to crust-render-<UTC stamp>.log), --scanline
#   (row order instead of the default 16x16 tiles; -b/--bucket is accepted and ignored),
# -s/--samples (override spp), -f/--frame (USD time code to evaluate the stage at),
# --strategy (power|balance|light|bsdf), --light-selection (uniform|power|learned),
# --filter (box|triangle|gaussian|blackman|mitchell) + --filter-radius (pixels),
# --indirect-clamp VALUE (firefly clamp on each sample's indirect light; default 10, 0 = off),
# --camera PRIM_PATH (render through that camera; else RenderSettings.camera,
#   else the first camera found -- a wrong path errors and lists the stage's cameras),
# --subdiv-level N (refinement level for every mesh whose subdivisionScheme is not none;
#   overrides RenderSettings crust:subdivisionLevel, default 0 = smooth-shaded cages; max 6),
# --subdiv-edge-length PX (adaptive subdivision: cut each cage edge of unshared geometry
#   into segments of at most PX pixels at the edge's own distance to the render camera,
#   per face; shared prototypes take --subdiv-level, else 0; out-of-view faces keep their
#   cage; overrides crust:subdivisionEdgeLength; --subdiv-level is the ceiling, default 3;
#   needs --camera or RenderSettings.camera, else warns and stays uniform;
#   A/B: CRUST_ADAPTIVE_PER_FACE=0, CRUST_ADAPTIVE_FRUSTUM=0),
# --stats (per-phase profile + scene statistics),
# --profile (implies --stats; adds a Guerilla-style per-section render profile),
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

# The geometry layout: --stats lists what the kernel holds per table (vertices,
# per-vertex normals, 24-byte triangle records, packets by layout, nodes), the
# share of packet lanes filled, and the kernel bytes per resident triangle --
# the one number a storage change is judged by (compact-triangle-storage:
# 196 -> 104 gathered on the subdivision stress grid). The two switches A/B
# the layout: `gathered` is the 192-byte packet before indexed packets existed,
# `indexed` the 92-byte one that gathers vertices at every test (bit-identical;
# 104 -> 79 B/triangle on the grid for 13% less throughput, 30% less on an
# out-of-cache soup), `auto` the measured default, gathered.
python3 scripts/gen_subdiv_stress.py /tmp/subdiv_stress.usda
CRUST_TRI_PACKETS=gathered target/release/crust-render -i /tmp/subdiv_stress.usda --stats -l error
CRUST_TRI_PACKETS=indexed  target/release/crust-render -i /tmp/subdiv_stress.usda --stats -l error
# ...and inside the render: Trace vs EvalBsdfs vs Texture vs SurfaceLighting,
# flat / by category / by execution tree. Costs render time (~15-20%, printed
# with the report), so never take a Render time from a --profile run.
cargo run --release -- -i samples/materialx_basic.usda --profile

# Subsurface random walks: --stats adds how many walks ran, the share that
# found an exit (the rest were absorbed or leaked out of an open mesh), their
# mean length in free flights, and the ray queries they cost ("subsurface walk
# rays", counted into the total but not into "bounce rays").
cargo run --release -- -i samples/materialx_subsurface.usda --stats

# Cutouts: --stats adds the closest-hit queries they cost ("cutout rays": the
# query past every hit a path passed through, plus every query of a blocked
# shadow ray re-walked through cutouts) and how many hits were passed through.
# A scene with no cutout material prints neither and runs the old code.
cargo run --release -- -i samples/materialx_cutout.usda --stats
# ...and the numbers behind it: opacity and each leaf's tangent at a point.
cargo run --release -p crust-render --example mtlx_shade -- samples/materialx_cutout.mtlx mtlx_gltf_card 0.05 0.05

# Tests (integration tests live in crust-core/tests/usd_scene.rs, load sample USD files)
cargo test
cargo test -p crust-core loads_cornellbox_usda     # run a single test by name

# Benchmarks (criterion)
cargo bench -p crust-core            # bench targets: "vec3 dot", "simple world", "simple world guided"
cargo bench -p crust-rt              # kernel traversal: intersect/occluded over 3 scene kinds + build

# Perf probes (better than criterion for kernel A/B: min-of-N, not a drifting mean)
cargo run --release -p crust-rt --example ray_throughput          # Mray/s per scene & query
# ...out of cache (8 M-triangle soup + 2 M-instance field, ~3 GiB, tens of seconds);
# built with --features traversal-stats it also prints nodes/leaves per ray:
cargo run --release -p crust-rt --example ray_throughput -- --large [MTRIS]
# Nightly-only experiment: 8-wide BVH nodes on std::simd (see docs/simd.md, "BVH8 on
# nightly"; stable rejects the feature with E0554 by design):
cargo +nightly test -p crust-rt --features bvh8
RUSTFLAGS='-C target-cpu=x86-64-v3' cargo +nightly run --release -p crust-rt --example ray_throughput --features bvh8
cargo run --release -p crust-render --example exr_diff -- a.exr b.exr   # did the image change?
cargo run --release -p crust-mtlx --example mtlx_bench -- lion_ldX.mtlx   # ns per MaterialX program run
cargo run --release -p crust-jit --example jit_bench -- lion_ldX.mtlx      # ...interpreter vs JIT

# Why does NEE fail? Per light: what fraction of its samples is backfacing,
# below the horizon, occluded (opaque vs glass only, by the light's own
# fixture vs the receiver's cavity) or visible. Renders nothing.
cargo run --release -p crust-render --example light_occlusion -- \
    samples/ALab/entry.usda --frame 1004 --camera /path/to/cam

# Placing a camera in a downloaded production asset, and settling whether a
# texture is display-encoded or linear (see "Ptex" under USD import).
cargo run --release -p crust-render --example scene_bounds -- scene.usda
cargo run --release -p crust-render --example tex_probe -- texture.ptx
cargo run --release -p crust-render --example tex_probe -- render.png [x0 y0 x1 y1]

# Does a MaterialX node compute what MaterialX's reference implementation does?
# The replay needs nothing installed; regenerating the reference values needs
# `pip install materialx` and an OSL built with -DUSE_FAST_MATH=0
# -DOSL_BUILD_TESTS=1 (the script refuses a fast-math build; the build recipe is
# under "Node semantics are checked" in openspec/specs/materials/design.md).
cargo test -p crust-mtlx --test osl_oracle -- --nocapture   # prints the guard counts
OSL_ROOT=/path/to/osl scripts/osl_oracle.py                 # rewrites tests/data/osl_oracle.txt

# What OpenPBR parameters does a MaterialX graph actually reduce to? A wrong
# albedo decode is a plausible pastel and a wrong mask is a plausible blend, so
# a MaterialX surface cannot be checked by eye -- this prints the numbers at a
# named point on the chart. See "MaterialX" under USD import.
cargo run --release -p crust-render --example mtlx_shade -- \
    samples/materialx_basic.mtlx mtlx_ceramic 0.25 0.5

# ...and across a whole MaterialX corpus: Ben Houston's Material Fidelity suite,
# 826 materials scored by PSNR against MaterialXView's render (~80 min on 4
# cores). Fetches the suite at a pinned revision into .fidelity/, renders,
# writes the report, and exits non-zero if any material fell more than 0.5 dB
# below scripts/material_fidelity/baseline.json. See docs/material_fidelity.md.
scripts/material_fidelity/fidelity.sh                             # init, build, run, report, check
scripts/material_fidelity/fidelity.sh run --materials noise3d     # a subset, then `check`
scripts/material_fidelity/fidelity.sh goldeneye -k noise3d        # through Goldeneye: FLIP + HTML report

# It goes through `FileAssets`, not just `UvTexture`, so `CRUST_TEX_STREAM` is
# honoured and the `emission` column shows the range an HDR texture actually
# carries -- a preloaded texel clips to 1.0 and a streamed one does not. This
# is the A/B for MaterialX emission (see "Known gaps: HDR texture range" in
# openspec/specs/textures/design.md):
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
# fixed upstream (see docs/issues/ and "Known gaps: openusd bugs and workarounds" in
# openspec/specs/usd-scene-import/design.md); keep them as the
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
# Both sides at -s 16, per "Measuring a change" in CLAUDE.md.
CRUST_PTEX_MAX_LOG2=5 cargo run --release -- -i samples/ptex_quads.usda -o a.exr -s 16
CRUST_PTEX_MAX_LOG2=5 CRUST_PTEX_STREAM=1 CRUST_PTEX_STREAM_MIN_MB=0 \
    CRUST_PTEX_STREAM_MIPSPACE=file \
    cargo run --release -- -i samples/ptex_quads.usda -o b.exr -s 16
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
scripts/bench_ab.sh -a A -b B -p "Parse USD stage" -x "-s 1" scene.usda  # ...of an import phase

# CI runs (toolchain pinned, RUSTFLAGS=-D warnings), as three parallel jobs:
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
```

## Logging

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
`thread_local`. The file is **unbuffered**, because the subscriber that owns it is the
process-global one, which is never dropped — a `BufWriter` would never be flushed and
would lose exactly the lines explaining why the run stopped. (`main` returns an
`ExitCode` rather than calling `std::process::exit`, so every other destructor does
run.) And `utc_stamp` **hand-rolls** the civil-from-days
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

## Environment overrides

Environment overrides, all of which exist to A/B an optimization against the behaviour it
replaced: `CRUST_STREAM_IMPORT=0` forces the single-stage USD import; `CRUST_MESH_BAKE=0`
forces every mesh to be instanced instead of baking single-placement geometry flat (output
is bit-identical with it set, which is what separates a deferral bug from a baking
difference); `CRUST_PTEX=0` declines every Ptex texture so surfaces fall back to their
constant `baseColor`; `CRUST_PTEX_MAX_LOG2` caps the per-face texture resolution loaded
(log2 edge length, default 5 = 32x32); `CRUST_SUBDIV=0` renders every mesh as its faceted base cage,
as before subdivision surfaces were read (unlike `--subdiv-level 0`, which keeps smooth
cage normals) — the A/B that separates a subdivision artifact from a material or
lighting one; `CRUST_TEX=0`
declines every UV texture so a MaterialX surface renders on its constant inputs (the
`CRUST_PTEX=0` of the UV path), and `CRUST_TEX_MAX` caps each decoded texture tile's edge
length in pixels (default 1024). MaterialX programs have two, each bit-identical against
the other side: `CRUST_MTLX_OPT=0` keeps a program as compiled (no constant folding,
hoisting or dead-op pruning), and `CRUST_SHADER_JIT=0` runs it on the interpreter instead
of crust-jit's machine code. Texture *filtering* has three more, which pair up:
`CRUST_TEX_MIP=0` and `CRUST_PTEX_MIP=0` build no mip pyramid (one level per tile / per
face, a third less memory, and `eval`'s width ignored structurally rather than by a
branch), while `CRUST_RAY_CONES=0` zeroes every footprint with the pyramids still
resident. Either side alone is bit-identical to the pre-filtering renderer, and the two
produce the same image as each other — which is what makes them an honest A/B of the two
halves: the pyramid, and the footprint that selects from it.

## Stats and render profile (`stats.rs`, `profile.rs`)

From the `crust-core` crate description:

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
  render/output phases, and the report prints the statistics (scene, allocated
  materials and lights by kind, kernel memory), ray statistics (primary / bounce
  / shadow rays, shadow rays per shading point, average and adaptive samples per
  pixel, how paths ended — the four endings sum to the primary rays), textures
  (Guerilla's total / loaded / still-in-cache memory and loaded / unloaded
  tiles, plus preloaded UV textures), Ptex, then phases by execution tree and by
  time. Collection is one `Instant` per *phase* plus integer counters, never a
  timer per ray, so it costs nothing in the integrator and is always on; only
  printing is gated (`--stats`). Two primitive views are reported because for an
  instanced scene they answer different questions: `top_level` is what the root
  BVH traverses, `unique` descends into instances counting each distinct
  prototype **once** and is therefore what occupies memory.
  **`profile.rs`** is the other half of Guerilla's page, the *Render Profile*:
  named `Section`s (`MainLoop`, `GeneratePrimary`, `Trace`, `Occlusion`,
  `Volume`, `EvalBsdfs`, `RunShader`, `Texture`, `TextureLoad`,
  `SurfaceLighting`, `VolumeLighting`, `Bounce`, `Subsurface`, `Contributions`) in
  four
  categories, recorded per thread into a call tree and merged by
  `profile::flush()` once per tile (per pixel in scanline mode), reported flat,
  by category and by execution tree with Guerilla's `local` / `total` / `glob.`
  columns, in **thread** time. Opt-in via `--profile` because it is not free:
  ~56 ns per section, 15-20% of render, printed with the report as an estimate
  and charged to the parents' local time. **Off, it must cost nothing, and
  that took two fixes**: a per-section runtime check cost +2.5% instructions on
  cornellbox (the guard's drop glue was an out-of-line call, and the recording
  code pushed `trace_path` out of line), so the integrator is **monomorphised**
  on `const PROFILE: bool` — `render_pass` reads the switch once and dispatches
  to `render_pixel::<true|false>`, and `profile::scope_if::<false>` compiles
  away. The second copy then stopped LLVM inlining `sample_bounce_direction`
  and `escaped_emission` (+1.1%), which are `inline(always)` for that reason.
  Net with profiling off: +0.18% instructions on cornellbox (the new counters),
  +0.48% on `materialx_basic` (the host's `Texture` / `RunShader` sections,
  which cannot take the const and keep a runtime check), `bench_ab` within
  noise on four scenes, images bit-identical with and without `--profile`.
  `tests/profile.rs` pins that each section's call count equals the matching
  `RayStats` counter (MainLoop = pixels, Trace = closest-hit, Occlusion = shadow
  rays, EvalBsdfs = surface vertices) and that the local times partition the
  thread time.
