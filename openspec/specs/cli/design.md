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
cargo run --release -- render -i samples/openpbr_showcase.usda -o out.exr
cargo run --release -- render -i samples/cornellbox.usda
cargo run --release -- render -i samples/usdlux.usda    # every UsdLux light: normalize, colour temperature, shaping, IES
cargo run --release -- render -i samples/materialx_teapot.usda    # MaterialX + UDIM (needs the DPEL download)
cargo run --release -- render -i samples/materialx_lion.usda      # the other DPEL asset: 140-op graph, sheen, 1.06 M tris
cargo run --release -- render -i samples/materialx_showcase.usda  # both, framed after the assets' overview.png (1080p)
cargo run --release -- render -i samples/materialx_basic.usda     # MaterialX fixture, self-contained
cargo run --release -- render -i samples/usdpreview_textured.usda # UsdPreviewSurface + UsdUVTexture (UDIM, EXR, auto)
cargo run --release -- render       # no -i → hard-coded procedural fallback (world::simple_scene)
cargo run --release -- render --scanline -i samples/cornellbox.usda # row order (tiles are the default)
cargo run --release -- ls camera -i samples/cornellbox.usda # the --camera paths, one per line (log on stderr)
cargo run --release -- ls light -i samples/cornellbox.usda  # also: material; plurals accepted

# Subcommands: `render` takes every flag below except -l, which is global (before or
# after the subcommand). --log-file stays render's: an optional value before a
# subcommand name or `ls`'s KIND would swallow it. `ls <camera|light|material>` lists through
# `Scene::list_usd(path, ListKind)`, the import's own walk and pruning per kind
# (`usd_import/listing.rs`) — keep the two walks agreeing when either changes.
# `ls --json` adds each prim's values (`Scene::list_usd_records`, read through the
# import's own readers), `diff A B` compares two EXRs (`crust_core::compare`).

# CLI flags: -i/--input, -o/--output (default output.exr), -l/--level (log level),
# --log-file [DIR] (tee the log to crust-<UTC stamp>.log), --scanline
#   (row order instead of the default 16x16 tiles; -b/--bucket is accepted and ignored),
# -s/--samples (override spp), -f/--frame (USD time code to evaluate the stage at),
# --region X0,Y0,X1,Y1 (render a crop: pixels, top-left origin, X1/Y1 excluded;
#   overrides dataWindowNDC; clap refuses a malformed one before the stage is read,
#   one outside the frame errors after import naming the resolution; the EXR keeps
#   the frame as display window with the crop as data window, the PNG is the crop),
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
# --stats-json PATH|- (the same statistics as crust-stats/1 JSON; `-` moves the log to stderr),
# --auto-tx (convert UV textures to a .tx beside the original on first use)

# Keep a full record of a render. The file gets the same events as the terminal
# at the same -l level, so DEBUG has to be asked for; bare --log-file writes
# into the working directory, and a directory argument is created if missing.
cargo run --release -- render -i samples/cornellbox.usda -l debug --log-file renders/logs

# Render one frame of an animated stage (time samples resolve at that code,
# interpolated; unanimated attributes read their default). Without -f every
# attribute reads its *default* value -- not frame 0.
cargo run --release -- render -i samples/animation.usda -f 5 -o frame.0005.exr

# Crop a frame: only the pixels of the rectangle are traced, each bit-identical to
# the full render's at -s 16 (the rendering design record's "Render regions" says
# when it is not). Check placement with crust diff against a full render, or in Nuke.
cargo run --release -- render -i samples/cornellbox.usda -s 16 --region 100,50,164,114 -o crop.exr

# The same figures for a script: crust-stats/1, one JSON object on stdout (the log,
# the progress bar and any text report go to stderr). Add --stats for both forms.
cargo run --release -- render -i scene.usda --stats-json - > stats.json

# What a stage holds, with the values a render reads for each prim (crust-ls/1):
# lens and the camera a render goes through, light inputs as authored, each
# material's surface shader and whether anything renders bound to it. -f
# evaluates the values at a time code; the paths never depend on it.
cargo run --release -- ls camera -i scene.usda --json -
cargo run --release -- ls light -i scene.usda -f 1004 --json -

