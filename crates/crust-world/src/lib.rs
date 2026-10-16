//! The scene the integrator traces: the [`World`] over the intersection
//! kernel with its per-geometry side tables, and the lights — their shapes,
//! the [`LightList`] and its selection, the dome's [`EnvironmentMap`].
#![forbid(unsafe_code)]

// The crates below, glob-imported at the root so `crate::color`,
// `crate::LightList` and the like keep naming what they named when this was
// all one crate (crust-core): the modules moved here unchanged.
#[allow(unused_imports)]
use crust_base::*;
#[allow(unused_imports)]
use crust_shading::*;

#[doc(hidden)]
pub mod environment;
#[doc(hidden)]
pub mod light;
#[doc(hidden)]
pub mod rt_world;

/// What every kernel scene commits with — the `CRUST_TRI_PACKETS` switch,
/// read once.
#[doc(hidden)]
pub fn commit_options() -> crust_rt::CommitOptions {
    crust_rt::CommitOptions {
        layout: config().tri_packets.into(),
    }
}

pub use environment::EnvironmentMap;
pub use light::{
    AffineShape, AreaLight, AreaShape, DistantLight, DomeLight, EVERY_CLASS, FoundAlong, Light,
    LightBounds, LightKind, LightLinks, LightList, LightSample, LightSelection, LightShape,
    LightTree, Receiver, RectShape, ShapeExtent, ShapeHits, SolidAngleSampler, SolidAngleSampling,
    SphereShape, TREE_DEFENSIVE_SHARE, UnitShape, projected_cone_solid_angle,
};
#[cfg(feature = "traversal-stats")]
pub use rt_world::traversal_report;
pub use rt_world::{FaceMap, FanSlice, SubFace, UvMap, World, WorldBuilder, WorldHit, tangent_of};
