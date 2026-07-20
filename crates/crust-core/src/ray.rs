use crate::medium::Medium;
use glam::Vec3A;
use std::sync::Arc;

/// The `Ray` struct represents a ray in 3D space, defined by an origin and a direction.
/// Rays are used in ray tracing to determine intersections with objects in the scene.
///
/// An optional `medium` describes the participating medium the ray is
/// currently travelling through — used by transmissive OpenPBR materials so
/// the tracer can apply Beer-Lambert attenuation between surface hits.
#[derive(Default, Clone)]
pub struct Ray {
    orig: Vec3A,
    dir: Vec3A,
    medium: Option<Arc<Medium>>,
}

impl Ray {
    /// Creates a new `Ray` with the specified origin and direction, in
    /// vacuum (no medium).
    pub fn new(origin: Vec3A, direction: Vec3A) -> Ray {
        Ray {
            orig: origin,
            dir: direction,
            medium: None,
        }
    }

    /// Creates a new `Ray` travelling through the given medium. Use this on
    /// a refraction that enters a transmissive volume.
    pub fn new_in_medium(origin: Vec3A, direction: Vec3A, medium: Arc<Medium>) -> Ray {
        Ray {
            orig: origin,
            dir: direction,
            medium: Some(medium),
        }
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
