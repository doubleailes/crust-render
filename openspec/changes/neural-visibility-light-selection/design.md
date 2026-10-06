# Design

## Context

See proposal.md for the motivation. The current state this builds on:

- **`learned` selection** (`crates/crust-core/src/light_cache.rs`, `docs/light_sampling.md` §3.12) already owns most of the plumbing this needs:
  - a deterministic pre-pass: one camera path per 4×4 pixels, 2 BSDF bounces, every light estimated at every vertex through `sample_li` → BSDF `eval` → shadow ray;
  - robust bounds from the receivers' 2–98% quantiles;
  - a `0.3` defensive uniform share;
  - installation over a power table.
- **One lookup serves both MIS sides.** `LightList::pick_index_at` / `pmf_at` / `find_index_by_geom_at` / `infinite_at` take the position of the vertex NEE sampled from. The bounce side passes `prev.pos`. Today the grid makes that lookup tolerant of a last-ulp difference between the two positions. A continuous function is not.
- **Rules from CLAUDE.md that bind the design:**
  - `forbid(unsafe_code)` outside the two audited crates, and no dependency carrying `unsafe` on the hot path without a project decision;
  - no RNG outside `openqmc`;
  - tiles ↔ scanlines bit-identity;
  - `INFO` stays bounded per render;
  - every `crust:*` attribute and CLI choice is documented in `site/`.
- **No ML code exists** in the workspace, and there is no batching: rayon runs one path per task, megakernel style. Every inference is one point, one call.

## Goals / Non-Goals

**Goals:**
- A `neural` selection mode that is unbiased, consistent across both MIS sides, and bit-deterministic. Specs: `lighting`.
- A reusable, dependency-free, safe-Rust tiny-network kit (`crust-nn`), testable in isolation with gradient checks.
- An honest equal-time comparison against `learned` and `power` on ALab, `usdlux`, `domelight` and `veach_mis`, written into `docs/light_sampling.md` whatever its result.
- A go/no-go gate on inference cost **before** the integration work, so a negative answer costs days, not weeks.

**Non-Goals:**
- Online training during render passes, or retraining per frame. The paper does this for real time; offline crust freezes before the first pass, as `learned` does.
- Many lights. There is no light tree, and the output layer is one unit per light (D7).
- Surface normal or view direction as network inputs. The `*_at(p)` contract is position-only, and widening it touches every MIS pair. That is a follow-up if position-only proves the idea.
- Learning contribution (radiance × BSDF × distance) rather than visibility. That is Figueiredo et al. 2025, a separate change.
- Replacing `learned`, or changing the default.
- GPU, SIMD intrinsics, or any `unsafe`.

## Decisions

### D1. A new crate, `crust-nn`, with no crust dependencies and `forbid(unsafe_code)`

It contains:
- `HashGrid`: multi-resolution hash encoding, trilinear interpolation, forward and backward.
- `Dense`: weights + bias, forward and backward.
- ReLU and sigmoid.
- `bce_with_logits` loss.
- `Adam`.
- A `Network` that composes them, with a finite-difference `grad_check` used by its tests.

**Why a crate:** it follows the `crust-rt` / `crust-mtlx` pattern: a kernel with no knowledge of crust, which crust-core adopts. It can be tested and benchmarked alone, and it keeps crust-core's dependency list honest.

**Alternatives:**
- `candle` / `burn` / `tch`: large dependency trees with `unsafe` SIMD or FFI, so a project decision against CLAUDE.md; overkill for ~10k parameters. Rejected.
- A module inside crust-core: workable, but mixes generic numerics into the engine, and crust-core is only `deny(unsafe)`. Rejected.

### D2. Learn visibility only, and weight the power table by it

At a vertex `x`, with power table `π`, predicted visibility `V̂ℓ(x) ∈ (0, 1)`, defensive share `D` and `n_live` emitting lights:

