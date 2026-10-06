## Context

The motivation is in proposal.md, under Why.

- **The kernel today.** Shadow rays reach the kernel through one call:
  `surface_visibility` (`crust-core/src/tracer/path.rs`) calls `World::occluded`, which
  calls `Scene::occluded`, which calls `Bvh::hit_any`. That is an early-exit BVH4
  traversal. It descends into an instance's inner `Scene` with the ray moved to local
  space, and the direction is left unnormalised, so `t` carries over.
- **Who else uses that answer.** NEE, volume NEE (through `shadow_transmittance`) and
  the learned light cache's training all share `surface_visibility`. That returns a
  colour. A blocked ray in a world with pass-throughs (cutouts, and thin-walled
  surfaces with straight transmission, `World::has_pass_throughs`) falls through to an
  exact `intersect` walk (`cutout_through`) that multiplies their transmittances.
- **Prototypes.** They are committed in `scene/usd_import/instancing.rs` with
  `crate::commit_options()`. On the Moana island, a grouped prototype is a scene of
  instances of its parts (`docs/moana_profile.md`).
- **Constraints that shape this design:**
  - `forbid(unsafe_code)` in `crust-rt`;
  - no RNG outside `openqmc`;
  - `crust-rt` stays free of crust types;
  - commit is deterministic across thread counts;
  - environment switches live in `Config`, with the old behaviour as the off side;
  - `C.*[LO]` is pinned bitwise to the beauty;
  - the zero-AOV render is pinned to its instruction count.

## Goals / Non-Goals

**Goals:**
- Answer, with numbers, whether a learned occlusion query beats the exact BVH4 any-hit
  on a CPU, and at what error, in cache and out of cache.
- If it does, make it usable on real scenes behind an off-by-default switch, without
  moving any exact result.

**Non-Goals:**
- A production feature. Nothing here becomes a default without a separate proposal.
- Learned closest-hit, learned geometry compression, or freeing the exact tree.
- Matching the papers' GPU throughput. Their numbers are context, not targets.
- Hiding the bias. It is measured and reported, not tuned away.

## Decisions

### D1. Occlusion only, inside instanced prototypes

A proxy answers one question: "is there a surface on this segment, for this mask?"
That is the whole contract of `occluded`. Closest-hit would need a hit point, a normal,
`prim_id` (for Ptex face ids, UVs and materials) and barycentrics. N-BVH and LSNIF
predict those with extra heads, and every error there shows up as a shading artefact.
The image can tolerate occlusion errors far better, and they are measurable as two
rates.

Proxies attach to prototypes rather than to the top-level scene, for three reasons:
- the out-of-cache, many-node trees the hypothesis needs are prototype trees (isDunesB,
  isBayCedar);
- the top level holds lights' own shapes and every pass-through surface;
- a prototype is trained once and placed thousands of times.

Alternative: a proxy for the whole world, as in N-BVH. Rejected because every shadow
ray toward a small area light would then cross a learned approximation of the light's
own neighbourhood.

### D2. N-BVH-style model: a cut of the existing BVH4, plus one hash grid and an MLP

- **The cut.** The cut is a set of `(wide node, lane)` children of the prototype's own
  collapsed BVH4, stored as a bitset beside the tree. Proxied `hit_any` traverses the
  exact tree down to a cut child, clips the segment to that child's box, and evaluates
  the model on the clipped segment. Empty-space skipping above the cut is exact, so a
  ray that misses every cut box costs exactly what it costs today.
- **The model** is shared across the whole cut:
  - **Encoding:** a multi-resolution hash grid over the prototype's root box (Instant
    NGP). It has L = 8 levels, resolutions growing geometrically from 16 to 1 024,
    T = 2^14 entries per level and F = 2 features, all `f32`, which is 1 MiB.
  - **Inputs:** S = 4 points at fixed stratum midpoints of the clipped segment. Each
    point gives L·F features, read by trilinear interpolation. The input adds the
    normalised direction and the clipped length relative to the box diagonal, so
    S·L·F + 4 = 68 inputs.
  - **MLP:** 68 → 32 → 32 → 1, with ReLU and a sigmoid. That is about 3.3 k weights.
  - **Output:** "occluded" when p > τ, with τ = 0.5 by default. The probe sweeps τ.
