## Context

See proposal.md (Why). `learned` is described in `docs/light_sampling.md` §3.12
and `crates/crust-core/src/light_cache.rs`:

- A deterministic pre-pass traces one camera path per 4×4 pixels with two
  bounces. At each vertex it estimates every light's NEE contribution (2
  samples per light).
- The estimates sum into a uniform grid over the receivers' 2–98% quantile
  bounds, sized for about 16 receivers per occupied cell.
- A cell with at least 2 receivers and any light seen gets
  `p = 0.7 · E/ΣE + 0.3 / n_live`. Other points use the power table.
- Both MIS sides read the stored `f32` at the NEE vertex. The tables are frozen
  before the first pass, so tiled and scanline renders stay bit-identical.

## Goals / Non-Goals

**Goals**

- Remove the full-relMSE penalty of `learned`, so it is at least as good as
  `power` at equal time on every scene measured.
- Keep everything that makes `learned` sound: the defensive share, deterministic
  tables, and one probability both MIS sides read.

**Non-Goals**

- A light tree inside the cells, online refinement of the tables between passes,
  and BSDF-aware (glossy-lobe) training. Each is a possible follow-up, once the
  gate below shows where `learned` still loses.
- Changing the pre-pass budget or the grid resolution rule.

## Decisions

### D1. Trilinear blending of neighbouring tables

The probability at point p is the trilinear blend of the tables of the 8 cells
whose centres surround p. A cell without a table contributes the power table.
The blend is a pmf, since every input is one, and it is a deterministic function
of p. NEE picks from it by CDF inversion over the blended values, and the bounce
side evaluates the same blend at the same vertex, so the MIS pair holds by
construction. Alternative: smaller cells. That would halve the receivers per
cell and so double the training noise in every table, without removing the
discontinuity.

### D2. A floor for lights seen nearby

Today a light seen by one training receiver in a cell can sit at the uniform
floor `0.3/n`, the same as a light never seen there. Where it is in fact visible
but was under-sampled, its contribution divided by a small pmf becomes a
firefly. The floor for a light seen anywhere in the cell or its 26 neighbours
becomes `max(0.3/n, f / n_seen)`, where `n_seen` counts the lights seen in that
neighbourhood. `f` is chosen by measurement (task 2.3), starting at 0.3.
Unseen lights keep `0.3/n`, so the ALab finding still holds: mixing the power
table back in put hidden lights back into the picks.

### D3. The default flips only through a gate

`learned` becomes the default only if, on every one of the checked-in samples,
`veach_mis`, the OpenPBR Shader Playground and ALab, its equal-time relMSE is no
worse than `power`'s by more than 5% on either the full or the trimmed measure.
The measurement uses 4 seeds, `--indirect-clamp 0` and `scripts/bench_ab.sh`
timing. If a scene fails, the change ships D1 and D2 with `learned` still
opt-in, and the design record names the failing scenes.

## Risks / Trade-offs

- [The blended lookup costs more per pick (8 tables)] → The tables are small
  (n lights) and cache-resident. The gate measures at equal time, and blending
  can be restricted to cells near a table discontinuity if the cost shows.
- [Single-seed full relMSE swings by fireflies] → 4 seeds per scene in the gate.
- [A default flip changes every golden image] → `check_images.sh` re-records
  once. A test asserts that `power` stays bit-identical to the pre-change
  renderer.

## Migration Plan

Users who need yesterday's images pass `--light-selection power` or author
`crust:lightSelection = "power"`. The release notes and the site say so.
