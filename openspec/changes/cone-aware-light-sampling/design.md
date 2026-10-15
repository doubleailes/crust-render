## Context

- Shaping is a property of the emitter. `Emissive::radiance_toward` multiplies by
  `Shaping::factor(emission_dir)`, and both MIS halves call it. The light shapes
  (`RectShape`, `AffineShape`) know nothing of it. `AreaLight::new` already pushes one
  emitter property into the shape: one-sidedness, through `set_front_only`.
- NEE tests radiance first, so a sample the cone kills costs no shadow ray. It still
  wastes the sample.
- `Shaping::factor` is exactly zero at and beyond `θc` (the smoothstep's top end), so
  removing that region from the sampled domain is unbiased. That holds for focus and
  IES too, since both multiply the cone factor.
- Rect lights sample the spherical rectangle (Ureña et al.) in an f64 `RectFrame`,
  with area sampling below 1e-4 sr and above 6.22 sr. Disk lights sample by area by
  default. `CRUST_DISK_SAMPLING = ellipse` samples the spherical ellipse in a 0.1–6.22
  sr band. It costs ~5,000 instructions per sample and loses on glossy receivers and
  small disks (lighting design record, "Disk and tube lights").
- The archived sweep for that work was never checked in, so its numbers cannot be
  reproduced. This change does not repeat that.

## Goals / Non-Goals

**Goals:**
- Reproducible measurement scenes and a baseline that say how large cases (A) and (B)
  are before any sampler is built.
- For case (B), two strategies that remove only the zero-radiance part of a shaped
  rect or disk. Each sits behind a switch whose off side is bit-identical to today.
- A disk strategy cheap enough to compete with `area` at equal time, which the
  ellipse cannot.

**Non-Goals:**
- Selection code. Case (A) is measured and handed to
  `learned-light-selection-by-default` or a future light tree.
- Sphere and cylinder lights. A small sphere's cone gates whole receivers, which is
  case (A). A tube's shaping axis is not its wall normal.
- Sampling the IES distribution.
- Sheared rects, elliptical disks, and lights whose shaping axis is not their normal.
  They keep today's sampler.

## Decisions

### D1. The scenes come first, and are checked in

- `samples/spotlights.usda`: a stage with six small spots on disjoint pools (four
  sphere lights of radius 0.05 with 20–30° cones, two IES fixtures from
  `samples/ies/spot30.ies`), a dim dome or rect fill so the gaps aren't black, and
  one wide shaped disk close to a subject (radius 0.6, height 1.5, 25° cone) so the
  sample exercises case (B) too. It loads in the integration tests like every sample,
  and `check_images.sh` covers it.
- `scripts/make_spot_sweep.py` writes the sweep to a directory given as an argument.
  Its output is not checked in. Each frame is a 40 × 40 floor, diffuse (UsdPreviewSurface
  roughness 1) or glossy (metallic 1, roughness 0.25) as in the archived sweep, under
  one light 1 unit up:
  - shaped: {rect, disk} × light half-size / `tan θc` ∈ {0.1, 0.5, 1, 3} × θc ∈ {10°,
    30°, 60°} × (softness, focus) ∈ {(0, 0), (0.4, 2)} × {diffuse, glossy}, 96 frames;
  - unshaped disks: radius/height ∈ {0.1, 0.3, 1, 3} × {diffuse, glossy}, the
    archived ellipse rows.
- The baseline and every later measurement go in the change's `notes.md`, with the
  exact command lines.

*Alternative:* a scratchpad sweep like the last one. Rejected because nobody could
check its numbers afterwards.

### D2. Measurement protocol and gates

- **Error.** relMSE from `crust diff` against a 1024 spp reference rendered with
  `area` / `off`, at 16 spp, `--indirect-clamp 0`, mean over four seeds.
- **Time.** The Render phase, min of four runs, interleaved by `bench_ab.sh`. Sweep
  frames are too short to time per sample, so price samples on the stage and on
  `usdlux`, and use callgrind instruction counts for the per-sample cost.
- **Equal-time ratio** = (relMSE × time) of the old strategy over the new one.
  Ratios within ±5% count as ties, the seed-to-seed spread seen before.
- **Gate (A).** Record power vs `learned` relMSE and time on the stage, overall and
  in the pools. Nothing is built either way.
- **Gate (B), clip.** `CRUST_CONE_CLIP` defaults to `on` if, at equal time, the
  shaped rect rows and the stage are better in mean and no case is worse than a tie.
- **Gate (B), polar.** `polar` becomes the disk default if it is better than `area`
  at equal time on every unshaped and shaped disk row (diffuse and glossy), the
  stage and `usdlux`, with ties allowed. If it wins only the shaped rows, the
  default stays `area` and the result is recorded. Making the strategy depend on
  whether the light is shaped is a separate decision (Open Questions).
- A default that flips moves the spec text ("off, the default" or the disk default)
  and the docs in the same task.

### D3. The cutoff region is a circle about the foot point

Let the receiver be `x`, at height `h` in front of the light's plane, with foot point
`f`. If the shaping axis `a` equals the plane's normal, the direction from `p` to `x`
makes angle θ with `a`, where `tan θ = |p − f| / h`. So `p` emits toward `x` exactly
when `|p − f| ≤ ρc = h · tan θc`. Everything in `Shaping` except IES depends only on
`|p − f| / h`.

- The condition is checked once, at light construction: `|a · n| ≥ 1 − 1e-6` in f64,
  the tolerance `RECT_ORTHOGONALITY` already uses. Otherwise there is no clip.
- `AreaLight::new` passes the shape a `ConeCut` (axis, cutoff, focus) from `Emissive`,
  as `set_front_only` passes one-sidedness. The cutoff is `None` for `θc ≥ 90°`. Focus
  alone is still passed, because `polar` follows it. A shaping with neither passes
  nothing.
- **Trap: the cap must be conservative.** `factor` computes θ with `acos` in f32, the
  cap uses `tan` in f64. A point the cap excludes can still have a few ulps of
  radiance from rounding. A relative inflation of `ρc` (the 1e-5 first planned here)
  is not enough: an f32 `acos` near a small angle θ errs by about `ε / sin θ`, which
  relative to θ grows as `1/θ²`, ~7e-4 at 1°. So the margin is taken in cosine:
  `θcut = acos(cos θc − 1e-6)`, about 17 f32 ulps of a cosine near 1, and
  `ρc = h · tan θcut`. `nothing_outside_the_cutoff_radius_is_lit` pins it: cones of
  0.5–85°, heights 1e-3–100, with softness and focus.

### D4. The clipped rect reuses the spherical rectangle

- In `RectFrame` coordinates, intersect `[0, w] × [0, h]` with
  `[fx − ρc, fx + ρc] × [fy − ρc, fy + ρc]`. An empty intersection means no light:
  `sample` and `pdf` return `None`, so NEE skips the light and the bounce side sees
  zero radiance there anyway.
- Run the existing map on the sub-rectangle by shifting the frame's origin and
  replacing its width and height. The solid-angle thresholds apply to the
  sub-rectangle. Below 1e-4 sr, sample it uniformly by area, still clipped.
- The bounce side computes the same sub-rectangle from `from`. A hit outside it
  gets `None` from `pdf`, which follows the existing "a strategy that exists is
  final" rule.
- The square loses at most `1 − π/4` (21%) of its samples in the corners outside the
  circle.

*Alternatives:*
- Sampling the lens (circle ∩ rect) exactly: no closed form that keeps `openqmc`'s
  stratification.
- Rejection: forbidden, because it breaks stratification.

### D5. The polar disk sampler

The disk has world radius `R` and centre `c`, with `D = |c − f|`.

- φ is uniform over the wedge the disk subtends from `f`: 2π if `D < R`, otherwise
  `2 asin(R / D)` about the direction `c − f`. Along φ the disk spans `[ρ1, ρ2]`, the
  ray–circle intersections (`ρ1 = 0` when `f` is inside the disk). Cap
  `ρ2 ← min(ρ2, ρc)`, and drop a slice where `ρ1 ≥ ρc`.
- ρ is drawn from `g(ρ) ∝ ρ (h² + ρ²)^(−(k+3)/2)`, whose CDF is closed form in
  `(h² + ρ²)^(−(k+1)/2)`. `k = 0` gives `cos θl / r²` exactly. `k = focus` also
  follows the focus falloff. Focus tint is a colour, so it stays out of the density.
- **Solid-angle pdf.** For `k = 0` it is constant along a slice:
  `1 / (Δφ · (cos α1 − cos α2))`.
  - **Trap:** never form that difference directly. For a thin slice far away it
    cancels, as the tube's two `atan`s did. Use
    `h (ρ2² − ρ1²) / (d1 d2 (d1 + d2))`, with `ρ2² − ρ1²` from the intersection's
    discriminant.
  - For `k > 0` the pdf carries `(h/d)^k` against the slice's normaliser, built the
    same cancellation-free way.
- **When it applies.** The world disk must be round: the two world in-plane axes of
  `M` are equal in length and perpendicular within 1e-6, checked once. `h` must be
  above `1e-4 R`. Otherwise sample by area.
- **Cost.** One `asin`/`atan2` pair for φ, one ray–circle intersection, one
  `powf` for `k > 0`. Roughly 100–200 instructions, against the ellipse's ~5,000.
  callgrind measures it as D2 says.
- **Out of line, answering in registers**, like the other round strategies, so the
  sampler doesn't grow (`round_strategies_do_not_grow_the_sampler`).

*Alternatives:*
- The spherical ellipse with a cone cap: keeps its cost, and capping an elliptic map
  has no closed form.
- Area sampling of the cap disk alone (uniform on `min(disk, cutoff circle)`):
  cheaper, but blind to `1/r²` and `cos θl`. It is the fallback if polar loses on
  cost.

### D6. Switches

- `CRUST_CONE_CLIP` (`off` | `on`) is one switch for both the rect clip and the
  polar cap. The cap is the same physics in both, and one A/B prices it.
- `polar` is a third value of the existing `CRUST_DISK_SAMPLING`. It is an
  alternative disk strategy, not a modifier.
- Both are `crust_core::Config` fields read in `config.rs`, with a row in
  `docs/architecture.md` § Environment switches and a section in
  `site/content/docs/reference/environment-variables.md`.

## Risks / Trade-offs

- [Polar stratifies worse than the concentric area map for small disks, as the
  ellipse did at ~0.03 sr] → The sweep's r/h = 0.1 rows are in the gate. If it loses
  there, the default stays `area`.
- [Glossy receivers prefer area sampling's `r²/cos θl` density (lighting design
  record §3.9)] → The glossy rows are in the gate. Polar is not uniform in solid
  angle overall, so the result is not predetermined.
- [The clip pushes the spherical rectangle below its 1e-4 sr floor more often] →
  That floor already falls back to area, now on the sub-rect.
- [Case (A) dominates and the samplers buy little on real scenes] → The baseline
  comes first. The gates allow "built, measured, left off", recorded as such.
- [A shaping-aware shape couples `light/` to `lux::Shaping`] → The shape receives
  three numbers, not the `Shaping`. `Emissive` remains the one answer to radiance.

## Migration Plan

With both switches at their defaults (`off`, `area`), every render is bit-identical,
pinned by `check_images.sh`. If a gate flips a default, the goldens are re-recorded in
the same task, and the old behaviour stays one environment variable away.

## Open Questions

- If polar wins only on shaped disks, should the default depend on whether the disk
  is shaped? The baseline decides whether the question arises at all.
