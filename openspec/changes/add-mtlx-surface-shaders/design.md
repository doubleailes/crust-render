# Design

## Context

See `proposal.md` § Why. The current state that shapes the approach:

- **`crust-mtlx` today.** A material compiles to a pattern `Program` (slot-indexed
  ops, optimised under `CRUST_MTLX_OPT`, JIT-compiled by `crust-jit`) plus
  `Flattened`: a *list* of `Lobe`s (`LobeKind` ∈ diffuse / dielectric / coat /
  conductor / sheen / subsurface) and `Emission` terms. `bsdf::flatten` walks
  `layer` / `mix` / `add` / `multiply` and bakes the tree's structure into
  per-leaf path weights. The tree itself is discarded. Only one fact survives, a
  dielectric over a specular becoming `Coat`.
- **`crust-core` today.** `MtlxMaterial::run` evaluates the program per path
  vertex, then `reduce()` pools the lobes onto one `OpenPBR`. `Material::resolve`
  → `Resolution` → `ShadingPoint` carries a `ResolvedOpenPBR`
  (`material/material.rs`). Outside `material/`, only
  `examples/light_occlusion.rs` reads it as OpenPBR. The integrator, NEE, MIS and
  guiding see only `scatter` / `eval` / `make_ray`.
- **Leaf BSDFs.** Crust already has them in `material/brdf.rs` and `openpbr/`:
  anisotropic GGX with VNDF sampling, EON diffuse, F82-tint and Schlick Fresnel,
  Charlie sheen, 3-wavelength thin film, a Walter BTDF with dispersion, and a
  homogeneous interior `Medium`.
- **The reference, Typhoon** (NVIDIA OpenUSD `typhoon/main` @ `70c45e8`,
  `pxr/imaging/plugin/hdEmbree/renderer/materials/MaterialXCpp/`):
  - Each surface node is a C++ builder emitting a closure tree
    (`materials/openPbr.cpp`, `standardSurface.cpp`, `gltfPbr.cpp`).
  - The tree is evaluated with `layer = f_top + f_base · T_top(ωo)`
    (`bsdf/closureTraversal.cpp`).
  - The dielectric `T` comes from BSDL's tabulated reflection filter by default
    (`ty:dielectricLayerThroughputMode = "bsdl"`, alternative `"materialxGlsl"`).
  - The branch is marked on hold pending a refactor, so the commit is pinned.
    Where Typhoon departs from the MaterialX graph, this design follows the graph
    and says so. Examples: `SheenMode::Zeltner` set but evaluated as Charlie, and
    no thin-walled subsurface branch.

## Goals / Non-Goals

**Goals:**

- One MaterialX evaluation path whose semantics are MaterialX's closure semantics,
  with leaves kept apart, for standalone-BSDF graphs and surface nodes alike.
- The three surface nodes expanded to their MaterialX 1.39 nodegraphs, node for
  node, auditable against the `.mtlx` sources.
- No new per-query tree walk. Tree work happens once per vertex.

**Non-Goals:**

- Changing crust's native `OpenPBR` (`crust:openpbr`, `UsdPreviewSurface`), or
  aligning it with the graph. That is a follow-up, informed by the Typhoon
  comparison.
- Missing pattern operators (`separate2`, `fract`, `range`, `ifgreater`,
  `combine4`, noises, `place2d`, …). A separate change. A builder needs some
  internal operators (D6), but no new *document* node categories are added.
- Opacity cutout, random-walk subsurface, Zeltner sheen, and anisotropy rotation.
  They are reported (spec), not implemented.
- Instantiating `<nodedef>` implementation graphs in general. Builders are
  hand-written (D6).
- Emissive MaterialX materials as light-list entries. Today's rule stands.

## Decisions

### D1. A closure tree, not a pooled übershader

MaterialX's own semantics are a tree, and the pooled reduction is where every
recorded MaterialX trap came from: coat promotion by tree shape, alpha pooling,
the `coat_darkening = 0` override, and independent-coverage bookkeeping. Keeping
the leaves apart retires the class of trap rather than patching instances of it.

Alternatives considered:
- **Map surface nodes onto `OpenPBR`**, the previous draft of this change.
  Rejected. It routes MaterialX through crust's OpenPBR, which differs from the
  MaterialX graph in layering, `specular_weight`, coat–base coupling and coat
  tint. It also forces the translation-graph approximations on `standard_surface`.
- **Keep `reduce()` for standalone-BSDF graphs.** Rejected by decision: two
  MaterialX semantics would coexist, and the same `.mtlx` would shade differently
  depending on whether it used a surface node.

### D2. The IR lives in `crust-mtlx`, in MaterialX vocabulary