# Where did the time and memory actually go? (parse vs build vs render vs output)
cargo run --release -- render -i samples/curves.usda --stats

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
CRUST_TRI_PACKETS=gathered target/release/crust render -i /tmp/subdiv_stress.usda --stats -l error
CRUST_TRI_PACKETS=indexed  target/release/crust render -i /tmp/subdiv_stress.usda --stats -l error
# ...and inside the render: Trace vs EvalBsdfs vs Texture vs SurfaceLighting,
# flat / by category / by execution tree. Costs render time (~15-20%, printed
# with the report), so never take a Render time from a --profile run.
cargo run --release -- render -i samples/materialx_basic.usda --profile

# Subsurface random walks: --stats adds how many walks ran, the share that
# found an exit (the rest were absorbed or leaked out of an open mesh), their
# mean length in free flights, and the ray queries they cost ("subsurface walk
# rays", counted into the total but not into "bounce rays").
cargo run --release -- render -i samples/materialx_subsurface.usda --stats

# Cutouts: --stats adds the closest-hit queries they cost ("cutout rays": the
# query past every hit a path passed through, plus every query of a blocked
# shadow ray re-walked through cutouts) and how many hits were passed through.
# A scene with no cutout material prints neither and runs the old code.
cargo run --release -- render -i samples/materialx_cutout.usda --stats
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
# Did the image change? Exit 0 identical, 1 differs, 2 unreadable. Reads both EXRs'
# crust:* sampling stamps and notes on stderr (or in --json's `comparability`) when
# the pixels cannot be compared: adaptive sampling on, different clamps, frames,
# cameras, filters. The notes never change the exit status.
cargo run --release -- diff a.exr b.exr                                  # did the image change? exit 0 / 1 / 2
cargo run --release -- diff ref.exr test.exr --json - | jq .beauty.relmse_trimmed
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
CRUST_TEX_STREAM=0 cargo run --release -- render -i samples/materialx_emissive.usda -o ldr.exr -s 32
cargo run --release -- render -i samples/materialx_emissive.usda -o hdr.exr -s 32
cargo run --release -- diff ldr.exr hdr.exr

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
cargo run --release -- render -i scene.usda --auto-tx
CRUST_TEX_CACHE_MB=256 cargo run --release -- render -i scene.usda --stats

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
CRUST_PTEX_MAX_LOG2=5 cargo run --release -- render -i samples/ptex_quads.usda -o a.exr -s 16
CRUST_PTEX_MAX_LOG2=5 CRUST_PTEX_STREAM=1 CRUST_PTEX_STREAM_MIN_MB=0 \
    CRUST_PTEX_STREAM_MIPSPACE=file \
    cargo run --release -- render -i samples/ptex_quads.usda -o b.exr -s 16
cargo run --release -- diff a.exr b.exr   # 0 pixels
# The configuration that streams under the *default* policy: no pyramid, so no
# chain to reduce in the wrong space. Exact and uncapped, at the cost of the
# pyramid's anti-aliasing.
CRUST_PTEX_STREAM=1 CRUST_PTEX_MIP=0 cargo run --release -- render -i scene.usda --stats
CRUST_PTEX_STREAM=1 CRUST_PTEX_CACHE_MB=64 CRUST_PTEX_STREAM_MIPSPACE=file \
    cargo run --release -- render -i scene.usda --stats

# --- Diagnose a scene: which *settings* make it faster or cleaner ----------
# (openspec/specs/diagnostics/design.md). Markdown on stdout, the
# crust-diagnostic/1 JSON at --json, log on stderr; exit 0 = tier 1 done,
# 3 = budget ran out first (reports still written). Takes render's scene flags.
cargo run --release -- diagnostic -i samples/cornellbox.usda --budget 30s > report.md
cargo run --release -- diagnostic -i scene.usda --region 0,0,512,512   # one crop, that region
# The loop: apply a suggestion as a flag, compare with the previous report.
cargo run --release -- diagnostic -i scene.usda --light-selection learned \
    --baseline crust-diagnostic.json --json r2.json > r2.md
cargo run --release -- diagnostic -i scene.usda --repeats 5 --budget 10m  # busy machine
cargo run --release -- diagnostic -i scene.usda -l debug 2> diag.log      # per-trial lines
# For a *code* change, bench_ab.sh below stays the tool: this one varies settings.