```
wℓ = πℓ · V̂ℓ(x)
pℓ(x) = (1 − D) · wℓ / Σw + D / n_live      if Σw > ε
pℓ(x) = πℓ                                   otherwise (all predicted hidden)
```

**Why:** it is the paper's division of labour. The network answers what geometry can't (is the light occluded from here), and an analytic term answers what it can. Weighting by `π` is O(1) per light. It fixes ALab's failure directly: the two hidden exterior lights get `V̂ ≈ 0`.

**Alternative:** the paper's unshadowed per-light contribution, used through weighted reservoir sampling. That needs every light evaluated at every vertex (O(N) `sample_li`), and a new per-light importance function whose pmf must match on the bounce side. Deferred: if power × V̂ loses to `learned` on scenes where distance matters (`usdlux`), this is the next step, and that comparison will show it.

**`D` starts at `light_cache::DEFENSIVE` (0.3)**, shared rather than copied. A continuous visibility does not straddle a shadow boundary the way a cell does, so `D` may be able to fall. That gets measured, not assumed.

### D3. Network shape: 8-level hash grid → 16 → 32 → 32 → N, ReLU, sigmoid out

| part | size | why |
|---|---|---|
| hash grid | L = 8 levels, F = 2 features, T = 2¹⁴ entries/level, base 4, growth ≈ 1.6 | finest level ≈ 107 cells per axis, matching `learned`'s `MAX_RESOLUTION` (96); 1 MiB of `f32`, inside L2 on most CPUs |
| hidden | 2 × 32, ReLU | the paper's network is small, too; width 32 keeps an inference near 3k multiply-adds at N = 47 |
| output | N ≤ 64 logits → sigmoid | one unit per light (D7) |

Input: `x` mapped into the receivers' quantile bounds. Outside the bounds, `power` answers, as with `learned`'s untrained cells.

**Alternatives:**
- Frequency (Fourier) encoding: no table, but needs a much wider MLP for the same sharpness, and MACs are the CPU's bottleneck.
- A dense grid, i.e. `learned` with interpolation: a fair baseline and cheaper. Recorded as the control the measurements must beat, not built here.

### D4. Labels come from the `learned` pre-pass walk, shared rather than copied

The receiver walk in `light_cache::train` gets split out. It is the camera paths, the bounces, and the per-light `sample_li` + shadow ray. It reports, per receiver per light, both the contribution estimate (what `learned` sums) and the label: the fraction of `LIGHT_SAMPLES` samples that **delivered light**, meaning nonzero radiance toward `x` and a clear shadow ray.

- The label deliberately folds in backfacing and below-horizon. On ALab those were 45% of wasted picks, and the network should learn them.
- Below-horizon depends on the normal, which is not an input (non-goal), so the network learns the local average, exactly as the grid does.

`learned`'s output must stay bit-identical after the split (pinned by the existing `tests/learned_selection.rs` plus a golden check).

### D5. Deterministic training

- **Initialisation and shuffling** come from `openqmc` draws under a new key `K_NVC`, never `rand`.
  - Xavier-uniform weights; hash-grid entries uniform in ±1e-4.
  - Each epoch's receiver permutation is derived from the epoch index.
- **Mini-batches of 256.** A batch's gradient is computed in parallel over fixed 32-sample chunks. The chunk gradients are summed **in chunk order**, so the result is the same at any thread count.
- **Adam:** learning rate 1e-2 for the hash grid and 1e-3 for the MLP; β = (0.9, 0.99), the tiny-cuda-nn defaults. The hash grid is updated densely (sparse "lazy Adam" changes results with batch order and saves little at 1 MiB).
- **A fixed epoch count (8)**, no early stopping on a timer. Then the weights freeze, before any render pass.
- **Plain `f32` arithmetic in a fixed loop order, no `mul_add`.** The bits are then the same under every target-feature set. `scripts/test_simd_matrix.sh -p crust-nn` pins this.

### D6. Inference is a pure function of `p`, memoised per thread