- **Expected per-query cost:** S·L·8 = 256 table reads and about 3.2 k multiply-adds.
  That is comparable to an in-cache any-hit, which is why the gate (D8) exists.
- **Why not the alternatives:**
  - NIF (two-sphere ray parametrisation per object) has no empty-space skipping inside
    the object's bounds.
  - LSNIF (a network per voxel) multiplies weight memory, and its encoding needs a
    voxelisation pass crust does not have.
  - Neural Bounding learns conservative bounds, not visibility. It helps animated
    queries, which crust's transform-only motion does not have.

  The cut reuses a tree that must stay resident for closest-hit anyway, so above the
  cut it costs only a bitset.

### D3. The cut: a subtree-size rule first, N-BVH's error-driven refinement second

- **First, the size rule.** Cut at the highest children whose subtree holds at most
  K = 4 096 triangle references. This is deterministic, one pass, and needs no
  training.
- **Then, refinement.** N-BVH's adaptive rule repeatedly splits the cut node with the
  largest validation error and re-trains. It is implemented in Phase 1 as a second
  option, and the probe compares the two. Whichever wins on the gate metric is the one
  Phase 2 uses.
- **Alternative:** a fixed tree depth. Rejected because prototype trees are unbalanced
  (SBVH spatial splits), so a depth cut gives cut boxes of wildly different contents.

### D4. Training distribution matches what shadow rays actually ask

Each training segment is labelled by the exact `hit_any` of the prototype on the
segment clipped to its cut box, with the ray mask the proxy is trained for. Geometry
masked off for shadow rays is therefore learned as empty. The segments come from three
families, all from openqmc draws:

1. **Surface-origin rays (50 %).**
   - A point on an area-weighted random triangle, reached through instances where
     present.
   - It is offset exactly as the integrator offsets shading points (`TRACE_T_MIN`).
   - The direction is uniform on the sphere, and the segment runs to the box exit or
     to a random `t_max`.

   This is the dominant real query, and the one where a light leak at contact would
   show.
2. **Through rays (35 %):** two uniform points on the faces of a random cut box.
3. **Short segments (15 %):** family 2 truncated at a uniform fraction, so segments
   that end inside a box are represented.

The loss is binary cross-entropy, weighted per batch to balance the two classes.
Validation keeps a separate held-out set of the same mix, and its FP and FN rates are
the numbers reported.

### D5. Deterministic training

- Weights are initialised from an openqmc stream keyed by a fixed constant and the
  proxy's index in commit order.
