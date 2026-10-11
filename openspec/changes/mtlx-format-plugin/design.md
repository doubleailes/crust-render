## Context

Crust reads MaterialX two ways today:

1. **Inline networks** (`usd_import/mtlx_network.rs`). `Shader` prims whose
   `info:id` is an `ND_*` nodedef are turned into the `crust_mtlx::Node`s the
   XML parser would have produced, then compiled. Literal values are formatted
   as `value` text and read back through `parse_literal`, and each input's
   `colorSpace` metadata becomes its `colorspace`. Asset inputs are anchored on
   their authoring layer.
2. **Referenced documents** (`usd_import/materials.rs`, `mtlx_reference` and
   `load_mtlx_material`). openusd has no `.mtlx` format, so the reference
   composes to nothing. The importer reads the arc off raw layer specs, opens
   the file and parses the XML itself. This route is consulted lazily
   (`try_mtlx!`) wherever the USD route gives up.

C++ OpenUSD has only the first route in the renderer. The second is the
`usdMtlx` file-format plugin (`pxr/usd/usdMtlx`), which translates the document
into `UsdShade` prims so composition does the rest. `crust-mtlx` sits on both
sides of that line: `parse.rs` is the translation's front end, and `eval/`,
`bsdf.rs`, `surface/`, `hair.rs` and `texture.rs` are the renderer's.

openusd today (`crates/openusd/src/sdf/layer_registry.rs`):

- `sdf::FileFormat` is a trait with read-only support (`caps`), so a `.mtlx`
  reader fits it.
- The format set is a fixed `static DEFAULT_FORMATS = [&USDA, &USDC, &USDZ]`.
  `find_by_extension` / `find_by_id` search only that list. A comment there
  says custom formats are planned.
- There is no Sdr registry. The ROADMAP's UsdShade row lists "the Sdr shader
  registry" and "renderer shader dialects (MDL / MaterialX)" as remaining.

## Goals / Non-Goals

**Goals:**

- A `.mtlx` reached by any composition arc (reference, payload, sublayer,
  root layer) composes into `UsdShade` prims laid out as `usdMtlx` lays them
  out.
- One route into MaterialX in crust: the inline-network reader.
- Every sample and asset that renders through a `.mtlx` reference today
  renders the same afterwards.

**Non-Goals:**

- Moving shading into openusd. The compiler, interpreter, closure tree,
  surface expansions, hair, textures and `crust-jit` stay in crust. In C++
  that work belongs to MaterialXGenShader and the render delegate, not to USD.
- A general Sdr registry. The plugin needs nodedef signatures to choose `ND_*`
  ids and to type inputs; that table can later back Sdr, but Sdr discovery and
  parser plugins are their own openusd item.
- Translating MaterialX `<look>`s into variant sets. `usdMtlx` does this, crust
  ignores looks today, and nothing in the samples or the DPEL assets needs
  them. The plugin may skip them at first; its ROADMAP row says so.
- Writing `.mtlx`. The format is read-only.

## Decisions

### The translation lives in openusd, in its own crate

`openusd-mtlx`, a sibling of `openusd-schemas`, depending on `openusd` and an
XML parser. openusd's rule is that the core never depends on domain crates, and
MaterialX is not part of the AOUSD core spec, so it cannot sit in `openusd`
itself. A crate inside crust would work technically, but it would keep a USD
concern in the renderer and leave every other openusd user with an empty
material.

The plugin does not reuse `crust-mtlx::parse`. openusd cannot depend on crust,
and the plugin needs what that parser deliberately skips: nodedefs. A shared
third crate would couple the two repositories' release cycles for about 300
lines of XML walking.

### openusd gains runtime format registration first

The plugin cannot ship before a third-party crate can add a format. The shape
is openusd's to decide; what crust needs is a call on the stage builder (or
the `LayerRegistry` it owns), so that `usd_import::stage_builder()` can add the
format next to `schema_registry(...)`. Global registration would be the C++
plugin model, but it is process-wide state and openusd keeps composition free
of shared mutable state. Per-builder registration fits its existing style.

### Layout follows `usdMtlx`

The translated layer roots everything at `/MaterialX`:
`/MaterialX/Materials/<name>` for each `surfacematerial`, with
`outputs:mtlx:surface`, `outputs:mtlx:displacement` and `outputs:mtlx:volume`
terminals; `Shader` prims with `info:id = "ND_…"`; `NodeGraph`s with their
outputs and interface inputs. The exact nesting (shaders encapsulated under
their material, node graphs authored once and brought in by internal
reference) is to be matched against `usdMtlx`'s `reader.cpp` and its test
baselines, since a reference only brings in the referenced prim's subtree. The
existing reference paths (`</MaterialX/Materials/name>`) must keep resolving;
that is what every authored asset points at.

