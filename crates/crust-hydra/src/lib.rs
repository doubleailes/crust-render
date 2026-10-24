//! Crust behind a C ABI, for `hdCrust`, the Hydra render delegate
//! (`hydra/hdCrust`, the `hydra-delegate` capability).
//!
//! The delegate writes what Hydra presents as USD layers in a session
//! directory — a flattened `scene.usdc` and an override `edits.usda` over it
//! — and this crate renders them: [`Session`] imports the override layer as
//! `crust render -i <dir>/edits.usda` would, applies each later change to it
//! in place or by a re-import exactly as `crust mcp` applies an edit
//! (`crust_render::session`), and renders it progressively while the
//! viewport polls.
//!
//! The safe Rust API is [`Session`]; the C ABI that `hdCrust` links
//! (`ffi`, declared by `include/crust_hydra.h`) wraps it.
//!
//! # `unsafe`
//!
//! The second crate in the workspace with `unsafe` in production code, after
//! `crust-jit`, and confined to the C ABI's pointers (`ffi.rs`).
//! `deny(unsafe_code)` holds everywhere except:
//! - the twelve `#[unsafe(no_mangle)]` exports, eleven of them `unsafe extern
//!   "C" fn`s whose pointer arguments are the caller's promise (the header
//!   states each contract);
//! - five audited blocks, each a helper with a `// SAFETY:` comment and the
//!   only way the exports touch a pointer: `c_str` (a C string in),
//!   `borrow` (the handle), `reclaim` (the handle freed), `out_slice` (an
//!   output buffer) and `put` (an output value).
//!
//! Nothing here reaches crust-core's hot path: the render runs in safe code
//! on the session's threads, and the ABI only copies its results out.
#![deny(unsafe_code)]

pub mod ffi;
mod log;
mod session;

pub use session::{Aov, Error, Session, Snapshot};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_ffi;
