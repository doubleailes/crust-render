# materials — design record

> Design record for the **materials** capability: the reasoning, measurements and
> history behind the behaviour `spec.md` states. Moved out of `CLAUDE.md`, which
> now keeps only the rules and pointers. Section and path references such as
> "above" or "see X" may point to another capability's `design.md` —
> `openspec/specs/*/design.md` is the whole record; `docs/architecture.md` is the map.

## Crate: crust-mtlx

- **`crust-mtlx`** (lib name `crust_mtlx`) — the MaterialX `.mtlx` reader,
  factored out the same way as `crust-rt`: a standalone library with **no crust
  dependency** (roxmltree + glam only) behind a seam crust-core consumes.
  `parse` (XML → name-addressed graph), `value` (the one runtime value), `eval`
  (the graph compiled to a slot-indexed `Program`), `bsdf` (the closure tree —
  `layer` / `mix` / `add` / `multiply` over leaf BSDFs in MaterialX vocabulary, and
  `edf` to `Emission` terms), `surface` (the three surface-shader nodes expanded
  into their nodegraphs' trees), and `compile()` running them for one
  material node. It names the one thing it asks of its host — `Texture`, a
  `(u, v) → RGBA` sampler — and crust-core re-exports that trait as its own
  `Texture2D`, exactly as it adopts `crust_rt::Geometry`. How a renderer
  evaluates a leaf is not decided here; crust's is in crust-core's
  `material/closure/`.

## Crate: crust-jit

- **`crust-jit`** (lib name `crust_jit`) — a Cranelift JIT for `crust-mtlx`
  programs (step 5 of `docs/shading_performance.md`), behind crust-core's `jit`
  feature (on by default in `crust-render`; `--no-default-features` builds
  without it) and `CRUST_SHADER_JIT=0` at run time. **Bit-identical to the
  interpreter by construction**: only exact IEEE ops (`+ − × ÷`, ordered
  compares and `select`, `fabs`, lane shuffles) are emitted inline, mirroring
  `Val::zip`'s broadcast with widths known at compile time; textures are called
  directly with coordinates computed in the interpreter's operand order; every
  other op (`ln`/`exp`/`pow`/trig, `min`/`max` — Rust's `minnum` is not
  Cranelift's `fmin` — `normalize`, `normalmap`, dot products, and any op whose
  operand width is only known at run time) calls back into the interpreter's
  own step, `Program::apply_op`. `tests/jit.rs` compares every slot bitwise.
  **Not `forbid(unsafe_code)`** but `deny`, as `crust-core` is too (for its
  one test-only `GlobalAlloc`); it is the only crate with `unsafe` in
  production code, in four audited blocks (the code-pointer transmute, the two host callbacks' raw
  pointers, and freeing the code in `Drop`). A `JitProgram` owns its
  `JITModule` (in a `Mutex`, since the module is `Send` but not `Sync`) and
  frees it on drop — cranelift-jit would otherwise leak the code, which a host
  reloading scenes would accumulate. A malformed program (an operand not
  strictly before its user) is refused, never compiled: the generated code
  has no bounds checks, and `Program::optimize` returns such a program
  unchanged.

## The Material trait

- **`Material`** (`material/material.rs`) —
  `scatter_importance(r_in, rec) -> Option<ScatterSample>` used by the integrator
  (`ScatterSample.delta` marks singular lobes like transmission: never mixed with a
  continuous density, no tracer cosine, emission carried at full weight),
  `eval(r_in, rec, wi) -> Option<(value, pdf)>` (evaluate the *continuous* component
  toward a given direction — what NEE and guided MIS need; `None` = no continuous
  component at all, and per its contract that decision must never depend on `wi`),
  `emitted()`, and `emitted_at(r_in, rec, cos_theta_o)` — the hit-aware emission the
  integrator calls at a surface hit, defaulting to `emitted_directional` so only a
  material whose emission is textured need override it. Keeping `emitted()` and
  `emitted_directional()` hit-free is deliberate: the **light list** reads the first,
  and the coat's angular emission factor stays unit-testable against a bare cosine.
  A material that emits only through `emitted_at` must therefore never become a
  light-list entry, or NEE would sample it at zero radiance while the bounce side saw
  the real value.
  **`resolve(r_in, rec, cos_theta_o) -> Option<Resolution>`** is the shade-once hook:
  a material with per-hit work (a MaterialX graph, `UsdUVTexture`s, a Ptex lookup)
  returns the fully resolved `OpenPBR`, the record carrying its shading normal, and
  the hit's emission, and the integrator builds one **`ShadingPoint`** per vertex
  from it, through which the emission, the scatter, NEE's `eval`, guiding's `eval`
  and `make_ray` all go. `None` (the default, and an untextured `OpenPBR`) queries
  the material in place with no copy. The contract is that each answer is exactly
  what the material's own method would give — output is bit-identical to per-query
  shading. One trap: `Resolution::emitted` must come from the parameters *before*
  `into_resolved`, as `emitted_at` does, because the coat's emission factor reads
  `base_color` and a resolved Ptex lookup would change it. That order is now the
  only one that compiles: `into_resolved` consumes the `OpenPBR` and returns a
  `ResolvedOpenPBR`, which has the BSDF queries and no emission method, and a
  `Resolution` is built only by `Resolution::new`, which reads the emission and
  then resolves. `tests/resolve.rs` pins the contract bit for bit for every kind
  of material (plain, Ptex, glass, emissive, textured preview surface, MaterialX).
  `PreviewSurface` gets its `Material` impl through `PatternMaterial`
  (`material/pattern.rs`), which states the order once; `MtlxMaterial` implements
  `resolve` directly and builds its resolution with `Resolution::closure`, which
  takes the emission first in the same way. Four implementations: **`OpenPBR`**,
  the single übershader for all surfaces (with `diffuse`/`metal`/`glass`/`glossy` preset
  constructors used by `world.rs` and the USD fallback), **`Emissive`**, a pure
  emitter with no geometry knowledge, and **`MtlxMaterial`**
  (`material/materialx.rs`), which evaluates a MaterialX graph per shading point and
  collapses its closure tree into a `ResolvedClosure` (§ MaterialX), and **`PreviewSurface`**
  (`material/preview_surface.rs`), the same delegation for a `UsdPreviewSurface`
  whose inputs are driven by `UsdUVTexture`s. Two hooks gate the
  per-triangle side tables the importer would otherwise build for every mesh:
  `face_texture()` for Ptex and `uses_uv()` for the `primvars:st` chart. Shared shading helpers (aniso GGX VNDF sampling,
  Schlick/F82 Fresnel, EON diffuse, Charlie sheen, thin-film, Cauchy dispersion) live in
  `material/brdf.rs`. The OpenPBR formulas are aligned against the MaterialX nodegraph
  and Adobe's `openpbr-bsdf` reference — the item-by-item alignment record (with the
  remaining gaps, e.g. no LUT-based multiple-scattering compensation and no random-walk
  SSS entry) is `docs/openpbr_reference_alignment.md`.

