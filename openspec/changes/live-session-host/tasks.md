## 1. The session moves

- [x] 1.1 Move `crust-hydra/src/session.rs` into
      `crust-render/src/session/live.rs` with `Hooks`. `crust-hydra::Session`
      wraps it. Done when `cargo test -p crust-hydra` passes unchanged.

## 2. Owned layers

- [x] 2.1 `LiveSession::load_owned`, the root-stack check, and `apply` over
      every owned layer (`classify_layers`), with the foreign layers stamped
      as sources.
- [x] 2.2 Tests (`session/live_tests.rs`): a light edit in an owned sublayer
      is `lights` and renders bitwise as a fresh session; a root and a
      sublayer edit classify together; a transform is `full` and renders as a
      fresh session; a change to a sublayer the session does not own is
      `full`; an owned layer outside the root stack is refused.

## 3. Cancel without the queue

- [x] 3.1 `LiveSession::cancel_render`, `render`, `outcome`, and
      `Snapshot::{render, completed}`. Test: a 4096 spp render stops after
      `cancel_render` and reports `Cancelled`.

## 4. Gates

- [x] 4.1 `cargo fmt`, `clippy -D warnings`, `cargo test --workspace`.
