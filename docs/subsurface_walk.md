# The subsurface random walk: cost, measured variants, roadmap

Scope: `crates/crust-core/src/subsurface.rs` (MaterialX `subsurface_bsdf`, the port of
Typhoon's / Cycles' random walk; design record in `openspec/specs/materials/design.md`)
and its call site in `tracer/path.rs`. The other walk
the integrator runs, the carried-medium walk for OpenPBR scattering interiors, is
covered at the end because the same fix applies to it.

Everything below is measured, following `CLAUDE.md` § Measuring a change: instruction
counts under callgrind (deterministic), `--stats` counters, and a deterministic
walk-level harness whose copy of `random_walk` was first checked bit-identical to the
real one (4 000 walks, same weights, exit points, step and ray counts).

Measured with the CI-pinned toolchain (1.98.1; a 1.94 rustc cannot build the locked
`wasmtime-internal-jit-icache-coherence 49.0.1`) on a shared 4-vCPU machine with about
two cores of throughput, so no wall-clock figure below is load-bearing; the instruction
counts are.

## 1. Where the time goes today

`samples/materialx_subsurface.usda`, 640×320, 64 spp adaptive (53 mean), baseline build
of `f295102` (the commit before this document):

| counter | value |
|---|---|
| subsurface walks | 5 588 780 (99.8 % exited, 8.1 steps each) |
| walk rays | 45 523 946 = **58 % of all 78.3 M ray queries** |
| walk rays / walk steps | 1.005 (foreign-surface restarts in `trace_owner` are negligible here) |
| primary / bounce / shadow rays | 10.85 M / 12.15 M / 9.79 M |

callgrind, `-s 2`, one thread (5.92 G instructions total, 174 139 walks, 1.42 M steps):

| what | instructions | share of render |
|---|---|---|
| `walk_subsurface` inclusive | 2 964 M | **50.1 %** |
| of which `World::intersect` called from the walk | 1 272 M | 21.5 % (43 % of the walk) |
| of which libm: `expf` 419 M, `powf` 152 M, `atanf` 117 M, `logf` 53 M, `sincosf` ~110 M | ~850 M | ~14 % (~28 % of the walk) |
| of which the walk's own arithmetic and bookkeeping (PCG draws 74 M, SIMD glue 125 M, …) | ~840 M | ~14 % |

Per step that is ~2 100 instructions, of which ~900 are the ray cast. In this six-primitive
scene the walk's **arithmetic costs more than its ray casts**; in a production scene the
traversal share grows, but the arithmetic does not shrink.

The nine `expf` per step are the classic transmittance, the forward-stretched one and the
backward-stretched one (three channels each). The `powf`/`atanf` are the Chiang remap:
12 `powf` + 6 `atanf` + 6 `expf` per walk, plus `powf` in `diffusion_length_dwivedi` and
in the guided fraction, so ~300 M instructions (10 % of the walk) are spent turning
`(color, radius, g)` into coefficients, once per walk.

`--profile` did not show any of this: the walk had no section, so its whole cost landed
in `MainLoop`'s local time (54.5 %). With the `Subsurface` section added by the
prototype the picture is:

| section | share of thread time | per call |
|---|---|---|
| Subsurface | **34.3 %** | 4.86 µs per walk |
| MainLoop (local) | 18.8 % | |
| SurfaceLighting | 15.5 % | 800 ns |
| Bounce | 8.9 % | |
| Trace | 8.3 % | 286 ns |

## 2. Walk-level experiments

Harness: a scratch integration test holding a copy of `random_walk` with knobs (not
checked in; its copy was verified bit-identical to the real walk first). Slab = sphere of radius 20 touching the entry
point (200 mean free paths); "sphere" = the fixture's 0.8 sphere; entries: cosine
into the medium (Chiang's fit), refracted (cosine incidence through a smooth IOR 1.5
interface, what the closure does), or straight down.

**E1 — albedo mapping × entry** (reflectance of a slab, 16 384 walks):

| case (target) | entry | Chiang (crust) | van de Hulst (Cycles ≥ 4.x) |
|---|---|---|---|
| skin (0.9, 0.6, 0.45), r (0.2, 0.1, 0.05) | cosine | (0.927, 0.604, 0.451), 27.3 steps | (0.906, 0.601, 0.448), 23.7 steps |
| | refracted | (0.913, 0.566, 0.410), 30.9 | (0.887, 0.562, 0.407), 27.0 |
| marble (0.85, 0.85, 0.8) | cosine | (0.876, 0.876, 0.824), 21.9 | (0.861, 0.861, 0.813), 20.4 |
| | refracted | (0.858, 0.858, 0.800), 25.0 | (0.841, 0.841, 0.788), 23.2 |
| jade (0.3, 0.75, 0.45), g 0.3 | cosine | (0.303, 0.766, 0.459), 16.4 | (0.302, 0.771, 0.458), 16.0 |
| | refracted | (0.254, 0.732, 0.405), 18.5 | (0.253, 0.737, 0.405), 18.1 |
| grey 0.7, g 0.6 | cosine | 0.697, 20.3 | 0.716, 20.0 |
| | refracted | 0.653, 23.0 | 0.674, 22.8 |

