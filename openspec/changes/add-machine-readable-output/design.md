# Design

## Context

See `proposal.md` for why. The state of the code this design builds on:

- **The stats report** is `crust_core::RenderStats` (`stats.rs`). It is plain
  data (`phases`, `scene`, `image`, `rays`, `textures`, `ptex`, `materials`,
  `light_kinds`, `profile`, `subdivision`, `displacement`), and all of its
  presentation is in `impl Display`. `main.rs` logs it as a single event on
  `STATS_TARGET`, which `-l` cannot silence.
- **The render's log goes to stdout** (`logging::Terminal::Stdout`). `ls`
  already moves its log to stderr (`Terminal::Stderr`), because its stdout is
  the result.
- **The listing.** `usd_import::listing::list_prims(path, kind)` opens the
  stage chunk by chunk and returns `Vec<String>`. It has no time code.
- **Two EXR writers.** `write_beauty` (`main.rs`) writes a stage with no
  products. `products::write_product` writes RenderProducts. The product
  writer already refuses authored attributes that describe the pixels:
  `colorInteropID`, and standard EXR names (warning, not copied).
- **The diff tool.** `examples/exr_diff.rs` is about 250 lines with the
  semantics the `image-comparison` spec keeps. Its only machine consumers are
  `check_images.sh` (which `sed`s the first line) and
  `gen_texture_alias_scene.py` (which scrapes `rmse:` and `mean abs diff:`).
- **Dependencies.** `serde` and `serde_json` are in `Cargo.lock` through
  dev-dependencies, but are not a direct dependency of any crate.
  `add-diagnostic-command` plans to add both to crust-core (its task 2.1).

## Goals / Non-Goals

**Goals:**

- One JSON vocabulary across `stats`, `ls` and `diff`, which
  `add-diagnostic-command` can adopt unchanged.
- No measurable cost on the render path: serialization and stamping happen
  once, after tracing.
- The text outputs keep their content. Reports are built from one value, so
  the JSON and the text cannot disagree.

**Non-Goals:**

- A JSON form of the log itself (`tracing` JSON layer). The log is for people;
  the reports are for programs.
- A published JSON Schema file. Key-order and key-set tests are the contract
  for now.
- `traversal_report` (feature `traversal-stats`) in the JSON. It is a kernel
  diagnostic behind a feature flag, and stays text-only.
- An MCP server or any long-lived process.
- World-space light power in `ls`. It needs the composed transform and the
  light-list build, which is a render's work, not a listing's.

## Decisions

### D1. serde in crust-core; the report types are the contract

- `RenderStats` and its children derive `Serialize`, with
  `#[serde(rename = ...)]` where a field name lacks its unit.
- `Duration` serializes as `f64` seconds through a small `serialize_with`
  helper, and memory as `*_bytes`.
- Derived figures that the text report prints (`total_rays`,
  `mean_path_length`, `rr_kill_rate`, `hit_rate`, `micro_rate`, Mrays/s) are
  serialized too, as computed fields on the struct's serialize impl. Each
  consumer would otherwise recompute them, and some of them have
  divide-by-zero guards worth keeping in one place.
- `phases` stays a flat array with `depth`, as the type is. Nesting it in the
  JSON would make the JSON's shape differ from the type's.

*Alternative:* hand-written `serde_json::Value` building in crust-render. It
was rejected because a field added to `RenderStats` would be silently missing
from the JSON. With derive, the new field appears in the JSON automatically,
and the key-set test fails until it has a deliberate name.

### D2. Envelope: `format` first, `crust_version` second

A generic `Report<T> { format: &'static str, crust_version: &'static str,
#[serde(flatten)] body: T }`. serde_json writes fields in declaration order,
so the envelope comes first without `preserve_order`. Formats in v1:
`crust-stats/1`, `crust-ls/1`, `crust-diff/1`. `add-diagnostic-command`'s
`crust-diagnostic/1` uses the same wrapper.

### D3. Non-finite floats → `null`

serde_json refuses to serialize NaN and infinity. A `finite_or_null` helper
maps every `f32`/`f64` metric field through `Option`. The rule is in the
`cli` spec, and a unit test feeds it `inf`.

### D4. `--stats-json PATH|-` and `--json PATH|-`; `-` decides the log's stream

- The decision is made where `main` already chooses `Terminal::Stdout` or
  `Stderr`. It is extended: a command whose JSON report targets `-` gets
  `Stderr`.
- The progress bar is already on stderr. With `--stats-json -` together with
  `--stats`, the text report is logged too, so it also goes to stderr, as the
  `cli` spec requires.
- Writing to a path happens after the images are written. If the JSON write
  fails, the command fails with the images already on disk, and the log says
  so.

*Alternative:* a global `--json` that turns every output into JSON. It was
rejected because `render` produces images, not a document, and it would leave
nowhere for the human report.

### D5. `crust diff` lives in crust-render, with the logic in its own module

- `crust-render/src/diff.rs` holds the comparison, moved from the example.
  `main.rs` holds only the clap struct and the call.
- The comparison returns a `DiffReport: Serialize`. The text form is rendered
  from it, keeping the example's line formats so people reading it see the
  same output.
- Reading uses the `exr` crate already in crust-render's dependencies.
  crust-core is not involved, because it decodes no assets.
- Errors are a `Result` mapped to exit 2, replacing the example's `panic!`
  and `exit(2)` paths.

