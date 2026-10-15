# Notes: cone-aware-light-sampling

Measurements and command lines for the tasks. `$S` is a scratch directory and
`crust` is `target/release/crust`.

## 1. Measurement scenes

### 1.1–1.2 `samples/spotlights.usda`

- `crust check -i samples/spotlights.usda --json -` reports no warnings.
- `crust render -i samples/spotlights.usda -s 16 --stats` takes 0.72 s for the Render
  phase at 640 × 360.
- Exposure: the first draft had every light at intensity 20. Normalised IES fixtures
  came out ~4× brighter than the bare sphere spots, and the wide disk saturated. The
  sphere spots are now 60, the IES fixtures 20 and the disk 6.

Attribution, `crust light-paths -i samples/spotlights.usda -s 64 --indirect-clamp 0`
(power selection), with `--group-by light` for the first three rows and
`--group-by expr+light` for the last two:

| region (px) | where | direct light from | own spot's NEE hit rate |
|---|---|---|---|
| 190,152,200,157 | pool 1 (Spot1) | Spot1 76%, Fill 24% | 7.4% |
| 275,205,285,210 | pool 5 (Ies1) | Ies1 97%, Fill 3% | 24.9% |
| 395,155,405,160 | pool 3 (Spot3) | Spot3 70%, Fill 29% | 7.7% |
| 243,203,250,208 | gap between pools 4 and 5 | `C<RD>L` from Fill only (94% of the luminance); the rest is multi-bounce | n/a |
| 190,152,200,157 | pool 1, after rebalancing | `C<RD>L` from Spot1 90%, Fill 10% | 8.1% |

Each pool's direct light comes from its own spot alone. The hit rate is already
case (A) in numbers: power selection gives a sphere spot about 1 pick in 13 at a
point that only it can light. The IES fixtures get more picks because their
normalised profile makes them more powerful.

### 1.3 Sweep

`python3 -I scripts/make_spot_sweep.py $S/sweep` writes 104 stages: 96 shaped
(2 shapes × 4 ratios × 3 cones × 2 shapings × 2 floors) and 8 unshaped disks.
`crust check` reports no warnings on any of them.

### 1.4 Goldens

`scripts/check_images.sh record $S/goldens` with the unmodified binary (`main` at
c88a7f0), then `check`: every checked-in sample, `spotlights` included, is
bit-identical (exit 0). `kitchen` and `kitchen_instanced` fail because
`samples/Kitchen_set/` is a downloaded asset that is not in this environment. That
predates this change. These goldens are the bit-identity reference for tasks 4.3 and
6.1.

## 2. Baseline

All with the unmodified binary (c88a7f0), saved as `$S/bin_base` before any code
change.

### 2.1 References

`$S/refs.sh`: 1024 spp, `--indirect-clamp 0`, seed frame 99 (independent of the test
seeds 1–4), today's defaults (power selection, `CRUST_DISK_SAMPLING=area`).
- `spotlights` and `usdlux` render through a wrapper layer that sublayers the sample and
  overrides `crust:minSamplesPerPixel = 100000`, so no reference pixel stops early. At
  16 spp the wrapper renders bit-identically to the sample itself (`crust diff`: 0
  differing pixels).
- The sweep frames already author that minimum.
- 106 references; 9 min 56 s.

The test runs (`$S/measure.py`) render each scene at 16 spp, `--indirect-clamp 0`,
frames 1–4. relMSE (mean of the four) is from `crust diff --json` against the
reference. Time is `--stats-json`'s Render phase, which includes `learned`'s
pre-pass. A sweep frame renders in ~40 ms, too short to time a sample: its times are
indicative only, as in the archived sweep.

### 2.2 Gate (A): power vs `learned` selection

| scene | power relMSE | learned relMSE | ratio (trimmed) | Render s, power / learned (`bench_ab.sh`, 6 reps, min / mean) | equal time |
|---|---|---|---|---|---|
| `spotlights` | 9.40e-3 | 2.08e-3 | 4.52 (6.30) | 0.577 / 0.646 vs 0.612 / 0.680 (+6.1% / +5.3%) | 4.3× |
| `spotlights`, pools only | 7.35e-2 | 1.17e-2 | 6.28 | as above | 5.9× |
| `spotlights`, outside the pools | 2.29e-3 | 1.01e-3 | 2.26 | as above | 2.1× |
| `usdlux` | 7.80e-2 | 2.33e-2 | 3.34 (3.35) | 1.140 / 1.222 vs 1.114 / 1.282 (−2.3% / +4.9%) | 3.2–3.4× |

