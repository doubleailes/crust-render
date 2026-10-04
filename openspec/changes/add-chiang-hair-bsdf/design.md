# Design

## Context

See `proposal.md` (Why). The parts of today's code this change has to fit:

- **MaterialX closures.**
  - `crust-mtlx` compiles a document into a pattern program plus a closure tree:
    - `bsdf.rs`: `enum Bsdf`, `Leaf`, `Closure`.
    - `leaf()` checks a `KNOWN` category list. Anything else goes to
      `unsupported`. The leaf is dropped, with a warning that the node has no
      operator.
  - `crust-core` prepares the tree once per hit (`material/closure/mod.rs`):
    - `prepare()` builds each leaf's `Lobe` in a `Frame { t, b, n }` taken from
      the leaf's `normal` / `tangent` inputs, or else `rec.normal` /
      `rec.tangent`.
    - `eval_lobe` returns `f` without the cosine, plus the lobe's pdf, over the
      whole sphere. `eval_pdf` multiplies by `|l.z|`.
    - `sample_lobe` receives a 2D sample and one extra scalar.
    - `Prepared::event` classifies a sample for light path expressions by its
      hemisphere (`l.z < 0` means transmitted).
  - The JIT compiles only the pattern program. Lobes are Rust.
- **Curve hits.**
  - `crust-rt`'s `rounded_cone_intersect` and `cubic_curve_intersect` return
    `(t, outward)`. The position along the segment (`y / d2` on the cone body;
    0 or 1 on a cap; `u0 + s·(u1 − u0)` through the cubic subdivision) is
    computed and thrown away.
  - `PrimHit` / `RayHit` carry `u = v = 0` for curves, and no tangent.
  - `HitRecord.tangent` is a UV-derived `dPdu` that only meshes with a UV map
    get. A curve gets `ZERO`, so `Frame::new` invents an arbitrary tangent
    with `tangent_frame(n)`.
  - A hair BSDF evaluated against an arbitrary fibre direction is not
    approximately right. It is wrong.
- **Recovering vertices at the hit is deliberately partial.**
  - `rt_world.rs` `VertexSource` resolves vertices only for baked meshes and
    for direct, static, unlabelled instances.
  - Motion-blurred and `PointInstancer`-forwarded placements are `Unresolved`,
    because their transform cannot be recovered from `(geom_id, prim_id)`.
  - Normal maps tolerate that fallback. Hair would not: fur on instanced clumps
    and moving grooms would lose its fibre direction.
- **Self-intersection.**
  - Every query uses an absolute `t_min = 0.001`.
  - A scattered ray starts at `rec.p`. A transmitted one starts at
    `rec.p + 1e-4·wi`.
  - In a scene in centimetres (the USD default), a strand is about
    `8e-3` units across. So a ray continuing into the tube meets the tube's far
    wall from inside, and a shadow ray toward a light behind the strand is
    blocked by the strand itself.
- **The MaterialX references.**
  - In MaterialX 1.39.4, the genosl implementations of the three hair helpers
    are stubs: `vector(1.0)` and zeros. `chiang_hair_bsdf` maps to OSL's
    renderer-side closure.
  - The only complete in-library implementation is genglsl
    (`mx_chiang_hair_bsdf.glsl`). It evaluates the lobes without importance
    sampling, scales the result by `1/π`, and gives TRRT+ an `Np` of
    `1.0 / 2.0 * M_PI` (= π/2) where `1/(2π)` is meant.

## Goals / Non-Goals

**Goals:**

- A strand renders as a fibre: R, TT, TRT and TRRT+ lobes, with the cuticle
  tilt, absorption inside the fibre, and per-lobe roughness.
- The hair lobe obeys the same closure invariants as every other leaf:
  - sampling agrees with evaluation;
  - the mixture pdf integrates to one;
  - the lobe split sums bitwise to eval;
  - nothing exceeds what it receives (white furnace).
- The fibre direction is correct on every curve placement: top-level,
  instanced, `PointInstancer`-forwarded, and motion-blurred.
- Scenes without curves stay bit-identical and pay no measurable cost.
  Scenes with curves but no hair leaf stay bit-identical.

**Non-Goals:**

- **Ribbon curves.** Authored `normals` are still ignored, and every strand is
  a round tube.
- **The other `BasisCurves` import gaps.** `wrap` and `varying` widths are
  separate fixes.
- **Curve primvars reaching materials.** `geompropvalue` on curves is out of
  scope. A per-strand colour needs it, and it is the natural follow-up.
