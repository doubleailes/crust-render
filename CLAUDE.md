# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

It is the contributor guide: what to run, the rules that keep the renderer correct, and
where the rest lives. It is deliberately short. Read `docs/architecture.md` first for the
map (crates, render flow, seams, every `CRUST_*` switch, the technical-debt register).
The long-form design record — why each feature is built the way it is, the measurements
that forced it, its history and its known gaps — is in `openspec/specs/<capability>/design.md`,
beside the behavioural `spec.md` for the same capability.

## What this is

Crust Render is a toy, physically-based path tracer written in safe Rust (edition 2024),
inspired by PBRT, *Ray Tracing in One Weekend*, and Autodesk Standard Surface / OpenPBR.
Scenes are loaded exclusively from **USD** (`.usda` / `.usdc` / `.usdz`) via the pure-Rust
[`openusd`](https://github.com/mxpv/openusd) crate (0.7, typed schemas in
`openusd-schemas`). Do not add loaders for any other scene or mesh format.

## Where things are documented

| topic | design record (`openspec/specs/…/design.md`) | also |
|-------|-----------------------------------------------|------|
| CLI, command cookbook (every probe / A/B recipe), logging, `--stats` / `--profile`, env switches | `cli` | `docs/architecture.md` § Environment switches |
| integrator, MIS, Russian roulette, media, volumes, guiding, adaptive sampling, pixel filters, openqmc | `rendering` | `docs/light_sampling.md` |
| `crust-rt`: SBVH → BVH4, watertight `Tri4` packets, instancing, SIMD | `intersection-kernel` | `docs/simd.md`, `docs/embree_comparison.md` |
| `Material` / `resolve` / `ShadingPoint`, OpenPBR, MaterialX (`crust-mtlx`, `crust-jit`), `UsdPreviewSurface` | `materials` | `docs/openpbr_reference_alignment.md`, `docs/shading_performance.md`, `docs/material_fidelity.md` |
| lights, light selection (`power` / `uniform` / `learned`), UsdLux units, shaping, IES | `lighting` | `docs/light_sampling.md` |
| UV / UDIM textures, `.tx` streaming, Ptex (preload and streaming), ray-cone filtering | `textures` | `docs/ptex_streaming.md`, `docs/color_management.md` |
| AOVs: `RenderProduct` / `RenderVar` resolution, the source vocabulary and its aliases, accumulation modes, light path expressions and their routing, albedo, light groups, the per-product EXR writer | `aovs` | `site/content/docs/usd/aovs.md` |
| USD import: streaming import, schema mapping, subdivision, displacement, instancing, time, camera, settings, Moana, ALab | `usd-scene-import` | `docs/alab_profile.md`, `docs/moana_profile.md`, `docs/issues/` |
| `crust diagnostic`: probe conditions, crops, trials and verdicts, the budget, the `crust-diagnostic/1` report, `--baseline`, the clamp counter's cost | `diagnostics` | `site/content/docs/help/diagnosing-a-render.md` |
| `crust mcp`: the session (the override layer is the session), the session thread, edits as USDA merged into the layer, undo, progressive renders, probe / diff, `render_final` | `mcp-session` | `site/content/docs/help/claude-desktop.md` |

Before changing a feature, read its design record: most sections end in a trap that was
already fallen into once. When a change moves a measurement or retires a gap, update the
record rather than appending history here.

## Commands

```bash
cargo run --release -- render -i samples/cornellbox.usda -o out.exr    # EXR + tone-mapped PNG beside it
cargo run --release -- render                                          # no -i: procedural fallback scene
cargo run --release -- render -i scene.usda -f 5 --camera /cam -s 16   # frame, camera, spp override
cargo run --release -- render -i scene.usda --stats                    # per-phase time/memory + scene stats
cargo run --release -- render -i scene.usda --profile                  # + per-section render profile (~15-20% slower)
cargo run --release -- render -i scene.usda -l debug --log-file logs   # tee the log to a timestamped file
cargo run --release -- render --help                                   # every render flag
cargo run --release -- ls camera -i scene.usda                         # cameras (or light, material), one path per line
cargo run --release -- check -i scene.usda --json -                    # what a render would use + the import's warnings (crust-check/1)
cargo run --release -- diagnostic -i scene.usda --budget 2m > r.md     # which settings make it faster/cleaner (+ crust-diagnostic.json)
cargo run --release -- mcp                                             # MCP server on stdio for Claude Desktop (feature `mcp`)

cargo test                                          # integration tests load samples/*.usda
cargo test -p crust-core loads_cornellbox_usda      # one test by name
scripts/test_simd_matrix.sh -p crust-rt             # kernel exactness under every SIMD codegen

# The optimization loop (see "Measuring a change")
scripts/bench_scenes.sh                             # min-of-N Render seconds + Mray/s per scene
scripts/check_images.sh record <dir>                # golden EXRs at 16 spp
scripts/check_images.sh check  <dir>                # re-render and diff; non-zero exit on any change
scripts/bench_ab.sh -a <binA> -b <binB> [scenes...] # interleaved A/B of two binaries
cargo run --release -- diff a.exr b.exr             # did the image change? exit 0/1/2 (+ relmse, --json -)
cargo test -p crust-mtlx --test osl_oracle          # MaterialX nodes vs MaterialX's own OSL implementation
scripts/osl_oracle.py                               # regenerate those values (needs materialx + exact-math OSL)
cargo test -p crust-core --test adobe_oracle        # native OpenPBR vs Adobe's openpbr-bsdf, gap by gap
scripts/adobe_oracle.py                             # regenerate those values (needs a C++17 compiler + network)

# CI (toolchain pinned by rust-toolchain.toml, Cargo.lock committed,
# RUSTFLAGS=-D warnings), four parallel jobs; skipped when a change touches only
# documentation (openspec/, docs/, site/, *.md — the list is in rust.yml):
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
cargo deny --locked check                           # advisories, licences, sources (deny.toml)
# ...and .github/workflows/nightly.yml in parallel: clippy + tests on a pinned nightly
# (NIGHTLY_PINNED; red means this code) and on the latest one (allowed to fail, and
# run daily), plus the nightly-only `bvh8` feature. Reproduce the pinned leg with:
cargo +nightly-2026-09-26 clippy --workspace --all-targets -- -D warnings
cargo +nightly-2026-09-26 test -p crust-rt --features bvh8
```