- Adam runs with fixed hyper-parameters:
  - learning rate 1e-2 for the grid and 1e-3 for the MLP (Instant NGP's split);
  - β = (0.9, 0.99);
  - batch 4 096;
  - 2 000 steps, overridable in the probe.
- Each batch is cut into 64 fixed chunks, independent of the thread count.
  - Rayon computes each chunk's MLP gradient and its list of sparse hash-grid gradient
    entries.
  - The lists are summed in chunk order on one thread. Float addition is not
    associative, so the order is the contract.
  - The sparse merge is about 2 M adds per step.
- Plain `f32` arithmetic with no fused or reassociated operations gives the same bits
  under every SIMD codegen. `scripts/test_simd_matrix.sh -p crust-rt` covers it.
- **Alternative:** Hogwild-style parallel SGD. Rejected because it is faster but
  nondeterministic, which breaks the commit contract.

### D6. It lives in `crust-rt`, behind a cargo feature

The cut and the labels need `Bvh` internals: wide nodes, lane boxes and the exact
`hit_any`. A separate crate would force those internals public. So the code is a
`neural/` module in `crust-rt`:
- `grid.rs`, `mlp.rs`, `adam.rs`, `train.rs`, `cut.rs`;
- compiled only with `neural-bvh`;
- forwarded by `crust-core` and `crust-render` the way `bvh8` is.

`openqmc-rs` becomes an optional dependency enabled by the feature. It is already a
dev-dependency and is the project's only sanctioned RNG. No ML framework is added:
candle and burn carry `unsafe` and would breach the safe-Rust rule on the hot path.

With the feature on, the API grows in three places:
- `CommitOptions` gains `occlusion_proxy: Option<ProxyRequest>`, which carries the
  minimum triangle count, the trained mask and the training budget;
- `Scene` gains `has_occlusion_proxy()`;
- `MemoryFootprint` gains `occlusion_proxies`.

`Bvh::hit_any` checks for an inner proxy only at instance descent. With the feature off,
that check is not compiled.

### D7. The host decides pass-throughs; the kernel decides geometry

The kernel checks what it can see. It refuses a request for a scene that is not
triangles (directly or through static instances), that has motion, or that is under the
threshold.

Pass-throughs (cutouts, and thin-walled surfaces with straight transmission) are a
material property the kernel never sees. So `crust-core` passes a request only for a
prototype with no pass-through material bound. A proxy answers only "blocked or not",
not a colour. Otherwise `cutout_through` would
resolve, exactly, a ray the proxy had already called blocked, or never revisit one it
had wrongly called open. `crust::commit_options()` stays the single source and grows a
`for_prototype(eligible)` variant. The top-level `WorldBuilder` commit never asks.

### D8. The gate between Phase 1 and Phase 2

Phase 2 is built only if, on at least one prototype of at least 1 M triangles traced
out of cache, proxied occlusion reaches **at least 1.25× the exact Mray/s**, with
**FP + FN ≤ 1 % of the shadow-distribution validation rays**.

- **Measured on:** the `ray_throughput --large` field, and `neural_probe` on the
  largest available USD prototypes. Those are ALab, and the island when present. A
  `scripts/gen_stress_scene.py` instanced scene is the reproducible fallback.
- **Speed method:** min-of-N and interleaved, per CLAUDE.md's measurement rules.
- **If the gate fails:**
  - the result is written into `openspec/specs/intersection-kernel/design.md` the way
    `bvh8` is;
  - this change is revised with `/opsx:update` to drop the `rendering` and `cli` deltas
    and the switch;
  - the code is not merged. Unlike `bvh8`, which is a lane-width swap of existing
    code, this is about 1.5 k lines of training code. Keeping it in the kernel crate
    for a negative result costs more than the record is worth. The branch is kept,
    and the record names it.
- **Phase 2's switch-off cost:** the instance-descent branch is pinned by callgrind on
  `cornellbox` and must stay at most +0.1 % instructions.

## Risks / Trade-offs

- **[The CPU MLP is simply slower than the tree]** That is likely in cache. The gate
  exists for exactly this, and a negative result is a recorded outcome, not a failure.
- **[Light leaks at contact]** A shadow ray from a leaf surface into a dense crown is
  where a false negative is most visible.
  → Family 1 dominates training, and the probe reports FN separately for segments
  starting within one finest-grid cell of a surface.
- **[Bias through MIS]** NEE's visibility differs from the bounce side's, so the
  NEE ↔ bounce pairing invariant no longer holds for proxied geometry.
  → The bias is accepted, documented, and measured as a relmse plateau (spec
  "Neural occlusion is a documented bias"). It is never the default. `C.*[LO]` stays
  bitwise equal to the beauty, because both go through the same `surface_visibility`.
- **[Import time]** About 2 000 steps per proxy, of order seconds to tens of seconds.
  On a scene with hundreds of large prototypes that adds up.
  → `CRUST_NEURAL_MIN_TRIS` (default 250 000) limits proxies to the few that matter,
  and `--stats` reports the training seconds. Caching weights to disk is out of scope.
- **[Memory grows, never shrinks]** About 1 MiB per proxy on top of the exact tree.
  → This is reported as its own footprint line. An `f16` table would halve it but needs
  a dependency (`half`) or hand-rolled conversion. That is left open.
- **[Adaptive sampling]** Proxied visibility changes per-pixel variance, so it can
  change the sample budget.
  → Image comparisons stay at `-s 16`, below `min_samples_per_pixel`, per CLAUDE.md.

## Migration Plan

Nothing migrates: the feature is off by default and the switch is off by default.
Rollback is not building the feature. If the gate fails, only the design-record entry
lands (D8).

## Open Questions

- Should the hash table be stored as `f16`? Decide only if Phase 2 lands and proxy
  memory matters.
- Does τ want to be per proxy, chosen on validation to balance FP and FN? The probe's τ
  sweep answers this without changing the specs.