- **Production hair speed-ups.** No dual-scattering or other multiple-scattering
  approximation, no near-field / far-field switch, and no hair-specific
  `--stats`.
- **A scale-aware ray epsilon.** The absolute `t_min = 0.001` is unchanged; see
  Risks.
- **A hair-specific AOV label.**

## Decisions

### D1. The BSDF follows Chiang 2016 as pbrt-v3 implements it, with MaterialX's parameterisation

**What it is:**

- The lobe is pbrt-v3's `HairBSDF`:
  - `Mp` with the low-variance log-space form below `v = 0.1`;
  - `Ap` from the dielectric Fresnel at `cos θo · cos γo` and the absorption
    path `2 cos γt / cos θt`, with the closed-form TRRT+ term;
  - `Np` as a trimmed logistic around `Φ(p, γo, γt)`, and uniform `1/(2π)` for
    TRRT+.
- The parameters are MaterialX's nodedef inputs:
  - `roughness_R`, `roughness_TT` and `roughness_TRT` are each a
    `(v, s)` pair: longitudinal variance and azimuthal logistic scale. This is
    exactly what `chiang_hair_roughness` outputs. They are clamped to
    [0.001, 1] as genglsl does.
  - The √(π/8) factor on `s` is applied inside the logistic, as genglsl does
    (it is where pbrt multiplies `s`).
  - `tint_R`, `tint_TT` and `tint_TRT` multiply their lobe's `Ap`. TRRT+ uses
    `tint_TRT`.
  - `absorption_coefficient` is σa per unit radius.
  - `cuticle_angle ∈ [0, 1]` maps to `α = cuticle_angle·π − π/2`, and the
    longitudinal shift is applied as genglsl applies it: θi rotated by
    `(2 − 3p)·α` for p = 0, 1, 2. This is pbrt's shift with α negated, so a
    document renders its highlight on the same side as MaterialX's viewer
    shows it.

**Why pbrt for the BSDF:**

- genglsl's evaluation is a real-time approximation. Its `1/π` scale and π/2
  TRRT+ term fail a white furnace, and it has no sampling routine at all.
- pbrt-v3 is the implementation Chiang et al. published against, and OSL's
  closure is a port of it.
- Its tests (white furnace with uniform and with importance sampling, sampling
  weights, sampling consistency) port directly as acceptance tests.

**Alternative:**

- Transcribe genglsl line for line, as the subsurface change ported Typhoon.
- Rejected: it would bake two known bugs into crust and leave the integrator
  with nothing to importance-sample.
- The deviations are recorded in the materials design record.

### D2. The lobe owns its cosine; evaluation is over the whole sphere

- The frame is `x = t` (fibre direction), `z = n` (the tube's ray-facing
  normal), `y = z × x`. This is `Frame::new(n, t)`, which keeps `n` and
  Gram-Schmidts `t` against it, exactly as genglsl orthonormalises `X`
  against `N`.
- The offset across the fibre is not a kernel output:
  - `γo` is derived from the view direction projected onto the plane normal to
    the fibre, against `n`, as genglsl derives it.
  - `h = sin γo`.
  - On a round tube this is exact. On a tapered cone it is off by the taper
    slope, as it is in MaterialX.
- **The cosine.** `eval_lobe` returns `Σp Mp·Ap·Np / |l.z|`, pbrt's
  `f / AbsCosTheta(wi)`. The shared `·|l.z|` in `eval_pdf` cancels it.
- **Grazing directions.** Where `|l.z| < 1e-7`, both `f` and the pdf are 0.
  - This is a set of measure zero. Without the guard it would be `inf · 0`.
  - The guard sits on both sides, so sampling still agrees with evaluation.
- **Sampling.** `sample_lobe` takes pbrt's route:
  1. pick p ∝ `luminance(tint_p · Ap)`;
  2. sample `Mp` (two scalars);
  3. sample `Np` by the trimmed logistic (one scalar).
- **Random numbers.** It has a 2D sample and one scalar, three in all. It needs
  four, so the fourth is demultiplexed from the scalar as pbrt-v3's
  `DemuxFloat` does.
  - This is not a new random number generator. It splits one `openqmc` draw.
  - Alternative: a new keyed `K_HAIR` sub-domain in `tracer/path.rs`.
    Rejected, because it threads one extra draw through every lobe signature
    for one leaf kind.
- **The pdf** is pbrt's `Pdf`: the same p-mixture, `Mp · Np` per lobe, divided
  by `|l.z|`.
- **Continuous samples.** The samples are `LobeSample::Continuous`, with a ray
  cone spread of `√v` of the chosen lobe, capped at `MAX_SPREAD`.
