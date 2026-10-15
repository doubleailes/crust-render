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