Both mappings reproduce the colour under the cosine entry (van de Hulst a little
closer at high albedo, where Chiang's fit passes 1 and is clamped); **neither fixes the
refracted-entry darkening** the design record already pins (jade's red 0.254 for 0.3).
Van de Hulst walks 10–13 % fewer steps at high albedo and costs three square roots
per walk instead of 12 `powf` + 6 `atanf` + 6 `expf`.

**E4 — the 256-step cap** (slab, refracted entry, 8 192 walks):

| medium | cap | reflectance | capped walks | mean steps |
|---|---|---|---|---|
| target 0.95 or 0.99 (Chiang clamps both to α = 0.999999) | 256 | 0.909 | 8.9 % | 44 |
| | 4096 | 0.982 | 1.5 % | 160 |
| skin fixture | 256 | (0.912, 0.565, 0.411) | 3.5 % | 31 |
| | 4096 | (0.950, 0.565, 0.411) | 0.01 % | 41 |
| marble fixture | 256 | (0.859, 0.859, 0.801) | 1.6 % | 25 |
| | 4096 | (0.868, 0.868, 0.803) | 0 % | 28 |

On a thick object the cap drops 7.5 % of a near-white medium's energy and 4 % of skin's
red. On the fixture's 0.8 sphere nothing is capped (E5: skin 11.8 steps, marble 5.3,
jade 7.9; 0 % capped), which is why the fixture does not show it. The tail is the
first-passage tail of a 3-D walk to a plane (∝ n^-1/2 without absorption), so no fixed
cap is unbiased and step-count roulette has unbounded variance in the α → 1 limit;
only a change of algorithm helps (§ 3.5).

**E6/E7 — steps spent at negligible weight, and in-walk roulette** (256 batches of 128
walks; std is the standard deviation of the batch means):

| case | steps below peak 0.05 | mean steps, no RR → RR at 0.05 | std, no RR → RR |
|---|---|---|---|
| skin, sphere | 12.8 % | 11.97 → 10.81 | (0.065, 0.036, 0.036) → identical |
| skin, slab | 18.9 % | 30.74 → 25.99 | (0.069, 0.035, 0.035) → identical |
| jade, sphere / slab | 0.5 % / 2.7 % | 7.85 → 7.83 / 18.82 → 18.44 | identical |
| marble | 0 % | 5.26 → 5.26 | identical |
| grey 0.3 | 1.2 % | 4.97 → 4.94 | identical |

Roulette below a peak throughput of 0.05 (survive with p = peak/0.05, floor 0.05,
reweight) removes 10–15 % of a chromatic medium's steps at no measurable variance and
identical means to three digits.

**E2 — stratified draws for bounces 1..k** (the walk stratifies only step 0): batch std
unchanged on the thick slab and on 1- and 4-mfp slabs; on a 2-mfp slab the blue
channel's std fell 0.021 → 0.016 with k = 2. Not worth a domain budget.

**E3 — Dwivedi length from the reduced albedo in the similarity regime** (d'Eon &
Křivánek 2020 § similarity): means and std unchanged, steps −1 to −4 %. Not worth it.

## 3. Recommendations, in order of payoff per effort

### 3.1 Done, in the commit that added this document

1. **`Section::Subsurface`** around the walk call, so `--profile` shows the walk
   instead of folding it into `MainLoop`. Zero cost off the walk path.
2. **In-walk Russian roulette** (`RR_THRESHOLD` 0.05, floor 0.05), drawn from the step's
   `new_domain(2)` (incidental draw, per the sampling rules). Unbiased; measured above.
3. **One exponential fewer per channel per step.** The backward-stretched transmittance
   is `exp(−σt)² / exp(−σ(1 − c/ν)t)`, i.e. `tr² / tr_fwd`, so the three backward
   `expf` are a multiply and a divide (with an `exp3` fallback when either term is below
   1e-18). Nine `expf` per step become six.

Measured, callgrind `-s 2` on the fixture, baseline → prototype:

| | baseline | prototype | change |
|---|---|---|---|
| whole render | 5 920 078 931 | 5 704 529 749 | **−3.6 %** |
| `walk_subsurface` inclusive | 2 963 950 492 | 2 754 230 339 | **−7.1 %** |
| `expf` | 418.6 M | 291.5 M | −30 % |
| `Bvh::hit` | 1 634.6 M | 1 591.1 M | −2.7 % |
| walk steps (full render) | 8.1 per walk | 7.8 per walk | −3.7 % |
| walk rays (full render) | 45.52 M | 43.47 M | −4.5 % |

Equal-sample noise against a 2048-spp reference of the fixture (`relmse`, the
`exr_diff` metric):

| spp | baseline | prototype |
|---|---|---|
| 16 | 4.8510e-2 | 4.8510e-2 |
| 32 | 2.3196e-2 | 2.3195e-2 |

Both halve from 16 to 32 spp, as unbiased estimators do, and the roulette costs no
measurable variance. Base vs prototype at 16 spp differ in 28 % of pixels with RMSE 3.0e-4, almost
all at the last ulp (the reordered transmittance arithmetic); the roulette's
reweighting shows as isolated differences of a few 1e-3. Cornellbox, which never walks,
executes 2 663 960 177 instructions before and 2 663 964 522 after (+0.0002 %), so the
per-vertex path is untouched. All pinned tests pass (`subsurface::tests`,
`mtlx_surfaces`, `resolve`, `stats`, `profile`); `cargo fmt` and `clippy -D warnings`
are clean on the prototype.

### 3.2 Bit-identical, not yet done: a geometry-only intersect for walk rays

`trace_owner` calls `World::intersect`, which after the kernel hit resolves the
per-face table, the UV map (interpolating three UVs and a tangent), and the ray-cone
footprint. A walk step needs `t`, `p`, `normal`, `front_face`, `geom_id` only. On the
fixture's spheres this is just the branch checks (~70 instructions per ray, ~1.7 % of
the render); on the realistic case, a UV- or Ptex-mapped skin mesh, it is a full UV
resolve per step, thrown away. A `World::intersect_geometry` (kernel hit → bare
`HitRecord`, no tables) keeps the image bit-identical. The exit record must still be
resolved fully, once, which `PendingExit::hit` already builds separately.

