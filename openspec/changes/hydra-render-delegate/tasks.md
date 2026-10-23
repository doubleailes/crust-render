## 1. Spike: the OpenUSD 26 renderer interface

- [x] 1.1 Build OpenUSD 26 locally (`build_usd.py --no-python --no-usdview`, GL
      off). From its headers, establish:
      - whether 26.x has a public native Hydra 2.0 renderer interface, or
        whether `HdRenderDelegate` + `GetTerminalSceneIndex()` is the entry
        point;
      - which prim types must be declared supported for them to reach the
        terminal scene index;
      - how the viewing camera and framing reach a render pass.
      Done when D1 and D4 in design.md are updated with the finding and the
      header paths.
- [ ] 1.2 Add a recipe to the command cookbook in `openspec/specs/cli/design.md`
      for building OpenUSD 26 and pointing `pxr_DIR` at it. Done when the
      recipe, followed from scratch in a clean container, gives a usable
      `pxrConfig.cmake`.

## 2. The session core in crust-render's library (D6)

- [x] 2.1 Record goldens with the current binary (`scripts/check_images.sh
      record <dir>`) and note the MCP test count. Done when the golden
      directory is complete.
- [x] 2.2 Add `src/lib.rs` to `crust-render`. Move the CLI's import-and-check
      policy into it (`load_scene`, `apply_scene_overrides`, `check_scene`,
      `select_products` and what they call). `main.rs` keeps argument parsing,
      writing and exit codes. Done when `cargo build -p crust-render` and
      `cargo test -p crust-render` pass.
- [x] 2.3 Move `mcp/{layer,delta,scene,sources}.rs` and the session's
      import/apply/render core into a `session` module behind a new `session`
      feature (openusd only). `mcp` implies `session` and keeps the server.
      Done when:
      - `cargo test -p crust-render` passes with the same MCP test count as
        2.1;
      - `cargo build -p crust-render --no-default-features --features session`
        builds with no rmcp or tokio in `cargo tree`.
- [x] 2.4 Run `scripts/check_images.sh check <dir>` against 2.1. Done when every
      sample is `identical`. Then update `docs/architecture.md` (crate table:
      crust-render is lib + bin) and `CLAUDE.md` (workspace paragraph).

## 3. crust-hydra: the C ABI crate (D7, D8)

- [x] 3.1 Add `[profile.hydra]` (inherits `release`, `panic = "unwind"`) to the
      workspace `Cargo.toml`, and reword the release profile's
      `catch_unwind` comment. Done when `cargo build --profile hydra -p
      crust-render` builds.
- [x] 3.2 Create `crates/crust-hydra` (`staticlib` + `rlib`,
      `#![deny(unsafe_code)]`), depending on `crust-render` with `session`,
      `jit` and `mmap`. Build the safe Rust API first: `Session::new(dir)`,
      `load`, `apply`, `snapshot`, `converged`, `aov`, `cancel`, with session
      and render threads as in D5. Done when Rust tests load cornellbox,
      snapshot a monotonic generation, complete, read `depth`, and cancel.
- [x] 3.3 Add a test that the session's 16 spp render of a layer equals `crust
      render` of the same layer, bitwise, through the Rust API. Done when it
      passes on cornellbox and `aovs.usda`.
- [x] 3.4 Add the `extern "C"` layer and `include/crust_hydra.h` (D7), with
      every `unsafe` block audited with `// SAFETY:`, and count them in the
      crate doc. Done when a Rust test calls each entry point through its C
      signature and checks `crust_hydra_abi_version()` against the header's
      constant parsed from the file.
- [x] 3.5 Wrap every entry point and the session and render threads in
      `catch_unwind`, forward the panic hook and `tracing` `WARN`+ to `log_cb`,
      and forward `Scene::warnings` once per import (D8, D7). Done when
      `cargo test --profile hydra -p crust-hydra` covers:
      - a panicking material during a render returning `Panicked`;
      - a following `apply` re-importing and rendering;
      - an import warning reaching the callback once.
- [ ] 3.6 Measure the cost of unwinding: callgrind, cornellbox at 2 spp, the
      `release` and `hydra` profiles. Record the figure in design.md D8. Done
      when the figure is recorded.

## 4. hdCrust: scene layer and first image (D1–D3, D9)

- [x] 4.1 Create `hydra/hdCrust/` with the CMake of D9 and `plugInfo.json`,
      registering the renderer "Crust", and a delegate that renders nothing.
      Done when usdview lists "Crust" in its renderer menu.
- [x] 4.2 Implement the scene index observer and the `scene.usdc` writer for
      meshes (topology, subdivision tags, primvars), transforms, cameras and
      the render settings prim `/__hdCrust/settings` + `/__hdCrust/camera` in
      `edits.usda`. Connect `_Execute` to `crust_hydra_load` and `snapshot`.
      Done when usdview shows cornellbox refining with Crust.
- [x] 4.3 Add materials (D3 table: UsdPreviewSurface and `ND_*` networks,
      terminals, direct bindings), UsdLux lights with shaping and textures,
      curves, points and implicit shapes. Done when `materialx_basic.usda`,
      `domelight.usda`, `curves.usda` and `implicit_shapes.usda` render in
      usdview, and each layer renders the same through `crust render`.