`Flattened { lobes, emission }` becomes `Closures { tree, emission }`:

- `tree` is an arena of `Closure` nodes: `Leaf(Leaf)`, `Layer { top, base }`,
  `Mix { fg, bg, mix: Slot }`, `Add { a, b }` and `Multiply { input, weight: Slot }`.
- A `Leaf` names a MaterialX BSDF node with its inputs as program slots:
  - `OrenNayarDiffuse { weight, color, roughness, energy_compensation }` and
    `BurleyDiffuse`;
  - `Dielectric { weight, tint, ior, roughness: vec2, scatter_mode: R|T|RT,
    thin_film }`;
  - `Conductor { weight, ior: color3, extinction: color3, roughness, thin_film }`;
  - `GeneralizedSchlick { weight, color0, color82, color90, exponent, roughness,
    scatter_mode, thin_film }`;
  - `Sheen { weight, color, roughness, mode }`;
  - `Subsurface { weight, color, radius, anisotropy }`;
  - `Translucent { weight, color }`.
- Every leaf carries optional `normal` / `tangent` slots.
- `emission` keeps today's EDF term list and its per-channel weight rules.
- A volume term (`vdf` / surface transmission medium) is recorded beside the tree
  (D8).

The crate stays free of crust types. It describes *what MaterialX says*. How a
renderer evaluates a `Dielectric` leaf is crust-core's business, as with
`Texture` today. `Compiled::roots()` / `optimize()` remap tree slots as they
remap lobe slots now, so constant branches fold. The flatten pass keeps pruning
literal-zero branches (a leaf `weight` of 0, a `multiply` by 0, a `mix` at 0 or
1) at compile time.

### D3. Collapse the tree at `resolve`, because throughput depends only on ωo