### 3.3 A design decision: the van de Hulst mapping

Cycles' current random walk maps albedo with van de Hulst's inversion (d'Eon,
*Hitchhiker's Guide*, eq. 53.7): `s = 4.09712 + 4.20863A − √(9.59217 + 41.6808A +
17.7126A²)`, `α = (1 − s²)/(1 − g s²)`, `σt = 1/r`; the Chiang polynomial survives only
in its skin/legacy modes. crust already has this exact code in `Medium::from_subsurface`.
Switching the walk to it:

- saves ~10 % of the walk's instructions (the per-walk `powf`/`atanf`/`expf` budget) and
  10–13 % of its steps at high albedo (E1);
- distinguishes colours above 0.95, which the clamped Chiang fit renders identically
  (E4: 0.95 and 0.99 give the same walk);
- moves reflectances by up to ±0.03 (E1), which is why it is a decision against
  D1 ("Typhoon's walk, line for line") rather than an optimization. It does not fix
  the refracted-entry darkening; a cosine or Cycles-skin 50/50 entry would.

### 3.4 Variance rather than cost: guiding beyond the entry normal

The walk's guiding is Dwivedi's (Křivánek & d'Eon 2014; Meng, Hanika & Dachsbacher
2016; exponent fit from d'Eon & Křivánek 2020), always toward the entry plane or its
detected opposite. Gouder et al., *A Data-Driven Approach to Analytical Dwivedi
Guiding* (CGF 2025), pick the Dwivedi slab normal from a radiance field learned at the
boundary (the average normal of lit boundary regions, or directions drawn from the
learned incident distribution), which is what makes the walk efficient under
production light rigs and indirect light. crust already trains an SD-tree field on
surfaces (`guiding/`), so the ingredient exists; the walk would query it at the entry
and MIS the extra lobe like the backward one. This is where the next large variance
win is, and it is a project.

### 3.5 The thick-object regime

E4 quantifies what the design record lists as a gap: on objects many mean free paths
thick, bright media lose 4–8 % of their energy to the 256-step cap, and lifting the
cap quadruples the steps. The literature answer is to stop walking step by step once
the walk is deep: shell transport / sphere tracing (Moon, Walter & Marschner 2007;
Müller et al. 2016; Leonard, Höhlein & Westermann 2021) jumps to the surface of the
largest empty sphere with a precomputed or learned exit distribution, cutting steps by
one to two orders of magnitude in exactly this regime; learned BSSRDFs (Vicini,
Koltun & Jakob 2019) replace the walk outright. Either needs a closest-point query on
the owner geometry, which `crust-rt` does not have (ray queries only), so this is a
kernel feature first. Until then the honest option is to document the measured loss
(Cycles shares the cap and the loss).

### 3.6 The carried-medium walk (OpenPBR scattering interiors)

`trace_path`'s carried-medium scatter samples the free flight at the max-channel
majorant and weights each channel by `σs/σ̄ · e^{(σ̄−σt)t}`, with no channel MIS. For a
chromatic medium that is the estimator the subsurface walk's own doc calls out as
blowing up ("skin: red travels four times further than blue"). The subsurface walk's
balance-heuristic channel selection (Kutz et al. 2017's spectral MIS in its
single-channel form) transfers directly: pick the channel ∝ throughput·albedo, divide
by `Σ P(c)·σc·e^{−σc t}`. Each of those scatters also spends a unit of path depth,
unlike the subsurface walk's free 256 steps, so a dense interior is truncated by
`maxDepth`.

### 3.7 Measured and not recommended

- Stratifying the walk's second and later steps (E2): no gain worth a domain.
- Reduced-albedo Dwivedi length after the similarity switch (E3): no gain.
- An owner-only traversal filter in the kernel: walk rays / steps is 1.005 on the
  fixture; it only pays with geometry nested inside the medium.
- Replacing the 256 cap by step-count roulette: unbounded variance in the α → 1 limit
  (first-passage tail), see § 3.5.

## 4. Reproducing

```bash
rustup toolchain install 1.98.1
cargo +1.98.1 build --release
target/release/crust-render -i samples/materialx_subsurface.usda -o sss.exr --stats --profile
RAYON_NUM_THREADS=1 valgrind --tool=callgrind --cache-sim=no --branch-sim=no \
    target/release/crust-render -i samples/materialx_subsurface.usda -o /tmp/x.exr -s 2
