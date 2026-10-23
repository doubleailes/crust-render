# Design

## Context

What crust does today, and where a precomp has to fit:

- **Per-sample AOVs.**
  - `Renderer::advance_pixel` (`crates/crust-core/src/tracer/mod.rs`, ~1707) traces one
    camera sample. It draws its pixel-filter offset and weight (`filter.sample`, ~1785),
    splats `radiance * (wx·wy)` into `PixelState` (~1875) and, on the AOV path, hands
    the sample's values to the unit's planes: `planes.add(&scratch.first, &extras, fx,
    fy, wx*wy)` (~1845).
  - `SampleExtras` (`aov.rs`, ~693) holds the sample's LPE outputs, albedo, diffuse
    filter, time and primvars. `FirstHit` (~734) holds the first-hit data.
  - This seam is the one place that sees a whole sample's AOV values before they are
    averaged.
- **Accumulation.**
  - `UnitAov::add` (`aov.rs`, ~953) implements the two modes: filtered
    (Σwᵢvᵢ / Σwᵢ with the beauty's own weight and `weight_sum`) and closest.
  - `AovFilm::store` (~1110) resolves a unit into the full-frame film, and
    `AovFilm::blend` (~1190) mixes guided passes with the beauty's pass weights.
  - `var_channels` (~1313) produces an output var's channels, some of them derived from
    the beauty `Buffer`.
  - Exposure is applied after every pass (`render_impl`, ~653, through
    `Measured::apply_exposure` and `AovFilm::apply_exposure`). It is the only existing
    post-pass over the resolved image.
- **The invariant.** "AOVs observe; they never steer" (`docs/architecture.md`, ~377).
  - The beauty is bit-identical with and without AOVs
    (`the_beauty_is_bit_identical_with_and_without_aovs`, `crust-core/tests/aovs.rs`).
  - The `AOV = false` instantiation of `trace_path` is pinned by callgrind's
    instruction count on `cornellbox.usda`.
- **Output.**
  - `write_product` (`crates/crust-render/src/products.rs`, ~311) writes each product's
    channels as they come from the film.
  - `main.rs` (~2190) writes the products and then the PNG through `write_png`, from the
    first product's beauty. `tone_map` → `crust_core::color::encode_preview` applies the
    OCIO display and view.
  - There is no image processing anywhere in the writer.
- **Snapshots.**
  - `publish` (`tracer/mod.rs`, ~1300) copies each finished unit's
    `estimate() * exposure` into `RenderControl` (`tracer/control.rs`, ~149), beauty
    only, one unit at a time.
  - `snapshot()` (~137) clones that buffer. The CLI's `--checkpoint` and the MCP job's
    `image()` (`mcp/render.rs`) read it.
  - A snapshot holds partially filled units, so an operation that reads neighbouring
    pixels cannot run inside `publish`.
- **MaterialX programs.**
  - `crust-mtlx` compiles a document into a `Program` (`eval/mod.rs`, ~431) that the
    interpreter or the JIT evaluates against a `ShadeCtx`.
  - `ShadeCtx::primvars` is a slice of host-supplied values, one per name in
    `Program::primvars`, read by `geompropvalue` (`eval/compile.rs`,
    `compile_geompropvalue`), with a node's `default` when absent.
  - This is already a "named per-evaluation input" mechanism.
- **Settings.**
  - `scene/usd_import/settings.rs` maps the `crust:*` attributes on `RenderSettings`.
  - `scene/usd_import/products.rs` resolves products and vars (`resolve_var`, ~884),
    each refusal a `warning!` with a `WarningCode`.

### Prior art

- **RenderMan.**
  - Sample filters (`PxrSampleFilter` plugins) see each sample's AOV values and write
    back into the sample before it is filtered.
  - Display filters (`PxrDisplayFilter` plugins, e.g. `PxrImageDisplayFilter`,
    `PxrEdgeDetect`) run on accumulated pixels.
  - In USD they are `outputs:ri:sampleFilters` / `outputs:ri:displayFilters`
    connections from `RenderSettings` to shader prims.
  - Both run in every host, including the Hydra delegate. That is the property wanted
    here.
- **Compositing packages (Nuke).** The look is a node graph over AOVs on finished
  pixels. It is flexible but post-hoc: no antialiasing of the recombined result, and no
  presence in the 3D tools.
- **Hydra tasks.** A post-process can be a task after the render task. It is host-side
  and usually GPU, so it is not carried in the USD and not seen by `crust render`.

## Goals / Non-Goals

**Goals:**

- A stage can carry a precomp, a recipe that rebuilds a comp-driven look from AOVs.
  Every crust host shows it identically: the final render, its PNG, `--checkpoint`, an
  MCP session, and later the Hydra viewport.
- Per-sample recombination is antialiased exactly as the beauty is, with the same
  pixel-filter weights. Edges of a recombined matte or a remapped lighting split do not
  alias.
- Image operations that a comp-driven look needs most:
  - convolution and blur (glows, softening);
  - directional and radial blur driven by motion vectors (2D motion blur, speed lines);
  - merges;
  - grades.
- Authoring is plain USD and standard MaterialX for the programmable part, so a TD can
  write it by hand and a tool can author it.
- Zero cost and bit-identity when nothing is authored. Determinism when something is:
  the same image for any thread count, tiles or scanlines.

**Non-Goals:**

- Replacing a compositing package. There is no roto, no tracking, no deep, no
  multi-frame operation.
- Changing the beauty. A filter never writes back into the beauty sample or the beauty
  EXR channels, unlike RenderMan's sample filters.
- GPU execution, or Hydra-side tasks. Crust is CPU-only, and the image must be the same
  in every host.
- User-programmable display operations (an expression language over neighbourhoods).
  The fixed set comes first; programmability is Phase 2.
- Steering sampling from a filter output (see D8).
- Reading RenderMan's `ri:` filter prims.

## Decisions

### D1. Filters are authored on `RenderSettings` as ordered relationships

```usda
def RenderSettings "settings" {
    rel crust:sampleFilters  = [</Render/Precomp/look>]
    rel crust:displayFilters = [</Render/Precomp/glow>, </Render/Precomp/speed>]
    token crust:display = "look_final"
    rel products = </Render/beauty>
}
```

- A relationship, not a shader connection: `RenderSettings` is not a connectable prim,
  and crust already reads `RenderSettings` relationships (`products`, `camera`).
- The order is evaluation order. A filter may read only what is earlier in its list, or
  earlier lists.
- The settings follow the active render pass like every other setting (`render-passes`),
  so a lighting pass and a preview pass can carry different precomps.
- There is no CLI flag or `CRUST_*` switch: this is authored content. An environment
  switch exists only to A/B an optimisation (CLAUDE.md), and this is not one.

### D2. A sample filter is a MaterialX standard-library `NodeGraph` over AOVs

```usda
def NodeGraph "look" {
    color3f inputs:key       # bound to the RenderVar named "key"
    color3f inputs:fill
    float   inputs:matte
    color3f inputs:beauty    # reserved: the beauty radiance
    color3f outputs:out.connect = </Render/Precomp/look/mix.outputs:out>
    # ... ND_* Shader prims: multiply, mix, ifgreater, clamp, hsvadjust ...
}
```

- **Binding by name.** An interface input `inputs:<name>` reads the sample's value of
  the RenderVar whose channel name is `<name>`, among the vars of any product of the
  render.
  - `beauty` is reserved for the sample's radiance, after the firefly clamp and before
    exposure.
  - An unbound name is refused, and the input then reads its authored default.
  - Binding by name keeps the recipe readable by a TD and portable between scenes that
    name their AOVs the same way.
- **Compilation reuses the primvar channel.** The interface inputs are lowered to
  `Program::primvars` entries, exactly what `geompropvalue` reads. The host fills
  `ShadeCtx::primvars` with the sample's values. No new op is added, so the program
  folds, optimises and JITs as a surface does, and the JIT ↔ interpreter bit-identity
  pair covers it.
- **Allowed nodes.** The pure-value standard-library nodes are allowed: math,
  comparison, `mix`, `ifgreater` / `ifequal`, `clamp`, `remap`, `smoothstep`, colour
  transforms, `convert`, `luminance`, `hsvadjust`.
  - Textures, `geompropvalue`, `position` / `normal` / `texcoord` and BSDF/EDF closures
    are refused with `precomp.unsupported_node`. A sample has no shading point; what the
    surface knew is what the AOVs carry.
  - A refused graph contributes its outputs' clear value. It never fails the render.
- **Why not a fixed set of crust ops here.** The per-sample part is where a look's
  arithmetic lives (remaps, ramps, ratios of splits), and MaterialX already expresses it
  in a vocabulary other tools read. Crust already compiles it.

### D3. Sample-filter outputs accumulate like filtered AOVs

- A sample filter is evaluated at the per-sample AOV seam (`planes.add`, see Context),
  from `SampleExtras`, `FirstHit` and the beauty sample. Its outputs are added to new
  filtered slots of `UnitAov` with the same `wx·wy`.
- They are resolved by `AovFilm::store` with the beauty's own `weight_sum` and `/taken`
  fallback, and blended across guided passes by `AovFilm::blend`. In short, the rules
  of aovs D7 and D9 apply unchanged.
- **Inputs read the unaccumulated sample value**, whatever the bound var's accumulation
  mode:
  - a `closest` var (depth, ID-like data) is read as this sample's first-hit value;
  - a `variance` var cannot be bound (refused: `precomp.unbindable_input`), since
    variance has no per-sample value.
- Exposure is applied to sample-filter outputs as to radiance outputs (the aovs
  requirement "Camera exposure scales the radiance outputs"), because their inputs are
  pre-exposure. A filter output declares itself non-radiance with
  `bool crust:filter:radiance = 0` on its RenderVar, to opt out, like `depth`.
- Holdout samples feed their AOVs' clear values to the filter, which matches the aovs
  requirement "Holdout samples are empty in every AOV".
- A render with a sample filter takes the `AOV = true` path. The beauty sample is read,
  never written.

### D4. A display filter is one of a fixed set of image operations

Each display filter is a `Shader` prim:

| `info:id` | inputs | does |
|---|---|---|
| `crust:convolve` | `in`, `asset kernel`, `float kernelScale` | convolves `in` with an image kernel (normalised to sum 1 unless `normalize = 0`); edges clamp |
| `crust:blur` | `in`, `float radius` (pixels) | separable gaussian |
| `crust:vectorBlur` | `in`, `vector2 / motion` connection or `float2 direction`, `float2 center` + `float strength` for radial, `int taps` | line integral along the per-pixel vector (motion AOV), a constant direction, or toward/away from a centre |
| `crust:merge` | `a`, `b`, `token operation` (`over`, `plus`, `multiply`, `screen`), `float mix` | per-pixel composite; `over` reads `b`'s alpha from a connected `alpha` input |
| `crust:grade` | `in`, `color3f lift`, `gain`, `gamma`, `offset` | per-pixel grade, Nuke's `Grade` formula |

- Image inputs are bound by name, as in D2, because a RenderVar has no outputs to
  connect to: `token inputs:in = "key"` names a RenderVar, `"look.out"` a sample-filter
  output, and `"glow"` an earlier display filter. An unknown id is `precomp.unknown_operation`, an unknown
  name `precomp.unbound_input`, and a reference to a later filter `precomp.cycle`.
- Operations run on full-frame `f32` planes after exposure, in list order, each
  writing a new plane. They are deterministic. Work is split by rows over rayon, and
  each output pixel's sum is taken in a fixed order, so the result is bitwise
  independent of the thread count and of tiles versus scanlines.
- Kernels are loaded through `AssetLoader` (crust-core decodes no assets); a loader
  returning `None` is `precomp.missing_kernel` and the operation passes `in` through.
- Cropped renders (`--region`) run display filters on the crop, with edges clamped at
  the crop. A blur near a crop edge therefore differs from the same pixels of a full
  frame. This is stated, not hidden.
- **Why fixed.** These five cover the effects a comp-driven look leans on, including
  the expensive one (vector blur for 2D motion blur and speed lines). Each operation is
  plain, testable code. A programmable neighbourhood language is Phase 2.

### D5. Outputs are new channels; the beauty is never replaced

- A RenderVar reads a filter output with `rel crust:filter = </Render/Precomp/look>`,
  plus `token crust:filter:output = "out"` when the graph has several outputs. Its
  `sourceType` and `sourceName` are then ignored and its `dataType` must fit
  (`aov.type_mismatch`, as for any var).
- `token crust:display` on `RenderSettings` names the **displayed output**: a RenderVar
  channel name, which may be a filter output or any other var. It is what the PNG,
  `--checkpoint`, the MCP session's `render`/`snapshot` and, later, a Hydra viewport
  show. Unauthored, the displayed output is the beauty, as today. A name matching no var
  is `precomp.unknown_display` and the beauty is shown.
- The EXR's beauty channels stay the beauty. This keeps the aovs requirement "AOVs
  never change the beauty", now extended to filters, and lets comp start from the same
  beauty whether or not a precomp exists.

### D6. One display-chain function, called where an image is read

- `crust_core::precomp::apply(&Chain, &Buffer, &AovFilm) -> DisplayPlanes` is the one
  implementation.
- It is called at three sites:
  - the final image, after `render_impl`'s exposure, so `write_product` and `write_png`
    see the chain's planes;
  - `RenderControl::snapshot()`;
  - the MCP job's final image.
- **Progressive snapshots.** When a display output is authored, `publish` also copies
  each finished unit's sample-filter and bound-var estimates into a snapshot film beside
  the display buffer. `snapshot()` runs the chain over the whole buffer on read.
  - Unfinished units read the clear value, so a blur bleeds into them, as a progressive
    comp would.
  - Without a display output, `publish` is unchanged.
- The CLI's PNG keeps going through `tone_map` / `encode_preview`. The displayed output
  is tone-mapped like the beauty when it is a colour, and written as grey when it is a
  float.

### D7. No filter authored, nothing changes

- A stage with no `crust:sampleFilters`, no `crust:displayFilters` and no
  `crust:display` renders bit-identically, takes the same code path and has the same
  instruction count:
  - pinned by an extension of `the_beauty_is_bit_identical_with_and_without_aovs`;
  - pinned by `scripts/check_images.sh check` against goldens recorded before the
    change;
  - pinned by callgrind on `cornellbox.usda` at `-s 2`, which must show the same
    instruction count.
- Filter slots and the snapshot film are allocated only when authored. The per-sample
  hook is a branch on a per-render constant hoisted out of the sample loop, not a
  per-sample `Option` test. If callgrind shows it is not free, it becomes a const
  generic like `AOV`.

### D8. The adaptive stop rule still reads the beauty

- The early-stop test stays beauty-only (aovs D9). Filters observe; they never steer.
- **Risk:** a stage whose surfaces write no beauty stops every pixel at
  `minSamplesPerPixel`, because a black beauty has no variance. The guidance, in the
  docs and in a `precomp.black_beauty` warning raised when the beauty var's
  shaders emit nothing (a check at import, not per pixel), is to author
  `crust:minSamplesPerPixel = crust:samplesPerPixel`.
- Letting the stop rule read the displayed output is Phase 2. It changes which samples
  the beauty takes, so it must be its own A/B.

### D9. Warnings, stamps and logging

- **New `WarningCode`s.** Each gets a row in `site/content/docs/reference/warnings.md`,
  kept in step by the existing test:
  - `precomp.unsupported_node`;
  - `precomp.unbound_input`;
  - `precomp.unbindable_input`;
  - `precomp.unknown_operation`;
  - `precomp.cycle`;
  - `precomp.missing_kernel`;
  - `precomp.unknown_display`;
  - `precomp.black_beauty`.
  All are raised on the importing thread, once per prim.
- **EXR stamps.** Every EXR carries `crust:sampleFilters` and `crust:displayFilters`:
  the prim paths in order, comma-separated, and empty when none. It also carries
  `crust:display` (the displayed output's name). These sit beside the sampling stamps,
  and crust's stamp wins over an authored product attribute of the same name.
- **Logging.** One `DEBUG` line per compiled filter (it scales with the stage). Nothing
  is logged per sample or pixel. `--stats` gains one line, `precomp: <n> sample /
  <m> display filters, <t> ms display` (bounded per render).

## Risks / Trade-offs

- **Per-sample cost.**
  - A sample filter is one program evaluation per camera sample, on top of the AOV
    path. For a typical look graph (tens of ops) that is small beside a path, but it is
    measured, not assumed.
  - It is benchmarked with `bench_ab.sh` on `samples/precomp.usda` with and without
    filters, and with callgrind at `-s 2`, and the numbers are recorded here when built.
- **Display cost at snapshot rate.**
  - A large vector blur over a 2K frame, at every `--checkpoint` or MCP snapshot, could
    dominate an interactive session.
  - Snapshot reads are already throttled by their callers, and the chain runs only on
    read. The cost is measured and reported in `--stats`.
- **Exposure order.** Sample filters see pre-exposure inputs and display filters
  post-exposure ones. This mirrors where each runs in RenderMan, and a recipe written for
  one place does not move to the other unchanged. It is documented with an example.
- **Bind-by-name fragility.** Renaming a RenderVar silently unbinds a filter input,
  which is caught as `precomp.unbound_input`. This is the price of a recipe a TD can read
  and reuse across scenes.
- **Not the final comp.** A preview that looks right but differs from comp in subtle
  ways (comp's filtering, colour pipeline) can mislead reviews. The docs state that the
  precomp is a preview, and the EXR stamps say which recipe made an image.
- **Crop edges.** Display filters on a `--region` crop clamp at the crop (D4), so a
  crop is not a bitwise sub-image of the full frame once a neighbourhood operation runs.

## As built

What the implementation settled that the decisions above left open, and where it
departs from them.

- **The precomp rides on `AovRequest`, not `RenderSettings`.** `RenderSettings` is
  `Copy`, and a compiled filter (programs, JIT builds, kernels) is not; the precomp is
  `AovRequest::precomp: Option<Arc<Precomp>>`, read with the products
  (`import_render_products`, which the full import, the in-place settings update and
  `crust check` share). It reaches the renderer through the layout
  (`AovLayout::precomp`), as every other AOV does.
- **Interface inputs are `crust_mtlx::Source::Interface`.** The USD translation
  (`mtlx_network::translate_value_graph`) turns a connection to the graph's own
  `inputs:<name>` into that source, and the compiler lowers it to an `Op::Primvar`
  channel, the op `geompropvalue` already emits; `compile_value_graph` (crust-mtlx)
  walks the graph first and refuses textures, `geompropvalue`, geometry nodes, closures
  and unknown categories. No op was added; the JIT test
  `value_graphs_over_interface_inputs_match` pins JIT ↔ interpreter on such a program.
- **`hsvadjust` is not available.** D2 lists it among the allowed nodes, but crust-mtlx
  has no operator for it, so a graph using it is refused like any unknown node.
  `rgbtohsv` / `hsvtorgb` and `colorcorrect` are there.
- **A ninth code, `precomp.unknown_filter`.** A var whose `crust:filter` targets a prim in
  neither list, or names an output the filter does not have, writes no channel; none of
  D9's eight codes fit it.
- **`precomp.black_beauty` is "no light".** The import cannot know what a stage's
  surfaces emit, so the check is that the stage has a precomp with filters and an empty
  light list.
- **The unit's filters are boxed** (`UnitAov::filters: Option<Box<UnitFilters>>`), so a
  unit without them keeps its size.
- **`probe` reads the beauty.** An MCP job answers with `Job::shown` (the displayed
  output), while `Job::image` and `RenderControl::beauty_snapshot` keep returning the
  beauty for `probe`.
- **Display operation details.** `crust:blur`'s σ is `radius / 3`, truncated at the
  radius. `crust:vectorBlur` averages `taps` bilinear lookups centred on the pixel, from
  `−v/2` to `+v/2`, and leaves a pixel with a zero vector exactly as it was; its radial
  `center` is in pixels from the image's bottom-left (default its centre). `crust:merge`'s
  `over` is `b + a·(1 − alpha)`. `crust:convolve` samples its kernel nearest, each texel
  covering `kernelScale` pixels, and uses the kernel's matching channel (its last past
  three).

- **Limits** (review on #382). A `crust:blur` radius past 1024 px and a `crust:convolve`
  footprint past 255 px a side are refused (`precomp.invalid_value`, a tenth code) and
  pass their input through, so an authored value cannot exhaust memory; a radius whose
  deviation underflows is the identity. A radial `crust:vectorBlur` measures its centre
  and pixels in the frame (`precomp::Frame`), so a crop blurs toward the full frame's
  centre. A multioutput root's width is its selected output's (one evaluation over the
  defaults), and a dangling nodegraph output refuses the graph.
- **EXR kernels are read by base channel name.** `crust-assets`' `decode_exr_pixels`
  (environment maps, light textures, kernels) now goes through `read_exr_channels`, so
  `rgb.R` / `rgb.G` / `rgb.B` or one `Y` channel decode as bare `R`, `G`, `B` do; the RGBA
  convenience reader refused them.

### Measurements

- **Goldens.** `scripts/check_images.sh check` against the goldens of task 1.1 (`f958a80`,
  `--indirect-clamp 0`): every one of the 43 scenes bit-identical.
- **Callgrind, `cornellbox.usda -s 2`, one thread.** `advance_pixel::<Sobol, false, …>`
  (the beauty-only per-sample path) is 3,816,397,660 instructions inclusive before and
  after, to the instruction; the parallel work over the units is 3,846,840,001 before and
  3,846,839,938 after (rayon's scheduling moves a few dozen). The process total moved from
  4,276,302,705 to 4,276,658,838 (+0.008%), all of it once per render or per work unit:
  the import reading the three settings attributes (`element_cmp`, about +64k), the three
  stamp attributes in the EXR header (about +40k), and `Unit::new` and the drop of its
  `Option<UnitAov>` (+44 and +28 instructions per unit over 920 units: codegen, not work).
  The per-sample hook is free, so it did not become a const generic (D7); the total is not
  equal to the instruction; task 6.2 was accepted on the per-sample equality, which is what D7 protects.
- **Sample filter cost.** `scripts/bench_ab.sh -n 10 -x "-s 64"` on `samples/precomp.usda`,
  A its AOVs only, B the same with the `look` sample filter (12 ops, four inputs), both
  without display filters, one binary: A 1.140 s min / 1.228 s mean, B 1.301 s min /
  1.418 s mean, **+14.1% min, +15.5% mean**. The scene is cheap per sample (four bounces,
  three objects), so the filter's evaluation and its four input reads are a visible share
  of a sample; a production path would dilute it.
- **Display chain.** The sample's three display filters (a radius-9 blur, a merge and a
  24-tap vector blur) over 320×180: 4.9 to 8.5 ms per render (`--stats`' `precomp` line,
  five runs at 64 spp), against a 1.2 to 1.4 s render.

## Migration Plan

- **Stages:** nothing changes for a stage that authors no filter (D7).
- **CLI:** no new flag. `crust check` lists the precomp (filters, bindings, displayed
  output) in its `crust-check/2` report, as an added field.
- **Session clients:** `render` and `snapshot` show the displayed output when one is
  authored. Editing a filter re-imports, like any settings edit that is not in place.
- **Rollback:** remove the relationships from the stage.

## Phase 2 (follow-up changes, not this one)

- An adaptive stop rule driven by the displayed output (D8), A/B'd against the
  beauty-only rule.
- Display filters edited in place in an MCP session without a re-import (a
  `Signature`-level classification in `crust_core::live`).
- The Hydra viewport shows the displayed output (`hydra-render-delegate`'s follow-up).
- Programmable display operations: a neighbourhood expression language, or a MaterialX
  subset with pixel-offset sampling.
- Sample filters that write back into the beauty sample, behind an explicit opt-in, if a
  look ever needs the beauty itself antialiased differently.