### Nodedefs come from the MaterialX libraries, embedded

Choosing `ND_standard_surface_surfaceshader` over another signature, and giving
an input its USD type, needs the nodedef table. The plugin embeds the MaterialX
1.39 `stdlib`, `pbrlib` and `bxdf` definition documents (Apache-2.0, allowed by
openusd's `deny.toml`) and parses them once. Documents that declare their own
`<nodedef>`s add to the table for that document only. A node with no matching
nodedef is still translated, with an `info:id` built the way MaterialX names
one (`ND_<category>_<type>`), and reported as a composition-time warning rather
than dropped.

crust-mtlx's own hard-coded defaults and `tests/nodedefs.rs` are unaffected:
the plugin decides ids and types, crust decides values for unauthored inputs.

### Colour space and asset paths are carried as metadata, not resolved

The plugin writes each input's effective `colorspace` (input, node,
nodegraph, document) as the attribute's `colorSpace` metadata, which
`mtlx_network.rs` already reads. `fileprefix` is folded into each `filename`
value. Asset values are written as authored, relative to the `.mtlx`, and
anchored by openusd against the layer that authored them, which is the `.mtlx`
itself. That is the MaterialX rule `load_mtlx_material` follows today by
anchoring on the file's directory.

### Crust switches over in one change, after a parity check

The fallback is not kept behind an environment switch. Switches exist to A/B
an optimisation; this is a change of route with an expected bit-identical
result, proved once with goldens (below). Keeping both routes would keep the
code this change exists to remove.

Before the switch, while `openusd-mtlx` is being written, crust can already
check the inline route against the reference route without the plugin: a test
that converts each sample `.mtlx` to inline USD (by hand or with C++
`usdcat`) and compares the compiled `Program`s. That surfaces gaps in
`mtlx_network.rs`, such as a `displacementshader` terminal or a node whose
`nodedef_category` inversion fails, before openusd is involved.

## Risks / Trade-offs

- [Image changes from the round trip through typed USD values] → Literals
  pass from `.mtlx` text to typed values and back to text before
  `parse_literal`. Rust's shortest float formatting round-trips `f32`
  exactly, but integer-versus-float inputs, `filename` versus `string`, and
  `color4` versus `vector4` must keep their types. Mitigation: compare
  compiled `Program`s per sample (exact), then `check_images.sh` at `-s 16`
  on every `.mtlx` sample and on the DPEL Teapot and Lion.
- [Node names change from document names to prim paths] → Names only key the
  graph, but a warning that names a node will now name a prim path. That is
  closer to what the user authored in USD.
- [Translation cost at import] → The whole document is translated when the
  layer opens, including materials no prim binds. A `.mtlx` is small next to
  the geometry, but Moana-style libraries with hundreds of materials per file
  are measured with `--stats`. A layer opens once per stage, which the current
  route does not guarantee: it parses per material.
- [Blocked on another repository] → Crust cannot finish until openusd ships
  registration and the plugin. The parity test and the `mtlx_network.rs` gaps
  are useful on their own and land first.
- [Behaviour the plugin skips] → Looks, and custom nodedefs implemented by a
  nodegraph, compose to less than the document says. Today crust ignores looks
  too. For custom nodedefs, `crust-mtlx` skips `<nodedef>` and
  `<implementation>`, so what it renders now is the inline expansion if the
  document has one. The parity check catches any material that loses
  something.

## Migration Plan

1. openusd: format registration, then `openusd-mtlx` (its ROADMAP row).
2. crust: the parity test and the `mtlx_network.rs` gaps, against hand-made
   inline equivalents.
3. crust: bump the openusd git revision, register the format, record goldens
   on the old binary, remove the fallback, check goldens, compare `--stats`
   import time.
4. Docs in the same change as step 3.

Rollback is reverting step 3; openusd's additions are inert unless
registered.

## Open Questions

- Does `crust-mtlx` keep `parse.rs` and `roxmltree` long term? It is still
  used by the oracle tests and the `mtlx_shade` probe, which read `.mtlx`
  fixtures. Rewriting them over a stage opened with the plugin would let the
  parser go, but it would make `crust-mtlx`'s tests depend on openusd.
- Should openusd's registration API also take an Sdr-style node-definition
  table, so that the plugin's nodedef library becomes the first Sdr
  discovery source? That is openusd's call, and it does not block crust.
