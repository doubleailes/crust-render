//! Stateless math shared by the workspace: sampling warps (cosine
//! hemisphere, concentric disk, uniform sphere and ball), the MIS heuristics,
//! the one Rec.709 `luminance`, frame alignment and small vector helpers.
//!
//! Nothing here holds state or draws randomness: callers pass in the uniform
//! numbers, which come from `openqmc` (CLAUDE.md: no RNG outside it).
//!
#![forbid(unsafe_code)]

mod common;
pub use common::Lerp;
pub use common::{
    align_to_normal, concentric_disk, cosine_hemisphere, degrees_to_radians, exp3, luminance,
    uniform_ball, uniform_sphere,
};
pub use common::{balance_heuristic, power_heuristic};
