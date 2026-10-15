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
