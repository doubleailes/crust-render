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
  `layer`/`mix` flattened to weighted `Lobe`s, and `edf` to `Emission` terms), and `compile()` running all three for one
  material node. It names the one thing it asks of its host — `Texture`, a
  `(u, v) → RGBA` sampler — and crust-core re-exports that trait as its own
  `Texture2D`, exactly as it adopts `crust_rt::Geometry`. What a renderer does
  with the lobes is not decided here; crust's OpenPBR pooling is in crust-core.

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
  The pattern materials share their `Material` impl through `PatternMaterial`
  (`material/pattern.rs`), which states the order once. Four implementations: **`OpenPBR`**,
  the single übershader for all surfaces (with `diffuse`/`metal`/`glass`/`glossy` preset
  constructors used by `world.rs` and the USD fallback), **`Emissive`**, a pure
  emitter with no geometry knowledge, and **`MtlxMaterial`**
  (`material/materialx.rs`), which evaluates a MaterialX graph per shading point and
  *delegates* the BSDF to the `OpenPBR` it reduces to — so sampling, MIS
  densities and energy compensation stay in one place — and **`PreviewSurface`**
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
    cutout mode and stays on the unimplemented `geometry_opacity`. A textured `ior`
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

- **MaterialX** (`crates/crust-mtlx`, adapter in `material/materialx.rs`) — `.mtlx` look-dev graphs, read directly.
  USD's own answer is a file-format plugin that composes a `.mtlx` into the
  stage as `UsdShade` prims; **openusd ships none**, so a `Material` prim whose
  only opinion is `references = @foo.mtlx@</MaterialX/Materials/name>` composes
  to a prim with a `Material` type name and *nothing inside it*. Every schema
  query then fails and the surface falls back to grey — which is what the two
  DPEL assets (MaterialX Teapot, MaterialX Lion) did on import. The reader is
  the standalone `crust-mtlx` crate — `parse.rs` (XML → a flat,
  name-addressable graph), `value.rs` (the one runtime value), `eval.rs` (the
  graph compiled to a slot-indexed program), `bsdf.rs` (the BSDF tree
  flattened to weighted lobes) — and crust-core's `material/materialx.rs` is
  the adapter: `MtlxMaterial` (impl `Material`), `reduce()` pooling the lobes
  onto OpenPBR, and the importer-facing `load()`.
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
  - **The BSDF reduction is the lossy part, and deliberately so.** MaterialX
    assembles a look from *standalone* BSDF nodes (`oren_nayar_diffuse_bsdf`,
    `dielectric_bsdf`, `conductor_bsdf`, `sheen_bsdf`) glued with `layer` and
    `mix`; crust has one übershader with a fixed lobe stack. At **compile
    time** the tree is flattened — a `mix(fg, bg, m)` sends `m` down one branch
    and `1 − m` down the other, a `layer` sends full weight down both — so each
    leaf arrives with a weight that is a product of mask expressions, compiled
    into the same program. At **shading time** the leaves pool by kind and the
    pools normalise into OpenPBR parameters (diffuse+metal → `base_color` and
    `base_metalness` as their ratio; dielectric+conductor → one joint
    `specular_roughness`; sheen → fuzz). A leaf's own `weight` input multiplies
    its path weight; a branch that is a *literal* zero — either a leaf whose
    `weight` is 0, the "transmission dummy" both assets use as a mix's null
    branch, or a `multiply(BSDF, 0)` — is pruned at flatten time rather than
    carried at weight 0. Both forms have to be pruned, and for the structural
    reason rather than the numeric one: `reduce` drops a zero-weight lobe
    anyway, but the `layer` promotion below counts it as a specular interface
    first. **Two specular lobes.** The flattening
    keeps one structural fact: a dielectric that is the `top` of a `layer`
    whose base already carries a specular (another dielectric, a conductor)
    arrives as `LobeKind::Coat` and pools onto OpenPBR's coat lobe with its own
    roughness and IOR, while a dielectric directly over a diffuse stays the
    base specular (that is how OpenPBR's own dielectric base is built). So a
    varnish over a conductor keeps its varnish — the single pool used to lose
    it, since a metal base zeroes the dielectric Fresnel term — and the DPEL
    **lion**, whose glaze sits over a `mix(conductor, diffuse)`, reduces to
    `coat_weight = 1`. The **teapot** does not: its glaze sits over a plain
    `oren_nayar_diffuse_bsdf`, so both its glazes stay the base specular and it
    reduces to `coat_weight = 0` at every point — correctly, and worth knowing
    before blaming the coat for anything the teapot does. The decision is
    by tree shape, never by evaluated weight (a per-point flip would draw a
    seam along a mask's zero contour), which is why a literal-zero branch has
    to be pruned before the layer looks at its base. What this still cannot
    represent: three or more stacked dielectrics pool their upper ones into one
    coat roughness, and a coat dielectric's `tint` is ignored (MaterialX tints
    the coat's reflection; OpenPBR's `coat_color` is substrate absorption). A
    `conductor_bsdf` fed by `artistic_ior` is reduced back to a reflectivity
    colour through the exact inverse of Gulbrandsen's formula, so a metal
    authored either way lands on the same OpenPBR metal lobe.
  - **Two unit conversions the reduction owes MaterialX**, both easy to
    re-break. First, a MaterialX specular BSDF's `roughness` input is the GGX
    **alpha**, not a perceptual roughness — that is what `roughness_anisotropy`
    exists to produce, and the DPEL teapot's `.mtlx` makes it explicit with
    `power` nodes named `desquare_roughness_*`. crust's OpenPBR roughness is
    perceptual and gets squared again, so `reduce()` takes the square root
    once, *after* pooling (`√` is concave, so pooling in alpha and converting
    at the end is both cheaper and the better stand-in for a GGX mixture). Only
    the GGX lobes convert: `oren_nayar_diffuse_bsdf`'s roughness is an
    Oren-Nayar sigma and `sheen_bsdf`'s drives the Charlie NDF directly.
    Second, a MaterialX coat arrives with `coat_darkening = 0`, set on the base
    in `load()`: MaterialX's `layer` is single-scattering, so imposing
    OpenPBR's coat-underside TIR bounce series would darken a substrate the
    source material never darkened (on the lion, by a factor of 0.52).
    Relatedly, the dielectric and conductor pools carry **independent
    coverage**, and keeping them independent takes both halves of the seam.
    `base_metalness` is the conductor's share of the substrate and
    `specular_weight` is the dielectric interface's share of the *dielectric
    base* (not of the whole surface), so `eval_specular` reconstructs the two as
    `base_metalness` and `(1 − base_metalness)·specular_weight` and each comes
    back as authored. The metal lobe therefore does **not** read
    `specular_weight` — that parameter belongs to the dielectric base, and a
    metal has no dielectric interface to weigh. While it scaled both halves, one
    pool's coverage multiplied the other: a conductor at 0.25 under a glaze at
    0.75 rendered its metal at 0.25 × 0.75. Two consequences worth knowing: the
    dielectric base's coverage is `max(diffuse + sss, dielectric)` rather than a
    sum, because MaterialX's `layer` puts an interface *on top of* a substrate
    rather than beside it — and a conductor mixed with a *bare* dielectric used
    to pin `base_metalness` to 1 and lose the dielectric outright. A graph with
    no base `dielectric_bsdf` at all — the lion, whose glazes are both coats, and
    the teapot's metal — now reduces to `specular_weight = 0`, which is correct:
    it has no base specular, and it used to be given one.
  - **The EDF half is a second list, not a seventh lobe kind.** MaterialX's
    `<surface>` has an `edf` input beside its `bsdf`, and the flatten now walks
    both — the same `layer`/`mix`/`multiply`/`add` algebra, since a mix
    partitions radiance exactly as it partitions reflectance. Three things are
    load-bearing. The walk carries a **closure domain**, because
    `closure_input` gates a branch on its declared type and an EDF-typed `mix`
    declares `type="EDF"`: asking "is this a BSDF?" in the emission tree
    resolves both branches to `None` and the emission vanishes silently. The
    terms land in `Flattened::emission` rather than as a `LobeKind`, which
    makes the `layer` arm's `base_has_specular` scan *structurally* unable to
    see an emitter — the strongest form of "emission does not disturb the coat
    promotion" — and keeps the two algebras apart: BSDF pools take a weighted
    **mean** (two diffuse leaves are one surface shared between them) while
    emission **sums** (two emitters are twice the light). And neither the
    weight nor the colour is clamped above: `multiply(uniform_edf, 100)` is how
    a document authors a bright emitter, and this is the one shading input for
    which a value above 1.0 is meaningful rather than an authoring error.
    **The weight is not a scalar**, and reading it as one is a quiet, severe
    bug: MaterialX declares `ND_multiply_edfC`, a `multiply` on an EDF by a
    `color3`, so a tinted emitter is ordinary authoring and `Mul` promotes arity
    into the weight slot per channel. Taking lane 0 alone turned a weight of
    `(0, 0.6, 0.9)` into a **black** emitter and `(1, 0.5, 0.2)` into a neutral
    one at full strength. `reduce()` reads both factors with `Val::rgb`, which
    broadcasts an arity-1 value so the `float` case is unchanged, and sanitises
    each factor *before* the product — clamping the product instead would let
    two negative channels multiply into positive light. Non-finite is refused
    per channel here where the lobe loop drops the whole lobe, and that
    asymmetry is structural: emission sums, so a zeroed channel contaminates
    nothing, while a lobe's weight is a *divisor* (`Pool::w` normalises every
    colour in its pool). The **BSDF** lobe weight still reads lane 0, and
    deliberately: `ND_multiply_bsdfC` exists too, but every OpenPBR coverage
    field it feeds (`base_weight`, `specular_weight`, `coat_weight`,
    `fuzz_weight`, `subsurface_weight`) is an `f32`, so a colour there has
    nowhere to go short of folding the tint into the lobe's own colour.
    `reduce()` factors the summed radiance by its **peak channel**, so
    `emission_color` is always a chromaticity in `[0,1]³` and the range lives
    in `emission_luminance`; factoring by Rec.709 luminance instead — what
    OpenPBR's spec means by nits — would send a saturated emitter's *colour*
    above one, since (0, 0, 8) has luminance 0.43 and would store (0, 0, 18.6).
    A `surface` with an `edf` and no `bsdf` also has its `base_weight` zeroed,
    or a pure emitter would keep OpenPBR's default grey diffuse underneath it.
  - **The shipped `.mtlx` files are not well-formed XML.** They address a UDIM
    set as `value="Albedo.<UDIM>.png"` — a bare `<` inside an attribute value,
    which XML forbids. MaterialX's own reader is PugiXML, which accepts it;
    `roxmltree` rejects the whole document with an `InvalidChar`. `parse.rs`
    escapes the two specified tokens first, so this is not "the UDIM path is
    wrong" but "no material at all" if it is ever removed.
  - **Verified in numbers, not by eye** — `examples/mtlx_shade` prints the
    OpenPBR parameters a graph reduces to at a named `(u, v)`. This is what
    settled the teapot: the render looked washed out against the reference, and
    the probe showed the graph producing exactly the right deep blue
    (0.005, 0.024, 0.074) on the body against light ribs — so the fault was the
    sample scene's exposure, not the material. A mis-decoded albedo is a
    plausible pastel and a mask read at the wrong colour space is a plausible
    blend; comparing renders settles nothing. It prints an `emission` column
    too — the *product* `emission_color * emission_luminance`, since OpenPBR
    only ever multiplies the two back together and either field alone would
    mislead — and it reaches its textures through `FileAssets` rather than
    `UvTexture` directly, so `CRUST_TEX_STREAM` is honoured. That last part is
    not tidying: the preloaded decoder narrows to 8 bits, so an HDR emission
    texture probed through it reads 1.0 whatever the file holds, and a probe
    that cannot see the range is worse than no probe because it answers
    confidently.
  - Sample scenes: `samples/materialx_basic.usda` + `.mtlx` (self-contained, 20
    KiB of textures, what `tests/usd_scene.rs` runs against; its `mtlx_lacquer`
    is the two-dielectric stack that must reduce to base specular + coat),
    `samples/materialx_emissive.usda` + `.mtlx` (the EDF fixture: a constant
    `multiply(uniform_edf, 12)` and a pure emitter driven by
    `textures/mtlx_emission.hdr`, whose bright cells sit at (16, 9, 3) —
    deliberately a **separate** document, because three assertions pin
    `materialx_basic` at exactly three materials and three textures), and the two shot
    layers for the DPEL assets, which are gitignored and must be downloaded:
    `samples/materialx_teapot.usda` and `samples/materialx_lion.usda`, plus
    `samples/materialx_showcase.usda` composing both after the `overview.png`
    the assets ship with (the lion is scaled to 0.52 there: both are ~0.26 m
    tall as authored, yet the overview shows the lion at ~60% of the teapot's
    height while standing nearer the camera, so it is scaled, not pushed back;
    the seamless sweep is a near-white floor under a uniform dome, the two
    meeting at the horizon because a Lambertian floor of albedo a under
    radiance L reflects a·L). The lion
    is the larger graph (140 ops, 8 lobes, 7 textures over 6 UDIM tiles, 1.06 M
    baked triangles) and the one that layers a `sheen_bsdf`, so it is what
    exercises the fuzz pool; both import with no unsupported nodes.