- **The `transmits` flag stays off.** A hair leaf does not set the closure's
  `transmits`, so a continuation ray never carries an interior medium.

### D3. A hair leaf inside the closure tree

- **Combinators.** `mix`, `add` and `multiply` behave as for any leaf.
- **Albedo.** The directional albedo is `Σp tint_p·Ap(ωo)`. It is exact,
  because `Mp` and `Np` are normalised.
- **As the top of a `layer`.** A hair leaf passes `1 − albedo` to the base.
  That keeps the layer contract ("never more than top and base separately")
  without a special case. It is an odd authoring choice, and the design record
  will say so.
- **Light path expressions.** A hair leaf is classified like every other leaf,
  by hemisphere:
  - on the viewer's side of `n` (R, most of TRT): reflection, `Glossy`,
    label `specular`;
  - on the far side (TT, much of TRRT+): transmission, `Glossy`, label
    `transmission`.
  - The split per lobe would need several `LobeSplit`s per leaf. A `hair` label
    would mean renumbering the label ids the LPE DFA assumes. Neither is worth
    it before anyone asks for a hair AOV.
- **Unsupported uses are not reported.** `chiang_hair_bsdf` on a mesh is
  allowed, because MaterialX allows it. Its fibre direction is the mesh's UV
  tangent, or arbitrary without one. That is MaterialX's `Tworld` contract, so
  it is not a warning case.

### D4. The kernel reports the curve's span parameter and tangent through the instance chain

- **`PrimHit`** gains a `dpdu: Vec3A`.
  - Curve prims fill it with the curve tangent at the hit, in object space.
    The linear segment uses `p1 − p0`. A cubic span uses the Bézier derivative
    at its span parameter.
  - They also set `u` to the span parameter.
  - Every other primitive leaves `dpdu` at zero.
- **`InstancePrim::hit`** maps `dpdu` through the linear part of the transform
  it already applies to the normal (by its inverse transpose), so composition
  through nested and motion-interpolated instances is free.
- **`RayHit`** exposes `dpdu`.
- **`World::intersect`** uses it, normalised, as `HitRecord.tangent` when the
  geometry has no UV map. Only meshes have UV maps, so this is in effect
  "for curves".

**Why in the kernel:**

- Reconstructing it in `crust-core` from `(geom_id, prim_id, u)` has exactly
  `VertexSource`'s blind spot: `PointInstancer` fur and moving grooms.
- Embree makes the same choice for curves: it reports `u` and the geometric
  tangent-based `Ng`.

**What it costs and how it is pinned:**

- `PrimHit` and `RayHit` grow by one `Vec3A`. Neither has a size pin, unlike
  the 48 B, 64 B and 96 B prim records, which are untouched.
- Triangle packets write a zero. Instances add one matrix-vector product per
  accepted instance candidate.
- The cost must be measured by callgrind on `cornellbox` and `curves`, and
  reported in the archive.
- The budget is +0.3% instructions on `cornellbox`, which has instances but no
  curves. Over that, the transform moves to the final closest hit only.
- `t` and `normal` are computed exactly as before, so they stay bitwise
  identical. The SIMD-matrix and packet bit-identity tests pin that.

**Cubic subdivision:**

- The span parameter is `u0 + s·(u1 − u0)` of the winning sub-segment.
- The tangent is the analytic Bézier derivative at that parameter, not the
  chord direction, so it does not facet at the subdivision joints.

### D5. Rays leaving a hair vertex pass out of curve tubes