`pmf_at(p, i)`, `pick_index_at(p, u)` and the rest all call one function: `p ↦ (pmf, cdf)`, of N floats each. Because it is a pure function, the bounce side recomputing it at `prev.pos` gives the same bits as NEE did. The MIS pair then holds structurally, as it does for the grid.

A single-entry `thread_local!` memo, keyed on the exact bits of `p`, makes the bounce side's lookup free when it follows NEE at the same vertex, which is the common case. It is an optimisation only. A miss recomputes the same bits.

**Prerequisite, verified rather than assumed:** NEE's sampling position and the bounce side's `prev.pos` must be the same `Vec3A`, not merely close. A grid hid an ulp difference; a continuous network turns one into an MIS mismatch. Task 3.1 checks every NEE site (surface, volume / phase) with a debug assertion before anything else is wired in.

**Alternative:** carry the pmf on `PrevVertex`. That is explicit, but it adds up to 512 B to the path state and touches every bounce-side site and the `*_at` signatures. Rejected unless the memo's hit rate disappoints.

### D7. At most 64 lights

The output layer is 32 × N. Past 64 lights, inference cost grows past the gate (D9). Past 64 the mode falls back to power with one `WARN`, the same shape as `learned`'s `MAX_LIGHTS` refusal. ALab has 47 lights; no checked-in sample has more than 7.

### D8. No environment switch; hyperparameters are named constants

`neural` is itself the A/B against `learned` and `power`, chosen per render. The constants in D3 and D5 live beside their measurements, as `light_cache.rs`'s do. If tuning one becomes an A/B of its own, it gets a `Config` field then, under the CLAUDE.md rule.

### D9. A go/no-go gate on inference cost, before integration

`crates/crust-render/examples/nvc_bench.rs` loads a scene, trains, and times three things on the same receivers:
- one `p ↦ (pmf, cdf)` evaluation;
- one shadow ray;
- one `learned` lookup.

It reports both min-of-N wall time and callgrind instruction counts.

- **Go** if one evaluation costs at most ~2 shadow rays on ALab.
- If it is over ~5, **stop**: record the numbers in `docs/light_sampling.md` and propose shrinking D3 (L = 4, width 16) before anything else.
- The final verdict is the equal-time table (task group 6), against `learned`, at `--indirect-clamp 0`.

## Risks / Trade-offs

- **[Inference cost cancels the gain at equal time.]** `learned` already pays +14% on ALab, mostly in extra unoccluded shadow rays, and neural adds ~3k MACs plus 64 hash reads per NEE vertex. → The D9 gate runs first. A smaller network is the first fallback. A negative result is still recorded and is a valid outcome of the experiment.
- **[An ulp mismatch between NEE's `p` and the bounce side's `prev.pos`.]** That is a silent MIS bias, not a crash. → Task 3.1's assertion, plus the `veach_mis` mean-value check from the spec.
- **[Training is not robust across scenes:]** bad learning rates, dead ReLUs, sparse receivers. → The defensive share bounds the cost of any mis-prediction to `n / D`× a light's uniform variance. The pre-pass logs its final training loss at `DEBUG`. A unit test trains on a synthetic scene with a known wall.
- **[Position-only input blurs visibility across a thin wall, or between a floor and the wall above it.]** → The same limitation as the grid, at finer resolution. Normals are the named follow-up.
- **[The 1 MiB hash table evicts BVH nodes from L2 during the render.]** → Visible as a slower shadow-ray time in `nvc_bench` with the network live versus idle. Shrink T if so.
- **[An eighth crate is maintenance surface.]** → It has no dependencies and is ~600–900 lines. If the experiment is abandoned, it goes with it.

## Migration Plan

Additive and opt-in. No existing mode's output changes (spec scenario "Existing modes are unchanged", checked with `scripts/check_images.sh`). Rollback is removing the variant and the crate. No scene or file format changes.
