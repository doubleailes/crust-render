//! Crust Render's ray tracing kernel — the intersection layer only,
//! factored out of the renderer the way `openqmc-rs` factored out
//! sampling. The API is deliberately **Embree-shaped** (geometry objects
//! attached to a scene, a commit step, `intersect`/`occluded` queries,
//! ID-based hits) so a renderer written against it could swap in Embree
//! bindings behind the same seam — while this native implementation stays
//! 100% safe Rust.
//!
//! ```
//! use crust_rt::{Geometry, Ray, SceneBuilder};
//! use glam::Vec3A;
//!
//! let mut builder = SceneBuilder::new();
//! let ball = builder.attach(Geometry::Sphere { center: Vec3A::ZERO, radius: 1.0 });
//! let scene = builder.commit();
//!
//! let hit = scene
//!     .intersect(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z), 0.001, f32::INFINITY)
//!     .expect("hit");
//! assert_eq!(hit.geom_id, ball);
//! assert!((hit.t - 4.0).abs() < 1e-4);
//! assert!(!scene.occluded(&Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z), 0.001, 3.9));
//! ```
//!
//! What Embree calls `rtcIntersect1` / `rtcOccluded1` are
//! [`Scene::intersect`] / [`Scene::occluded`]; geometry types map to the
//! [`Geometry`] enum (triangle meshes with optional per-vertex shading
//! normals, analytic spheres, round curve segments, and instances — which
//! nest — with optional transform motion blur); per-geometry visibility
//! masks test against the ray's category bit exactly like Embree's. Hits
//! carry `geom_id`/`prim_id` — the application owns the mapping from IDs
//! to materials or anything else; this crate never sees shading data.

mod aabb;
mod bvh;
mod curve;
mod prim;
mod ray;
mod scene;
mod triangle;

pub use aabb::AABB;
pub use ray::{MASK_ALL, MASK_CAMERA, MASK_INDIRECT, MASK_SHADOW, Ray};
pub use scene::{CubicCurveSegment, CurveSegment, Geometry, RayHit, Scene, SceneBuilder};

/// The `geom_id`/`prim_id` value that never names a real geometry.
pub const INVALID_ID: u32 = u32::MAX;
