## Why

A `Material` prim whose only opinion is
`references = @foo.mtlx@</MaterialX/Materials/name>` composes empty, because
openusd has no MaterialX file format. Crust works around this in the importer:
`mtlx_reference` (`usd_import/materials.rs`) reads the `references` list-op off
raw layer specs across the prim's composition graph, takes the first `.mtlx` it
finds, anchors it to the authoring layer, and `load_mtlx_material` parses the
file with `crust-mtlx`. The workaround is a second route into MaterialX beside
the one USD defines. It only sees references, not payloads, inherits or a
`.mtlx` opened as a sublayer or as the root layer. Anything that reads the
composed stage rather than the importer, such as flattening or an MCP edit to
a shader input, sees an empty `Material` prim.

C++ OpenUSD splits MaterialX in two. The `usdMtlx` file-format plugin
translates a `.mtlx` into `UsdShade` prims (`Material`, `Shader` prims with
`info:id = "ND_…"`, `NodeGraph`s). The shading itself (code generation,
evaluation) lives in the renderer. Crust already has the renderer half for
inline networks (`mtlx_network.rs` reads `ND_*` shader prims into a
`crust_mtlx::Doc`), so the translation half is the only part on the wrong side
of the line.

## What Changes

- **openusd** (out of tree, tracked here as a dependency): openusd gains a way
  to register a `sdf::FileFormat` at runtime, and a new crate,
  `openusd-mtlx`, provides a read-only `.mtlx` format that translates a
  document into `UsdShade` specs the way `usdMtlx` does. The work is a row in
  openusd's `ROADMAP.md`.
- **crust-core**: `usd_import::stage_builder()` registers the `.mtlx` format,
  so a reference, payload or sublayer into a `.mtlx` composes into ordinary
  shading prims and goes down the inline-network path.
  **BREAKING (internal)**: `mtlx_reference`, `load_mtlx_material` and the
  `try_mtlx!` fallback are removed.
- **crust-mtlx** keeps the parser, compiler, interpreter, closure tree and
  surface expansions. Its XML parser stays for its own tests, the OSL and
  nodedef oracles and the `mtlx_shade` probe, but the import no longer calls
  it.
- **crust-jit** is unchanged.
- Every `.mtlx`-referencing sample renders the same as today: bit-identical
  at `-s 16`, or any difference shown to be noise.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `materials`: a `.mtlx` reached by any composition arc composes into the
  stage as `UsdShade` prims, and renders through the inline-network
  requirement. A MaterialX document is visible to every consumer of the stage,
  not only to the renderer.

## Impact

- `crust-core`: `usd_import/mod.rs` (`stage_builder`),
  `usd_import/materials.rs` (removal of the reference fallback),
  `usd_import/mtlx_network.rs` (any gaps the translated networks expose:
  `displacementshader`, `fileprefix`, look-level data),
  `material/materialx.rs` (module doc; the file-loading entry points the
  importer no longer uses), and `tests/usd_scene.rs` (the reference test's
  rationale).
- `crust-mtlx`: `lib.rs` module doc (it no longer says it exists because
  openusd lacks a plugin).
- `Cargo.toml`: a dependency on `openusd-mtlx`, patched to openusd's GitHub
  `main` like `openusd` and `openusd-schemas`; `deny.toml` if the plugin
  brings a new XML dependency.
- Docs: `openspec/specs/materials/design.md` § MaterialX, `CLAUDE.md`
  (workspace line for `crust-mtlx`), `site/content/docs/usd/materials.md` (the
  `.mtlx` row and its example: any arc now works),
  `site/content/docs/architecture/design-choices.md` /
  `limitations.md` where they mention reading `.mtlx` directly.
- Performance: a `.mtlx` is translated once per layer open instead of being
  parsed once per material. Import of the DPEL Teapot and Lion is measured
  before and after with `--stats`. Render throughput is unaffected: the
  compiled `Program` is the same.
- Blocked on openusd: until `openusd-mtlx` exists on openusd `main`, only the
  measurement and gap-closing tasks can run.
