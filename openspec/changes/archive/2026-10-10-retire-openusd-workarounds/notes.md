# Working notes

Measured on the session container, 4 cores, against `be551ed` (`bin_before`).
ALab and the Moana island are not available there, so the sample counts and images
cover the checked-in samples only. `samples/Kitchen_set` is not checked in either. The
ALab and island rows were measured later (2026-10-10) on the 72-vCPU / 93 GiB VM, idle
but for an idle gateway service, with the page cache warm (an untimed run first; a cold
first ALab run read 3:23 against 2:22–2:37 warm and is excluded).

## 1.2 Warning counts (baseline, `-s 16 -l debug`, every sample)

| warning | scenes | count |
|---|---|---|
| `Nested native instance … skipped` | none | 0 |
| `could not decode the xformOp stack` | `PointInstancedMedCity.usd` (`/Cameras/main_cam`, a `Camera`, which already fell back to openusd) | 2 (the placement count and the traversal) |

After the change, the only warning difference across all samples is that this camera
warning is gone.

ALab (`-f 1004 --camera $CAM`) and the island (`--camera /island/cam/shotCam`), same
`bin_before`, `-s 1 -l debug`:

| warning | ALab | island |
|---|---|---|
| `Nested native instance … skipped` | 0 | 0 |
| `could not decode the xformOp stack` | 0 | 0 |

Neither scene emitted either warning, so no ALab or island image difference can come from
group 2's stacks or group 3's splice. Every other warning is unchanged from 0.7.0
(`ef14d26`) to `main` (`crust check --json -`, same frame and camera): ALab raises one
(`texture.unreadable`, a missing `tool_wrench_boxend03` roughness UDIM set), the island 82
(47 `mesh.displaced_at_cage`, 35 `instancing.empty_prototype`), with the same codes,
counts and prims on both sides. None appears or disappears.

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
regression, so `xformOpOrder` is not read once.

ALab (`-n 3 -x "-f 1004 --camera $CAM -s 1"`), `bin_before` against `ef14d26` (phase 1
on openusd 0.7.0, the build phase 2 is measured against):

| scene | phase | before | after |
|---|---|---|---|
| ALab | Parse USD stage | 148.7 / 150.4 | 146.0 / 149.6 |
| ALab | Traverse prims | 145.3 / 147.0 | 142.9 / 146.4 |

−1.8% / −0.6% and −1.7% / −0.4%, inside the spread of either side (146.0–154.3 s), at the
same 28.5 GiB peak. `ef14d26` also carries the unrelated commits between the two, none of
which touches the traversal. No regression on ALab either, so `xformOpOrder` is still not
read once.

## Phase 2: openusd `main` (`933aa8f`), against `ef14d26` (openusd 0.7.0)

### Images (`--indirect-clamp 0`, 16 spp, every `samples/*.usd*`)

37 of 38 samples are **bit-identical**. The one that differs is `animation` without
`-f`: an `xformOp` now reads its default value, as every other attribute does, so the
sphere sits at its off-screen default translate instead of holding its time-0 sample.
That is task 5.3's intended change; at `-f 1`, `5.5` and `10` it is bit-identical.
So on the samples the bump moves no pixel through transforms (`XformQuery` composes
their stacks as 0.7's `local_to_parent_transform` did), schema fallbacks (`value_at`
keeps unauthored reads `None`) or light links (openusd's `includeRoot` fallback equals the
removed one).

ALab (`-f 1004`) and the island, 16 spp, `--indirect-clamp 0`, `crust diff`:

| scene | 0.7.0 vs `main` | `main` with `CRUST_USD_MMAP=0` vs `main` |
|---|---|---|
| island | bit-identical | bit-identical |
| ALab | 13 374 of 230 400 pixels differ; relmse 26.0, trimmed 1.1e-3 | bit-identical |

