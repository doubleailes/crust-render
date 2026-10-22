# Tasks

## 1. Kernel

- [x] 1.1 `Width` trait, `W4` / `W8`, `WideNode<W>`, `collapse::<W>`, generic traversals,
  `enum Tree`. Verify: `cargo test -p crust-rt` passes.
- [x] 1.2 `NodeWidth` on `CommitOptions`, `Auto` by primitive count. Verify:
  `eight_wide_hits_are_bitwise_the_four_wide_hits`, `collapse_widens_the_tree`, node-size
  tests pass; `cargo +nightly test -p crust-rt --features bvh8` passes.
- [x] 1.3 `ray_throughput --width`. Verify: the sweep in `docs/simd.md` § "BVH8 per tree".

## 2. Renderer

- [x] 2.1 `CRUST_BVH_WIDTH` in `crust_core::Config`, wired into `commit_options()`.
  Verify: config parse test; under `auto` ten sample scenes render bit-identically to
  the previous binary at 16 spp (`crust diff`); under `eight` seven do, and the three
  that do not (`subdivision`, `implicit_shapes`, `hair`) differ only at exact ties,
  found by a debug twin tracing every query at both widths.

## 3. Documentation

- [x] 3.1 `docs/simd.md`, `docs/architecture.md` § Environment switches, the
  intersection-kernel design record, `site/` (environment variables, design choices).
  Verify: `zola build` in `site/`.

## 4. Final checks

- [x] 4.1 `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --no-fail-fast`.
