# Design

## Context

See `proposal.md` for the motivation. What exists today:

- `crust-mtlx` compiles a document into a closure arena (`Leaf`, `Layer`,
  `Mix`, `Add`, `Multiply`, `Empty`) whose parameters are program slots. This
  change leaves it alone.
- `crust-core/src/material/closure/mod.rs` collapses the tree at each vertex
  (`ResolvedClosure::resolve` → `walk`). Each call returns the subtree's
  GLSL throughput `T`. A leaf gives `1 − E·w` if it lets light through and 0 if
  it is opaque. `Multiply` passes `T` through, and `Layer` multiplies `T_top`
  by `T_base`. `prepare` builds each leaf from `closure/mx.rs` (genglsl ports)
  and from `bsdl_tables.rs` (BSDL's `DielectricReflFront`, built from Typhoon's
  vendored copy).
- `adobe_oracle` is the precedent for a reference check. A Python script
  builds a C++ probe against pinned sources and writes a text fixture. A Rust
  test replays it, with named `Deviation` rules that fail when stale.

The upstream sources read for this design, both cloned at their heads on
2026-10-22: OSL `cc6f9b1` (`src/libbsdl`, `src/testrender/shading.cpp`) and
MaterialX `dcb61fe` (`libraries/pbrlib/genosl`).

## Goals / Non-Goals

**Goals:**

- The `layer` opacity is testrender's, exactly, including its sum over a
  nested layer.
- Each leaf's value, albedo and opacity filter are BSDL's, checked against a
  fixture BSDL itself produced.
- Every table comes from upstream OSL at one pinned commit, through one script.

**Non-Goals:**

- Matching testrender's images pixel for pixel. Its path-roughness
  regularization, its light sampling and its subsurface Lambertian all differ,
  on purpose.
- Matching BSDL's sampling densities. crust's pdf only has to agree with
  crust's sampler, which the existing histogram tests already check.
- Spectral rendering and `dispersion`: testrender runs BSDL with
  `lambda_0 = 0` (RGB), and so does crust.

## Decisions

### 1. Opacity is a second quantity carried up the walk

`walk` keeps passing the response weight down as it does today, and returns an
**opacity** `O` (RGB, unclamped) instead of a throughput:

```
 Leaf      O = own · (1 − filter_o(ωo))   see-through: R-only dielectric/Schlick, sheen
           O = own                        everything else (filter 0)
 Mix       O = m·O_fg + (1 − m)·O_bg
 Add       O = O_a + O_b
 Multiply  O = k · O_in
 Layer     base walked with weight · (1 − clamp(O_top, 0, 1));  O = O_top + O_base
 Empty     O = 0
```

The `Layer` row is testrender's `evaluate_layer_opacity` read in one pass: it
pushes the base of a nested layer with the branch's unscaled weight and adds
both opacities. Two consequences follow from that structure:

- The walk computes `O_base` for every layer, even though only a layer inside
  a top reads it. That costs one more add per layer and avoids a second walk.
- A pruned branch (`Empty`) has opacity 0. That is the same as today's
  throughput of 1, so the DPEL "transmission dummy" (`mix(diffuse, dummy, mask)`)
  still gives a coverage of `mask`.

*Alternative considered:* keep the product `T_top · T_base` for a nested layer.
It is more physical, but the user chose to match testrender exactly. Two
counterweights: no built-in graph nests a layer in a top, and the scenario in
the spec pins the sum so it cannot drift silently.

### 2. Leaves are ported from BSDL `mtx::`, one module, one leaf at a time

A new `closure/bsdl.rs` transcribes `mtx::DielectricLobe`, `ConductorLobe`,
`SchlickLobe`, `SheenLobe`, `OrenNayarDiffuseLobe`, `BurleyDiffuseLobe`,
`TranslucentLobe` and the helpers they use: `DielectricFresnel`,
`ConductorFresnel`, `GGXDist`, `TabulatedEnergyCurve` and
`eval_turquin_microms_reflection`. As in `mx.rs`, functions keep BSDL's names,
so each one can be diffed against its source. Specific points:

- **Roughness parameterisation.** BSDL indexes its tables with
  `r = sqrt(ax / (1 + aniso))`, where `aniso = (1 − ay/ax)/(1 + ay/ax)`. GGX is
  then built from `(r, aniso)`, not from the raw `(ax, ay)`. crust uses
  BSDL's construction, so the table index and the distribution agree. The
  documented rule that "a specular leaf's `roughness` is GGX alpha, taken as
  authored" still holds: BSDL also takes the authored value as alpha.
- **Multiple scattering.** Reflection-only lobes use Turquin's compensation
  with `F(cos θ_mo)` and a scalar `E_ms` from the table:
  `GGX` (`MiniMicrofacetGGX`) for a conductor, `DielectricReflFront` for a
  dielectric or Schlick. Refracting lobes are renormalised by
  `1 / max(0.01, 1 − E_ms)` from `DielectricBothFront` / `BothBack`, chosen by
  face.
- **Sheen.** `conty_kulla` uses BSDL's `ContyKullaSheen` table. `zeltner` uses
  BSDL's `ZeltnerBurleySheen`, which uses the same LTC fit as crust's current
  table (`ltc_sheen_table.rs`). If the two match bit for bit, crust keeps one
  table; if not, BSDL's replaces it (open question 1).
- **Diffuse.** BSDL's Oren–Nayar lobe contains the EON energy compensation
  behind its `energy_compensation` flag, and Burley is BSDL's.
- **Where `mx.rs` remains.** It keeps the Airy thin film (BSDL's `mtx::`
  lobes declare `thinfilm_*` and never apply it) and the hair helpers. The
  rest of `mx.rs` is deleted once every leaf has moved, so no leaf can quietly
  stay on genglsl.

*Alternative considered:* adopt BSDL's tables but keep genglsl's lobe shapes.
That leaves value and albedo out of step: the table describes BSDL's lobe,
which `design.md` already records as a measured gap for the dielectric. It
gives up the oracle's point.

### 3. Tables are regenerated from upstream, through one script

`scripts/tables/bsdl_luts_to_rust.py` changes in three ways:

- It fetches OSL at the pinned commit instead of reading Typhoon's copy.
- It runs `genluts`.
- It converts every table the ported leaves use, not only `ReflFront`: `MTX/`
  dielectric reflfront, bothfront and bothback, contysheen and zeltnersheen,
  and `SPI/` microfacet_tools.

The first gate in the task list is that the regenerated `ReflFront` equals
today's `bsdl_tables.rs`. If the files differ, the diff is explained before
anything else moves, because it would mean upstream changed BSDL since
Typhoon's snapshot.

### 4. The oracle compares values, albedos and opacity filters

`scripts/bsdl_oracle.py` builds `scripts/bsdl_oracle/probe.cpp` against the
pinned OSL `src/libbsdl` plus Imath, with no OSL runtime. It mirrors
`adobe_oracle.py`. The probe writes `crates/crust-core/tests/data/bsdl_oracle.txt`
with one case per (leaf, parameters, ωo):

- the value `f·|cos θ_i|` toward a fixed set of `wi`, both hemispheres for
  refracting lobes. The value is `Sample::weight · pdf`, which does not depend
  on BSDL's sampler;
- the directional albedo over the same 32 × 32 cosine grid `adobe_oracle`
  uses;
- `filter_o(ωo)` and `albedo_impl()`.