callgrind_annotate --inclusive=yes callgrind.out.<pid> | grep -E "walk_subsurface|World>::intersect|expf|powf|atanf"
```

## References

- Chiang, Kutz, Burley. *Practical and Controllable Subsurface Scattering for Production Path Tracing.* SIGGRAPH 2016 Talks. (The albedo inversion the walk uses.)
- Křivánek, d'Eon. *A Zero-Variance-Based Sampling Scheme for Monte Carlo Subsurface Scattering.* SIGGRAPH 2014 Talks. <https://cgg.mff.cuni.cz/~jaroslav/papers/2014-zerovar/>
- Meng, Hanika, Dachsbacher. *Improving the Dwivedi Sampling Scheme.* EGSR 2016. <https://cg.ivd.kit.edu/1951.php>
- d'Eon, Křivánek. *Zero-Variance Theory for Efficient Subsurface Scattering.* SIGGRAPH 2020 Course. <https://www.researchgate.net/publication/344211208_Zero-Variance_Theory_for_Efficient_Subsurface_Scattering>
- Wrenninge, Villemin, Hery. *Path Traced Subsurface Scattering using Anisotropic Phase Functions and Non-Exponential Free Flights.* Pixar Technical Memo 17-07. <https://graphics.pixar.com/library/PathTracedSubsurface/paper.pdf>
- Gouder et al. *A Data-Driven Approach to Analytical Dwivedi Guiding.* Computer Graphics Forum 2025. <https://onlinelibrary.wiley.com/doi/10.1111/cgf.70164>, <https://cgg.mff.cuni.cz/publications/a-data-driven-approach-to-dwivedi-guiding/>
- Herholz, Zhao, Elek, Nowrouzezahrai, Lensch, Křivánek. *Volume Path Guiding Based on Zero-Variance Random Walk Theory.* ACM TOG 38(3), 2019.
- Vicini, Koltun, Jakob. *A Learned Shape-Adaptive Subsurface Scattering Model.* SIGGRAPH 2019.
- Leonard, Höhlein, Westermann. *Learning Multiple-Scattering Solutions for Sphere-Tracing of Volumetric Subsurface Effects.* Eurographics 2021.
- Moon, Walter, Marschner. *Rendering Discrete Random Media Using Precomputed Scattering Solutions.* EGSR 2007; Müller, Papas, Gross, Jarosz, Novák. *Efficient Rendering of Heterogeneous Polydisperse Granular Media.* SIGGRAPH Asia 2016. (Shell transport.)
- Kutz, Habel, Li, Novák. *Spectral and Decomposition Tracking for Rendering Heterogeneous Volumes.* SIGGRAPH 2017. (Spectral MIS.)
- Xu et al. *ReSTIR Subsurface Scattering for Real-Time Path Tracing.* PACMCGIT 2024. <https://dl.acm.org/doi/10.1145/3675372> (real-time reuse of walks; not applicable offline, listed for completeness)
- d'Eon. *A Hitchhiker's Guide to Multiple Scattering*, eq. 53.7. <https://eugenedeon.com/hitchhikers>
- Blender Cycles, current `subsurface_random_walk.h`, `subsurface.h`, `bssrdf.h`: <https://raw.githubusercontent.com/blender/blender/main/intern/cycles/kernel/integrator/subsurface_random_walk.h>
