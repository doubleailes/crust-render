# Shading performance: a plan

Arnold and RenderMan are much faster than crust, and the working bet is that
shading is where much of the gap lies. This document records what the shading
path does today, where its cost multiplies, and an ordered plan for reducing it.
The plan runs from the cheapest, output-preserving changes up to a JIT. Step 1
(the profile) and step 2 are done; their measurements are recorded under each
step, and the call counts below were confirmed by them.

## What a shading call costs today

Two materials evaluate a pattern network at every hit:

- **`MtlxMaterial`** (`material/materialx.rs`). `shade()` runs the compiled
  MaterialX `Program` (`crust-mtlx/src/eval.rs`), then `reduce()` pools the
  flattened lobes onto an `OpenPBR`, then hands that `OpenPBR` to the requested
  query. The `Program` is already a linear, slot-indexed instruction list with a
  thread-local value buffer. It does no name hashing and allocates nothing.
  Each instruction is still one `match` arm over `Op`, and each operand read is a
  bounds-checked `slots.get()`. `Val` is always four lanes plus an arity tag, so
  scalar graphs do four-lane work.
- **`PreviewSurface`** (`material/preview_surface.rs`). Its `shade()` samples each
  connected `UsdUVTexture` and writes the results over its `OpenPBR` fields. It
  has almost no arithmetic: its cost is texture fetches.

Both do this work on **every** `Material` call. The comment on
`MtlxMaterial::shade` states why: `Material`'s methods take `&self` and a
`&HitRecord`, and there is nowhere to keep the result between calls.

### How many times a vertex is shaded

At one vertex of an unguided path (`tracer.rs`):

| Call | Site | Graph runs |
|---|---|---|
| `emitted_at`, when the ray arrives | `trace_path` | 1 |
| `scatter_importance` | `sample_bounce_direction` | 1 |
| NEE `mat.eval` toward the sampled light | `trace_path` | 1 |
| `eval(..).is_none()` on the *previous* vertex, when the bounce hits an emitter | `bounce_emission_weight` | +1 |
| `eval(..).is_some()` on the previous vertex, when the path escapes | `escaped_emission` | +1 |

A guided render adds one or two more `eval` calls in `sample_bounce_direction`:
the guide branch evaluates the BSDF at the guided direction, and the BSDF branch
calls `eval(..).is_some()` to decide whether to mix densities.

So a textured surface's network runs **3–5 times per vertex**. Every run
evaluates every graph instruction, fetches every texture and repeats the
reduction.

### How production renderers avoid it

OSL (Arnold, RenderMan) and pbrt-v4 split shading into two phases:

1. **Run the shader once per hit.** This produces a BSDF value: OSL closures, or
   pbrt-v4's `BSDF` returned by `Material::GetBSDF`. All texture fetches and
   pattern arithmetic happen here.
2. **Sample and evaluate that BSDF** as many times as the integrator needs, for
   NEE, the bounce, guiding and MIS weights. None of this touches textures or the
   pattern network.

A JIT makes one shader run cheaper. The split removes most of the runs. A JIT
without the split still pays 3–5 runs per vertex, so the split comes first
whether or not a JIT follows.

## The plan

Each step is independently useful, can stop the sequence if the profile says
so, and keeps the previous behaviour reachable for A/B comparison, as the rest
of the renderer does.

### 1. Profile the shading path

Measure before changing anything. Scenes:

- `samples/materialx_teapot.usda` and `samples/materialx_lion.usda` (large
  MaterialX graphs; the lion is 140 ops and 7 textures);
- `samples/materialx_basic.usda` (small, checked in, always available);
- `samples/usdpreview_textured.usda`, and one ALab frame if it is available
  (`PreviewSurface`-dominated).

Use callgrind, per "Measuring a change" in `CLAUDE.md`:

```bash
RAYON_NUM_THREADS=1 valgrind --tool=callgrind --cache-sim=no --branch-sim=no \
    target/release/crust-render -i samples/materialx_lion.usda -o /tmp/x.exr -s 2
callgrind_annotate --inclusive=yes callgrind.out.<pid>
```

Report the inclusive share of:

- `Program::eval`, split into texture ops versus everything else;
- `reduce`;
- the `OpenPBR` BSDF methods (`scatter_importance`, `eval`);
- `MtlxMaterial::shade` / `PreviewSurface::shade` as a whole;
- the kernel (`intersect`, `occluded`) for scale.

Also count calls to `shade` per camera sample, to confirm the 3–5 figure above.

**Measured** (2026-09-26, callgrind, `RAYON_NUM_THREADS=1`, `-s 1`, before
step 2). Shares are of `Renderer::render_pixel` inclusive, not of the whole
process: at 1 spp the two DPEL assets spend over 90% of their instructions
loading, which says nothing about shading.