`tests/bsdl_oracle.rs` replays the cases through crust's leaves. It has the
same `Deviation` mechanism as `adobe_oracle.rs`: a named condition, a bound,
and a failure when the rule no longer excuses anything. The rules expected
from the start are the thin film and the GGX alpha floor (`MIN_ALPHA` 1e-4
against BSDL's `EPSILON`). Any others are added only with a measurement.

The pdf is not compared. crust keeps its VNDF sampling and its leaf selection,
and BSDL's dielectric uses bounded-VNDF ("BVN") sampling, so the two densities
differ by design.

### 5. Layer opacity is pinned without C++

`closure/tests.rs` contains a direct transcription of
`evaluate_layer_opacity`: a recursive walk over the crust arena with leaf
filters fed in. It runs on fixed trees and checks that `resolve`'s base
weights equal `1 − clamp(O)` from the transcription. The transcription is
about 30 lines and has no dependency. The probe would need the OSL runtime to
build real closure trees, which this design does not bring in.

### 6. Leaf selection stays crust's, fed by the new albedos

Selection keeps `∝ lum(wᵢ) · Êᵢ(ωo)` with the 0.02 floor. `Ê` becomes the
BSDL leaf's albedo: `refl_tint · (1 − E_ms)` for reflection-only lobes, the
renormalised full lobe for refracting ones, and the tabulated value for sheen.
For the conductor, BSDL's `albedo_impl` is a deliberately rough fit, so crust
uses `1 − E_ms` times the Fresnel average from its own albedo grid; it is used
for selection only. Because selection and pdf come from the same `select`, the
NEE ↔ bounce MIS pair does not change shape.

### 7. No environment switch

CLAUDE.md reserves `CRUST_*` switches for A/B-ing an optimisation against the
behaviour it replaced. This change alters what the image *should* be, so the
A/B is two binaries: `bench_ab.sh` for time and `crust diff` / `check_images.sh`
for the image.

## Risks / Trade-offs

- [Every MaterialX golden changes] → Re-record them once, at the end, at
  `-s 16` and `--indirect-clamp 0`. For the shipped surfaces, show that the
  difference from the old goldens does not shrink as 1/√N across spp (it is a
  bias change, which is the point), and that each changed leaf's oracle cases
  pass.
- [The opacity rule changes hand-authored looks] → The only built-in pattern
  affected is a weighted opaque leaf or a `multiply` in a top. The design
  record's "trap" section is rewritten to say so, with the DPEL case kept as
  a pinned test because it is unchanged.
- [Performance] → A table lookup with bilinear or trilinear interpolation
  replaces a 9-term rational fit per leaf per vertex, which is roughly a wash.
  The opacity walk adds one add per layer. Measure with callgrind (`-s 2`) on
  `materialx_basic`, `cornellbox` (which must not move at all) and a
  `standard_surface` scene, plus `bench_ab.sh -n 7`, and record the numbers
  in the design record.
- [Upstream BSDL moves] → The commit is pinned in the script and the fixture
  header. Moving it is a deliberate fixture regeneration whose diff shows what
  changed.
- [Licence] → BSDL is BSD-3-Clause, already listed. `THIRD-PARTY.md` changes
  its source from Typhoon's copy to upstream OSL and adds the new tables. The
  Typhoon-derived random walk is untouched and stays out of scope.

## Migration Plan

The work lands as one change in steps that each leave the tree green:

1. Regenerate the tables from upstream (gate: `ReflFront` is unchanged).
2. Add the oracle and fixture. Each leaf not yet ported is excused by a
   temporary "genglsl leaf" deviation rule with its measured bound. The rule
   goes stale, and so fails, the moment that leaf's port passes, which forces
   its deletion. No test is ignored.
3. Switch the opacity walk.
4. Port the leaves one at a time, removing each leaf's `#[ignore]` as it
   passes.
5. Delete the genglsl lobes.
6. Re-record the goldens.
7. Update the documentation.

Rollback means reverting the change. No scene format or CLI flag changes.

## Open Questions

1. Is BSDL's `ZeltnerBurleySheen` table the same data as crust's Disney table?
   The answer only decides whether one table file or two ship. The behaviour
   follows BSDL either way.
2. What tolerance does the oracle use? `adobe_oracle`'s 2e-3 is the starting
   point. Table-interpolated values in f32 may need a looser bound per leaf,
   set from the first measured worst case and recorded with it.
