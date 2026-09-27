//! Path guiding: a pure-Rust, surface-only implementation of Practical Path
//! Guiding (Müller et al. 2017) — the SD-tree family of algorithms that
//! Intel's OpenPGL generalizes. The renderer trains a [`GuidingField`] over
//! progressive passes and mixes its directional distribution with BSDF
//! sampling via one-sample MIS.

mod dtree;
mod field;
mod sdtree;

pub use field::{GuidingConfig, GuidingField, SampleData};