"Pools" are the pixels whose reference luminance exceeds 0.05, 10% of the frame (the
fill alone gives ~0.014). relMSE there is crust's formula over that mask
(`$S/regional.py`, which reproduces `crust diff`'s whole-image relMSE exactly).

**Conclusion (gate A): `learned` selection removes most of case (A)'s waste.** In
the pools it lowers relMSE 6.3× for 5–6% more Render time. It sends a point's picks
to the spot that lights it rather than to the five whose cones exclude it, which is
what a cone-aware light tree would do, without one. So nothing here argues for
selection code in this change. It is evidence for `learned-light-selection-by-default`:
a scene where `learned` wins 4.3× at equal time, with no firefly regression in the
trimmed relMSE (6.3× trimmed against 4.5× full).

### 2.3 Case (B) baseline

**Zero-sample share** (`$S/zero_fraction.py`, a numeric probe, not committed): the
share of today's NEE light samples that land outside the cutoff circle. It is
averaged over the floor points the light reaches, uniformly by area and weighted by
the direct irradiance each point gets (where noise is visible). Today a disk is
sampled uniformly by area and a rect uniformly in solid angle. The probe is
independent of softness and focus, which do not move the cutoff.

| shape | ratio | cone | zero share (area mean) | zero share (irradiance-weighted) |
|---|---|---|---|---|
| rect | 0.1 | 10 | 20.6% | 6.5% |
| rect | 0.1 | 30 | 20.2% | 5.0% |
| rect | 0.1 | 60 | 19.4% | 1.7% |
| rect | 0.5 | 10 | 60.7% | 31.5% |
| rect | 0.5 | 30 | 56.6% | 25.4% |
| rect | 0.5 | 60 | 46.9% | 10.4% |
| rect | 1 | 10 | 77.5% | 58.6% |
| rect | 1 | 30 | 68.8% | 47.5% |
| rect | 1 | 60 | 50.4% | 21.2% |
| rect | 3 | 10 | 92.4% | 89.7% |
| rect | 3 | 30 | 78.5% | 72.7% |
| rect | 3 | 60 | 48.7% | 35.8% |
| disk | 0.1 | 10 | 16.8% | 5.6% |
| disk | 0.1 | 30 | 16.8% | 4.5% |
| disk | 0.1 | 60 | 16.8% | 1.7% |
| disk | 0.5 | 10 | 55.1% | 28.1% |
| disk | 0.5 | 30 | 55.2% | 25.6% |
| disk | 0.5 | 60 | 55.2% | 15.5% |
| disk | 1 | 10 | 74.7% | 54.2% |
| disk | 1 | 30 | 74.7% | 53.2% |
| disk | 1 | 60 | 74.8% | 48.8% |
| disk | 3 | 10 | 93.7% | 90.9% |
| disk | 3 | 30 | 93.7% | 91.0% |
| disk | 3 | 60 | 93.7% | 90.7% |

The waste grows with the light's size relative to its cutoff circle. At ratio 0.1
(a source ten times narrower than its beam) under 7% of the irradiance-weighted
samples are wasted, so in-light sampling cannot help and a small source is case (A).
At ratio 1, half are wasted; at ratio 3, 73–91%.

**Disk rows, `area` vs `ellipse`.** "Equal time" is
`(relMSE · t_min)_area / (relMSE · t_min)_ellipse`; above 1, `ellipse` wins.

| disk row | area relMSE | ellipse relMSE | relMSE ratio | equal time |
|---|---|---|---|---|
| `disk_k0.1_c10_s0.4_f2_diffuse` | 2.268e-05 | 2.455e-05 | 0.92 | 0.45 |
| `disk_k0.1_c10_s0.4_f2_glossy` | 6.180e-07 | 1.347e-06 | 0.46 | 0.24 |
| `disk_k0.1_c10_s0_f0_diffuse` | 1.838e-04 | 1.856e-04 | 0.99 | 0.49 |
| `disk_k0.1_c10_s0_f0_glossy` | 6.352e-06 | 7.200e-06 | 0.88 | 0.49 |
| `disk_k0.1_c30_s0.4_f2_diffuse` | 3.695e-05 | 4.279e-05 | 0.86 | 0.40 |
| `disk_k0.1_c30_s0.4_f2_glossy` | 1.579e-05 | 1.910e-05 | 0.83 | 0.39 |
| `disk_k0.1_c30_s0_f0_diffuse` | 4.277e-04 | 4.336e-04 | 0.99 | 0.52 |
| `disk_k0.1_c30_s0_f0_glossy` | 3.445e-04 | 3.480e-04 | 0.99 | 0.60 |
| `disk_k0.1_c60_s0.4_f2_diffuse` | 3.051e-05 | 4.151e-05 | 0.73 | 0.44 |
| `disk_k0.1_c60_s0.4_f2_glossy` | 2.469e-04 | 3.096e-04 | 0.80 | 0.52 |
| `disk_k0.1_c60_s0_f0_diffuse` | 1.191e-04 | 1.328e-04 | 0.90 | 0.51 |
| `disk_k0.1_c60_s0_f0_glossy` | 2.256e-04 | 2.992e-04 | 0.75 | 0.37 |
| `disk_k0.5_c10_s0.4_f2_diffuse` | 2.214e-04 | 7.090e-05 | 3.12 | 1.95 |
| `disk_k0.5_c10_s0.4_f2_glossy` | 5.468e-06 | 5.372e-06 | 1.02 | 0.54 |
| `disk_k0.5_c10_s0_f0_diffuse` | 3.785e-04 | 2.188e-04 | 1.73 | 0.86 |
| `disk_k0.5_c10_s0_f0_glossy` | 1.344e-05 | 1.508e-05 | 0.89 | 0.39 |
| `disk_k0.5_c30_s0.4_f2_diffuse` | 4.918e-04 | 8.439e-05 | 5.83 | 2.81 |
| `disk_k0.5_c30_s0.4_f2_glossy` | 2.319e-04 | 1.015e-04 | 2.29 | 1.09 |
| `disk_k0.5_c30_s0_f0_diffuse` | 1.207e-03 | 4.991e-04 | 2.42 | 1.05 |
| `disk_k0.5_c30_s0_f0_glossy` | 1.189e-03 | 1.467e-03 | 0.81 | 0.40 |
| `disk_k0.5_c60_s0.4_f2_diffuse` | 1.237e-03 | 5.377e-05 | 23.00 | 8.77 |
| `disk_k0.5_c60_s0.4_f2_glossy` | 1.918e-03 | 1.134e-03 | 1.69 | 0.70 |
| `disk_k0.5_c60_s0_f0_diffuse` | 1.083e-03 | 1.634e-04 | 6.63 | 2.16 |
| `disk_k0.5_c60_s0_f0_glossy` | 2.780e-03 | 1.468e-03 | 1.89 | 0.64 |
| `disk_k1_c10_s0.4_f2_diffuse` | 4.103e-04 | 7.985e-05 | 5.14 | 2.50 |
| `disk_k1_c10_s0.4_f2_glossy` | 6.973e-06 | 2.407e-06 | 2.90 | 1.99 |
| `disk_k1_c10_s0_f0_diffuse` | 6.732e-04 | 1.989e-04 | 3.38 | 2.01 |
| `disk_k1_c10_s0_f0_glossy` | 1.564e-05 | 9.800e-06 | 1.60 | 0.98 |
| `disk_k1_c30_s0.4_f2_diffuse` | 1.523e-03 | 1.724e-04 | 8.83 | 3.88 |
| `disk_k1_c30_s0.4_f2_glossy` | 5.533e-04 | 2.745e-04 | 2.02 | 0.90 |
| `disk_k1_c30_s0_f0_diffuse` | 2.416e-03 | 6.210e-04 | 3.89 | 1.80 |
| `disk_k1_c30_s0_f0_glossy` | 4.848e-03 | 3.588e-03 | 1.35 | 0.72 |
| `disk_k1_c60_s0.4_f2_diffuse` | 2.692e-03 | 6.835e-04 | 3.94 | 1.26 |
| `disk_k1_c60_s0.4_f2_glossy` | 1.218e-03 | 9.033e-04 | 1.35 | 0.37 |
| `disk_k1_c60_s0_f0_diffuse` | 3.853e-03 | 6.294e-04 | 6.12 | 1.78 |
| `disk_k1_c60_s0_f0_glossy` | 2.028e-03 | 1.526e-03 | 1.33 | 0.38 |
| `disk_k3_c10_s0.4_f2_diffuse` | 2.434e-03 | 4.146e-04 | 5.87 | 2.80 |
| `disk_k3_c10_s0.4_f2_glossy` | 2.228e-05 | 4.347e-06 | 5.13 | 2.98 |
| `disk_k3_c10_s0_f0_diffuse` | 2.934e-03 | 6.502e-04 | 4.51 | 2.06 |
| `disk_k3_c10_s0_f0_glossy` | 4.103e-05 | 1.261e-05 | 3.25 | 1.69 |
| `disk_k3_c30_s0.4_f2_diffuse` | 2.566e-03 | 1.337e-03 | 1.92 | 0.53 |
| `disk_k3_c30_s0.4_f2_glossy` | 1.623e-03 | 1.037e-03 | 1.57 | 0.42 |
| `disk_k3_c30_s0_f0_diffuse` | 3.851e-03 | 2.134e-03 | 1.80 | 0.50 |
| `disk_k3_c30_s0_f0_glossy` | 4.076e-03 | 3.365e-03 | 1.21 | 0.39 |
| `disk_k3_c60_s0.4_f2_diffuse` | 1.394e-04 | 1.381e-04 | 1.01 | 0.25 |
| `disk_k3_c60_s0.4_f2_glossy` | 9.618e-05 | 8.970e-05 | 1.07 | 0.36 |
| `disk_k3_c60_s0_f0_diffuse` | 3.651e-04 | 3.697e-04 | 0.99 | 0.33 |
| `disk_k3_c60_s0_f0_glossy` | 2.407e-04 | 2.207e-04 | 1.09 | 0.31 |
| `disk_r0.1_unshaped_diffuse` | 5.204e-06 | 1.494e-05 | 0.35 | 0.25 |
| `disk_r0.1_unshaped_glossy` | 6.222e-05 | 7.786e-05 | 0.80 | 0.39 |
| `disk_r0.3_unshaped_diffuse` | 4.891e-05 | 5.500e-05 | 0.89 | 0.51 |
| `disk_r0.3_unshaped_glossy` | 6.717e-04 | 9.908e-04 | 0.68 | 0.45 |
| `disk_r1_unshaped_diffuse` | 9.491e-04 | 3.669e-05 | 25.86 | 12.45 |
| `disk_r1_unshaped_glossy` | 2.744e-03 | 1.576e-03 | 1.74 | 0.58 |
| `disk_r3_unshaped_diffuse` | 2.201e-03 | 1.138e-03 | 1.93 | 0.48 |
| `disk_r3_unshaped_glossy` | 7.771e-04 | 7.130e-04 | 1.09 | 0.31 |

The unshaped rows reproduce the archived sweep's verdict. `ellipse` wins at equal
time only on the radius-1 diffuse disk (12.5×; archived 7.6×). It loses on every
glossy and every small disk, and costs ~2× per frame. On wide shaped disks over
diffuse it often wins (up to 8.8× at k0.5 c60 with softness and focus), even though
it ignores the cone: uniform solid-angle sampling concentrates samples near the foot,
which is where the cutoff circle is. That is the bar `polar` must clear, the gate
being against `area`.

The shaped rect rows' baseline (today's spherical rectangle) is tabulated beside the
clipped rect in 4.4.

