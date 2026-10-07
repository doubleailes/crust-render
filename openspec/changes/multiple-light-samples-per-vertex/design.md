## Context

See proposal.md (Why). Today, at a surface vertex, NEE draws one 4D sample from
the `K_NEE` domain (`tracer/path.rs`, the "Direct Lighting via Light Sampling"
block). Dimension 0 picks a light through `LightList::pick_index_at` (CDF
inversion, monotone in the pick dimension). Dimensions 1–2 place the point
through `sample_li`. The shadow ray draws its own randomness from
`K_NEE_SHADOW`. `volume_nee` mirrors this at volume scatter points. On the
bounce side, `bounce_emission_weight` and `escaped_emission` weigh an emission
hit against `LightList::density(point_pdf, pmf)` at the previous vertex. These
are the NEE ↔ bounce pairs `CLAUDE.md` lists as changing together.

## Goals / Non-Goals

**Goals**

- N light samples at the camera vertex and M at later vertices, each an
  unbiased, MIS-consistent estimator.
- Bit-identical output and instruction count at N = M = 1.

**Non-Goals**

- One sample per light ("per-light" sampling). It costs one shadow ray per
  light at every vertex, and stratified selection over N samples already
  approaches it for small light counts.
- Adaptive splitting (Conty & Kulla), which is a light-tree feature.
- Changing the default counts. That waits for the measurement in tasks
  section 4.

## Decisions

### D1. Stratify the N picks

The N samples at a vertex take their pick coordinate as `(i + u_i) / N`,
i = 0…N−1, from one draw per sample (the first sample draws from the vertex
itself, as the one sample always did; sample i ≥ 1 from the vertex's
`K_NEE_SAMPLES` sub-domain `i`). Since the pick is a monotone CDF inversion, stratified picks give each
light close to `N · pmf` samples instead of a binomial count. This is the main
variance win over N independent picks when there are few lights. Alternative: N
independent `K_NEE` domains. That is simpler, but at N = 2 two independent picks
of the same dim light leave a bright one unsampled half as often as they should.

**Only the pick is stratified.** The proposal's first draft stratified the
point-on-light dimensions by the same slice `i`. That is a bias, not a
stratification: with pmfs 0.5/0.25/0.25 and N = 4, the third light is only ever
picked by slice 3, so its point would only ever be drawn from the top quarter
of its `(u, v)` square. The point coordinates come unstratified from each
sample's own sub-domain.

### D2. Multi-sample MIS uses `N · p_light`

With N light samples and one BSDF sample, the balance and power heuristics use
sample counts (Veach 1997, §9.2): the light strategy's effective density is
`N · density`, which `LightList::density(light_pdf, pmf, samples)` now takes as
a parameter, so every caller — both MIS halves — states the count. NEE weights
each of its N contributions with `N · p_L` against `p_B` **and divides it by
`N · p_L`**: Veach's estimator is `Σ_i w_i f / (n_i · p_i)`, so the division is
also the average over the N samples. (The first implementation divided by
`N · p_L` and then multiplied by `1/N` again; the agreement test caught it as a
floor at 38% of its reference.) The bounce side uses the same `N · p_L` for the
light it hits. `PrevVertex` carries the count used at the vertex the bounce
left, so the camera vertex (N) and later vertices (M) both pair correctly. The
same factor enters `escaped_emission` for domes and distant lights, the
phase-side arm for volumes, and guiding's competing NEE density (the NEE weight
against the guide/BSDF mixture takes the same `N · p_L`).

### D3. Two counts, camera and indirect

"Camera vertex" is the first vertex of the path, whatever it is — a surface,
or a volume scatter on the camera ray — and every later vertex takes the
indirect count, a subsurface walk's exit included. A delta first vertex (glass)
takes N picks that its `eval` refuses before any shadow ray, so N is cheap
there; the first diffuse vertex behind it is indirect.

The camera vertex dominates direct-light noise in the image. Later vertices
carry little throughput, and a count there multiplies cost along the whole path.
Two separate counts let the measurement find the right split, for example
`4, 1` against `2, 2`. Arnold's global `light_samples` and Cycles' former
branched path tracing both separate them in some form.

### D4. Bit-identity at 1

At N = M = 1 the first sample is today's draw (`(0 + u)/1 = u`), the factor is
1.0, and no new domain is opened. Task 3.1 pins this with
`scripts/check_images.sh` and callgrind: every sample scene and both Kitchen
sets are identical to `main`'s goldens, and cornellbox at 2 spp on one thread
counts 4,231,470,450 instructions against `main`'s 4,233,257,504 (−0.04%).
`cornellbox_guided` differs between any two multi-threaded runs of `main`
itself (the guiding pre-pass trains in thread order), and is identical to
`main` on one thread. The structure that keeps the
instruction count: the surface block is an `inline(always)` function
(`surface_light_sample`) the vertex calls straight-line for its first sample,
and a cold out-of-line loop (`extra_surface_light_samples`) calls it for the
rest, and that function holds no closure: inlined twice, its closures (the
pick under `then`, the shadow-ray `visibility`) each got a second call site
and LLVM stopped inlining them — an out-of-line call per sample, 0.65% of
cornellbox's instructions apiece. The versions on the way: a `for` around the
block +0.57%, cold multi-sample arms +0.90%, a context struct by reference
+1.4% (its address escapes into the cold call, so it and `ray`, `rec`, `sp`
live in memory), by value +0.96%, plain parameters +1.0%, and the count
pinned to 1 still +0.76%, which is what exposed the closures.

### D5. One `L` event per light sample

The light path expression route used to record one light per vertex. It now
records the light of every NEE entry (`Route::nee_lights`, one per entry, so
`nee` may be called once per contributing sample), and the gather's per-entry
masks moved from a `MAX_SPLIT` stack array to a reusable `Vec`. `volume_nee`
records its own events, so the route's vertex opens before it, and the phase
vertex no longer re-draws `K_NEE` to learn which light NEE picked.

## Risks / Trade-offs

- [The MIS factor misses one of the pairs] → Bias that looks like plausible
  light. Mitigation: a strategy-agreement test (light-only, BSDF-only and MIS
  agree) at N = 4 for surfaces, volumes and a dome, and the LPE bitwise pin.
- [Cost grows faster than noise falls on scenes with mostly occluded lights,
  such as ALab] → The measurement records equal-time relMSE per scene, and the
  default stays 1 until it is positive everywhere.
- [Stratification across N interacts with openqmc padding] → The draws come
  from one `K_NEE` domain per sample index. A test checks that each light's
  pick frequency matches `N · pmf` within one sample.
