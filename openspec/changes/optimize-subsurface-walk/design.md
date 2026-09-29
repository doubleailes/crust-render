# Design

## Context

See proposal.md — Why. The walk is a line-for-line port (design record D1 of
`add-mtlx-random-walk-subsurface`), kept off the per-vertex path with `#[cold]`
out-of-line calls, a pending-exit slot and a leaf index in `ScatterSample`'s
padding, each measured against cornellbox's instruction count. Any change to it has
to keep that property. Its draws come from `K_SSS` off the entry vertex: the first
step stratified, everything after incidental (`draw_rnd`).

Measured on the fixture (`docs/subsurface_walk.md` § 1): per step ~2 100
instructions, ~900 of them the ray cast, ~600 libm (nine `expf`, one `logf`, one
`sincosf`), the rest arithmetic; per walk another ~1 500 in the Chiang remap.
Walk-level experiments (§ 2) showed which changes pay and which do not.

## Goals / Non-Goals

**Goals:**

- Fewer instructions per walk with the estimator's expectation unchanged and its
  noise unchanged at equal sample counts.
- The walk visible in `--profile`.
- Cornellbox's instruction count unchanged (the per-vertex path is not touched).

**Non-Goals:**

- Changing what the walk converges to: the albedo mapping (Chiang), the refracted
  entry, the 256-step cap and the exit Lambertian stay as ported. The van de Hulst
  mapping is measured and left as a design decision (`docs/subsurface_walk.md`
  § 3.3).
- Guiding changes (boundary-radiance Dwivedi normals) and the thick-object regime
  (shell / sphere tracing): projects, recorded in the roadmap.
- A geometry-only intersect for walk rays: bit-identical and worth doing, but a
  `World` API change, so its own change.

## Decisions

- **D1 — Roulette at a fixed throughput threshold, survival ∝ peak.** Below a peak
  throughput of 0.05 the walk survives with `p = peak / 0.05` floored at 0.05 and
  is reweighted by `1/p`. Measured over 256 batches × 128 walks: means identical to
  three digits, batch std identical, steps −10 % (skin on the fixture's sphere) to
  −15 % (skin on a slab), 0 % for marble. *Alternatives:* a step-count roulette
  (rejected: the first-passage tail of a walk in an α → 1 medium is heavy, so
  its variance is unbounded); a lower threshold (0.01 saves a third as much);
  roulette only after N steps (no benefit measured: dim walks are dim early).
  The coin is `d.new_domain(2).draw_rnd_f32::<1>()`, the step's third sub-domain,
  an incidental draw like the step's others.
- **D2 — The backward transmittance as a quotient, with an underflow guard.** With
  `s_b = 2 − s_f`, `exp(−σ s_b t) = exp(−σt)² / exp(−σ s_f t)` exactly; in `f32` it
  differs by a few ulps. The quotient is taken only while both `tr` and `tr_fwd`
  are above 1e-18 in every channel, so `tr²` stays a normal float; otherwise the
  exponential is evaluated as before. *Alternative:* keep nine `expf` (rejected:
  the three saved are 3 % of the walk for two lines), or a vectorised
  approximation of `exp` (rejected: a biased pdf term, and glam has no `exp`).
- **D3 — A profile scope in the caller, inside the walk branch.** `scope_if::<PROFILE>`
  around `walk_subsurface` in `trace_path`, inside the `if let` that only a
  subsurface sample enters; `walk_subsurface` stays `#[cold] #[inline(never)]`.
  Cornellbox measured +4 345 of 2.66 G instructions. *Alternative:* the scope inside
  `random_walk` (rejected: `subsurface.rs` would then depend on `profile`, and the
  exit record's construction in `PendingExit::hit` would be left out).
- **D4 — No `CRUST_*` switch.** The "off" side is the previous commit; both changes
  are a handful of lines on the per-step path, and a switch there would be a branch
  per step for every user to pay. The A/B was done with two binaries under
  callgrind and against a 2048-spp reference, and is recorded.
- **D5 — Measured and not adopted.** Stratifying steps 1..k (no gain beyond a
  2-mfp slab's blue channel), the reduced-albedo Dwivedi length after the
  similarity switch (no gain), an owner-only traversal filter (walk rays per step
  are 1.005 on the fixture). Each has its table in `docs/subsurface_walk.md` so
  nobody retries it blind.

## Risks / Trade-offs

- [Roulette reweights by up to 20×, which could show as fireflies] → the threshold
  is low enough that a surviving walk carries at most 1.0 in its brightest channel,
  and the fixture's relmse against the reference is unchanged at 16 and 32 spp;
  the firefly clamp still applies downstream.
- [The quotient loses ulps against the exponential] → images with subsurface change
  at the last ulp only (28 % of the fixture's pixels, RMSE 3e-4 against the
  previous build); nothing bit-identical was promised for scenes with walks, and
  every scene without one is unchanged.
- [The 256-step cap's energy loss stays] → stated in the spec as a known bias with
  its measured size; the roadmap names the fix.
- [Roulette changes `--stats` exit share and mean steps] → 99.8 % → 99.1 % exited
  and 8.1 → 7.8 steps on the fixture; the record notes it so a reader does not
  read it as a regression.

## Migration Plan

None: no file format, setting or flag changes. Goldens of scenes with a
`subsurface_bsdf` need re-recording (noise-level change); every other golden is
bit-identical.
