# Design

## Context

See proposal.md for the motivation. Here is the state this design starts from.

- **Native fuzz today** (`material/openpbr/lobes.rs`):
  - `eval_fuzz` is `fuzz_color · w · sheen_charlie(…)`, using `brdf.rs`'s Charlie D
    with Neubelt's visibility (mislabelled "Imageworks" there) and α floored at 0.05.
  - Every layer below is multiplied by a scalar `base_atten = 1 − w`, in `eval_all`,
    `eval_split`, `diffuse_filter` and `albedo`.
  - The fuzz is sampled cosine-weighted (`mod.rs:596`). Its pdf shares
    `pdf_cosine` with diffuse (`pdf_all`).
  - Its pick weight is `w · luma(fuzz_color)`, in `LobePmf::selecting`, which takes
    only the material, not the view.
- **Emission** already has a view-dependent hook, `emitted_directional(cos_θo)`. It
  carries the coat passage. `Material::emitted_at` forwards to it.
- **The MaterialX `Sheen` leaf** (`material/closure/mod.rs`):
  - It is evaluated as `mx::imageworks_sheen`, with α floored at 0.005, and
    sampled cosine-weighted.
  - It layers with `1 − E·w`, where E is MaterialX's Imageworks rational fit.
  - `crust-mtlx` drops the authored `mode` after reporting `zeltner` as
    approximated (`bsdf.rs:888`).
- **The reference**: Adobe `openpbr-bsdf` (Apache-2.0, header-only C++/GLSL/…),
  `impl/openpbr_fuzz_lobe.h` and `impl/openpbr_bsdf.h`. On 2026-10-08 a probe built
  against commit `c91aad1` with g++ and GLM ran in seconds. Its sampled fuzz albedo
  reproduced the table's R to four digits at every point tried.
- **The table**: Disney `ltc-sheen` "Volume" fit, 32×32 entries of (a⁻¹, b⁻¹, R).
  The copies in Adobe (`impl/data/openpbr_ltc_data.h`) and in the BSDL vendored by
  Typhoon (`MTX/bsdf_zeltnersheen_param.h`) are identical across all 3072 floats.

## Goals / Non-Goals

**Goals:**

- A reference oracle for native `OpenPBR`, whose deviation rules are the alignment
  gap list in executable form.
- Native fuzz equal to Adobe's within float tolerance, for every input the oracle
  samples.
- One Zeltner implementation shared by native fuzz and the MaterialX leaf.
- The zero-fuzz path unchanged: bit-identical images, and the same instruction count
  on `cornellbox`.

**Non-Goals:**

- Closing any other gap: MMS compensation, F0→IOR, interior emission, geometry
  inputs. Each stays a deviation rule with its measured bound.
- Matching MaterialX's analytic Zeltner fits on the closure path.
- Adobe's reciprocal layering mode (`OPENPBR_RECIPROCAL_COAT_AND_FUZZ = 1`).
- A fuzz normal distinct from the shading normal. Adobe mixes in the coat normal, and
  crust has none yet, so Adobe's expression reduces to the shading normal.
- Ray-cone spread for the narrower LTC lobe. The fuzz keeps `RayCone::MAX_SPREAD`.
- A `CRUST_*` A/B switch. This is a model change, not an optimization, so the
  "off" side would be the inaccurate model with nothing to measure against.
  The before and after images are part of the change instead.

## Decisions

### D1. The oracle mirrors `osl_oracle`: a generator outside Cargo, a fixture inside

- `scripts/adobe_oracle/probe.cpp` and `scripts/adobe_oracle.py` fetch Adobe at a
  pinned commit plus GLM into a temporary directory, compile, and write
  `crates/crust-core/tests/data/adobe_oracle.txt`.
- `crates/crust-core/tests/adobe_oracle.rs` replays the fixture.
- CI never runs the generator.

Each fixture case holds:
- the parameter set (every Adobe input crust maps, at a value drawn from a
  fixed-seed generator, plus hand-picked corner cases: one per known gap with
  nothing else present, so each rule has a case it alone explains);
- ω_o and eight ω_i in the local frame (outward normal +Z; a ω_o below the plane
  is a back-face hit);
- the reference eval (Adobe's `diffuse` and `specular` parts summed), the BSDF
  times the cosine;
- the directional albedo at ω_o: `(π/N) Σ f(ω_o, ω_i)` over a fixed 32 × 32
  cosine-weighted midpoint grid that the replay rebuilds, so both sides compute
  one quantity rather than two estimates;
- the emission toward ω_o.

The pdf is **not** compared. It is each renderer's own sampling density (crust's
comes from its lobe-selection heuristic and its floors), so no two correct
implementations need agree on it. crust's pdf has to agree with crust's values,
which the consistency tests in `openpbr/tests.rs` check (D4).

