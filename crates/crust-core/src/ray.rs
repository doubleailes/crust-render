use crate::medium::Medium;
use glam::Vec3A;
use std::sync::Arc;

/// Ray visibility categories, Embree-mask style: a ray carries the bit of
/// the category it belongs to, geometry carries a mask of the categories it
/// is visible to, and an intersection only counts when
/// `geometry_mask & ray_mask != 0`. Geometry defaults to [`MASK_ALL`]
/// (visible to everything), rays default to `MASK_ALL` too so rays built
/// outside the integrator (tests, benches) keep seeing the whole scene.
pub const MASK_CAMERA: u32 = 1 << 0;
pub const MASK_SHADOW: u32 = 1 << 1;
pub const MASK_INDIRECT: u32 = 1 << 2;
pub const MASK_ALL: u32 = u32::MAX;

/// The `Ray` struct represents a ray in 3D space, defined by an origin and a direction.
/// Rays are used in ray tracing to determine intersections with objects in the scene.
///
/// An optional `medium` describes the participating medium the ray is
/// currently travelling through — used by transmissive OpenPBR materials so
/// the tracer can apply Beer-Lambert attenuation between surface hits.
///
/// `time` is the shutter time in `[0, 1)` for motion blur (0 for a static
/// render) and `mask` the ray's visibility category (see [`MASK_ALL`]).
/// Both are stamped by the integrator: the camera sets them on primary
/// rays, and every secondary/shadow ray inherits the path's time.
#[derive(Clone)]
pub struct Ray {
    orig: Vec3A,
    dir: Vec3A,
    medium: Option<Arc<Medium>>,
    time: f32,
    mask: u32,
}

impl Default for Ray {
    fn default() -> Self {
        Ray::new(Vec3A::ZERO, Vec3A::ZERO)
    }
}

impl Ray {
    /// Creates a new `Ray` with the specified origin and direction, in
    /// vacuum (no medium), at shutter time 0, visible to all geometry.
    pub fn new(origin: Vec3A, direction: Vec3A) -> Ray {
        Ray {
            orig: origin,
            dir: direction,
            medium: None,
            time: 0.0,
            mask: MASK_ALL,
        }
    }

    /// Creates a new `Ray` travelling through the given medium. Use this on
    /// a refraction that enters a transmissive volume.
    pub fn new_in_medium(origin: Vec3A, direction: Vec3A, medium: Arc<Medium>) -> Ray {
        Ray {
            orig: origin,
            dir: direction,
            medium: Some(medium),
            time: 0.0,
            mask: MASK_ALL,
        }
    }

    /// Same ray with the shutter time replaced.
    pub fn with_time(mut self, time: f32) -> Ray {
        self.time = time;
        self
    }

    /// Same ray with the visibility mask replaced.
    pub fn with_mask(mut self, mask: u32) -> Ray {
        self.mask = mask;
        self
    }

    /// Same origin/direction replaced, keeping medium, time and mask — for
    /// transforming a ray into an instance's local space.
    pub fn transformed(&self, origin: Vec3A, direction: Vec3A) -> Ray {
        Ray {
            orig: origin,
            dir: direction,
            medium: self.medium.clone(),
            time: self.time,
            mask: self.mask,
        }
    }

    pub fn time(&self) -> f32 {
        self.time
    }

    pub fn mask(&self) -> u32 {
        self.mask
    }

    pub fn origin(&self) -> Vec3A {
        self.orig
    }

    pub fn direction(&self) -> Vec3A {
        self.dir
    }

    pub fn medium(&self) -> Option<&Arc<Medium>> {
        self.medium.as_ref()
    }

    pub fn at(&self, t: f32) -> Vec3A {
        self.orig + t * self.dir
    }
}