| Scene | `render_pixel` Ir | Material `eval`+`scatter` | `Program::eval` | of which textures | non-texture | `reduce` | `World::intersect` |
|---|---|---|---|---|---|---|---|
| `materialx_basic` | 1.95 G | 67.5% | 33.6% | 13.2% | 20.4% | 9.9% | 8.9% |
| `usdpreview_textured` | 1.38 G | 55.0% | — | 14.3% | — | — | 12.3% |
| `materialx_teapot` | 7.78 G | 39.2% | 33.3% | 15.5% | 17.9% | 2.1% | 21.7% |
| `materialx_lion` | 11.18 G | 55.8% | 46.5% | 18.4% | 28.1% | 2.9% | 18.0% |

(`PreviewSurface` has no `Program`; its `probe` — texture fetches and the
normal map — is 22.0% of render, and the `OpenPBR` BSDF it delegates to 30.2%.)

- **Shading is the gap on every textured scene**: 39–68% of render, against
  9–22% for the kernel.
- **Runs per vertex: 3.0**, confirmed. On `materialx_basic`,
  `scatter_importance` ran 131 255 times and `eval` 259 169 times: 129 019 of
  those from NEE, the rest from the yes/no checks step 2 removes (12 935 in
  `bounce_emission_weight`, the remainder in `escaped_emission`, inlined into
  `render_pixel`). `usdpreview_textured`: 128 956 and 253 580.
- **Neither texture fetches nor interpreter overhead dominate alone.** Texture
  fetches are 13–18% of render; non-texture `Program::eval` is 18–28%, largest on
  the 140-op lion. Both are multiplied by the run count, so steps 2–3 come first,
  and step 4 stays justified afterwards if the lion's non-texture share holds.

**What decides the next steps:**

- If `shade` is a small share, shading is not the gap and this plan stops.
- If texture fetches dominate `shade`, steps 2–3 are the lever and a JIT buys
  nothing: generated code still calls the same `Texture2D::eval`.
- If interpreter overhead (non-texture `Program::eval`) is large even after
  step 3, steps 4–5 are justified.

### 2. Stop evaluating the BSDF to answer a yes/no question

`bounce_emission_weight` and `escaped_emission` call `p.mat.eval(&p.ray, &p.rec,
p.dir)` and keep only whether the result is `Some`, and so does
`sample_bounce_direction`'s BSDF branch. For a MaterialX or
`PreviewSurface` material, each call is a full graph run, a reduction and a
full BSDF evaluation that is then discarded.

The `Material::eval` contract already guarantees that whether it returns `None`
"must never depend on `wi`". The answer is therefore known when the vertex is
scattered. Record it as a `has_continuous: bool` on the surface
`PrevVertex` record, set in `sample_bounce_direction`, and read the flag in
place of the three calls.

- **Expected output:** bit-identical. The same boolean is computed from the same
  hit, only once. Verify with `scripts/check_images.sh check` on all samples.
- **Cost:** one byte per vertex record.
- **Risk:** a material that breaks the contract (its `None` depends on `wi`)
  would change behaviour. Add a debug assertion comparing the flag with a fresh
  `eval` in debug builds while the change settles.

**Done.** No flag had to be stored: a *non-delta* sample is a draw from the
continuous component, so "`eval` would return `Some`" is `!sample.delta`, which
`PrevBounce` already carried. That implication is now stated in the
`Material::eval` contract, and the three calls are replaced by it, each behind a
`debug_assert!` that runs the old `eval` (`PrevBounce::continuous`;
`PrevBounce` itself shrinks to position, pdf and the delta flag in release).
All four materials already satisfied it: `OpenPBR` returns `None` from both
`scatter` and `eval` on the same `v·n ≤ 0` test, `MtlxMaterial` and
`PreviewSurface` delegate with the same shaded record, and `Emissive` never
scatters.

- **Output:** bit-identical on all 25 `check_images.sh` scenes, and on guided
  variants of `materialx_basic` and `usdpreview_textured` (the guided BSDF
  branch is one of the three sites).
- **Instructions** (`render_pixel`, callgrind): `materialx_basic` 1.954 G →
  1.519 G (−22.2%), `usdpreview_textured` 1.377 G → 1.130 G (−17.9%). `eval`
  calls on `materialx_basic`: 259 169 → 129 019 — what remains is NEE, so a
  vertex now runs its network 2.0 times.
- **Time** (`bench_ab.sh`, 6 interleaved reps, min / mean): `materialx_basic`
  −22.3% / −21.7%, `usdpreview_textured` −19.6% / −15.4%, `materialx_teapot`
  −12.5% / −13.2%, `cornellbox` −3.8% / −2.5% (untextured: it saves the
  `OpenPBR` evaluation, not a network run).

