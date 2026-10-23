# Tasks

## 1. Baselines (design D7)

- [x] 1.1 Record goldens with `scripts/check_images.sh record <dir>` (with
  `--indirect-clamp 0`) and the callgrind instruction count of `cornellbox.usda` at
  `-s 2`, both before any code change. Record the numbers in this file.
  - Recorded on `f958a80`: 43 goldens (every `samples/*.usda`; the gitignored
    `Kitchen_set` download is absent from the checkout, so its two scenes were not
    recorded).
  - Callgrind, `RAYON_NUM_THREADS=1`, `cornellbox.usda -s 2`: **4,276,302,705**
    instructions.

## 2. Import (design D1, D2, D5, D9)

- [x] 2.1 Read `crust:sampleFilters`, `crust:displayFilters` and `crust:display` in
  `scene/usd_import/settings.rs`, after the active pass is applied. Store a
  `PrecompSpec` on the scene's render settings, `None` when nothing is authored.
- [x] 2.2 Read `crust:filter`, `crust:filter:output` and `crust:filter:radiance` in
  `resolve_var` (`scene/usd_import/products.rs`). Add the fields to `AovVar`, and
  refuse a type mismatch with `aov.type_mismatch`.
- [x] 2.3 Add the `precomp.*` `WarningCode`s to `crust-core/src/warnings.rs` and their
  rows to `site/content/docs/reference/warnings.md`. Verify the test that keeps the list
  and the page in step passes.
- [x] 2.4 Add `crust check` support: the precomp in the `crust-check/2` report. Verify
  with a test over `samples/precomp.usda`.

## 3. Sample filters (design D2, D3)

- [x] 3.1 In crust-mtlx, compile a `NodeGraph` whose interface inputs are lowered to
  `Program::primvars` channels. Refuse textures, `geompropvalue`, geometry nodes and
  closures as unsupported. Verify with unit tests in `crust-mtlx`, and that JIT ↔
  interpreter is bit-identical for the new programs
  (`cargo test -p crust-jit --features jit`).
- [x] 3.2 Add filtered filter slots to `UnitAov` / `AovFilm`. Evaluate the programs at
  the per-sample seam in `advance_pixel`, from `SampleExtras`, `FirstHit` and the beauty
  sample, with a reused per-thread scratch and no per-sample allocation.
- [x] 3.3 Apply exposure to radiance filter outputs (`AovFilm::apply_exposure`) and
  blend them across guided passes (`AovFilm::blend`). Verify with tests in
  `crust-core/tests/aovs.rs`:
  - the antialiased-edge scenario;
  - the `closest` var read per sample;
  - exposure with `crust:filter:radiance = 0`;
  - holdout clear values.

## 4. Display filters (design D4, D6)

- [x] 4.1 New `crust-core/src/precomp.rs` holding the chain type and the five
  operations, row-parallel with a fixed summation order. Load kernels through
  `AssetLoader`. Verify each operation against a hand-computed reference on small
  images, and the thread-count bit-identity with `RAYON_NUM_THREADS=1` against the
  default.
- [x] 4.2 Call the chain after `render_impl`'s exposure, and feed the planes to
  `write_product` and `write_png` (`crates/crust-render/src/main.rs`, `products.rs`).
- [x] 4.3 Make `publish` copy filter and bound-var estimates into a snapshot film only
  when a displayed output is authored, and make `RenderControl::snapshot()` run the
  chain on read. Verify with a `--checkpoint` test and an MCP `snapshot` test.

## 5. Output (design D5, D9)

- [x] 5.1 Write filter-output channels in products, the PNG from the displayed output,
  and the EXR stamps `crust:sampleFilters`, `crust:displayFilters` and `crust:display`.
  Verify the image-output scenarios.
- [x] 5.2 Make the MCP session's `render` / `snapshot` return the displayed output.
  Verify with a session test.

## 6. Guarantees and measurement (design D7, Risks)

- [x] 6.1 Extend `the_beauty_is_bit_identical_with_and_without_aovs` to cover filters.
  Run `scripts/check_images.sh check` against the goldens from 1.1, which must show no
  change.
- [x] 6.2 Callgrind on `cornellbox.usda` at `-s 2`: the instruction count must equal
  the one from 1.1. If it does not, make the hook a const generic (D7).
  - Accepted on the per-sample path: `advance_pixel::<Sobol, false, …>` is
    3,816,397,660 instructions inclusive before and after, to the instruction. The
    process total is 4,276,658,838 (+0.008%), all of it once per render or per work
    unit (the import's three settings reads, three EXR stamp attributes, `Unit::new`
    codegen); a const generic would remove none of it. See design § As built.
- [x] 6.3 Run `scripts/bench_ab.sh` on `samples/precomp.usda` with and without the
  sample filter, and time the display chain. Record min and mean in the design record.
- [x] 6.4 `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo test --workspace`.

## 7. Sample and docs

- [x] 7.1 Add `samples/precomp.usda`: surfaces that write AOVs only (key, fill, rim,
  matte, motion), a sample filter that rebuilds a stylised look, and a glow plus a
  speed-line blur as display filters.
- [x] 7.2 Update `site/content/docs/usd/render-settings.md` with a "Precomp" section:
  `### crust:sampleFilters`, `### crust:displayFilters`, `### crust:display`, the
  operation table, the exposure-order note, and the black-beauty guidance.
- [x] 7.3 Update `site/content/docs/usd/aovs.md`:
  - the `crust:filter` var;
  - the EXR metadata rows;
  - the sentence that says products and vars take no `crust:*` attribute.
- [x] 7.4 Update `site/content/docs/architecture/` (the design choice, and the limits:
  fixed operations, crop edges, beauty-only adaptive stop) and `docs/architecture.md`
  (the seam, and the invariant extended to filters). Build the site with Zola 0.21.
- [ ] 7.5 When archiving, create `openspec/specs/precomp-filters/design.md` from this
  design, and end it with a "Known gaps" section.
