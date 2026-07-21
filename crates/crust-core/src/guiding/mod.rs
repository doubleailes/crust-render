//! Path guiding: a pure-Rust, surface-only implementation of Practical Path
//! Guiding (Müller et al. 2017) — the SD-tree family of algorithms that
//! Intel's OpenPGL generalizes. The renderer trains a [`GuidingField`] over
//! progressive passes and mixes its directional distribution with BSDF
//! sampling via one-sample MIS.

mod dtree;
mod field;
mod sdtree;

pub use field::{GuidingConfig, GuidingField, SampleData};

use glam::Vec3A;

/// Rec. 709 luminance, used to collapse radiance to the scalar flux the
/// guiding trees store.
#[inline]
pub fn luminance(c: Vec3A) -> f32 {
    0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z
}
