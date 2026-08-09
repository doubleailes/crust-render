//! crust-capi — the C ABI boundary for the hdCrust Hydra render delegate.
//!
//! This crate is the **one sanctioned exception** to the workspace's
//! safe-Rust rule (see `docs/hydra_delegate.md`): `unsafe` here is confined
//! to the `extern "C"` surface (raw-pointer arguments become checked
//! references and slices), and to the pinned self-reference inside
//! [`handles::RendererHandle`]. No engine crate depends on this crate, so
//! the exception cannot leak inward.
//!
//! Every export that takes a raw pointer is `pub unsafe extern "C" fn`,
//! with its preconditions in a `# Safety` section: validation here checks
//! what CAN be checked (null, counts, finiteness), but validity, alignment,
//! liveness and exclusivity are promises only the caller can make — a safe
//! signature would let safe Rust cause UB with a dangling pointer. The
//! `unsafe` marker is Rust-side only: symbol names and the C ABI are
//! unchanged.
//!
//! The C contract is `include/crust.h` — hand-written, and every
//! `extern "C"` function below carries its C declaration in the doc comment
//! directly above it. The C smoke test (`tests/smoke.c`, run by
//! `scripts/test_capi_c.sh`) compiles that header with `-Wall -Wextra
//! -Werror` and exercises every exported symbol; POD struct sizes are
//! pinned by paired static asserts on both sides.
//!
//! Two facts shape the implementation:
//!
//! - The workspace builds release with `panic = "abort"`, so
//!   `catch_unwind` cannot protect the boundary — **validation is the
//!   safety net**. Every argument is checked before any engine call, and
//!   the engine entry points used here are non-panicking on bad geometry
//!   (the kernel skips out-of-range indices at commit).
//! - `ProgressiveRender` borrows its `Renderer`; C wants one opaque handle
//!   owning both. `handles::RendererHandle` documents how that
//!   self-reference is kept sound.

mod dome;
mod geo_cache;
mod handles;
mod material;
mod render;
mod scene;
mod status;
mod token;
mod validate;

pub use geo_cache::*;
pub use handles::{RendererHandle, SceneHandle, TokenHandle};
pub use material::*;
pub use render::*;
pub use scene::*;
pub use status::*;
pub use token::*;