The probe examples (`mtlx_shade`, `tex_probe`, `ptex_verify`, `ptex_seams`,
`light_occlusion`, `scene_bounds`, `maketx`, the openusd `*_probe`s, `ray_throughput`,
`mtlx_bench`, `jit_bench`) and the texture / Ptex streaming A/B recipes — several of which
need two or three environment variables set together to measure anything — are in the
command cookbook in `openspec/specs/cli/design.md`. Use them: a wrong albedo decode, mip
level, Ptex face id or MaterialX mask all render as something plausible, so this codebase
verifies shading **in numbers, not by eye**.


## Workspace

Seven crates under `crates/` (ownership table in `docs/architecture.md`):
`crust-rt` (intersection kernel, no crust deps), `crust-mtlx` (MaterialX reader, no crust
deps), `crust-jit` (Cranelift JIT for `crust-mtlx` programs, feature `jit`), `crust-core`
(the engine library: import, integrator, materials, lights, volumes, guiding, stats),
`crust-assets` (every file decoder and texture cache, behind `crust_core::AssetLoader`),
`crust-render` (the CLI, binary `crust`; it drives a render, a diagnosis or a comparison and
writes their files — the images, the reports — while the engine writes nothing: `main.rs`,
`products.rs`, `logging.rs`) and `utils` (stateless math:
warps, MIS heuristics, the one Rec.709 `luminance`). `openqmc-rs` (all sampling) and
`opensubdiv-rs` / `ptex-rs` are external. Import from `crust_core::` roots; `lib.rs`
re-exports the public surface.

## Rules

**Code shape**

- Safe Rust everywhere: crates are `forbid(unsafe_code)` except `crust-core` (`deny`, one
  test-only `GlobalAlloc`) and `crust-jit` (`deny`, five audited blocks). Adding `unsafe`,
  or a dependency that carries it on the hot path, is a project decision, not a local one.
- **crust-core decodes no assets.** Every image, Ptex or IES byte crosses `AssetLoader`
  into `crust-assets`; a loader returning `None` means "fall back", never an error.
- `crust-rt` and `crust-mtlx` stay free of crust types. crust-core adopts their
  vocabulary (`crust_rt::Geometry`, `Texture2D = crust_mtlx::Texture`) instead of wrapping
  it — a wrapper adds a vtable hop per texel fetch that LTO cannot remove.
- `scene/usd_import/` siblings share through `pub(super)` and import each other
  explicitly — no `use super::*`, so a file's `use` block is its real dependency list.
- The USD import is single-threaded and reads the time code from a thread-local
  (`EvalTimeScope`); parallelising it means threading the time explicitly.
- No RNG outside `openqmc`: stratified draws through keyed sub-domains (`K_*` in
  `tracer/path.rs`), incidental ones through `draw_rnd` / `domain.rng()`.

**Pairs that must change together.** Most bugs here were one half of a pair changing
without the other (full list: `docs/architecture.md` § Invariants):

- Every NEE weight has a bounce-side twin (`bounce_emission_weight`, `escaped_emission`),
  both routed through `SamplingStrategy` and `LightList::density` / the `*_at` lookups —
  surface NEE ↔ BSDF bounce, volume NEE ↔ `PrevVertex::Phase`, guide mixture ↔ NEE.
  Emission at a bounce-arrival vertex is owned by the previous vertex's record.
- `Emissive::radiance_toward` is the one answer to "what does this light emit toward
  here", for `AreaLight::sample_li` and `Material::emitted_at` alike. A material that
  emits only through `emitted_at` must never become a light-list entry.