## 4. Cone-clipped rect

### 4.3 Bit-identity

`scripts/check_images.sh check $S/goldens` against the goldens of 1.4 (the
unmodified binary):
- `CRUST_CONE_CLIP` unset (off): every sample is bit-identical.
- `CRUST_CONE_CLIP=on`: every sample is bit-identical except `usdlux`, whose "Spot" is
  the one shaped rect whose axis is its normal. 1.03% of its pixels change (the spot's
  pool), relMSE 2.0e-5 against the unclipped 16 spp golden: noise.

### 4.4 and 5.5: measurements

Same protocol as § 2: 16 spp, `--indirect-clamp 0`, frames 1–4, relMSE against the
1024 spp references. The old side of each comparison is the baseline runs. The new
binary with the switches off is bit-identical to them (4.3), and with them on its
images are bit-identical to those measured here.

**Cost is from instruction counts, not sweep timings.** A sweep frame renders in ~40
ms, and timing it showed rows with a relMSE ratio of exactly 1.00 "winning" by
1.2–1.4× at equal time: noise from 40 ms frames measured minutes apart. Each
strategy's cost per frame is therefore callgrind's instruction count (1 thread, 4 spp)
on representative rows, against the old strategy on the same row (`$S/cost.sh`):

| row | old | new | ratio |
|---|---|---|---|
| `rect_k0.1_c30_s0_f0_diffuse` | 284.2M (off) | 253.0M (clip) | 0.890 |
| `rect_k1_c30_s0_f0_diffuse` | 289.1M | 260.1M | 0.900 |
| `rect_k3_c10_s0.4_f2_glossy` | 296.7M | 260.0M | 0.876 |
| `disk_r0.1_unshaped_diffuse` | 297.2M (area) | 381.5M (polar), 401.4M (ellipse) | 1.284, 1.351 |
| `disk_r1_unshaped_diffuse` | 301.4M | 394.6M, 567.6M | 1.309, 1.883 |
| `disk_k1_c30_s0.4_f2_diffuse` | 256.9M | 352.6M; 248.0M (polar + clip); 406.8M | 1.373; 0.965; 1.584 |
| `disk_k3_c10_s0_f0_glossy` | 255.9M | 345.2M; 245.2M; 396.9M | 1.349; 0.958; 1.551 |

