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
whose centres surround p. **Only the trained ones among them take part**, with
their weights renormalised; a point with no trained cell around it reads the
power table, as an untrained cell did. The proposal had untrained corners
contribute the power table, and that is wrong for the reason the ALab finding
gives: receivers lie on surfaces, so half of a floor point's corners are empty
cells, and mixing the power table back in at every surface is what put ALab's
hidden exterior lights back into the picks. The blend is a pmf, since every
input is one, and a deterministic function of p. The blended **CDF** is what
is evaluated — the weighted sum of the corners' CDFs, which is the CDF of the
blended pmf, monotone because rounding keeps order — so NEE picks by binary
search over it and a light's probability is its interval of it, the same f32
on both MIS sides. Only CDFs are stored now. The blend extends a trained
table half a cell into untrained space; at the frontier of trained space the
probability still jumps to the power table, which both sides see alike.
Alternative: smaller cells. That would halve the receivers per cell and so
double the training noise in every table, without removing the discontinuity.

### D2. A floor for lights seen nearby

Today a light seen by one training receiver in a cell can sit at the uniform
floor `0.3/n`, the same as a light never seen there. Where it is in fact visible
but was under-sampled, its contribution divided by a small pmf becomes a
firefly. The floor for a light seen anywhere in the cell or its 26 neighbours
becomes `max(0.3/n, f / n_seen)`, where `n_seen` counts the lights seen in that
neighbourhood. `f` is chosen by measurement (task 2.3), starting at 0.3.
Unseen lights keep `0.3/n`, so the ALab finding still holds: mixing the power
table back in put hidden lights back into the picks. The floor's excess is taken
from the lights above their floor in proportion to their excess, so an unseen
light keeps exactly `0.3/n` and the table still sums to one.

**Tried and reverted: a higher floor for lights at infinity**, the uniform
`1/n_live` the power table gives them. ALab's full relMSE under today's
`learned` is 221 against 2 under `power` on one seed, the excess is NEE's (it
is there under light sampling alone), and the sun picked at `0.3/n` from a
cell the training never saw it from was the suspect. It was not: with the
floor ALab's full relMSE stayed a single-seed lottery (57 against 49 without
it), its trimmed relMSE rose 4%, and on `domelight`, whose two lights are both
at infinity, the floor forced a 50/50 table and erased the 1.24× the blend had
won there. ALab's untrimmed number is in any case unusable as a gate: the
1024 spp `power` reference itself holds a firefly of radiance 2684 that every
test image is black at.

### D3. The default flips only through a gate

**Outcome: it did not flip.** The gate ran on all 28 scenes with two or more
lights (`docs/light_sampling.md` §3.12, "The gate"): 12 pass, 16 fail —
`materialx_showcase`, `openpbr_showcase`, `aovs_lpe`, `subdivision_adaptive`,
`subdivision`, `pxr_displace`, `motionblur`, `nested_instancing`,
`materialx_cutout`, `instancing`, `domelight` (full measure only), `displacement`,
`curves`, `aovs`, `animation` and ALab (full measure only, a reference firefly).
The failures are per-sample losses on small two-light scenes, present under
today's tables too (`animation` 2.5× worse than `power` on `main`, 2.0× with
the blend), plus 10–28% more time; the blend and the floor change none of
that. `power` stays the default, D1 and D2 ship, and the spec delta below
keeps `power` as the default while adding the firefly requirement.

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