- A shape's solid-angle sample and its pdf answer for exactly the same `from`s with
  the same density — structural now: both come from the one
  `LightShape::solid_angle_sampler(from)`. A non-finite density is refused on both
  sides (`PdfSolidAngle::new` → `None`), never replaced by a finite stand-in.
- Light path expression AOVs reuse the beauty's own recurrence, so their pairs are the
  beauty's: `eval_all` ↔ `eval_split`, `scatter_resolved` ↔ `scatter_split` (one
  `scatter_with`), `escaped_emission` ↔ `escaped_split`. `C.*[LO]` is pinned bitwise to the
  beauty, and the zero-AOV render to its instruction count (`docs/architecture.md`).
- Bit-identity pairs, each pinned by a bitwise test: `Tri4` packets ↔ scalar triangles;
  JIT ↔ interpreter; streamed ↔ preloaded `u8` textures; streamed `capped` Ptex ↔
  preloaded Ptex at and below the cap (one `decode_face`, one `reduce_level`); tiles ↔
  scanlines (a render mode is scheduling only); the staged first sweep ↔ one unstaged
  sweep (so is staging); `reduce_half` ↔ `reduce_half_linear` (they share `axis_taps`).
- Anything keyed on a prototype path is scoped by the stage epoch
  (`ImportCaches::epoch`): `/__Prototype_N` is renumbered per masked stage.
- `Material::resolve` must return exactly what per-query shading would (pinned for every
  material by `crust-core/tests/resolve.rs`); build it with `Resolution::new`, which takes
  the emission from the parameters *before* `into_resolved` — the type allows no other order.
- Every colour input states its colour space (`docs/color_management.md`).

**Logging.** The level is decided by whether the line scales with the scene, not by how
interesting it is. `INFO` is bounded per render (a default render prints four lines);
anything whose count grows with the input (per prim, material, texture, chunk, pass) is
`DEBUG`; `WARN` means something authored was refused, approximated or skipped. An
import-time WARN goes through `crust_core::warning!` with a `WarningCode` (one per cause,
listed in `site/content/docs/reference/warnings.md`, kept in step by a test), raised on the
importing thread so it lands on `Scene::warnings`. Nothing is logged per ray, pixel or
sample. `--stats` output is an event on `STATS_TARGET`, which
`-l` cannot silence. Details: `openspec/specs/cli/design.md` § Logging.

**Environment switches** exist to A/B an optimization against the behaviour it replaced.
Adding one: a field on `crust_core::Config` (`crust-core/src/config.rs`, the only place
the environment is read), a row in `docs/architecture.md` § Environment switches, a
section in the user documentation (`site/`, below), and make the "off" side the old
behaviour so the A/B is honest.

**User documentation** is the Zola + AdiDoks site in `site/` (theme is a git submodule;
`.github/workflows/docs.yml` builds it on PRs and publishes it to `gh-pages`). It
describes every CLI flag (`reference/command-line.md`), every `CRUST_*` switch
(`reference/environment-variables.md`), every `crust:*` USD attribute (`usd/`), and the
architecture, design choices and limitations for users (`architecture/`). A
change that adds, removes or changes the default of any of those updates its page in the
same change; so does one that retires a limitation or changes a design choice listed
there. Build it with Zola 0.21 (`zola build` in `site/`; 0.23 cannot parse the
theme), which also checks every internal link and anchor.

**Unbiased measurements** need `--indirect-clamp 0`: the default firefly clamp (10) is the
one biased setting. Record goldens with the same setting on both sides of an A/B.

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
       target/release/crust render -i samples/cornellbox.usda -o /tmp/x.exr -s 2
   callgrind_annotate --inclusive=no callgrind.out.<pid>
   ```

   Function names resolve from the symbol table with no debug info; add
   `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` only when you want line-level detail
   (not `RUSTFLAGS='-C debuginfo=…'`: the release profile's default `strip = "debuginfo"`
   removes it, and callgrind silently attributes every line to `???`).
   Note callgrind counts instructions, not cycles, so it under-reports anything whose gain
   is cache behaviour — and it cannot see `panic = "abort"` at all.
3. **Compare images at `-s 16`, never higher.** `min_samples_per_pixel` defaults to 32 and
   the adaptive early-stop needs `taken >= min_spp`, so at 16 spp every pixel takes exactly
   16 samples. Above that a single-ulp difference changes a pixel's sample budget and
   cascades, making a bit-identical change look structural. `check_images.sh` pins this.

For a change that *does* legitimately alter output (a different BVH reorders exact ties),
prove it is noise rather than bias by checking the difference falls as 1/√N across spp
rather than plateauing.

## Known gaps

Documented rather than silent: each capability's `design.md` ends with its "Known gaps"
sections (geometry and SIMD limits in `intersection-kernel`; MaterialX reduction limits in
`materials`; residency, filtering and UV limits in `textures`; unsupported UsdLux features
and light-sampling noise in `lighting`; instancing, openusd workarounds and ALab gaps in
`usd-scene-import`; guiding and volume limits in `rendering`). Specs in
`openspec/specs/*/spec.md` describe current behaviour; propose behavioural changes through
`openspec/changes/`.
