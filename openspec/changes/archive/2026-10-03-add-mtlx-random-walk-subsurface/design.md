# Design

## Context

The closure tree collapses at a vertex into weighted leaves (`closure/mod.rs`);
the integrator (`tracer/path.rs`) samples one, runs NEE over all, and continues.
A random walk does not fit a BSDF: its "outgoing direction" leaves the surface
somewhere else. Typhoon's answer, which this follows, is to make the walk a
single surface event the integrator runs between two vertices.

## Decisions

- **D1 — Typhoon's walk, ported line for line.** Chiang remap (coefficients
  checked number-for-number against `sss.cpp`), the 0.2 albedo floor and its
  throughput correction, channel MIS, Dwivedi forward/backward guiding, the
  extended first ray, the similarity relation after 9 bounces. Deviations, each
  found by a failing test: (a) only the entry segment is offset — offsetting
  scatter points let walks step out through the boundary; (b) a boundary met
  from the entry's side ends the walk as lost; (c) the anisotropy is clamped to
  `[0, 0.99]`, since Chiang's fit has no `g < 0` branch (Cycles clamps the same).
- **D2 — The leaf carries no value; its selection carries the entry.**
  `Lobe::Subsurface` evaluates to zero; `scatter` returns a delta
  `ScatterSample` with `subsurface: Some(leaf index)`, and the tracer asks the
  `ShadingPoint` for the walk's parameters. An index, not the parameters: the
  byte fits in padding, the parameters grew every sample by 64 bytes.
- **D3 — The entry interface is the nearest dielectric layered over the leaf**,
  found while the collapse walks the tree, else Typhoon's defaults.
- **D4 — The exit is the next vertex, on `ExitLambertian`.** The walk's last
  hit record, turned to face outward, is shaded without tracing a segment;
  `prev = None`, no emission, no volume segment. The entry's record carries the
  walk's throughput; nothing inside the walk is a vertex.
- **D5 — The owner is the hit's `geom_id`.** Other geometry is stepped past, as
  Typhoon traces the owner's prototype scene alone.
- **D6 — Off the per-vertex path.** Walk and exit are `#[cold]` and out of
  line, the pending exit is a flag plus a slot in `PathScratch`, and walk rays
  use the tracer's `(0.001, ∞)` interval by moving their origin, so LLVM keeps
  propagating those constants into the kernel.

## Risks / Trade-offs

- The refracted entry reflects less than the authored colour (Chiang's fit is
  for a diffuse entry): (0.78, 0.45, 0.16) for (0.8, 0.5, 0.2). Followed, as
  Typhoon's behaviour; measured and pinned rather than tuned away.
- Brute-force walks are noisy on thin backlit features; there is no BSSRDF
  importance sampling or NEE inside the medium.
- The native `crust:openpbr` subsurface is unchanged (tinted diffuse).