*Alternative:* calling Adobe's GLSL through a Rust shader crate, or binding the C++
with `cc` in a `build.rs`. Both would bring a C++ toolchain into
`cargo test` and CI, which CLAUDE.md's CI shape does not allow. A committed fixture
keeps CI pure Rust, exactly as the OSL oracle does.

### D2. A deviation is a predicate on the inputs, with a bound

A rule names the condition under which it applies, for example "`base_metalness
> 0`: no multiple-scattering compensation on the metal lobe", and the largest
error it excuses.

- A case's error is the worst of three measures that stay meaningful where values
  are near zero and where they peak: the albedo's absolute error, the emission's
  error relative to `max(1, |e|)`, and each value's error relative to
  `max(0.05, |f·cos|)`.
- A case outside every rule's condition must be within the global tolerance,
  2e-3.
- A case under several rules is excused up to the **sum** of their bounds. Random
  cases stack gaps, and their errors do not separate.
- A rule's bound is the worst error among the cases it alone applies to. For a gap
  no case can isolate (thin film needs a specular lobe), it is what its cases need
  beyond the other rules' bounds. Both are measured by an `#[ignore]`d report test
  and get a 2% margin.
- A rule is **stale** when every case passes without it, and a stale rule fails
  the test. Without that, a closed gap would leave its excuse behind. This is also
  "deleting any single rule makes the oracle fail", checked on every run.
- The initial rules are each gap in `docs/openpbr_reference_alignment.md`,
  "Fuzz" included (until 4.7), plus the interactions the fixture exposed: the
  specular-diffuse coupling and the coat's effect on the base.

*Alternative:* a per-case "expected crust value" snapshot. It is simpler, but it
records a difference without saying why, and the oracle's whole point is to make
the reason explicit.

### D3. One `ZeltnerSheen` in `material/brdf.rs`, from the table, with exact LTC sampling

Following Adobe's and Disney's formulation:

- The table is a `[[f32; 3]; 1024]` constant, ported from Disney's source.
  `scripts/tables/` gets the conversion script and a check that its output matches
  Adobe's copy.
- The lookup is bilinear: `row = clamp(α)·31`, `col = clamp(cos θ_o)·31`, clamped
  to the largest float below 1, the same as Adobe's array mode.