# --- Check a stage before rendering it: one import, no render --------------
# What the render would use (camera, resolution, products and channels, effective
# settings), the import's cost, the diagnostic's import-only findings and the import's
# coded warnings. Text on stdout, log on stderr; crust-check/1 with --json PATH|-.
# Exit 0, or 3 when --deny matched a warning kind (reports still written).
cargo run --release -- check -i samples/cornellbox.usda
cargo run --release -- check -i scene.usda --json - | jq '.warnings[] | [.code, .count]'
cargo run --release -- check -i scene.usda --deny refused,skipped   # a pipeline gate

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
A render logs to stdout, as it always has; `crust ls`, `crust diff` and `crust diagnostic`
log to stderr, because their stdout is their result and is read by scripts. One flag moves
a render's log: `--stats-json -`. `main` picks the stream (`logging::Terminal`) before
the subscriber is built, from the parsed command, so the decision is made once and the
whole run — the log, the `--stats` text report, the progress bar (always stderr) — lands
on stderr, leaving stdout one JSON object. `--stats-json PATH` changes nothing about the
streams.

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
  When the cause can repeat per lookup, the WARN is bounded by what failed, not by how
  often: an unreadable streamed `.tx` is named once per file, and the render ends with
  one WARN counting the tile reads that used a fallback.

**Import warnings carry a code.** Every WARN the USD import raises goes through
`crust_core::warning!` (`crust-core/src/warnings.rs`) with a `WarningCode`, and the macro
writes the code into the message itself — `[light.degenerate_shape] RectLight at …` — so
every subscriber shows it, `--log-file` included, and `logging.rs` is unchanged. One code
per *cause*; each has one kind (`refused` / `approximated` / `skipped`) and one log
policy. `Each` logs every occurrence. `Once` logs the first occurrence in an import and
then only counts — it replaced the `cage_warned` / `legacy_warned` / `ptex_cage_warned`
booleans in `mesh.rs`, whose "(and possibly others)" is now "(further occurrences are
counted in the import's warnings)" — and, outside an import, logs once per thread.
While `load_scene`'s `WarningScope` lives, every occurrence is also recorded: one
`Warning` per code (count, the first 16 distinct prims, the first message) on
`Scene::warnings`. An asset that fails is explained once by the loader
(`cause_warning!`, which logs and supplies the message without counting) and counted per
referencing material or light by the core (`record_warning!`, which counts without
logging). Environment, render-time and diagnostic-analysis warnings keep plain `warn!`.

Adding an import warning means adding a code to the table in `warnings.rs` and a row to
`site/content/docs/reference/warnings.md`: `the_reference_page_lists_every_code` fails
until both agree, kinds included. Raise it on the importing thread (see
`docs/architecture.md` § Invariants).

The practical consequence when adding a log: if you can write a stage that makes your new
line print a thousand times, it is `DEBUG`. Nothing is logged per ray, per pixel or per
sample — the finest granularity in the engine is per *pass* (`render_pass`), which is one
line on an ordinary render and a handful on a guided one.

`--log-file` tees the same stream to `crust-<YYYYMMDDTHHMMSSZ>.log`. Four
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

## Machine-readable reports

`--stats-json`, `ls --json` and `diff --json` share the `cli` spec's rule ("Machine-readable
reports share one shape"), implemented once in `crust_core::report`:

- **`Report<T>`** is the envelope: `format` and `crust_version`, then the body
  `#[serde(flatten)]`ed. serde writes a struct's fields in declaration order, so the
  envelope leads without `preserve_order`. `crust-diagnostic/1` predates it and spells the
  two fields out itself; same keys, same place.
- **`finite_or_null`** maps NaN and infinities to `null` (serde_json refuses them) and
  writes an `f32` at `f32` precision — through the `f64` widening, a focal length of 12.7
  printed as `12.700016975402832`. **`seconds`** writes a `Duration` as `f64` seconds for
  a `*_s` key.
- **The report types are the contract.** `RenderStats` serializes through a view that
  *destructures* it, so a field added to it is a compile error until it has a key, and
  `stats_json_key_set_is_pinned` lists all 150-odd keys: a new one fails the test until
  its name is reviewed. The kernel's `MemoryFootprint` is crust-rt's (serde-free), so it is
  destructured into `*_bytes` keys the same way. The ratios the text report derives
  (`total_rays`, `mean_path_length`, `rr_kill_rate`, `hit_rate`, `micro_rate`,
  `rays_per_s`) are computed in the view, once, zero guards included.