`T_top` in `layer` is a function of the outgoing direction alone (MaterialX's
and Typhoon's model). ωo is fixed at a path vertex, so the tree is **exactly**
equivalent there to a weighted sum of leaves:

- `mix` scales its branches by `m` and `1 − m`;
- `multiply` scales its input;
- `add` concatenates;
- `layer` scales every base leaf by `T_top(ωo)`.

`Material::resolve` walks the tree once. It produces a `ResolvedClosure`: an
inline, fixed-capacity list of `(rgb_weight, leaf, frame)`. The list is
safe-Rust, with no heap allocation per vertex and no new dependency. Then:

- `eval(ωi)` is `Σ wᵢ · fᵢ(ωo, ωi)`;
- the pdf is the mixture `Σ pᵢ · pdfᵢ` with `pᵢ ∝ lum(wᵢ) · Êᵢ(ωo)`, floored as
  Typhoon's `_ApproxWeight` does so no live leaf gets probability 0;
- sampling picks a leaf by `pᵢ`, samples it, and returns the mixture pdf and the
  full `eval`. That is one-sample MIS, as `OpenPBR` already composes its lobes.

This is not pooling. Each leaf keeps its own roughness, IOR, normal and Fresnel,
and the weights vary with the view. Capacity is 8 leaves, and the worst built-in
expansion is 7 (`standard_surface`). 16 was the first plan, but it would have
doubled `ShadingPoint`'s stack size for every material, MaterialX or not. A
document whose tree has more than 8 leaves is refused at load with a `WARN` and
falls back.

### D4. Throughput and energy tables are ported from BSDL and MaterialX

Per decision, the tables are ported rather than regenerated:

- **Dielectric reflection throughput**: BSDL's `DielectricReflFront` filter
  (OpenShadingLanguage `libbsdl` `genluts.cpp`, BSD-3-Clause, at OSL commit
  `3dd1d94fe07b5374c6519cd44ca65f83d4598a80`, the commit Typhoon's
  `dielectricReflFrontLut.h` documents). It keeps BSDL's axes: IOR index
  `√((ior − 1.001)/(5 − 1.001))`, perceptual roughness, and a linear `cosθo`
  grid. Regenerated from BSDL's source, it matches Typhoon's shipped table bit
  for bit. BSDL's transmission-albedo and coupled-dielectric compensation tables
  are *not* ported. MaterialX's `layer` throughput reads the reflection albedo
  alone, and its GLSL does not compensate transmission, so they would have no
  consumer.
- **Conductor / generalized-Schlick throughput** and **GGX multiple-scattering
  compensation**: MaterialX GLSL's `mx_ggx_dir_albedo_analytic` fit and
  `mx_ggx_energy_compensation` (Apache-2.0). Throughput is `1 − E_ss(cosθo, α) ·
  F`, the form Typhoon's `LayerThroughputReflectance` uses.
- **Sheen throughput**: MaterialX's `mx_imageworks_sheen_dir_albedo` fit.
- **Thin film on a top leaf** uses the Fresnel-weighted form with the thin-film
  Fresnel, as Typhoon does, not the dielectric filter table.

The BSDL table becomes a Rust `static` array
(`crust-core/src/material/closure/bsdl_tables.rs`) and the MaterialX fits are
ported as functions (`closure/mx.rs`). The table is converted by a checked-in
script (`scripts/tables/bsdl_luts_to_rust.py`) from BSDL's own output, with a
`THIRD-PARTY.md` notice (BSD-3-Clause and Apache-2.0 are compatible with crust's
MIT licence under attribution). Tests pin spot values against the upstream
formulas and the axis conventions.

**Consequence to measure, not assume.** The tables describe BSDL's and
MaterialX's lobes, not crust's own GGX. The gap between a table's `E` and crust's
integrated leaf albedo is measured per table over the grid and recorded in the
materials `design.md`. The furnace scenario bounds its effect on energy.

### D5. Leaves are evaluated with `brdf.rs`, and microfacet leaves are compensated

- `Dielectric` uses the existing GGX VNDF, dielectric Fresnel and Walter BTDF
  (`R` / `T` / `RT`, thin-walled window when the surface is thin-walled), with
  dispersion when the builder passes an Abbe number. `T`'s transmission carries its own `(1 − F)`, as MaterialX GLSL's
  `mx_surface_transmission`, OSL, BSDL and Typhoon all do; the furnace (task
  5.4) caught an implementation without it.
- `Conductor` needs a complex-IOR Fresnel (new, per channel).
- `GeneralizedSchlick` uses the F82 form for `color82` (new generalisation of
  the existing F82-tint).
- `OrenNayarDiffuse` with `energy_compensation` is EON. Without it, it is
  MaterialX's plain Oren–Nayar.
- `Sheen` is Charlie (both modes, reported per spec).
- `Subsurface` is a diffuse-like leaf in its colour (reported).
- `Translucent` is a Lambertian BTDF.
- Microfacet leaves apply GGX multiple-scattering compensation with the D4 fit,
  as MaterialX GLSL does.
- Each leaf builds its own frame from its normal and tangent slots, following
  today's normal rule, and per-leaf thereafter.

### D6. Hand-written surface builders in `crust-mtlx`, node for node

`crust-mtlx/src/surface/{open_pbr,standard_surface,gltf_pbr}.rs` each emit the
tree of their nodegraph (`NG_open_pbr_surface_surfaceshader`,
`NG_standard_surface_surfaceshader_100`, `IMPL_gltf_pbr_surfaceshader`), as
Typhoon's builders do. Every block is commented with the nodegraph node names it
reproduces.

- Derived leaf parameters are **program ops** emitted by the builder, so they fold
  and JIT. Examples: coat-broadened roughness, the effective IOR modulated by
  `specular_weight`, the darkening factor, and `artistic_ior`, which is already an
  operator.
- Any operator this needs and the program lacks is added to the interpreter and
  `crust-jit` together, with a bit-identity test. Candidates are a select /
  compare and `copysign`.
- **Where Typhoon and the graph disagree, the graph wins.** The
  `open_pbr_surface` builder includes the thin-walled subsurface branch
  (diffuse-reflection + translucent), which Typhoon omits. The builder records
  the sheen `mode` it would need.
- Inputs are compiled connection → value → **nodedef default**. The defaults are
  per-model tables in `crust-mtlx`. The three nodedefs are vendored as test
  fixtures (Apache-2.0 header kept), and a test asserts the tables match them
  exactly.
- A non-default nodedef `version` is warned about and built as the default.

Alternative considered: instantiating the vendored stdlib nodegraphs through a
general nodedef mechanism. Rejected by decision. It needs many more pattern
operators and roughly a hundred-op program per surface per vertex. It would
become the natural cross-check once the pattern-node change lands.

### D7. `ShadingPoint` gains a closure variant; the ordering rule is kept

`Resolved` gains `Closure(ResolvedClosure)`. `Resolution` gains a closure
constructor that, like `Resolution::new`, reads emission from the evaluated
program **before** resolving the BSDF. The type still allows no other order.
`MtlxMaterial` implements `resolve` directly, not through `PatternMaterial`,
which stays for `PreviewSurface`.

`examples/light_occlusion.rs` stops reading `bsdf().params()`: it asks the
resolution whether it transmits. `tests/resolve.rs` keeps its contract, that
`resolve` answers exactly as per-query shading does, for the closure material.

### D8. Media come from the resolved closure, converted as MaterialX does

The resolved closure knows its interior medium, so `make_ray` attaches it on
refraction below a thick surface, and nothing re-runs the graph. The medium comes
from, in priority order:

- the surface node's transmission parameters, converted like the MaterialX volume
  graph and Typhoon's `MakeTransmissionMedium`: `σ_t = −ln(color)/depth`,
  `σ_s = scatter/depth`, `σ_a = σ_t − σ_s` shifted so its minimum is 0. Only
  where the node's nodegraph builds a volume (`open_pbr_surface`): Typhoon also
  gives `standard_surface` one, but its MaterialX graph has none, so the graph
  wins and `transmission_depth` / `transmission_scatter` are reported;
- `gltf_pbr`'s attenuation;
- an `anisotropic_vdf`.

Depth 0 means no medium. The medium is crust's homogeneous `Medium`. This
conversion differs from crust's `OpenPBR`, which uses the van de Hulst inversion.
That is deliberate for the MaterialX path and recorded.

### D9. Reporting is static where it can be

"Authored away from default" (connected, or a differing value) and "closure live"
(weight not a literal 0 after optimisation) are known at compile time. The
importer prints one `WARN` per material beside the unsupported-node warning:
the same cardinality, and `WARN`'s meaning of "authored, refused or approximated".
The suite authors `alpha_mode` / `geometry_opacity` at their defaults in dozens of
documents, so default-valued inputs stay silent.

### D10. Numbers first: the probe prints the resolved leaves

`examples/mtlx_shade` prints, at a named `(u, v)` and ωo, each resolved leaf: its
kind, RGB weight, roughness / IOR / colours, and normal. It also prints the
emission and the medium. Every spec scenario that says "the probe" is a unit test
over the same function. That is the check `CLAUDE.md` asks for. A wrong albedo or
weight still renders as a plausible surface.

## Risks / Trade-offs

- [**BREAKING renders**: `materialx_basic`, `materialx_emissive`, the DPEL Teapot
  and Lion change] → intended. Before and after is checked by probe numbers per
  material, not by eye. The DPEL numbers already recorded in the materials
  `design.md` (for example the teapot's body albedo) must survive. Goldens are
  re-recorded in the same commit that changes them, and nothing else changes
  goldens.
- [**Shading cost**: a vertex with 4–7 leaves evaluates each for every NEE / guide
  query, against one pooled OpenPBR today] → the tree walk is once per vertex
  (D3), and zero branches are pruned at compile time and resolve. It is measured
  with `bench_ab.sh` on `materialx_basic` and the DPEL assets, plus callgrind at
  `-s 2`, and recorded. A regression is accepted only as the price of correctness
  and is stated.
- [**Table ↔ leaf mismatch**: ported BSDL / MaterialX tables are not integrals of
  crust's leaves] → measured and recorded (D4). The furnace test bounds energy. If
  the mismatch matters, the follow-up is to regenerate tables from crust's leaves
  with the same axes.
- [**Non-reciprocal layering**: `T_top(ωo)` only] → it is the model's definition
  (MaterialX, Typhoon). NEE and bounce evaluate the same `f(ωo, ωi)`, so MIS stays
  consistent. Bidirectional methods, which crust does not have, would need care.
- [**`ShadingPoint` grows**: 8 inline leaves on the stack per vertex] → sized and
  measured (group 6). The capacity is already the smallest that holds every
  built-in expansion.
- [**The reference moves**: Typhoon is on hold pending a refactor] → commit pinned.
  MaterialX 1.39's nodegraphs, not Typhoon, are the normative source. Typhoon is
  the worked example.
- [**`crust:openpbr` and MaterialX `open_pbr_surface` disagree**] → stated in the
  proposal and in the materials `design.md`. The follow-up aligns crust's
  `OpenPBR` with the graph.
- [**Colour space of constant inputs**] → inherits the compiler's existing
  handling, unchanged. The suite is `lin_rec709` throughout, so it cannot catch a
  mistake here. Noted in `docs/color_management.md`.

## Migration Plan

- **One path.** The pooled `reduce()` path and `LobeKind` are removed in the same
  change that makes the tree live, so no document is ever shaded by both.
- **No environment switch.** This is a semantic change, not an optimisation, so
  there is no honest "old behaviour" to A/B.
- **Goldens.** Image goldens that include MaterialX materials are re-recorded,
  with the probe-number check above as the gate. Rollback is a revert.

## Open Questions

- Whether to also port MaterialX GLSL's dielectric layer-throughput fit, as a
  second mode for A/B against the suite's `materialx-glsl` reference (Typhoon's
  `ty:dielectricLayerThroughputMode = "materialxGlsl"`). It is deferrable: it adds
  a table and a switch without changing the specs or the task structure.
