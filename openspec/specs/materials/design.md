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
  production code, in five audited blocks (the code-pointer transmute, the call into the generated code, the two host callbacks' raw
  pointers, and freeing the code in `Drop`). A `JitProgram` owns its
  `JITModule` (in a `Mutex`, since the module is `Send` but not `Sync`) and
  frees it on drop — cranelift-jit would otherwise leak the code, which a host
  reloading scenes would accumulate. A malformed program (an operand not
  strictly before its user) is refused, never compiled: the generated code
  has no bounds checks, and `Program::optimize` returns such a program
  unchanged.

## The Material trait

- **`Material`** (`material/material.rs`) —
  `scatter_importance(r_in, rec, draw: BsdfDraw) -> Option<ScatterSample>` used by the integrator
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
  **`uniform_emission() -> Option<Vec3A>`** is the stricter hit-free question: `Some(c)`
  only when `emitted_at` returns exactly `c`, bit for bit, at every hit and every
  angle. `OpenPBR` answers its uncoated emission when no coat or fuzz fades it
  (`emitted_directional`'s own early return); a preview surface its constants' under
  the same condition, unless a texture drives its emission or its clearcoat; an
  `Emissive` its colour when unshaped, untextured and two-sided; `MtlxMaterial` its
  folded constant (below). Everything else answers `None`. It is not `emitted()`,
  which a coated `OpenPBR` answers with its uncoated value while `emitted_at` fades
  it. A `MeshLightAPI` mesh under `materialGlowTintsLight` is lit by it (lighting
  design record, "Mesh lights"); `crust-core/tests/resolve.rs` runs every material
  that answers `Some` through `emitted_at` and its resolution at random hits and
  angles, with and without the `jit` feature.
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
  - **The scatter contract: numbers in, one 4D block.** A material samples from at most
    four uniforms in `[0, 1)` that the integrator hands it, never from a sampler: the
    same material, hit, ray and numbers give the same `ScatterSample` whichever QMC
    sampler produced them (`openpbr/tests.rs::exact_uniforms_select_a_lobe` pins a coat
    picked by hand-written uniforms, bit for bit across calls). Every implementation
    draws exactly one block — `s[0]` picks the lobe or leaf, `s[1..3]` the direction,
    `s[3]` a lobe's own discrete choice (dispersion channel, reflect-or-refract) — and
    none derives a sub-domain, which is what let the sampler type leave the material
    layer without changing a pixel: openqmc computes a whole 4D block per draw and
    `draw_sample::<N>` returns its prefix, so a `<2>` draw moved out of a material and
    into its caller is the first two values of the same block. The draw is **lazy** at
    every point a material may not sample: the concrete entry points
    (`OpenPBR::scatter_with`, `ResolvedClosure::scatter_choosing`,
    `ResolvedClosure::straight_transmittance` and their `scatter_split` twins) take
    `impl FnOnce() -> [f32; 4]` (`[f32; 2]` for the straight transmittance) and call it
    only past the back-facing return, the empty-tree return and the no-thin-leaf case —
    an unnecessary block per camera ray measured 4.2% of cornellbox (the `rendering`
    record). The closure inlines away, so the code on these paths is what it was. The
    `dyn Material` arm, reached in a render only by `Emissive` (draws nothing) and the
    subsurface `ExitLambertian` (reads the block's first two), takes the object-safe
    **`BsdfDraw<'_>`**: `Lazy(&dyn Fn() -> [f32; 4])` from the integrator, `Fixed([f32; 4])`
    from a test, with `u()` drawing on demand; `ShadingPoint::scatter_importance` builds the
    `Lazy` only on that arm, so the branch in `u()` is never on the hot path. A material
    that one day needs unbounded numbers gets a `fn rng(&self)` on `BsdfDraw` (a
    `pcg::Rng` seeded from the same domain): not a breaking change, and free until it
    exists. Trap: a test helper that hands out "the next sampler" must now hand out the
    next *block* and advance eagerly, as `S::next()` does in the openpbr and closure
    tests — a lazily advanced index would skip a step at every early return.
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
  Schlick/F82 Fresnel, EON diffuse, Zeltner's LTC sheen, thin-film, Cauchy dispersion)
  live in `material/brdf.rs`; the sheen's table is `material/ltc_sheen_table.rs`. The
  OpenPBR formulas are aligned against the MaterialX nodegraph and Adobe's `openpbr-bsdf`
  reference — the item-by-item alignment record (with the remaining gaps, e.g. no
  LUT-based multiple-scattering compensation and no random-walk SSS entry) is
  `docs/openpbr_reference_alignment.md`, and `crust-core/tests/adobe_oracle.rs` measures
  it: reference values from Adobe's library at a pinned commit, with each remaining gap a
  named deviation and a bound, and a deviation every case passes without failing as stale.
  - **The fuzz is Adobe's.** Zeltner, Burley and Chiang's LTC sheen with Disney's
    "Volume" table, over everything beneath it, which it attenuates by
    `1 − fuzz_weight · R(ω_o)` on the view side only (Adobe's default layering), emission
    included; Adobe's fuzz-to-coat roughening too. The oracle matches every fuzz-only case
    within 1e-4. It replaced a Charlie sheen under a flat `1 − fuzz_weight`, and the look
    is not close: head-on, a white fuzz of roughness 0.3 now reflects 0.0008 (Imageworks'
    fit gave 0.052) and one of roughness 1 reflects 0.342 (0.157); at cos θ_o = 0.25,
    roughness 0.3 reflects 0.166 (0.431). The full table is in the alignment record.
  - **No roughness floor.** BSDL clamps its Zeltner roughness at 0.02 because its LTC
    sampling "gains energy" there, and MaterialX at 0.01. That gain comes from weighing
    samples with a separately tabulated albedo; here the sample weight and the value share
    the table's `R`, and a furnace sweep down to roughness 0 never exceeds `R`
    (`brdf::zeltner_tests::a_sheen_never_gains_energy`).
  - **The trap: a fuzz with no weight still owns a lobe.** `LobePmf::selecting` floors
    every lobe's selection weight above zero, so a material with `fuzz_weight = 0` still
    picks its fuzz one sample in a million. That lobe keeps drawing cosine-weighted, with
    the cosine density in `pdf_all`: any density is correct for a lobe with no energy, and
    switching it to the LTC would have changed every image, fuzz or not.
  - **Kept on back faces, unlike Adobe.** Adobe gives the fuzz no presence when a closed
    surface is hit from inside. crust keeps it, as it keeps the coat and emission there,
    because cloth is mostly open meshes not authored thin-walled; the oracle's `interior`
    deviation records the difference.

## Material resolution on import

- Materials resolve via `MaterialBindingAPI::compute_bound_material("full")`
  (`bound_material`), which uses the `full` purpose falling back to all-purpose, with
  bindings inherited from ancestors, binding strength honoured, and collection bindings
  read. The walk starts at the nearest ancestor that *carries* the API, because
  `MaterialBindingAPI::get` refuses any other prim — which is every mesh under a bound
  group. openusd-schemas' `bound_material` answers with the
  `Material` prim (since upstream `c3af19e`), so `None` when the winning target is not
  one *on this stage* — every material outside the chunk on a streamed import's masked
  stage. Then the binding is read as the *winning* binding's target path instead
  (`winning_binding_target`, the same walk — strength and collection bindings
  included — answering the path rather than the prim), so the prim keeps its
  provenance and the material still falls back to default grey with
  `material.fallback_default`, as before. The trap: the nearest *direct* binding is not
  the winner — under a `strongerThanDescendants` ancestor bound off-chunk it names the
  mesh's own material, which resolves and renders silently, and a collection binding
  has no direct one at all (pinned by
  `an_unresolvable_winning_binding_falls_back_at_its_own_path`). `preview` bindings are never used. Prims under a `proxy` or `guide` purpose are
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
    cutout mode, and a cutout: a constant `opacity` is thresholded into `geometry_opacity`
    (1 at or above the threshold, else 0, as Typhoon does), and a textured one becomes
    the `PreviewSurface`'s mask, sampled and thresholded per hit by `Material::opacity`
    before anything shades (the integrator's cutout, § MaterialX). A textured `ior`
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
    and which output (`r`/`g`/`b`/`a`/`rgb`) is connected. Five details:
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
    - **`a` is the file's alpha**, decoded by the host as coverage (never through the
      colour space; see the `textures` design record § UV textures), and 1.0 for a file
      without one, as the node set specifies. So the cutout above has a real mask when
      `opacity` reads a texture's `a` under `opacityThreshold > 0`, which is how every DCC
      exports a foliage card. Until issue #267 the host samplers returned an opaque alpha,
      the cards rendered solid, and the import warned `preview.texture_alpha` (retired
      with it; `crust-check` went to `/2`). The same alpha reaches a MaterialX `image`
      node of type `color4` / `vector4` as its fourth channel.
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
    plastic. `alpha` is `geometry_opacity`, which is a cutout: an island material that
    authors it below 1 renders partly present, where before this was ignored and it
    rendered solid. `subsurface*`, `diffuseTransmission` and `specularTint` have no equivalent
    lobe and are dropped.
  - **`.mtlx` reference** → the MaterialX graph, read by crust itself
    (`crust-mtlx` + `material/materialx.rs`, below). Checked at each point the USD path gives up,
    not first: finding the reference means walking the prim's composition
    graph, which a stage of ordinary USD materials should not pay for.
  - **Inline MaterialX** (`info:id = "ND_…"`) → the network translated into a
    `crust_mtlx::Doc` and compiled as the same graph in a `.mtlx` would be
    (`scene/usd_import/mtlx_network.rs`, § MaterialX volume terminals and inline
    networks below). `mtlx` is the **last** surface context consulted
    (`SURFACE_RENDER_CONTEXTS = ["", "glslfx", "mtlx"]`), so a material that also
    authors a decodable universal or preview surface renders through that, exactly as
    before inline networks were read; a MaterialX `volume` terminal is checked before
    any of it, since it changes what the material *is*.
  - Unbound geometry → grey diffuse `OpenPBR`.
  - **Displacement is resolved beside the material, not inside it.** `resolve_bound`
    returns a `BoundMaterial { material, displacement }`, cached together per
    `(epoch, path)`; the displacement is resolved the first time a mesh asks, so a
    material bound only to spheres or curves never opens its displacement maps. A `Displacement` (`material/displacement.rs`) is consumed once, at
    import, by the mesh displacement pass (`usd-scene-import` design record §
    Displacement), never per hit, so it is not a `Material` method and the
    `tests/resolve.rs` pins are untouched. Sources, first match wins: a `PxrDisplace`
    child shader (RenderMan / Moana), a `.mtlx` `displacementshader`, then
    `UsdPreviewSurface.inputs:displacement`. `CRUST_DISPLACE=0` makes every displacement
    `None`, which is the code path that existed before.

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
  name-addressable graph), `value.rs` (the one runtime value), `eval/` (the
  graph compiled to a slot-indexed program: `compile.rs`, and the interpreter
  in `apply.rs`), `bsdf.rs` (the closure tree), `surface/` (the three
  surface-shader nodes expanded into closure trees, one file each) —
  and crust-core evaluates what it describes: `closure/` collapses the tree at
  a vertex and shades its leaves, `materialx.rs` is the `Material` and the
  importer-facing `load()`.
  - **Colour spaces follow the specification.** crust-mtlx resolves each
    input's effective `colorspace` (input → node → nodegraph → document; an
    explicit empty value means "no conversion", as MaterialX's
    `getActiveColorSpace` does) and manages only `color3` / `color4` values.
    An `image` / `tiledimage` with a colour output hands its `file`'s effective
    space to the host's texture loader; a data image (`float`, `vector*`) is
    handed `None` whatever the document declares, so masks, heights and normal
    maps are never converted. Every authored colour literal — an image's
    authored colour `default` included — goes once, at compile time, through
    the host's `ColorConverter` (`crust_mtlx::Host::convert_color`), which
    crust-core implements with OCIO into the working space; RGB is converted,
    alpha kept. Nodedef defaults, and colours no scope declares a space for,
    are taken as already in the working space. Literal-zero pruning tests the
    *converted* value, so it stays exact under any conversion; `authored_away`
    still compares the authored value with the nodedef default, since it asks
    whether the author changed it. A document whose root declares
    `colorspace="lin_rec709"` (the Material Fidelity suite's) converts nothing
    in a `lin_rec709` render. See `docs/color_management.md`.
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
  - **A closure tree, evaluated with testrender's semantics.** MaterialX
    assembles a look from standalone BSDF nodes glued with `layer`, `mix`, `add`
    and `multiply`, and `bsdf::flatten` keeps that tree as it is: an arena of
    `Closure::{Leaf, Layer, Mix, Add, Multiply, Empty}` whose parameters are
    program slots, so the graph's masks and textures drive them per point. The
    rules are those of MaterialX's OSL backend as OSL's `testrender` renders it
    (`process_closure` and `evaluate_layer_opacity` in `src/testrender/shading.cpp`):
    `layer(top, base) = top + base · (1 − clamp(O_top(ωo), 0, 1))`;
    `mix(fg, bg, m) = m·fg + (1 − m)·bg` with `m` clamped; `multiply` scales
    the response (its weight clamped to [0, 1]); `add` sums the responses. The
    opacity `O` is a second quantity the walk returns, linear in every weight:
    a leaf gives `w · (1 − filter_o)` — `w·E(ωo)` for a dielectric or
    generalized Schlick that only reflects and for a sheen, `w` for every
    other leaf (diffuse, conductor, translucent, subsurface, hair, and a
    dielectric or Schlick that refracts); `mix` blends opacities, `add` sums
    them, `multiply` scales them, and a `layer` inside a top contributes
    `O_top + O_base`, neither attenuated by the other.
    That last rule is the trap, in reverse of the one the GLSL rules set. Under
    MaterialX GLSL (what this followed until the BSDL port) an opaque leaf
    hid its base *whatever its weight*, `multiply` passed its input's
    throughput through, and a nested layer multiplied `T_top · T_base`; now a
    diffuse at weight 0.3 over a conductor leaves 0.7 of it, a quarter of a
    conductor leaves three quarters, and two reflection layers in a top sum
    their `E`s (`1 − clamp(E_a + E_b)`, not `(1 − E_a)(1 − E_b)`). The sum is
    testrender's and was kept on purpose: it is less physical than the product,
    but no built-in graph nests a layer in a top, and `closure/tests.rs` pins it
    (`a_layer_nested_in_a_top_sums_its_opacities`) beside a direct
    transcription of `evaluate_layer_opacity` run on fixed trees
    (`the_opacity_rule_matches_testrender`). No shipped surface moved under the
    rule alone: every sample scene rendered bit-identically before the leaves
    were ported, since the built-in graphs' tops are reflection-only
    dielectrics and sheens, whose opacity is `w·E` under either rule.
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
    `BSDF(0, 1)`: no response, and nothing in the way (opacity 0, testrender's
    null closure). A leaf at a *connected* weight of 0 gives the same, so
    pruning is exact for every leaf. A `mix` at a literal endpoint selects its
    other branch exactly, and `layer`, `add` and `multiply` simplify over an
    empty branch exactly (`layer(∅, b) = b`, `add(a, ∅) = a`). A `mix` with a
    live factor stays a mix, with a `Closure::Empty` on the pruned side
    (`Closures::mix`, the one place `bsdf_tree` and the surface builders build
    a mix). Under the GLSL rules that was the trap: rewriting `mix(fg, ∅, m)`
    as `multiply(fg, m)` kept the response but lost the throughput, because
    GLSL's `multiply` passed `T_fg` through, and for an opaque `fg` that is 0.
    Under testrender's rule the two agree (`m · O_fg` either way), but the mix
    is kept as the tree authors it. The DPEL assets are built on exactly this
    shape. Each dust and stain diffuse is mixed by its mask against a
    zero-weight, IOR-1 `dielectric_bsdf` (their "transmission dummy"), which
    turns the mask into a *coverage*, and that coverage is layered over the
    glaze; with the rewrite under GLSL, both assets rendered black except for
    their dust and the Lion's sheen. `closure/tests.rs` pins a pruned dummy
    against the same dummy at a *connected* weight of 0, which the compiler
    cannot prune, bit for bit.
  - **Collapsed at `resolve`, exactly.** `O_top` depends on ωo alone, and ωo
    is fixed at a path vertex, so there the tree *is* a weighted sum of leaves.
    `ResolvedClosure::resolve` walks it once per vertex: `mix` and `multiply`
    scale the weight passed down, `layer` scales everything below by
    `1 − clamp(O_top)`, and each live leaf lands in an inline list of at most
    `MAX_LEAVES` = 8 with its RGB weight, its own frame (a leaf's `normal` and
    `tangent` inputs, so a coat can carry a normal map its base does not) and its
    parameters. `eval` is `Σ wᵢ fᵢ`; sampling picks a leaf with probability
    `∝ lum(wᵢ)·lum(Êᵢ(ωo))` (floored at 0.02 so no live leaf gets probability 0,
    as Typhoon's `_ApproxWeight` does), `Ê` BSDL's `albedo_impl` — the colour
    for a diffuse or translucent leaf, `refl_tint · E_R` for a reflection-only
    interface, 1 for a refracting one, `color · min(E · max(color), 1)` for a
    sheen — except for a conductor, where BSDL's is a rough fit to the
    average Fresnel and crust uses the compensated albedo toward the view,
    `F(1 − E_ms) + F²E_ms` (`tests/bsdl_oracle.rs`, `conductor-selection`) and returns the mixture pdf, one-sample MIS
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
  - **Leaves are BSDL's.** Each is the `mtx::` lobe of BSDL (OSL's
    `src/libbsdl`, BSD-3-Clause, at the commit `bsdl_luts_to_rust.py` pins),
    transcribed in `closure/bsdl.rs` under BSDL's names: `DielectricLobe`,
    `ConductorLobe`, `SchlickLobe`, `SheenLobe`, `OrenNayarDiffuseLobe` (with
    and without `energy_compensation`, the latter EON), `BurleyDiffuseLobe` and
    `TranslucentLobe`, with the helpers they use (`GGXDist`,
    `TabulatedEnergyCurve`, the dielectric, conductor and Schlick Fresnel,
    `eval_turquin_microms_reflection`). The value is BSDL's arithmetic in BSDL's
    order, as `f` without the cosine, and so is the sampling, density
    included (below). Specific points:
    - **Roughness.** A specular leaf's `roughness` is GGX **alpha**, taken as
      authored (floored at BSDL's `EPSILON`, 1e-4, and capped at 1) — the DPEL
      teapot's `desquare_roughness_*` nodes exist to produce it. BSDL
      re-expresses the pair as `roughness = √(α_max/(1 + aniso))`,
      `aniso = (1 − b)/(1 + b)` with `b = α_min/α_max`, which is what its
      tables are indexed by, and rebuilds GGX from that pair; the alphas come
      back to rounding, and `r² = (α_x + α_y)/2`.
    - **Multiple scattering.** A lobe that only reflects multiplies single
      scattering by Turquin's `1 + F·E_ms/(1 − E_ms)` with `F = F(cos θ_mo)`
      and `E_ms` white GGX's missing energy at the view (the `GGX` table): a
      conductor, a reflection-only dielectric or Schlick. A lobe that refracts
      gets no Turquin term; its single scattering (reflection and Walter
      refraction, `1 − F` per channel) is divided by `max(0.01, 1 − E_ms)`
      with `E_ms` from `DielectricBothFront` or `BothBack`, by face. A
      thin-walled leaf, which transmits straight through and loses nothing
      to shadowing, is not renormalised.
    - **Layering filter.** A reflection-only dielectric or Schlick lets
      `DielectricReflFront(cos θo)` through (a Schlick at the IOR its average
      `F0` implies, `(1 + √F0)/(1 − √F0)`) — on a front face. On a back face
      it lets through what its own lobe leaves, `DielectricReflBack`, which
      is nothing past the critical angle (crust's deviation from BSDL, below
      in "Known gaps: MaterialX"); a sheen `1 − min(E·max(color), 1)`
      with `E` MaterialX's rational fit (`conty_kulla`, roughness floored at
      0.06) or the LTC table's `R` (`zeltner`); everything else nothing. A
      dielectric whose tints are both black is opaque, as BSDL computes no
      filter for it.
    - **Tints.** genosl splits `tint` by scatter mode: `R` reflects it and
      transmits black, `T` the reverse, `RT` both (`refl_tint`, `refr_tint`).
      A `T` leaf therefore pays its own `1 − F` (the reflection half is
      black), as MaterialX GLSL, OSL, BSDL and Typhoon all do. Leaving it out,
      on the reading that the reflection layer above "owns" the Fresnel loss,
      once made a glass's exit leaf transmit `1 − E_R` beside a reflection leaf
      already reflecting total internal reflection, and the furnace caught a
      `standard_surface` glass sphere at 1.16× its environment.
    - **IOR.** Clamped into BSDL's [1.001, 5] and inverted on a back face
      (`DielectricFresnel`); `generalized_schlick_bsdf` reflects everything
      past the critical angle on a back face (`SchlickFresnel::tir_cos`).
    - **Sheen.** `conty_kulla` is BSDL's `ContyKullaSheenMTX`: the Charlie
      distribution with the non-legacy visibility
      `cosθi·cosθo/(cosθi + cosθo − cosθi·cosθo)` — MaterialX's Imageworks
      sheen, with BSDL's 0.06 floor. `zeltner` reads the LTC table
      (`brdf::ZeltnerSheen`, the native fuzz's, value for value BSDL's
      `bsdf_zeltnersheen_param.h`) at `max(√roughness, 0.02)`, BSDL's index —
      so the MaterialX sheen at `r` equals the native fuzz at `√r`. Neither
      shades a back face.
    - **Sampling is BSDL's** (doubleailes/crust-render#378). A microfacet
      leaf draws its micro-normal with Eto and Tokuyoshi's bounded VNDF
      (`GGXDist::sample_for_refl`) and reflects it, or — for a lobe that
      refracts — reflects it with probability `P = avg F(cos θ_mo)` and
      refracts it otherwise (`DielectricBSDF::sample`; a `T` leaf draws its
      black reflection too, as BSDL does). Its density is BSDL's: `D_refl /
      (4 cos θo)` reflected (times `P` when it refracts), `D_refl · J · (1 −
      P)` refracted, `D_refl = D_refl_D(ωo) · D(m)`. A `conty_kulla` sheen is
      sampled uniformly over the hemisphere (`1/2π`, none where the lobe has
      no value), a `zeltner` one by its LTC, the diffuse and translucent
      leaves cosine-weighted, all as BSDL. This is what makes the refracting
      renormalisation right: `BothFront` / `BothBack` are the sampled weights'
      average under exactly this sampler, so the BSDF-sampled estimator
      returns what the lobe receives. Under the full GGX VNDF that crust used
      first, a rough clear glass sphere returned 2.0 of a white furnace at
      roughness 1 (1.23 at 0.7, 1.06 at 0.5); now 1.004
      (`a_rough_glass_is_bounded_in_a_white_furnace`). Two things stay crust's:
      a thin-walled leaf that transmits (its straight-through delta has no
      BSDL counterpart) samples the full VNDF, and leaf *selection* stays
      `∝ lum(w)·lum(Ê)`.
      Cost (callgrind, `-s 2`, against the VNDF build): `cornellbox`
      4 276 033 387 → 4 276 091 761 (+0.001%, bit-identical image),
      `materialx_basic` 2 763 512 676 → 2 762 675 754 (−0.03%),
      `materialx_surfaces` 5 440 988 714 → 5 515 381 081 (+1.37%: the rough
      glass and its refractions); only MaterialX scenes change.
    - **Diffuse and translucent.** BSDL's QON (no compensation) and EON
      (compensation, Portsmouth) and Burley are MaterialX's GLSL lobes to
      rounding; translucent is `color/π` below the plane.
    - **What stays MaterialX's GLSL** (`closure/mx.rs`): the Airy thin film
      (BSDL's lobes declare `thinfilm_*` and never apply it) and the F82 tint
      of generalized Schlick (BSDL's `SchlickLobe` has no `color82`; with a
      white `color82` crust evaluates BSDL's Schlick bit for bit), and
      `chiang_hair_bsdf` (`closure/hair.rs`; genosl has only a placeholder).
    - **Not BSDL's, on purpose:** no path-roughness regularisation
      (testrender's `regularize_roughness` widens a lobe after a rough bounce,
      which biases); no `visible_normal` bending — a normal map that tilts
      the frame under the view is handled by lifting the view to 1e-6 above
      the leaf's plane, in both `eval` and sampling.
    Dielectric scatter modes: `R`; `RT`, which picks reflection with
    probability `F(v·h)` — the *same* film-aware Fresnel on the sampling and
    the pdf side, or the two disagree; and `T`. A thin-walled surface
    transmits straight through; a thick one hands the refracted ray the
    interior medium.
    The straight transmission is the integrator's to carry, as a pass-through
    (rendering design record, "Thin walls are pass-throughs"):
    `ResolvedClosure::straight_transmittance` estimates it with one VNDF draw
    (a rough leaf's `1 − F(v·h)` averaged over its microfacets has no closed
    form, so `T` is an unbiased estimate), and `exclude_straight` leaves what a
    path meeting the wall scatters through — the transmission-only thin leaves
    dropped, a thin `RT` leaf sampled as `R`, every other weight untouched, so the
    layering above the sheet still attenuates the rest as before.
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
    the leaf's weight (the layer's opacity above it); the entry only picks a
    direction. The tracer then runs the walk (`subsurface.rs`, a port of
    `ty::RandomWalkSSS`, itself Cycles' `subsurface_random_walk.h`): Chiang
    2016's albedo inversion with the 0.2 albedo floor and its throughput
    correction, per-bounce channel MIS, forward and backward Dwivedi guiding
    with the extended first ray finding the opposite interface, the similarity
    relation after 9 bounces, 256 bounces at most. Two additions over the port,
    both measured in `docs/subsurface_walk.md`: a scatter whose every channel has
    fallen under 0.05 survives a roulette with probability `peak / 0.05`
    (floor 0.05) and is reweighted — unbiased, 10–15% fewer steps for a
    chromatic medium such as skin, no measurable variance; and the
    backward-stretched transmittance is `exp(−σt)² / exp(−σ(1 − c/ν)t)` rather
    than a third exponential per channel, six `expf` a step instead of nine
    (the quotient yields to the exponential when either term is below 1e-18).
    It traces the owner's
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
      sample scene without a subsurface leaf renders bit-identically. The walk
      is its own `--profile` section (`Subsurface`, opened only when a walk
      runs: cornellbox +0.0002%); without one it hid in `MainLoop`'s local
      time, 34% of the fixture's thread time.
  - **`chiang_hair_bsdf` is pbrt-v3's `HairBSDF` with MaterialX's parameters**
    (`closure/hair.rs`, `Lobe::Hair`). The leaf frame is the fibre's:
    - `x` is the tangent: `curve_direction`, else the hit's. On a curve, the
      kernel reports the strand's direction through every instance (see the
      intersection-kernel record).
    - `z` is the tube's ray-facing normal.
    - γo is derived from ωo's projection into the normal plane, against `n`,
      as genglsl derives it. There is no `h` input: on a round tube the normal
      already says where on the circle the ray landed. On a tapered cone, γo is
      off by the taper's slope.

    Everything that depends only on ωo is computed once in `prepare`: the
    tilted θo per lobe, the tinted `A_p`, the lobe pmf, γo, γt and φo. `eval`
    and `sample` pay only for ωi.

    `eval_lobe` returns pbrt's `Σ M_p·A_p·N_p / |cos θ|` against `|wi.z|`, which
    the closure's shared `·|l.z|` cancels. The pdf is pbrt's mixture
    `Σ pdf_p·M_p·N_p`, per solid angle, with no cosine. Both are zero where
    `|wi.z| < 1e-7` (measure zero; otherwise `inf · 0`). `M_p` and its sampling
    run in f64: at `v = 0.001`, which the roughness clamp reaches, the
    exponent is a difference of thousands.

    `sample` needs four numbers and the closure hands each leaf three. The
    fourth comes from pbrt-v3's `DemuxFloat` of the scalar, which splits it
    into lobe choice and azimuth.

    The directional albedo `Σ_p tint_p·A_p` is exact, because `M_p` and `N_p`
    are normalised. It is the leaf's selection weight, its `albedo` AOV, and,
    as the top of a `layer`, what it does not pass on. A fibre as a layer's top
    is an odd authoring choice, but this keeps the layer contract with no
    special case.

    Light path expressions classify it by hemisphere, like every leaf: glossy
    `specular` on ωo's side and glossy `transmission` beyond. A per-lobe
    (R / TT / TRT) split would need several `LobeSplit`s per leaf.

    **Deviations from genglsl (`mx_chiang_hair_bsdf.glsl`, MaterialX 1.39.5),
    on purpose:**
    - no `1/π` on the response;
    - TRRT+'s `N_p` is `1/(2π)`, not genglsl's `1.0 / 2.0 * M_PI` (= π/2);
    - importance sampling, which genglsl has none of.

    With either genglsl constant, the white furnace fails (pinned by
    `a_clear_fibre_conserves_energy`; mutation-checked). OSL's
    `chiang_hair_bsdf` is a renderer-side closure, so there is no OSL value to
    match.

    **What is genglsl's, read where MaterialX defines nothing:**
    - **The roughness clamp.** `(v, s)` are clamped to [0.001, 1]. That caps
      artist roughness at about β_m 0.62 and β_n 0.68, which pbrt has no cap
      for.
    - **The cuticle sign.** genglsl tilts θi by `(2 − 3p)·α` with
      `α = cuticle_angle·π − π/2`. That is pbrt's tilt of θo with α negated, so
      pbrt's formulas run on `−α`. `the_cuticle_tilt_moves_r_and_trt_opposite_ways`
      pins R's peak at `−θo − 2α` and TRT's at `−θo + 4α`.

    **The helpers** (`chiang_hair_roughness`, `chiang_hair_absorption_from_color`,
    `deon_hair_absorption_from_melanin`; `crust-mtlx/src/hair.rs`) compile
    from existing ops, so the JIT runs them unchanged; `crust-jit`'s fixture
    test pins it bitwise on `samples/hair.mtlx`. MaterialX's genosl versions
    are placeholders (`vector(1.0)`, zeros), so the OSL oracle cannot pin
    them. `scripts/hair_reference.py` transcribes genglsl in float64 into
    `tests/data/hair_helpers.txt`, at the oracle's format and tolerance.

    **A fibre's light passes out of curve tubes.** The model already
    integrates light's path through the fibre from the entry point, so the
    tube's far wall belongs to that same event. `ResolvedClosure::hair` is set
    when a live hair leaf is pushed, and makes the vertex's rays carry
    `crust_rt::Ray::ignore_curve_exits`:
    - the continuation ray (`ray()`, so guided directions too);
    - the NEE shadow ray (`ShadingPoint::passes_out_of_curves`);
    - the learned light cache's training shadow rays, so a backlight behind a
      strand is learned as reachable;
    - and through cutout restarts.

    **Only the fibre's light, where a fibre shares the vertex with a
    transmitting leaf** (`mixes_hair`: a refracting dielectric or a
    translucent beside it). That leaf's light going into the tube must meet
    the far wall as it would on its own, so one closure-wide flag is wrong
    there (Qodo's review of #207).
    - **Continuation.** One ray cannot carry both answers, so
      `scatter_choosing` picks the fibres' share or the others' in proportion
      to their values toward the sampled direction, using the leaf pick's
      residual as its uniform number (no new draw). The value is that share
      over its probability: an unbiased one-sample estimate of the sum.
      `scatter_split` scales the chosen leaves' shares by the same factor and
      zeroes the others', so the split still sums to the value bit for bit.
      Out of the tube the flag changes nothing, so the value is `eval`'s
      there.
    - **NEE.** Toward a light on the tube's far side, the shadow ray is traced
      twice, once passing and once not. The fibres' share takes the first
      transmittance and the others' the second. Light path expressions route
      each leaf with its own transmittance (`hair_leaves` marks the fibres).
    - **Guiding** is off at such a vertex, on both MIS sides: a guided
      direction carries the whole value on one ray, with nothing to split.
    - **The light cache** keeps one flag (the fibre's), being a guide only.
    - **A delta lobe is never a fibre's**, so a delta sample's ray never
      passes.
    - **Pinned** by `a_mixed_fibre_vertex_splits_its_continuation_by_share`
      and by `only_the_fibres_light_passes_out_of_a_mixed_strand`. That test
      checks a backlit half-fibre, half-translucent strand against half the
      clear fibre's glow: exact under light sampling, within noise under BSDF
      sampling. With the old closure-wide flag it fails under both.

    A ray starting outside a tube must enter it before it can leave it, so
    only the strand the ray started in is hidden; a fibre still shadows
    everything else as an opaque tube. On a strand in a white furnace, a clear
    fibre returns 1.00 at every offset across it (`tests/hair.rs`). The furnace
    cannot see the rule: a lossless fibre scattering twice still returns
    everything. What the rule changes is a light *behind* the strand, which its
    far wall otherwise hides. `a_backlit_strand_glows_where_a_diffuse_one_is_dark`
    runs that test once under `LightOnly` and once under `BsdfOnly`, so each run
    pins one of the two rays. With either ray's flag removed, its run renders the
    strand black (mutation-checked).

    **Cost, measured** (callgrind at `-s 2`, before → after the change):
    - `cornellbox`, which has no curves: 4 207 630 659 → 4 225 099 773 instructions
      (+0.42%). Of that, +0.34% is the kernel's hit records growing to carry the
      tangent (intersection-kernel record), +0.05% is the NEE shadow ray asking
      `passes_out_of_curves`, and +0.03% is the mixed-fibre check (`mixes_hair`).
      The mixed vertex's second shadow ray and split live in a cold, out-of-line
      `mixed_hair_shadow`. Written as a second closure around the shadow query,
      it cost +0.8% (+23 M in a closure no longer inlined), even though no
      vertex here is mixed.
    - `curves`: 58 028 309 → 58 201 388 (+0.30%).
    - `scripts/bench_ab.sh -n 7`: `cornellbox` 6.561 / 6.884 s → 6.693 / 6.982 s,
      min / mean (+2.0% / +1.4%), inside the run-to-run spread. `curves` renders in
      5 ms, below what wall-clock can resolve.
    - Every sample scene renders bit-identically (`scripts/check_images.sh`).

    This is over the change's +0.3% budget, and kept: the cost is the hit record's
    size, not the instance transform the budget's fallback would have moved, and
    the one packing that keeps 48 bytes costs more.
  - **Energy tables are BSDL's, regenerated from upstream.**
    `closure/bsdl_tables.rs` holds the four tables the ported lobes read:
    `spi::MiniMicrofacetGGX` (16 roughness × 16 cosines spaced `(i/15)²`, white
    GGX's missing energy, Turquin's `E_ms`) and `mtx::DielectricReflFront`,
    `DielectricBothFront`, `DielectricBothBack` (32 IOR × 16 roughness × 16
    cosines `i/15`), and a fifth BSDL does not bake, `DielectricReflBack`.
    `scripts/tables/bsdl_luts_to_rust.py` fetches OSL
    (`src/libbsdl`) and Imath at pinned commits, builds and runs BSDL's
    `genluts` (about a minute on four cores) and writes the file; the bake is
    deterministic (hashed QMC seeds), so a rerun reproduces it bit for bit.
    `DielectricReflBack` is `DielectricReflFront` built with
    `from_table_index(f, true)` — the reflection-only lobe seen from inside —
    and baked by a copy of `genluts.cpp` with that struct and one loop added
    (the pinned checkout is never modified; the other four tables come out
    unchanged, as the seeds depend on each entry's indices alone). Its
    cosine axis is not `i/15`: every IOR row puts its own critical cosine
    `√(1 − η²)` at column 7, `[0, c_crit]` before it and `[c_crit, 1]` after,
    each quadratically denser toward that column, and crust reads it at
    `bsdl::refl_back_axis(cos θo, η)`. On BSDL's fixed cosines the edge where
    the lobe starts to totally reflect moves with the IOR, so bilinear
    interpolation straddled it — across cosines and across IOR rows alike —
    and let a smooth top pass up to half its base past the critical angle
    (a coat over glass returned 1.50 per bounce at IOR 1.6, 39°); evenly
    spaced but aligned columns still left a slightly rough lobe's whole
    transition inside one bin (1.22 at IOR 2.4, roughness 0.02). Aligned and
    warped, the filter tracks the leaf within +0.058 / −0.016
    (`the_back_face_filter_tracks_the_leaf_it_stands_for`, worst at IOR 2.4,
    roughness 0.05, 1° past the critical angle).
    The first gate when the source moved off NVIDIA Typhoon's vendored copy
    held: upstream `cc6f9b1`'s `ReflFront` is bit for bit the table crust had
    taken from Typhoon (`70c45e8`). The sheen tables `genluts` also bakes are
    not converted: `mtx::SheenLobe` reads neither, and its `zeltner` LTC
    parameters (`bsdf_zeltnersheen_param.h`) are `ltc_sheen_table.rs` value for
    value, all 3072, so one table ships. The tables are read as
    `TabulatedEnergyCurve` reads them — bilinear in (IOR index, roughness),
    piecewise linear in the cosine — with the rows and weights found once per
    curve and the cosine's bin found from the spacing rather than by BSDL's
    scan (`the_direct_bin_is_the_scanned_one` pins the two equal, bit for
    bit). Because the lobes are BSDL's and so are the tables, a filter and
    the leaf it stands for are the same integral up to the bake's sampling:
    `ReflFront` is within 0.025 of crust's integrated dielectric leaf, the
    worst near-smooth grazing where linear interpolation in the cosine
    undershoots a steep curve (`the_bsdl_table_tracks_the_leaf_it_stands_for`).
    The filters BSDL takes from elsewhere are pinned the same way
    (`the_layering_filters_track_the_leaves_they_stand_for`): a generalized
    Schlick reads the dielectric table at the IOR its `F0` implies, which BSDL
    itself notes is "a bit off" for a Schlick curve (worst +0.033 at roughness
    0.1, grazing); a `conty_kulla` sheen layers with MaterialX's rational fit,
    off by up to 0.035 at low roughness and grazing, where the layer then
    loses energy rather than creating it.
  - **Cost of the BSDL leaves, measured** (callgrind, `-s 2`, one thread,
    before → after the port): `cornellbox` 4 279 082 833 → 4 275 967 240
    (−0.07%, codegen in `advance_pixel`; it renders bit-identically);
    `materialx_basic` 2 719 059 626 → 2 763 360 610 (+1.63%);
    `materialx_surfaces` 5 329 130 786 → 5 441 103 373 (+2.10%). The cost is
    table reads: a reflection-only interface reads two curves per vertex
    (Turquin's GGX `E_ms` and its `ReflFront` filter) where the GLSL leaf read
    one table and one rational fit. The first cut was +2.4% / +4.3%:
    `Emiss_eval`'s linear scan over 16 cosines and `interpolate_emiss`
    recomputing its rows per call cost more than the reads themselves, and a
    refracting leaf read `ReflFront` for a selection albedo BSDL sets to 1.
    `scripts/bench_ab.sh -n 7`, min / mean seconds: `cornellbox` 8.470 / 10.289
    → 8.568 / 10.658 (+1.2% / +3.6%), `materialx_basic` 2.132 / 2.336 → 2.147 /
    2.433 (+0.7% / +4.2%), `materialx_surfaces` 4.753 / 5.120 → 4.896 / 5.268
    (+3.0% / +2.9%). The machine was loaded (a download ran beside it), and
    `cornellbox`, whose instructions did not move, sets the noise floor: the
    MaterialX scenes are inside it. The back-face reflection filter (a second
    curve's axis, a square root under a back face) then cost
    `materialx_surfaces` 5 512 095 636 → 5 534 763 330 (+0.41%, partly paths
    that change with the image) and `cornellbox` 4 272 611 828 → 4 272 737 158
    (+0.003%, codegen; bit-identical).
  - **The BSDL oracle.** `scripts/bsdl_oracle.py` bakes the tables as above,
    compiles `scripts/bsdl_oracle/probe.cpp` against the pinned `libbsdl` and
    Imath (no OSL runtime: each lobe is built as testrender's `BSDL_WRAP`
    builds it, on BSDL's default configuration with exact libm math), and
    writes `crates/crust-core/tests/data/bsdl_oracle.txt`: 414 cases, each one
    leaf with every input authored, a view and eight lights, answered with
    the directional albedo over a 32 × 32 cosine-weighted grid on each side,
    `filter_o`, `albedo_impl`, and `f·|cos|` and the sampling density toward
    each light. The cases cover
    every leaf and mode (`dielectric_bsdf` `R` / `T` / `RT` on both faces at
    IOR 1.001–5 and roughness 0–1, anisotropic both ways and tinted;
    `conductor_bsdf` for three metals; `generalized_schlick_bsdf` `R` and `RT`;
    both sheens; Oren–Nayar with and without compensation; Burley;
    translucent; films), plus 160 random ones, with a header naming both
    pins. `tests/bsdl_oracle.rs` compiles each case as a MaterialX document,
    collapses it with `ResolvedClosure::resolve` and reads the filter where it
    acts (the weight a `layer` gives a base under the leaf). It uses
    `adobe_oracle`'s `Deviation` rules: a case may differ only under rules
    whose conditions hold, by at most the sum of their bounds, and a rule
    that excuses nothing fails as stale. Tolerance 2e-3; every case outside
    the rules matches to 0.00000 at the fixture's four decimals. The rules:
    `thin-film` (0.611; BSDL ignores the film), `conductor-selection` (0.357;
    `albedo_impl` only), `back-face-reflection-filter` (0.928; the filter and
    the `albedo_impl` derived from it, for a reflection-only leaf under a back
    face — a bound that excuses nearly the whole range, so the filter there is
    pinned instead against the leaf itself, see "Energy tables" above). CI
    needs no C++: the fixture is committed. Regenerate
    with `scripts/bsdl_oracle.py` (git, cmake, a C++17 compiler, network),
    then `cargo test -p crust-core --test bsdl_oracle -- --ignored --nocapture`
    to re-measure the bounds; move the OSL pin in both scripts together. The
    leaves were moved onto BSDL one at a time against temporary "genglsl
    leaf" rules, each bounded by the GLSL leaf's measured error and each
    failing as stale once its leaf matched; none remains.
  - **Surface shaders are their nodegraphs, node for node.** `open_pbr_surface`
    (1.1), `standard_surface` (1.0.1), `gltf_pbr` (2.0.1) and `UsdPreviewSurface`
    (2.6) are MaterialX
    nodedefs implemented as nodegraphs over standalone BSDFs, and a document
    never carries the implementation. `surface/` reproduces each graph — the
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
    `UsdPreviewSurface` (2.6, `ND_UsdPreviewSurface_surfaceshader`, the id a
    DCC's MaterialX export often gives its surfaces) is built from MaterialX's own
    `IMP_UsdPreviewSurface_surfaceshader`, not mapped onto OpenPBR as the
    UsdShade `UsdPreviewSurface` id is (`preview.rs`): its inputs arrive from
    MaterialX node graphs that the preview decoder cannot read, and the graph
    is the definition of that id. Both workflows, the clearcoat, the
    `opacity` transmission mix and the `opacityMode` switch are the graph's;
    `occlusion` and `displacement`, which the graph does not use, are
    reported. **One deliberate departure:** at an `opacityThreshold` that
    folds to 0 the graph's presence is `opacity ≥ 0`, which is 1 for every
    opacity that means anything, so it is taken as 1. Compiled as written, it
    would make every preview surface with a connected opacity a cutout, and a
    cutout makes the shadow side walk every blocked ray hit by hit.
  - **Custom nodes are their graphs, expanded before compilation.**
    `crust_mtlx::Library` holds `<nodedef>`s and their `<nodegraph nodedef=…>`
    implementations: a document's own (`Doc::library`) and those under
    `PXR_MTLX_PLUGIN_SEARCH_PATHS` (`MaterialCache::library`, read once per
    import, `materials::mtlx_library`, each directory walked once by its
    canonical path so a link back to an ancestor reads nothing twice). The
    import keeps that `Arc` in its provenance (`StageFacts::mtlx_library`):
    an in-place rebuild resolves against it rather than reading the files
    again, which would also raise `mtlx.library_unreadable` with no prims and
    so force a full re-import. An instance names its nodedef by category,
    output type and, when it authors one, `version` (`Library::for_node`); an
    inline shader whose `info:id` names a library nodedef carries that
    nodedef's version. A category a library defines, but not for that type or
    version, is reported and left unsupported rather than expanded with
    another signature. `Doc::expand` replaces each instance
    with a copy of its graph whose nodes are renamed `instance/node` in the
    instance's scope; an `interfacename` input takes the instance's input as
    authored, else the nodedef default, else stays unauthored; every reference
    to the instance — node inputs, nodegraph and document outputs, and a
    terminal (`Doc::find_node`) — is redirected to the graph output it selects,
    keeping that output's own `output=`. It runs in passes, so a graph that
    instantiates another custom node (a layering node built from several
    instances of a blending node) expands in turn. Each copy remembers the
    nodedefs it was expanded through: a definition that reaches itself is a
    cycle, refused at its first re-entry and reported once. A chain past
    `MAX_DEPTH` (16), or an expansion past `MAX_EXPANDED_NODES` (262 144) in
    all, is cut and reported too. **Trap:** the depth alone bounds nothing —
    a graph holding k instances of the next definition grows as k to the
    depth, 43 million nodes for k = 3 at 16. Both ways MaterialX arrives go through it: `compile_with` for a
    `.mtlx`, `mtlx_network::translate` for an inline network, whose shader ids
    are looked up in the library before `nodedef_category` parses them (a
    studio nodedef's name need not follow `ND_<category>_<type>`). **The compiler's operators
    always win** (`expand::BUILTIN_CATEGORIES`, kept in step with
    `compile_node` by a test): a library shipping a node-graph `place2d` must not
    replace the operator the OSL oracle pins. An expanded node computes, bit for
    bit, what the same graph written by hand computes
    (`an_expanded_node_is_bit_identical_to_its_graph_by_hand`): expansion adds
    no ops of its own, and the compiler's memo shares a node read by several
    interface inputs.
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
  - **Displacement is a third program, run at vertices.** `crust-mtlx` follows a
    `surfacematerial`'s `displacementshader` to a `displacement` node and records its
    `float` `displacement` input and its `scale` as `Compiled::displacement`, two slots
    that are **not** among `roots()`. crust-core slices them into their own program
    (`Compiled::displacement_program`, `Program::optimize` to the two roots,
    JIT-compiled like the opacity slice and pinned against the interpreter by
    `the_jit_displacement_matches_the_interpreter`) *before* optimising the surface
    program, which then prunes the displacement's ops: shading a hit never runs them.
    `MtlxDisplacement` evaluates `value · scale` from a vertex `ShadeCtx`: the owner
    corner's `uv` and footprint, the **local** position and normal (MaterialX
    displacement is object space), a zero tangent and a head-on viewer (`view = −normal`:
    `view` points toward the surface). So `position`-,
    `normal`- and chart-driven graphs work, and view-dependent nodes have no meaningful
    value. A `vector3` input (vector displacement), or a `displacementshader` that is not
    a `displacement` node, is refused into `Closures::reported`, which the loader warns
    once per material. A declined `image` falls back to its `default` (0.5 unless
    authored), as it does for shading.
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
    `dispersion`, OpenPBR's `transmission_dispersion_scale`). A `zeltner` sheen was
    once reported too, while it was evaluated as Imageworks; it is now evaluated as
    itself. Default-valued inputs stay silent: the suite authors
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
  - **Node semantics are checked against the reference implementation.**
    `crust-mtlx/tests/osl_oracle.rs` replays 5 440 one-node cases through
    `Compiler` + `Program::eval` and compares every lane with the value
    MaterialX's own OSL code generator (`genosl`) produces for the same
    document, compiled by `oslc` and run once by `testshade`. That covers all
    319 stdlib signatures of the pattern nodes crust compiles, over the six
    value types it models (plus the `integer` and `boolean` inputs and outputs
    of the conditionals and random nodes, carried as one-lane floats), with a
    zero-input "defaults" case plus 16 seeded
    random ones each (a fifth of the inputs are left unauthored, so defaults
    are also checked in combination). The expected values live in
    `tests/data/osl_oracle.txt` (committed, ~450 KiB), so `cargo test` needs
    neither OSL nor MaterialX; `scripts/osl_oracle.py` regenerates it. Inputs
    are drawn from literals with at most three decimals, so genosl's `%f`
    printing and crust's parse give both sides bit-identical inputs, and lanes
    must agree to 1e-5 relative (the two sides use different `libm`s). The
    width must agree exactly: a `float` zero where a `vector4` was due matches
    four zero lanes but reads as a scalar in `luminance`, `normalize`,
    `dotproduct`, `combine2` and the JIT's width inference. The fixture header
    states its signature and per-signature case counts, and
    `the_fixture_is_complete` holds the fixture to them, so a regeneration that
    lost rows cannot pass by checking less (the script also writes nothing
    unless every case ran). The
    first run found sixteen node categories that differed, every one of them
    something a render would show as a plausible value. Apart from the guards
    below, they were crust bugs and were fixed to the reference (the JIT
    followed wherever it inlines the op, and `crust-jit/tests/jit.rs` checks it
    still bit for bit):
    - unauthored defaults: `multiply` / `divide` / `power` / `modulo` default
      `in1` to **0**, not 1; `ln` defaults `in` to 1 (crust used 0, so an
      unauthored `ln` was −69); `artistic_ior`'s reflectivity is
      (0.944, 0.776, 0.373), not the two-decimal rounding;
    - `sign(0)` was 1 (Rust's `f32::signum` answers ±1 for ±0); the
      surface builders' `ifgreater` select, which leaned on it to send
      equality to `in2`, now tests `sign(value1 − value2)` instead;
    - `modulo` truncated (`%`) where MaterialX floors: `-0.2 mod 1` is 0.8.
      It is OSL's `x − y·floor(x / y)`, whose rounding the reference has
      (`-1 mod -0.2` is 0, not the exact −0.19999999), except where that
      quotient overflows for finite operands (`1 mod 1e-40`, −inf), where the
      exact remainder `%`, floored, takes over;
    - `power` clamped a negative base to 0, where OSL's `safe_pow` takes
      integer exponents of it (`(-3)^2 = 9`), and `0^-1` was +inf, now 0;
    - `smoothstep` and `clamp` disagreed when `low > high` (OSL's
      `smoothstep` tests `x < low`, then `x >= high`; its `clamp` is
      `max(min(x, high), low)`);
    - `luminance` ignored `lumacoeffs`, dropped a `color4`'s alpha and
      returned a `color3`'s grey as a `float`;
    - an unauthored input was a `float` whatever the node's type, so an
      `add_color3` over nothing but defaults came out one lane wide (191
      signatures, found once the test compared widths). `Compiler::input_or`
      now widens a `float` default to the node's width, which changes no lane;
    - `dotproduct` and `normalize` of a `vector4` read three lanes, and of a
      `vector2` read a third lane that was not the value's;
    - `combine2` took lane 0 of each operand, so `color4CF`, `vector4VF` and
      `vector4VV` lost lanes (the `vector4` signatures are told apart by the
      connected producers' types, since the parser reads an input with no
      `type` as a `float`); `convert` to a `color4` / `vector4` left alpha 0
      instead of 1, and widening kept whatever the producing op left in the
      new lanes;
    - `normalmap` ignored the second component of a `vector2` scale;
    - `ln` of a non-positive value is now OSL's `ln(FLT_MIN)` = −87.34, not
      −69 (the same finite guard, the reference's constant);
    - `artistic_ior` blended the IOR as `x + (y − x)·t`, which at the default
      white edge leaves `n_max`'s rounding (~70) in `n_min`: 7e-5 relative.
    Rendered, the fixes move three sample scenes, all through the
    `artistic_ior` blend: `check_images.sh` (16 spp, old binary against new)
    finds every other scene bit-identical, and restoring the old blend alone
    makes those three bit-identical too. Each holds a metal whose conductor is
    an `artistic_ior` — `materialx_basic`'s dirtied metal (6.6% of pixels, at
    most 1.5e-7 absolute, relmse 4e-16) and the metal `standard_surface`s of
    `materialx_cutout` (19%, 1.7e-5, 1.5e-14) and `materialx_surfaces`
    (7.6%, 4.6e-5, 1.5e-15) — the blend's rounding. The DPEL teapot and lion
    render identically.
    What still differs is six guards, each a rule in the test's `deviation()`
    that names the one input condition it applies to (91 lanes in all):
    a zero divisor in `divide` gives 0 and an empty `remap` input range gives
    `outlow`, not ±inf / NaN (an infinity survives every later multiply and
    reaches the framebuffer); `modulo` by zero returns the dividend, as OSL's
    float `mod` does, where its vector `mod` gives NaN; `normalmap`'s decoded
    z is raised to 1e-4, keeping the normal off the tangent plane;
    `artistic_ior` clamps `edge_color` to [0, 1] (outside it the IOR
    extrapolates to negative values); and an unauthored `convert` input is a
    zero `float`, because crust never reads the `nodedef` attribute and nothing
    else tells `convert_color3_color4` from `convert_float_color4` (alpha 0,
    not 1). **Regenerating** needs OSL built from source with
    `-DUSE_FAST_MATH=0 -DOSL_BUILD_TESTS=1`: the default build compiles OIIO's
    approximate `acos` / `asin` / `pow` into the runtime, off by ~2e-5 —
    enough to fail every inverse-trig case — and no run-time option turns it
    off (`testshade --options opt_fastmath=0` is silently accepted and does
    nothing). The script checks `acos(0.5)` and refuses such a build. OSL
    1.13.9's exact path also does not compile as shipped: four float calls
    (`safe_asin`, `safe_acos` in `liboslexec/llvm_ops.cpp`, `safe_log2`,
    `safe_log10` in `include/OSL/dual.h`) need an `OIIO::` qualifier. On
    Ubuntu 24.04 the dependencies are the distribution's
    `libopenimageio-dev`, `llvm-17-dev`, `libclang-17-dev`, `libboost-dev`,
    `libpugixml-dev`, `flex` and `bison`, plus an empty `/usr/include/opencv4`
    (OIIO's CMake config names it); MaterialX is `pip install materialx`. A
    minimal image also needs `libboost-filesystem-dev`,
    `libboost-thread-dev`, `libopenexr-dev` and `openimageio-tools` (OIIO's
    CMake targets name its `iconvert`), and `-DCMAKE_CXX_STANDARD=17`: OSL
    defaults to C++14, which LLVM 17's headers do not compile under. Such a
    build reproduced the committed fixture row for row before the HSV,
    conditional and random signatures were added.
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

## MaterialX volume terminals and inline networks

NVIDIA's Typhoon (hdEmbree, `typhoon/main` of NVIDIA-Omniverse/OpenUSD) has no
`UsdVolVolume` support at all: its volumes are **materials**. A `Material`'s
`volume` terminal is the participating medium inside the geometry it is bound to
(`EvalSurfaceVolumeMaterial` / `ApplyVolumeToSurfaceClosure` in
`MaterialXCpp/surfaceShaderUtils.h`), and crust follows it:

- **The VDF tree** (`crust-mtlx/src/bsdf.rs`, `vdf_tree` / `volume_shader`):
  `anisotropic_vdf` and `absorption_vdf` leaves; `mix`, `add` and `multiply`
  over VDFs; `volume` (its `vdf` input), `mix` of two `volumeshader`s, and
  `volumematerial`. The combinators are Typhoon's (`_EvalMixVdf`,
  `_AddVdfClosures`, `_MultiplyVdfClosure`): coefficients combine linearly and
  the anisotropy is the mixture weighted by each side's scattering lane sum, so
  an absorber never drags `g` toward its own. They are **program ops**
  (`sum_volumes`: `Mul`, `Add`, `DotProduct`, and `Div`, whose zero divisor
  answers 0 — no scattering, no anisotropy), so a textured VDF works, the
  optimiser folds a constant one to three constants, and the JIT needed nothing
  new. A vacuum branch is zeros. `volume`'s `edf` is reported, not rendered.
- **Under a surface the terminal replaces the interior** the surface described
  through its own `transmission_*` inputs (`flatten_volume` overwrites
  `Closures::volume`): it is the authored answer to "what is inside". It still
  only matters below a thick, transmitting surface — `ResolvedClosure`'s rule,
  now shared through `closure::volume_medium` — so a thin-walled or opaque
  surface ignores it, as `ApplyVolumeToSurfaceClosure` does.
- **With no surface the material is a medium boundary**
  (`Compiled::volume_only` → `Material::is_medium_boundary`): Typhoon's
  `MakeVolumeSurfaceClosure`, an opacity-0 closure flagged `isVolumeBoundary`.
  It is not a cutout — a cutout changes no medium (§ opacity above) and
  `pass_cutouts` would step over it — so it is its own trait method, counted by
  the world at commit (`World::has_medium_boundaries`), and the integrator
  crosses it (rendering record § Medium boundaries). `boundary_medium` runs the
  whole program; a volume-only material's program is its volume network alone.
- **Inline networks** (`scene/usd_import/mtlx_network.rs`). Typhoon receives
  every material as a Hydra network of `ND_*` nodes, which is also how
  `usdMtlx` composes a document into a stage. Each `Shader` becomes the `Node`
  the XML parser would have produced: the category and type from the nodedef
  name (`nodedef_category`: trailing signature tokens are popped — `_100`
  versions, `color3`, `vdfC` — but a `_bsdf` / `_edf` / `_vdf` only after a
  combinator, since on a leaf it *is* the category: `ND_mix_vdf` is `mix`,
  `ND_anisotropic_vdf` is `anisotropic_vdf`), and each literal formatted as the
  text of a `value` attribute and read back through `parse_literal`, so a value
  means exactly what it would in a `.mtlx`. Connections follow `NodeGraph`
  outputs and `Material` / `NodeGraph` interface inputs to a shader output or a
  value (32 hops, then cut and reported); an unconnected, unauthored interface
  input leaves the node's input unauthored, i.e. its nodedef default. `asset`
  inputs are anchored on their authoring layer (`attribute_asset_path`) and
  written absolute, so the texture loader's join resolves them to themselves.
  The node's name is its prim path, which is unique by construction.
- **Precedence.** A MaterialX `volume` terminal is read first, with the
  `mtlx` surface (else a universal `ND_*` surface) of the same material. If the
  surface is something else — a `UsdPreviewSurface` beside a MaterialX volume
  — the volume is **ignored with a warning** rather than the preview surface
  dropped: that pairing is a viewer fallback, and keeping the surface is what
  the stage rendered as before.

## Known gaps: MaterialX

- **Volume terminals are homogeneous per hit, and one at a time.** The medium
  is evaluated once where a ray enters (a textured VDF picks its value at the
  entry point and carries it), so a VDF cannot vary *through* the volume; for
  that, use a volume region. Nesting and overlap follow Typhoon's single
  `MediumState` owner (rendering record), not its planned priority-based
  interior list (`doc/plan-volume-ids.md` there). `volume`'s `edf` is not
  rendered. The free flight is crust's max-channel majorant, not Typhoon's
  Chiang channel MIS, so a strongly chromatic interior is noisier than
  Typhoon's (unbiased either way). A `PreviewSurface` beside a MaterialX
  volume drops the volume (above).
- **Inline networks read the nodedef by name.** A shader id the import's node
  library defines is read through its nodedef (category, instance type) first;
  any other id that breaks the `ND_<category>_<signature>` pattern is translated
  as whatever category that parse gives, which the compiler then reports as
  unsupported. Nodegraph-scoped names are flattened: every node is addressed by
  its prim path.
- **A displacement reads no primvar.** It is evaluated at the import's
  vertices, before any `PrimvarTable` exists, so a `geompropvalue` in a
  `displacementshader` graph gives its default (`mtlx.incomplete`). Reading one
  would mean interpolating the mesh's shading primvars at each displaced vertex
  into `VertexCtx`.

- **MaterialX's reference is BSDL, not its GLSL.** The leaves are BSDL's
  `mtx::` lobes and the layering testrender's, so a document can render
  differently in a GLSL viewer (MaterialX Viewer, Typhoon). The two differ
  most in `layer` (GLSL hides a base under any opaque leaf whatever its
  weight; testrender lets `1 − w` through), in the energy compensation of
  refracting dielectrics, and in a `zeltner` sheen, which BSDL reads at
  `max(√roughness, 0.02)` where GLSL's fits read `roughness`. Pattern nodes
  follow genosl for the same reason, and one differs from GLSL in kind:
  `transformmatrix` of a `vector3` by a `matrix44` is a direction (no
  translation), where GLSL translates it as a point. A node library that
  places a projection that way, authored against a GLSL viewer, needs the
  point lifted to a `vector4` with `w = 1` to translate in crust.
- **A rough refracting dielectric's value is not its sampler's.** BSDL
  renormalises a refracting lobe by `DielectricBothFront` / `BothBack`, baked
  from its bounded-VNDF sampler (`use_bvn_refraction`, on by default), which
  never draws a micro-normal that cannot reflect, though it could refract. So
  the lobe's value integrates above 1 — 1.005 at roughness 0.3, 1.04 at 0.5,
  1.16 at 0.7 and 1.75 at 1, head on, for a clear IOR-1.5 `RT` dielectric —
  while its sampled weights average to 1, and BSDL's `eval` still answers,
  with a density, for the refracted directions its sampler never reaches.
  crust ports both halves exactly (#378, the user's choice over restricting
  `eval` to the cap or baking the tables with BVN off). BSDF sampling alone
  conserves energy; MIS with a light does not quite: light samples reach
  those directions and are weighted against a BSDF density that never fires,
  so a rough clear glass sphere in a white furnace whose emitter is also a
  light returns 1.012–1.023 of it at roughness 1, 1.007–1.017 at 0.5 —
  testrender's own inconsistency, and down from 2.0 before the port. Measured
  directly, the density BSDL's `eval` claims, integrated over the sphere at
  θo = 0.8, exceeds what its sampler lands by 0.031 at roughness 0.5, 0.125
  at 0.7 and 0.614 at 1 (`bsdl_refraction_claims_more_density_than_it_samples`
  pins those). Every other leaf's claimed density accounts for exactly the
  draws that land, within 0.015 (`no_leaf_claims_more_density_than_it_samples`):
  `every_sample_agrees_with_eval` checks the density only where samples land,
  so it cannot see a density claimed where none do.
- **A reflection-only top under a back face filters as crust's, not BSDL's.**
  BSDL's `DielectricLobe` (and `SchlickLobe`) reads its layering filter from
  `DielectricReflFront` on both faces, while its value on a back face inverts
  the IOR and totally reflects past the critical angle. Under testrender's
  `evaluate_layer_opacity` a coat seen from inside a glass therefore reflects
  everything *and* lets about 95% of the glass beneath it through, and the
  glass, seen from inside too, totally reflects as well: up to ≈ 2× per
  bounce, compounding along paths trapped inside. The OpenPBR Shader
  Playground's coated mason jar (`masonJarGlass`: coat 1.0 over
  transmission) took `renderCam_main` from a mean luminance of 0.71 to 3.76
  at 1024 spp, single paths reaching 1.1e7; a coated `open_pbr` glass cube
  returned 4.03 of a white furnace, a raw `layer(dielectric R, dielectric
  RT)` 7.03. (crust's GLSL leaves had the same flaw, more weakly: 3.30 and
  6.89.) crust reads the filter under a back face from `DielectricReflBack`
  instead (above, "Energy tables"): the same cube returns 0.13 and the raw
  layer 0.94 at depth 12, the Playground region its base's median with
  none of its fireflies. The oracle excuses it as `back-face-reflection-filter`;
  `a_coat_over_glass_returns_no_more_than_it_receives_from_inside` pins
  what is left, the table's interpolation within a degree or two past the
  critical angle: at most 1.058 per bounce (IOR 2.4, roughness 0.05). A
  perfect cube, which traps rays at the same angles forever, still gains
  about 0.2% per bounce there: the coated cube passes 1 at depth 256 (0.67
  at 64; the default is 32). Two things stay as testrender has them: the
  layer order is not reversed under a back face — the coat's own IOR against
  the outside sets its critical angle, though from inside the ray meets the
  base first (38.7° rather than 41.8° for a 1.6 coat over 1.5 glass) — and
  `open_pbr_surface`'s coat darkening, which its nodegraph applies to the
  whole base, transmission included (0.706 of a clear glass under a clear
  coat head on).
- **Selection is crust's.** A leaf's value, albedo, filter, sampling and
  density are BSDL's; the choice among leaves is `∝ lum(w)·lum(Ê)` with a
  0.02 floor rather than testrender's `CompositeBSDF::prepare`, and a
  conductor is picked by its compensated albedo rather than BSDL's fit. The
  mixture pdf agrees with the mixture's samples
  (`every_sample_agrees_with_eval`).
- **No path regularisation, no visible-normal bending.** testrender widens a
  lobe's roughness after a rough bounce (`regularize_roughness`), which is
  biased; crust does not, so a smooth conductor seen through a diffuse bounce
  is as sharp as seen directly. BSDL bends a shading normal the view cannot
  see (`visible_normal`); crust lifts the view to 1e-6 above the leaf's
  plane instead.
- **Thin film, F82 and hair are not BSDL's.** BSDL's lobes declare
  `thinfilm_*` and never apply it; crust keeps MaterialX's GLSL Airy film on
  the dielectric and conductor (excused by the oracle's `thin-film` rule, up to
  0.611) and the GLSL F82 tint on generalized Schlick (BSDL has no
  `color82`; the oracle authors it white). `chiang_hair_bsdf` stays the GLSL
  fibre (genosl has a placeholder) and, as testrender takes every lobe it has
  no filter for, is opaque in a layer's top. `subsurface_bsdf` stays a random
  walk, where testrender renders a Lambertian.
- **A sheen is black on a back face**, as BSDL's `SheenLobe` shades only the
  front, and lets everything beneath it through there.
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
- **Hair: metre-scale grooms lose strand-to-strand light.** The tracer's
  absolute `t_min = 0.001` exceeds a real hair's ~7e-5 m diameter in a stage
  authored in metres, so strands closer than 1 mm do not shadow or scatter into
  each other. In centimetres (the USD default), it is 10 µm. A scale-aware
  epsilon is its own change.
- **Hair: a ray can still meet its own strand on the inside of a sharp bend.**
  The buried half of an end cap, and a joint's cap for a ray starting at that
  joint, are skipped (intersection-kernel record). Where two segments' *bodies*
  overlap on the inside of a bend, though, a ray crossing the strand can enter
  the neighbour's body while still inside the strand. Within a cubic span the
  subdivision keeps pieces nearly straight, so this is a sliver. A linear
  curve with sharp corners is where it would show.
- **Hair: far-field only, one fibre at a time.** There is no dual scattering or
  other multiple-scattering approximation, so a dense groom converges by
  tracing bounces, and there is no near-field azimuthal term for a tube wider
  than the pixel. Ribbons (`BasisCurves` `normals`) are not read.
- **Primvars: not inherited, not `faceVarying` on curves, not beside Ptex.**
  `geompropvalue` reads a primvar by name on a curve or a mesh hit (§ MaterialX,
  "Curve properties and primvars", "Mesh primvars"). A primvar authored on an
  ancestor or a `PointInstancer` is not read (the AOVs' gap, `aovs` record). A
  curve's `faceVarying` primvar is refused, and an implicit shape reads `constant`
  ones only. A mesh whose material reads a Ptex texture as well carries its face,
  not its primvars (`HitPart` holds one), so its primvars give the `default`, with
  `mesh.unsupported_primvar`: holding both would grow `HitRecord` past 96 bytes.
  Point, vector and normal primvars are read in object space. `curve:v` is the
  curve's parameter, not arc length: on a strand with unevenly spaced control
  points a `v` ramp follows the parameter, as USD's `vertex` primvars do.
- **Not applied.** glTF `occlusion` is reported, not implemented: a path
  tracer computes its own. Dispersion is ignored where MaterialX's own graphs
  ignore it, and reported.
- **Cutout limits.** The shadow side walks every blocked shadow ray hit by hit
  once a world holds any cutout, since the kernel's any-hit query has no
  filter to skip one (an Embree-style any-hit callback is the fix, in
  `crust-rt`); an unblocked ray keeps the fast answer. The opacity is asked
  with the shadow ray's direction there and the path's on the bounce side, so
  a graph whose opacity reads `viewdirection` sees opposite views on the two
  sides. The learned light cache's training shadow rays see cutouts, but
  its training paths stop at one as if it were present (a guide only, never
  bias). A walk's exit and the walk itself ignore the entered object's own
  opacity. A segment follows at most 256 cutouts, on both sides. Opacity is
  point-sampled, so a distant cutout reads its alpha map's finest level —
  the price of NEE and the bounce side agreeing (rendering record).
- **Layering is testrender's, not physical.** `O_top(ωo)` only
  (non-reciprocal, single-scattering): NEE and bounce evaluate the same
  `f(ωo, ωi)`, so MIS is consistent, but a bidirectional method would need
  care. A nested layer in a top sums its opacities rather than multiplying
  its throughputs. A reflection layer over a `T` dielectric pays Fresnel
  twice, so a smooth `standard_surface` glass is ~8% dark in a white furnace,
  as it is in MaterialX GLSL, testrender and Typhoon.
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
  The same parameters authored both ways shade differently, and pass shadow rays
  differently: each path's straight transmittance `T` is its own BSDF's (the window
  model's `(1−R)/(1+R)` natively, one interface's `1 − F` in the closure), which is
  what keeps NEE and the bounce side in agreement on each.
- **Capacity.** A tree with more than 8 live-reachable leaves is refused, whole.
- **Pattern nodes.** Only document-scope and `<nodegraph>` nodes are compiled.
  A custom node is first replaced by its `<nodegraph nodedef=…>` implementation
  (`Doc::expand`, below); one with no graph implementation (an
  `<implementation>` naming source code only) gets that input at a constant
  (reported). A plain `<nodegraph>`'s own interface (`<input>` children read
  through `interfacename`) is still not wired: such an input is a zero. The pattern operators the Material
  Fidelity suite still lacks are listed in `docs/material_fidelity.md`. No
  `<look>` / `<materialassign>`: bindings come from USD. One parser quirk
  predates the tree: two nodes with the same name in one scope collide.
- **The OSL oracle covers the pattern nodes only** (§ MaterialX, "Node
  semantics are checked"): `image` / `tiledimage` (a texture file), `texcoord`,
  `normal`, `position` and `viewdirection` (the shading globals) are not in it,
  nor are `hsvadjust`, `colorcorrect` and `heighttonormal` (not yet in the
  script's `CATEGORIES`), nor `geompropvalue` (it reads the host's primvars),
  nor anything the closure and surface builders
  (`bsdf.rs`, `surface/`) emit, which the pattern ops they lower to are
  checked through. BSDF nodes are the BSDL oracle's (§ MaterialX, "The BSDL
  oracle"). Matrix signatures are not generated because crust has no
  such values, and `integer` / `boolean` ones only for the conditionals and
  random nodes (the conditionals' `...B` variants, with `boolean` values, not
  at all). `cellnoise2d`'s `texcoord` is always authored, through a
  connection like `normalmap`'s frame: unauthored, crust reads the host's
  chart and `testshade` its own `u, v`.
- **Stdlib nodegraphs lowered by hand.** `colorcorrect` (`color3`) is compiled to
  `NG_colorcorrect_color3`'s own chain (`hsvadjust` → `saturate` → `range` gamma →
  lift → gain → `contrast` → exposure) out of existing ops plus `HsvAdjust`, with a
  stage whose parameter folds to its identity left out. The stage order is pinned
  by a test, since swapping any two still renders plausibly. `colorcorrect` on
  `color4` is refused and reported, because there is no `combine4` to route alpha
  around the chain.
- **Curve properties and primvars** (`geompropvalue`). `ShadeCtx::curve`
  (`CurveCtx`: `v`, `index`, `random` and the hit's primvar values) is `Some` on a
  curve hit whose geometry carries a strand table (usd-scene-import record,
  "Curve attributes"), `None` anywhere else. `geompropvalue` compiles `st` to the
  chart, `curve:v` / `curve:index` / `curve:random` to `Op::Curve`, and any other
  name to `Op::Primvar` on a channel of `Program::primvars` (the names, in the order
  compilation met them, which the host decodes and supplies in that order); each
  falls back to the node's `default`, as MaterialX specifies for an absent
  property, and converts as `convert` does (a `float` broadcasts). An unknown
  `curve:` name is reported. An image whose `texcoord` is a `geompropvalue` of a
  named primvar keeps reading the chart (below), so this changed no mesh image.
  `ShadeCtx` borrows the primvar values, hence its lifetime; the JIT reads the
  context only through the interpreter's ops, which it hands both new ops to.
  **Cost.** Measured against the commit before (callgrind, `-s 2`, one thread):
  cornellbox −0.24% and `samples/curves.usda` −0.16% instructions, and every
  sample scene bit-identical. The record a hit carries for its strand costs
  nothing per hit once it shares `HitRecord::part` with the Ptex face (beside it,
  +0.47%: `WorldHit` passed 128 bytes and every move became a `memcpy`), and the
  no-table fast path in `World::intersect` saves more than the strand test adds
  (usd-scene-import record, "Curve attributes").
- **Mesh primvars.** The values a program's `Op::Primvar` reads are
  `ShadeCtx::primvars`, not the strand's (they were `CurveCtx::primvars`), so any
  hit supplies them. A mesh supplies them through `HitPart::Mesh` (`MeshHit`: its
  `PrimvarTable`, the kernel's triangle and barycentrics, the winding undone),
  which `World::intersect` builds for a geometry whose table holds a channel of
  `PrimvarTable::shading` — the material's `Material::primvars`, in
  `Program::primvars` order, each with its authored arity, beside the AOVs'
  `channels` in the same table (usd-scene-import record, "Mesh primvars").
  `materialx::run` looks the values up into the per-thread buffer only for a
  program that reads one, on a hit that carries some, as for a curve. The opacity
  slice shares the surface program's `primvars` list (`Program::optimize` keeps
  it), so one table serves both. The displacement slice keeps the list too, but
  runs at the import's vertices, which supply none (`MtlxDisplacement::eval_with`
  passes `&[]`): a `geompropvalue` it reads gives its default, reported per material
  (`mtlx.incomplete`) when the sliced program `reads_primvar` — asked of the
  optimised slice whatever the optimiser setting, or a surface read would count. The variant fits
  in `HitPart` beside the strand only because `MeshHit` is `repr(C, packed(4))`:
  20 bytes, where aligned to eight it pads to 24, leaves the tag no room and made
  the record 112 bytes (the record stays 96, `a_world_hit_stays_an_inline_copy`).
  The Ptex face wins where both exist.
  `Signature` carries a non-curve slot's primvar names, so a material edit that
  changes them re-imports. A mesh whose material reads none keeps the plain path:
  every sample scene, which authors no such material, is unchanged.
- **Channel, compositing, `switch`, `place2d` and `transformmatrix`.** What a
  studio node library is typically built from, each as MaterialX 1.39 defines it and out of
  existing ops only (the JIT needed nothing new). `separate2/3/4` is one
  `Extract` per output (`NG_separate*`). `switch` is `NG_switch_*`'s chain of
  `ifgreater(k, which, in<k>, …)`: `in1` below a `which` of 1, … `in10` below
  10, **zero** from 10 on (not `in10`); a `which` that folds selects one input
  and compiles nothing else, so a layered texture's unused blend modes load no
  texture. `over`, `in`, `out`, `difference` and `premult` are their genosl
  `sourcecode` / `mx_premult_color4`, in the same order of operations.
  `place2d` is `NG_place2d_vector2` (SRT or TRS through its own `switch`),
  `rotate2d` inlined as `mx_rotate_vector2`; its `rotate` is read as a scalar,
  since an unauthored float default is widened to the node's `vector2`.
  `transformmatrix` takes its matrix only as a **compile-time constant** (a
  literal, a `constant` node's value through any chain of them, or the
  identity): `Val` holds four lanes and a projection's matrix is in practice a
  constant, so each row becomes a constant and the product is
  `(x·r0 + y·r1) + z·r2`. Any other matrix is reported and the input
  passes through. **genosl, not GLSL**, as everywhere else: a `vector3` by a
  `matrix44` is `transform(matrix, vector)`, a direction, so the last row is
  not read and nothing translates; only a `vector2` by a `matrix33`
  (`mx_transformmatrix_vector2M3`, `point(x, y, 1)`) adds its last row.
  MaterialX's GLSL (`mx_transformmatrix_vector3M4`) multiplies `(v, 1)` and
  translates; it was followed at first, and dropped to keep one reference
  (Known gaps). The matrix is row-major with the
  translation in the last row, which is USD's `Gf` order too, so an inline
  `matrix4d` is its 16 values as authored. `position` reads world space; any
  other `space` is reported (it would silently read world otherwise). Pinned
  by `crust-mtlx/tests/stdlib_nodes.rs` against the stdlib's expressions;
  `scripts/osl_oracle.py` lists them all (it generates `transformmatrix`'s
  matrix literals too), and their fixture cases wait for a regeneration on an
  exact-math OSL build.
- **HSV, conditionals and random nodes.** `rgbtohsv` / `hsvtorgb` are ops over
  the `hsvadjust` transcriptions of OSL's `transformc` (Foley & van Dam); a
  `color4` keeps its alpha. `ifgreater` / `ifgreatereq` / `ifequal` are one
  `Select` op whose width is the wider branch's whichever is taken, so the
  JIT knows it without evaluating the comparison (a mistyped document with a
  `vector2` and a `color3` branch is where that shows). The unauthored
  `value1` is 1, but 0 for `ifequal`, except `ND_ifequal_boolean`'s; nothing
  but the output type tells that signature from `ND_ifequal_booleanB`, so a
  boolean `ifequal` authoring a `boolean` value takes the latter's (0).
  `randomfloat` and `randomcolor` are lowered to their stdlib nodegraphs
  (`NG_randomfloat_*`: `cellnoise2d` of `(in · 4096, seed)`, `range`d and
  clamped; `NG_randomcolor_float`: three of those under the seeds
  `ceil(seed + 413.3)`, `+ 1522.4`, `+ 1813.8`, then `hsvtorgb`), leaving out
  `range`'s first remap and gamma stage, exact identities at the nodegraph's
  constants. `cellnoise2d` is OSL's `cellnoise(x, y)`, lookup3's `final` over
  the floored cell (seed `0xdeadbeef + 21`) scaled by 2⁻³², transcribed from
  MaterialX's `mx_noise.glsl`; the oracle matches it bit for bit, so a look
  draws the same random values here as in any MaterialX renderer. The JIT
  hands all four ops to the interpreter.
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
  `None`, which keeps the lookup on the JIT's inline path, and so does a
  `geompropvalue` naming any other primvar (a second UV set): it reads the
  chart the host chose for the geometry, as it did while `geompropvalue` did
  not compile, so a mesh renders as before. Only a `curve:` property, or a
  subgraph over one, is a coordinate of its own — with or without a `primvars:`
  prefix, which `geomprop_name` strips for the image's test as for the op. A coordinate
  subgraph that hits an unsupported node (`place2d`) also
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
  That is also why `MtlxMaterial` answers through `emitted_at`: the light list reads
  the hit-free `emitted()`, and the two must agree for anything it samples.
  **The constant fold.** Where the emission cannot vary, it is known at compile time:
  after the closure tree is built, `crust-mtlx` folds each EDF term's `weight` and
  `color` with `Compiler::fold` (literals and pure operators over them — no texture,
  primvar, curve property or shading-point input), and accepts a
  `generalized_schlick_edf` only when its `color0` and `color90` fold equal (a neutral
  falloff, up to its tint). If every term folds, `Compiled::constant_emission` is the
  folded slots run through `Closures::emission` itself — the one function the runtime
  evaluates emission with, so the per-channel sanitising and the order of the sum are
  the runtime's — and `MtlxMaterial::emitted` / `uniform_emission` report it (zero for
  a surface with no EDF); otherwise `emitted()` is zero and `uniform_emission` `None`.
  `fold` evaluates with the interpreter's own operators and the optimiser and the JIT
  keep every slot's bits, so the constant is what each hit computes. One trap, met
  writing it: glam's `lerp` is `a·(1−s) + b·s`, which for `a = b` rounds to a value
  that moves with `s` by an ulp, so a falloff with equal colours was not constant in
  the bits. `Closures::emission` takes `color0` as is when the two are equal — the
  only change to a rendered value, and only for a hand-authored equal-coloured
  falloff (no surface node builds one). Rendering is otherwise unchanged: the light
  list builds nothing from a material's emission unless its mesh applies
  `MeshLightAPI`.