- **One snapshot of the peak RSS.** `RenderStats::peak_memory_bytes` is taken by the host
  after the outputs are written, and both `Display` and the JSON read the field: the text
  report used to call `peak_memory_bytes()` while formatting, which a second form would
  have read at another moment.

`ls --json` reads values through the import's own readers (`camera_lens`,
`light_inputs`, `bound_material`, `surface_shader_id`, made `pub(super)`), never a copy,
and the camera choice through the import's (`settings::wanted_camera`, `pick_camera`), so
`is_render_camera` is the camera the import resolves — checked against `Scene::camera_path`
in `usd_listing.rs`. The import's fallback is the first camera *its* walk meets, which pops
children last-first; the listing prints in authored order, so it walks the cameras a second
time in the import's order to find that one. Text `ls` stays the old walk, byte-identical,
and pays for none of this. `material`'s `bound` resolves every geometry prim's binding (a
second walk); measured `ls material --json` against text `ls` at 1.03× on
`PointInstancedMedCity.usd`, 0.93× on the Cornell box and 1.12× on `materialx_showcase`
(min of 5). ALab, where the binding walk would matter, was not available to measure.

`crust check` writes **`crust-check/1`** (`crust_core::check::CheckReport`), built
from pieces other reports already own, so it adds no vocabulary:
`format`, `crust_version`; `scene` (the diagnostic's `SceneInfo`: `path`, `frame`,
`camera` — the camera the import resolved, after any fallback — `resolution`,
`region`); `products[]` (`prim` — `null` for the default beauty — `file`, `channels`);
`effective_settings[]` (`name`, `value`, `flag`, `usd_attribute`, from the one
`diagnostic::effective_settings`); `import[]` (`crust-stats/1`'s phase objects:
`name`, `depth`, `time_s`, `rss_end_bytes`, `peak_end_bytes`); `counts`
(`crust-stats/1`'s `scene` object, `geometries` / `top_level` / `unique` / `footprint`
/ `lights` / `volumes`); `findings[]` (the diagnostic's `id`, `kind`, `summary`,
`evidence`, `action`); `warnings[]` (the import's records: `code`, `kind`, `count`,
`prims`, `message`); `denied` (codes). `check_json_keys_are_pinned` pins the order and
these paths. Findings come from `checks::run(&Facts::from_import(..))`, the same import
facts the diagnostic builds its own `Facts` on (it adds the baseline's with `..`), so the
two cannot disagree on an import-only finding
(`import_only_findings_match_the_diagnostics`). Products go through the render's own
`select_products` (which applies `refuse_shared_paths`) and `product_channels`, and the
default beauty path through `beauty_output`, which `render` calls too.

**Known gap: products refused by the CLI are not warning records.** A product with no
`productName`, no writable var, or a path another product already claims is dropped by
`select_products` / `refuse_shared_paths` with a plain `warn!` on stderr: that happens
in the CLI, after the import's `WarningScope` has closed, so it is absent from
`products` but has no code and no record, and `--deny` cannot see it. Coding it needs a
CLI-side scope.

Comparability's adaptive note reads the tracer's own rule (`tracer::samples_adaptively`,
over the `adaptive_check_points` `render_pass` uses), not `spp > minSpp`: with the
threshold at 0, or a budget that never passes the first check point, a render took a
fixed budget, and the first version of the rule warned about it anyway.

`diff` splits as a render does: crust-assets decodes (`read_exr_planes`, the old example's
`load` with errors for panics), crust-core compares (`compare`, no I/O, so the diagnostic
can call it on buffers), the CLI prints and maps the exit status. Its text report is the
old diff example's, line for line — checked on eight fixture pairs (identical,
4 vs 16 spp both ways, a crop against the frame, AOV-only products, a beauty product
against a single beauty) before the example was deleted.

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
  tiles, the peak open `.tx` files against `CRUST_TEX_MAX_OPEN_FILES` and the reopens
  it cost, plus preloaded UV textures), Ptex, then phases by execution tree and by
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
