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
i = 0…N−1, from one draw per sample in the `K_NEE` domain, with the
point-on-light dimensions stratified the same way. Since the pick is a monotone
CDF inversion, stratified picks give each light close to `N · pmf` samples
instead of a binomial count. This is the main variance win over N independent
picks when there are few lights. Alternative: N independent `K_NEE` domains.
That is simpler, but at N = 2 two independent picks of the same dim light leave
a bright one unsampled half as often as they should.

### D2. Multi-sample MIS uses `N · p_light`

With N light samples and one BSDF sample, the balance and power heuristics use
sample counts (Veach 1997, §9.2): the light strategy's effective density is
`N · density`. NEE averages its N contributions (each weighted with `N · p_L`
against `p_B`). The bounce side uses the same `N · p_L` for the light it hits.
`PrevVertex` carries the count used at the vertex the bounce left, so the
camera vertex (N) and later vertices (M) both pair correctly. The same factor
enters `escaped_emission` for domes and distant lights, the phase-side arm for
volumes, and guiding's competing NEE density.

### D3. Two counts, camera and indirect

The camera vertex dominates direct-light noise in the image. Later vertices
carry little throughput, and a count there multiplies cost along the whole path.
Two separate counts let the measurement find the right split, for example
`4, 1` against `2, 2`. Arnold's global `light_samples` and Cycles' former
branched path tracing both separate them in some form.

### D4. Bit-identity at 1

At N = M = 1 the loop runs once with `u_0` as today's draw (`(0 + u)/1 = u`),
the factor is 1.0, and no new domain is opened. Task 3.1 pins this with
`scripts/check_images.sh` and callgrind.

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