### 3. Shade once per hit

Split `Material` into a preparation step and a prepared BSDF, in the OSL /
pbrt-v4 shape:

```rust
trait Material {
    /// Runs the pattern network once at a hit.
    fn prepare(&self, r_in: &Ray, rec: &HitRecord) -> ShadingPoint;
    ...
}

/// Everything the integrator asks of a surface at one vertex, with no texture
/// or graph work left in it.
struct ShadingPoint {
    bsdf: OpenPBR,        // or a smaller, hit-specific BSDF value
    rec: HitRecord,       // with the graph's shading normal applied
    has_continuous: bool, // subsumes step 2
    ...
}
```

The integrator calls `prepare` once per vertex and routes `emitted_at`,
`scatter_importance`, NEE `eval` and every guided / MIS `eval` through the
`ShadingPoint`. The previous vertex keeps its `ShadingPoint` in `PrevVertex`, so
`bounce_emission_weight` and `escaped_emission` read it without shading again.

Things this has to respect:

- **The light list reads `emitted()`, which stays hit-free.** A material that
  emits only through `emitted_at` must still never become a light-list entry.
  The split must not blur that.
- **NEE order.** `eval_reads_textures` exists so that texture-reading materials
  trace the shadow ray *before* `eval` (on ALab, evaluating the texture network
  for occluded samples cost 87–97 s against 66 s). Once `prepare` has already
  run, `eval` no longer reads textures, and the reordering stops mattering. The
  cost it avoided moves into `prepare`, which runs whether or not the shadow ray
  is occluded, because the bounce needs it anyway.
- **Size of `ShadingPoint`.** `OpenPBR` is large. If copying it per vertex
  shows up in the profile, return a trimmed, hit-specific BSDF value instead.
  This is a second-order concern next to removing 2–4 graph runs.
- **`OpenPBR`, `Emissive` and other materials with no network** implement
  `prepare` as a cheap copy, so they pay nearly nothing.
- **Expected output:** bit-identical, if every query sees the same `OpenPBR` and
  `HitRecord` it saw before. Verify with `check_images.sh` on all samples.

This is the largest structural change in the plan and the one expected to pay
most. It changes the `Material` trait and the integrator's vertex records.

**Done**, in a narrower shape than sketched above:

- **`Material::resolve(r_in, rec, cos_theta_o) -> Option<Resolution>`** instead
  of a `prepare` every material must implement. `MtlxMaterial` runs its graph,
  `PreviewSurface` its texture inputs, and a Ptex `OpenPBR` its face lookup; each
  returns the fully resolved `OpenPBR` (`OpenPBR::into_resolved` applies Ptex
  last, as the per-query path did), the record with the shading normal, and the
  hit's emission.
  Everything else returns `None` and is queried in place: no copy, which is why
  `cornellbox` does not move.
- **`ShadingPoint`** wraps either case, and the integrator builds one per surface
  vertex, where the ray arrives. The emission there, the scatter, NEE's `eval`,
  the guide branch's `eval` and `make_ray` go through it. Every surface vertex
  scatters, so it is never wasted work. `PrevVertex` needs nothing from it,
  because step 2 already removed its `eval` calls.
- **Emission is computed from the pre-Ptex parameters.** `Resolution::emitted`
  answers as `emitted_at` would: gated for surfaces that cannot emit, and taken
  from the network's output *before* `into_resolved`, because OpenPBR's coat
  emission factor reads `base_color` and `emitted_at` never saw the Ptex one.
  The first version left emission on `emitted_at`, which re-ran a textured
  emitter's network (review on #151): `materialx_emissive` −7.3% `render_pixel`
  instructions once folded in, `usdpreview_textured` +0.25% (the gate).
- **`eval_reads_textures` is retired.** With the network already run, `eval` is
  cheaper than a shadow ray for every material, so NEE takes radiance → eval →
  shadow everywhere. That also skips shadow rays toward lights below a textured
  surface's horizon, which the reordered path used to trace (ALab below).
- `OpenPBR` size (the concern above) did not show up: copying it once per
  textured vertex is far below the graph runs it replaces.

- **Output:** bit-identical at 16 spp on all 25 `check_images.sh` scenes, both
  guided variants, and ALab frame 1004 (0 of 230 400 pixels differ). The 32 spp
  ALab runs below are timing only.
- **Runs per vertex: 1.0.** On `materialx_basic`, `resolve` runs 131 255 times
  and `scatter_resolved` 131 255 times; texture-fetch instructions fall from
  258 M (before step 2) to 87 M.
- **Instructions** (`render_pixel`): `materialx_basic` 1.519 G → 1.212 G after
  step 2 (−20.2%; −38.0% against the original 1.954 G), `usdpreview_textured`
  1.130 G → 1.019 G (−9.8%; −26.0% against 1.377 G).
