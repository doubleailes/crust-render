//! # openqmc
//!
//! A faithful Rust port of [AcademySoftwareFoundation/openqmc][upstream]
//! (Apache-2.0), the quasi-Monte Carlo sampling library. It provides three base
//! samplers — Owen-scrambled [`SobolSampler`], rank-1 [`LatticeSampler`], and
//! progressive multi-jittered [`PmjSampler`] — each with a blue-noise variant
//! ([`SobolBnSampler`], [`LatticeBnSampler`], [`PmjBnSampler`]), all behind a
//! single pass-by-value **domain-tree** API ([`Sampler`]).
//!
//! ## Domain-tree usage
//!
//! Construct a root sampler per (pixel, frame, sample-index), derive independent
//! 4-dimensional sub-patterns with [`Sampler::new_domain`] (padding), and draw up
//! to four dimensions per domain:
//!
//! ```
//! use openqmc::SobolSampler;
//!
//! let root = SobolSampler::new(/* x */ 12, /* y */ 34, /* frame */ 0, /* index */ 7);
//! let camera = root.new_domain(0);
//! let [jitter_x, jitter_y, lens_u, lens_v] = camera.draw_sample_f32::<4>();
//! let bounce = root.new_domain(1);
//! let dir = bounce.draw_sample_f32::<2>();
//! # let _ = (jitter_x, jitter_y, lens_u, lens_v, dir);
//! ```
//!
//! Use [`Sampler::draw_sample`] for the high-quality stratified dimensions and
//! [`Sampler::draw_rnd`] / [`Sampler::rng`] for incidental or unbounded random
//! draws (Russian roulette, delta tracking) where stratification does not apply.
//!
//! ## Relationship to the C++ headers
//!
//! Modules map one-to-one to the upstream `oqmc/*.h` headers, with the same
//! algorithms and (bit-for-bit) the same sample values. Two idiomatic
//! adaptations: the caller-allocated `void*` cache — a GPU memory-management
//! concern — becomes a lazily-initialised process global, so every [`Sampler`]
//! stays a small `Copy + Send` value; and the sequential PRNG (`pcg::Rng`) is
//! surfaced as a named type. The bundled blue-noise tables under `src/data/` are
//! the upstream optimised tables converted verbatim to little-endian blobs.
//!
//! [upstream]: https://github.com/AcademySoftwareFoundation/openqmc

// Utilities (oqmc/*.h leaf headers).
pub mod encode;
pub mod float;
pub mod pcg;
pub mod permute;
pub mod range;
pub mod reverse;
pub mod rotate;

// Sequence cores.
pub mod lookup;
pub mod owen;
pub mod rank1;
pub mod stochastic;

// State + generic interface.
pub mod sampler;
pub mod state;

// Samplers.
pub mod bntables;
pub mod lattice;
pub mod latticebn;
pub mod pmj;
pub mod pmjbn;
pub mod sobol;
pub mod sobolbn;

// Re-export the public surface at the crate root, mirroring the `oqmc::`
// namespace.
pub use sampler::{Sampler, SamplerImpl};
pub use state::State64Bit;

pub use lattice::LatticeSampler;
pub use latticebn::LatticeBnSampler;
pub use pmj::PmjSampler;
pub use pmjbn::PmjBnSampler;
pub use sobol::SobolSampler;
pub use sobolbn::SobolBnSampler;
