# Proposal

## Why

Some studios build their look in compositing, not in the renderer. Their surface
shaders write no meaningful beauty, only AOVs: lighting splits, mattes, albedo,
primvars, motion vectors. The final image exists only once a compositing package
recombines them. Each frame costs a farm render plus 30 s to 1.5 min of comp. So the
artists upstream (texturing, layout, set exploration in usdview) never see anything close
to the final image while they work. They see an approximation built in another DCC, far
from the final look.

Production renderers solve this inside the renderer. RenderMan has **sample filters**,
programs that run on each camera sample after it is traced and before it is accumulated
into the film. Because they run per sample, the pixel filter antialiases their result for
free, which a comp on finished pixels cannot do for edges. RenderMan's **display
filters** are image operations on the accumulated pixels: convolutions, blurs, merges.
Both run in the renderer, so every host that drives it shows the same image: the CLI, a
render delegate in a Hydra viewer, an MCP session. The same idea is often sketched as
Hydra tasks stacked after the render. That ties the look to one host, and the comp
recipe is not carried in the USD.

Crust already produces every AOV such a look is made of (see `aovs`), but nothing can
combine them. This change adds the combining stage, a **precomp**, authored in USD next
to the render settings. Then `crust render`, `crust mcp` and the planned Hydra delegate
all show the studio's look, progressively, without a compositing round trip. It is a
preview of the look, not a replacement for comp. Comp stays where final-frame artistry
happens.

## What Changes

- **Sample filters.**
  - Authored as an ordered relationship `crust:sampleFilters` on the `RenderSettings`
    prim.
  - Each target is a MaterialX `NodeGraph` made of standard-library nodes (math,
    `mix`, `ifgreater`, `clamp`, colour ops).
  - Its interface inputs are bound by name to the render's RenderVars, and `beauty` is
    the beauty radiance.
  - It is compiled once by `crust-mtlx`, and by the JIT when built with `jit`.
  - It is evaluated on every camera sample from that sample's own AOV values.
  - Its outputs accumulate with the beauty's own pixel-filter weights.
- **Display filters.**
  - Authored as an ordered relationship `crust:displayFilters` on the `RenderSettings`
    prim.
  - Each target is a `Shader` prim whose `info:id` is one of a small fixed set of image
    operations:
    - `crust:convolve` (with an image kernel);
    - `crust:blur` (gaussian);
    - `crust:vectorBlur` (directional or radial blur driven by the motion-vector AOV or
      a constant direction, for 2D motion blur and speed lines);
    - `crust:merge` (`over`, `plus`, `multiply`, `screen`);
    - `crust:grade`.
  - Inputs connect to RenderVars, to sample-filter outputs and to other display filters.
  - They run on the whole accumulated image, deterministically.
- **Outputs are new channels, never the beauty.**
  - A RenderVar with `rel crust:filter` writes a filter's output; its `sourceType` and
    `sourceName` are then ignored.
  - `token crust:display` on `RenderSettings` names the output shown as *the* image.
    That output becomes the tone-mapped PNG, `--checkpoint` snapshots, MCP renders and,
    once it exists, the Hydra viewport.
  - The EXR beauty stays the renderer's beauty.
- **Progressive.** When a display output is authored, the render's snapshots carry the
  sample-filter planes, and the display filters run when a snapshot is read. A session or
  viewport therefore shows the look while the image refines.
- **Zero cost when unauthored.** A stage with no filters renders bit-identically with the
  same instruction count. There is no CLI flag and no `CRUST_*` switch: the feature is
  driven only by what the stage authors.
- **Refusals are loud.** Each of these is a typed import warning, and the output reads
  its clear value:
  - an unknown operation;
  - an unbound input;
  - a cycle;
  - a node a sample filter cannot evaluate (textures, geometry, closures);
  - a `crust:display` naming no output.
- **EXRs record the chain.** Every EXR carries `crust:sampleFilters` and
  `crust:displayFilters` (the filter prim paths, in order), so an image says which
  precomp produced it.

## Capabilities

### New Capabilities

- `precomp-filters`: sample and display filters authored in USD. Covers how they are
  bound to AOVs, when they run, what they may read, the displayed output, determinism,
  and what is refused.

### Modified Capabilities

- `aovs`: a RenderVar may read a filter output. The beauty-invariance requirement now
  covers filters too: no filter changes the beauty.
- `image-output`: the PNG is made from the displayed output when one is authored, and
  EXRs record the filter chain.

## Impact

- **crust-core:**
  - `scene/usd_import/settings.rs` reads the relationships and `crust:display`;
    `scene/usd_import/products.rs` (`resolve_var`) reads `crust:filter`.
  - `aov.rs`: new filter slots in `UnitAov` / `AovFilm`.
  - `tracer/mod.rs`: evaluation at the per-sample AOV seam, and the display chain after
    exposure.
  - `tracer/control.rs`: snapshots carry filter planes.
  - New `precomp.rs` for the display operations.
  - `warnings.rs`: new `precomp.*` codes.
- **crust-mtlx:** compiles a `NodeGraph` whose interface inputs are host-supplied
  channels, reusing the per-hit primvar channel that `geompropvalue` reads. No new op is
  needed.
- **crust-render:** `main.rs` (PNG and checkpoint from the displayed output) and
  `products.rs` (filter channels, EXR stamps). The MCP session's `render` and `snapshot`
  show the displayed output.
- **Performance:**
  - With no filters authored, nothing changes. This is pinned bitwise and with callgrind
    on `cornellbox.usda` at `-s 2`.
  - A sample filter forces the AOV path and costs one program evaluation per camera
    sample. It is measured on a sample scene with `bench_ab.sh` and recorded in the
    design.
  - Display filters cost one pass per operation over the image, at the end and on each
    snapshot read, and are independent of the sample count.
- **Interplay:**
  - `hydra-render-delegate` shows the displayed output in the viewport (its follow-up).
  - `payload-and-purpose-view` is independent.
- **Docs:**
  - `site/content/docs/usd/render-settings.md` (new section "Precomp"), and
    `site/content/docs/usd/aovs.md` (the `crust:filter` var, the EXR metadata rows, and
    the sentence saying products take no `crust:*` attribute).
  - `site/content/docs/reference/warnings.md`, and `site/content/docs/architecture/`
    (the design choice, and the limits).
  - `docs/architecture.md`: the seam and the invariant.
  - New design record `openspec/specs/precomp-filters/design.md` when archived.
- **Samples:** `samples/precomp.usda`, a scene whose surface writes AOVs only, with a
  precomp that rebuilds a stylised look from them.
