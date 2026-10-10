//! The axis-aligned bounding box now lives in the `crust-rt` kernel
//! crate; re-exported here for the modules (volumes) that use it
//! as a plain geometric type.

pub use crust_rt::AABB;
