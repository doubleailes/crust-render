## 1. Inline-route parity (needs no openusd change)

- [ ] 1.1 Write the inline `ND_*` equivalent of each `samples/*.mtlx` material
      as a test fixture, laid out as `usdMtlx` lays it out (check against
      `usdcat` output of the same file where a C++ build is available). Done
      when each fixture loads through `mtlx_network.rs` without a fallback
      warning.
- [ ] 1.2 Add a test that compiles each material both ways (the `.mtlx`
      through `crust-mtlx::parse`, the fixture through `mtlx_network.rs`) and
      compares the compiled programs, closure trees and texture requests
      exactly. Done when it passes for every sample, or each failure has a
      task below.
- [ ] 1.3 Close the gaps 1.2 finds in `mtlx_network.rs`. Known candidates:
      the `displacementshader` terminal (`samples/displacement_height.mtlx`),
      `fileprefix`, integer versus float literals, `filename` versus
      `string`. Done when 1.2 passes for every sample.

## 2. openusd (tracked; done in `doubleailes/openusd`)

- [ ] 2.1 Runtime `sdf::FileFormat` registration on the stage builder or
      `LayerRegistry`.
- [ ] 2.2 `openusd-mtlx`: a read-only `.mtlx` format with the `usdMtlx`
      layout, embedded MaterialX 1.39 nodedef libraries, `colorSpace`
      metadata and `fileprefix` folding.
- [ ] 2.3 Both on openusd `main`, with the ROADMAP row updated.

## 3. Switch crust over

- [ ] 3.1 Record goldens with the current binary:
      `scripts/check_images.sh record <dir>`, and `--stats` import timings for
      the DPEL Teapot and Lion and one Moana material library.
- [ ] 3.2 Bump the openusd revision in the workspace `Cargo.toml` and
      `Cargo.lock`, add `openusd-mtlx` patched the same way, and run
      `cargo deny --locked check`.
- [ ] 3.3 Register the format in `usd_import::stage_builder()`. Done when
      `a_materialx_reference_resolves_to_a_real_material` passes with the
      fallback still in place but never reached (a debug assertion or log
      check).
- [ ] 3.4 Remove `mtlx_reference`, `load_mtlx_material`, `try_mtlx!` and any
      `material/materialx.rs` loading entry point left without a caller.
      Done when `cargo clippy --workspace --all-targets -- -D warnings` and
      `cargo test --workspace` pass.
- [ ] 3.5 Add the spec's payload and texture-anchoring scenarios as tests
      under `crates/crust-core/tests/`.
- [ ] 3.6 `scripts/check_images.sh check <dir>`: every sample `identical`.
      Re-render the DPEL Teapot and Lion and `crust diff` them against the
      old binary's output. Compare import time with 3.1 and record the result
      in the design record.

## 4. Documentation

- [ ] 4.1 `openspec/specs/materials/design.md` § MaterialX: replace the
      "openusd ships none" rationale with the plugin route, and move the
      layout and round-trip notes there from this change's design.
- [ ] 4.2 Module docs: `crust-mtlx/src/lib.rs`, `material/materialx.rs`, the
      `mtlx_network.rs` header and the `tests/usd_scene.rs` MaterialX comment.
- [ ] 4.3 `CLAUDE.md` workspace line for `crust-mtlx`, and `openspec/config.yaml`
      context.
- [ ] 4.4 `site/content/docs/usd/materials.md`: a `.mtlx` works through any
      arc; `architecture/design-choices.md` and `limitations.md` where they
      say crust reads `.mtlx` itself. Build with `zola build`.
- [ ] 4.5 Sync this change's spec delta into `openspec/specs/materials/spec.md`
      and archive the change.