## Known gaps: MaterialX

- **MaterialX caveats.** The BSDF reduction projects a layered MaterialX stack
  onto one OpenPBR lobe set. Two stacked dielectrics survive (the upper one is
  the coat), but a *third* is averaged into the coat's roughness, a coat's
  `tint` is dropped, and a glaze over a base specular whose mask is zero at
  some point still shades there as coat-over-diffuse (the promotion is
  structural, by design). Anything past two specular interfaces needs a
  layered BSDF material, not a different reduction. `subsurface_bsdf` maps to OpenPBR's
  subsurface weight but not its radius; `thin_film_bsdf` is pooled as an
  ordinary dielectric; MaterialX transmission maps to no lobe, so a
  MaterialX-authored glass renders opaque. The graph runs **once per path
  vertex** (`Material::resolve` → `ShadingPoint`; it used to run once per
  query, 3.0 times a vertex measured), and `docs/shading_performance.md` is the
  plan from here: a faster interpreter next, a Cranelift JIT only last. Only
  document-scope and `<nodegraph>` nodes are read — `<nodedef>` custom node
  *implementations* are not, so a graph instantiating one gets that input at a
  constant (reported, not silent). No `<look>` / `<materialassign>`: bindings
  come from USD.
  On the **emission** side: `uniform_edf` is read exactly, and it is the only EDF
  that is. `conical_edf`, `measured_edf` and `generalized_schlick_edf` are all
  *directional* distributions, and crust's OpenPBR emitter is uniform —
  `emitted_directional` varies with angle only through the coat, which is a slab
  above the emitter and not the emitter's own lobe shape — so a cone, an IES profile
  or a Schlick falloff has nowhere to go. Pooling one onto a uniform emitter would be
  a plausible glow at the wrong intensity, so they are refused and reported rather
  than approximated, the same standard `crust:mipspace` applies. A `surface`'s
  `opacity` input is still dropped. And an **emissive MaterialX surface is not a
  light-list entry**: `AreaLight` pairs a `LightShape` with an `Arc<Emissive>`, whose
  radiance is a constant, and a graph's emission is a function of the shading point.
  So such a surface is found by BSDF/bounce sampling only, at full weight — exactly
  how emissive curves, instances and volumes already behave — which means no NEE and
  a firefly risk near a small bright emitter. That is also why `MtlxMaterial` leaves
  `emitted()` at zero and answers through `emitted_at` instead: the light list reads
  the hit-free one, and the two must agree for anything it samples.