*Alternative:* put the comparison in crust-core so `add-diagnostic-command`
can reuse it. It was rejected because the diagnostic compares in-memory
buffers, not files, and crust-core must not read EXRs. The metric arithmetic
(`relmse` and the trimmed variant) is small. If the diagnostic needs it, it
can move to `utils` then.

### D6. The listing returns records, read through the import's readers

- `list_prims` becomes `list_records(path, kind, frame: Option<f64>) ->
  Vec<ListRecord>`. `ListRecord` is an enum per kind holding `path` plus its
  values. `Scene::list_usd` keeps returning paths, mapped from the records, so
  text `ls` is unchanged.
- The values come from the functions the import calls:
  - cameras: the camera reader used to build `Camera` (focal length, aperture,
    clipping, projection);
  - lights: the UsdLux readers in `lux.rs`;
  - materials: the surface-output resolution the material import starts from.

  Where a reader is currently private to its sibling file, it becomes
  `pub(super)`. A second copy of a reader would create a new pair that has to
  change together, which is the kind of bug CLAUDE.md warns about.
- The time code is set with `EvalTimeScope` around the walk, as the import
  does.
- `is_render_camera` compares against `RenderSettings.camera`.
- `bound` is gathered in the same walk: every `material:binding` relationship
  target the walk meets goes into a set, and the material records are marked
  at the end. Collection bindings resolve their targets the way the import
  does.

*Alternative:* a material `kind` matching `Material::kind()` (the names
`--stats` prints). It was rejected because it requires building the material
(MaterialX compile, texture resolution), which makes `ls` as slow as an
import. The authored shader identifier is cheap, and it is honest about what
it is.

### D7. One stamp, built once, applied by both writers

- crust-core gains `SamplingStamp` with a
  `from(&RenderSettings, &RayStats, camera_path)` constructor and
  `fn attributes(&self) -> Vec<(&'static str, StampValue)>`. Each `StampValue`
  is an int, float, double or text.
- crust-render has one `stamp(&mut LayerAttributes, &SamplingStamp)`, called by
  both `write_beauty` and `write_product`. A test renders through each path and
  reads the stamp back.
- EXR types:
  - `crust:spp` and `crust:minSpp` are `int`;
  - `crust:sppTaken` is `v2i` (min, max);
  - `crust:indirectClamp` is `float`, `0` when off;
  - `crust:frame` is `double`;
  - the rest are `string`.

  Typed attributes let Nuke and `exrheader` show them correctly.
- The product writer adds `crust:` to the reserved prefixes it already refuses
  for `colorInteropID` and the standard names. The authored value is not
  copied, and a warning is logged.
- `crust:sppTaken` comes from `RayStats.spp_min`/`spp_max`. Those fields are
  only filled during adaptive passes. Without adaptive passes, both are `spp`.
- Naming: `crust:` plus the USD attribute's name (`crust:indirectClamp`), not
  OpenEXR's `renderer/key` convention. That keeps one vocabulary across USD,
  CLI and EXR.

### D8. Comparability rules in `diff`

Comparability is computed from the two stamps only:

- `unknown` if either stamp is missing.
- Otherwise, `warn` with one note per rule that fires:
  - adaptive sampling was active on either side (`spp > minSpp`, or
    `sppTaken.min != sppTaken.max`);
  - `indirectClamp` differs;
  - `frame`, `camera`, `spp`, `samplingStrategy` or `lightSelection` differs;
    each differing field is named.
- Comparability never changes the exit status. In text mode its notes are
  printed to stderr, so a script reading stdout sees the same report as
  before.

### D9. `exr_diff` is deleted, not kept as a wrapper

The example is removed in the same change. Every reference moves to
`crust diff`, and `check_images.sh` switches to the exit status (it already
builds `target/release/crust` for rendering).
`gen_texture_alias_scene.py` reads `--json -`. Keeping a wrapper would
leave two names for one tool in the docs.

## Risks / Trade-offs

- **[Key names become an API]** → Keys are reviewed once in this change, and
  a test per format pins the key set. The version bumps on any rename (spec
  rule).
- **[Stamp breaks header-identity tests]** → The specs now promise identical
  pixels and windows, and a header that differs only by the stamp. Tests that
  compare whole headers byte for byte are rewritten to compare with the
  `crust:` attributes removed.
- **[Stamped goldens differ from old goldens]** → `check_images.sh` uses
  `crust diff`, which compares channels, not headers. Old goldens without a
  stamp report `unknown` and still pass on identical pixels.
- **[`ls` gets slower with metadata]** → Values are read only with `--json`.
  The text path keeps today's walk. `bound` needs a binding walk, and that
  cost is measured on ALab before landing; if it is significant, it runs only
  with `--json`.
- **[Conflict with `add-diagnostic-command`]** → Both modify the `cli`
  "Subcommands" requirement. Whichever lands second merges the list. If this
  change lands first, the diagnostic's task 2.1 (add serde) becomes a no-op.
- **[An agent trusts `ok` too much]** → `ok` means only that the stamps
  match. It does not mean the difference is noise. The 1/√N check in CLAUDE.md
  remains the test for that, and the site page says so.

## Migration Plan

- The tooling is internal and the change ships in one PR. Everything that
  calls `exr_diff` is updated in that PR, and `rg exr_diff` must find nothing
  outside `openspec/changes/archive/`.
- Rollback is a revert. No data format is persisted except EXR headers, and
  extra header attributes are ignored by every reader.

## Open Questions

- Should `crust diff` take `--channels` to restrict the comparison (e.g. the
  beauty only)? Deferrable: the JSON already reports per channel.