- **Time** (`bench_ab.sh`, 6 interleaved reps, min / mean):

  | Scene | step 2 → step 3 | original → step 3 |
  |---|---|---|
  | `materialx_basic` | −21.8% / −25.6% | −41.4% / −41.5% |
  | `usdpreview_textured` | −12.9% / −11.3% | −27.4% / −28.2% |
  | `materialx_teapot` | −10.4% / −9.8% | −21.4% / −22.0% |
  | `ptex_quads` | −28.9% / −35.4% | −54.5% / −53.9% |
  | `cornellbox` | +0.5% / +0.7% | −1.3% / +0.6% |

- **ALab timing** (frame 1004, shot camera, 32 spp, `--stats` Render phase, two
  interleaved runs each, before emission was folded in): original 64.8 s / 73.1 s → step 3 56.7 s / 60.2 s
  (min −12.5%, mean −15.2%), shadow rays 16 273 773 → 11 496 977. The
  interior's occluded samples were the reason textured `eval` went after the
  shadow ray; with one network run per vertex that order no longer pays.

### 4. Make the interpreter faster, in safe Rust

If step 1, re-run after step 3, still shows non-texture `Program::eval` as a
significant share:

- **Closure compilation.** Compile each `Op` into a pre-built closure (or a tree
  of them) once at material load. This removes the per-instruction `match` and
  the per-operand bounds checks. It is typically 1.5–3× faster than a `match`
  interpreter, and needs no `unsafe`.
- **Optimisation passes over `Program`**, at compile time:
  - constant folding (for example a `Mix` whose mask is a constant, or a chain
    of arithmetic on constants);
  - dead-code elimination for ops that only feed lobes pruned at flatten time;
  - common-subexpression elimination;
  - arity specialisation, so scalar ops stop doing four-lane work;
  - fused instructions for frequent sequences (a texture fetch followed by a
    colour decode, a `Mix` with constant operands).

Keep the current interpreter as the reference and gate the new path behind an
environment switch (for example `CRUST_MTLX_OPT=0` restores the unoptimised
program). Pin the optimised program to the reference with a test over the
checked-in `.mtlx` fixtures at many `(u, v)` points, as `examples/mtlx_shade`
does by hand.

### 5. A JIT, only if steps 3–4 are not enough

If `Program::eval` still dominates after steps 3–4, compile each material's
program to machine code.

| Option | For | Against |
|---|---|---|
| **Cranelift** (`cranelift-jit`) | Pure Rust. About a millisecond to compile a material. Code quality roughly LLVM `-O1`. | Calling generated code needs one `unsafe` transmute to a function pointer, and every crate in the workspace is `forbid`/`deny(unsafe_code)`. |
| LLVM (inkwell) | What OSL uses; best code. | A C++ toolchain dependency, against the project's pure-Rust approach. |
| wasmtime | Safe API; Cranelift underneath. | Each texture fetch becomes a host call across the sandbox boundary, which is exactly the hot path. |
| Rust codegen, compiled ahead of time | Like MaterialX ShaderGen, targeting Rust. | Needs `rustc` at render time and `unsafe` dylib loading. |

**Cranelift is the realistic choice**, under these conditions:

- It lives in its own crate (`crust-jit`) that opts out of `forbid(unsafe_code)`
  for one audited block, behind a cargo feature. Nothing else in the workspace
  changes its unsafe policy.
- The interpreter remains the reference and the fallback. A switch (for example
  `CRUST_SHADER_JIT=0`) selects it at runtime.
- Output should be bit-identical to the interpreter. That holds if the generated
  code keeps the interpreter's operation order and emits no fused multiply-add.
  Pin it with the same fixture test as step 4.
- Texture fetches stay calls into the host's `Texture2D::eval`. A JIT does not
  make them faster.

### 6. Longer term: shade many points at once

Interpreter overhead (and JIT call overhead) can also be amortised by running
one program over a batch of shading points, in SIMD lanes. This is how batched
OSL and wavefront GPU renderers work. It requires the integrator to queue hits
by material instead of shading each path as it goes: a wavefront restructure,
not a shading change. It is recorded here as the direction after steps 1–5, not
as a near-term step.

## Validation for every step

- **Images:** `scripts/check_images.sh record` before, `check` after, at 16 spp.
  Steps 2–5 are expected to be bit-identical. Any difference is a bug until
  shown otherwise.
- **Time:** `scripts/bench_ab.sh` between the two binaries, reporting min and
  mean. Sequential before/after timings are not trusted here (see "Measuring a
  change" in `CLAUDE.md`).
- **Instructions:** callgrind, for steps whose gain is below the timing noise
  floor.