- `eval(ω_o, ω_i) → (R · D_ltc(ω_i), pdf = D_ltc(ω_i))`. Here `D_ltc` already
  includes the cosine (crust's lobes return `f·cos` and pdf separately). Its Jacobian
  is factored as Adobe factors it (`a⁻¹ / |M⁻¹ω|²`, squared), to keep grazing pdfs
  representable.
- `sample(ω_o, u)`: cosine-sample, apply M, normalise, rotate by ω_o's azimuth. A
  sample below the horizon is a failed sample, as Adobe treats it, not a reflection.
- The roughness is used as authored, with no floor, as Adobe does. BSDL and MaterialX
  clamp at 0.02 and 0.01 because LTC "gains energy" at low α. The furnace test in
  the spec decides whether crust needs a floor. If it does, the floor becomes a
  deviation rule rather than a silent clamp.

*Alternative:* MaterialX's analytic fits, which need no table. They differ from the
"Volume" fit by up to 0.013 in R for roughness ≥ 0.3 and 0.076 below it toward
grazing, and by up to 0.30 in a⁻¹ (measured over a 101 × 100 grid; an early
spot check had said "about 0.01"), and Adobe is the reference this change
tracks. The user chose one flavour, so it is the table.

### D4. Native layering takes R(ω_o) once per shading point

R depends only on (cos θ_o, `fuzz_roughness`), so it is computed once per hit along
with the coat and base terms:

- `base_atten = 1 − w · R(ω_o)` replaces `1 − w` in `eval_all`, `eval_split`,
  `diffuse_filter` and `albedo`. The ω_o-only form stays light-independent, so
  `diffuse_filter`'s "`raw × filter` reproduces `eval_split`" contract
  (`docs/openpbr_reference_alignment.md`) holds. That is why Adobe's default
  (view-side only) was chosen over the reciprocal mode, which would move a factor
  into the light.
- `diffuse_filter` and `albedo` gain the view cosine as a parameter. The AOV callers
  already have it.
- `LobePmf::selecting` gains the view cosine. Its fuzz weight becomes
  `w · R · max(fuzz_color)`, and its base weights are scaled by `base_atten`, as
  Adobe's `openpbr_sheen_probability` scales them. The pdf in `pdf_all` uses the
  same `LobePmf`, so pick and pdf stay paired. `pdf_cosine` stops being shared with
  the fuzz, which now has its own `D_ltc`.
- Emission: `emitted_directional(cos_θo)` multiplies by `base_atten(cos_θo)` after
  the coat passage. That is Adobe's `openpbr_compute_emission` order.
  `Material::emitted_at` and every NEE consumer already go through this one function,
  so the NEE ↔ bounce pairing is inherited, not new.
- Coat roughness: Adobe's
  `coat_r = mix(coat_r, ⁴√min(1, coat_r⁴ + avg(fuzz_color)·fuzz_r·0.005·fuzz_r⁴), w)`
  is applied where the coat α is derived. It is applied once, so `eval` and `sample`
  see the same α.
- Interior: **not** Adobe's. Adobe gives the fuzz no presence on a back-facing,
  non-thin-walled hit. Applying that, found while implementing, would strip the
  fuzz from the back of every open cloth mesh not authored thin-walled, which is
  most cloth. So crust keeps the fuzz there, as it keeps the coat and the
  emission, and the oracle's `interior` deviation covers the difference with the
  rest of that gap.

### D5. The resolve contract carries R as derived state, not a parameter

`Material::resolve` must equal per-query shading (`tests/resolve.rs`), and it is
built by `Resolution::new`. R is a function of view and roughness, not an authored
input, so it is computed inside `eval` from the resolved `fuzz_roughness` and the
query's ω_o. It is not cached across queries in the resolved struct, so resolved and
per-query shading compute it from identical inputs and stay bit-identical. A per-hit
cache is a later optimization. If one is added, `tests/resolve.rs` pins it.

### D6. The MaterialX leaf carries its mode

- `crust-mtlx`'s `Sheen` leaf keeps `SheenMode`, and the report is removed.
- In `closure/mod.rs`, `prepare_sheen` branches on the mode:
  - `conty_kulla` is unchanged.
  - `zeltner` uses `ZeltnerSheen`: E = R(cos θ_o), pick weight
    `luma(color) · R` floored at 0.02 as today, value
    `color · weight · R · D_ltc`, pdf `D_ltc`, and sampler `ZeltnerSheen::sample`.
- `LobeLabel::Sheen` and `Scatter::Glossy` are unchanged, so LPEs route the same way.

## Risks / Trade-offs

- **The look changes, and not subtly.** Low-roughness fuzz becomes rim-only, and
  high-roughness fuzz is about twice as bright facing the camera and hides more of
  the base. → Before and after renders of `openpbr_showcase` and `materialx_lion` go in
  the change, along with the R table excerpt in the design record, so the change
  reads as intended.
- **LTC at α → 0 may gain energy.** BSDL clamps for this reason. → The spec's
  furnace scenario sweeps α down to 0. If it fails, the floor is added and recorded
  as a deviation from Adobe, never as a silent clamp.
- **The pick weight becomes view-dependent.** An error that let pick and pdf disagree
  would bias the image silently. → New sample-vs-pdf consistency tests in
  `openpbr/tests.rs` (the pdf integrates to 1 over the hemisphere, and the sampled
  histogram matches the pdf), modelled on the closure tests' existing checks, for
  fuzz-only and fuzz-over-base materials at several ω_o. The native shader has none
  today.
- **The oracle's tolerance hides drift.** → The tolerance is global and small, and
  every excuse is a named rule with a measured bound. A stale rule fails the test.
- **The fixture goes stale when Adobe moves.** → The commit is pinned. Regenerating
  against a new commit is an explicit act, and its diff shows exactly which cases
  moved.
- **The zero-fuzz path's instruction count.** `LobePmf::selecting` now takes the
  view, and `base_atten` is computed per query. → Under `w == 0`, `base_atten` is the
  literal 1.0 and no table fetch happens, with the same "skip, not multiply by zero"
  argument `eval_all` already relies on. The callgrind gate on `cornellbox` confirms
  it.
- **Licensing.** The table's numbers are Disney's (Apache-2.0). → They get a
  `THIRD-PARTY.md` entry naming Disney's `ltc-sheen` and Adobe's `openpbr-bsdf`.
  The oracle generator fetches Adobe at run time and vendors nothing.

## Migration Plan

There is no data migration. The authored `fuzz_*` and `sheen_bsdf` inputs keep their
names and ranges, and only their meaning changes.

Order of work:
1. Land the oracle with the current fuzz as a deviation rule (no image change).
2. Port the lobe and the layering, delete the fuzz rule, and re-record the affected
   goldens.

Rollback is a revert of step 2. Step 1 stays useful on its own.

## Open Questions

- Whether a per-hit cache of R is worth its complexity. This is decided by profiling
  after the port, and it does not change the specs.
- Whether Typhoon should be told that its `SheenMode` is never read. This is outside
  crust.