## Material resolution on import

- Materials resolve via `MaterialBindingAPI::compute_bound_material("full")`
  (`bound_material`), which uses the `full` purpose falling back to all-purpose, with
  bindings inherited from ancestors, binding strength honoured, and collection bindings
  read. The walk starts at the nearest ancestor that *carries* the API, because
  `MaterialBindingAPI::get` refuses any other prim — which is every mesh under a bound
  group. `preview` bindings are never used. Prims under a `proxy` or `guide` purpose are
  pruned with their subtree, like `active = false`, and so is a `visibility =
  "invisible"` subtree (read at the evaluated time, lights included — ALab parks four
  lights that way), except that the top-level walk still descends it **for cameras
  only**: a hidden camera rig is still rendered through, and pruning it would break
  `--camera`. Then dispatch is on the bound shader's `info:id`:
  - `UsdPreviewSurface` → mapped into `OpenPBR` (portable; `diffuseColor→baseColor`,
    `metallic→baseMetalness`, `roughness→specularRoughness`, etc.). **`opacity` < 1 is
    refraction, not a cutout**: `transmission_weight = 1 − opacity` at `ior`, the
    spec's "index of refraction to be used for translucent objects" — how ALab authors
    all its glass (opacity ≈ 0, ior ≈ 1.49). `opacityThreshold > 0` is the spec's
    cutout mode and stays on native `OpenPBR`'s `geometry_opacity`, which nothing
    reads yet: the integrator's cutout (`Material::opacity`, § MaterialX) is
    MaterialX's alone so far. A textured `ior`
    below 1 keeps the constant: production `ior` maps are 0 in their UV gutters, so a
    filtered tap there would invert refraction along every seam. Constant inputs
    are still read **undecoded** (the openspec `add-material-color-management` change
    owns that). An input connected to a **`UsdUVTexture`** makes the material a
    `PreviewSurface` (`material/preview_surface.rs`), which samples the network per
    hit and writes each result over its `OpenPBR` field before delegating the BSDF, as
    `MtlxMaterial` does. With no texture connection the surface stays the plain
    `OpenPBR` it always was, with no chart and no per-hit cost, and every sample golden
    is unchanged. `preview_uv_input` resolves the whole node through
    `value_producing_attributes`, so interface-connected inputs work: `file` (layer-anchored,
    `<UDIM>` intact), `sourceColorSpace`, `wrapS`/`wrapT`, `scale`/`bias`, `fallback`,
    and which output (`r`/`g`/`b`/`a`/`rgb`) is connected. Four details:
    - **`sourceColorSpace` defaults to `auto`, not raw**, which is the opposite of
      MaterialX's default and why `ColorSpace::from_usd` is separate from
      `from_mtlx`. `ColorSpace::Auto` crosses the seam unresolved, and the host settles it
      against the file (`resolve_auto`, which returns a `ResolvedColorSpace` — the type every
      decoder, `.tx` marker and load report holds, and which has no `Auto`). By the UsdUVTexture rule, 8-bit RGB/RGBA is sRGB
      and anything else is raw. So a greyscale roughness PNG stays raw and an EXR is
      linear.
    - **A texture that does not load reads the node's `fallback`, unscaled** (the spec),
      and with none authored the *surface input's* constant, and with none of that the
      input's **`UsdPreviewSurface` schema default** (`preview_surface_default`:
      roughness 0.5, diffuse 0.18, ior 1.5, …) — not the node set's opaque black, which
      is not neutral. So `CRUST_TEX=0` still means "render on constants". ALab authors a deliberate green `fallback`, which
      therefore shows up only when a file genuinely fails.
    - **Wrap modes apply only to a single image.** A `<UDIM>` set's addressing is the
      host's, and wrapping would fold every tile onto the first. `repeat` and
      `useMetadata` are the identity (the host already wraps periodically).
    - **`normal` is decoded by the texture's own `scale = 2, bias = -1`**, which is the node
      set's convention and not a hard-coded `*2-1`, then rotated by
      `crust_mtlx::perturb_normal`, the half of MaterialX's `normalmap` split out for
      this. It is under the same no-flip guard.
    `UsdPrimvarReader_float2` naming a primvar other than `st` (or the `mesh_uvs`
    fallbacks) warns and shades from the one chart crust reads. `UsdTransform2d` warns,
    and the identity chart is used. Textured emission answers through `emitted_at` only,
    like MaterialX's.
  - `crust:openpbr` → decoded 1:1 into `OpenPBR`; every input is the camelCase mirror of the
    Rust field name (lossless but non-portable). Reference scene: `samples/openpbr_showcase.usda`.
  - `PxrDisneyBsdf` → mapped into `OpenPBR` (both descend from Burley's model). Checked
    **before** `compute_surface_source()`, by looking for a child shader with that
    `info:id` — a material with several render-context outputs resolves through that call
    to whichever one USD prefers, and on the Moana island (which authors
    `outputs:ri:surface`, `outputs:glslfx:surface` and `outputs:ri:displacement` on every
    material) that is the *preview* shader, whose inputs are all `.connect`ed to the
    material interface rather than authored as values. Decoding it yields every parameter
    at its default. Parameters are therefore read off the **Material** prim, where the
    island authors them. `sheen` is deliberately **not** mapped to `fuzz_weight`: Disney
    adds sheen at grazing angles, OpenPBR mixes fuzz *over* the layers beneath, so the
    island's `sheen = 1` erased all base colour (Ptex included) and rendered smooth
    plastic. `subsurface*`, `diffuseTransmission` and `specularTint` have no equivalent
    lobe and are dropped.
  - **`.mtlx` reference** → the MaterialX graph, read by crust itself
    (`crust-mtlx` + `material/materialx.rs`, below). Checked at each point the USD path gives up,
    not first: finding the reference means walking the prim's composition
    graph, which a stage of ordinary USD materials should not pay for.
  - Unbound geometry → grey diffuse `OpenPBR`.

## MaterialX

- **MaterialX** (`crates/crust-mtlx`, crust-core's `material/materialx.rs` and
  `material/closure/`) — `.mtlx` look-dev graphs, read directly.
  USD's own answer is a file-format plugin that composes a `.mtlx` into the
  stage as `UsdShade` prims; **openusd ships none**, so a `Material` prim whose
  only opinion is `references = @foo.mtlx@</MaterialX/Materials/name>` composes
  to a prim with a `Material` type name and *nothing inside it*. Every schema
  query then fails and the surface falls back to grey — which is what the two
  DPEL assets (MaterialX Teapot, MaterialX Lion) did on import. The reader is
  the standalone `crust-mtlx` crate — `parse.rs` (XML → a flat,
  name-addressable graph), `value.rs` (the one runtime value), `eval.rs` (the
  graph compiled to a slot-indexed program), `bsdf.rs` (the closure tree),
  `surface.rs` (the three surface-shader nodes expanded into closure trees) —
  and crust-core evaluates what it describes: `closure/` collapses the tree at
  a vertex and shades its leaves, `materialx.rs` is the `Material` and the
  importer-facing `load()`.
  - **Compiled once, not walked per hit.** A look-dev graph must be evaluated
    per shading point — its textures and masks are the point — but the teapot's
    ceramic graph is ~50 nodes run at every path vertex (once, since
    `Material::resolve` shares the result between the vertex's sample, NEE and
    guide queries). So the graph is compiled into a
    `Program`: a topologically ordered `Vec<Op>` whose operands are slot
    *indices*. Evaluation is a linear scan with no name hashing and no
    allocation (the value stack is a thread-local scratch buffer). ~30 node
    types are implemented; an unknown one degrades that one input to a constant
    and is reported once per material, never fails the material.
  - **A closure tree, evaluated with MaterialX's own semantics.** MaterialX
    assembles a look from standalone BSDF nodes glued with `layer`, `mix`, `add`
    and `multiply`, and `bsdf::flatten` keeps that tree as it is: an arena of
    `Closure::{Leaf, Layer, Mix, Add, Multiply, Empty}` whose parameters are
    program slots, so the graph's masks and textures drive them per point. The
    rules are MaterialX GLSL's (`mx_*_bsdf.glsl`), which NVIDIA's Typhoon
    follows too: `layer(top, base) = top + base · T_top(ωo)`;
    `mix(fg, bg, m) = m·fg + (1 − m)·bg` with `m` clamped; `multiply` scales
    the response (its weight clamped to [0, 1]) and passes the throughput
    through unchanged; `add` sums the responses and its throughput is
    `max(t₁ + t₂ − 1, 0)`. A leaf's
    throughput is `1 − E_R·w` for a dielectric (every scatter mode),
    `1 − avg(E)·w` for generalized Schlick and `1 − E·w` for sheen — and **0**
    for diffuse, conductor, subsurface and translucent *whatever their weight*.
    That last rule is the trap: a leaf at weight 0.3 still hides its base
    completely, and an early version that returned `1 − own albedo` let a
    conductor's base shine through it.
    This replaced a reduction that pooled the leaves by kind onto one `OpenPBR`.
    Every recorded MaterialX bug came from that projection — coat promotion by
    tree shape, roughness pooled in alpha, a `coat_darkening` override,
    independent-coverage bookkeeping between the metal and dielectric pools —
    and keeping the leaves apart retired the class rather than patching
    instances of it. It also makes a MaterialX glass refract, a coat keep its
    `tint`, and a third stacked dielectric keep its own roughness.
  - **Pruned at compile time, to MaterialX's empty BSDF.** A branch that can
    never respond is dropped at `flatten`, so it costs no leaf slot and no work
    per hit. That covers a leaf whose weight is a literal 0 (in the surface
    builders, one that folds to 0), a `multiply` by a literal 0, and the far
    side of a `mix` at a literal 0 or 1. What it leaves behind is MaterialX's
    `BSDF(0, 1)`: no response and a throughput of 1. That is the value
    MaterialX's GLSL starts every BSDF at, and the value it leaves a
    zero-weight dielectric, generalized Schlick or sheen at. A `mix` at a
    literal endpoint selects its other branch exactly, and `layer`, `add` and
    `multiply` simplify over an empty branch exactly (`layer(∅, b) = b`,
    `add(a, ∅) = a`). **A `mix` with a live factor does not**: its throughput
    `m·T_fg + (1 − m)·T_bg` still needs the pruned side's 1. So the mix stays a
    mix, with a `Closure::Empty` on the pruned side (`Closures::mix`, the one
    place `bsdf_tree` and the surface builders build a mix). That is the trap.
    Rewriting `mix(fg, ∅, m)` as `multiply(fg, m)` keeps the response but loses
    the throughput, because `multiply` passes `T_fg` through, and for an opaque
    `fg` that is 0. The DPEL assets are built on exactly this difference. Each
    dust and stain diffuse is mixed by its mask against a zero-weight, IOR-1
    `dielectric_bsdf` (their "transmission dummy"), which turns the mask into a
    *coverage*, and that coverage is layered over the glaze. With the rewrite,
    the diffuse's 0 stood for the whole top, and both assets rendered black
    except for their dust and the Lion's sheen: every glaze, conductor and body
    leaf at weight exactly 0, with no NaN and no warning. The probe showed it at
    the first point it was aimed at. `closure/tests.rs` now pins a pruned dummy
    against the same dummy at a *connected* weight of 0, which the compiler
    cannot prune, bit for bit.
  - **Collapsed at `resolve`, exactly.** `T_top` depends on ωo alone, and ωo
    is fixed at a path vertex, so there the tree *is* a weighted sum of leaves.
    `ResolvedClosure::resolve` walks it once per vertex: `mix` and `multiply`
    scale the weight passed down, `layer` scales everything below by the top's
    throughput, and each live leaf lands in an inline list of at most
    `MAX_LEAVES` = 8 with its RGB weight, its own frame (a leaf's `normal` and
    `tangent` inputs, so a coat can carry a normal map its base does not) and its
    parameters. `eval` is `Σ wᵢ fᵢ`; sampling picks a leaf with probability
    `∝ lum(wᵢ)·Êᵢ(ωo)` (floored at 0.02 so no live leaf gets probability 0, as
    Typhoon's `_ApproxWeight` does) and returns the mixture pdf, one-sample MIS
    as `OpenPBR` composes its own lobes. The worst built-in expansion is 7
    leaves (`standard_surface`); a document above 8 is **refused** at load
    with a `WARN` naming the count, never truncated — dropping a leaf would be a
    plausible, wrong surface. Leaves are counted from the root, so branches a
    surface node builds lazily and never connects do not count.
    **The leaves live behind a pooled box** (`PooledClosure`). Inline, 8 × 224
    bytes made `ShadingPoint` 1984 bytes for *every* material, and the path
    from `Material::resolve` into a `ShadingPoint` moved it whole about seven
    times per vertex: `memcpy` was 17% of `materialx_basic`'s instructions, and
    the change measured +20.3% against the pooled reduction. Boxed and
    recycled through a per-thread pool (bounded at 16 per thread), a vertex
    moves a pointer, resolves in place and allocates nothing once the pool is
    warm; `ShadingPoint` is back to 464 bytes and `materialx_basic` costs +2.2%
    instructions (callgrind, `-s 2`) over the reduction it replaced. The
    `Drop` is inlined with the pool work out of line — otherwise every
    `ShadingPoint`, MaterialX or not, paid an out-of-line drop call.
  - **Leaves.** Each is the MaterialX lobe, ported from MaterialX GLSL
    (Apache-2.0, `closure/mx.rs`): GGX with VNDF sampling and MaterialX's
    `mx_ggx_energy_compensation`; exact dielectric, complex-IOR conductor and
    F82 generalized-Schlick Fresnel, each optionally through the Airy thin film
    a `layer` of `thin_film_bsdf` puts on the base's specular leaves; EON (an
    `oren_nayar_diffuse_bsdf` with `energy_compensation`), plain Oren–Nayar and
    Burley diffuse; Imageworks sheen. A specular leaf's `roughness` is GGX
    **alpha**, taken as authored — the DPEL teapot's `desquare_roughness_*`
    nodes exist to produce it — and only the BSDL table below is indexed by
    perceptual roughness `√α`. Dielectric scatter modes: `R`; `RT`, which picks
    reflection with probability `F(v·h)` — the *same* film-aware Fresnel on the
    sampling and the pdf side, or the two disagree; and `T`, whose
    transmission **pays its own `(1 − F)`**. MaterialX GLSL
    (`mx_surface_transmission`), OSL, BSDL and Typhoon all do. Leaving it out,
    on the reading that the reflection layer above "owns" the Fresnel loss,
    made a glass's exit leaf transmit `1 − E_R` beside a reflection leaf already
    reflecting total internal reflection, and the furnace caught a
    `standard_surface` glass sphere returning 1.16× its environment at grazing
    incidence. With it, that glass reads 0.92 head-on: MaterialX's own double
    Fresnel, the reflection layer's `1 − E_R` times the transmission's `1 − F`,
    which Typhoon's `standard_surface` shares. A thin-walled surface transmits
    straight through; a thick one hands the refracted ray the interior medium.
  - **`subsurface_bsdf` is a random walk — Typhoon's.** The leaf
    (`Lobe::Subsurface`) has no value toward any direction, so NEE at the entry
    sees nothing of it, exactly as Typhoon's `EvalNode` returns 0 for it.
    Selecting it (with probability `∝ weight · luminance(color)`, Typhoon's
    `_ApproxWeight`) is a delta event carrying `weight / p` and an entry
    direction: a refraction through the interface above the leaf
    (`closure::subsurface_entry`, Typhoon's `SampleSubsurfaceEntry`) — the
    nearest *live* dielectric a `layer` puts over it, whose IOR and GGX alpha
    the collapse walk carries down (`interface_of`: a leaf at weight 0, a
    `mix` branch at factor 0 and a `multiply` by 0 are skipped as the collapse
    drops them, and a live `mix` asks its heavier branch first — a coat
    textured to 0 at the hit must not set the entry), else Typhoon's closure
    defaults, IOR 1.5 and roughness 0.5. The interface's energy is already in
    the leaf's weight (the layer's throughput); the entry only picks a
    direction. The tracer then runs the walk (`subsurface.rs`, a port of
    `ty::RandomWalkSSS`, itself Cycles' `subsurface_random_walk.h`): Chiang
    2016's albedo inversion with the 0.2 albedo floor and its throughput
    correction, per-bounce channel MIS, forward and backward Dwivedi guiding
    with the extended first ray finding the opposite interface, the similarity
    relation after 9 bounces, 256 bounces at most. It traces the owner's
    `geom_id` alone, stepping past every other surface in a segment however
    many there are (a search capped at 16 read nine embedded spheres as "no
    boundary"; `many_embedded_objects_do_not_hide_the_boundary`), and the path
    resumes at
    the exit on a white Lambertian with no emission (`ExitLambertian`,
    Typhoon's and Cycles' synthetic exit), weighted by the walk's throughput.
    The walk is one surface event: it spends no path depth, records no vertex
    of its own and runs no NEE inside. Only `trace_path` runs walks: the
    learned light cache's training paths stop at a subsurface sample, whose
    ray is an entry, not a bounce — followed as one it crossed the object and
    trained receivers on its far inside. A zero radius falls back to the diffuse
    in the leaf's colour that the leaf used to be. Verified in numbers: on a
    semi-infinite slab with a cosine-weighted entry the walk reflects its
    colour to within 0.02 for `g ≤ 0.6` and albedos up to 0.9, a chromatic
    radius included (`subsurface/tests.rs`); through the integrator, the white
    furnace bounds every fixture of `samples/materialx_subsurface.mtlx`
    (`tests/mtlx_surfaces.rs`). Three traps, each fallen into once:
    - **Scatter points are not offset.** Typhoon starts every segment at
      `tnear = 1e-4`. From a scatter point that lies within that distance
      under the surface the ray steps straight through the boundary, the walk
      carries on *outside* the object, and later re-enters through a front
      face; on a 0.1 mean free path this killed the slab test outright. Only
      the entry, which lies on the surface, is offset. A walk that still meets
      the boundary from its entry's side (a point rounded across the surface)
      is dropped as lost — the facing test is relative to the entry, so an
      inverted winding walks the same.
    - **Chiang's fit is for `g ≥ 0`.** At `g = −0.4` its `d` coefficient is
      −3.7, the remapped albedo is negative, and every walk dies. Typhoon clamps
      the anisotropy to ±0.99 and inherits that; the walk clamps it to
      `[0, 0.99]`, as Cycles does (`walk_anisotropy`).
    - **The walk's rays use the tracer's own interval.** Every other
      `World::intersect` asks for `(0.001, ∞)`, and LLVM propagates the two
      constants into the kernel. The walk asking for its own bounds lost that:
      cornellbox, which never walks, ran 0.3% more instructions in the
      triangle test. `trace_owner` moves the ray's origin instead. The rest of
      the integration is kept off the per-vertex path the same way — the walk
      and the exit hit are `#[cold]`, out of line, the pending exit lives in
      `PathScratch` behind a flag, and `ScatterSample` carries a one-byte leaf
      index in its padding rather than the walk's parameters; inlined, or as
      an `Option<WalkExit>`, each of those cost cornellbox about 1%. What is
      left is +0.49% instructions on cornellbox (callgrind, 2 spp), and every
      sample scene without a subsurface leaf renders bit-identically.
  - **Throughput tables are ported, not regenerated.** The dielectric throughput
    is BSDL's `DielectricReflFront` filter `1 − E_R(cosθo)`
    (`closure/bsdl_tables.rs`, 32 IOR × 16 roughness × 16 cosines, BSD-3-Clause),
    regenerated from BSDL's `genluts` by `scripts/tables/bsdl_luts_to_rust.py`
    and bit-identical to the table Typhoon ships
    (`ty:dielectricLayerThroughputMode = "bsdl"`, its default); a film on the
    dielectric uses MaterialX's Fresnel-weighted fit instead, as Typhoon does.
    Generalized Schlick and sheen use MaterialX's analytic fits
    (`mx_ggx_dir_albedo`, `mx_imageworks_sheen_dir_albedo`). The tables describe
    BSDL's and MaterialX's lobes, not crust's, so the gap is measured and pinned
    (`closure/tests.rs`, 4096 samples per point): the BSDL table is within
    ±0.004 of crust's integrated dielectric leaf everywhere except near-smooth
    grazing, worst +0.020 (IOR 2, r 0.05, cos 0.1), where its linear
    interpolation in cosine undershoots a steep curve; the Schlick fit is within
    0.011; the sheen fit overestimates by up to 0.035 at low roughness and
    grazing — a sheen layer there loses energy, never creates it. The white
    furnace over every fixture material (`tests/mtlx_surfaces.rs`) bounds the
    sum: nothing returns more than its environment.
  - **Surface shaders are their nodegraphs, node for node.** `open_pbr_surface`
    (1.1), `standard_surface` (1.0.1) and `gltf_pbr` (2.0.1) are MaterialX
    nodedefs implemented as nodegraphs over standalone BSDFs, and a document
    never carries the implementation. `surface.rs` reproduces each graph — the
    same leaves, the same `layer` / `mix` / `multiply` in the same order, the same
    derived parameters emitted as program ops so they fold, optimise and JIT —
    commented with the nodegraph node names so it can be checked against the
    `.mtlx` line by line; Typhoon builds its surfaces the same way. **Where
    Typhoon and the graph disagree, the graph wins** (Typhoon omits OpenPBR's
    thin-walled subsurface branch; this does not). Inputs resolve connection →
    authored value → **nodedef default**, from tables vendored with their
    nodedefs under `crust-mtlx/tests/nodedefs/` and checked against them by
    `tests/nodedefs.rs` — a slip there shades every document that leaves the
    input unauthored, plausibly. `standard_surface` 1.0.1 *inherits* 1.0.0 and
    overrides `base` = 1 and `base_color` = 0.8, which is why both nodedefs are
    vendored. A non-default `version` is warned about and built as the default.
    The graphs need `ifgreater`, which the program has no op for; it is built as
    `mix(in2, in1, max(sign(v2 − v1), 0))` from existing ops, so the JIT needed
    nothing new (`sign(+0) = 1` in Rust, so equality takes `in2`). The
    transmission medium is the graph's own: OpenPBR's `transmission_depth` /
    `transmission_scatter` volume and glTF's attenuation (`σ = −ln(color) /
    distance`) become the interior `Medium`; `standard_surface`'s graph has
    none, so its `transmission_depth` is reported rather than invented.
  - **Opacity is presence, and the integrator's.** Every surface graph ends in
    MaterialX's `surface` node, whose `opacity` blends the whole surface —
    BSDF and EDF — with nothing, and that is a statement about the geometry,
    not a lobe: a cutout leaf is not there at all, so it neither refracts nor
    changes the medium a ray is in. It is therefore carried beside the tree
    (`Closures::opacity`) and applied where hits are found. OpenPBR's is
    `geometry_opacity`; `standard_surface`'s is the graph's own
    `luminance(opacity)` (ACEScg weights, the node's default — Typhoon averages
    the channels; the graph wins); glTF's is `alpha` through `alpha_mode`,
    OPAQUE 1, MASK `alpha ≥ alpha_cutoff`, BLEND `alpha`. `alpha_mode` is a
    uniform, so the builder selects on its folded value rather than building
    the graph's two `ifequal`s: exporters wire `alpha` to the base texture's
    alpha whatever the mode, and a select through a texture never folds, which
    would have made every such OPAQUE material a cutout — correct, and every
    shadow ray in its scene several times dearer. Only a mode that does not
    fold builds the chain (`ifequal` as "neither is greater", from `gt`). An
    opacity that folds to 1 is no cutout: `Material::has_cutout` is false and
    the integrator never asks. One that does not is evaluated by its own
    slice of the program — `Program::optimize` to the one root, JIT-compiled
    like the whole — because it is asked before a hit is shaded, at every
    crossing, and running the full graph for one float would shade every
    skipped leaf card in full. The integrator's side (stochastic pass-through
    on the bounce side, `Π(1 − opacity)` on shadow rays) is in the rendering
    record.
  - **Anisotropy rotation turns the leaf's frame.** `standard_surface`'s
    `main_tangent` / `coat_tangent` and glTF's `selected_tangent` are a
    `rotate3d` of the *authored* tangent about the leaf's own normal input,
    which the BSDF then projects onto that normal's plane — the same as
    turning the projected tangent. The host's `Tworld` has no program slot to
    rotate, so the angle rides on the leaf (`Leaf::rotation`, radians,
    right-handed about the leaf normal) and `closure::prepare` turns the
    frame after building it: sampling and evaluation share that frame, so
    they cannot disagree. The sign is the trap: `mx_rotate_vector3` is
    `v·cos θ + (v × axis)·sin θ + …`, Rodrigues' formula at **−θ**, and
    Typhoon's `Rotate3d` is the same. So `standard_surface`'s
    `rotation · 360°` turns the tangent *clockwise* seen from above the
    surface, and glTF's `−rotation · 57.29578°` turns it counter-clockwise by
    `rotation` radians, which is what glTF's own spec says. Guards follow the
    graphs: `standard_surface` rotates only where its anisotropy is `> 0`
    (`ifgreater`), glTF everywhere (a zero angle turns nothing, bit for bit);
    glTF's clearcoat keeps the unrotated tangent, as its graph wires it;
    OpenPBR has no rotation input.
  - **Reported, not dropped.** What the tree cannot represent is known at
    compile time — an input *authored away from its default* (connected, or a
    differing value) and a closure that stays live after optimisation — so the
    importer prints **one `WARN` per material**, beside the unsupported-node
    warning: glTF `occlusion`, the inputs MaterialX's own graphs ignore (glTF `dispersion` and
    `thickness`, `standard_surface`'s `transmission_depth` / `scatter` /
    `dispersion`, OpenPBR's `transmission_dispersion_scale`) and a live Zeltner
    sheen (evaluated as Imageworks). Default-valued inputs stay silent: the suite authors
    `alpha_mode` and `geometry_opacity` at their defaults in dozens of
    documents.
  - **The EDF half is a second list, not a leaf.** MaterialX's `<surface>` has an
    `edf` input beside its `bsdf`, and the flatten walks both — the same
    `layer`/`mix`/`multiply`/`add` algebra, since a mix partitions radiance
    exactly as it partitions reflectance. The walk carries a **closure domain**,
    because `closure_input` gates a branch on its declared type and an EDF-typed
    `mix` declares `type="EDF"`: asking "is this a BSDF?" in the emission tree
    resolves both branches to `None` and the emission vanishes silently.
    Emission terms **sum** (two emitters are twice the light), and neither the
    weight nor the colour is clamped above: `multiply(uniform_edf, 100)` is how
    a document authors a bright emitter, and this is the one shading input for
    which a value above 1.0 is meaningful. **The weight is not a scalar**:
    MaterialX declares `ND_multiply_edfC`, so a tinted emitter is ordinary
    authoring, and taking lane 0 alone turned a weight of `(0, 0.6, 0.9)` into a
    **black** emitter. Both factors are read with `Val::rgb` and sanitised per
    channel *before* the product — clamping the product would let two negative
    channels multiply into positive light. `generalized_schlick_edf` is carried
    as a falloff on its base's term, `mix(color0, color90, (1 − cosθo)^exponent)`
    — it is how `open_pbr_surface` darkens its emission through the coat. A
    `surface` with an `edf` and no `bsdf` has no leaves: a pure emitter does not
    also reflect.
  - **The shipped `.mtlx` files are not well-formed XML.** They address a UDIM
    set as `value="Albedo.<UDIM>.png"` — a bare `<` inside an attribute value,
    which XML forbids. MaterialX's own reader is PugiXML, which accepts it;
    `roxmltree` rejects the whole document with an `InvalidChar`. `parse.rs`
    escapes the two specified tokens first, so this is not "the UDIM path is
    wrong" but "no material at all" if it is ever removed.
  - **Verified in numbers, not by eye** — `examples/mtlx_shade` prints, at a
    named `(u, v)` and view angle (`--theta`), every live leaf the tree
    collapses to: its MaterialX category, RGB weight, parameters and normal,
    plus the emission and the interior medium. Every spec scenario that says
    "the probe" is a test over the same function (`tests/mtlx_surfaces.rs`).
    This is what settled the teapot: the render looked washed out against the
    reference, and the probe showed the graph producing exactly the right deep
    blue on the body against light ribs — so the fault was the sample scene's
    exposure, not the material. The reduction printed that blue as one pooled
    base colour, (0.0048, 0.0234, 0.0734) at (0.962, 0.312), matching the
    (0.005, 0.024, 0.074) first recorded here. The tree shows its parts: the
    body diffuse at (0.0026, 0.0213, 0.0717), weight 0.956 under the glaze,
    beside a stain diffuse at 0.006, which pool back to the same colour. It
    reaches its textures through `FileAssets` rather than `UvTexture`
    directly, so `CRUST_TEX_STREAM` is honoured. That last part is not tidying: the
    preloaded decoder narrows to 8 bits, so an HDR emission texture probed
    through it reads 1.0 whatever the file holds, and a probe that cannot see
    the range is worse than no probe because it answers confidently.
  - Sample scenes: `samples/materialx_basic.usda` + `.mtlx` (self-contained, 20
    KiB of textures, what `tests/usd_scene.rs` runs against; its `mtlx_lacquer`
    is a two-dielectric stack whose interfaces stay separate leaves — α 0.02 at
    weight 1 over α 0.4 at 0.96 over the diffuse at 0.929),
    `samples/materialx_emissive.usda` + `.mtlx` (the EDF fixture: a constant
    `multiply(uniform_edf, 12)` and a pure emitter driven by
    `textures/mtlx_emission.hdr`, whose bright cells sit at (16, 9, 3) —
    deliberately a **separate** document, because three assertions pin
    `materialx_basic` at exactly three materials and three textures),
    `samples/materialx_surfaces.usda` + `.mtlx` (one texture-free material per
    surface node plus a `standard_surface` glass, a `gltf_pbr` with attenuation
    and an `open_pbr_surface` with a coat normal: what the surface-node tests
    and the furnace run against), and the two shot layers for the DPEL assets,
    which are gitignored and must be downloaded:
    `samples/materialx_teapot.usda` and `samples/materialx_lion.usda`, plus
    `samples/materialx_showcase.usda` composing both after the `overview.png`
    the assets ship with (the lion is scaled to 0.52 there: both are ~0.26 m
    tall as authored, yet the overview shows the lion at ~60% of the teapot's
    height while standing nearer the camera, so it is scaled, not pushed back;
    the seamless sweep is a near-white floor under a uniform dome, the two
    meeting at the horizon because a Lambertian floor of albedo a under
    radiance L reflects a·L). The lion
    is the larger graph (140 ops, 7 textures over 6 UDIM tiles, 1.06 M
    baked triangles) and the one that layers a `sheen_bsdf`; both import with no
    unsupported nodes. They are also the only documents here that build
    coverages from zero-weight dummies (the pruning trap above). CI cannot see
    them, so `tests/mtlx_surfaces.rs` carries the Teapot's four-layer stack in
    miniature, in white, and they still need probing by hand after any change
    to the tree.

## Known gaps: MaterialX

- **Approximated leaves.** A Zeltner sheen (`mode = zeltner`, OpenPBR's fuzz)
  is evaluated as Imageworks / Charlie, and reported per material when live.
- **The random walk reflects less than its colour, as Typhoon's does.**
  Chiang's inversion is fitted for a *diffuse* entry; entered by refraction, a
  walk heads deeper and more of it is absorbed. Through the integrator a colour
  of (0.8, 0.5, 0.2) on a near-slab reflects (0.78, 0.45, 0.16) — pinned by
  `a_short_walk_reflects_its_colour_in_the_furnace` and, for the walk alone,
  `a_normal_entry_reflects_less_than_the_fit`. The refracted entry is
  Typhoon's (and Cycles' when it has an IOR); a cosine-weighted entry, Cycles'
  classic random walk, would reflect the colour to 0.02 and is a one-function
  change in `closure::subsurface_entry` if matching the authored albedo ever
  matters more than matching the reference. The fit also over-reflects where
  it passes 1 and is clamped: `g ≥ 0.8`, and colours near white (0.95 at
  `g = 0` reflects 0.93, 0.6 at `g = 0.9` reflects 0.70) —
  `high_anisotropy_over_reflects_boundedly`. Negative anisotropy walks as
  isotropic (the fit has no `g < 0` branch).
- **The walk scores lower on the Material Fidelity suite than the model it
  replaced.** Every subsurface material there fell 2.5–6 dB against
  `materialx-glsl` (`ketchup` 26.8 → 20.7) and 1.6–7.5 dB against `materialx-osl`.
  No reference in the suite is known to trace a random walk, and the old model
  scored the same whatever the radius, so this does not show the walk is wrong.
  Settling it needs a random-walk reference (Typhoon, or Cycles' random walk);
  `docs/material_fidelity.md` § Subsurface has the table.
- **A walk sees its own `geom_id` only.** A mesh split into several geometries
  (one per material subset) is several media: a walk entering one does not
  exit through another, and meets it as nothing. Nested objects inside the
  medium (eyes in a head) are stepped past, not scattered off, as in Typhoon's
  owner-scene trace. A walk that leaves no exit in 256 bounces — an open mesh,
  a sheet, a sliver — is absorbed, and so is one that rounds across its
  boundary (`--stats` reports the share that exited).
- **No BSSRDF importance, no NEE inside.** Walks are brute-force: the exit's
  Lambertian is the first place light is sampled, so a thin backlit feature
  (ears, leaves) converges only as fast as its walks reach the far side. The
  native `crust:openpbr` / `UsdPreviewSurface` subsurface still renders as the
  tinted diffuse (`docs/openpbr_reference_alignment.md`): the walk is wired to
  the MaterialX leaf only.
- **Not applied.** glTF `occlusion` is reported, not implemented: a path
  tracer computes its own. Dispersion is ignored where MaterialX's own graphs
  ignore it, and reported.
- **Cutout limits.** The shadow side walks every blocked shadow ray hit by hit
  once a world holds any cutout, since the kernel's any-hit query has no
  filter to skip one (an Embree-style any-hit callback is the fix, in
  `crust-rt`); an unblocked ray keeps the fast answer. The opacity is asked
  with the shadow ray's direction there and the path's on the bounce side, so
  a graph whose opacity reads `viewdirection` sees opposite views on the two
  sides. The learned light cache's training paths ignore cutouts (a guide
  only, never bias). A walk's exit and the walk itself ignore the entered
  object's own opacity. A segment follows at most 256 cutouts.
- **The throughput tables are not integrals of crust's leaves.** Measured above
  (worst +0.020 dielectric, 0.035 sheen); the follow-up, if it ever matters, is
  regenerating them from crust's leaves on the same axes. The dielectric table is
  BSDL's *front-side* reflection filter and is used for a back-facing hit too, as
  Typhoon does; from inside a thick glass the true reflection albedo includes
  total internal reflection, so the transmission leaf below is weighted a little
  high there. The `T` leaf's own `(1 − F)` keeps the sum bounded (furnace), but the
  exit weighting is not BSDF-exact.
- **Pruning is GLSL-exact only for leaves that let light through.** A diffuse,
  conductor, subsurface or translucent leaf at a literal weight of 0 is pruned
  to the empty BSDF like any other. MaterialX's GLSL, though, writes such a
  leaf's throughput of 0 before it checks the weight, so there it still hides
  its base, as it does in crust at a *connected* weight of 0, which is not
  pruned. A literal `multiply` by 0 is pruned the same way, where GLSL passes
  its input's throughput through. The difference shows only under a layer's
  top, and neither the built-in surfaces (whose layer tops are dielectric and
  sheen leaves) nor the DPEL assets put either one there.
- **Layering is MaterialX's, not physical.** `T_top(ωo)` only (non-reciprocal,
  single-scattering): NEE and bounce evaluate the same `f(ωo, ωi)`, so MIS is
  consistent, but a bidirectional method would need care. A reflection layer over
  a `T` dielectric pays Fresnel twice, so a `standard_surface` glass is ~8% dark
  in a white furnace — as it is in MaterialX GLSL and Typhoon.
- **`crust:openpbr` and MaterialX `open_pbr_surface` disagree.** crust's native
  `OpenPBR` (also what `UsdPreviewSurface` maps to) is its own übershader: it
  differs from the MaterialX graph in layering, `specular_weight`, coat–base
  coupling and coat tint, and converts transmission depth to a medium by the
  van de Hulst inversion rather than the graph's `−ln(color)/depth`. A thin-walled
  dielectric differs too: the closure leaf transmits one interface's `(1 − F)`,
  as MaterialX's GLSL does (`mx_surface_transmission`; `$refractionTwoSided`
  squares the tint, nothing more), where native OpenPBR uses the Adobe
  reference's window model, `2R/(1+R)` reflected and `(1−R)/(1+R)` through,
  with the sheet's internal bounces. Moving the leaf to the window model would
  take the MaterialX path away from its reference, and in a surface graph the
  reflection is a separate leaf layered above, so the leaf would count it twice.
  The same parameters authored both ways shade differently.
- **Capacity.** A tree with more than 8 live-reachable leaves is refused, whole.
- **Pattern nodes.** Only document-scope and `<nodegraph>` nodes are read;
  `<nodedef>` custom node *implementations* are not, so a graph instantiating one
  gets that input at a constant (reported). The pattern operators the Material
  Fidelity suite still lacks are listed in `docs/material_fidelity.md`. No
  `<look>` / `<materialassign>`: bindings come from USD. Two parser quirks
  predate the tree: `sign(0)` is 1 where GLSL's is 0, and two nodes with the same
  name in one scope collide.
- **Stdlib nodegraphs lowered by hand.** `colorcorrect` (`color3`) is compiled to
  `NG_colorcorrect_color3`'s own chain (`hsvadjust` → `saturate` → `range` gamma →
  lift → gain → `contrast` → exposure) out of existing ops plus `HsvAdjust`, with a
  stage whose parameter folds to its identity left out. The stage order is pinned
  by a test, since swapping any two still renders plausibly. `colorcorrect` on
  `color4` is refused and reported, because there is no `combine4` to route alpha
  around the chain.
- **`heighttonormal` differentiates over the ray footprint.** The OSL reference is
  `normalize(-Dx(h), Dy(h), scale·√…)`, which uses screen-space derivatives. There
  is no `Dx` here, so the compiler builds the `in` subgraph four more times with
  every `Texture` / `TexCoord` op under it displaced by ±½ `uv_width` along `u` and
  `v` (`Op::*::shift`, part of the memo key; the four copies share one sampler).
  Two consequences are inherited from the reference, not bugs: the bump is
  resolution-dependent (a coarser footprint reads steeper, so the effect also
  strengthens at later bounces as ray cones widen), and with no footprint
  (`CRUST_RAY_CONES=0`, or geometry with no UV density) it reads **flat**. Only
  UV lookups move: a height built from `position` or `normal` differentiates to
  zero, and so does an image at a constant `texcoord`.
- **An image's `texcoord` connection is honoured** (`Op::Texture::coord`).
  The default chart spelled out (`texcoord` index 0, `geompropvalue st`) stays
  `None`, which keeps the lookup on the JIT's inline path. A coordinate
  subgraph that hits an unsupported node (`place2d`, a second UV set) also
  falls back to the chart, because the constant that node would compile to
  pins the lookup to a single texel. The JIT sends shifted lookups back to the interpreter, which keeps its
  inline texture path, and so the bit-identity pin, untouched.
- **Emission.** `uniform_edf` is exact and `generalized_schlick_edf` is carried as
  its closed-form falloff; `conical_edf` and `measured_edf` are directional
  distributions with nowhere to go and are refused and reported. An **emissive
  MaterialX surface is not a light-list entry**: `AreaLight` pairs a `LightShape`
  with an `Arc<Emissive>`, whose radiance is a constant, and a graph's emission is
  a function of the shading point. So such a surface is found by BSDF/bounce
  sampling only, at full weight — as emissive curves, instances and volumes
  already are — which means no NEE and a firefly risk near a small bright emitter.
  That is also why `MtlxMaterial` leaves `emitted()` at zero and answers through
  `emitted_at`: the light list reads the hit-free one, and the two must agree for
  anything it samples.