- **`crust_rt::Ray`** gains a flag (it fits in the struct's existing padding),
  "curve exits ignored".
- **Effect.** When it is set, a curve primitive rejects a hit where the ray
  *leaves* the tube (`dir · outward > 0`, tested in object space).
  - `dot(M d, M⁻ᵀ n) = d · n`, so the test is invariant through every
    instance transform, mirrors included.
  - Entry hits are untouched.
  - The flag has no effect on any other primitive.
- **Who sets it:**
  - a continuation ray, when the closure at its vertex holds a live hair leaf
    (`ResolvedClosure::ray`);
  - a shadow ray toward a sampled light from such a vertex (the NEE in
    `tracer/path.rs`, through a `ShadingPoint` query).
- **Why it is correct:**
  - Chiang's model already integrates the light's whole path through the fibre
    from the entry point. The far wall belongs to the same event.
  - A ray starting outside any other tube must enter that tube before it can
    leave it. So rejecting exit hits can never hide a *different* strand.
    It can only hide the strand the ray started in.
  - A hair strand still shadows other geometry, and other strands, as an
    opaque tube.

**Alternatives:**

- Offset the origin to the far side of the tube. Rejected: that needs the
  span's geometry at the hit, and is wrong for a bent cubic.
- Skip the own `prim_id`. Rejected: it misses neighbouring spans of the same
  strand at a joint, and needs the id on every ray.
- Make curves one-sided for every material. Rejected: it would break glass or
  transmissive materials on curves, which refract inward and must meet the
  exit.

### D6. The helper nodes are composed from existing ops, against a genglsl-transcribed table

- `chiang_hair_roughness` (three `vector2` outputs),
  `chiang_hair_absorption_from_color` and `deon_hair_absorption_from_melanin`
  are built in `Compiler::compile_node` from existing `Op`s: `Mul`, `Add`,
  `Clamp`, `Pow`, `Ln`, `Exp`, `Max`, `Min`, `Combine2`.
  `chiang_hair_roughness` uses `compile_named`'s multi-output path, as
  `artistic_ior` does.
- **No new `Op`, so `crust-jit` is untouched.** Its bitwise JIT-versus-
  interpreter test covers the helpers once a fixture uses them.
- **Reference values.**
  - The OSL oracle cannot pin these nodes: their genosl implementations are
    stubs.
  - A new `scripts/hair_reference.py` transcribes the genglsl functions in
    float64.
  - It writes `crates/crust-mtlx/tests/data/hair_helpers.txt`, in the same
    case format as `osl_oracle.txt`.
  - A new test, `crates/crust-mtlx/tests/hair_helpers.rs`, holds every lane to
    1e-5 relative.
  - The pattern-node requirement keeps its OSL wording. These three are named
    as pinned against genglsl instead.

### D7. A hair leaf's live state is known at prepare time; nothing changes for other materials

- `PooledClosure` gains a `hair: bool`, set in `walk()` when a hair leaf is
  pushed. It drives D5's flag.
- An OpenPBR, `UsdPreviewSurface` or `Emissive` vertex never sets it.
- Their rays and shadow rays are built exactly as today, so their images stay
  bit-identical. `scripts/check_images.sh` pins this on the sample set.

## Risks / Trade-offs

- **[Metre-scale grooms lose hair-to-hair interaction within 1 mm]**
  - `t_min = 0.001` is larger than a real hair's diameter (~7e-5 m), so
    neighbouring strands closer than 1 mm do not shadow or scatter into each
    other.
  - → Pre-existing and global. Recorded as a known gap in the materials design
    record and on the user documentation's limitations page, with the
    workaround: author in centimetres (`metersPerUnit = 0.01`, the USD
    default).
  - A scale-aware epsilon is its own change.
- **[A ray can re-enter its own strand at a joint]**
  - At a joint between segments, a ray that leaves its tube through one cone
    body can enter the neighbouring segment's cap sphere while still inside
    the union. That is an entry hit, so the flag keeps it.
  - → Bounded to rays at joints, heading along the strand. Measured on the
    fixture as the share of hair continuation rays whose first hit is their
    own geometry within one diameter.
  - If it is visible, the follow-up is to also skip entry hits on the same
    `geom_id` with `t <` the hit radius.
- **[The cuticle sign is a reading of genglsl]**
  - Neither MaterialX's nodedef nor its specification defines the sign of
    `cuticle_angle`.
  - → A test pins the direction of the R highlight's shift at
    `cuticle_angle > 0.5`, so a later correction is a visible, deliberate
    change.
- **[Roughness clamped at 1]**
  - The genglsl clamp of `(v, s)` to ≤ 1 caps longitudinal roughness at about
    β_m ≈ 0.62 and azimuthal at β_n ≈ 0.68. pbrt has no such cap.
  - → Kept, to match the MaterialX reference a document was authored against.
    Recorded as a deviation candidate in the design record.
- **[Hot-path cost of the tangent]**
  - → D4's budget and fallback. Callgrind on `cornellbox` before and after,
    reported in the archive.
- **[`DemuxFloat` costs stratification in the `Np` dimension]**
  - → The same trade pbrt-v3 makes. The sampling-consistency test is the guard
    on correctness. Noise is judged on the fixture against a uniform-sphere
    reference.

## Migration Plan

- **Additive.** A document using `chiang_hair_bsdf` used to lose that leaf,
  with a warning; it now renders the leaf, and the warning is gone.
- No authored attribute changes meaning, and there is no switch to flip.
- Rollback is a revert of the change.
