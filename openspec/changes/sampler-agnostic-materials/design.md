## Context

See proposal.md for the motivation and the spike's numbers. This section covers the code as it
stands.

- `crust_core::PathSampler = openqmc::SobolSampler` (`lib.rs:68`). openqmc's
  `Sampler<T: SamplerImpl>` is a `Copy` wrapper. For Sobol, `T` is an 8-byte `State64Bit`. The
  `*Bn` and PMJ impls also carry one or two `&'static` table slices (24–40 bytes).
- The sampler type appears in the integrator (`tracer/path.rs`, about 25 functions,
  `tracer/mod.rs::render_pixel`), in `subsurface::random_walk`, in `light_cache.rs`, and in the
  material layer. `volume.rs` and `guiding/` take a `pcg::Rng` and do not depend on the sampler
  type.
- **The `dyn` boundary is narrow.** The integrator samples a BSDF through `ShadingPoint`
  (`material/material.rs:435`), a concrete struct over `enum Resolved { Material(&dyn Material),
  Plain(&OpenPBR), OpenPBR(..), Closure(..) }`. Only the `Material` arm makes a virtual call. In
  a render it is reached only by `Emissive` (draws nothing) and the subsurface `ExitLambertian`
  (draws 2). The `dyn` impls on `OpenPBR`, `MtlxMaterial` and pattern materials are reached
  only by tests and probes.
- **What materials draw.** One `draw_sample_f32::<4>()` in `OpenPBR::scatter_with`
  (`openpbr/mod.rs:534`) and `ResolvedClosure::scatter_choosing` (`closure/mod.rs:928`). One
  lazy `<2>` in `ResolvedClosure::straight_transmittance` (`closure/mod.rs:570`, only for a
  thin-walled transmitting leaf). One `<2>` in `ExitLambertian`. No material derives a
  sub-domain or uses `draw_rnd` / `rng()`. The tracer hands each one `vertex.new_domain(K_BSDF)`.
- **The draw prefix property.** openqmc computes a whole 4D block per draw, and
  `draw_sample::<N>` returns its first N values. So `<2>` equals the first two values of `<4>`
  from the same domain. Moving a draw out of a material and into its caller is therefore exact.