The ALab difference is none of the four intended changes. A probe over the whole composed
stage (openusd `main`, frame 1004, prototypes included) finds 2 583 prims with an
`xformOpOrder` and none on a `Scope` or untyped prim, no `!resetXformStack!` after the
first entry, and no `strongerThanDescendants` binding; `-f` is given, so default-vs-time-0
does not apply either. It is the stoat. The same dump under 0.7.0 and `main` (every prim
whose path contains `stoat`, every authored attribute's value at 1004 and the local
matrix) finds every attribute value identical (points, clips and all) and the local matrix
different on exactly eight prims, all `translate`/`orient`/`scale` stacks with a `quatf`
orient:

- `/root/stoat/body_M_hrc`, `/root/stoat/outfit_M_hrc`, `/root/stoat/backpack_M_hrc`
- `…/backpack_M_hrc/GEO/helmet_M_hrc/buttons_M_hrc/buttonsLeft_M_hrc/buttonsLeft03_M_geo`
- `…/buttonsRight_M_hrc/buttonsRight03_M_geo`
- `…/helmet_M_hrc/watchStrap_M_hrc/buckle01_M_geo`, `buckle02_M_geo`, `buckleTongue_M_geo`

`main` normalises the quaternion (`gf::Quatd::from(q).normalize()` in `XformOp`'s
`Orient` arm); 0.7 built the matrix from it as authored. The stoat's are ~1.1e-8 off unit
length, so entries move by ≤ 2e-8, which after crust's cast changes one to four f32
entries per matrix by an ulp; the root three carry the whole character. The BVH sees it
(16 fewer packet lanes of 31 222 264), and the diff map is the stoat and fur plus scattered
pixels whose paths cross it. C++ USD normalises as well, by another route
(`GfRotation::SetQuat`), and matches neither bit-for-bit here (both sides are 1 ulp from
it at f32). So it is a deterministic ulp move, like D2's, not a misplacement, but it is
not on the list of intended changes.

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
time.

ALab (`-x "-f 1004 --camera $CAM -s 1"`) and the island (`-x "--camera
/island/cam/shotCam -s 1"`), seconds, min / mean. A is 0.7.0 (`ef14d26`), B the branch
head with `CRUST_USD_MMAP=0`, C the branch head. Each binary tees its `--stats` report,
so one interleaved run gives both phases and the peak:

| scene | runs a side | phase | A | C | A → C |
|---|---|---|---|---|---|
| ALab | 4 | Parse USD stage | 142.0 / 146.6 | 120.5 / 122.8 | −15.1% / −16.2% |
| ALab | 4 | Traverse prims | 139.0 / 143.5 | 117.6 / 119.7 | −15.4% / −16.6% |
| island | 4 | Parse USD stage | 204.1 / 212.7 | 192.8 / 197.8 | −5.5% / −7.0% |
| island | 4 | Traverse prims | 159.4 / 167.8 | 149.5 / 151.6 | −6.2% / −9.7% |

| scene | runs a side | phase | B | C | B → C |
|---|---|---|---|---|---|
| ALab | 3 | Parse USD stage | 126.1 / 127.8 | 122.6 / 124.4 | −2.8% / −2.7% |
| ALab | 3 | Traverse prims | 123.0 / 124.7 | 119.5 / 121.1 | −2.8% / −2.9% |
| island | 4 | Parse USD stage | 191.7 / 200.7 | 191.7 / 196.4 | 0.0% / −2.1% |
| island | 4 | Traverse prims | 149.8 / 155.7 | 148.0 / 151.2 | −1.2% / −2.9% |

The island B → C is noise: its two interleaved pairs gave −7% and +1%.

Peak RSS (`--stats`, every timed run; the parse phase's peak column is within 0.1 GiB of
it):

| scene | A | B | C |
|---|---|---|---|
| ALab | 28.45–28.53 GiB | 18.95–19.00 GiB | 15.42–15.57 GiB |
| island | 23.71–23.92 GiB | 23.71–23.93 GiB | 23.60–23.91 GiB |

On ALab openusd `main` takes 9.5 GiB off the peak and the mapping a further 3.5 GiB
(−46% in all). The island's peak does not move: its import composes one masked stage per
chunk and drops it, so the peak is crust's own geometry and acceleration structure.
