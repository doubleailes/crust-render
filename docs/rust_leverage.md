# Leveraging Rust's strengths

An audit of how crust uses what Rust is good at — traits and monomorphisation,
zero-cost abstractions, the type system as a proof checker, ownership-checked
concurrency — and where it could use more of it. Written 2026-09-27 against
the tree after the architecture pass (`docs/architecture.md` § Technical debt
register). Every proposal names the check that had to pass before it landed;
what was done, declined or left, with the measurements, is in
[Status](#status-implemented-2026-09-27). The sections below are the audit as
written, so their descriptions of "the current code" are of the tree before it.

References are by file and function name, not line number (the convention
`docs/color_management.md` adopted, so this page does not rot with every edit).
Paths are relative to `crates/`.

## Status (implemented 2026-09-27)

Every item in the recommended order was carried out on the branch
`claude/rust-leverage-plan-phy9zw`, one commit per change, each landing only
after `cargo fmt` / `clippy -D warnings` / the workspace tests, and — unless
the row says otherwise — `check_images.sh`-style 16 spp renders of **every
sample bit-identical** to the pre-branch goldens. Instruction counts are
callgrind at 2 spp, one thread; wall clock is `bench_ab.sh`-style
interleaving. Proposals that were measured and turned down are listed too,
with the number that turned them down, so the next audit does not repeat
them.

| # | change | § | result |
|---|--------|---|--------|
| 1 | `EvalTimeScope: !Send` | 2.5 | done (`PhantomData<*const ()>`) |
| 2 | `#[must_use]` on builders, queries, samplers | 2.8 | done |
| 3 | escaped rays loop over the lights at infinity only | 1.2 | done: usdlux −3.1%, veach_mis −2.0% instructions |
| 4 | interior medium built once per material | 4.1 | done: openpbr_showcase −0.29% |
| 5 | typed `Config` for every `CRUST_*` | 6 | done; each switch A/B'd old ↔ new binary, bit-identical. Booleans now accept `0/false/off/no` and `1/true/on/yes`, and warn on anything else |
| 6 | `PdfSolidAngle` / `InvPdfArea` at the light seam | 2.1 | done: ±0.02%, domelight +0.35% (the map pdf is now validated on both MIS sides) |
| 7 | `enum AreaShape` | 1.1 | done: veach_mis −0.76%, usdlux −0.70% |
| 8 | one `trilinear<S: MipSource>` for the four backends | 5.2 | done: usdpreview_textured −0.49%, ptex_quads +0.14%; streamed ↔ preloaded bitwise tests pass |
| 9 | `PatternMaterial` blanket `Material` impl | 5.1 | done: usdpreview_textured −0.43% |
| 10 | `ResolvedOpenPBR` + `tests/resolve.rs` (bitwise, every material kind) | 2.3 | done: ±0.03% |
| 11 | `Medium: Copy`, carried by value in `Ray` | 4.2 | done: instructions flat; wall clock on 4 threads within noise — the contention it removes needs many threads on one glass |
| 12 | `Axis` enum in the shear permutation | 3.1 | done: cornellbox −0.82% (`Bvh::hit` −2.3%), instancing −1.38%; `test_simd_matrix.sh` passes |
| 13 | `FromStr` / `Display` from one name table (`named!`) | 5.3 | done; the CLI builds its clap values from the same table |
| 14 | untextured `OpenPBR` queried statically in `ShadingPoint` | 1.4 | done: cornellbox −0.88%, openpbr_showcase −1.18% |
| 15 | scanline rows rendered in parallel, written in place | 4.3 | done: cornellbox −5.0%, materialx_basic −14.0%, ptex_quads −13.5% wall clock (min of 8) |
| 16 | the rest, below | — | — |

Item 16, by section:

| § | change | result |
|---|--------|--------|
| 1.3 | `LightList` holds `enum LightKind` | done: usdlux −0.34%, others ±0.05% |
| 2.2 | one `LightShape::solid_angle_sampler(from)` for both MIS halves | done: +0.03 to +0.05% once `inline(always)` (plain `inline`: `sample_li` +6%) |
| 2.4 | `ResolvedColorSpace` (no `Auto`) past the open; transfer curves as `TransferCurve` methods | done. `RawColor3` at the colour-attribute readers (`docs/color_management.md` gaps 1–2) is **not** done |
| 2.6 | `Option<FaceHit>`; `indirect_clamp: Option<f32>`; `Cell<Option<usize>>` stripe; subdivision `base_face: Vec<Option<u32>>` | done; ptex_quads −1.0% with `OpenPBR::shaded` `inline(always)` |
| 2.7 | `RayMask` newtype | done: ±0.007% |
| 2.7 | `WideNode::child(l) -> Child { Node, Leaf }` | **declined**: `Bvh::hit` +0.74% with `inline` or `inline(always)` |
| 2.7 | `GeomId` / `PrimId` | **not done**: it changes every geometry-keyed table in crust-core; a pass of its own |
| 3.2 | `& 3` on `trailing_zeros` lane indices | **declined**: `Bvh::hit` +0.7% |
| 3.3 | hoist the per-packet shear `expect` | **declined**: +0.5%; LLVM already hoists it |
| 3.4 | `Box<[T]>` for the finished BVH tables | done |
| 3.4 | `WideNode` `repr(C, align(64))` | **declined**: no gain in or out of cache (`ray_throughput --large`: −3.0% to +4.8% across scenes, noise) |
| 3.4 | triangle shading normals in a side table | done: `PrimNode` 128 → 80 bytes, cornellbox kernel memory −22%; +0.5 to +1.0% instructions (`Bvh::hit`); `ray_throughput --large` −0.2 to −5.8% |
| 3.5 | check "no FMA contraction" directly | done, at the IR level: `test_simd_matrix.sh` fails on any `contract` / fast-math flag or `llvm.fmuladd` in crust-rt. The suggested `vfmadd` grep would fail spuriously — **glam fuses `cross` on purpose** under `+fma`, so FMA builds shade slightly different normals from SSE2 ones (`docs/simd.md`) |
| 4.4 | guiding samples appended in place, `update` takes an iterator | done: cornellbox_guided −19.5% wall clock |
| 4.5 | Ptex `MICRO_BYTES` as a `StripedCounter` | done. The `.tx` shard `RwLock` and the `files` list are left until contention is measured, as the file header asks |
| 4.6 | parallel USD import | **not started** (large; the `MeshKey` `Arc`-address hazard stands) |
| 4.7 | `inputs:` names as literals; `MeshSource` moved, not cloned | done. Interned cache keys **not done** |
| 4.8 | in-place BVH build | **not done** |
| 4.9 | `SyncWrapper` for the JIT module | **declined**: it needs an `unsafe impl Sync`, and adding `unsafe` is a project decision (`CLAUDE.md`); the `Mutex` is uncontended |
| 5.4 | `Val` arithmetic operators; `LobePmf` indexed by `Lobe` | done: openpbr_showcase −0.14% |
| 5.5 | one `stats::breakdown` | done |
| 7 | `Error::source`, `AssetError` logged once as `WARN` at the seam, `main -> ExitCode`, CLI numbers validated at parse | done |

Found on the way, not caused by this work:

- `guided_tiles_and_rows_are_bit_identical` failed about 2 runs in 100 on the
  pre-branch tree (a guiding pass decision reads wall-clock time); after item
  4.4 it passed 100 of 100, but the time dependence is still there.
- `CRUST_MESH_BAKE=0` is not bit-identical, although `docs/architecture.md`
  listed it so: an instanced mesh is intersected in local space, and 348
  (pre-branch) / 391 (now) of cornellbox's pixels differ in the last ulp at
  16 spp, relmse 4e-18. Rounding, not a bug; the table now says so.

## Summary

Crust already uses Rust's strengths deliberately in the places where it was
*measured* to matter: the profiler and texture payload are const generics, the
kernel's primitive set is an enum rather than `dyn`, per-thread scratch is
reused, and the render phases are separated by the borrow checker rather than
by locks. The main gaps are elsewhere:

1. **Closed sets dispatched through `dyn`.** `Light` (3 implementors) and
   `LightShape` (3) are trait objects on the NEE hot path, nested two deep, and
   the escaped-ray path makes one virtual call per light in the scene.
2. **Invariants held by prose, not types.** The pairs `CLAUDE.md` lists as
   "must change together" — pdf measures, resolved vs unresolved materials,
   `ColorSpace::Auto` never reaching a lookup, the thread-bound import time —
   are almost all enforceable by a newtype or a marker, at zero runtime cost.
3. **Per-bounce allocation and refcounting.** Every refraction into a medium
   does an `Arc::new(Medium)`, and every ray carries an `Arc` whose clones are
   atomic operations on a cache line shared by all threads.
4. **Duplicated code a generic would unify.** Trilinear/bilinear filtering
   (×4), the pattern-material `Material` impl (×2), environment parsing (×6),
   string parsing that should be `FromStr`/`Display`.

The recommended order is at the end.

## What is already done well

These are the patterns to copy. Each was introduced for a measured reason,
recorded beside the code.

| pattern | where | Rust feature |
|---------|-------|--------------|
| Profiling compiled away when off | `render_pixel::<PROFILE>` → `trace_path::<PROFILE>` → `profile::scope_if::<PROFILE>` (`crust-core/src/tracer/`, `profile.rs`) | const generic `bool`, branch once per pass; the runtime-check version cost +2.5% instructions |
| Texture payload chosen once per lookup | `StreamingTexture::eval_as::<HALF>` (`crust-assets/src/tiled/stream.rs`) | const generic; the comment records why closure and enum variants were slower |
| Texel storage chosen once per lookup | `UvTexture::eval_tiles::<T: Texel>` (`crust-assets/src/uv_texture/`) | trait bound + one `match Storage` per lookup, not per texel |
| Closed primitive set | `PrimNode` enum over the `Prim` trait (`crust-rt/src/prim.rs`) | enum dispatch replaced `Box<dyn Prim>`; `CubicCurve`/`Instance` boxed to keep the enum small |
| Backend off the hot path | `TiledFile` copies `levels`, `tile_edge` out of `dyn Backend` at open (`crust-assets/src/tiled/mod.rs`) | `dyn` kept, but hoisted: the indirect call cost ~3% of a frame |
| Build/query typestate | `WorldBuilder::commit(self) -> World`, `SceneBuilder::commit` | consuming `self`: intersecting before commit does not compile |
| Phase separation without locks | `GuidingField` is `&` during a pass, `&mut` between passes (`tracer/mod.rs`, `guiding/field.rs`) | borrow checker enforces "no training while rendering" |
| Deterministic parallel reduction | tiles `par_iter().map().collect()` then serial scanline-order copy; `RayStats::merge` | ownership: no shared mutable framebuffer, so float sums are reproducible |
| Thread-safety by construction | every seam trait is `: Send + Sync` as a supertrait; no `Rc` in any `src/` | `Arc<dyn Material>` is shareable with no annotation at use sites |
| Contention-free counters | `StripedCounter` (128 cache-line-padded slots, `crust-assets/src/tiled/cache.rs`) | thread-local stripe index; a single atomic was measured as the ALab bottleneck |
| Borrowing hot lookups | `with_tile` closure over a thread-local microcache | a hit does no `Arc` refcount traffic |
| Hot-path scratch reuse | `PathScratch`, MaterialX `SLOTS` thread-local, BVH `TraversalStack { inline: [u32; 32], spill }` | stack arrays / reused `Vec`s: `PathScratch` removed 4.4% of cornellbox |
| Audited `unsafe` | `crust-jit`: four blocks, each `#[allow(unsafe_code)]` + `// SAFETY:`; `JitProgram::new` refuses programs failing `Program::is_well_formed()` | `deny(unsafe_code)` elsewhere is `forbid` |
| Bit-identity by test | `to_bits()` comparisons: `Tri4` ↔ scalar, JIT ↔ interpreter, streamed ↔ preloaded | IEEE semantics without fast-math; fat LTO provably moves no bits |
| `panic = "abort"`, fat LTO, 1 CGU | root `Cargo.toml` | measured −1.5% instructions vs thin LTO, landing pads removed |
| No `Any`, no downcasting, no `Box<dyn Fn>` | whole workspace | closures go through `impl Fn` generics |

## 1. Dispatch: closed sets as enums

Rust's rule of thumb: an **open** set (plugged in from outside the crate) is a
trait; a **closed** set is an enum, whose `match` the optimiser can inline and
whose variants the compiler checks exhaustively. Crust follows this for
`PrimNode`, `Lobe`, `PrevVertex`, `SamplingStrategy`, `UnitShape`,
`PixelFilter` and the interpreter's `Op`. It does not for three hot seams.

### 1.1 `LightShape` → `enum AreaShape` (high payoff, medium risk)

`AreaLight` holds `Box<dyn LightShape>` (`crust-core/src/light/area.rs`), and
`AreaLight::sample_li` makes three or four virtual calls through it per NEE
sample (`sample_solid_angle`, `sample_point`, `normal_at`, `inv_pdf_area` via
`pdf_toward`). The three implementors — `SphereShape`, `AffineShape`,
`RectShape` — are all in `light/`, and `AreaLight`'s fields are `pub(super)`,
so the set is closed in practice. `AffineShape` already matches on `UnitShape`
internally.

```rust
pub(crate) enum AreaShape { Sphere(SphereShape), Affine(AffineShape), Rect(RectShape) }
```

Keeping `LightShape` as the trait each variant implements, and `impl
LightShape for AreaShape` by `match`, is the same move `PrimNode` made: the
trait stays the contract, the enum is the dispatch. Removes one heap box and
one vtable hop per call, and lets the shape's sampling inline into
`sample_li`.

### 1.2 Infinite lights in their own list (high payoff, low risk)

`escaped_emission` (`tracer/path.rs`) iterates **every** light via
`LightList::iter_at` and calls `light.escaped(from, direction)` virtually;
`AreaLight::escaped` always returns `None`. A scene with N area lights pays N
wasted virtual calls per escaped ray. Storing the indices of lights whose
`escaped` can return `Some` (distant and dome) once, in `LightList::new`, and
iterating only those, keeps the pmf lookups (`pmf[index]`) unchanged. This is
not even an enum change — just a precomputed `Box<[u32]>`. Output must be
bit-identical (the skipped calls contributed nothing), which
`check_images.sh check` proves.

### 1.3 `Light` → enum (medium payoff, medium risk)

`LightList` holds `Vec<Arc<dyn Light>>`; `sample_li`, `pdf_at_point` and
`escaped` are each one vtable call on the NEE / bounce paths. The three
implementors are in-crate and nothing in `crust-render` implements `Light`.
After 1.1 and 1.2, an `enum LightKind { Area(AreaLight), Distant(..), Dome(..) }`
would make the whole NEE sample statically dispatched. Lower priority than 1.1
because the outer call is one hop, not three.

### 1.4 `Material`: keep the trait, add a static fast path (medium payoff, medium risk)

`Material` is a real seam (`pub`, four implementors, probes and tests rely on
it being open), so it should stay a trait. But `ShadingPoint::new` makes a
virtual `resolve`, and when that returns `None` — every untextured `OpenPBR` —
also a virtual `emitted_at`, then `scatter_importance`, `eval`, `make_ray`
through `Resolved::Material(&dyn Material)`: 4–5 vtable calls per vertex, each
re-running `self.shaded(rec)` on `OpenPBR`.

Every non-`Emissive` material reduces to `OpenPBR`. A provided trait method
`fn as_openpbr(&self) -> Option<&OpenPBR> { None }`, overridden by `OpenPBR`,
lets `ShadingPoint::new` take `Resolved::OpenPBR` for the untextured case too,
making the rest of that vertex static. This keeps the open seam and the
"resolve returns exactly what per-query shading would" contract (the path taken
is the same concrete code).

**Verification for all of §1:** these are hot-path changes. Per `CLAUDE.md`,
compare per-function instruction counts under callgrind (`render_pixel`,
`sample_li`, `scatter_resolved`) and prove the image bit-identical with
`check_images.sh` at 16 spp.

## 2. Types as proofs: making the "pairs" structural

`CLAUDE.md` notes that most bugs here were one half of a pair changing without
the other. Several pairs can be enforced by the compiler. Newtypes with
`#[repr(transparent)]` cost nothing at runtime.

### 2.1 Pdf measure newtypes (high payoff, low risk)

Every pdf is a bare `f32`, and measure lives in doc comments:
`LightSample::pdf` (solid angle), `LightShape::inv_pdf_area` (reciprocal area),
`sample_solid_angle`'s `(Vec3A, f32)`. `AreaLight::pdf_toward` converts inline.
`light/area.rs` records a real measure bug: an `1e-4` added to the conversion
denominator made the result depend on scene units (+100% on a face-on 1 cm²
light in a scene modelled in metres).

```rust
#[repr(transparent)] #[derive(Clone, Copy, PartialEq, PartialOrd)]
pub struct PdfSolidAngle(f32);
#[repr(transparent)] #[derive(Clone, Copy, PartialEq, PartialOrd)]
pub struct PdfArea(f32);

impl PdfArea {
    /// The one area → solid-angle conversion. `None` edge-on or at zero
    /// distance: refused, never a finite stand-in.
    pub fn to_solid_angle(self, dist2: f32, cos_light: f32) -> Option<PdfSolidAngle>;
}
```

`SamplingStrategy::light_weight` / `bounce_weight` and `LightList::density`
then take `PdfSolidAngle` on both arguments, so a BSDF pdf and a light pdf of
different measures cannot be MIS-combined. The "non-finite density is refused
on both sides" rule becomes a constructor (`PdfSolidAngle::new(f32) ->
Option<Self>` refusing non-finite and non-positive), which also retires the
`pdf <= 0.0` "refused" convention: `AreaLight::solid_angle_pdf` currently
flattens `pdf_toward`'s `Option<f32>` to `0.0` with `unwrap_or`, and the caller
re-tests `point_pdf <= 0.0`.

Start at the light seam (`light/`, `tracer/path.rs`, `tracer/settings.rs`);
BSDF pdfs follow. The newtype compiles to the same code, but check it under
callgrind anyway since it touches `trace_path`.

### 2.2 One sampler for `sample_solid_angle` ↔ `solid_angle_pdf` (medium payoff, low risk)

These are two independent defaulted methods on `LightShape`; that they answer
for exactly the same `from`s is prose. Each implementation already routes both
through one constructor (`SubtendedCone::new`, `spherical_rect(from)?`), and
`AffineShape` repeats its `unit != Sphere` guard in both. Make that structural:

```rust
fn solid_angle_sampler(&self, from: Vec3A) -> Option<impl SolidAngleSampler>;
trait SolidAngleSampler {
    fn sample(&self, u: f32, v: f32) -> (Vec3A, PdfSolidAngle);
    fn pdf(&self, dir: Vec3A) -> PdfSolidAngle;
}
```

The `from`-only decision is made once; neither half can refuse a `from` the
other accepts. (`impl Trait` in trait return position is stable; if
`LightShape` stays object-safe for now, return a concrete enum of the two
sampler kinds instead.)

### 2.3 Resolved materials as their own type (medium payoff, low risk)

`OpenPBR::into_resolved(self, rec) -> OpenPBR` returns the same type, and the
design record's trap — take `Resolution::emitted` from the parameters *before*
`into_resolved` — is convention. Return a `ResolvedOpenPBR` newtype that has
the `*_resolved` methods and no emission method, and give `Resolution` one
constructor that computes `emitted` itself from the unresolved value before
resolving. Swapping the order then does not type-check. Add a generic test
over every `Material` that `resolve` matches per-query shading bit for bit
(today only `preview_surface::resolve_emits_what_emitted_at_does` exists).

### 2.4 Colour spaces in the type (medium payoff, low risk)

- `ColorSpace::Auto` "never reaches a lookup", but `resolve_auto` returns
  `ColorSpace`, and `uv_texture/mip.rs` treats `Raw | Auto` alike. A
  `ResolvedColorSpace` enum without `Auto` makes the resolved state
  unrepresentable-as-unresolved.
- `docs/color_management.md` Known gaps #1–2 already proposes a `RawColor3`
  newtype at `shader_input_vec3` / `attr_color3f` / `custom_color3`, so a newly
  added colour attribute has to state its source space to get a `Vec3A`. This
  audit confirms that is the right tool; it is unimplemented.
- The helpers `to_linear(space, x)`, `encode_fn(space)`,
  `to_linear_table(space)` are free functions on a `ColorSpace` argument;
  as methods they are discoverable and cannot be called with the wrong enum.

A full `Rgb` spectrum type (separate from points and directions) is **not**
recommended: glam's `Vec3A` operators are exactly the ones colours need, and a
wrapper would have to forward all of them for little safety gain. The value is
at the *boundaries* (encoded vs linear, raw vs resolved), not in arithmetic.

### 2.5 Thread-bound import time (high payoff, trivial)

`EvalTimeScope` (`scene/usd_import/time.rs`) restores a thread-local on drop,
but is `Send`: moved to another thread it would restore the time on the wrong
one. `profile::Scope` already carries a `PhantomData` marker to pin it to its
thread; do the same here:

```rust
pub(super) struct EvalTimeScope(Option<f64>, PhantomData<*const ()>);
```

The comment's invariant ("a change that reads attributes from a rayon task
must thread the time explicitly") becomes a compile error for the guard, and
is the first step of §4.6.

### 2.6 Sentinels that should be `Option` (low payoff, low risk)

| sentinel | location | better |
|----------|----------|--------|
| `HitRecord::NO_FACE = u32::MAX`, `face_uv` "meaningless" when set | `hittable.rs` | `Option<FaceHit { id, uv }>` ties the uv to the id |
| `base_face = u32::MAX` | `scene/subdiv/`, `usd_import/mesh.rs` | same |
| `indirect_clamp == 0.0` means off | `tracer/settings.rs` | `Option<f32>` validated at construction |
| `STRIPE = usize::MAX` means unset | `tiled/cache.rs` | `Cell<Option<usize>>` |

Keep the sentinels where layout is the point: `NO_CHILD` in the guiding
`DTree` node, the light-cache `slot`, the BVH `EMPTY_LANE`/`INVALID_ID`.
`NonZeroU32` does not fit them (index 0 is valid); if an `Option` is wanted
there, a `NonMaxU32`-style newtype is the tool.

### 2.7 Kernel id newtypes (low payoff, low risk; API change)

In `crust-rt`, `geom_id`, `prim_id` and every node, leaf and packet index are
bare `u32`. The clearest hazard is `WideNode.child[l]`, which indexes `leaves`
or `wide` depending on a flag bit. `GeomId(u32)` / `PrimId(u32)` on the public
API (`attach`, `RayHit`) and a `WideNode::child(l) -> Child { Node(NodeIdx),
Leaf(LeafIdx) }` accessor make mix-ups unrepresentable at zero cost. Ray masks
(`MASK_*` in `crust-core/src/ray.rs`) are the same story: a `RayMask(u32)`
with `BitAnd`/`BitOr` and associated consts. This is a breaking change to the
`crust_rt` vocabulary that crust-core adopts, so do it in one pass.

### 2.8 `#[must_use]` (trivial)

There is none in the workspace. `Scene::intersect` / `occluded`,
`SceneBuilder::commit`, `WorldBuilder::commit`, `Ray::with_time` /
`with_mask`, `JitProgram::new`, `Material::resolve` and every `Option`-
returning sampler are values that are always a bug to drop.

## 3. Zero-cost in the kernel: bounds checks and layout

`crust-rt` is `forbid(unsafe_code)`, so there is no `get_unchecked`, and the
remaining bounds checks have to be removed by giving LLVM a proof. The kernel
already does this where it matters most (the traversal stack tests its range
before indexing; leaves slice once and iterate). What is left:

### 3.1 `Axis` enum for the shear permutation (medium payoff, medium risk)

`RayShear` stores `kx`, `ky`, `kz` as `usize`, and `Tri4::intersect` indexes
`self.v[0][kz]` etc. with them — runtime indices LLVM cannot bound, three
checks per packet. The scalar path's `a[kx]` goes through glam's panicking
`Index`. An `#[repr(u8)] enum Axis { X, Y, Z }` with `Vec4`-row accessors that
`match` makes every access provably in range. Must stay bit-identical to the
scalar path (same operations, same order), which `simd_matches_scalar_bitwise`
checks under every codegen in `scripts/test_simd_matrix.sh`.

### 3.2 Lane indices from `trailing_zeros` (low payoff, medium risk)

`l = mask.trailing_zeros()` is known to LLVM only as `≤ 32`, so `tn[l]`,
`out.t[lane]`, `packet.prim[lane]` keep their checks. `l & 3` (free on x86) or
iterating a `Hit4` as `(0..4).filter(...)` bounds them. Inspect the asm before
and after; callgrind shows the instruction delta.

### 3.3 Per-packet `expect`s (low payoff, low risk)

`shear.expect(...)` in `intersect_leaf` / `occlude_leaf` and
`.as_triangle().expect(...)` run per packet. Making `shear` a plain
`RayShear` whenever a leaf has packets (compute it eagerly, it is cheap), or
storing what `triangle()` needs in the packet, removes the branch. With
`panic = "abort"` the cost is a compare-and-branch, not a landing pad.

### 3.4 Layout

- `WideNode` is 128 bytes with `align_of == 16` (pinned in `bvh/tests.rs`), so
  a node can straddle three cache lines, not the two its comment assumes.
  `#[repr(C, align(64))]` makes the claim true. Measure with `ray_throughput`
  min-of-40; update the pinned `align_of` test.
- `TrianglePrim.normals: Option<[Vec3A; 3]>` is stored inline even when
  `None`, and dominates `PrimNode`'s ~128 bytes. A side table indexed by prim
  shrinks every baked triangle; it is read only in `hit_from_barycentric`,
  after traversal.
- Finished BVH arrays (`wide`, `leaves`, `packets`, `prims`) are `Vec`;
  `into_boxed_slice()` at the end of `Bvh::new` drops the capacity slack that
  `MemoryFootprint` counts and states "immutable after build" in the type.

### 3.5 What *not* to do in the kernel

- **Const generics for lane width / BVH arity** (`WideNode<const N>`). Glam
  has no 8-lane type and `bvh/lane_width.rs` shows 8-wide leaves buy nothing; the
  abstraction would have no second instantiation. Revisit only if BVH8 lands.
- **`std::simd`**: nightly-only (`docs/simd.md`); crust builds on stable.
- **`#[inline(always)]`** on `Tri4::intersect` / `slab4` without a callgrind
  showing they are not inlined: fat LTO already inlines them.
- **Tooling gap**: `test_simd_matrix.sh` checks test results, not codegen.
  Grepping the `+fma` build for `vfmadd` in the kernel would test the
  "no FMA contraction" claim directly instead of inferring it.

## 4. Ownership, allocation and concurrency

### 4.1 Cache the interior medium (high payoff, low risk)

`OpenPBR::interior_medium` rebuilds the `Medium` (including a `Medium::blend`
for subsurface) and does `Arc::new` on **every** sampled or evaluated
refraction into it. The result depends only on the material's parameters.
Compute it once — at construction for constant `OpenPBR`, or in the
`ResolvedOpenPBR` of §2.3 for textured ones — and hand out clones.

### 4.2 Stop refcounting the medium per ray (medium payoff, medium risk)

`Ray` carries `Option<Arc<Medium>>`, so `r.clone()` and each `m.clone()` in
`trace_path` is an atomic increment on a cache line that every thread shading
the same glass contends for. Two options, both enabled by ownership:

- `Medium` is plain data (two `Vec3A`s and an `f32`, `Clone`): derive `Copy`
  and carry `Option<Medium>` by value. No refcount, no indirection; `Ray`
  grows, so measure.
- Or borrow: `Ray<'a> { medium: Option<&'a Medium> }`. Materials outlive every
  path, so the lifetime is sound, but `'a` spreads through `trace_path`'s
  signatures.

The first is simpler and likely enough.

### 4.3 Parallel framebuffer writes with disjoint borrows (medium payoff, medium risk)

After the tile or row join, one thread copies every result into `Buffer`
(`tracer/mod.rs`), and the scanline path pays a fork/join barrier per row.
`buffer.data.par_chunks_mut(width)` (zipped with the variance map) gives each
worker a disjoint `&mut` row — the borrow checker proves the absence of races
with no lock or atomic. Keep order-dependent reductions (`variance_sum`,
guiding samples) serial or as per-row partials reduced in order, so the image
stays bit-identical (tiles ↔ scanlines is a pinned pair).

### 4.4 Guiding samples without the double copy (low payoff, low risk)

`render_pixel` returns a fresh `Vec<SampleData>` per pixel in training passes;
it is moved into a per-tile `Vec`, then appended to `all_samples`. Pass a
per-tile `&mut Vec<SampleData>` down instead, and let `GuidingField::update`
take `&[Vec<SampleData>]` in scanline order.

### 4.5 Texture cache contention (measure first)

- `.tx` shard hits take a `Mutex` only to set `e.used` and clone the `Arc`
  (`tiled/cache.rs`). `used: AtomicBool` plus `RwLock<HashMap>` makes hits
  read-locked. The file header says to wait for measured contention; this is
  the change to try when it shows up.
- `files: Mutex<Vec<Arc<FileSlot>>>` is locked on every miss; the list is
  append-only, so an `RwLock` (or building it before the render) suffices.
- Ptex's `MICRO_BYTES: AtomicU64` is written by every thread on every
  microcache insert/evict (`ptex_stream.rs`). The workspace already has the
  answer: a `StripedCounter`, or a thread-local count published at report time.

### 4.6 Parallel USD import (large, later)

The import is single-threaded because of the `EVAL_TIME` thread-local. Rust
would make a parallel version checkable: each worker opens its own masked
`Stage` (needs `Stage: Send`, not `Sync` — unverified, the openusd source is
not vendored), the time travels as a `Copy` field in a per-worker context, and
`par_iter().map().collect()` over chunks keeps output order deterministic. The
hazards the compiler will *not* catch:

- `MeshKey.material` is an `Arc` **address**, unique only because nothing is
  freed during import. Merging per-worker material caches would produce
  duplicate `Arc`s and silently break dedup. Key on an interned material id
  first.
- Geom id order and `MeshArena.slots` order must be preserved by merging in
  chunk order.

A cheaper first step needs no stage access: hash (`MeshKey::new`) and
triangulate collected `MeshSource`s in a `par_iter` before the serial dedup,
with a faster non-SipHash hasher for the content hash.

### 4.7 Import-time allocation (low payoff, low risk)

- Material, prototype, texture and Ptex caches are keyed by `String` built
  per lookup (`MaterialCache::key`, `(epoch, proto_path.to_string())`). An
  interned `Arc<str>` or `u32` id avoids the allocation.
- `format!("inputs:{name}")` runs for every attribute read; the names are
  static, so `&'static str` constants (or a `Cow`) suffice.
- `src.normals.clone()` in `mesh.rs` copies a whole `Vec` that can be moved.

### 4.8 BVH build in place (medium payoff, build time only)

`merge` copies both child node arrays at every internal node (O(n log n)
copying), and `partition_by_bin` allocates two `Vec`s per node. Partitioning a
`&mut [PrimRef]` in place and handing `split_at_mut`'s halves to `rayon::join`
is the idiomatic, borrow-checked form; spatial splits (which duplicate
references) still need their own buffers. The tree must come out identical
(same split decisions), which the kernel tests and `check_images.sh` pin.

### 4.9 Small ones

- `crust-jit` wraps `JITModule` in `Mutex<Option<..>>` only to be `Sync`; it is
  touched solely in `Drop` via `get_mut`. A wrapper that only hands out `&mut`
  (the `SyncWrapper` pattern) is `Sync` without a lock. No speedup, but it says
  what it means.
- Not a candidate: the progress `Mutex<u64>` taken per tile looks like an
  `AtomicU64::fetch_add`, but it is deliberate — incrementing and reporting
  under one lock is what keeps the callback's values increasing.

## 5. Standard traits and generic deduplication

### 5.1 Pattern materials: one blanket impl (medium payoff, low risk)

`MtlxMaterial` and `PreviewSurface` each write the same `Material` impl: a
`shade(f: impl FnOnce(&OpenPBR, &HitRecord))`, forwarding
`scatter_importance` / `eval`, a `can_emit` gate, and the same `resolve` body.

```rust
pub(crate) trait PatternMaterial: Send + Sync {
    fn run(&self, r_in: &Ray, rec: &HitRecord) -> (OpenPBR, HitRecord);
    fn can_emit(&self) -> bool;
}
impl<T: PatternMaterial> Material for T { /* written once */ }
```

Monomorphised, so the generated code is what it is today — but the next
pattern material cannot get the forwarding subtly wrong.

### 5.2 One generic trilinear filter (medium payoff, low risk)

Trilinear LOD selection is written four times (`uv_texture/mod.rs`,
`tiled/stream.rs` — whose comment calls itself a "line-for-line equivalent" —
`ptex_texture.rs`, `ptex_stream.rs`), and bilinear sampling four times. A
crate-private trait keeps static dispatch:

```rust
trait MipSource { fn levels(&self) -> usize; fn dims(&self, l: usize) -> (u32, u32);
                  fn bilinear(&self, l: usize, u: f32, v: f32) -> [f32; 4]; }
fn trilinear<S: MipSource>(s: &S, u: f32, v: f32, footprint: f32) -> [f32; 4];
```

Four copies of the same pair is exactly how a "streamed ↔ preloaded"
bit-identity pair drifts. The existing bitwise streamed-vs-preloaded tests pin
the refactor. The UDIM addressing copied between `uv_texture` and
`tiled/stream.rs` (the latter inlines `1001 + tu + 10 * tv` rather than
calling `udim_number`) folds into the same pass.

### 5.3 `FromStr` / `Display` instead of ad-hoc names

`SamplingStrategy` and `LightSelection` are parsed inline in
`usd_import/settings.rs`, mirrored by clap enums with `From` in
`crust-render/src/main.rs`, and parsed again in `examples/light_occlusion.rs`.
`PixelFilter::from_name`/`name`, `TexOutput::from_name`, `Category::name`,
`Section::name` are the same pattern. Implementing `FromStr` and `Display` on
the core types gives one spelling of each name, and clap can use `FromStr`
directly (`value_parser`), removing the mirror enums and the
`From<Filter> for PixelFilter` that goes through a string and an `expect`.
Leave `ColorSpace::from_mtlx` / `from_usd` as they are: their defaults differ
on purpose.

### 5.4 Operators where the code spells them out

- `crust_mtlx::Val` has `zip`/`map` only, and the interpreter builds `+ − ×`
  from closure chains. `Add`/`Sub`/`Mul` with broadcasting make them read as
  arithmetic. The JIT ↔ interpreter bitwise test pins the change (operand
  order must stay the same).
- Texel blends are manual `for k in 0..3` loops over `[f32; 4]`, while
  `utils::Lerp` is implemented only for `f32`. `impl Lerp for [f32; 4]` (or
  `glam::Vec4`) removes them.
- `LobePmf` keeps five named fields parallel to `Lobe` with a hand-unrolled
  `pick`. `[f32; 5]` with `impl Index<Lobe>` ties the two together, so adding
  a lobe cannot miss the pmf.

### 5.5 Small duplications

- `LightList::kind_breakdown` and `World::material_breakdown` are the same
  function; both also have a doc comment that belongs to a neighbour.
- `TextureRef` and `PtexRef` are the same newtype-for-`Debug` written twice.
- `build_pyramid` / `reduce_half` take `encode: fn(f32) -> f32` called per
  texel; load-time only, but a generic `impl Fn` would inline it.

## 6. Configuration: one typed layer for `CRUST_*`

`docs/architecture.md` lists this as the top open debt item; the audit adds
detail. There are 19 `std::env::var` reads, hand-parsed six different ways:

- Only `CRUST_RAY_CONES`, `CRUST_MTLX_OPT` and `CRUST_SHADER_JIT` are cached in
  `OnceLock`. `CRUST_SUBDIV` and `CRUST_MESH_BAKE` are re-read per prim;
  `CRUST_TEX`, `CRUST_PTEX`, `CRUST_*_MIP`, `CRUST_TEX_MAX`,
  `CRUST_PTEX_MAX_LOG2` per texture open; `cache_budget_from_env()` is called
  from six sites, so a bad `CRUST_PTEX_CACHE_MB` warns once per call.
- Booleans are off only for the exact string `"0"`; `CRUST_PTEX_STREAM` is on
  only for `"1"`. `CRUST_TEX_MAX` falls back silently on bad input while the
  budgets warn; `CRUST_PTEX_STREAM_MIN_MB` accepts 0, its siblings do not.

The Rust shape is a single struct built once (`LazyLock`), parsed with
`FromStr`, with the table in `docs/architecture.md` as its field list:

```rust
pub struct Config { pub ray_cones: bool, pub tex_max: usize,
                    pub tex_cache_mb: NonZeroU64, pub ptex_mip_space: MipSpace, /* … */ }
fn env_flag(name: &'static str, default: bool) -> bool;             // one boolean grammar, warns once
fn env_parse<T: FromStr>(name: &'static str, default: T, ok: impl Fn(&T) -> bool) -> T;
pub fn config() -> &'static Config;
```

`crust-assets` already depends on `crust-core`, so the module can live in
`crust-core`. Tests that need a different setting construct a `Config` and pass
it (`FileAssets::new(config)`) rather than mutating the process environment —
which, since edition 2024 made `std::env::set_var` `unsafe`, is also the only
way that fits `forbid(unsafe_code)`. Keep the "off side = old behaviour" rule;
changing which spellings count as off is a behaviour change to note in the
table.

## 7. Errors

- `crust-core`'s `Error` is hand-rolled (`Display` + `std::error::Error`), which
  is fine for five variants, but `UsdOpen { message: String }` flattens its
  cause: implementing `source()` over a boxed inner error keeps the chain.
- `crust-assets` has no error type: `Result<_, String>` in `ptex_texture.rs`,
  `ptex_stream.rs`, `tiled/make.rs`, and a dozen `.ok()?` that turn a real
  decode failure into the same `None` as "declined by `CRUST_TEX=0`". The
  `AssetLoader` contract ("`None` means fall back") is right and should stay;
  the fix is internal: an `AssetError` enum inside crust-assets, so a failure
  can be logged as `WARN` at the one place it becomes `None`, and the decline
  reasons (`PreloadReason` already exists) stay distinct from failures.
- `crust-render`'s `main` calls `std::process::exit(1)` from five places.
  `fn main() -> ExitCode` (or `Result<(), E>`) returns through destructors,
  which matters once anything buffered — the log file tee — needs flushing.
- `--filter-radius` and `--indirect-clamp` are silently clamped in core
  (non-finite or negative → 0); a clap `value_parser` rejects them at parse
  time as usage errors, the way `parse_frame` already does.

## Recommended order

Ranked by payoff over risk. "Bit-identical" means `check_images.sh check`
passes unchanged; "callgrind" means the per-function instruction comparison
from `CLAUDE.md` § Measuring a change.

| # | change | section | proof required |
|---|--------|---------|----------------|
| 1 | `EvalTimeScope: !Send` | 2.5 | compiles |
| 2 | `#[must_use]` on builders, queries, samplers | 2.8 | clippy clean |
| 3 | escaped-ray loop over infinite lights only | 1.2 | bit-identical + callgrind |
| 4 | cache `interior_medium` per material | 4.1 | bit-identical + callgrind |
| 5 | typed `Config` for `CRUST_*` | 6 | switch table unchanged; A/B each switch |
| 6 | `PdfSolidAngle` / `PdfArea` at the light seam | 2.1 | bit-identical + callgrind |
| 7 | `enum AreaShape` for `LightShape` | 1.1 | bit-identical + callgrind |
| 8 | generic `trilinear<S: MipSource>` | 5.2 | streamed ↔ preloaded bitwise tests |
| 9 | `PatternMaterial` blanket impl | 5.1 | bit-identical |
| 10 | `ResolvedOpenPBR` + generic resolve test | 2.3 | bit-identical |
| 11 | `Medium: Copy`, carried by value in `Ray` | 4.2 | bit-identical + `bench_ab.sh` |
| 12 | `Axis` enum in `Tri4::intersect` | 3.1 | `test_simd_matrix.sh` + callgrind |
| 13 | `FromStr`/`Display` for settings enums | 5.3 | CLI tests |
| 14 | `OpenPBR` static fast path in `ShadingPoint` | 1.4 | bit-identical + callgrind |
| 15 | parallel framebuffer writes | 4.3 | tiles ↔ scanlines bitwise + `bench_ab.sh` |
| 16 | everything else in §2.6, §2.7, §3.2–3.4, §4.4–4.9, §5.4–5.5, §7 | — | per item |

Items 1–2 are mechanical. Items 3–4 are the cheapest measurable speedups.
Items 6, 7 and 10 turn three of the `CLAUDE.md` pairs from prose into types,
which is where this codebase's bugs have historically come from.