- [x] 4.4 Add instancers as `PointInstancer`s, visibility and render-tag
      filtering, and the once-per-kind warnings for light linking, volume
      fields, shutter motion and unread prim types. Done when:
      - `instancing.usda` and `PointInstancedMedCity.usd` render;
      - `light_linking.usda` and `fog.usda` each print one warning and still
        render.
      (Done as amended. `fog.usda` turned out not to be a volume field: its
      fog is `crust:volume:*` on a `Cube`. Hydra drops prim attributes, so
      hdCrust gained a keyless UsdImaging adapter that carries every
      `crust:*` attribute (D3), and `fog.usda` renders *with* its fog and
      no warning. The volume-fields warning is checked on a UsdVol
      `Volume` instead: one warning, scene rendered.)
- [x] 4.5 Write `hdCrustTest` (D9): headless scene indices to the Crust render
      index, a 16 spp render pass, an EXR of the color buffer. Add a test
      script that runs it on a set of samples and runs `crust diff` against
      `crust render -i <dir>/edits.usda -s 16`. Done when every sample in the
      set is identical (spec "Agreeing with the CLI").
- [x] 4.6 Add a USDC round-trip test: export every `samples/*.usda` with
      `SdfLayer::Export` to USDC, and render both with `crust render` at 16 spp.
      Done when every pair is identical, or each difference has an openusd
      issue filed and a known gap recorded.
      (`hydra/hdCrust/test/usdc_roundtrip.sh`: 42 of 43 identical. The 43rd,
      `cornellbox_guided`, is not a USDC difference. Its USDA renders one of
      two images from run to run (the USDC run matched the golden): path
      guiding is not deterministic, a crust-core issue outside this change.
      No openusd issue to file.)

## 5. Interaction: camera, edits, time, settings (D4, D5)

- [x] 5.1 Write the viewing camera from the render pass state each `_Execute`,
      and set `crust:dicingCamera` to the stage's camera or `/__hdCrust/dicing`.
      Coalesce changes into one batch and `crust_hydra_apply`. Done when:
      - `hdCrustTest` moves the camera on an adaptive sample with a stage
        camera and the session reports `settings`, not `full`;
      - the result equals `crust render` of the written layers.
- [x] 5.2 Write dirtied prims as `over`s, removed prims as `active = false`
      and added prims as `def`s. Done when `hdCrustTest` covers:
      - a light intensity change (`lights`, in place, equal to `full`);
      - a mesh transform (`full`);
      - a prim removal (`full`, the prim gone).
- [x] 5.3 On a time change, rewrite both layers at the new time and re-import.
      Done when `hdCrustTest` renders `animation.usda` at frames 1 and 10, and
      each equals `crust render` of that frame's layers.
- [x] 5.4 Expose the render settings of the spec through the host's render
      setting descriptors, with crust's defaults, written as `crust:*` on
      `/__hdCrust/settings`, plus `crust:sessionDirectory`. Done when
      `hdCrustTest` changes samples per pixel to 16 as a `settings` edit, and a
      set session directory keeps the layers after the delegate is destroyed.
- [x] 5.5 Copy AOVs into bound render buffers at completion, and report
      `IsConverged` from `crust_hydra_converged`. Done when `hdCrustTest` reads
      `depth` after completion equal to `crust render`'s `depth` AOV.
- [x] 5.6 Measure the following, and record each in design.md Risks:
      - in usdview on ALab, or the largest sample when ALab is not available:
        time to first image, peak RSS against `crust render` of the same
        layers, settings-rebuild time per drag;
      - the snapshot copy time at 1920×1080.
      Done when the figures are recorded.

## 6. CI and documentation

- [ ] 6.1 Add `.github/workflows/hydra.yml` (D9): OpenUSD 26 built and cached,
      `hdCrust` built, `hdCrustTest` and the comparison script run, on the
      listed paths and nightly. It is not a required check. Done when the job
      passes on the branch.
- [x] 6.2 Write `site/content/docs/help/usdview.md`, covering:
      - building OpenUSD 26 and `hdCrust`, `PXR_PLUGINPATH_NAME`;
      - the renderer settings, the dicing camera and what isn't carried yet;
      - reproducing a viewport image with `crust render`.
      Link it from the architecture pages. Done when `zola build` in `site/`
      passes its link check.
- [x] 6.3 Update `docs/architecture.md`:
      - crate table: `crust-hydra`;
      - seams: `RenderControl`'s "later a Hydra delegate";
      - the `unsafe` list.
      Update `CLAUDE.md`:
      - the workspace line;
      - `unsafe` rules: `crust-hydra` `deny` with audited blocks;
      - the `[profile.hydra]` command and the hydra job in the CI list.
      Retire the "No Hydra render delegate" known gaps in
      `openspec/specs/usd-scene-import/design.md` and
      `openspec/specs/mcp-session/design.md`. Add
      `openspec/specs/hydra-delegate/design.md` from this design.md, with its
      known gaps. Done when every reference to a missing delegate is gone
      (`grep -rn "No Hydra render delegate"` returns nothing).

## 7. Integration checks

- [ ] 7.1 Run `cargo fmt --all -- --check`, `cargo clippy --workspace
      --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`
      and `cargo deny --locked check`. Done when all pass, and the required
      CI does not build `hdCrust`.
- [x] 7.2 Run `scripts/check_images.sh check <dir>` against 2.1's goldens. Done
      when every sample is `identical`: the `crust` binary is unchanged.
      (2.1's goldens were lost to a disk cleanup; re-recorded from a worktree
      of the base commit `15bf8872`. 43 samples identical, `cornellbox_guided`
      included; the two Kitchen_set scenes are not on the machine, and
      `purpose_bindings` is new with this change.)

## Workflow follow-up

- Archive `live-camera-update` first. This change depends on it.
- Archive this change after review, once the hydra job is green.