- **The integrator is already monomorphised.** `trace_path<PROFILE, AOV, MEDIA, TWINS>` is
  dispatched per pixel by `advance_dispatch` (`tracer/mod.rs:1488`), with `AOV` fixed per pass.
  That is up to 16 instantiations. Several `#[inline(always)]` / `#[inline(never)]`
  annotations on this path carry measured costs (0.3–0.5% of cornellbox's instructions each).

## Goals / Non-Goals

**Goals:**

- No material or BSDF type mentions a sampler type.
- Pick the sampler at run time with no per-draw dispatch and no change to the default
  render's instruction count beyond callgrind noise (≤ 0.2% on cornellbox, 2 spp).
- `sobol` stays bit-identical to today's renderer (specs: rendering, "The default sampler
  leaves renders unchanged").

**Non-Goals:**

- Choosing a better default. The spike found none.
- A `crust diagnostic` trial over samplers. This change makes that possible; adding it is a
  follow-up.
- Fixing the blue-noise samplers' weak spectral effect, or their seams at 256-pixel tile
  boundaries (see Risks).
- More than four stratified dimensions per BSDF sample. `BsdfDraw` leaves room for it (D1)
  but adds none.

## Decisions

### D1 — Materials take numbers, in two shapes

- **Concrete arms** (`OpenPBR::scatter_with`, `ResolvedClosure::scatter_choosing` /
  `straight_transmittance`, and their `scatter_split` twins) take
  `draw: impl FnOnce() -> [f32; 4]` and call it at exactly the point that draws today.
  `ShadingPoint::{scatter_importance, scatter_split, straight_transmittance}` are generic over
  the closure. The closure inlines away, so the code generated for these paths is today's.
  Nothing is drawn where nothing is drawn now: the back-facing early return in `scatter_with`
  and the no-thin-leaf case in `straight_transmittance`.
- **The `dyn Material` arm** takes `BsdfDraw<'_>`, a concrete, object-safe lazy handle:
  `enum BsdfDraw<'a> { Lazy(&'a dyn Fn() -> [f32; 4]), Fixed([f32; 4]) }` with
  `fn u(&self) -> [f32; 4]`. The integrator passes `Lazy`, and tests pass `Fixed` with literal
  uniforms (a borrowed closure could not own them). `Emissive` never calls `u()`, so it draws
  nothing. The branch in `u()` is on the cold `dyn` arm only. A later need for unbounded
  numbers adds a `fn rng(&self) -> pcg::Rng` method. That is not a breaking change, and
  nothing pays for it until it exists.

Alternatives considered:

- *Eager `[f32; 4]` everywhere.* Simplest, but every emitter hit, back-facing return and
  thin-wall query would draw a block it does not use. The `rendering` design record measured
  one unnecessary block per camera ray at 4.2% of cornellbox.
- *`Material<S: SamplerImpl>` / generic trait methods.* Not object-safe, and it would multiply
  every material's code by the sampler count.
- *An enum wrapping the six samplers.* Grows the by-value sampler from 8 to about 48 bytes
  through about 25 integrator functions, and adds a branch per draw. The default would pay for
  it.
- *`&dyn Fn` on the concrete arms too.* One indirect call per bounce on the hottest path, while
  the generic closure costs nothing.

### D2 — The integrator is generic over `S: SamplerImpl`

Every integrator function that takes `PathSampler` takes `Sampler<S>`, and `PathContext`
gains no field. The domain keys (`K_CAMERA`, `K_BSDF`, `K_NEE`, …) and the `new_domain(tile)`
decorrelation are unchanged. The BSDF call sites become
`sp.scatter_importance(r, || bsdf_dom.draw_sample_f32::<4>())`. `subsurface::random_walk` is
generic the same way. `PathSampler` remains as the name of the default
(`Sampler<SobolImpl>`) for the light cache and tests.

### D3 — Dispatch once per pass, outside the per-pixel match

`render_pass` matches on the render's `SamplerKind` once and calls a generic
`render_pass_with::<S>()` holding today's body. The per-pixel `advance_dispatch` match on
`(PROFILE, MEDIA, TWINS)` is unchanged inside each instantiation, so there is still no
per-pixel branch on the sampler. `Sampler::<S>::warm_cache()` runs before the pass's timer
starts, so building the PMJ table (2^16 entries) or decoding the BN tables never counts as
render time in `--stats` or `bench_ab.sh`.

### D4 — Code size and compile time are measured and gated

Six samplers × up to 16 flag combinations means up to 96 `trace_path` instantiations. After
the integrator is generic, measure on the CI image: clean `cargo build --release` time and the
`crust` binary size, before and after. Accept up to **+40% build time** and **+25% binary
size**. Beyond either, stop and cut the shipped set to `sobol`, `sobolBn`, `lattice`, `pmj`
(the base sequences plus the one blue-noise variant the spike measured), amending the
`rendering` delta's value list before continuing. Record the measured numbers in the
`rendering` design record either way.

### D5 — The learned-selection pre-pass stays on Sobol

`light_cache.rs` keeps the concrete `PathSampler`. The pre-pass is a training estimate, not an
image sample. Keeping it fixed makes the learned table, and with it the light picks, the same
under every sampler, so a sampler comparison measures the sampler alone. Path-guiding training
passes do use the selected sampler, because they render image samples and blend them into the
final image.

### D6 — The setting follows `crust:pixelFilter`'s plumbing

- `SamplerKind { Sobol, SobolBn, Lattice, LatticeBn, Pmj, PmjBn }` with `CHOICES`, `FromStr`
  and `name()`, as `PixelFilter` has.
- `RenderSettings::{with_sampler, sampler}`.
- `custom_token(prim, "crust:sampler")` in `usd_import/settings.rs`, with
  `SettingsInvalidValue` on a parse error (no new `WarningCode`).
- `--sampler` with `choices(SamplerKind::CHOICES)` on render, check and diagnostic.
- `crust:sampler` in the stamp (`stamp.rs`).
- A stamp key in `compare.rs`. A missing stamp key is read as `sobol`.
- An effective-settings row in `diagnostic/mod.rs`, which `crust check` shares.

Token spelling is camelCase (`sobolBn`), matching how USD tokens name compound words.

This is not a `CRUST_*` environment switch. Those exist only to A/B an optimisation against the
behaviour it replaced (CLAUDE.md), and this is a user-facing quality setting.

### D7 — Proving the default unchanged

Three measurements, all taken before any non-Sobol sampler is reachable:

1. Goldens recorded from `main` with `scripts/check_images.sh record` (16 spp,
   `--indirect-clamp 0`), checked after every step.
2. `callgrind` on cornellbox at 2 spp, one thread: total and `render_pixel`-inclusive
   instructions within 0.2% of `main`.
3. `bench_ab.sh` against a `main` binary on cornellbox, veach_mis, materialx_basic and
   openpbr_showcase, reporting min and mean.

## Risks / Trade-offs

- [Inlining decisions drift as instantiations multiply. The annotated helpers' measured costs
  were taken with one sampler.] → D7's callgrind gate runs after each step. Any drift is fixed
  with the same `inline` annotations, measured the same way.
- [Compile time and binary size grow roughly with the sampler count.] → D4's gate and its
  pre-decided fallback.
- [The blue-noise samplers cost 3–10% wall time (table reads) for a small low-spp gain.] → The
  user docs state the spike's numbers and recommend them only for low-spp previews and
  denoised output.
- [Lattice is better on some scenes and worse on others (−9% / +20% relMSE).] → The docs say
  so. A diagnostic trial (non-goal) is the follow-up that would choose per scene.
- [`.new_domain(tile)` shifts the `*Bn` table per 256-pixel tile, so the blue-noise pattern
  may show seams at tile boundaries.] → Recorded as a known gap in the `rendering` design
  record. Not fixed here.
- [Test churn: 24 `PathSampler::new(0, 0, 0, i)` test sites.] → Mechanical. Where a test
  searches sample indices for a lobe, it is rewritten with exact uniforms, which makes it
  stricter.
- [`Material` is a public trait.] → Every implementer is inside `crust-core`. The change is
  noted as breaking in the crate's API, with no in-tree consumer to migrate.

## Migration Plan

There is no data migration. The default is unchanged, and old EXRs without `crust:sampler`
compare as `sobol`. The steps are ordered so `main` stays shippable after each one:
(1) the material boundary with Sobol only, (2) a generic integrator with Sobol only, (3) the
setting and dispatch. Steps 1 and 2 each pass D7. Rollback is a revert of step 3, which
removes the choice and leaves the cleaner boundary.
