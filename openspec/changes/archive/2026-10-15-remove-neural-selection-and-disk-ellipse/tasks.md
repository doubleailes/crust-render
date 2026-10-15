## 1. Remove

- [x] 1.1 Remove `neural` selection, `light_visibility.rs`, `crust-nn`, `nvc_bench`
      and `tests/neural_selection.rs`; drop the neural-only labels from
      `light_cache::walk`.
- [x] 1.2 Remove `CRUST_DISK_SAMPLING`, `DiskSampling`, `light/ellipse.rs`, the
      ellipse oracle and its tests; `with_sampling` takes the tube strategy.
- [x] 1.3 Update the user docs (`site/`), `docs/architecture.md`, `CLAUDE.md`,
      the `lighting` spec and design record, and `docs/light_sampling.md`.

## 2. Verify

- [x] 2.1 `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`,
      `cargo test --workspace`.