The clip makes a frame *cheaper*. Where the cutoff square misses the light, the light
yields no sample, and nothing else about it is evaluated. "Equal time" below is
`relMSE ratio / cost ratio`, with each class's mean cost ratio.

Wall clock on the two samples (`bench_ab.sh`, 8 interleaved reps, final binary,
`CRUST_CONE_CLIP` off vs on):

| scene | off min / mean (s) | on min / mean (s) | Δ min / mean |
|---|---|---|---|
| `usdlux` | 1.101 / 1.252 | 1.039 / 1.206 | −5.6% / −3.7% |
| `spotlights` (no rect: noise) | 0.565 / 0.590 | 0.525 / 0.600 | −7.1% / +1.7% |

Relative to power selection with the switches off: `spotlights` relMSE 9.40e-3 →
9.10e-3 with `polar` and 7.73e-3 with `polar` + clip (its wide disk; 1.22×); `usdlux`
7.80e-2 → 7.79e-2 with the clip (its spot's pool is 1% of the frame), and 8.06e-2
with `polar` (its warm disk is unshaped, radius 0.6 at height 4.2: the small-disk case).

### Shaped rect rows: clip vs off

| row | off relMSE | clip relMSE | relMSE ratio | equal time |
|---|---|---|---|---|
| `rect_k0.1_c10_s0.4_f2_diffuse` | 2.953e-05 | 2.905e-05 | 1.02 | 1.14 |
| `rect_k0.1_c10_s0.4_f2_glossy` | 7.773e-07 | 7.667e-07 | 1.01 | 1.14 |
| `rect_k0.1_c10_s0_f0_diffuse` | 1.547e-04 | 1.090e-04 | 1.42 | 1.60 |
| `rect_k0.1_c10_s0_f0_glossy` | 5.229e-06 | 4.724e-06 | 1.11 | 1.25 |
| `rect_k0.1_c30_s0.4_f2_diffuse` | 3.045e-05 | 3.009e-05 | 1.01 | 1.14 |
| `rect_k0.1_c30_s0.4_f2_glossy` | 1.653e-05 | 1.368e-05 | 1.21 | 1.36 |
| `rect_k0.1_c30_s0_f0_diffuse` | 4.378e-04 | 2.459e-04 | 1.78 | 2.00 |
| `rect_k0.1_c30_s0_f0_glossy` | 2.789e-04 | 9.087e-05 | 3.07 | 3.45 |
| `rect_k0.1_c60_s0.4_f2_diffuse` | 5.188e-06 | 5.188e-06 | 1.00 | 1.12 |
| `rect_k0.1_c60_s0.4_f2_glossy` | 2.354e-04 | 2.354e-04 | 1.00 | 1.13 |
| `rect_k0.1_c60_s0_f0_diffuse` | 9.473e-05 | 6.454e-05 | 1.47 | 1.65 |
| `rect_k0.1_c60_s0_f0_glossy` | 2.013e-04 | 2.006e-04 | 1.00 | 1.13 |
| `rect_k0.5_c10_s0.4_f2_diffuse` | 2.038e-04 | 8.644e-05 | 2.36 | 2.65 |
| `rect_k0.5_c10_s0.4_f2_glossy` | 5.111e-06 | 2.463e-06 | 2.07 | 2.33 |
| `rect_k0.5_c10_s0_f0_diffuse` | 4.327e-04 | 8.152e-05 | 5.31 | 5.97 |
| `rect_k0.5_c10_s0_f0_glossy` | 1.489e-05 | 4.090e-06 | 3.64 | 4.10 |
| `rect_k0.5_c30_s0.4_f2_diffuse` | 2.059e-04 | 7.775e-05 | 2.65 | 2.98 |
| `rect_k0.5_c30_s0.4_f2_glossy` | 1.214e-04 | 6.102e-05 | 1.99 | 2.24 |
| `rect_k0.5_c30_s0_f0_diffuse` | 6.973e-04 | 1.560e-04 | 4.47 | 5.03 |
| `rect_k0.5_c30_s0_f0_glossy` | 1.601e-03 | 3.122e-04 | 5.13 | 5.77 |
| `rect_k0.5_c60_s0.4_f2_diffuse` | 1.293e-04 | 1.138e-04 | 1.14 | 1.28 |
| `rect_k0.5_c60_s0.4_f2_glossy` | 1.276e-03 | 1.214e-03 | 1.05 | 1.18 |
| `rect_k0.5_c60_s0_f0_diffuse` | 2.636e-04 | 1.289e-04 | 2.04 | 2.30 |
| `rect_k0.5_c60_s0_f0_glossy` | 1.777e-03 | 1.656e-03 | 1.07 | 1.21 |
| `rect_k1_c10_s0.4_f2_diffuse` | 3.510e-04 | 4.476e-05 | 7.84 | 8.82 |
| `rect_k1_c10_s0.4_f2_glossy` | 7.979e-06 | 1.722e-06 | 4.63 | 5.21 |
| `rect_k1_c10_s0_f0_diffuse` | 5.212e-04 | 4.677e-05 | 11.15 | 12.54 |
| `rect_k1_c10_s0_f0_glossy` | 1.928e-05 | 2.774e-06 | 6.95 | 7.82 |
| `rect_k1_c30_s0.4_f2_diffuse` | 4.577e-04 | 9.865e-05 | 4.64 | 5.22 |
| `rect_k1_c30_s0.4_f2_glossy` | 4.495e-04 | 1.076e-04 | 4.18 | 4.70 |
| `rect_k1_c30_s0_f0_diffuse` | 9.646e-04 | 1.590e-04 | 6.07 | 6.83 |
| `rect_k1_c30_s0_f0_glossy` | 4.464e-03 | 9.648e-04 | 4.63 | 5.21 |
| `rect_k1_c60_s0.4_f2_diffuse` | 3.355e-04 | 2.209e-04 | 1.52 | 1.71 |
| `rect_k1_c60_s0.4_f2_glossy` | 7.352e-04 | 6.723e-04 | 1.09 | 1.23 |
| `rect_k1_c60_s0_f0_diffuse` | 5.212e-04 | 2.257e-04 | 2.31 | 2.60 |
| `rect_k1_c60_s0_f0_glossy` | 1.259e-03 | 1.142e-03 | 1.10 | 1.24 |
| `rect_k3_c10_s0.4_f2_diffuse` | 1.401e-03 | 3.656e-05 | 38.32 | 43.11 |
| `rect_k3_c10_s0.4_f2_glossy` | 1.218e-05 | 3.570e-07 | 34.12 | 38.39 |
| `rect_k3_c10_s0_f0_diffuse` | 1.639e-03 | 5.577e-05 | 29.38 | 33.06 |
| `rect_k3_c10_s0_f0_glossy` | 2.504e-05 | 8.577e-07 | 29.20 | 32.85 |
| `rect_k3_c30_s0.4_f2_diffuse` | 8.747e-04 | 1.331e-04 | 6.57 | 7.39 |
| `rect_k3_c30_s0.4_f2_glossy` | 7.443e-04 | 2.291e-04 | 3.25 | 3.65 |
| `rect_k3_c30_s0_f0_diffuse` | 1.401e-03 | 2.148e-04 | 6.53 | 7.34 |
| `rect_k3_c30_s0_f0_glossy` | 2.729e-03 | 1.563e-03 | 1.75 | 1.96 |
| `rect_k3_c60_s0.4_f2_diffuse` | 5.312e-05 | 3.729e-05 | 1.42 | 1.60 |
| `rect_k3_c60_s0.4_f2_glossy` | 3.408e-05 | 2.951e-05 | 1.16 | 1.30 |
| `rect_k3_c60_s0_f0_diffuse` | 1.761e-04 | 7.107e-05 | 2.48 | 2.79 |
| `rect_k3_c60_s0_f0_glossy` | 1.505e-04 | 1.419e-04 | 1.06 | 1.19 |

Clip: 48 wins, 0 ties, 0 losses at equal time (±5%); equal time 1.12–43.11, median 2.47.

### Disk rows: ellipse, polar and polar + clip vs area

| row | area relMSE | ellipse ratio / eq. time | polar ratio / eq. time | polar + clip ratio / eq. time |
|---|---|---|---|---|
| `disk_k0.1_c10_s0.4_f2_diffuse` | 2.268e-05 | 0.92 / 0.58 | 0.67 / 0.50 | 0.69 / 0.71 |
| `disk_k0.1_c10_s0.4_f2_glossy` | 6.180e-07 | 0.46 / 0.29 | 0.40 / 0.30 | 0.41 / 0.43 |
| `disk_k0.1_c10_s0_f0_diffuse` | 1.838e-04 | 0.99 / 0.62 | 1.01 / 0.76 | 4.00 / 4.16 |
| `disk_k0.1_c10_s0_f0_glossy` | 6.352e-06 | 0.88 / 0.55 | 0.80 / 0.61 | 2.65 / 2.75 |
| `disk_k0.1_c30_s0.4_f2_diffuse` | 3.695e-05 | 0.86 / 0.54 | 0.79 / 0.59 | 0.77 / 0.80 |
| `disk_k0.1_c30_s0.4_f2_glossy` | 1.579e-05 | 0.83 / 0.52 | 0.66 / 0.50 | 0.72 / 0.75 |
| `disk_k0.1_c30_s0_f0_diffuse` | 4.277e-04 | 0.99 / 0.62 | 0.86 / 0.65 | 10.94 / 11.38 |
| `disk_k0.1_c30_s0_f0_glossy` | 3.445e-04 | 0.99 / 0.62 | 0.72 / 0.54 | 6.11 / 6.35 |
| `disk_k0.1_c60_s0.4_f2_diffuse` | 3.051e-05 | 0.73 / 0.46 | 0.84 / 0.63 | 0.84 / 0.87 |
| `disk_k0.1_c60_s0.4_f2_glossy` | 2.469e-04 | 0.80 / 0.50 | 1.30 / 0.98 | 1.30 / 1.35 |
| `disk_k0.1_c60_s0_f0_diffuse` | 1.191e-04 | 0.90 / 0.56 | 0.85 / 0.64 | 2.59 / 2.70 |
| `disk_k0.1_c60_s0_f0_glossy` | 2.256e-04 | 0.75 / 0.47 | 1.30 / 0.98 | 1.31 / 1.37 |
| `disk_k0.5_c10_s0.4_f2_diffuse` | 2.214e-04 | 3.12 / 1.96 | 1.19 / 0.89 | 3.17 / 3.30 |
| `disk_k0.5_c10_s0.4_f2_glossy` | 5.468e-06 | 1.02 / 0.64 | 0.76 / 0.57 | 1.10 / 1.14 |
| `disk_k0.5_c10_s0_f0_diffuse` | 3.785e-04 | 1.73 / 1.09 | 0.88 / 0.66 | 11.70 / 12.17 |
| `disk_k0.5_c10_s0_f0_glossy` | 1.344e-05 | 0.89 / 0.56 | 0.68 / 0.51 | 2.06 / 2.14 |
| `disk_k0.5_c30_s0.4_f2_diffuse` | 4.918e-04 | 5.83 / 3.66 | 2.19 / 1.65 | 4.08 / 4.24 |
| `disk_k0.5_c30_s0.4_f2_glossy` | 2.319e-04 | 2.29 / 1.44 | 1.33 / 1.00 | 1.96 / 2.03 |
| `disk_k0.5_c30_s0_f0_diffuse` | 1.207e-03 | 2.42 / 1.52 | 1.46 / 1.10 | 20.47 / 21.29 |
| `disk_k0.5_c30_s0_f0_glossy` | 1.189e-03 | 0.81 / 0.51 | 0.69 / 0.52 | 5.13 / 5.33 |
| `disk_k0.5_c60_s0.4_f2_diffuse` | 1.237e-03 | 23.00 / 14.45 | 6.00 / 4.52 | 6.13 / 6.37 |
| `disk_k0.5_c60_s0.4_f2_glossy` | 1.918e-03 | 1.69 / 1.06 | 1.87 / 1.40 | 1.91 / 1.98 |
| `disk_k0.5_c60_s0_f0_diffuse` | 1.083e-03 | 6.63 / 4.16 | 3.11 / 2.34 | 7.33 / 7.62 |
| `disk_k0.5_c60_s0_f0_glossy` | 2.780e-03 | 1.89 / 1.19 | 1.99 / 1.50 | 2.13 / 2.21 |
| `disk_k1_c10_s0.4_f2_diffuse` | 4.103e-04 | 5.14 / 3.23 | 1.25 / 0.94 | 7.47 / 7.77 |
| `disk_k1_c10_s0.4_f2_glossy` | 6.973e-06 | 2.90 / 1.82 | 0.94 / 0.70 | 3.35 / 3.48 |
| `disk_k1_c10_s0_f0_diffuse` | 6.732e-04 | 3.38 / 2.13 | 1.41 / 1.06 | 19.09 / 19.85 |
| `disk_k1_c10_s0_f0_glossy` | 1.564e-05 | 1.60 / 1.00 | 0.71 / 0.53 | 4.18 / 4.35 |
| `disk_k1_c30_s0.4_f2_diffuse` | 1.523e-03 | 8.83 / 5.55 | 3.24 / 2.44 | 9.02 / 9.38 |
| `disk_k1_c30_s0.4_f2_glossy` | 5.533e-04 | 2.02 / 1.27 | 1.20 / 0.90 | 3.08 / 3.21 |
| `disk_k1_c30_s0_f0_diffuse` | 2.416e-03 | 3.89 / 2.44 | 2.37 / 1.79 | 22.35 / 23.24 |
| `disk_k1_c30_s0_f0_glossy` | 4.848e-03 | 1.35 / 0.85 | 1.08 / 0.81 | 6.04 / 6.28 |
| `disk_k1_c60_s0.4_f2_diffuse` | 2.692e-03 | 3.94 / 2.47 | 11.41 / 8.59 | 12.93 / 13.45 |
| `disk_k1_c60_s0.4_f2_glossy` | 1.218e-03 | 1.35 / 0.85 | 1.91 / 1.44 | 1.97 / 2.05 |
| `disk_k1_c60_s0_f0_diffuse` | 3.853e-03 | 6.12 / 3.85 | 5.43 / 4.08 | 14.67 / 15.26 |
| `disk_k1_c60_s0_f0_glossy` | 2.028e-03 | 1.33 / 0.83 | 1.68 / 1.26 | 1.84 / 1.91 |
| `disk_k3_c10_s0.4_f2_diffuse` | 2.434e-03 | 5.87 / 3.69 | 2.03 / 1.52 | 77.57 / 80.66 |
| `disk_k3_c10_s0.4_f2_glossy` | 2.228e-05 | 5.13 / 3.22 | 2.02 / 1.52 | 65.03 / 67.62 |
| `disk_k3_c10_s0_f0_diffuse` | 2.934e-03 | 4.51 / 2.83 | 1.62 / 1.22 | 73.14 / 76.06 |
| `disk_k3_c10_s0_f0_glossy` | 4.103e-05 | 3.25 / 2.04 | 1.33 / 1.00 | 57.17 / 59.45 |
| `disk_k3_c30_s0.4_f2_diffuse` | 2.566e-03 | 1.92 / 1.21 | 6.67 / 5.02 | 39.61 / 41.19 |
| `disk_k3_c30_s0.4_f2_glossy` | 1.623e-03 | 1.57 / 0.98 | 2.70 / 2.03 | 6.50 / 6.75 |
| `disk_k3_c30_s0_f0_diffuse` | 3.851e-03 | 1.80 / 1.13 | 2.73 / 2.05 | 52.43 / 54.52 |
| `disk_k3_c30_s0_f0_glossy` | 4.076e-03 | 1.21 / 0.76 | 1.35 / 1.01 | 3.04 / 3.16 |
| `disk_k3_c60_s0.4_f2_diffuse` | 1.394e-04 | 1.01 / 0.63 | 8.41 / 6.33 | 11.10 / 11.54 |
| `disk_k3_c60_s0.4_f2_glossy` | 9.618e-05 | 1.07 / 0.67 | 2.37 / 1.78 | 2.39 / 2.49 |
| `disk_k3_c60_s0_f0_diffuse` | 3.651e-04 | 0.99 / 0.62 | 2.30 / 1.73 | 9.71 / 10.10 |
| `disk_k3_c60_s0_f0_glossy` | 2.407e-04 | 1.09 / 0.68 | 1.24 / 0.94 | 1.30 / 1.35 |
| `disk_r0.1_unshaped_diffuse` | 5.204e-06 | 0.35 / 0.22 | 0.12 / 0.09 | 0.12 / 0.09 |
| `disk_r0.1_unshaped_glossy` | 6.222e-05 | 0.80 / 0.50 | 0.99 / 0.75 | 0.99 / 0.75 |
| `disk_r0.3_unshaped_diffuse` | 4.891e-05 | 0.89 / 0.56 | 0.75 / 0.56 | 0.75 / 0.56 |
| `disk_r0.3_unshaped_glossy` | 6.717e-04 | 0.68 / 0.43 | 1.34 / 1.01 | 1.34 / 1.01 |
| `disk_r1_unshaped_diffuse` | 9.491e-04 | 25.86 / 16.25 | 5.06 / 3.81 | 5.06 / 3.81 |
| `disk_r1_unshaped_glossy` | 2.744e-03 | 1.74 / 1.09 | 1.83 / 1.38 | 1.83 / 1.38 |
| `disk_r3_unshaped_diffuse` | 2.201e-03 | 1.93 / 1.21 | 6.63 / 4.99 | 6.63 / 4.99 |
| `disk_r3_unshaped_glossy` | 7.771e-04 | 1.09 / 0.68 | 1.30 / 0.98 | 1.30 / 0.98 |

- `ellipse`: 26 wins, 2 ties, 28 losses at equal time against `area` (best 16.2×)
- `polar`: 25 wins, 7 ties, 24 losses at equal time against `area` (best 8.6×)
- `polarclip`: 46 wins, 2 ties, 8 losses at equal time against `area` (best 80.7×)

### Gate verdicts (6.1)

- **`CRUST_CONE_CLIP`: on by default.** It wins at equal time on all 48 shaped rect
  rows (1.12–43×, median 2.5×) and is never worse in relMSE (min ratio 1.00). It is
  bit-identical on the stage, which has no rect, and ties on `usdlux` (relMSE 1.001×,
  1.5% fewer instructions, 4–6% faster by wall clock). The gate ("better in mean, no
  case worse than a tie") passes on every row.
- **`CRUST_DISK_SAMPLING`: stays `area`.** `polar` alone splits the rows (25 wins, 7
  ties, 24 losses), as `ellipse` does (26, 2, 28) at a fifth more cost. With the clip
  (the default), `polar` wins 46 of 56 rows, up to 81× on a wide disk with a narrow
  cone, and is cheaper than `area` on shaped disks. It loses on 8, all small disks:
  the unshaped radius 0.1 (0.09× diffuse) and 0.3 (0.56×), and the soft, focused cones
  at ratio 0.1 (0.43–0.87×). The gate requires no loss, so it fails.
- **Cause of the small-disk losses.** With the foot outside the disk, the azimuth is
  uniform over the wedge, so the thin slices at its edges get as many samples as the
  diameter. The estimator then varies with chord length, where area sampling of a
  small far disk is nearly noiseless. It is not bias: on
  `disk_r0.1_unshaped_diffuse` polar's relMSE falls 4.0e-5 → 5.0e-6 → 5.2e-7 → 1.7e-7
  from 16 to 1024 spp, with no floor.
- **Open question (design): a shaped-only default?** No: `polar` + clip still loses
  the five small shaped rows, so it would fail the same gate. The fix is in the
  strategy (sample `ψ` by slice weight, or use polar only where the foot is inside the
  disk or the cap clips). That is a follow-up, recorded in the design record.

### Default-path cost: a regression found and fixed

With every switch off, the first build of the strategies cost `veach_mis` (four sphere
lights, no shaped light, no disk) +0.71% instructions over the base (14.86G → 14.96G).
All of it was in `AreaLight::sample_li` (+13%, 725M → 819M), and `usdlux` cost +0.49%.
Bisecting with callgrind: the task-3 commit (5f5f01c, the cone fields on the shapes)
is exactly the base (`sample_li` 724,923,838 both), so the strategies were the cause.
Ruled out, one build each:
1. Moving the clipped rect's `full` corner out of `SphericalRect` (the sampler back to
   96 bytes): 827M.
2. Moving the clipped rect's body out of line: 822M.
3. Folding the three new arms into one out-of-line arm: 816M.
4. Returning an outcome rather than a sampler from the out-of-line decisions: 828M.

Per-instruction profiles (`--dump-instr=yes`) showed the cause: the `Cone` strategy,
which the base keeps in registers, was written to the stack, tag and fields, and read
back (147 stack moves in `sample_li`'s body became 256). Every new arm handed an
f64-typed payload (the clipped rectangle, the polar view) out of line, and those bytes
overlap the f32 fields of `Cone` in the sampler, so the compiler could no longer split
the enum into registers. The fix is to hold only the shape and `from` (a pointer and a
`Vec3A`, what the ellipse and tube arms hold) and rebuild the clip or view inside the
out-of-line `sample` / `pdf`. That costs a shaped light a second clip per sample, which
is in the cost table above. After it:
- `veach_mis`: `sample_li` 727.8M (+0.4%), program +0.07% (callgrind's run-to-run
  drift is ~0.1%);
- `usdlux` with the switches off: +0.22%, and with the clip on (the new default) 1%
  fewer instructions than the base.

The images are bit-identical before and after the restructure.

### 6.1 Defaults applied

`CRUST_CONE_CLIP` defaults to on: `config.rs`, the spec delta (with a scenario that
`off` restores the previous sampling bit for bit), `docs/architecture.md` and
`reference/environment-variables.md`. Goldens are re-recorded; `CRUST_CONE_CLIP=off`
reproduces the old goldens bit for bit.

### 6.1–6.3 Verification (final binary)

- `CRUST_CONE_CLIP=off scripts/check_images.sh check` against the goldens of 1.4:
  every sample is bit-identical.
- Default (clip on) against those goldens: only `usdlux` differs. New goldens are
  recorded (39 samples), and `check` against them is bit-identical.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D
  warnings` are clean. `cargo test --workspace --no-fail-fast`: 2096 passed, 0 failed.
- `openspec validate cone-aware-light-sampling --strict` passes. `zola build` (0.21)
  checks 354 internal links.
- `cargo deny` is not installed here. No manifest or lockfile changed.
