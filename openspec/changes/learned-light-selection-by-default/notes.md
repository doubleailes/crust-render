# Notes: learned-light-selection-by-default

## Evidence from `cone-aware-light-sampling`

`samples/spotlights.usda` (six small shaped spots on disjoint pools under a dim
fill) is a case power selection cannot see: a pool's own spot gets about 1 NEE pick
in 13. Measured with the unmodified renderer at 16 spp, `--indirect-clamp 0`, 4
seeds, against a 1024 spp reference; Render time interleaved by `bench_ab.sh`:

| scene | relMSE, power → learned | trimmed | Render time | equal time |
|---|---|---|---|---|
| `spotlights` | 9.40e-3 → 2.08e-3 (4.5×) | 6.3× | +6.1% min, +5.3% mean | 4.3× |
| `spotlights`, pools only | 7.35e-2 → 1.17e-2 (6.3×) | | | 5.9× |
| `usdlux` | 7.80e-2 → 2.33e-2 (3.3×) | 3.4× | −2.3% min, +4.9% mean | 3.2–3.4× |

The trimmed gain is larger than the full one on both scenes, so `learned` adds no
fireflies here. Details and commands:
`openspec/changes/cone-aware-light-sampling/notes.md` § 2.2.
