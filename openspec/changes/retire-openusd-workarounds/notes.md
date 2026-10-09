# Working notes

Measured on the session container, 4 cores, against `be551ed` (`bin_before`).
ALab and the Moana island are not available here, so every count and image below
covers the checked-in samples only. `samples/Kitchen_set` is not checked in either.

## 1.2 Warning counts (baseline, `-s 16 -l debug`, every sample)

| warning | scenes | count |
|---|---|---|
| `Nested native instance … skipped` | none | 0 |
| `could not decode the xformOp stack` | `PointInstancedMedCity.usd` (`/Cameras/main_cam`, a `Camera`, which already fell back to openusd) | 2 (the placement count and the traversal) |

After the change, the only warning difference across all samples is that this camera
warning is gone.

## 2.4 / 3.3 Images (`--indirect-clamp 0`, `bin_before` vs the change)

23 of 36 samples are bit-identical at 16 spp. No sample emitted the nested-instance
skip warning, so the splice (group 3) changes no sample, and every difference below is
group 2.

- `animation`: no `-f` now reads the sphere's `xformOp:translate` at time 0, holding
  frame 1, instead of its off-screen default (design, Risks: "Default value vs.
  time 0"). Expected; the test and the user docs are updated, and phase 2 removes it.
- The other 12, as relmse / relmse trimmed of the worst 0.1% of pixels:

| scene | 16 spp | 64 spp | 256 spp |
|---|---|---|---|
| `domelight` | 5.1e-14 / 1.7e-15 | 1.1e-07 / 2.8e-15 | 1.1e-08 / 8.1e-15 |
| `fog` | 7.2e-15 / 3.9e-15 | 4.3e-15 / 3.6e-15 | 3.6e-15 / 3.4e-15 |
| `light_linking` | 1.1e-13 / 7.8e-14 | 4.3e-14 / 3.5e-14 | 9.5e-10 / 2.1e-14 |
| `materialx_cutout` | 1.3e-07 / 6.4e-13 | 1.7e-07 / 1.1e-12 | 1.8e-07 / 1.3e-12 |
| `materialx_subsurface` | 1.8e-09 / 5.1e-13 | 4.3e-08 / 7.3e-13 | 1.3e-08 / 9.4e-13 |
| `nested_instancing` | 3.0e-09 / 4.7e-13 | 5.3e-09 / 1.7e-12 | 1.0e-09 / 2.0e-12 |
| `pxr_displace` | 9.7e-08 / 1.9e-15 | 5.3e-08 / 1.8e-14 | 1.6e-08 / 6.4e-14 |
| `smoke` | 3.9e-15 / 3.6e-15 | 3.6e-15 / 3.3e-15 | 5.0e-14 / 3.2e-15 |
| `subdivision_adaptive` | 7.3e-19 / 0.0e+00 | 7.8e-19 / 0.0e+00 | 6.7e-19 / 0.0e+00 |
| `thin_window` | 5.0e-14 / 4.9e-14 | 4.9e-14 / 4.8e-14 | 5.5e-12 / 4.8e-14 |
| `usdlux` | 5.7e-08 / 6.9e-15 | 6.1e-09 / 4.6e-15 | 4.6e-10 / 4.1e-15 |

Trimmed relmse is ≤ 2e-12 everywhere, about 1e-6 relative RMS (f32 ulps), and flat
across spp. That is a deterministic move, not noise. Untrimmed relmse is dominated by a
few edge pixels. It is erratic above 16 spp because adaptive sampling cascades, and it
**plateaus** on `materialx_cutout` (1.3e-7 → 1.7e-7 → 1.8e-7, whose geometry is placed
by `rotateY`). That is consistent with D2: a silhouette or cutout edge pixel flips when
f64 trigonometry moves the geometry by an ulp relative to the old f32 composer. No stack
in the samples composed wrongly before (1.2), so none of this is a fixed bug either.
The 1/√N criterion in 2.4 is therefore not met as written: the differences are a
deterministic ulp-level placement change, not noise.

## 2.5 Import A/B (`bench_ab.sh -n 15 -x "-s 1"`, seconds, min / mean)

| scene | phase | before | after |
|---|---|---|---|
| cornellbox | Parse USD stage | 0.026 / 0.028 | 0.027 / 0.030 |
| cornellbox | Traverse prims | 0.020 / 0.021 | 0.020 / 0.022 |
| PointInstancedMedCity | Parse USD stage | 0.004 / 0.005 | 0.004 / 0.005 |
| PointInstancedMedCity | Traverse prims | 0.002 / 0.003 | 0.002 / 0.003 |

Below the noise floor, so instructions were counted (callgrind, `RAYON_NUM_THREADS=1`,
cornellbox `-s 1`): `load_scene` inclusive 252.94 M → 252.67 M (−0.1%). Not a
regression, so `xformOpOrder` is not read once. ALab (`-f 1004`) is still to measure.

## Phase 2: openusd `main` (`933aa8f`), against `ef14d26` (openusd 0.7.0)

### Images (`--indirect-clamp 0`, 16 spp, every `samples/*.usd*`)

37 of 38 samples are **bit-identical**. The one that differs is `animation` without
`-f`: an `xformOp` now reads its default value, as every other attribute does, so the
sphere sits at its off-screen default translate instead of holding its time-0 sample.
That is task 5.3's intended change; at `-f 1`, `5.5` and `10` it is bit-identical.
So the bump moves no pixel through transforms (`XformQuery` composes as 0.7's
`local_to_parent_transform` did), schema fallbacks (`value_at` keeps unauthored reads
`None`) or light links (openusd's `includeRoot` fallback equals the removed one).

### Import A/B (`bench_ab.sh -n 15 -x "-s 1"`, seconds, min / mean)

| scene | phase | 0.7.0 | `main` |
|---|---|---|---|
| cornellbox | Parse USD stage | 0.019 / 0.021 | 0.014 / 0.014 |
| cornellbox | Traverse prims | 0.015 / 0.015 | 0.008 / 0.009 |
| hair | Parse USD stage | 0.007 / 0.007 | 0.008 / 0.008 |
| hair | Traverse prims | 0.004 / 0.004 | 0.003 / 0.004 |
| PointInstancedMedCity | Parse USD stage | 0.003 / 0.003 | 0.004 / 0.004 |
| materialx_lion | Parse USD stage | 0.001 / 0.001 | 0.003 / 0.003 |
| openpbr_showcase | Parse USD stage | 0.003 / 0.003 | 0.005 / 0.005 |

The ~2 ms (and ~1.3 MiB) every scene now pays once is building the schema registry
(`openusd_schemas::schema_registry()`); past it, cornellbox imports in about half the
time. ALab and the Moana island, where mxpv/openusd#106's gains are measured, are not
available in the session container: still to measure.
